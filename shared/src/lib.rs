use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Proxy token types in separate module
pub mod proxy_tokens;
pub use proxy_tokens::*;

// Typed WebSocket endpoint definitions
pub mod endpoints;
pub use endpoints::*;

// Chunked image upload protocol (mixed JSON + binary WS frames)
pub mod image_upload;
pub use image_upload::{ImageUploadClientMsg, ImageUploadEndpoint, ImageUploadServerMsg};

// Protocol constants shared between backend and proxy
pub mod protocol;

// String/number formatting helpers shared between frontend and native crates
pub mod fmt;

// Timezone canonicalization (abbreviation -> IANA) shared across crates
pub mod timezone;

// API client types and trait
pub mod api;
pub use api::{
    CodexPermissionInput, CompactionExtra, InitExtra, ModelUsage, ModelUsageEntry,
    SoundSettingsResponse, TaskNotificationExtra, TurnMetrics, TurnMetricsResponse,
};

/// Default backend URL based on build profile.
/// Release builds point to `wss://txcl.io`, debug builds to `ws://localhost:3000`.
pub fn default_backend_url() -> &'static str {
    if cfg!(debug_assertions) {
        "ws://localhost:3000"
    } else {
        "wss://txcl.io"
    }
}

// Re-export claude-codes types for frontend message parsing
pub use claude_codes::io::{
    Citation, ContentBlock, ImageBlock, ImageSource, ImageSourceType, MediaType,
    PermissionSuggestion, TextBlock, ThinkingBlock, ToolResultBlock, ToolResultContent,
    ToolUseBlock,
};

// Re-export claude-codes output types for typed parsing.
pub use claude_codes::io::{
    AnthropicError, AssistantMessage, AssistantUsage, MessageContent, RateLimitEvent,
    ResultMessage, ServerToolUse, SystemMessage, SystemSubtype, TaskNotificationMessage,
    TaskProgressMessage, TaskStartedMessage, TaskStatus, TaskType, TaskUsage, UserMessage,
};
pub use claude_codes::CacheCreationDetails;
pub use claude_codes::ClaudeOutput;

// Re-export typed tool-input types so frontend renderers can match on enum
// variants instead of poking at JSON field names.
pub use claude_codes::tool_inputs::{
    AskUserQuestionInput, BashInput, EditInput, GlobInput, GrepInput, Question, QuestionOption,
    ReadInput, TaskInput, TodoItem, TodoStatus, TodoWriteInput, ToolInput, WebFetchInput,
    WebSearchInput, WriteInput,
};
pub use claude_codes::{AllowedPrompt, ExitPlanModeInput};

/// Returns true when a system message marks the END of a context compaction.
///
/// The CLI uses several spellings depending on version and code path, so this
/// helper centralizes the predicate. Callers should use this instead of
/// inlining the disjunction.
pub fn is_compaction_boundary(sys: &SystemMessage) -> bool {
    sys.is_compact_boundary()
        || matches!(
            sys.subtype.as_str(),
            "compaction" | "context_compaction" | "summary"
        )
}

/// Which agent CLI backs a session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentType {
    #[default]
    Claude,
    Codex,
}

impl AgentType {
    pub fn as_str(&self) -> &str {
        match self {
            AgentType::Claude => "claude",
            AgentType::Codex => "codex",
        }
    }
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for AgentType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "claude" => Ok(AgentType::Claude),
            "codex" => Ok(AgentType::Codex),
            other => Err(format!("unknown agent type: {}", other)),
        }
    }
}

/// Cost and token usage information for a single session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionCost {
    pub session_id: Uuid,
    pub total_cost_usd: f64,
    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_creation_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionStatus {
    Active,
    Inactive,
    Disconnected,
    Replaced,
}

impl SessionStatus {
    pub fn as_str(&self) -> &str {
        match self {
            SessionStatus::Active => "active",
            SessionStatus::Inactive => "inactive",
            SessionStatus::Disconnected => "disconnected",
            SessionStatus::Replaced => "replaced",
        }
    }
}

impl SendMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SendMode::Normal => "normal",
            SendMode::Wiggum => "wiggum",
        }
    }
}

/// Send mode for user input
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum SendMode {
    /// Normal single message send
    #[default]
    Normal,
    /// Wiggum mode - iterative autonomous loop until completion
    /// Proxy will re-send the prompt after each result until Claude responds with "DONE"
    Wiggum,
}

/// A directory entry returned by the launcher's filesystem listing
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DirectoryEntry {
    pub name: String,
    pub is_dir: bool,
}

