//! Request/response correlation for launcher RPCs: a handler registers a
//! oneshot for a request id, the launcher socket completes it when the
//! matching response frame arrives.

use shared::{FileDownloadResponseFields, ForwardStatusFields, LauncherToServer};
use tokio::sync::oneshot;
use uuid::Uuid;

use super::{ForwardHealth, SessionManager};

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

    /// Record a probe verdict for `(session, port)`. Returns `true` when the
    /// stored health *changed* — the caller broadcasts `ForwardsChanged` so
    /// chips refetch and re-tint. The process name is part of the verdict so
    /// an app swap on the same port also nudges clients.
    pub fn update_forward_health(
        &self,
        session_id: Uuid,
        port: u16,
        listening: bool,
        process: Option<String>,
    ) -> bool {
        let verdict = ForwardHealth { listening, process };
        self.forward_health
            .insert((session_id, port), verdict.clone())
            != Some(verdict)
    }

    /// The last reported health for `(session, port)`, if the proxy has
    /// probed it since it (re)connected.
    pub fn forward_health(&self, session_id: Uuid, port: u16) -> Option<ForwardHealth> {
        self.forward_health
            .get(&(session_id, port))
            .map(|entry| entry.clone())
    }

    /// Drop a cached verdict — called when a forward is revoked or re-pointed
    /// so the abandoned port's entry doesn't linger.
    pub fn forget_forward_health(&self, session_id: Uuid, port: u16) {
        self.forward_health.remove(&(session_id, port));
    }

    /// Drop every cached verdict for `session_id` — called on proxy
    /// disconnect, when no tunnel path exists and any verdict is stale (the
    /// chip must fall back to neutral). Returns how many entries were removed
    /// so the caller can broadcast `ForwardsChanged` only when something
    /// actually changed.
    pub fn forget_forward_health_for_session(&self, session_id: Uuid) -> usize {
        let before = self.forward_health.len();
        self.forward_health.retain(|(sid, _), _| *sid != session_id);
        before - self.forward_health.len()
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

#[cfg(test)]
mod tests {
    use crate::handlers::websocket::{ForwardHealth, SessionManager};
    use uuid::Uuid;

    #[test]
    fn stale_old_port_status_cannot_clobber_current_port_health() {
        // Regression (codex review on #1257): health is keyed by
        // (session, port), so a late status for a just-replaced port must
        // not erase the live port's verdict.
        let mgr = SessionManager::default();
        let session = Uuid::new_v4();
        let py = || Some("python3".to_string());
        let health =
            |listening: bool, process: Option<String>| Some(ForwardHealth { listening, process });

        assert!(mgr.update_forward_health(session, 9000, true, py()));
        // Stale frame for the old port arrives late.
        assert!(mgr.update_forward_health(session, 8000, false, None));
        // The live port's verdict is untouched.
        assert_eq!(mgr.forward_health(session, 9000), health(true, py()));
        assert_eq!(mgr.forward_health(session, 8000), health(false, None));

        // Change-detection: same verdict again is not a change; a listening
        // flip or an app swap on the same port is.
        assert!(!mgr.update_forward_health(session, 9000, true, py()));
        assert!(mgr.update_forward_health(session, 9000, true, Some("vite".to_string())));
        assert!(mgr.update_forward_health(session, 9000, false, None));

        // Forgetting drops only the named pair.
        mgr.forget_forward_health(session, 8000);
        assert_eq!(mgr.forward_health(session, 8000), None);
        assert_eq!(mgr.forward_health(session, 9000), health(false, None));
    }

    #[test]
    fn proxy_disconnect_clears_only_that_sessions_health() {
        // Regression (codex re-review on #1257): a disconnected proxy has no
        // tunnel path, so its cached verdicts must clear (chip → neutral)
        // without touching other sessions'.
        let mgr = SessionManager::default();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());

        mgr.update_forward_health(a, 8000, true, None);
        mgr.update_forward_health(a, 9000, true, None);
        mgr.update_forward_health(b, 8000, true, None);

        assert_eq!(mgr.forget_forward_health_for_session(a), 2);
        assert_eq!(mgr.forward_health(a, 8000), None);
        assert_eq!(mgr.forward_health(a, 9000), None);
        assert_eq!(
            mgr.forward_health(b, 8000),
            Some(ForwardHealth {
                listening: true,
                process: None
            })
        );
        // Nothing left for `a` → nothing removed → caller skips the broadcast.
        assert_eq!(mgr.forget_forward_health_for_session(a), 0);
    }
}
