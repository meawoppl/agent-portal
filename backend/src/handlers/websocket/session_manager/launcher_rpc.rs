//! The shared lifecycle for launcher request/reply exchanges. Response families
//! remain separate: a directory reply cannot consume an agent-login waiter.

use super::SessionManager;
use dashmap::DashMap;
use shared::{LauncherToServer, ServerToLauncher};
use std::time::Duration;
use tokio::sync::oneshot;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum LauncherRpcKind {
    Directory,
    Agent,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LauncherRpcError {
    Disconnected,
    Closed,
    Timeout,
}

pub(super) type LauncherRequests =
    DashMap<(LauncherRpcKind, Uuid), oneshot::Sender<LauncherToServer>>;

struct PendingRequest<'a> {
    requests: &'a LauncherRequests,
    key: (LauncherRpcKind, Uuid),
}

impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        self.requests.remove(&self.key);
    }
}

impl SessionManager {
    /// Register before sending, and release correlation state on every exit,
    /// including cancellation of the HTTP handler while its launcher is busy.
    /// HTTP status and error wording stay with the endpoint that owns them.
    pub(crate) async fn request_launcher(
        &self,
        launcher_id: Uuid,
        kind: LauncherRpcKind,
        request_id: Uuid,
        message: ServerToLauncher,
        timeout: Duration,
    ) -> Result<LauncherToServer, LauncherRpcError> {
        let key = (kind, request_id);
        let (tx, rx) = oneshot::channel();
        self.pending_launcher_requests.insert(key, tx);
        let _pending = PendingRequest {
            requests: &self.pending_launcher_requests,
            key,
        };
        if !self.send_to_launcher(&launcher_id, message) {
            return Err(LauncherRpcError::Disconnected);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err(LauncherRpcError::Closed),
            Err(_) => Err(LauncherRpcError::Timeout),
        }
    }

    pub(crate) fn complete_launcher_request(
        &self,
        kind: LauncherRpcKind,
        request_id: Uuid,
        reply: LauncherToServer,
    ) {
        if let Some((_, sender)) = self.pending_launcher_requests.remove(&(kind, request_id)) {
            let _ = sender.send(reply);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{conn_channel, LauncherConnection};
    use super::*;

    fn connected() -> (
        SessionManager,
        Uuid,
        tokio::sync::mpsc::Receiver<ServerToLauncher>,
    ) {
        let manager = SessionManager::new();
        let host = Uuid::new_v4();
        let (sender, receiver) = conn_channel(8);
        manager
            .try_register_launcher(
                host,
                LauncherConnection {
                    system: None,
                    sender,
                    launcher_name: "rpc-test".into(),
                    hostname: "rpc-host".into(),
                    user_id: Uuid::new_v4(),
                    running_sessions: vec![],
                    working_directory: None,
                    version: "test".into(),
                    capabilities: vec![],
                    cancel: tokio_util::sync::CancellationToken::new(),
                    gen: 0,
                    last_seen: std::sync::atomic::AtomicU64::new(0),
                },
            )
            .unwrap();
        (manager, host, receiver)
    }

    #[tokio::test]
    async fn reply_family_is_preserved_and_success_releases_waiter() {
        let (manager, host, mut receiver) = connected();
        let id = Uuid::new_v4();
        let task = tokio::spawn({
            let manager = manager.clone();
            async move {
                manager
                    .request_launcher(
                        host,
                        LauncherRpcKind::Agent,
                        id,
                        ServerToLauncher::ProbeAgents { request_id: id },
                        Duration::from_secs(5),
                    )
                    .await
            }
        });
        assert!(
            matches!(receiver.recv().await, Some(ServerToLauncher::ProbeAgents { request_id }) if request_id == id)
        );
        manager.complete_launcher_request(
            LauncherRpcKind::Directory,
            id,
            LauncherToServer::ListDirectoriesResult {
                request_id: id,
                entries: vec![],
                error: None,
                resolved_path: None,
            },
        );
        assert_eq!(manager.pending_launcher_requests.len(), 1);
        manager.complete_launcher_request(
            LauncherRpcKind::Agent,
            id,
            LauncherToServer::ProbeAgentsResult {
                request_id: id,
                agents: vec![],
            },
        );
        assert!(matches!(
            task.await.unwrap(),
            Ok(LauncherToServer::ProbeAgentsResult { .. })
        ));
        assert!(manager.pending_launcher_requests.is_empty());
    }

    #[tokio::test]
    async fn cancelled_caller_releases_waiter_and_late_reply_is_ignored() {
        let (manager, host, mut receiver) = connected();
        let id = Uuid::new_v4();
        let task = tokio::spawn({
            let manager = manager.clone();
            async move {
                manager
                    .request_launcher(
                        host,
                        LauncherRpcKind::Agent,
                        id,
                        ServerToLauncher::ProbeAgents { request_id: id },
                        Duration::from_secs(5),
                    )
                    .await
            }
        });
        receiver.recv().await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(manager.pending_launcher_requests.is_empty());
        manager.complete_launcher_request(
            LauncherRpcKind::Agent,
            id,
            LauncherToServer::ProbeAgentsResult {
                request_id: id,
                agents: vec![],
            },
        );
        assert!(manager.pending_launcher_requests.is_empty());
    }

    #[tokio::test]
    async fn disconnected_and_timed_out_requests_keep_distinct_errors() {
        let manager = SessionManager::new();
        let id = Uuid::new_v4();
        assert!(matches!(
            manager
                .request_launcher(
                    Uuid::new_v4(),
                    LauncherRpcKind::Agent,
                    id,
                    ServerToLauncher::ProbeAgents { request_id: id },
                    Duration::ZERO
                )
                .await,
            Err(LauncherRpcError::Disconnected)
        ));
        assert!(manager.pending_launcher_requests.is_empty());

        let (manager, host, _receiver) = connected();
        assert!(matches!(
            manager
                .request_launcher(
                    host,
                    LauncherRpcKind::Agent,
                    id,
                    ServerToLauncher::ProbeAgents { request_id: id },
                    Duration::ZERO
                )
                .await,
            Err(LauncherRpcError::Timeout)
        ));
        assert!(manager.pending_launcher_requests.is_empty());
    }
}
