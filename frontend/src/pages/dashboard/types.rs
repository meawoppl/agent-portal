//! Shared types for the dashboard module

use crate::utils::{storage_get, storage_set};
use serde::Deserialize;
use shared::ToolInput;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Answers for multiple AskUserQuestion questions
/// Key is question index, value is the selected answer(s)
pub type QuestionAnswers = HashMap<usize, String>;

/// Storage key for hidden sessions in localStorage
pub const HIDDEN_SESSIONS_STORAGE_KEY: &str = "claude-portal-hidden-sessions";

/// Storage key for inactive hidden state in localStorage
pub const INACTIVE_HIDDEN_STORAGE_KEY: &str = "claude-portal-inactive-hidden";

/// Storage key for cost display visibility in localStorage
pub const SHOW_COST_STORAGE_KEY: &str = "claude-portal-show-cost";

/// Storage key for session rail orientation in localStorage
pub const RAIL_ORIENTATION_STORAGE_KEY: &str = "claude-portal-rail-orientation";

/// Maximum number of messages to keep in frontend memory (matches backend limit)
pub const MAX_MESSAGES_PER_SESSION: usize = 100;

/// Type alias for WebSocket sender.
///
/// This is the **producer half** of an unbounded mpsc queue that feeds a
/// single owner task holding the real `ws_bridge` sink. Multiple callers
/// can push concurrently without coordination — see
/// `pages::dashboard::session_view::websocket::send_message`. The previous
/// `Rc<RefCell<Option<Sender>>>` shape was racy under concurrent producers
/// (closes #783).
pub type WsSender = futures_channel::mpsc::UnboundedSender<shared::ClientToServer>;

/// Message data from the API
#[derive(Clone, PartialEq, Deserialize)]
pub struct MessageData {
    pub role: String,
    pub content: String,
    /// ISO 8601 timestamp when message was created
    pub created_at: String,
    /// Typed portal sidecar for attribution (incl. the human sender name via
    /// `meta.source`), timestamp, and delivery state. The former top-level
    /// `user_id`/`sender_name` were redundant with this and have been removed.
    #[serde(default)]
    pub meta: Option<shared::PortalMeta>,
}

/// Response from the messages API endpoint — the shared envelope
/// (`messages` + `total`, the latter unused here) over this module's
/// lenient `MessageData` projection.
pub type MessagesResponse = shared::api::MessagesListResponse<MessageData>;

/// Pending permission request
#[derive(Clone, Debug, PartialEq)]
pub struct PendingPermission {
    pub request_id: String,
    pub tool_name: String,
    pub input: serde_json::Value,
    pub permission_suggestions: Vec<shared::PermissionSuggestion>,
}

/// Parsed AskUserQuestion option
#[derive(Clone, Debug, Deserialize)]
pub struct AskUserOption {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// Parsed AskUserQuestion question
#[derive(Clone, Debug, Deserialize)]
pub struct AskUserQuestion {
    pub question: String,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub options: Vec<AskUserOption>,
    #[serde(default, rename = "multiSelect")]
    pub multi_select: bool,
}

/// Parsed AskUserQuestion input
#[derive(Clone, Debug, Deserialize)]
pub struct AskUserQuestionInput {
    pub questions: Vec<AskUserQuestion>,
}

/// Try to parse AskUserQuestion input from permission input
pub fn parse_ask_user_question(input: &serde_json::Value) -> Option<AskUserQuestionInput> {
    serde_json::from_value(input.clone()).ok()
}

/// Load a boolean preference from localStorage (default: false)
fn load_bool_pref(key: &str) -> bool {
    storage_get(key).map(|v| v == "true").unwrap_or(false)
}

/// Save a boolean preference to localStorage
fn save_bool_pref(key: &str, value: bool) {
    storage_set(key, if value { "true" } else { "false" });
}

/// Load whether inactive sessions section is hidden from localStorage
pub fn load_inactive_hidden() -> bool {
    load_bool_pref(INACTIVE_HIDDEN_STORAGE_KEY)
}

/// Save inactive hidden state to localStorage
pub fn save_inactive_hidden(hidden: bool) {
    save_bool_pref(INACTIVE_HIDDEN_STORAGE_KEY, hidden);
}

/// Load cost display preference from localStorage (default: hidden)
pub fn load_show_cost() -> bool {
    load_bool_pref(SHOW_COST_STORAGE_KEY)
}

/// Save cost display preference to localStorage
pub fn save_show_cost(show: bool) {
    save_bool_pref(SHOW_COST_STORAGE_KEY, show);
}

/// Where the session pill rail sits on the dashboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailPosition {
    Top,
    Bottom,
    Left,
    Right,
}

