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

impl SessionManager {
    pub fn send_to_session(&self, session_key: &SessionId, msg: ServerToProxy) -> bool {
        let msg = match self.sessions.get(session_key) {
            Some(sender) => match sender.send(msg) {
                Ok(()) => return true,
                // A closed channel hands the message back in the error, so
                // there's no need to clone up front just in case.
                Err(mpsc::error::SendError(msg)) => msg,
            },
            None => msg,
        };

        self.queue_pending_message(session_key, msg)
    }

    pub fn send_to_connected_session(&self, session_key: &SessionId, msg: ServerToProxy) -> bool {
        self.sessions
            .get(session_key)
            .is_some_and(|sender| sender.send(msg).is_ok())
    }

    /// Drain this session's pending queue into `sender`, dropping entries
    /// older than [`MAX_PENDING_MESSAGE_AGE`]. Called on proxy registration.
    pub(super) fn replay_pending_messages(
        &self,
        session_key: &SessionId,
        sender: &ProxySender,
    ) -> usize {
        let mut replayed = 0;
        let now = Instant::now();

        if let Some(mut pending) = self.pending_messages.get_mut(session_key) {
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

        self.pending_messages.remove(session_key);
        replayed
    }

    fn queue_pending_message(&self, session_key: &SessionId, msg: ServerToProxy) -> bool {
        let mut queue = self
            .pending_messages
            .entry(session_key.clone())
            .or_default();

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

        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::make_output;
    use super::*;
    use uuid::Uuid;

    #[test]
    fn send_to_unregistered_queues_pending() {
        let mgr = SessionManager::new();

        assert!(mgr.send_to_session(&"s1".into(), make_output(1)));
        assert!(mgr.send_to_session(&"s1".into(), make_output(2)));

        let (tx, mut rx) = mpsc::unbounded_channel();
        mgr.register_session("s1".into(), tx);

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
            mgr.send_to_session(&"s1".into(), make_output(i));
        }

        let (tx, mut rx) = mpsc::unbounded_channel();
        mgr.register_session("s1".into(), tx);

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
        let (tx, rx) = mpsc::unbounded_channel();
        mgr.register_session("s1".into(), tx);
        drop(rx);

        let queued = mgr.send_to_session(
            &"s1".into(),
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
    }

    #[test]
    fn send_to_disconnected_session_queues_and_replays() {
        let mgr = SessionManager::new();
        let (tx, _rx) = mpsc::unbounded_channel();

        let gen = mgr.register_session("s1".into(), tx);
        mgr.unregister_session(&"s1".into(), Some(gen));

        mgr.send_to_session(&"s1".into(), make_output(1));
        mgr.send_to_session(&"s1".into(), make_output(2));

        let (tx2, mut rx2) = mpsc::unbounded_channel();
        mgr.register_session("s1".into(), tx2);

        let msg1 = rx2.try_recv().unwrap();
        let msg2 = rx2.try_recv().unwrap();
        assert!(matches!(msg1, ServerToProxy::SequencedInput { .. }));
        assert!(matches!(msg2, ServerToProxy::SequencedInput { .. }));
    }
}
