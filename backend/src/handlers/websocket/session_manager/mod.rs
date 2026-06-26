//! Shared connection state for every websocket endpoint, split by
//! responsibility:
//!
//! - [`proxy_lifecycle`] — proxy registration, connection generations,
//!   unregister/disconnect
//! - [`pending_queue`] — queue + replay of messages for disconnected proxies
//! - [`client_fanout`] — web/user client registries and broadcast fanout
//! - [`correlation`] — request/response correlation for launcher RPCs
//! - [`launcher_registry`] — launcher registration and `(user, host)` dedup
//!
//! The `SessionManager` struct itself (and its small, cross-cutting helpers)
//! lives here; each submodule contributes a focused `impl SessionManager`
//! block.

use dashmap::{DashMap, DashSet};
use shared::{FileDownloadResponseFields, LauncherToServer, ServerToClient, ServerToProxy};
use std::collections::VecDeque;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

mod client_fanout;
mod correlation;
mod input_queue;
mod launcher_registry;
mod pending_queue;
mod proxy_lifecycle;

pub use launcher_registry::LauncherConnection;
use pending_queue::PendingMessage;

pub type SessionId = String;
pub type ProxySender = mpsc::UnboundedSender<ServerToProxy>;
pub type WebClientSender = mpsc::UnboundedSender<ServerToClient>;

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
    /// Running lifetime total of sub-agent (Task tool) tokens per session.
    /// Claude's `result.usage` reports only the parent conversation; sub-agents
    /// run as separate API conversations whose tokens arrive on `task_notification`
    /// frames (`TaskUsage.total_tokens`, cumulative-per-task, emitted once at
    /// completion). We sum each completed task's total here and fold it into the
    /// session's `output_tokens` at result time so session totals (and the admin
    /// spend dashboard) don't under-count sub-agent usage.
    pub subagent_tokens: Arc<DashMap<Uuid, i64>>,
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
            subagent_tokens: Arc::new(DashMap::new()),
            gen_counter: Arc::new(AtomicU64::new(1)),
            connection_gen: Arc::new(DashMap::new()),
        }
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
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
}

/// Message constructors shared by the submodule test suites.
#[cfg(test)]
pub(super) mod test_support {
    use shared::{ServerToClient, ServerToProxy};
    use uuid::Uuid;

    pub fn make_heartbeat() -> ServerToProxy {
        ServerToProxy::Heartbeat
    }

    pub fn make_output(n: u32) -> ServerToProxy {
        ServerToProxy::SequencedInput {
            session_id: Uuid::nil(),
            seq: n as i64,
            content: serde_json::json!({"n": n}),
            send_mode: None,
            client_msg_id: None,
        }
    }

    pub fn make_client_msg() -> ServerToClient {
        ServerToClient::AgentOutput {
            content: serde_json::json!({"text": "hello"}),
            agent_type: shared::AgentType::Claude,
            meta: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
