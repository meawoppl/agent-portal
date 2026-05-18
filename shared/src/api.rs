//! Shared API request/response types for HTTP endpoints.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::SessionInfo;

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

/// Request to update a user's admin/ban/voice settings (admin endpoint)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UpdateUserRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_admin: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_enabled: Option<bool>,
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

/// Permission answer payload sent with PermissionResponse
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionAnswers {
    pub answers: serde_json::Map<String, serde_json::Value>,
}

impl PermissionAnswers {
    pub fn empty() -> Self {
        Self {
            answers: serde_json::Map::new(),
        }
    }
}

/// Response for GET /api/settings/sound
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundSettingsResponse {
    pub sound_config: Option<serde_json::Value>,
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
    #[serde(default)]
    pub voice_enabled: bool,
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
    pub voice_enabled: bool,
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
}
