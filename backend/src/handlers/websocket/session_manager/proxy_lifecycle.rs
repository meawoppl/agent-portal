//! Proxy connection lifecycle: registration with generation tracking,
//! stale-safe unregistration, and user-initiated disconnect.

use std::sync::atomic::Ordering;

use shared::ServerToProxy;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use super::{ProxyConnection, ProxySender, SessionId, SessionManager};

impl SessionManager {
    /// Register a proxy connection for a session. Returns a generation number
    /// that must be passed to `unregister_session` to prevent stale cleanup
    /// from removing a newer connection. `cancel` is the socket task's
    /// cancellation token; the manager fires it to force-close a connection
    /// whose channel has died (see `evict_dead_connection`).
    pub fn register_session(
        &self,
        session_key: SessionId,
        sender: ProxySender,
        cancel: CancellationToken,
    ) -> u64 {
        let gen = self.gen_counter.fetch_add(1, Ordering::Relaxed);
        info!("Registering session: {} (gen={})", session_key, gen);

        let pending_count = self.replay_pending_messages(&session_key, &sender);
        if pending_count > 0 {
            info!(
                "Replayed {} pending messages to reconnected proxy for session: {}",
                pending_count, session_key
            );
        }

        self.sessions.insert(
            session_key,
            ProxyConnection {
                sender,
                gen,
                cancel,
                last_seen: std::sync::atomic::AtomicU64::new(super::liveness::epoch_secs()),
            },
        );
        gen
    }

    /// Unregister a proxy connection. If `gen` is provided, only removes the
    /// entry when it matches the current generation — preventing a stale
    /// connection's cleanup from removing a newer connection's registration.
    /// Pass `None` to force removal (e.g. admin delete).
    pub fn unregister_session(&self, session_key: &SessionId, gen: Option<u64>) {
        let removed = match gen {
            // Atomic compare-and-remove under one shard lock (the previous
            // separate check-then-remove had a small TOCTOU window).
            Some(expected) => self
                .sessions
                .remove_if(session_key, |_, conn| conn.gen == expected)
                .is_some(),
            None => self.sessions.remove(session_key).is_some(),
        };
        if removed {
            info!("Unregistering session: {}", session_key);
        } else if gen.is_some() {
            info!(
                "Skipping stale unregister for session {} (superseded or already gone)",
                session_key
            );
        }
    }

    /// A send to this registered connection failed: its channel is closed, so
    /// the socket task is dead — or wedged half-open (inbound frames still
    /// arriving, outbound writer gone: the #1256 incident shape). Evict the
    /// registry entry (generation-guarded) and cancel the socket task so the
    /// transport actually closes and the peer's reconnect logic takes over,
    /// instead of the send failure looping silently forever.
    /// Returns whether this call actually removed the entry (false when a
    /// newer generation had already replaced it — the socket is still
    /// cancelled either way).
    pub(super) fn evict_dead_connection(
        &self,
        session_key: &SessionId,
        gen: u64,
        cancel: &CancellationToken,
    ) -> bool {
        let removed = self
            .sessions
            .remove_if(session_key, |_, conn| conn.gen == gen)
            .is_some();
        if removed {
            warn!(
                "Evicted dead proxy connection for session {} (gen {}); closing its socket",
                session_key, gen
            );
        }
        cancel.cancel();
        removed
    }

    /// Check whether the given generation is still the current connection for
    /// this session. Used by cleanup code to avoid overwriting a newer
    /// connection's DB status.
    pub fn is_current_connection(&self, session_key: &SessionId, gen: u64) -> bool {
        self.sessions
            .get(session_key)
            .is_none_or(|conn| conn.gen == gen)
    }

    /// The current connection generation for a session, if a proxy is
    /// registered. Used to bind a tunnel stream to the exact connection it
    /// was opened on.
    pub fn current_connection_gen(&self, session_key: &str) -> Option<u64> {
        self.sessions.get(session_key).map(|conn| conn.gen)
    }

    /// Stop a directly-connected proxy by sending it a termination message.
    /// The proxy will disconnect without attempting to reconnect.
    /// Returns true if the session was found and the message was sent.
    pub fn disconnect_session(&self, session_id: Uuid) -> bool {
        let key = session_id.to_string();
        // Evicting send: a dead channel tears the stale entry down (and
        // closes the socket) instead of lingering half-registered.
        self.send_to_connected_session(
            &key,
            ServerToProxy::SessionTerminated {
                reason: "Session stopped by user".to_string(),
            },
        )
    }

