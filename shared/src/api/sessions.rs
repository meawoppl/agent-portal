use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::SessionInfo;

/// An error message for display in the terminal output stream
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorMessage {
    #[serde(rename = "type")]
    pub error_type: String,
    pub message: String,
}

impl ErrorMessage {
    pub fn new(message: String) -> Self {
        Self {
            error_type: "error".to_string(),
            message,
        }
    }
}

/// Response for GET /api/settings/sound
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundSettingsResponse {
    pub sound_config: Option<serde_json::Value>,
}

// =============================================================================
// Sessions List Endpoint (GET /api/sessions)
// =============================================================================

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

// =============================================================================
// Proxy Session Resolution Endpoint (POST /api/proxy/resolve-session)
// =============================================================================

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
