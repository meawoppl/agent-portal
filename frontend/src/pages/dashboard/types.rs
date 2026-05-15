//! Shared types for the dashboard module

use serde::Deserialize;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
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

/// Type alias for WebSocket sender to reduce type complexity
pub type WsSender = Rc<RefCell<Option<ws_bridge::yew_client::Sender<shared::ClientEndpoint>>>>;

/// Message data from the API
#[derive(Clone, PartialEq, Deserialize)]
pub struct MessageData {
    pub role: String,
    pub content: String,
    /// ISO 8601 timestamp when message was created
    pub created_at: String,
    /// User ID of who sent this message (for user-role messages)
    #[serde(default)]
    pub user_id: Option<String>,
    /// Display name of the sender (looked up from users table)
    #[serde(default)]
    pub sender_name: Option<String>,
}

/// Response from messages API endpoint
#[derive(Clone, PartialEq, Deserialize)]
pub struct MessagesResponse {
    pub messages: Vec<MessageData>,
}

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

/// Load whether inactive sessions section is hidden from localStorage
pub fn load_inactive_hidden() -> bool {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(INACTIVE_HIDDEN_STORAGE_KEY).ok().flatten())
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Save inactive hidden state to localStorage
pub fn save_inactive_hidden(hidden: bool) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(
            INACTIVE_HIDDEN_STORAGE_KEY,
            if hidden { "true" } else { "false" },
        );
    }
}

/// Load cost display preference from localStorage (default: hidden)
pub fn load_show_cost() -> bool {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(SHOW_COST_STORAGE_KEY).ok().flatten())
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Save cost display preference to localStorage
pub fn save_show_cost(show: bool) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(SHOW_COST_STORAGE_KEY, if show { "true" } else { "false" });
    }
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
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|storage| {
            storage
                .get_item(RAIL_ORIENTATION_STORAGE_KEY)
                .ok()
                .flatten()
        })
        .and_then(|v| RailPosition::from_str(&v))
        .unwrap_or(RailPosition::Top)
}

/// Save rail position preference to localStorage.
pub fn save_rail_position(position: RailPosition) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(RAIL_ORIENTATION_STORAGE_KEY, position.as_str());
    }
}

/// Load hidden session IDs from localStorage
pub fn load_hidden_sessions() -> HashSet<Uuid> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(HIDDEN_SESSIONS_STORAGE_KEY).ok().flatten())
        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
        .map(|ids| ids.iter().filter_map(|s| Uuid::parse_str(s).ok()).collect())
        .unwrap_or_default()
}

/// Save hidden session IDs to localStorage
pub fn save_hidden_sessions(hidden: &HashSet<Uuid>) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let ids: Vec<String> = hidden.iter().map(|id| id.to_string()).collect();
        if let Ok(json) = serde_json::to_string(&ids) {
            let _ = storage.set_item(HIDDEN_SESSIONS_STORAGE_KEY, &json);
        }
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

/// Format permission input for display
pub fn format_permission_input(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| format!("$ {}", s))
            .unwrap_or_else(|| serde_json::to_string_pretty(input).unwrap_or_default()),
        "Read" | "Edit" | "Write" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| serde_json::to_string_pretty(input).unwrap_or_default()),
        // Codex 0.130+ approval types — single-line summaries so the permission
        // card doesn't drop a full JSON blob into the dialog.
        "ExecCommand" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(|s| format!("$ {}", s))
            .unwrap_or_else(|| serde_json::to_string_pretty(input).unwrap_or_default()),
        "ApplyPatch" => {
            // `fileChanges` is a JSON object keyed by file path — list the paths.
            let paths: Vec<String> = input
                .get("fileChanges")
                .and_then(|v| v.as_object())
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            if paths.is_empty() {
                serde_json::to_string_pretty(input).unwrap_or_default()
            } else {
                format!("Patch {} file(s):\n  {}", paths.len(), paths.join("\n  "))
            }
        }
        "Permissions" => input
            .get("reason")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .unwrap_or_else(|| serde_json::to_string_pretty(input).unwrap_or_default()),
        "McpElicitation" => input
            .get("serverName")
            .and_then(|v| v.as_str())
            .map(|s| format!("MCP server `{}` is asking for input", s))
            .unwrap_or_else(|| serde_json::to_string_pretty(input).unwrap_or_default()),
        _ => serde_json::to_string_pretty(input).unwrap_or_else(|_| format!("{:?}", input)),
    }
}