/// Result of probing one agent CLI on a launcher host. Built at launcher
/// startup (sent in `LauncherRegister`) and refreshed on demand via
/// `ProbeAgents` when the user opens the launch dialog.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentInstall {
    pub agent_type: AgentType,
    /// True iff `<bin> --version` exited successfully.
    pub installed: bool,
    /// Absolute path the launcher's `which` lookup resolved to, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_path: Option<String>,
    /// `<bin> --version` stdout, trimmed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Info about a connected launcher daemon
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LauncherInfo {
    pub launcher_id: Uuid,
    pub launcher_name: String,
    pub hostname: String,
    pub connected: bool,
    pub running_sessions: u32,
    /// Working directory where the launcher process is running
    #[serde(default)]
    pub working_directory: Option<String>,
    /// Launcher binary version
    #[serde(default)]
    pub version: String,
}

/// A single open GitHub pull request associated with a session's repository.
///
/// Carried as a list so the session pill can show every open PR (one row per
/// PR, repo-root link above them) and use the branch as a hover tooltip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrRef {
    /// PR number (e.g. 34). Used for the `#34` pill label and sort order.
    pub number: i64,
    /// Full GitHub PR URL.
    pub url: String,
    /// Head branch name (`headRefName`), shown as the pill tooltip.
    pub branch: String,
}

/// API types for HTTP endpoints
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: Uuid,
    pub user_id: Uuid,
    pub session_name: String,
    pub session_key: String,
    pub working_directory: String,
    pub status: SessionStatus,
    pub last_activity: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub git_branch: Option<String>,
    /// The current user's role in this session (owner, editor, viewer)
    pub my_role: String,
    /// Hostname of the machine running the session
    #[serde(default)]
    pub hostname: String,
    /// Launcher ID if this session was started by a launcher
    #[serde(default)]
    pub launcher_id: Option<Uuid>,
    /// GitHub PR URL for the current branch
    #[serde(default)]
    pub pr_url: Option<String>,
    /// GitHub repository URL
    #[serde(default)]
    pub repo_url: Option<String>,
    /// All open PRs in the session's repo, sorted by number. Drives the pill's
    /// PR list (one row each) and the `#34 #35` collapsed label.
    #[serde(default)]
    pub open_prs: Vec<PrRef>,
    /// Which agent CLI backs this session (claude or codex)
    #[serde(default)]
    pub agent_type: AgentType,
    /// Proxy client version string (e.g. "1.3.39")
    #[serde(default)]
    pub client_version: Option<String>,
    /// Scheduled task ID if this session was spawned by a scheduled task
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_id: Option<Uuid>,
    /// Server-side pause flag. Paused sessions are resumable on demand but
    /// launchers must not auto-restart them during reconnect/startup.
    #[serde(default)]
    pub paused: bool,
    /// Arguments used when launching the agent CLI.
    #[serde(default)]
    pub claude_args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    Assistant,
    User,
    Result,
    Error,
    Portal,
    #[serde(other)]
    Unknown,
}

impl MessageRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Assistant => "assistant",
            Self::User => "user",
            Self::Result => "result",
            Self::Error => "error",
            Self::Portal => "portal",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a message-type string; any unrecognized value maps to `Unknown`.
    pub fn from_type_str(s: &str) -> Self {
        match s {
            "system" => Self::System,
            "assistant" => Self::Assistant,
            "user" => Self::User,
            "result" => Self::Result,
            "error" => Self::Error,
            "portal" => Self::Portal,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A portal-originated message that can carry text or images.
/// Serializes with `"type": "portal"` for the frontend's `ClaudeMessage` enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalMessage {
    /// Always "portal" — used as the serde tag for ClaudeMessage dispatch
    #[serde(rename = "type")]
    pub message_type: String,
    pub content: Vec<PortalContent>,
}

impl PortalMessage {
    /// The invariant `"type"` tag value for portal messages.
    pub const MESSAGE_TYPE: &'static str = "portal";

    /// Build a portal message with the invariant `"type":"portal"` tag.
    pub fn with_content(content: Vec<PortalContent>) -> Self {
        Self {
            message_type: Self::MESSAGE_TYPE.to_string(),
            content,
        }
    }

    pub fn text(text: String) -> Self {
        Self::with_content(vec![PortalContent::Text { text }])
    }

    pub fn image(media_type: String, data: String) -> Self {
        Self::with_content(vec![PortalContent::Image {
            media_type,
            data,
            file_path: None,
            file_size: None,
            source_type: None,
        }])
    }

    pub fn image_with_info(
        media_type: String,
        data: String,
        file_path: Option<String>,
        file_size: Option<u64>,
    ) -> Self {
        Self::with_content(vec![PortalContent::Image {
            media_type,
            data,
            file_path,
            file_size,
            source_type: None,
        }])
    }

    /// Build a collapsible "portal features reminder" message — same envelope
    /// as text/image portal messages, rendered with a header bar and a
    /// click-to-expand body on the frontend.
    pub fn reminder(title: String, body: String) -> Self {
        Self::with_content(vec![PortalContent::Reminder { title, body }])
    }

    pub fn continuation_prompt(
        continuation_id: Uuid,
        reset_at: String,
        status: String,
        source_message: String,
    ) -> Self {
        Self::with_content(vec![PortalContent::ContinuationPrompt {
            continuation_id,
            reset_at,
            status,
            source_message,
        }])
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PortalContent {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_size: Option<u64>,
        /// "base64" (default) or "url" (served from /api/images/{id})
        #[serde(default)]
        source_type: Option<String>,
    },
    /// Collapsible "portal features reminder" emitted at session start and
    /// after compaction boundaries. The body is markdown — rendered through
    /// the same pipeline as text portal messages — and lives behind a
    /// click-to-expand header on the frontend so it doesn't clutter the
    /// scrollback.
    Reminder {
        title: String,
        body: String,
    },
    /// Action card shown when Claude reports a hard session limit. The
    /// frontend may schedule a one-shot continuation for `reset_at`; the
    /// launcher only injects it if the original local process is still alive.
    ContinuationPrompt {
        continuation_id: Uuid,
        reset_at: String,
        status: String,
        source_message: String,
    },
}

impl std::fmt::Debug for PortalContent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text { text } => f.debug_struct("Text").field("text", text).finish(),
            Self::Image {
                media_type,
                data,
                file_path,
                file_size,
                source_type,
            } => f
                .debug_struct("Image")
                .field("media_type", media_type)
                .field("data", &format_args!("<{} bytes>", data.len()))
                .field("file_path", file_path)
                .field("file_size", file_size)
                .field("source_type", source_type)
                .finish(),
            Self::Reminder { title, body } => f
                .debug_struct("Reminder")
                .field("title", title)
                .field("body", &format_args!("<{} bytes>", body.len()))
                .finish(),
            Self::ContinuationPrompt {
                continuation_id,
                reset_at,
                status,
                source_message,
            } => f
                .debug_struct("ContinuationPrompt")
                .field("continuation_id", continuation_id)
                .field("reset_at", reset_at)
                .field("status", status)
                .field("source_message", source_message)
                .finish(),
        }
    }
}

