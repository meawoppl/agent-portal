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
    pub launch_failure_count: i32,
    pub last_launch_attempt_at: Option<NaiveDateTime>,
    pub launch_lease_until: Option<NaiveDateTime>,
    /// All open PRs in the repo as a JSON array of `shared::PrRef`
    /// (`[{number,url,branch}]`). Surfaces on the wire via `SessionWithRole`'s
    /// flatten, where the frontend deserializes it into `Vec<PrRef>`.
    pub open_prs: serde_json::Value,
    /// When this session was last archived to long-term storage (#1258).
    /// `None` = never; the sweep re-archives when `last_activity` advances
    /// past this.
    pub archived_at: Option<NaiveDateTime>,
}

/// Insertable session that specifies the ID (so we can use Claude's session ID)
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
    pub provenance_kind: Option<String>,
    pub provenance_session_id: Option<Uuid>,
    pub provenance_agent_type: Option<String>,
}

impl Message {
    /// Who produced this message, mapped from the durable columns to the typed
    /// [`shared::MessageSource`] (portal-meta sidecar, see
    /// `docs/PORTAL_META_SIDECAR.md`). Inter-agent provenance wins (it is itself
    /// a `portal`-role row); otherwise the role decides. `sender_name` is the
    /// resolved display name for user-role messages. Returns `None` for the
    /// session's own agent output (assistant/system/result/error).
    pub fn message_source(&self, sender_name: Option<String>) -> Option<shared::MessageSource> {
        if let (Some("inter_agent"), Some(session_id), Some(agent_type)) = (
            self.provenance_kind.as_deref(),
            self.provenance_session_id,
            self.provenance_agent_type.as_deref(),
        ) {
            return Some(shared::MessageSource::Agent {
                session_id,
                agent_type: agent_type.to_string(),
            });
        }
        match self.role.as_str() {
            "user" => Some(shared::MessageSource::Human {
                account_id: self.user_id,
                name: sender_name.unwrap_or_default(),
            }),
            "portal" => Some(shared::MessageSource::Portal),
            _ => None,
        }
    }

    /// ISO-8601 microsecond timestamp matching the wire format the frontend's
    /// reconnect watermark + `replay_history` parser expect.
    pub fn created_at_iso(&self) -> String {
        self.created_at.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    }

    /// Build the typed [`shared::PortalMeta`] sidecar for this row. The backend
    /// only ever populates `created_at` + `source`; `delivery` is frontend-owned.
    pub fn portal_meta(&self, sender_name: Option<String>) -> shared::PortalMeta {
        shared::PortalMeta {
            created_at: Some(self.created_at_iso()),
            source: self.message_source(sender_name),
            delivery: None,
        }
    }
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::messages)]
pub struct NewMessage {
    pub session_id: Uuid,
    pub role: String,
    pub content: String,
    pub user_id: Uuid,
    pub agent_type: String,
    pub provenance_kind: Option<String>,
    pub provenance_session_id: Option<Uuid>,
    pub provenance_agent_type: Option<String>,
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
    /// `None` means the token has no row-level expiry. Session launch tokens
    /// track their session lifetime; live launcher credentials are rotated to
    /// expiring replacements after registration.
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
    /// Browser outbox delivery-tracking id (#1236). Persisted so replay
    /// keeps delivery tracking and resends can be deduplicated across a
    /// backend restart. `None` for non-browser inputs.
    pub client_msg_id: Option<Uuid>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::pending_inputs)]
pub struct NewPendingInput {
    pub session_id: Uuid,
    pub seq_num: i64,
    pub content: String,
    pub send_mode: Option<String>,
    pub client_msg_id: Option<Uuid>,
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

/// Partial update for a scheduled task. `None` fields are left unchanged
/// (Diesel skips them with the default `treat_none_as_null = false`); all
/// columns here are NOT NULL, so there is no set-to-null case to represent.
#[derive(Debug, AsChangeset)]
#[diesel(table_name = crate::schema::scheduled_tasks)]
pub struct ScheduledTaskChangeset {
    pub name: Option<String>,
    pub cron_expression: Option<String>,
    pub timezone: Option<String>,
    pub hostname: Option<String>,
    pub working_directory: Option<String>,
    pub prompt: Option<String>,
    pub claude_args: Option<serde_json::Value>,
    pub agent_type: Option<String>,
    pub enabled: Option<bool>,
    pub max_runtime_minutes: Option<i32>,
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
// Port Forward Models (docs/PORT_FORWARDING.md)
// ============================================================================

/// The session's single forwarded port (`agent-portal forward <port>`; at most
/// one row per session). The backend only tunnels this port; the row dies with
/// the session (`ON DELETE CASCADE`).
#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::session_forwards)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct SessionForward {
    pub id: Uuid,
    pub session_id: Uuid,
    pub port: i32,
    pub created_at: NaiveDateTime,
    /// When true the forward-origin serves without the token-handoff auth —
    /// anyone with the URL reaches the service (owner opt-in).
    pub public: bool,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::session_forwards)]
pub struct NewSessionForward {
    pub session_id: Uuid,
    pub port: i32,
}

