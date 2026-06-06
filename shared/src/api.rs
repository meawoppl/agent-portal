//! Shared API request/response types for HTTP endpoints.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::SessionInfo;

/// Typed citation entry attached to an Anthropic text content block.
///
/// The Anthropic wire shape carries one of several citation variants
/// (`char_location`, `page_location`, `content_block_location`,
/// `web_search_result_location`, …); only a small subset of fields is
/// shown in the agent-portal UI today (`url`, `title`, and `cited_text`),
/// so we model just those as a single flat struct with optional fields
/// rather than re-deriving the full upstream enum here. Unknown fields on
/// the wire are silently ignored — the UI only needs enough to render a
/// `[1]`/`[2]`/… link list.
///
/// Closes #754: replaces a previous `Vec<serde_json::Value>` field on
/// `ContentBlock::Text` that the frontend JSON-poked with
/// `cite.get("url")` / `cite.get("title")` / `cite.get("cited_text")`.
///
/// `claude-codes` 2.1.141 also stores citations as `Vec<serde_json::Value>`
/// on `TextBlock`; upstream issue
/// <https://github.com/meawoppl/rust-code-agent-sdks/issues/142> proposes
/// this typed shape so we can eventually drop the local definition and
/// just re-export from the SDK.
// TODO(SDK #142): replace with `claude_codes::Citation` once upstream
// adds a typed model — see agent-portal #754.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Citation {
    /// URL of the cited source (e.g. a web search result link).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Human-readable title of the cited source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Exact text quoted from the cited source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cited_text: Option<String>,
}

/// Typed envelope for a Codex permission request's `input` payload.
///
/// Replaces the prior `serde_json::Value` envelope shared between the proxy
/// (Codex app-server bridge in `codex-session-lib/src/handler.rs`) and the
/// frontend's permission dialog (`frontend/src/pages/dashboard/types.rs`).
/// Both sides used to JSON-poke field names ("itemId", "fileChanges",
/// "serverName", …); this enum makes the contract a compile-time one.
///
/// Closes #725 (proxy-side typed write) and #731 (frontend-side typed read).
///
/// # Wire shape
///
/// Serializes with a `tool` discriminant in camelCase:
///
/// ```json
/// {"tool": "fileChange", "itemId": "call_…", "reason": null, "grantRoot": null}
/// {"tool": "applyPatch", "fileChanges": {…}, "grantRoot": null, "reason": null}
/// {"tool": "bash", "command": "ls -la", "cwd": "/tmp"}
/// {"tool": "execCommand", "command": "ls -la", "cwd": "/tmp", "parsedCmd": [...]}
/// {"tool": "permissions", "cwd": "/tmp", "permissions": {…}, "reason": null}
/// {"tool": "mcpElicitation", "serverName": "…"}
/// {"tool": "askUserQuestion", "questions": [...]}
/// ```
///
/// The variant is the only authoritative source of the tool kind — the
/// human-readable string ("FileChange", "Bash", etc.) the frontend used to
/// dispatch on is derived from the variant via [`Self::tool_name`] so callers
/// that still want a stringly-typed CSS / sorting key can keep getting one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "camelCase")]
pub enum CodexPermissionInput {
    /// Codex `item/fileChange/requestApproval` — file-modification approval.
    /// The actual diff streamed earlier under the matching `itemId`.
    FileChange {
        #[serde(rename = "itemId")]
        item_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(rename = "grantRoot", default, skip_serializing_if = "Option::is_none")]
        grant_root: Option<String>,
    },
    /// Codex `applyPatchApproval` — apply-patch approval (0.130+).
    /// `file_changes` is a JSON object keyed by file path. Typed as `Value`
    /// here because the upstream `BTreeMap<String, FileChange>` shape is
    /// rich, evolves with the SDK, and the frontend only enumerates the
    /// keys for a one-line summary.
    ApplyPatch {
        #[serde(rename = "fileChanges", default)]
        file_changes: serde_json::Value,
        #[serde(rename = "grantRoot", default, skip_serializing_if = "Option::is_none")]
        grant_root: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Codex `item/commandExecution/requestApproval` — single-string command form.
    Bash {
        #[serde(default)]
        command: String,
        #[serde(default)]
        cwd: String,
        #[serde(rename = "parsedCmd", default, skip_serializing_if = "Option::is_none")]
        parsed_cmd: Option<serde_json::Value>,
    },
    /// Codex `execCommandApproval` (0.130+) — argv-vector command form.
    ExecCommand {
        #[serde(default)]
        command: String,
        #[serde(default)]
        cwd: String,
        #[serde(rename = "parsedCmd", default, skip_serializing_if = "Option::is_none")]
        parsed_cmd: Option<serde_json::Value>,
    },
    /// Codex `item/permissions/requestApproval` — broader permission profile.
    Permissions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permissions: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Codex `mcpServer/elicitation/request` — MCP server prompt.
    /// Wire shape today only carries `serverName`; the upstream typed enum
    /// is richer (Form / Url variants) but the proxy hasn't surfaced those
    /// fields yet — preserved as-is to avoid changing user-visible output.
    McpElicitation {
        #[serde(rename = "serverName", default)]
        server_name: String,
    },
    /// Codex `item/tool/requestUserInput` — reuses the Claude
    /// AskUserQuestion renderer; the Codex question shape is structurally
    /// compatible with Claude's so the frontend keeps a single dispatch.
    AskUserQuestion {
        #[serde(default)]
        questions: serde_json::Value,
    },
}

