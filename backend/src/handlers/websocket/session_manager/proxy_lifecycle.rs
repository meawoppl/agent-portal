//! Proxy connection lifecycle: registration with generation tracking,
//! stale-safe unregistration, and user-initiated disconnect.

use std::sync::atomic::Ordering;

use shared::ServerToProxy;
use tracing::info;
use uuid::Uuid;

use super::{ProxySender, SessionId, SessionManager};

impl SessionManager {
    /// Register a proxy connection for a session. Returns a generation number
    /// that must be passed to `unregister_session` to prevent stale cleanup
    /// from removing a newer connection.
    pub fn register_session(&self, session_key: SessionId, sender: ProxySender) -> u64 {
        let gen = self.gen_counter.fetch_add(1, Ordering::Relaxed);
        info!("Registering session: {} (gen={})", session_key, gen);

        let pending_count = self.replay_pending_messages(&session_key, &sender);
        if pending_count > 0 {
            info!(
                "Replayed {} pending messages to reconnected proxy for session: {}",
                pending_count, session_key
            );
        }

        self.connection_gen.insert(session_key.clone(), gen);
        self.sessions.insert(session_key, sender);
        gen
    }

    /// Unregister a proxy connection. If `gen` is provided, only removes the
    /// entry when it matches the current generation — preventing a stale
    /// connection's cleanup from removing a newer connection's registration.
    /// Pass `None` to force removal (e.g. admin delete).
    pub fn unregister_session(&self, session_key: &SessionId, gen: Option<u64>) {
        if let Some(expected) = gen {
            if let Some(current) = self.connection_gen.get(session_key) {
                if *current != expected {
                    info!(
                        "Skipping stale unregister for session {} (gen {} != current {})",
                        session_key, expected, *current
                    );
                    return;
                }
            }
        }
        info!("Unregistering session: {}", session_key);
        self.connection_gen.remove(session_key);
        self.sessions.remove(session_key);
    }

    /// Check whether the given generation is still the current connection for
    /// this session. Used by cleanup code to avoid overwriting a newer
    /// connection's DB status.
    pub fn is_current_connection(&self, session_key: &SessionId, gen: u64) -> bool {
        self.connection_gen
            .get(session_key)
            .is_none_or(|current| *current == gen)
    }

    /// Stop a directly-connected proxy by sending it a termination message.
    /// The proxy will disconnect without attempting to reconnect.
    /// Returns true if the session was found and the message was sent.
    pub fn disconnect_session(&self, session_id: Uuid) -> bool {
        let key = session_id.to_string();
        if let Some(sender) = self.sessions.get(&key) {
            sender
                .send(ServerToProxy::SessionTerminated {
                    reason: "Session stopped by user".to_string(),
                })
                .is_ok()
        } else {
            false
        }
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
    use tokio::sync::mpsc;

    #[test]
    fn session_register_and_send() {
        let mgr = SessionManager::new();
        let (tx, mut rx) = mpsc::unbounded_channel();

        mgr.register_session("s1".into(), tx);

        assert!(mgr.send_to_session(&"s1".into(), make_heartbeat()));

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, ServerToProxy::Heartbeat));
    }

    #[test]
    fn unregister_removes_session() {
        let mgr = SessionManager::new();
        let (tx, _rx) = mpsc::unbounded_channel();

        let gen = mgr.register_session("s1".into(), tx);
        assert!(mgr.sessions.contains_key("s1"));

        mgr.unregister_session(&"s1".into(), Some(gen));
        assert!(!mgr.sessions.contains_key("s1"));
    }

    #[test]
    fn unregister_force_removes_without_gen() {
        let mgr = SessionManager::new();
        let (tx, _rx) = mpsc::unbounded_channel();

        mgr.register_session("s1".into(), tx);
        assert!(mgr.sessions.contains_key("s1"));

        mgr.unregister_session(&"s1".into(), None);
        assert!(!mgr.sessions.contains_key("s1"));
    }

    #[test]
    fn stale_unregister_is_noop() {
        let mgr = SessionManager::new();
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();

        // Old connection registers
        let old_gen = mgr.register_session("s1".into(), tx1);

        // New connection registers (simulates reconnect)
        let _new_gen = mgr.register_session("s1".into(), tx2);

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
        let (tx1, _rx1) = mpsc::unbounded_channel();

        let gen1 = mgr.register_session("s1".into(), tx1);
        assert!(mgr.is_current_connection(&"s1".into(), gen1));

        let (tx2, _rx2) = mpsc::unbounded_channel();
        let gen2 = mgr.register_session("s1".into(), tx2);
        assert!(!mgr.is_current_connection(&"s1".into(), gen1));
        assert!(mgr.is_current_connection(&"s1".into(), gen2));
    }
}
