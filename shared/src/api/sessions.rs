//! Session listing, resolution, membership, message and agent-messaging types.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{SessionInfo, SessionRole};

/// Response from GET /api/sessions — sessions visible to the current user.
///
/// Wire shape matches the legacy `SessionListResponse` in
/// `backend/src/handlers/sessions.rs`. Each entry is a `SessionInfo`
/// (the existing shared type — wire-compatible with the backend's
/// `SessionWithRole` flatten projection of `Session` + `my_role`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionsResponse {
    #[serde(default)]
    pub sessions: Vec<SessionInfo>,
}

/// Request from a proxy before it spawns the agent process. The backend uses
/// this to choose the latest resumable session for the authenticated user and
/// local machine/path, so clients don't need to persist directory -> session_id
/// as the source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveProxySessionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    pub working_directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default)]
    pub agent_type: crate::AgentType,
}

/// Response from `POST /api/proxy/resolve-session`. `session_id == None`
/// means the backend has no matching resumable session and the proxy should
/// allocate a fresh UUID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolveProxySessionResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
}

/// One member of a shared session.
///
/// Wire shape matches the legacy `SessionMemberInfo` in
/// `backend/src/handlers/sessions.rs` — `created_at` serializes as the naive
/// ISO timestamp chrono emits for `NaiveDateTime` (the frontend's old
/// `MemberInfo` copy silently dropped this field).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionMemberInfo {
    pub user_id: Uuid,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub role: SessionRole,
    pub created_at: NaiveDateTime,
}

/// Response from GET /api/sessions/:id/members.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMembersResponse {
    #[serde(default)]
    pub members: Vec<SessionMemberInfo>,
}

/// Response envelope for listing messages.
///
/// Generic over the message element type because the two sides project the
/// row differently: the backend serializes a flattened Diesel `Message` model
/// (+ optional `sender_name`), while the frontend deserializes its lenient
/// `MessageData` view. The envelope shape (`messages` + `total`) is the
/// shared wire contract.
///
/// `total` is the page length (post-#788 it reflects what was actually
/// returned, not the session-wide row count). The wire field stays `total`
/// for backward compatibility with older clients that might key off it (the
/// current frontend ignores this field).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessagesListResponse<T> {
    pub messages: Vec<T>,
    #[serde(default)]
    pub total: i64,
}

// ---- Inter-agent messaging --------------------------------------------------

/// One of the caller's sessions, as listed for the agent-messaging page/API
/// and mobile status surfaces (`GET /api/agent/sessions`). A lightweight
/// summary — enough to pick a recipient or render a widget row — not the
/// full `SessionInfo`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionInfo {
    pub id: Uuid,
    pub session_name: String,
    pub working_directory: String,
    pub agent_type: String,
    pub status: String,
    pub hostname: String,
    /// True when the session has a pending permission request waiting on the
    /// user — the "agent is blocked on you" signal mobile widgets surface.
    #[serde(default)]
    pub awaiting_permission: bool,
    /// RFC3339 UTC timestamp of the session's last activity. Empty when the
    /// backend predates this field (`serde(default)`).
    #[serde(default)]
    pub last_activity: String,
}

/// Response for `GET /api/agent/sessions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionsResponse {
    #[serde(default)]
    pub sessions: Vec<AgentSessionInfo>,
}

/// Body for `POST /api/agent/sessions/{id}/message`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendAgentMessageRequest {
    pub message: String,
    /// Sender's session id, when sent by an agent (the `agent-portal message
    /// send` CLI fills this from `$PORTAL_SESSION_ID`). Drives the recipient's
    /// attribution bumper. Absent for the human web page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// Response for `POST /api/agent/sessions/{id}/message`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendAgentMessageResponse {
    /// True if a live proxy received it; false means it was queued for the
    /// session's next reconnect.
    pub delivered: bool,
    /// True if the backend persisted a pending-input row for replay on proxy
    /// reconnect. If false and `delivered` is also false, the sender should
    /// treat the message as not replay-safe.
    #[serde(default)]
    pub persisted: bool,
    /// The input sequence number assigned to the injected message.
    pub seq: i64,
    /// Number of pending input rows for this session after this send.
    #[serde(default)]
    pub pending_inputs: usize,
}

#[cfg(test)]
mod agent_session_wire_tests {
    use super::*;

    /// Widget/CLI consumers must be able to parse responses from backends
    /// that predate the status-surface fields: absent `awaiting_permission`
    /// and `last_activity` fall back to defaults rather than failing.
    #[test]
    fn agent_session_info_defaults_missing_status_fields() {
        let old_wire = r#"{
            "id": "00000000-0000-0000-0000-000000000001",
            "session_name": "s",
            "working_directory": "/w",
            "agent_type": "claude",
            "status": "active",
            "hostname": "h"
        }"#;
        let parsed: AgentSessionInfo = serde_json::from_str(old_wire).unwrap();
        assert!(!parsed.awaiting_permission);
        assert!(parsed.last_activity.is_empty());
    }
}
