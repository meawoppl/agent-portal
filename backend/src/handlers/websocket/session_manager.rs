use dashmap::{DashMap, DashSet};
use shared::{LauncherToServer, ServerToClient, ServerToLauncher, ServerToProxy};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use uuid::Uuid;

use shared::protocol::{MAX_PENDING_MESSAGES_PER_SESSION, MAX_PENDING_MESSAGE_AGE_SECS};

/// Maximum age of pending messages before they're dropped
const MAX_PENDING_MESSAGE_AGE: Duration = Duration::from_secs(MAX_PENDING_MESSAGE_AGE_SECS);

/// A message queued for a disconnected proxy
#[derive(Clone)]
struct PendingMessage {
    msg: ServerToProxy,
    queued_at: Instant,
}

pub type SessionId = String;
pub type ProxySender = mpsc::UnboundedSender<ServerToProxy>;
pub type WebClientSender = mpsc::UnboundedSender<ServerToClient>;
pub type LauncherSender = mpsc::UnboundedSender<ServerToLauncher>;

/// A connected launcher daemon
pub struct LauncherConnection {
    pub sender: LauncherSender,
    pub launcher_name: String,
    pub hostname: String,
    pub user_id: Uuid,
    pub running_sessions: Vec<Uuid>,
}

#[derive(Clone)]
pub struct SessionManager {
    pub sessions: Arc<DashMap<SessionId, ProxySender>>,
    pub web_clients: Arc<DashMap<SessionId, Vec<WebClientSender>>>,
    pub user_clients: Arc<DashMap<Uuid, Vec<WebClientSender>>>,
    pub last_ack_seq: Arc<DashMap<Uuid, u64>>,
    pending_messages: Arc<DashMap<SessionId, VecDeque<PendingMessage>>>,
    pub pending_truncations: Arc<DashSet<Uuid>>,
    pub launchers: Arc<DashMap<Uuid, LauncherConnection>>,
    pub pending_dir_requests: Arc<DashMap<Uuid, oneshot::Sender<LauncherToServer>>>,
    /// Tracks who sent the last input for each session (session_id → (user_id, display_name))
    pub last_input_sender: Arc<DashMap<Uuid, (Uuid, String)>>,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            web_clients: Arc::new(DashMap::new()),
            user_clients: Arc::new(DashMap::new()),
            last_ack_seq: Arc::new(DashMap::new()),
            pending_messages: Arc::new(DashMap::new()),
            pending_truncations: Arc::new(DashSet::new()),
            launchers: Arc::new(DashMap::new()),
            pending_dir_requests: Arc::new(DashMap::new()),
            last_input_sender: Arc::new(DashMap::new()),
        }
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_session(&self, session_key: SessionId, sender: ProxySender) {
        info!("Registering session: {}", session_key);

        let pending_count = self.replay_pending_messages(&session_key, &sender);
        if pending_count > 0 {
            info!(
                "Replayed {} pending messages to reconnected proxy for session: {}",
                pending_count, session_key
            );
        }

        self.sessions.insert(session_key, sender);
    }

    fn replay_pending_messages(&self, session_key: &SessionId, sender: &ProxySender) -> usize {
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

    pub fn unregister_session(&self, session_key: &SessionId) {
        info!("Unregistering session: {}", session_key);
        self.sessions.remove(session_key);
    }

    pub fn add_web_client(&self, session_key: SessionId, sender: WebClientSender) {
        info!("Adding web client for session: {}", session_key);
        self.web_clients
            .entry(session_key)
            .or_default()
            .push(sender);
    }

    pub fn broadcast_to_web_clients(&self, session_key: &SessionId, msg: ServerToClient) {
        if let Some(mut clients) = self.web_clients.get_mut(session_key) {
            clients.retain(|sender| sender.send(msg.clone()).is_ok());
        }
    }

    pub fn send_to_session(&self, session_key: &SessionId, msg: ServerToProxy) -> bool {
        if let Some(sender) = self.sessions.get(session_key) {
            if sender.send(msg.clone()).is_ok() {
                return true;
            }
        }

        self.queue_pending_message(session_key, msg)
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

    pub fn add_user_client(&self, user_id: Uuid, sender: WebClientSender) {
        info!("Adding web client for user: {}", user_id);
        self.user_clients.entry(user_id).or_default().push(sender);
    }

    pub fn broadcast_to_user(&self, user_id: &Uuid, msg: ServerToClient) {
        if let Some(mut clients) = self.user_clients.get_mut(user_id) {
            clients.retain(|sender| sender.send(msg.clone()).is_ok());
        }
    }

    pub fn get_all_user_ids(&self) -> Vec<Uuid> {
        self.user_clients.iter().map(|r| *r.key()).collect()
    }

    /// Broadcast a shutdown message to all connected clients of every type.
    pub fn broadcast_shutdown(&self, reason: String, reconnect_delay_ms: u64) {
        let proxy_msg = ServerToProxy::ServerShutdown {
            reason: reason.clone(),
            reconnect_delay_ms,
        };
        for entry in self.sessions.iter() {
            let _ = entry.value().send(proxy_msg.clone());
        }

        let client_msg = ServerToClient::ServerShutdown {
            reason: reason.clone(),
            reconnect_delay_ms,
        };
        for mut entry in self.web_clients.iter_mut() {
            entry
                .value_mut()
                .retain(|sender| sender.send(client_msg.clone()).is_ok());
        }
        for mut entry in self.user_clients.iter_mut() {
            entry
                .value_mut()
                .retain(|sender| sender.send(client_msg.clone()).is_ok());
        }

        let launcher_msg = ServerToLauncher::ServerShutdown {
            reason,
            reconnect_delay_ms,
        };
        for entry in self.launchers.iter() {
            let _ = entry.value().sender.send(launcher_msg.clone());
        }
    }

    pub fn queue_truncation(&self, session_id: Uuid) {
        self.pending_truncations.insert(session_id);
    }

    pub fn drain_pending_truncations(&self) -> Vec<Uuid> {
        let ids: Vec<Uuid> = self.pending_truncations.iter().map(|r| *r).collect();
        for id in &ids {
            self.pending_truncations.remove(id);
        }
        ids
    }

    pub fn register_launcher(&self, launcher_id: Uuid, connection: LauncherConnection) {
        info!(
            "Registering launcher: {} ({})",
            connection.launcher_name, launcher_id
        );
        self.launchers.insert(launcher_id, connection);
    }

    pub fn unregister_launcher(&self, launcher_id: &Uuid) {
        info!("Unregistering launcher: {}", launcher_id);
        self.launchers.remove(launcher_id);
    }

    pub fn get_launchers_for_user(&self, user_id: &Uuid) -> Vec<shared::LauncherInfo> {
        self.launchers
            .iter()
            .filter(|entry| entry.value().user_id == *user_id)
            .map(|entry| shared::LauncherInfo {
                launcher_id: *entry.key(),
                launcher_name: entry.value().launcher_name.clone(),
                hostname: entry.value().hostname.clone(),
                connected: true,
                running_sessions: entry.value().running_sessions.len() as u32,
            })
            .collect()
    }

    pub fn send_to_launcher(&self, launcher_id: &Uuid, msg: ServerToLauncher) -> bool {
        if let Some(launcher) = self.launchers.get(launcher_id) {
            launcher.sender.send(msg).is_ok()
        } else {
            false
        }
    }

    /// Find the launcher running a given session and send StopSession to it.
    /// Returns true if the message was sent successfully.
    pub fn stop_session_on_launcher(&self, session_id: Uuid) -> bool {
        for entry in self.launchers.iter() {
            if entry.value().running_sessions.contains(&session_id) {
                return entry
                    .value()
                    .sender
                    .send(ServerToLauncher::StopSession { session_id })
                    .is_ok();
            }
        }
        false
    }

    /// Stop a directly-connected proxy by sending it a shutdown message.
    /// The proxy will disconnect, and the session will be marked as disconnected in the DB.
    /// Returns true if the session was found and the message was sent.
    pub fn disconnect_session(&self, session_id: Uuid) -> bool {
        let key = session_id.to_string();
        if let Some(sender) = self.sessions.get(&key) {
            let _ = sender.send(ServerToProxy::ServerShutdown {
                reason: "Session stopped by user".to_string(),
                reconnect_delay_ms: 30_000,
            });
            true
        } else {
            false
        }
    }

    pub fn register_dir_request(&self, request_id: Uuid) -> oneshot::Receiver<LauncherToServer> {
        let (tx, rx) = oneshot::channel();
        self.pending_dir_requests.insert(request_id, tx);
        rx
    }

    pub fn complete_dir_request(&self, request_id: Uuid, msg: LauncherToServer) {
        if let Some((_, tx)) = self.pending_dir_requests.remove(&request_id) {
            let _ = tx.send(msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_heartbeat() -> ServerToProxy {
        ServerToProxy::Heartbeat
    }

    fn make_output(n: u32) -> ServerToProxy {
        ServerToProxy::SequencedInput {
            session_id: Uuid::nil(),
            seq: n as i64,
            content: serde_json::json!({"n": n}),
            send_mode: None,
        }
    }

    fn make_client_msg() -> ServerToClient {
        ServerToClient::ClaudeOutput {
            content: serde_json::json!({"text": "hello"}),
            sender_user_id: None,
            sender_name: None,
        }
    }

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
    fn unregister_removes_session() {
        let mgr = SessionManager::new();
        let (tx, _rx) = mpsc::unbounded_channel();

        mgr.register_session("s1".into(), tx);
        assert!(mgr.sessions.contains_key("s1"));

        mgr.unregister_session(&"s1".into());
        assert!(!mgr.sessions.contains_key("s1"));
    }

    #[test]
    fn broadcast_to_web_clients() {
        let mgr = SessionManager::new();
        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();

        mgr.add_web_client("s1".into(), tx1);
        mgr.add_web_client("s1".into(), tx2);

        mgr.broadcast_to_web_clients(&"s1".into(), make_client_msg());

        assert!(matches!(
            rx1.try_recv().unwrap(),
            ServerToClient::ClaudeOutput { .. }
        ));
        assert!(matches!(
            rx2.try_recv().unwrap(),
            ServerToClient::ClaudeOutput { .. }
        ));
    }

    #[test]
    fn broadcast_removes_closed_clients() {
        let mgr = SessionManager::new();
        let (tx1, rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();

        mgr.add_web_client("s1".into(), tx1);
        mgr.add_web_client("s1".into(), tx2);

        drop(rx1);

        mgr.broadcast_to_web_clients(&"s1".into(), make_client_msg());

        assert!(matches!(
            rx2.try_recv().unwrap(),
            ServerToClient::ClaudeOutput { .. }
        ));

        let clients = mgr.web_clients.get("s1").unwrap();
        assert_eq!(clients.len(), 1);
    }

    #[test]
    fn broadcast_to_user() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let (tx, mut rx) = mpsc::unbounded_channel();

        mgr.add_user_client(user_id, tx);
        mgr.broadcast_to_user(&user_id, make_client_msg());

        assert!(matches!(
            rx.try_recv().unwrap(),
            ServerToClient::ClaudeOutput { .. }
        ));
    }

    #[test]
    fn broadcast_shutdown_reaches_all() {
        let mgr = SessionManager::new();
        let (session_tx, mut session_rx) = mpsc::unbounded_channel();
        let (web_tx, mut web_rx) = mpsc::unbounded_channel();
        let (user_tx, mut user_rx) = mpsc::unbounded_channel();

        mgr.register_session("s1".into(), session_tx);
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
    fn truncation_queue_and_drain() {
        let mgr = SessionManager::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();

        mgr.queue_truncation(id1);
        mgr.queue_truncation(id2);
        mgr.queue_truncation(id1); // duplicate should be idempotent

        let drained = mgr.drain_pending_truncations();
        assert_eq!(drained.len(), 2);
        assert!(drained.contains(&id1));
        assert!(drained.contains(&id2));

        let drained2 = mgr.drain_pending_truncations();
        assert!(drained2.is_empty());
    }

    #[test]
    fn last_ack_seq_tracking() {
        let mgr = SessionManager::new();
        let session_id = Uuid::new_v4();

        assert!(mgr.last_ack_seq.get(&session_id).is_none());

        mgr.last_ack_seq.insert(session_id, 5);
        assert_eq!(*mgr.last_ack_seq.get(&session_id).unwrap(), 5);

        mgr.last_ack_seq.entry(session_id).and_modify(|v| {
            if 10 > *v {
                *v = 10;
            }
        });
        assert_eq!(*mgr.last_ack_seq.get(&session_id).unwrap(), 10);

        mgr.last_ack_seq.entry(session_id).and_modify(|v| {
            if 3 > *v {
                *v = 3;
            }
        });
        assert_eq!(*mgr.last_ack_seq.get(&session_id).unwrap(), 10);
    }

    #[test]
    fn send_to_disconnected_session_queues_and_replays() {
        let mgr = SessionManager::new();
        let (tx, _rx) = mpsc::unbounded_channel();

        mgr.register_session("s1".into(), tx);
        mgr.unregister_session(&"s1".into());

        mgr.send_to_session(&"s1".into(), make_output(1));
        mgr.send_to_session(&"s1".into(), make_output(2));

        let (tx2, mut rx2) = mpsc::unbounded_channel();
        mgr.register_session("s1".into(), tx2);

        let msg1 = rx2.try_recv().unwrap();
        let msg2 = rx2.try_recv().unwrap();
        assert!(matches!(msg1, ServerToProxy::SequencedInput { .. }));
        assert!(matches!(msg2, ServerToProxy::SequencedInput { .. }));
    }

    #[test]
    fn get_all_user_ids() {
        let mgr = SessionManager::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let (tx1, _rx1) = mpsc::unbounded_channel();
        let (tx2, _rx2) = mpsc::unbounded_channel();

        mgr.add_user_client(id1, tx1);
        mgr.add_user_client(id2, tx2);

        let ids = mgr.get_all_user_ids();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&id1));
        assert!(ids.contains(&id2));
    }
}
