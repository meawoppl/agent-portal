//! Shared types for the dashboard module

use serde::Deserialize;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use uuid::Uuid;

/// Answers for multiple AskUserQuestion questions
/// Key is question index, value is the selected answer(s)
pub type QuestionAnswers = HashMap<usize, String>;

/// Storage key for paused sessions in localStorage
pub const PAUSED_SESSIONS_STORAGE_KEY: &str = "claude-portal-paused-sessions";

/// Storage key for inactive hidden state in localStorage
pub const INACTIVE_HIDDEN_STORAGE_KEY: &str = "claude-portal-inactive-hidden";

/// Storage key for cost display visibility in localStorage
pub const SHOW_COST_STORAGE_KEY: &str = "claude-portal-show-cost";

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

/// Load cost display preference from localStorage (default: shown)
pub fn load_show_cost() -> bool {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(SHOW_COST_STORAGE_KEY).ok().flatten())
        .map(|v| v != "false")
        .unwrap_or(true)
}

/// Save cost display preference to localStorage
pub fn save_show_cost(show: bool) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let _ = storage.set_item(SHOW_COST_STORAGE_KEY, if show { "true" } else { "false" });
    }
}

/// Load paused session IDs from localStorage
pub fn load_paused_sessions() -> HashSet<Uuid> {
    web_sys::window()
        .and_then(|w| w.local_storage().ok().flatten())
        .and_then(|storage| storage.get_item(PAUSED_SESSIONS_STORAGE_KEY).ok().flatten())
        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
        .map(|ids| ids.iter().filter_map(|s| Uuid::parse_str(s).ok()).collect())
        .unwrap_or_default()
}

/// Save paused session IDs to localStorage
pub fn save_paused_sessions(paused: &HashSet<Uuid>) {
    if let Some(storage) = web_sys::window().and_then(|w| w.local_storage().ok().flatten()) {
        let ids: Vec<String> = paused.iter().map(|id| id.to_string()).collect();
        if let Ok(json) = serde_json::to_string(&ids) {
            let _ = storage.set_item(PAUSED_SESSIONS_STORAGE_KEY, &json);
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
        _ => serde_json::to_string_pretty(input).unwrap_or_else(|_| format!("{:?}", input)),
    }
}
