//! User and admin endpoint request/response types.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Request to update a user's admin/ban settings (admin endpoint)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateUserRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_admin: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_double_option",
        serialize_with = "serialize_double_option"
    )]
    pub ban_reason: Option<Option<String>>,
}

fn deserialize_double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // If the field is present, deserialize its value (which may be null)
    Ok(Some(Option::deserialize(deserializer)?))
}

fn serialize_double_option<S>(
    value: &Option<Option<String>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        None => serializer.serialize_none(),
        Some(inner) => inner.serialize(serializer),
    }
}

/// Request to add a member to a session
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddMemberRequest {
    pub email: String,
    pub role: String,
}

/// Request to update a session member's role
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMemberRoleRequest {
    pub role: String,
}

/// Response from GET /api/auth/me — the currently authenticated user.
///
/// Wire shape matches the legacy `UserResponse` struct that lived in
/// `backend/src/handlers/auth.rs`. Lifted into `shared` so frontend call sites
/// can deserialize against the same type the backend serializes from. All
/// optional/derived fields default so older or partial responses still parse.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MeResponse {
    pub id: Uuid,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub is_admin: bool,
}

/// One row in the admin users table.
///
/// Wire shape matches the backend's `AdminUserInfo` in
/// `backend/src/handlers/admin.rs`. All non-identifying fields default so a
/// partial or older response still parses; `id` is required because every
/// frontend site keys off it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminUserEntry {
    pub id: Uuid,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub session_count: i64,
    #[serde(default)]
    pub total_spend_usd: f64,
    #[serde(default)]
    pub total_input_tokens: i64,
    #[serde(default)]
    pub total_output_tokens: i64,
    #[serde(default)]
    pub total_cache_creation_tokens: i64,
    #[serde(default)]
    pub total_cache_read_tokens: i64,
}

/// Response from GET /api/admin/users.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdminUsersResponse {
    #[serde(default)]
    pub users: Vec<AdminUserEntry>,
}

/// Response from GET /api/admin/stats — system overview statistics.
///
/// Wire shape matches the legacy `AdminStats` that lived in
/// `backend/src/handlers/admin.rs`. All fields default so older or partial
/// responses still parse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AdminStats {
    /// Total number of registered users
    #[serde(default)]
    pub total_users: i64,
    /// Number of users with is_admin=true
    #[serde(default)]
    pub admin_users: i64,
    /// Number of disabled users
    #[serde(default)]
    pub disabled_users: i64,
    /// Total number of sessions (all time)
    #[serde(default)]
    pub total_sessions: i64,
    /// Number of active sessions
    #[serde(default)]
    pub active_sessions: i64,
    /// Number of currently connected proxy clients
    #[serde(default)]
    pub connected_proxy_clients: usize,
    /// Number of currently connected web clients
    #[serde(default)]
    pub connected_web_clients: usize,
    /// Total API spend across all sessions
    #[serde(default)]
    pub total_spend_usd: f64,
    /// Total input tokens across all sessions
    #[serde(default)]
    pub total_input_tokens: i64,
    /// Total output tokens across all sessions
    #[serde(default)]
    pub total_output_tokens: i64,
    /// Total cache creation tokens across all sessions
    #[serde(default)]
    pub total_cache_creation_tokens: i64,
    /// Total cache read tokens across all sessions
    #[serde(default)]
    pub total_cache_read_tokens: i64,
}

/// One row in the admin sessions table.
///
/// Wire shape matches the legacy `AdminSessionInfo` that was duplicated
/// between `backend/src/handlers/admin.rs` and
/// `frontend/src/pages/admin/mod.rs`. `id` is required because the frontend
/// keys off it; everything else defaults so partial responses still parse.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdminSessionInfo {
    pub id: Uuid,
    #[serde(default)]
    pub user_id: Uuid,
    #[serde(default)]
    pub user_email: String,
    #[serde(default)]
    pub session_name: String,
    #[serde(default)]
    pub working_directory: String,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub last_activity: String,
    #[serde(default)]
    pub is_connected: bool,
    #[serde(default)]
    pub hostname: String,
}

/// Response from GET /api/admin/sessions.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AdminSessionsResponse {
    #[serde(default)]
    pub sessions: Vec<AdminSessionInfo>,
}