impl CodexPermissionInput {
    /// Human-readable tool name matching the historical `tool_name` strings
    /// the wire used to carry alongside this payload. Kept stable for the
    /// frontend's CSS / sort keys and for the existing `PendingPermission`
    /// debug logs.
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::FileChange { .. } => "FileChange",
            Self::ApplyPatch { .. } => "ApplyPatch",
            Self::Bash { .. } => "Bash",
            Self::ExecCommand { .. } => "ExecCommand",
            Self::Permissions { .. } => "Permissions",
            Self::McpElicitation { .. } => "McpElicitation",
            Self::AskUserQuestion { .. } => "AskUserQuestion",
        }
    }
}

/// API error types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApiError {
    /// Network or connection error
    Network(String),
    /// Server returned an error status
    Server { status: u16, message: String },
    /// Failed to parse response
    Parse(String),
    /// Authentication required or failed
    Auth(String),
    /// Resource not found
    NotFound(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Network(msg) => write!(f, "Network error: {}", msg),
            ApiError::Server { status, message } => {
                write!(f, "Server error ({}): {}", status, message)
            }
            ApiError::Parse(msg) => write!(f, "Parse error: {}", msg),
            ApiError::Auth(msg) => write!(f, "Auth error: {}", msg),
            ApiError::NotFound(msg) => write!(f, "Not found: {}", msg),
        }
    }
}

impl std::error::Error for ApiError {}

/// Device flow code request response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Request to launch a session via a launcher
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchRequest {
    pub working_directory: String,
    #[serde(default)]
    pub launcher_id: Option<uuid::Uuid>,
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub agent_type: crate::AgentType,
}

/// Request body for device code creation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DeviceCodeRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
}

/// Request body for polling device flow status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceFlowPollRequest {
    pub device_code: String,
}

