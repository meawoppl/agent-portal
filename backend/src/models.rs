use chrono::{DateTime, NaiveDateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::users)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct User {
    pub id: Uuid,
    pub google_id: String,
    pub email: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub is_admin: bool,
    pub disabled: bool,
    pub ban_reason: Option<String>,
    pub sound_config: Option<serde_json::Value>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::users)]
pub struct NewUser {
    pub google_id: String,
    pub email: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::sessions)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Session {
    pub id: Uuid,
    pub user_id: Uuid,
    pub session_name: String,
    pub session_key: String,
    pub working_directory: String,
    pub status: String,
    pub last_activity: NaiveDateTime,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub git_branch: Option<String>,
    pub total_cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub client_version: Option<String>,
    pub input_seq: i64,
    pub hostname: String,
    pub launcher_id: Option<Uuid>,
    pub pr_url: Option<String>,
    pub agent_type: String,
    pub repo_url: Option<String>,
    pub scheduled_task_id: Option<Uuid>,
    pub paused: bool,
    pub claude_args: serde_json::Value,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::sessions)]
pub struct NewSession {
    pub user_id: Uuid,
    pub session_name: String,
    pub session_key: String,
    pub working_directory: String,
    pub status: String,
    pub git_branch: Option<String>,
}

/// NewSession variant that allows specifying the ID (for when we want to use Claude's session ID)
#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::sessions)]
pub struct NewSessionWithId {
    pub id: Uuid,
    pub user_id: Uuid,
    pub session_name: String,
    pub session_key: String,
    pub working_directory: String,
    pub status: String,
    pub git_branch: Option<String>,
    pub client_version: Option<String>,
    pub hostname: String,
    pub launcher_id: Option<Uuid>,
    pub agent_type: String,
    pub repo_url: Option<String>,
    pub scheduled_task_id: Option<Uuid>,
    pub paused: bool,
    pub claude_args: serde_json::Value,
}

#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::messages)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct Message {
    pub id: Uuid,
    pub session_id: Uuid,
    pub role: String,
    pub content: String,
    pub created_at: NaiveDateTime,
    pub user_id: Uuid,
    pub agent_type: String,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::messages)]
pub struct NewMessage {
    pub session_id: Uuid,
    pub role: String,
    pub content: String,
    pub user_id: Uuid,
    pub agent_type: String,
}

// ============================================================================
// Proxy Auth Token Models
// ============================================================================

#[derive(Debug, Queryable, Selectable, Serialize, Deserialize)]
#[diesel(table_name = crate::schema::proxy_auth_tokens)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ProxyAuthToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub token_hash: String,
    pub created_at: NaiveDateTime,
    pub last_used_at: Option<NaiveDateTime>,
    /// `None` means the token never expires (launch/launcher tokens). User
    /// dashboard tokens still carry an explicit expiry. See #932.
    pub expires_at: Option<NaiveDateTime>,
    pub revoked: bool,
    /// Session whose proxy holds this token, if it is a launch token. Used to
    /// revoke the token when that session terminates.
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::proxy_auth_tokens)]
pub struct NewProxyAuthToken {
    pub user_id: Uuid,
    pub name: String,
    pub token_hash: String,
    /// `None` mints a non-expiring token.
    pub expires_at: Option<NaiveDateTime>,
}

// ============================================================================
// Pending Permission Request Models
// ============================================================================

#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::pending_permission_requests)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct PendingPermissionRequest {
    pub id: Uuid,
    pub session_id: Uuid,
    pub request_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub permission_suggestions: Option<serde_json::Value>,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::pending_permission_requests)]
pub struct NewPendingPermissionRequest {
    pub session_id: Uuid,
    pub request_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub permission_suggestions: Option<serde_json::Value>,
}

// ============================================================================
// Deleted Session Costs Models
// ============================================================================

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::deleted_session_costs)]
pub struct NewDeletedSessionCosts {
    pub user_id: Uuid,
    pub cost_usd: f64,
    pub session_count: i32,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
}

// ============================================================================
// Session Member Models
// ============================================================================

#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::session_members)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SessionMember {
    pub id: Uuid,
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::session_members)]
pub struct NewSessionMember {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
}

