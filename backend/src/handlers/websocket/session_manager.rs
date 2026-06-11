use dashmap::{DashMap, DashSet};
use shared::{
    FileDownloadResponseFields, LauncherToServer, ServerToClient, ServerToLauncher, ServerToProxy,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};
use uuid::Uuid;

use shared::protocol::{MAX_PENDING_MESSAGES_PER_SESSION, MAX_PENDING_MESSAGE_AGE_SECS};

#[path = "launcher_registry.rs"]
mod launcher_registry;
pub use launcher_registry::LauncherConnection;

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

#[derive(Clone)]
pub struct SessionManager {
    pub sessions: Arc<DashMap<SessionId, ProxySender>>,
    pub web_clients: Arc<DashMap<SessionId, Vec<WebClientSender>>>,
    pub user_clients: Arc<DashMap<Uuid, Vec<WebClientSender>>>,
    pub last_ack_seq: Arc<DashMap<Uuid, u64>>,
    pending_messages: Arc<DashMap<SessionId, VecDeque<PendingMessage>>>,
    pub pending_truncations: Arc<DashSet<Uuid>>,
    pub launchers: Arc<DashMap<Uuid, LauncherConnection>>,
    /// Dedup index: `(user_id, hostname)` → `launcher_id`. Used to atomically
    /// reject a second launcher connection from the same host for the same
    /// user. Entries here are kept in lockstep with `launchers`: inserted by
    /// `try_register_launcher`, removed by `unregister_launcher`.
    launcher_dedup: Arc<DashMap<(Uuid, String), Uuid>>,
    pub pending_dir_requests: Arc<DashMap<Uuid, oneshot::Sender<LauncherToServer>>>,
    pub pending_probe_requests: Arc<DashMap<Uuid, oneshot::Sender<LauncherToServer>>>,
    pub pending_file_downloads: Arc<DashMap<Uuid, oneshot::Sender<FileDownloadResponseFields>>>,
    pub pending_launch_sessions: Arc<DashMap<Uuid, Uuid>>,
    /// Tracks who sent the last input for each session (session_id → (user_id, display_name))
    pub last_input_sender: Arc<DashMap<Uuid, (Uuid, String)>>,
    /// Monotonic counter for connection generations (prevents stale cleanup)
    gen_counter: Arc<AtomicU64>,
    /// Current connection generation per session
    connection_gen: Arc<DashMap<SessionId, u64>>,
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
            launcher_dedup: Arc::new(DashMap::new()),
            pending_dir_requests: Arc::new(DashMap::new()),
            pending_probe_requests: Arc::new(DashMap::new()),
            pending_file_downloads: Arc::new(DashMap::new()),
            pending_launch_sessions: Arc::new(DashMap::new()),
            last_input_sender: Arc::new(DashMap::new()),
            gen_counter: Arc::new(AtomicU64::new(1)),
            connection_gen: Arc::new(DashMap::new()),
        }
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

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
            fanout_to_clients(clients.value_mut(), msg);
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
            fanout_to_clients(entry.value_mut(), client_msg.clone());
        }
        for mut entry in self.user_clients.iter_mut() {
            fanout_to_clients(entry.value_mut(), client_msg.clone());
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

    pub fn register_probe_request(&self, request_id: Uuid) -> oneshot::Receiver<LauncherToServer> {
        let (tx, rx) = oneshot::channel();
        self.pending_probe_requests.insert(request_id, tx);
        rx
    }

    pub fn complete_probe_request(&self, request_id: Uuid, msg: LauncherToServer) {
        if let Some((_, tx)) = self.pending_probe_requests.remove(&request_id) {
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
            agent_type: shared::AgentType::Claude,
            created_at: None,
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
    fn broadcast_delivers_when_last_client_closed() {
        // The fanout moves the payload into the final send; make sure a
        // closed channel in that last slot still prunes correctly and the
        // open clients before it were already served.
        let mgr = SessionManager::new();
        let (tx1, mut rx1) = mpsc::unbounded_channel();
        let (tx2, mut rx2) = mpsc::unbounded_channel();
        let (tx3, rx3) = mpsc::unbounded_channel();

        mgr.add_web_client("s1".into(), tx1);
        mgr.add_web_client("s1".into(), tx2);
        mgr.add_web_client("s1".into(), tx3);

        drop(rx3);

        mgr.broadcast_to_web_clients(&"s1".into(), make_client_msg());

        assert!(matches!(
            rx1.try_recv().unwrap(),
            ServerToClient::ClaudeOutput { .. }
        ));
        assert!(matches!(
            rx2.try_recv().unwrap(),
            ServerToClient::ClaudeOutput { .. }
        ));
        let clients = mgr.web_clients.get("s1").unwrap();
        assert_eq!(clients.len(), 2);
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

    fn make_launcher_connection(user_id: Uuid, hostname: &str) -> LauncherConnection {
        let (sender, _rx) = mpsc::unbounded_channel();
        LauncherConnection {
            sender,
            launcher_name: format!("launcher-{}", hostname),
            hostname: hostname.to_string(),
            user_id,
            running_sessions: Vec::new(),
            working_directory: None,
            version: "test".to_string(),
        }
    }

    #[test]
    fn try_register_launcher_rejects_serial_duplicate() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        assert!(mgr
            .try_register_launcher(id_a, make_launcher_connection(user_id, "host1"))
            .is_ok());

        let err = mgr
            .try_register_launcher(id_b, make_launcher_connection(user_id, "host1"))
            .expect_err("second registration for same (user, host) must be rejected");
        assert_eq!(err, format!("launcher-host1"));

        assert_eq!(mgr.launchers.len(), 1);
        assert!(mgr.launchers.contains_key(&id_a));
        assert!(!mgr.launchers.contains_key(&id_b));
    }

    #[test]
    fn try_register_launcher_allows_different_user_same_host() {
        let mgr = SessionManager::new();
        let user_a = Uuid::new_v4();
        let user_b = Uuid::new_v4();

        assert!(mgr
            .try_register_launcher(Uuid::new_v4(), make_launcher_connection(user_a, "host1"))
            .is_ok());
        assert!(mgr
            .try_register_launcher(Uuid::new_v4(), make_launcher_connection(user_b, "host1"))
            .is_ok());
        assert_eq!(mgr.launchers.len(), 2);
    }

    #[test]
    fn unregister_launcher_releases_dedup_slot() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();

        assert!(mgr
            .try_register_launcher(id_a, make_launcher_connection(user_id, "host1"))
            .is_ok());
        mgr.unregister_launcher(&id_a);

        // After unregister the same (user, host) should be claimable again.
        assert!(mgr
            .try_register_launcher(id_b, make_launcher_connection(user_id, "host1"))
            .is_ok());
        assert_eq!(mgr.launchers.len(), 1);
        assert!(mgr.launchers.contains_key(&id_b));
    }

    /// Regression test for #790: concurrent registrations from the same
    /// `(user_id, hostname)` must result in exactly one launcher being
    /// registered. Previously `find_duplicate_launcher` + `register_launcher`
    /// were separate steps, so two simultaneous connections could both pass
    /// the duplicate check and both insert.
    #[tokio::test]
    async fn concurrent_launcher_registrations_dedupe_to_single_entry() {
        let mgr = SessionManager::new();
        let user_id = Uuid::new_v4();
        let hostname = "race-host";

        // Spawn 10 concurrent registration attempts for the same (user, host).
        let mut handles = Vec::with_capacity(10);
        for _ in 0..10 {
            let mgr_clone = mgr.clone();
            let host = hostname.to_string();
            handles.push(tokio::spawn(async move {
                let launcher_id = Uuid::new_v4();
                let conn = {
                    let (sender, _rx) = mpsc::unbounded_channel();
                    LauncherConnection {
                        sender,
                        launcher_name: format!("launcher-{}", launcher_id),
                        hostname: host,
                        user_id,
                        running_sessions: Vec::new(),
                        working_directory: None,
                        version: "test".to_string(),
                    }
                };
                mgr_clone
                    .try_register_launcher(launcher_id, conn)
                    .map(|_| launcher_id)
            }));
        }

        let mut successes = 0usize;
        let mut failures = 0usize;
        for h in handles {
            match h.await.expect("task panicked") {
                Ok(_) => successes += 1,
                Err(_) => failures += 1,
            }
        }

        // Exactly one registration must succeed; the other nine must be
        // rejected as duplicates. And the launchers map must hold exactly
        // one entry for this (user, host).
        assert_eq!(successes, 1, "exactly one registration should succeed");
        assert_eq!(failures, 9, "nine registrations should be rejected");
        assert_eq!(
            mgr.launchers.len(),
            1,
            "launchers map must contain exactly one entry after the race"
        );
        let user_count = mgr
            .launchers
            .iter()
            .filter(|e| e.value().user_id == user_id && e.value().hostname == hostname)
            .count();
        assert_eq!(user_count, 1);
    }
}