/// Response for device flow approve/deny actions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceFlowActionResponse {
    pub success: bool,
    pub message: String,
}

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
// Typed shapes for `SystemMessage.extra` per-subtype dispatch
//
// Closes agent-portal #752. The local lenient `SystemMessage` type used by the
// frontend renderer carries a `#[serde(flatten)] extra: Option<serde_json::Value>`
// for ad-hoc per-subtype metadata. Renderers previously poked `extra.get("…")`
// by field name; these structs are the typed mirrors so renderers can
// `serde_json::from_value::<T>(extra.clone())` once per branch and read named
// fields. The wire shape is unchanged — these structs are deserialize-only
// views over the same JSON bytes.
//
// Where a subtype is already fully covered upstream by `claude-codes`, the
// renderer uses the SDK type directly (`TaskNotificationMessage`,
// `TaskStartedMessage`). The remaining structs below cover gaps not yet
// represented in `claude-codes`:
//
// - `CompactionExtra` — `summary`, `leaf_message_count`/`message_count`,
//   `duration_ms`, `content`, `text`. Filed upstream as
//   `rust-code-agent-sdks#141`; the SDK's `CompactBoundaryMessage` currently
//   only exposes `compact_metadata { pre_tokens, trigger }`. Once upstream
//   lands these, this struct can be deleted and the renderer can switch to
//   `SystemMessage::as_compact_boundary()`.
// - `InitExtra` — `fast_mode_state`. The SDK's `InitMessage::fast_mode_state`
//   is already typed `Option<String>`; we keep a narrow local mirror because
//   `InitMessage` has many required fields and a single-field shape is
//   friendlier to partial frames the renderer encounters in practice.
// =============================================================================

/// Typed view of the `compact_boundary` subtype's `SystemMessage.extra`.
///
/// All fields are optional with `#[serde(default)]` so any wire shape that
/// omits them still deserializes (yielding `None`). Read priority for the
/// summary text is `summary` → `content` → `text` to match the historical
/// renderer fallback chain.
//
// TODO(SDK rust-code-agent-sdks#141): drop this struct once
// `claude_codes::CompactBoundaryMessage` exposes these fields directly and
// switch `render_compaction_completed` to `SystemMessage::as_compact_boundary`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionExtra {
    #[serde(default)]
    pub summary: Option<String>,
    /// Primary "messages summarized" count. CLI variants spell this
    /// `leaf_message_count` (preferred) or `message_count` (legacy).
    #[serde(default)]
    pub leaf_message_count: Option<u32>,
    #[serde(default)]
    pub message_count: Option<u32>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Legacy aliases for the summary text — older CLI builds emitted under
    /// `content` or `text` instead of `summary`.
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

impl CompactionExtra {
    /// First-non-empty summary text, mirroring the historical renderer fallback
    /// chain `summary` → `content` → `text`.
    pub fn summary_text(&self) -> Option<&str> {
        self.summary
            .as_deref()
            .or(self.content.as_deref())
            .or(self.text.as_deref())
    }

    /// First-set message count, preferring `leaf_message_count` over the
    /// legacy `message_count` spelling.
    pub fn message_count(&self) -> Option<u32> {
        self.leaf_message_count.or(self.message_count)
    }
}

/// Typed view of the `init` subtype's `SystemMessage.extra` for fields the
/// renderer needs that aren't already top-level on the local lenient
/// `SystemMessage`. `fast_mode_state` matches `claude_codes::InitMessage::fast_mode_state`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InitExtra {
    #[serde(default)]
    pub fast_mode_state: Option<String>,
}

/// Typed view of the `task_notification` subtype's `SystemMessage.extra`.
///
/// Mirrors the renderable subset of `claude_codes::TaskNotificationMessage`
/// (which requires `session_id` + `summary` — both already consumed by the
/// outer `SystemMessage`'s typed top-level fields, so they would not appear
/// in `extra` if we deserialized the SDK type directly). All fields optional
/// so partial frames (e.g. `failed` notifications without `usage` or
/// `tool_use_id`) still parse.
///
/// The nested `status` and `usage` types are re-used from `claude-codes` so
/// the wire shape stays in lockstep with the SDK enum.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskNotificationExtra {
    #[serde(default)]
    pub status: Option<claude_codes::io::TaskStatus>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub usage: Option<claude_codes::io::TaskUsage>,
}

// =============================================================================
// Authenticated User Endpoint (GET /api/auth/me)
// =============================================================================

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

// =============================================================================
// Admin Users Endpoint (GET /api/admin/users)
// =============================================================================

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

// =============================================================================
// Scheduled Tasks API Types
// =============================================================================