// ============================================================================
// Pending Input Models (for reliable frontend->proxy message delivery)
// ============================================================================

#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::pending_inputs)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct PendingInput {
    pub id: Uuid,
    pub session_id: Uuid,
    pub seq_num: i64,
    pub content: String,
    pub created_at: NaiveDateTime,
    pub send_mode: Option<String>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::pending_inputs)]
pub struct NewPendingInput {
    pub session_id: Uuid,
    pub seq_num: i64,
    pub content: String,
    pub send_mode: Option<String>,
}

// ============================================================================
// Scheduled Task Models
// ============================================================================

#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::scheduled_tasks)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ScheduledTask {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub cron_expression: String,
    pub timezone: String,
    pub hostname: String,
    pub working_directory: String,
    pub prompt: String,
    pub claude_args: serde_json::Value,
    pub agent_type: String,
    pub enabled: bool,
    pub max_runtime_minutes: i32,
    pub last_session_id: Option<Uuid>,
    pub last_run_at: Option<NaiveDateTime>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::scheduled_tasks)]
pub struct NewScheduledTask {
    pub user_id: Uuid,
    pub name: String,
    pub cron_expression: String,
    pub timezone: String,
    pub hostname: String,
    pub working_directory: String,
    pub prompt: String,
    pub claude_args: serde_json::Value,
    pub agent_type: String,
    pub max_runtime_minutes: i32,
}

// ============================================================================
// Session Continuation Models
// ============================================================================

#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::session_continuations)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SessionContinuation {
    pub id: Uuid,
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub launcher_id: Uuid,
    pub reset_at: DateTime<Utc>,
    pub prompt: String,
    pub status: String,
    pub source_message: Option<String>,
    pub last_error: Option<String>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub scheduled_at: Option<NaiveDateTime>,
    pub fired_at: Option<NaiveDateTime>,
    pub dropped_at: Option<NaiveDateTime>,
    pub cancelled_at: Option<NaiveDateTime>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::session_continuations)]
pub struct NewSessionContinuation {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub launcher_id: Uuid,
    pub reset_at: DateTime<Utc>,
    pub prompt: String,
    pub status: String,
    pub source_message: Option<String>,
}

// ============================================================================
// Turn Metrics Models (per-turn performance metrics; PR 1 of N)
// ============================================================================

/// One row in `turn_metrics`. Persisted per user-input → terminator. See the
/// `2026-05-27-184255_add_turn_metrics` migration for column semantics. The
/// table is a durable per-user archive: it's outside the `MESSAGE_RETENTION_DAYS`
/// sweep, and `2026-06-04-120000_decouple_turn_metrics_from_sessions` made
/// `session_id` nullable with `ON DELETE SET NULL` (was `NOT NULL`/`CASCADE`) so
/// a row survives its session's deletion. Ownership now lives on `user_id`.
#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::turn_metrics)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct TurnMetric {
    pub id: Uuid,
    pub session_id: Option<Uuid>,
    pub user_message_id: Option<Uuid>,
    pub agent_type: String,
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub started_at: DateTime<Utc>,
    pub first_token_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub ttft_ms: Option<i64>,
    pub total_duration_ms: Option<i64>,
    pub generation_duration_ms: Option<i64>,
    pub max_inter_token_gap_ms: Option<i64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub thinking_tokens: i64,
    pub stop_reason: Option<String>,
    pub is_error: bool,
    pub tool_call_count: i32,
    pub stream_restarts: i32,
    pub total_cost_usd: Option<f64>,
    pub created_at: DateTime<Utc>,
    pub user_id: Uuid,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::turn_metrics)]
pub struct NewTurnMetric {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub user_message_id: Option<Uuid>,
    pub agent_type: String,
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub started_at: DateTime<Utc>,
    pub first_token_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub ttft_ms: Option<i64>,
    pub total_duration_ms: Option<i64>,
    pub generation_duration_ms: Option<i64>,
    pub max_inter_token_gap_ms: Option<i64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub thinking_tokens: i64,
    pub stop_reason: Option<String>,
    pub is_error: bool,
    pub tool_call_count: i32,
    pub stream_restarts: i32,
    pub total_cost_usd: Option<f64>,
}