/// A session's stable subdomain label (the LUT that maps a `Host`-header label
/// back to its session). Allocated on first forward, kept across close/reopen,
/// cascade-deleted with the session. See `forwards::ensure_subdomain_label`.
#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::forward_subdomains)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct ForwardSubdomain {
    pub label: String,
    pub session_id: Uuid,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::forward_subdomains)]
pub struct NewForwardSubdomain {
    pub label: String,
    pub session_id: Uuid,
}

/// An admin-assigned custom subdomain label that routes to a session's forward
/// alongside its auto label. One per session; cascade-deleted with the session.
#[derive(Debug, Queryable, Selectable, Serialize, Deserialize, Clone)]
#[diesel(table_name = crate::schema::custom_subdomains)]
#[diesel(check_for_backend(diesel::pg::Pg))]
pub struct CustomSubdomain {
    pub label: String,
    pub session_id: Uuid,
    pub created_by: Option<Uuid>,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = crate::schema::custom_subdomains)]
pub struct NewCustomSubdomain {
    pub label: String,
    pub session_id: Uuid,
    pub created_by: Option<Uuid>,
}

// ============================================================================
// Turn Metrics Models (per-turn performance metrics; PR 1 of N)
// ============================================================================

/// One row in `turn_metrics`. Persisted per user-input → terminator. See the
/// `2026-05-27-184255_add_turn_metrics` migration for column semantics. The
/// table is a durable per-user archive: it's outside the `MESSAGE_RETENTION_DAYS`
/// sweep, and `2026-06-04-120001_decouple_turn_metrics_from_sessions` made
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
    pub subagent_tokens: i64,
}

impl TurnMetric {
    /// Map a DB `TurnMetric` row into the wire-facing `shared::TurnMetrics`
    /// shape. Field-by-field rather than `From` impl so the two structs stay
    /// explicitly synchronized without one silently picking up a stray field
    /// from the other. Shared by the REST turn-metrics handlers and the
    /// WebSocket persist-and-broadcast path.
    pub fn into_wire(self) -> shared::TurnMetrics {
        shared::TurnMetrics {
            id: Some(self.id),
            // Nullable in the DB (orphaned-from-session rows); the wire shape
            // keeps a non-null `Uuid`, so fall back to nil for rows whose
            // session is gone. Freshly inserted rows always carry one.
            session_id: self.session_id.unwrap_or_default(),
            user_message_id: self.user_message_id,
            agent_type: self.agent_type,
            model: self.model,
            service_tier: self.service_tier,
            started_at: self.started_at,
            first_token_at: self.first_token_at,
            completed_at: self.completed_at,
            ttft_ms: self.ttft_ms,
            total_duration_ms: self.total_duration_ms,
            generation_duration_ms: self.generation_duration_ms,
            max_inter_token_gap_ms: self.max_inter_token_gap_ms,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_creation_tokens: self.cache_creation_tokens,
            cache_read_tokens: self.cache_read_tokens,
            thinking_tokens: self.thinking_tokens,
            subagent_tokens: self.subagent_tokens,
            stop_reason: self.stop_reason,
            is_error: self.is_error,
            tool_call_count: self.tool_call_count,
            stream_restarts: self.stream_restarts,
            total_cost_usd: self.total_cost_usd,
        }
    }
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
    pub subagent_tokens: i64,
    pub stop_reason: Option<String>,
    pub is_error: bool,
    pub tool_call_count: i32,
    pub stream_restarts: i32,
    pub total_cost_usd: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, provenance_kind: Option<&str>) -> Message {
        Message {
            id: Uuid::nil(),
            session_id: Uuid::nil(),
            role: role.to_string(),
            content: "{}".to_string(),
            created_at: NaiveDateTime::default(),
            user_id: Uuid::from_u128(7),
            agent_type: "claude".to_string(),
            provenance_kind: provenance_kind.map(str::to_string),
            provenance_session_id: provenance_kind.map(|_| Uuid::from_u128(9)),
            provenance_agent_type: provenance_kind.map(|_| "codex".to_string()),
        }
    }

    #[test]
    fn message_source_maps_columns_to_typed_source() {
        use shared::MessageSource;

        // Inter-agent provenance wins, even though the row is portal-role.
        assert_eq!(
            msg("portal", Some("inter_agent")).message_source(None),
            Some(MessageSource::Agent {
                session_id: Uuid::from_u128(9),
                agent_type: "codex".to_string(),
            })
        );
        // User row → Human, carrying the resolved display name.
        assert_eq!(
            msg("user", None).message_source(Some("Matt".to_string())),
            Some(MessageSource::Human {
                account_id: Uuid::from_u128(7),
                name: "Matt".to_string(),
            })
        );
        // Plain portal row (no provenance) → Portal.
        assert_eq!(
            msg("portal", None).message_source(None),
            Some(MessageSource::Portal)
        );
        // The session's own agent output carries no source.
        assert!(msg("assistant", None).message_source(None).is_none());
    }

    #[test]
    fn portal_meta_carries_created_at_and_source_only() {
        let meta = msg("user", None).portal_meta(Some("Matt".to_string()));
        assert!(meta.created_at.is_some());
        assert!(meta.source.is_some());
        // Delivery is frontend-owned — the backend never sets it.
        assert!(meta.delivery.is_none());
    }
}
