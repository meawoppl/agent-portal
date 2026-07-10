// Ratchet for the workspace unwrap/expect deny (#1165 item 8): this crate
// still has production unwrap/expect; remove this allow as it is cleaned.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The portal version, `major.minor.{git commit count}` — derived at build
/// time by `build.rs` (issue #1096) so no PR ever edits a version line. Every
/// crate reports this single value (`server_version`, launcher `version`, the
/// frontend footer, the `agent-portal` client version). The Cargo.toml
/// `[workspace.package] version` supplies only `major.minor`; its patch is a
/// placeholder.
pub const VERSION: &str = env!("PORTAL_VERSION");

/// Split [`VERSION`] into `(major, minor, patch)` numeric components.
/// `None` on the (impossible-by-construction) malformed string.
pub fn version_parts() -> Option<(u64, u64, u64)> {
    let mut it = VERSION.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    Some((major, minor, patch))
}

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
    AgentSessionInfo, AgentSessionsResponse, CodexPermissionInput, InitExtra, ModelUsage,
    ModelUsageEntry, SendAgentMessageRequest, SendAgentMessageResponse, SoundSettingsResponse,
    TaskNotificationExtra, TurnMetrics, TurnMetricsResponse,
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
    /// Version of the connected launcher that owns this session, when known.
    ///
    /// This is a live connection property, not persisted session state, so it
    /// is absent when the launcher is disconnected or the session did not
    /// originate from a launcher.
    #[serde(default)]
    pub launcher_version: Option<String>,
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

    /// Build an explicit inter-agent message event. The proxy converts this
    /// envelope to agent-facing text before delivery, while the frontend
    /// renders the typed event directly.
    pub fn agent_message(from_agent_type: String, from_session_id: String, text: String) -> Self {
        Self::with_content(vec![PortalContent::AgentMessage {
            from_agent_type,
            from_session_id,
            text,
        }])
    }

    /// Text form to send to the agent for portal event envelopes that have an
    /// agent-facing representation.
    pub fn agent_facing_text(&self) -> Option<String> {
        let [PortalContent::AgentMessage {
            from_agent_type,
            from_session_id,
            text,
        }] = self.content.as_slice()
        else {
            return None;
        };
        let reply_session_id = short_reply_session_id(from_session_id);
        Some(format!(
            "[message from {from_agent_type} {from_session_id}]\n{text}\n\n\
<system-reminder>\n\
This message came from another agent. Reply to that agent, not the user.\n\
Sender session id: {from_session_id}\n\
Reply with:\n\
agent-portal message send {reply_session_id} \"your reply\"\n\
</system-reminder>"
        ))
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_default()
    }
}

fn short_reply_session_id(session_id: &str) -> String {
    let compact = session_id.replace('-', "");
    if compact.len() >= 8 && compact.chars().all(|c| c.is_ascii_hexdigit()) {
        compact.chars().take(8).collect()
    } else {
        session_id.to_string()
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
    /// Typed event for an inter-agent message. This replaces UI parsing of
    /// the agent-facing `[message from ...]` text prefix for new messages.
    #[serde(rename = "agent_message")]
    AgentMessage {
        from_agent_type: String,
        from_session_id: String,
        text: String,
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
            Self::AgentMessage {
                from_agent_type,
                from_session_id,
                text,
            } => f
                .debug_struct("AgentMessage")
                .field("from_agent_type", from_agent_type)
                .field("from_session_id", from_session_id)
                .field("text", text)
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
    fn version_is_well_formed() {
        // Derived by build.rs; must always be `major.minor.patch`, all numeric
        // (issue #1096). A parse failure means the derivation broke.
        let (major, minor, _patch) = version_parts().expect("VERSION is major.minor.patch");
        // major.minor track the workspace Cargo.toml; patch is the commit
        // count (fallback to the Cargo patch when git is absent).
        assert!(major >= 2, "unexpected major in {VERSION}");
        let _ = minor;
        assert_eq!(VERSION.split('.').count(), 3, "VERSION = {VERSION}");
    }

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
    fn agent_message_round_trips_to_agent_facing_text() {
        // This is the exact envelope→text conversion behind the inter-agent
        // raw-JSON render bug (#1123/#1124): a single AgentMessage content
        // becomes the bracketed "[message from …]" prefix the agent reads.
        let msg = PortalMessage::agent_message(
            "codex".to_string(),
            "12345678-0000-0000-0000-000000000000".to_string(),
            "hello there".to_string(),
        );
        let text = msg.agent_facing_text().expect("agent-facing text");
        assert!(text
            .starts_with("[message from codex 12345678-0000-0000-0000-000000000000]\nhello there"));
        assert!(text.contains("Reply to that agent, not the user."));
        assert!(text.contains("agent-portal message send 12345678 \"your reply\""));
    }

    #[test]
    fn non_agent_message_has_no_agent_facing_text() {
        // Plain text / image / reminder envelopes have no agent-facing form;
        // returning None keeps them on the normal display path.
        assert!(PortalMessage::text("just text".to_string())
            .agent_facing_text()
            .is_none());
        assert!(
            PortalMessage::reminder("title".to_string(), "body".to_string())
                .agent_facing_text()
                .is_none()
        );
    }

    #[test]
    fn multi_content_message_has_no_agent_facing_text() {
        // The match requires *exactly one* AgentMessage content; a mixed or
        // multi-block envelope must not be mistaken for an inter-agent message.
        let msg = PortalMessage::with_content(vec![
            PortalContent::AgentMessage {
                from_agent_type: "codex".to_string(),
                from_session_id: "id".to_string(),
                text: "a".to_string(),
            },
            PortalContent::Text {
                text: "trailing".to_string(),
            },
        ]);
        assert!(msg.agent_facing_text().is_none());
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

    #[test]
    fn portal_agent_message_serializes_and_has_agent_facing_text() {
        let msg = PortalMessage::agent_message(
            "codex".to_string(),
            "12345678-0000-0000-0000-000000000000".to_string(),
            "hello from another agent".to_string(),
        );
        let json = msg.to_json();
        assert_eq!(json["type"], "portal");
        assert_eq!(json["content"][0]["type"], "agent_message");
        assert_eq!(json["content"][0]["from_agent_type"], "codex");
        assert_eq!(json["content"][0]["text"], "hello from another agent");
        let text = msg.agent_facing_text().expect("agent-facing text");
        assert!(text.starts_with(
            "[message from codex 12345678-0000-0000-0000-000000000000]\nhello from another agent"
        ));
        assert!(text.contains("agent-portal message send 12345678 \"your reply\""));
    }

    #[test]
    fn compact_boundary_uses_sdk_summary_stats_aliases() {
        let json = serde_json::json!({
            "type": "system",
            "subtype": "compact_boundary",
            "session_id": "session-1",
            "compact_metadata": { "pre_tokens": 2000, "trigger": "auto" },
            "content": "summarized earlier context",
            "message_count": 7,
            "duration_ms": 1234
        });
        let output: ClaudeOutput = serde_json::from_value(json).unwrap();
        let ClaudeOutput::System(system) = output else {
            panic!("expected system output");
        };

        let compact = system.as_compact_boundary().expect("compact boundary");
        assert_eq!(
            compact.summary.as_deref(),
            Some("summarized earlier context")
        );
        assert_eq!(compact.leaf_message_count, Some(7));
        assert_eq!(compact.duration_ms, Some(1234));
    }
}