impl RailPosition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Right => "right",
        }
    }

    /// Parse a stored preference value. Accepts both the new four-value form
    /// (`top`/`bottom`/`left`/`right`) and the legacy two-value form
    /// (`horizontal` → top, `vertical` → left) so older browser local storage
    /// entries keep working without forcing the user to re-pick.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "top" | "horizontal" => Some(Self::Top),
            "bottom" => Some(Self::Bottom),
            "left" | "vertical" => Some(Self::Left),
            "right" => Some(Self::Right),
            _ => None,
        }
    }

    /// CSS class applied to the dashboard body wrapper.
    pub fn body_class(self) -> &'static str {
        match self {
            Self::Top => "rail-top",
            Self::Bottom => "rail-bottom",
            Self::Left => "rail-left",
            Self::Right => "rail-right",
        }
    }
}

/// Load rail position preference from localStorage (default: top).
pub fn load_rail_position() -> RailPosition {
    storage_get(RAIL_ORIENTATION_STORAGE_KEY)
        .and_then(|v| RailPosition::from_str(&v))
        .unwrap_or(RailPosition::Top)
}

/// Save rail position preference to localStorage.
pub fn save_rail_position(position: RailPosition) {
    storage_set(RAIL_ORIENTATION_STORAGE_KEY, position.as_str());
}

/// Load hidden session IDs from localStorage
pub fn load_hidden_sessions() -> HashSet<Uuid> {
    storage_get(HIDDEN_SESSIONS_STORAGE_KEY)
        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
        .map(|ids| ids.iter().filter_map(|s| Uuid::parse_str(s).ok()).collect())
        .unwrap_or_default()
}

/// Save hidden session IDs to localStorage
pub fn save_hidden_sessions(hidden: &HashSet<Uuid>) {
    let ids: Vec<String> = hidden.iter().map(|id| id.to_string()).collect();
    if let Ok(json) = serde_json::to_string(&ids) {
        storage_set(HIDDEN_SESSIONS_STORAGE_KEY, &json);
    }
}

/// Calculate exponential backoff delay for reconnection attempts
pub fn calculate_backoff(attempt: u32) -> u32 {
    const INITIAL_MS: u32 = 1000;
    const MAX_MS: u32 = 30000;
    INITIAL_MS
        .saturating_mul(2u32.saturating_pow(attempt.min(5)))
        .min(MAX_MS)
}

/// Format permission input for display.
///
/// `input` is a `serde_json::Value` because the cross-agent
/// `ServerToClient::PermissionRequest` wire envelope carries both
/// claude-side tool inputs (whose typed shape lives in `claude-codes`)
/// and codex-side typed `CodexPermissionInput` envelopes. Both sides
/// parse into typed SDK/shared shapes so field-name changes break at
/// compile time instead of silently falling through display code.
pub fn format_permission_input(tool_name: &str, input: &serde_json::Value) -> String {
    // Codex-side: try to round-trip the typed envelope first. Closes #731.
    if let Ok(codex_input) = serde_json::from_value::<shared::CodexPermissionInput>(input.clone()) {
        return format_codex_permission_input(&codex_input);
    }

    format_claude_permission_input(tool_name, input)
}

