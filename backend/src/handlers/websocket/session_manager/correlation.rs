//! Request/response correlation for launcher RPCs: a handler registers a
//! oneshot for a request id, the launcher socket completes it when the
//! matching response frame arrives.

use shared::LauncherToServer;
use tokio::sync::oneshot;
use uuid::Uuid;

use super::SessionManager;

impl SessionManager {
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
