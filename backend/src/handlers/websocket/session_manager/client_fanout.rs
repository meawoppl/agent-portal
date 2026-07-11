//! Web/user client registries and broadcast fanout, including the
//! everything-connected shutdown broadcast.

use shared::{ServerToClient, ServerToLauncher, ServerToProxy};
use tracing::info;
use uuid::Uuid;

use super::{SessionId, SessionManager, WebClientSender};

/// Fan a message out to every sender in `clients`, pruning senders whose
/// channel has closed. The message is moved (not cloned) into the final
/// send, so the common single-client case never deep-copies the payload.
fn fanout_to_clients(clients: &mut Vec<WebClientSender>, msg: ServerToClient) {
    let mut msg = Some(msg);
    let mut idx = 0;
    while idx < clients.len() {
        let payload = if idx + 1 == clients.len() {
            msg.take()
        } else {
            msg.clone()
        };
        let Some(payload) = payload else { return };
        if clients[idx].send(payload).is_ok() {
            idx += 1;
        } else {
            clients.remove(idx);
        }
    }
}

impl SessionManager {
    pub fn add_web_client(&self, session_key: SessionId, sender: WebClientSender) {
        info!("Adding web client for session: {}", session_key);
        self.web_clients
            .entry(session_key)
            .or_default()
            .push(sender);
    }

    pub fn broadcast_to_web_clients(&self, session_key: &SessionId, msg: ServerToClient) {
        if let Some(mut clients) = self.web_clients.get_mut(session_key) {
            fanout_to_clients(clients.value_mut(), msg);
        }
    }

    /// Eagerly remove `sender`'s connection from a session's web-client
    /// registry when its socket task ends, dropping the map entry once the
    /// last client leaves.
    ///
    /// WHY: presence must not contain dead senders. Push-notification
    /// suppression ("don't push if the user has a live web client") reads
    /// these registries; without eager removal, a disconnected tab/phone
    /// would keep suppressing pushes until the next failed send lazily
    /// pruned it (see docs/MOBILE_APPS_PLAN.md §8.2). The lazy prune in
    /// `fanout_to_clients` stays as a backstop.
    pub fn remove_web_client(&self, session_key: &SessionId, sender: &WebClientSender) {
        if let Some(mut clients) = self.web_clients.get_mut(session_key) {
            clients.retain(|c| !c.same_channel(sender));
            if clients.is_empty() {
                drop(clients);
                self.web_clients.remove(session_key);
            }
        }
    }

    pub fn add_user_client(&self, user_id: Uuid, sender: WebClientSender) {
        info!("Adding web client for user: {}", user_id);
        self.user_clients.entry(user_id).or_default().push(sender);
    }

    pub fn broadcast_to_user(&self, user_id: &Uuid, msg: ServerToClient) {
        if let Some(mut clients) = self.user_clients.get_mut(user_id) {
            fanout_to_clients(clients.value_mut(), msg);
        }
    }

    /// Eagerly remove `sender`'s connection from a user's client registry
    /// when its socket task ends, dropping the map entry once the user's
    /// last client leaves.
    ///
    /// WHY: presence must not contain dead senders. Push-notification
    /// suppression ("don't push if the user has a live web client") reads
    /// `user_clients` (and treats a present key as "user is watching");
    /// without eager removal, a disconnected tab/phone would keep
    /// suppressing pushes until the next failed send lazily pruned it (see
    /// docs/MOBILE_APPS_PLAN.md §8.2). The lazy prune in `fanout_to_clients`
    /// stays as a backstop.
    pub fn remove_user_client(&self, user_id: &Uuid, sender: &WebClientSender) {
        if let Some(mut clients) = self.user_clients.get_mut(user_id) {
            clients.retain(|c| !c.same_channel(sender));
            if clients.is_empty() {
                drop(clients);
                self.user_clients.remove(user_id);
            }
        }
    }

    pub fn get_all_user_ids(&self) -> Vec<Uuid> {
        self.user_clients.iter().map(|r| *r.key()).collect()
    }

    /// Whether `user_id` currently has at least one live web client. Used by the
    /// push dispatcher to suppress a push when in-app WS delivery already covers
    /// the user (mobile-apps plan §8.2). Presence is trustworthy because dead
    /// senders are pruned eagerly on socket close (#1291): an empty (or absent)
    /// vector means genuinely no live client.
    pub fn has_user_client(&self, user_id: Uuid) -> bool {
        self.user_clients
            .get(&user_id)
            .is_some_and(|clients| !clients.value().is_empty())
    }