/// Render a Claude-side permission tool input from the SDK's named,
/// typed `ToolInput` parser.
fn format_claude_permission_input(tool_name: &str, input: &serde_json::Value) -> String {
    match ToolInput::from_named_input(tool_name, input.clone()) {
        ToolInput::Bash(bash) => format!("$ {}", bash.command),
        ToolInput::Read(read) => read.file_path,
        ToolInput::Edit(edit) => edit.file_path,
        ToolInput::Write(write) => write.file_path,
        _ => serde_json::to_string_pretty(input).unwrap_or_else(|_| format!("{:?}", input)),
    }
}

/// Render a typed `CodexPermissionInput` into a one-line (or short
/// multi-line) summary for the permission card. Mirrors the previous
/// per-`tool_name` arms in [`format_permission_input`], but dispatches
/// on the typed variant so field-name changes break at compile time.
fn format_codex_permission_input(input: &shared::CodexPermissionInput) -> String {
    use shared::CodexPermissionInput as C;
    match input {
        C::FileChange {
            item_id,
            reason,
            grant_root,
        } => {
            let mut lines = vec![format!("File change for item `{}`", item_id)];
            if let Some(r) = reason.as_deref().filter(|s| !s.is_empty()) {
                lines.push(format!("Reason: {}", r));
            }
            if let Some(g) = grant_root.as_deref().filter(|s| !s.is_empty()) {
                lines.push(format!("Grant root: {}", g));
            }
            lines.push("(diff streamed above under this item id)".to_string());
            lines.join("\n")
        }
        C::ApplyPatch { file_changes, .. } => {
            // `file_changes` is a JSON object keyed by file path — list the paths.
            let paths: Vec<String> = file_changes
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            if paths.is_empty() {
                serde_json::to_string_pretty(file_changes).unwrap_or_default()
            } else {
                format!("Patch {} file(s):\n  {}", paths.len(), paths.join("\n  "))
            }
        }
        C::Bash { command, .. } | C::ExecCommand { command, .. } => {
            format!("$ {}", command)
        }
        C::Permissions { reason, .. } => reason
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                serde_json::to_string_pretty(input).unwrap_or_else(|_| format!("{:?}", input))
            }),
        C::McpElicitation { server_name } => {
            if server_name.is_empty() {
                "MCP server is asking for input".to_string()
            } else {
                format!("MCP server `{}` is asking for input", server_name)
            }
        }
        C::AskUserQuestion { .. } => {
            // The AskUserQuestion renderer is invoked elsewhere in the
            // permission dialog; this code path is only hit for the
            // standard-permission summary preview, so fall back to the
            // typed pretty-print.
            serde_json::to_string_pretty(input).unwrap_or_else(|_| format!("{:?}", input))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::format_permission_input;

    #[test]
    fn format_claude_bash_permission_uses_typed_tool_input() {
        let input = serde_json::json!({ "command": "cargo test -p frontend" });
        assert_eq!(
            format_permission_input("Bash", &input),
            "$ cargo test -p frontend"
        );
    }

    #[test]
    fn format_claude_file_permissions_use_typed_tool_input() {
        let read = serde_json::json!({ "file_path": "/tmp/a.txt" });
        let edit = serde_json::json!({
            "file_path": "/tmp/b.txt",
            "old_string": "old",
            "new_string": "new"
        });
        let write = serde_json::json!({
            "file_path": "/tmp/c.txt",
            "content": "body"
        });

        assert_eq!(format_permission_input("Read", &read), "/tmp/a.txt");
        assert_eq!(format_permission_input("Edit", &edit), "/tmp/b.txt");
        assert_eq!(format_permission_input("Write", &write), "/tmp/c.txt");
    }

    #[test]
    fn format_claude_permission_falls_back_for_malformed_named_input() {
        let input = serde_json::json!({ "cmd": "not the Bash field" });
        assert_eq!(
            format_permission_input("Bash", &input),
            serde_json::to_string_pretty(&input).unwrap()
        );
    }

    #[test]
    fn format_codex_permission_still_uses_typed_envelope_first() {
        let input = serde_json::json!({
            "tool": "bash",
            "command": "ls",
            "cwd": "/tmp"
        });
        assert_eq!(format_permission_input("Bash", &input), "$ ls");
    }
}
