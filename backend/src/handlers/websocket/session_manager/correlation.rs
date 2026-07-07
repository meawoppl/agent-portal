//! Request/response correlation for launcher RPCs: a handler registers a
//! oneshot for a request id, the launcher socket completes it when the
//! matching response frame arrives.

use shared::{FileDownloadResponseFields, ForwardStatusFields, LauncherToServer};
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

    /// Drop a pending directory-listing request without resolving (send
    /// failure or timeout).
    pub fn cancel_dir_request(&self, request_id: Uuid) {
        self.pending_dir_requests.remove(&request_id);
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

    /// Drop a pending agent-probe request without resolving (send failure or
    /// timeout).
    pub fn cancel_probe_request(&self, request_id: Uuid) {
        self.pending_probe_requests.remove(&request_id);
    }

    /// Register a pending file-download RPC; the returned receiver resolves when
    /// the owning proxy replies (or is cancelled on send-failure/timeout).
    pub fn register_file_download(
        &self,
        request_id: Uuid,
    ) -> oneshot::Receiver<FileDownloadResponseFields> {
        let (tx, rx) = oneshot::channel();
        self.pending_file_downloads.insert(request_id, tx);
        rx
    }

    /// Drop a pending file-download RPC without delivering (proxy not connected
    /// or the wait timed out).
    pub fn cancel_file_download(&self, request_id: Uuid) {
        self.pending_file_downloads.remove(&request_id);
    }

    /// Deliver a proxy's file-download response to the waiting handler. Returns
    /// `false` if no request was pending for `request_id` (already cancelled).
    pub fn complete_file_download(
        &self,
        request_id: Uuid,
        response: FileDownloadResponseFields,
    ) -> bool {
        if let Some((_, tx)) = self.pending_file_downloads.remove(&request_id) {
            let _ = tx.send(response);
            true
        } else {
            false
        }
    }

    /// Register a pending `ForwardOpen` → `ForwardStatus` round-trip for
    /// `(session, port)`; the receiver resolves when the proxy replies. A
    /// second registration for the same key replaces (and thereby cancels)
    /// the first.
    pub fn register_forward_status(
        &self,
        session_id: Uuid,
        port: u16,
    ) -> oneshot::Receiver<ForwardStatusFields> {
        let (tx, rx) = oneshot::channel();
        self.pending_forward_status.insert((session_id, port), tx);
        rx
    }

    /// Drop a pending forward-status wait without resolving (send failure or
    /// timeout).
    pub fn cancel_forward_status(&self, session_id: Uuid, port: u16) {
        self.pending_forward_status.remove(&(session_id, port));
    }

    /// Deliver a proxy's `ForwardStatus` to the waiting handler. Returns
    /// `false` if nothing was waiting (replayed `ForwardOpen`s after a
    /// reconnect get unsolicited replies — that's fine).
    pub fn complete_forward_status(&self, session_id: Uuid, status: ForwardStatusFields) -> bool {
        if let Some((_, tx)) = self
            .pending_forward_status
            .remove(&(session_id, status.port))
        {
            let _ = tx.send(status);
            true
        } else {
            false
        }
    }

    /// Record the session a `LaunchSession` request is expected to produce, so
    /// the launcher's result frame can be correlated back to it.
    pub fn register_launch_session(&self, request_id: Uuid, session_id: Uuid) {
        self.pending_launch_sessions.insert(request_id, session_id);
    }

    /// Drop a pending launch correlation without resolving (the launch frame
    /// failed to send).
    pub fn cancel_launch_session(&self, request_id: Uuid) {
        self.pending_launch_sessions.remove(&request_id);
    }

    /// Resolve a launch request to its expected session id, consuming the
    /// correlation entry. `None` if the request id was unknown/already taken.
    pub fn take_launch_session(&self, request_id: Uuid) -> Option<Uuid> {
        self.pending_launch_sessions
            .remove(&request_id)
            .map(|(_, id)| id)
    }
}