/// Request to create a scheduled task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateScheduledTaskRequest {
    pub name: String,
    pub cron_expression: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    pub hostname: String,
    pub working_directory: String,
    pub prompt: String,
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub agent_type: crate::AgentType,
    #[serde(default = "default_max_runtime")]
    pub max_runtime_minutes: i32,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_max_runtime() -> i32 {
    30
}

/// Request to update a scheduled task (all fields optional)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UpdateScheduledTaskRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron_expression: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<crate::AgentType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runtime_minutes: Option<i32>,
}

/// Info about a scheduled task (returned by list/create endpoints)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledTaskInfo {
    pub id: uuid::Uuid,
    pub name: String,
    pub cron_expression: String,
    pub timezone: String,
    pub hostname: String,
    pub working_directory: String,
    pub prompt: String,
    pub claude_args: Vec<String>,
    pub agent_type: crate::AgentType,
    pub enabled: bool,
    pub max_runtime_minutes: i32,
    pub last_session_id: Option<uuid::Uuid>,
    pub last_run_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// Response listing scheduled tasks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTaskListResponse {
    pub tasks: Vec<ScheduledTaskInfo>,
}

// =============================================================================
// ResultMessage.modelUsage typed shape (closes #756)
// =============================================================================

/// Per-model usage / cost breakdown carried by Claude's `ResultMessage.modelUsage`
/// field. Keyed by model name (e.g. `"claude-opus-4-7[1m]"`) in the parent map.
///
/// Wire shape from claude-codes' own `test_result_with_new_fields`:
/// ```json
/// "modelUsage": {
///     "claude-opus-4-7[1m]": {
///         "inputTokens": 3817,
///         "outputTokens": 14,
///         "costUSD": 0.06
///     }
/// }
/// ```
///
/// TODO(SDK #140): `claude-codes::ResultMessage.model_usage` is currently
/// `Option<serde_json::Value>` upstream. This local typed mirror exists so the
/// frontend can iterate the per-model breakdown without poking JSON field
/// names. When the SDK adopts a typed `BTreeMap<String, ModelUsageEntry>`
/// itself, callers can `serde_json::from_value::<ModelUsage>(value)` directly
/// against the upstream type instead, and this struct can be deleted.
///
/// All fields default so partial / older frames still parse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageEntry {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    /// Wire name is `costUSD`, not `costUsd`.
    #[serde(default, rename = "costUSD")]
    pub cost_usd: f64,
    #[serde(default)]
    pub web_search_requests: u32,
}

/// Convenience alias for the full `modelUsage` map. The map key is the model
/// name string as emitted by claude (e.g. `"claude-opus-4-7[1m]"`).
pub type ModelUsage = BTreeMap<String, ModelUsageEntry>;

// =============================================================================
// Per-turn performance metrics (PR 1 of N — capture + persist pipeline)
// =============================================================================

/// Per-turn performance metrics captured by the proxy and persisted by the
/// backend. One row per user-input → terminator (`ClaudeOutput::Result` for
/// Claude, `CodexEvent::TurnCompleted` / `TurnFailed` for Codex).
///
/// Shared on the wire in two places:
///   - proxy → backend: `ProxyToServer::TurnMetricsReport(TurnMetrics)`
///   - backend → frontend: `ServerToClient::TurnMetrics(TurnMetrics)`
///
/// Frontend rendering ships in a follow-up PR; this type is the foundation
/// the capture pipeline writes to (and the broadcast pipeline reads from).
///
/// Field shapes mirror the `turn_metrics` DB columns:
///   - timestamps are `chrono::DateTime<Utc>` (`Option<_>` for the post-start
///     ones because an error before any content gives `None`)
///   - all token counters and derived ms durations are `i64`
///   - tool/restart counters are `i32`
///   - `total_cost_usd` is `Option<f64>` because Codex does not surface cost
///   - `id` and `user_message_id` are server-side (assigned at insert) and
///     therefore optional on the proxy-emit side; populated by the backend
///     before broadcast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnMetrics {
    /// DB row id. None on the proxy-emit side; populated by the backend
    /// after insert and present on the broadcast frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,

    pub session_id: Uuid,

    /// Optional foreign key into `messages` for the user prompt that opened
    /// this turn. The proxy doesn't know the backend's `messages.id`, so
    /// this stays `None` on the proxy-emit side until the backend wires up
    /// per-turn linkage in a future PR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message_id: Option<Uuid>,

    pub agent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_inter_token_gap_ms: Option<i64>,

    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_creation_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub thinking_tokens: i64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub tool_call_count: i32,
    #[serde(default)]
    pub stream_restarts: i32,

    /// Cost in USD. Claude provides this on `Result.total_cost_usd`; Codex
    /// does not surface cost on its wire today, so for codex turns this stays
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
}