    /// Broadcast a shutdown message to all connected clients of every type.
    pub fn broadcast_shutdown(&self, reason: String, reconnect_delay_ms: u64) {
        let proxy_msg = ServerToProxy::ServerShutdown {
            reason: reason.clone(),
            reconnect_delay_ms,
        };
        // Collect keys first, then send through the evicting path: a direct
        // send off the iter guard would bypass dead-connection eviction, and
        // eviction must not run under the shard lock the iterator holds.
        let session_keys: Vec<_> = self.sessions.iter().map(|e| e.key().clone()).collect();
        for key in session_keys {
            self.send_to_connected_session(&key, proxy_msg.clone());
        }

        let client_msg = ServerToClient::ServerShutdown {
            reason: reason.clone(),
            reconnect_delay_ms,
        };
        for mut entry in self.web_clients.iter_mut() {
            fanout_to_clients(entry.value_mut(), client_msg.clone());
        }
        for mut entry in self.user_clients.iter_mut() {
            fanout_to_clients(entry.value_mut(), client_msg.clone());
        }

        let launcher_msg = ServerToLauncher::ServerShutdown {
            reason,
            reconnect_delay_ms,
        };
        let launcher_ids: Vec<_> = self.launchers.iter().map(|e| *e.key()).collect();
        for id in launcher_ids {
            self.send_to_launcher(&id, launcher_msg.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::make_client_msg;
    use super::*;

    #[test]
    fn broadcast_to_web_clients() {
        let mgr = SessionManager::new();
        let (tx1, mut rx1) = crate::handlers::websocket::conn_channel(64);
        let (tx2, mut rx2) = crate::handlers::websocket::conn_channel(64);

        mgr.add_web_client("s1".into(), tx1);
        mgr.add_web_client("s1".into(), tx2);

        mgr.broadcast_to_web_clients(&"s1".into(), make_client_msg());

        assert!(matches!(
            rx1.try_recv().unwrap(),
            ServerToClient::AgentOutput { .. }
        ));
        assert!(matches!(
            rx2.try_recv().unwrap(),
            ServerToClient::AgentOutput { .. }
        ));
    }

    #[test]
    fn broadcast_removes_closed_clients() {
        let mgr = SessionManager::new();
        let (tx1, rx1) = crate::handlers::websocket::conn_channel(64);
        let (tx2, mut rx2) = crate::handlers::websocket::conn_channel(64);

        mgr.add_web_client("s1".into(), tx1);
        mgr.add_web_client("s1".into(), tx2);

        drop(rx1);

        mgr.broadcast_to_web_clients(&"s1".into(), make_client_msg());

        assert!(matches!(
            rx2.try_recv().unwrap(),
            ServerToClient::AgentOutput { .. }
        ));

        let clients = mgr.web_clients.get("s1").unwrap();
        assert_eq!(clients.len(), 1);
    }

    #[test]
    fn broadcast_delivers_when_last_client_closed() {
        // The fanout moves the payload into the final send; make sure a
        // closed channel in that last slot still prunes correctly and the
        // open clients before it were already served.
        let mgr = SessionManager::new();
        let (tx1, mut rx1) = crate::handlers::websocket::conn_channel(64);
        let (tx2, mut rx2) = crate::handlers::websocket::conn_channel(64);
        let (tx3, rx3) = crate::handlers::websocket::conn_channel(64);

        mgr.add_web_client("s1".into(), tx1);
        mgr.add_web_client("s1".into(), tx2);
        mgr.add_web_client("s1".into(), tx3);

        drop(rx3);

        mgr.broadcast_to_web_clients(&"s1".into(), make_client_msg());

        assert!(matches!(
            rx1.try_recv().unwrap(),
            ServerToClient::AgentOutput { .. }
        ));
        assert!(matches!(
            rx2.try_recv().unwrap(),
            ServerToClient::AgentOutput { .. }
        ));
        let clients = mgr.web_clients.get("s1").unwrap();
        assert_eq!(clients.len(), 2);
    }

    #[test]
    fn broadcast_to_user() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let (tx, mut rx) = crate::handlers::websocket::conn_channel(64);

        mgr.add_user_client(user_id, tx);
        mgr.broadcast_to_user(&user_id, make_client_msg());

        assert!(matches!(
            rx.try_recv().unwrap(),
            ServerToClient::AgentOutput { .. }
        ));
    }

    #[test]
    fn broadcast_shutdown_reaches_all() {
        let mgr = SessionManager::new();
        let (session_tx, mut session_rx) = crate::handlers::websocket::conn_channel(64);
        let (web_tx, mut web_rx) = crate::handlers::websocket::conn_channel(64);
        let (user_tx, mut user_rx) = crate::handlers::websocket::conn_channel(64);

        mgr.register_session(
            "s1".into(),
            session_tx,
            tokio_util::sync::CancellationToken::new(),
        );
        mgr.add_web_client("s1".into(), web_tx);
        mgr.add_user_client(Uuid::new_v4(), user_tx);

        mgr.broadcast_shutdown("test".into(), 1000);

        assert!(matches!(
            session_rx.try_recv().unwrap(),
            ServerToProxy::ServerShutdown { .. }
        ));
        assert!(matches!(
            web_rx.try_recv().unwrap(),
            ServerToClient::ServerShutdown { .. }
        ));
        assert!(matches!(
            user_rx.try_recv().unwrap(),
            ServerToClient::ServerShutdown { .. }
        ));
    }

    #[test]
    fn remove_web_client_drops_entry_when_empty() {
        let mgr = SessionManager::new();
        let (tx1, _rx1) = crate::handlers::websocket::conn_channel(64);
        let (tx2, _rx2) = crate::handlers::websocket::conn_channel(64);

        mgr.add_web_client("s1".into(), tx1.clone());
        mgr.add_web_client("s1".into(), tx2.clone());

        // Removing one sender leaves the other and keeps the entry.
        mgr.remove_web_client(&"s1".into(), &tx1);
        assert_eq!(mgr.web_clients.get("s1").unwrap().len(), 1);

        // Removing the last sender drops the map entry entirely, so presence
        // checks (map contains session) stay correct.
        mgr.remove_web_client(&"s1".into(), &tx2);
        assert!(mgr.web_clients.get("s1").is_none());
    }

    #[test]
    fn remove_user_client_drops_entry_when_empty() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let (tx1, _rx1) = crate::handlers::websocket::conn_channel(64);
        let (tx2, _rx2) = crate::handlers::websocket::conn_channel(64);

        mgr.add_user_client(user_id, tx1.clone());
        mgr.add_user_client(user_id, tx2.clone());

        mgr.remove_user_client(&user_id, &tx1);
        assert_eq!(mgr.user_clients.get(&user_id).unwrap().len(), 1);

        mgr.remove_user_client(&user_id, &tx2);
        assert!(mgr.user_clients.get(&user_id).is_none());
        // Presence query must no longer see the user.
        assert!(!mgr.get_all_user_ids().contains(&user_id));
    }

    #[test]
    fn has_user_client_presence() {
        let mgr = SessionManager::new();
        let present = Uuid::new_v4();
        let absent = Uuid::new_v4();
        let (tx, _rx) = crate::handlers::websocket::conn_channel(64);

        assert!(!mgr.has_user_client(present));
        mgr.add_user_client(present, tx);
        assert!(mgr.has_user_client(present));
        // A user with no registered client (never added) is absent.
        assert!(!mgr.has_user_client(absent));
    }

    #[test]
    fn has_user_client_false_after_prune_empties_vec() {
        // A dead sender pruned to an empty vec must read as "no live client"
        // so the push dispatcher does not wrongly suppress (§8.2).
        let mgr = SessionManager::new();
        let user = Uuid::new_v4();
        let (tx, rx) = crate::handlers::websocket::conn_channel(64);
        mgr.add_user_client(user, tx);
        drop(rx);
        // Prune the dead sender via a fanout (fanout removes closed senders).
        mgr.broadcast_to_user(&user, make_client_msg());
        assert!(!mgr.has_user_client(user));
    }

    #[test]
    fn get_all_user_ids() {
        let mgr = SessionManager::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let (tx1, _rx1) = crate::handlers::websocket::conn_channel(64);
        let (tx2, _rx2) = crate::handlers::websocket::conn_channel(64);

        mgr.add_user_client(id1, tx1);
        mgr.add_user_client(id2, tx2);

        let ids = mgr.get_all_user_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }
}
