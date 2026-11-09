//! Queue-and-replay for proxies that are temporarily disconnected: inputs
//! sent while the proxy is away are buffered (bounded, age-limited) and
//! replayed in order on reconnect.

use std::time::{Duration, Instant};

use shared::protocol::{MAX_PENDING_MESSAGES_PER_SESSION, MAX_PENDING_MESSAGE_AGE_SECS};
use shared::ServerToProxy;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::{ProxySender, SessionId, SessionManager};

/// Maximum age of pending messages before they're dropped
const MAX_PENDING_MESSAGE_AGE: Duration = Duration::from_secs(MAX_PENDING_MESSAGE_AGE_SECS);

/// A message queued for a disconnected proxy
#[derive(Clone)]
pub(super) struct PendingMessage {
    pub(super) msg: ServerToProxy,
    pub(super) queued_at: Instant,
}

/// Where [`SessionManager::send_or_queue`] put a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sent {
    /// Handed to the connected proxy's channel (not yet acknowledged).
    Live,
    /// No live proxy: held in memory for it to receive on reconnect.
    Queued,
}

impl SessionManager {
    pub fn send_to_session(&self, session_key: &str, msg: ServerToProxy) -> bool {
        self.send_or_queue(session_key, msg);
        true
    }

    pub(crate) fn send_or_queue(&self, session_key: &str, msg: ServerToProxy) -> Sent {
        let msg = match self.sessions.get(session_key) {
            Some(conn) => match conn.sender.send(msg) {
                Ok(()) => return Sent::Live,
                // A closed channel hands the message back in the error, so
                // there's no need to clone up front just in case.
                Err(mpsc::error::SendError(msg)) => {
                    let gen = conn.gen;
                    let cancel = conn.cancel.clone();
                    // Release the shard guard before mutating the map.
                    drop(conn);
                    self.evict_dead_connection(session_key, gen, &cancel);
                    msg
                }
            },
            None => msg,
        };

        self.queue_pending_message(session_key, msg);
        Sent::Queued
    }

    pub fn send_to_connected_session(&self, session_key: &str, msg: ServerToProxy) -> bool {
        match self.sessions.get(session_key) {
            Some(conn) => match conn.sender.send(msg) {
                Ok(()) => true,
                Err(_) => {
                    let gen = conn.gen;
                    let cancel = conn.cancel.clone();
                    drop(conn);
                    self.evict_dead_connection(session_key, gen, &cancel);
                    false
                }
            },
            None => false,
        }
    }

    /// Drain this session's pending queue into `sender`, dropping entries
    /// older than [`MAX_PENDING_MESSAGE_AGE`]. Called on proxy registration.
    pub(super) fn replay_pending_messages(&self, session_key: &str, sender: &ProxySender) -> usize {
        let mut replayed = 0;
        let now = Instant::now();

        if let Some((_, mut pending)) = self.pending_messages.remove(session_key) {
            while let Some(pending_msg) = pending.pop_front() {
                if now.duration_since(pending_msg.queued_at) < MAX_PENDING_MESSAGE_AGE {
                    if sender.send(pending_msg.msg).is_ok() {
                        replayed += 1;
                    } else {
                        warn!("Failed to replay pending message, sender closed");
                        break;
                    }
                } else {
                    debug!(
                        "Dropping expired pending message (age: {:?})",
                        now.duration_since(pending_msg.queued_at)
                    );
                }
            }
        }

        replayed
    }

    /// Expiry must run even when a proxy never reconnects. Retain under the
    /// shard lock so removing an empty queue cannot erase a concurrent enqueue.
    pub fn sweep_expired_pending_messages(&self) -> usize {
        let now = Instant::now();
        let mut expired = 0;
        self.pending_messages.retain(|_, queue| {
            let before = queue.len();
            queue.retain(|message| {
                now.saturating_duration_since(message.queued_at) < MAX_PENDING_MESSAGE_AGE
            });
            expired += before - queue.len();
            !queue.is_empty()
        });
        expired
    }