// =============================================================================
// Turn-metrics list endpoint (`GET /api/sessions/{id}/turn-metrics`)
//
// Hydrates the SessionView's per-turn metrics buffer on initial load. The live
// path is the existing `ServerToClient::TurnMetrics` WS frame; this endpoint
// covers the cold-start gap (frontend reload, tab restore) and matches the
// access gate used by `GET /api/sessions/{id}/messages` (session_members ACL).
// =============================================================================

/// Response from `GET /api/sessions/{id}/turn-metrics`.
///
/// Rows are ordered `started_at ASC` so the SessionView's join walk (pair Nth
/// terminator message with Nth metrics row) works without a second sort.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnMetricsResponse {
    #[serde(default)]
    pub metrics: Vec<TurnMetrics>,
}

// =============================================================================
// Aggregated turn-metrics endpoint (`GET /api/metrics/turns`)
//
// Powers the Settings → Performance page (PR 4). Bucketed (`hour` | `day`)
// rollups grouped by `(agent_type, model, service_tier)` over a sliding window
// (`7d` / `30d` / `48h` / `2h`, …). Each row carries server-side computed
// counts, p50/p95 latency / throughput aggregates, token sums, cost sum, and
// the stop-reason histogram for the bucket.
// =============================================================================

/// One bucket in the `GET /api/metrics/turns` response. Aggregates `turn_metrics`
/// rows over the time slice keyed by `bucket_start`, grouped by
/// `(agent_type, model, service_tier)`. Percentiles and throughput are computed
/// server-side via Postgres `percentile_cont(...)` so the frontend gets ready-
/// to-plot scalars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricBucket {
    /// Bucket start timestamp (UTC). `date_trunc('hour' | 'day', started_at)`.
    pub bucket_start: DateTime<Utc>,
    pub agent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    // Counts
    pub turn_count: i64,
    pub error_count: i64,
    // Latency aggregates (millis)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_p50_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_p95_ms: Option<i64>,
    /// Throughput in output tokens per second (computed server-side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_p50_tps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_p95_tps: Option<f64>,
    // Tokens
    pub input_tokens_sum: i64,
    pub output_tokens_sum: i64,
    pub cache_read_tokens_sum: i64,
    pub cache_creation_tokens_sum: i64,
    // Cost
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd_sum: Option<f64>,
    /// Stop-reason mix for this bucket — keyed by the raw `stop_reason` string
    /// (`end_turn`, `max_tokens`, `tool_use`, …). Rows with `is_error = true`
    /// fold into the `"error"` key regardless of their `stop_reason` value so
    /// the stacked-area chart's red band reads as "errors" not as a particular
    /// reason. Rows with `stop_reason = NULL && is_error = false` fold into
    /// `"unknown"`.
    #[serde(default)]
    pub stop_reason_counts: BTreeMap<String, i64>,
}