// ============================================================================
// Device Flow Types (shared between backend and proxy)
// ============================================================================

/// Response from device flow polling
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum DevicePollResponse {
    #[serde(rename = "pending")]
    Pending,
    #[serde(rename = "complete")]
    Complete {
        access_token: String,
        user_id: String,
        user_email: String,
    },
    #[serde(rename = "expired")]
    Expired,
    #[serde(rename = "denied")]
    Denied,
}

// ============================================================================
// App Configuration (served to frontend)
// ============================================================================

/// Application configuration returned by /api/config endpoint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Custom title for the app (displayed in top bar)
    /// Defaults to "Agent Portal" if not configured; override with APP_TITLE env var
    pub app_title: String,
    /// Backend server version string (e.g. "1.3.24")
    pub server_version: String,
    /// When set, replaces the marketing splash page with a minimal login page
    /// displaying this text as the heading. Set via SPLASH_TEXT env var.
    #[serde(default)]
    pub splash_text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_serialization() {
        assert_eq!(SessionStatus::Active.as_str(), "active");
        assert_eq!(SessionStatus::Inactive.as_str(), "inactive");
        assert_eq!(SessionStatus::Disconnected.as_str(), "disconnected");
        assert_eq!(SessionStatus::Replaced.as_str(), "replaced");

        let json = serde_json::to_string(&SessionStatus::Active).unwrap();
        assert_eq!(json, "\"active\"");

        let replaced: SessionStatus = serde_json::from_str("\"replaced\"").unwrap();
        assert_eq!(replaced, SessionStatus::Replaced);
    }

    #[test]
    fn message_role_as_str_matches_serde_encoding() {
        let roles = [
            MessageRole::System,
            MessageRole::Assistant,
            MessageRole::User,
            MessageRole::Result,
            MessageRole::Error,
            MessageRole::Portal,
            MessageRole::Unknown,
        ];
        for role in roles {
            // Display / as_str must agree with the serde wire encoding.
            let json = serde_json::to_string(&role).unwrap();
            assert_eq!(json, format!("\"{}\"", role.as_str()));
            assert_eq!(role.to_string(), role.as_str());
            // from_type_str round-trips every known encoding.
            assert_eq!(MessageRole::from_type_str(role.as_str()), role);
        }
        // Unrecognized strings fall back to Unknown.
        assert_eq!(
            MessageRole::from_type_str("not-a-role"),
            MessageRole::Unknown
        );
        assert_eq!(MessageRole::from_type_str(""), MessageRole::Unknown);
    }

    #[test]
    fn portal_message_serializes_with_portal_tag() {
        let msg = PortalMessage::text("hello".to_string());
        let json = msg.to_json();
        assert_eq!(json["type"], "portal");
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "hello");

        let custom = PortalMessage::with_content(vec![PortalContent::Reminder {
            title: "t".to_string(),
            body: "b".to_string(),
        }]);
        assert_eq!(custom.message_type, PortalMessage::MESSAGE_TYPE);
        assert_eq!(custom.to_json()["type"], "portal");
    }
}