    fn queue_pending_message(&self, session_key: &str, msg: ServerToProxy) {
        let mut queue = self
            .pending_messages
            .entry(SessionId::from(session_key))
            .or_default();

        let now = Instant::now();
        queue.retain(|message| {
            now.saturating_duration_since(message.queued_at) < MAX_PENDING_MESSAGE_AGE
        });
        while queue.len() >= MAX_PENDING_MESSAGES_PER_SESSION {
            if let Some(dropped) = queue.pop_front() {
                warn!(
                    "Pending message queue full for session {}, dropping oldest message (age: {:?})",
                    session_key,
                    Instant::now().duration_since(dropped.queued_at)
                );
            }
        }

        queue.push_back(PendingMessage {
            msg,
            queued_at: Instant::now(),
        });

        info!(
            "Queued message for disconnected proxy, session: {}, queue size: {}",
            session_key,
            queue.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::make_output;
    use super::*;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    fn register(mgr: &SessionManager, key: &str, sender: super::super::ProxySender) -> u64 {
        mgr.register_session(key.into(), sender, CancellationToken::new())
    }

    #[test]
    fn expired_offline_queues_are_reclaimed_without_reconnecting() {
        let mgr = SessionManager::new();
        mgr.send_to_session("gone", make_output(1));
        mgr.send_to_session("mixed", make_output(2));
        mgr.send_to_session("mixed", make_output(3));
        let expired_at = Instant::now() - MAX_PENDING_MESSAGE_AGE - Duration::from_secs(1);
        mgr.pending_messages.get_mut("gone").unwrap()[0].queued_at = expired_at;
        mgr.pending_messages.get_mut("mixed").unwrap()[0].queued_at = expired_at;
        assert_eq!(mgr.sweep_expired_pending_messages(), 2);
        assert!(!mgr.pending_messages.contains_key("gone"));
        assert_eq!(mgr.pending_messages.get("mixed").unwrap().len(), 1);
        let (tx, mut rx) = crate::handlers::websocket::conn_channel(4);
        register(&mgr, "mixed", tx);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
        assert_eq!(mgr.sweep_expired_pending_messages(), 0);
    }

    #[test]
    fn send_to_unregistered_queues_pending() {
        let mgr = SessionManager::new();

        assert!(mgr.send_to_session("s1", make_output(1)));
        assert!(mgr.send_to_session("s1", make_output(2)));

        let (tx, mut rx) = crate::handlers::websocket::conn_channel(64);
        register(&mgr, "s1", tx);

        let msg1 = rx.try_recv().unwrap();
        let msg2 = rx.try_recv().unwrap();
        assert!(matches!(msg1, ServerToProxy::SequencedInput { .. }));
        assert!(matches!(msg2, ServerToProxy::SequencedInput { .. }));

        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn pending_queue_overflow_drops_oldest() {
        let mgr = SessionManager::new();

        for i in 0..(MAX_PENDING_MESSAGES_PER_SESSION + 10) as u32 {
            mgr.send_to_session("s1", make_output(i));
        }

        // Must exceed MAX_PENDING_MESSAGES_PER_SESSION: registration replays
        // the whole pending queue into this channel in one burst.
        let (tx, mut rx) =
            crate::handlers::websocket::conn_channel(MAX_PENDING_MESSAGES_PER_SESSION * 2);
        register(&mgr, "s1", tx);

        let mut received = vec![];
        while let Ok(msg) = rx.try_recv() {
            received.push(msg);
        }

        assert_eq!(received.len(), MAX_PENDING_MESSAGES_PER_SESSION);

        if let ServerToProxy::SequencedInput { content, .. } = &received[0] {
            assert_eq!(content["n"], 10);
        } else {
            panic!("Expected SequencedInput");
        }
    }

    #[test]
    fn send_to_session_queues_message_returned_by_closed_channel() {
        let mgr = SessionManager::new();
        let (tx, rx) = crate::handlers::websocket::conn_channel(64);
        let cancel = CancellationToken::new();
        mgr.register_session("s1".into(), tx, cancel.clone());
        drop(rx);

        let queued = mgr.send_to_session(
            "s1",
            ServerToProxy::OutputAck {
                session_id: Uuid::nil(),
                ack_seq: 7,
            },
        );

        assert!(queued);
        let pending = mgr.pending_messages.get("s1").unwrap();
        assert_eq!(pending.len(), 1);
        assert!(matches!(
            pending.front().unwrap().msg,
            ServerToProxy::OutputAck { ack_seq: 7, .. }
        ));
        // #1256: the dead entry must also be evicted and its socket closed.
        assert!(!mgr.sessions.contains_key("s1"));
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn send_to_disconnected_session_queues_and_replays() {
        let mgr = SessionManager::new();
        let (tx, _rx) = crate::handlers::websocket::conn_channel(64);

        let gen = register(&mgr, "s1", tx);
        mgr.unregister_session("s1", Some(gen));

        mgr.send_to_session("s1", make_output(1));
        mgr.send_to_session("s1", make_output(2));

        let (tx2, mut rx2) = crate::handlers::websocket::conn_channel(64);
        register(&mgr, "s1", tx2);

        let msg1 = rx2.try_recv().unwrap();
        let msg2 = rx2.try_recv().unwrap();
        assert!(matches!(msg1, ServerToProxy::SequencedInput { .. }));
        assert!(matches!(msg2, ServerToProxy::SequencedInput { .. }));
    }
}