/// Response shape for `GET /api/metrics/turns?bucket=…&window=…`.
///
/// Buckets are ordered `(bucket_start ASC, agent_type ASC, model ASC, tier ASC)`
/// so the frontend can stream-render a stacked area / multi-line chart without
/// a second sort. The frontend `(model, service_tier)` drop-down filters
/// client-side.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricBucketsResponse {
    #[serde(default)]
    pub buckets: Vec<MetricBucket>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_permission_input_file_change_roundtrip() {
        // Wire shape mirrors codex_codes 0.129.3 FileChangeRequestApprovalParams
        // — item_id present, reason and grantRoot optional.
        let input = CodexPermissionInput::FileChange {
            item_id: "call_HKaP84kozIUwWE1Ynd5hPpCN".to_string(),
            reason: None,
            grant_root: None,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "fileChange");
        assert_eq!(json["itemId"], "call_HKaP84kozIUwWE1Ynd5hPpCN");
        // Optional `None`s should not be serialized
        assert!(json.get("reason").is_none());
        assert!(json.get("grantRoot").is_none());

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "FileChange");
    }

    #[test]
    fn codex_permission_input_apply_patch_roundtrip() {
        let input = CodexPermissionInput::ApplyPatch {
            file_changes: serde_json::json!({
                "/tmp/a.rs": {"kind": "modify"},
                "/tmp/b.rs": {"kind": "add"},
            }),
            grant_root: Some("/tmp".to_string()),
            reason: None,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "applyPatch");
        assert_eq!(json["fileChanges"]["/tmp/a.rs"]["kind"], "modify");
        assert_eq!(json["grantRoot"], "/tmp");

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "ApplyPatch");
    }

    #[test]
    fn codex_permission_input_bash_roundtrip() {
        let input = CodexPermissionInput::Bash {
            command: "ls -la".to_string(),
            cwd: "/tmp".to_string(),
            parsed_cmd: None,
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "bash");
        assert_eq!(json["command"], "ls -la");
        assert_eq!(json["cwd"], "/tmp");
        assert!(json.get("parsedCmd").is_none());

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "Bash");
    }

    #[test]
    fn codex_permission_input_exec_command_roundtrip() {
        let input = CodexPermissionInput::ExecCommand {
            command: "ls -la".to_string(),
            cwd: "/tmp".to_string(),
            parsed_cmd: Some(serde_json::json!([{"cmd": "ls"}])),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "execCommand");
        assert_eq!(json["parsedCmd"][0]["cmd"], "ls");

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "ExecCommand");
    }

    #[test]
    fn codex_permission_input_permissions_roundtrip() {
        let input = CodexPermissionInput::Permissions {
            cwd: Some("/home/user/project".to_string()),
            permissions: Some(serde_json::json!({"read": ["/tmp"]})),
            reason: Some("requested by agent".to_string()),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "permissions");

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "Permissions");
    }

    #[test]
    fn codex_permission_input_mcp_elicitation_roundtrip() {
        let input = CodexPermissionInput::McpElicitation {
            server_name: "github".to_string(),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "mcpElicitation");
        assert_eq!(json["serverName"], "github");

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "McpElicitation");
    }

    #[test]
    fn codex_permission_input_ask_user_question_roundtrip() {
        let input = CodexPermissionInput::AskUserQuestion {
            questions: serde_json::json!([{"question": "ok?"}]),
        };
        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["tool"], "askUserQuestion");

        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, input);
        assert_eq!(parsed.tool_name(), "AskUserQuestion");
    }

    #[test]
    fn citation_full_roundtrip() {
        let cite = Citation {
            url: Some("https://example.com".to_string()),
            title: Some("Example".to_string()),
            cited_text: Some("hello world".to_string()),
        };
        let json = serde_json::to_value(&cite).unwrap();
        assert_eq!(json["url"], "https://example.com");
        assert_eq!(json["title"], "Example");
        assert_eq!(json["cited_text"], "hello world");
        let parsed: Citation = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, cite);
    }

    /// Wire frame as actually produced by the Anthropic API for a
    /// `web_search_result_location` citation (per claude-codes
    /// `test_text_block_with_citations`). Extra fields like `type` must
    /// be silently ignored.
    #[test]
    fn citation_ignores_unknown_fields() {
        let json = serde_json::json!({
            "type": "web_search_result_location",
            "url": "https://example.com",
            "title": "Example"
        });
        let parsed: Citation = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.url.as_deref(), Some("https://example.com"));
        assert_eq!(parsed.title.as_deref(), Some("Example"));
        assert!(parsed.cited_text.is_none());
    }

    /// Empty wire frame must parse to all-`None` so historical content
    /// blocks that omitted citations stay readable.
    #[test]
    fn citation_empty_roundtrip() {
        let json = serde_json::json!({});
        let parsed: Citation = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, Citation::default());
    }

    /// Wire shape lifted verbatim from `claude-codes`'
    /// `test_result_with_new_fields`: per-model entry with camelCase keys and
    /// `costUSD` (not `costUsd`). The frontend renderer iterates this map for
    /// the timing tooltip; the typed parse must accept the live wire shape.
    #[test]
    fn model_usage_entry_roundtrip() {
        let json = serde_json::json!({
            "inputTokens": 3817,
            "outputTokens": 14,
            "costUSD": 0.06,
        });
        let parsed: ModelUsageEntry = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.input_tokens, 3817);
        assert_eq!(parsed.output_tokens, 14);
        assert!((parsed.cost_usd - 0.06).abs() < 1e-9);
        // Unset fields default to 0
        assert_eq!(parsed.cache_read_input_tokens, 0);
        assert_eq!(parsed.cache_creation_input_tokens, 0);
        assert_eq!(parsed.web_search_requests, 0);
    }

    /// The full `modelUsage` map: a `BTreeMap<String, ModelUsageEntry>` keyed
    /// by model name. Multiple models accumulate when a session uses haiku +
    /// opus or similar.
    #[test]
    fn model_usage_map_roundtrip() {
        let json = serde_json::json!({
            "claude-opus-4-7[1m]": {
                "inputTokens": 3817,
                "outputTokens": 14,
                "costUSD": 0.06,
            },
            "claude-haiku-4-5": {
                "inputTokens": 100,
                "outputTokens": 5,
                "costUSD": 0.001,
            },
        });
        let parsed: ModelUsage = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.len(), 2);
        let opus = parsed.get("claude-opus-4-7[1m]").unwrap();
        assert_eq!(opus.input_tokens, 3817);
        assert!((opus.cost_usd - 0.06).abs() < 1e-9);
        let haiku = parsed.get("claude-haiku-4-5").unwrap();
        assert_eq!(haiku.output_tokens, 5);
    }

    /// Regression guard for the pattern that motivated #725/#731: optional
    /// fields must default cleanly so a Codex frame that omits them parses
    /// rather than silently surfacing as Unknown.
    #[test]
    fn codex_permission_input_file_change_omits_optional_fields() {
        // Frame as observed live in PR #721's bug report — no `reason` /
        // `grantRoot`; should parse as-is.
        let json = serde_json::json!({
            "tool": "fileChange",
            "itemId": "call_HKaP84kozIUwWE1Ynd5hPpCN",
        });
        let parsed: CodexPermissionInput = serde_json::from_value(json).unwrap();
        match parsed {
            CodexPermissionInput::FileChange {
                item_id,
                reason,
                grant_root,
            } => {
                assert_eq!(item_id, "call_HKaP84kozIUwWE1Ynd5hPpCN");
                assert!(reason.is_none());
                assert!(grant_root.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// `TurnMetrics` must round-trip with all the optional fields populated
    /// and with cost present (Claude shape).
    #[test]
    fn turn_metrics_full_roundtrip() {
        let started = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let metrics = TurnMetrics {
            id: None,
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: "claude".to_string(),
            model: Some("claude-opus-4-7".to_string()),
            service_tier: Some("standard".to_string()),
            started_at: started,
            first_token_at: Some(started + chrono::Duration::milliseconds(420)),
            completed_at: Some(started + chrono::Duration::milliseconds(8000)),
            ttft_ms: Some(420),
            total_duration_ms: Some(8000),
            generation_duration_ms: Some(7580),
            max_inter_token_gap_ms: Some(150),
            input_tokens: 1234,
            output_tokens: 567,
            cache_creation_tokens: 0,
            cache_read_tokens: 90,
            thinking_tokens: 12,
            stop_reason: Some("end_turn".to_string()),
            is_error: false,
            tool_call_count: 3,
            stream_restarts: 0,
            total_cost_usd: Some(0.0145),
        };
        let json = serde_json::to_value(&metrics).unwrap();
        assert_eq!(json["agent_type"], "claude");
        assert_eq!(json["ttft_ms"], 420);
        assert_eq!(json["total_cost_usd"], 0.0145);
        let parsed: TurnMetrics = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, metrics);
    }

    /// `MetricBucket` round-trip. Claude row with all fields populated; the
    /// stop-reason mix has a couple of representative entries.
    #[test]
    fn metric_bucket_full_roundtrip() {
        let bucket_start = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut counts = BTreeMap::new();
        counts.insert("end_turn".to_string(), 5);
        counts.insert("max_tokens".to_string(), 1);
        let bucket = MetricBucket {
            bucket_start,
            agent_type: "claude".to_string(),
            model: Some("claude-opus-4-7".to_string()),
            service_tier: Some("standard".to_string()),
            turn_count: 6,
            error_count: 0,
            ttft_p50_ms: Some(420),
            ttft_p95_ms: Some(1200),
            throughput_p50_tps: Some(47.5),
            throughput_p95_tps: Some(65.0),
            input_tokens_sum: 12_000,
            output_tokens_sum: 3_400,
            cache_read_tokens_sum: 800,
            cache_creation_tokens_sum: 200,
            total_cost_usd_sum: Some(0.18),
            stop_reason_counts: counts,
        };
        let json = serde_json::to_value(&bucket).unwrap();
        assert_eq!(json["agent_type"], "claude");
        assert_eq!(json["turn_count"], 6);
        assert_eq!(json["stop_reason_counts"]["end_turn"], 5);
        let parsed: MetricBucket = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, bucket);
    }

    /// `MetricBucketsResponse` round-trip via the top-level envelope.
    #[test]
    fn metric_buckets_response_roundtrip() {
        let bucket = MetricBucket {
            bucket_start: chrono::Utc::now(),
            agent_type: "codex".to_string(),
            model: None,
            service_tier: None,
            turn_count: 3,
            error_count: 1,
            ttft_p50_ms: None,
            ttft_p95_ms: None,
            throughput_p50_tps: None,
            throughput_p95_tps: None,
            input_tokens_sum: 0,
            output_tokens_sum: 0,
            cache_read_tokens_sum: 0,
            cache_creation_tokens_sum: 0,
            total_cost_usd_sum: None,
            stop_reason_counts: BTreeMap::new(),
        };
        let resp = MetricBucketsResponse {
            buckets: vec![bucket.clone()],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json["buckets"].is_array());
        let parsed: MetricBucketsResponse = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.buckets, vec![bucket]);
    }

    /// Codex-shape: cost is `None` and many of the optional fields are also
    /// unset. Must serialize without nulls for the skip-if-none fields and
    /// must round-trip back to the same value.
    #[test]
    fn turn_metrics_codex_no_cost_roundtrip() {
        let started = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let metrics = TurnMetrics {
            id: None,
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: "codex".to_string(),
            model: None,
            service_tier: None,
            started_at: started,
            first_token_at: None,
            completed_at: Some(started + chrono::Duration::milliseconds(120)),
            ttft_ms: None,
            total_duration_ms: Some(120),
            generation_duration_ms: None,
            max_inter_token_gap_ms: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            thinking_tokens: 0,
            stop_reason: Some("failed".to_string()),
            is_error: true,
            tool_call_count: 0,
            stream_restarts: 0,
            total_cost_usd: None,
        };
        let json = serde_json::to_value(&metrics).unwrap();
        assert_eq!(json["agent_type"], "codex");
        assert!(json.get("total_cost_usd").is_none());
        assert!(json.get("ttft_ms").is_none());
        let parsed: TurnMetrics = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, metrics);
    }
}