    /// Return the set of session keys that currently have a registered proxy connection.
    pub fn registered_session_keys(&self) -> Vec<SessionId> {
        self.sessions.iter().map(|r| r.key().clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::make_heartbeat;
    use super::*;

    fn register(mgr: &SessionManager, key: &str, sender: ProxySender) -> u64 {
        mgr.register_session(key.into(), sender, CancellationToken::new())
    }

    #[test]
    fn session_register_and_send() {
        let mgr = SessionManager::new();
        let (tx, mut rx) = crate::handlers::websocket::conn_channel(64);

        register(&mgr, "s1", tx);

        assert!(mgr.send_to_session(&"s1".into(), make_heartbeat()));

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerToProxy::Heartbeat));
    }

    #[test]
    fn unregister_removes_session() {
        let mgr = SessionManager::new();
        let (tx, _rx) = crate::handlers::websocket::conn_channel(64);

        let gen = register(&mgr, "s1", tx);
        assert!(mgr.sessions.contains_key("s1"));

        mgr.unregister_session(&"s1".into(), Some(gen));
        assert!(!mgr.sessions.contains_key("s1"));
    }

    #[test]
    fn unregister_force_removes_without_gen() {
        let mgr = SessionManager::new();
        let (tx, _rx) = crate::handlers::websocket::conn_channel(64);

        register(&mgr, "s1", tx);
        assert!(mgr.sessions.contains_key("s1"));

        mgr.unregister_session(&"s1".into(), None);
        assert!(!mgr.sessions.contains_key("s1"));
    }

    #[test]
    fn stale_unregister_is_noop() {
        let mgr = SessionManager::new();
        let (tx1, _rx1) = crate::handlers::websocket::conn_channel(64);
        let (tx2, mut rx2) = crate::handlers::websocket::conn_channel(64);

        // Old connection registers
        let old_gen = register(&mgr, "s1", tx1);

        // New connection registers (simulates reconnect)
        let _new_gen = register(&mgr, "s1", tx2);

        // Old connection's cleanup tries to unregister with stale gen
        mgr.unregister_session(&"s1".into(), Some(old_gen));

        // Session should still be registered with the new sender
        assert!(mgr.sessions.contains_key("s1"));
        assert!(mgr.send_to_session(&"s1".into(), make_heartbeat()));
        assert!(matches!(rx2.try_recv().unwrap(), ServerToProxy::Heartbeat));
    }

    #[test]
    fn is_current_connection_checks_gen() {
        let mgr = SessionManager::new();
        let (tx1, _rx1) = crate::handlers::websocket::conn_channel(64);

        let gen1 = register(&mgr, "s1", tx1);
        assert!(mgr.is_current_connection(&"s1".into(), gen1));

        let (tx2, _rx2) = crate::handlers::websocket::conn_channel(64);
        let gen2 = register(&mgr, "s1", tx2);
        assert!(!mgr.is_current_connection(&"s1".into(), gen1));
        assert!(mgr.is_current_connection(&"s1".into(), gen2));
    }

    /// #1256: a send to a registered connection whose channel has died must
    /// evict the entry, fire its cancel token (so the socket task closes the
    /// transport), and still queue the message for the successor.
    #[test]
    fn send_failure_evicts_and_cancels_dead_connection() {
        let mgr = SessionManager::new();
        let (tx, rx) = crate::handlers::websocket::conn_channel(64);
        let cancel = CancellationToken::new();
        mgr.register_session("s1".into(), tx, cancel.clone());

        // Kill the receiving side: the socket task is "dead".
        drop(rx);

        // The direct send fails internally; the message is queued for the
        // successor (hence `true`), the dead entry is evicted, and its
        // socket is told to close.
        assert!(mgr.send_to_session(&"s1".into(), make_heartbeat()));
        assert!(!mgr.sessions.contains_key("s1"));
        assert!(cancel.is_cancelled());

        // A reconnect replays the queued message.
        let (tx2, mut rx2) = crate::handlers::websocket::conn_channel(64);
        register(&mgr, "s1", tx2);
        assert!(matches!(rx2.try_recv().unwrap(), ServerToProxy::Heartbeat));
    }

    /// The eviction must be generation-guarded: if a successor has already
    /// re-registered by the time the dead connection's send failure is
    /// observed, the successor's entry must survive.
    #[test]
    fn evict_dead_connection_spares_successor() {
        let mgr = SessionManager::new();
        let (tx1, rx1) = crate::handlers::websocket::conn_channel(64);
        let cancel1 = CancellationToken::new();
        let gen1 = mgr.register_session("s1".into(), tx1, cancel1.clone());

        let (tx2, mut rx2) = crate::handlers::websocket::conn_channel(64);
        register(&mgr, "s1", tx2);

        drop(rx1);
        mgr.evict_dead_connection(&"s1".into(), gen1, &cancel1);

        // Successor survives and still receives.
        assert!(mgr.sessions.contains_key("s1"));
        assert!(mgr.send_to_session(&"s1".into(), make_heartbeat()));
        assert!(matches!(rx2.try_recv().unwrap(), ServerToProxy::Heartbeat));
        // The dead connection's socket still gets closed.
        assert!(cancel1.is_cancelled());
    }
}
