use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Proxy token types in separate module
pub mod proxy_tokens;
pub use proxy_tokens::*;

// Typed WebSocket endpoint definitions
pub mod endpoints;
pub use endpoints::*;

// Protocol constants shared between backend and proxy
pub mod protocol;

// API client types and trait
pub mod api;
pub use api::{ApiClientConfig, ApiError, CcProxyApi, SoundSettingsResponse};

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
    ContentBlock, ImageBlock, ImageSource, PermissionSuggestion, TextBlock, ThinkingBlock,
    ToolResultBlock, ToolResultContent, ToolUseBlock,
};

// Re-export claude-codes output types for typed parsing (aliased to avoid conflicts with
// frontend's local lenient types in message_renderer/types.rs)
pub use claude_codes::io::{
    ResultMessage as CCResultMessage, SystemMessage as CCSystemMessage,
    SystemSubtype as CCSystemSubtype, TaskNotificationMessage, TaskProgressMessage,
    TaskStartedMessage, TaskStatus as CCTaskStatus, TaskType as CCTaskType, TaskUsage,
};
pub use claude_codes::ClaudeOutput;

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

/// Voice WebSocket message types (frontend <-> backend via /ws/voice/:id).
/// These are NOT part of the typed ws-bridge endpoints because voice mixes
/// binary audio frames with JSON text messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VoiceMessage {
    /// Start voice recording session (frontend -> backend)
    StartVoice {
        session_id: Uuid,
        #[serde(default = "default_language_code")]
        language_code: String,
    },

    /// Stop voice recording (frontend -> backend)
    StopVoice { session_id: Uuid },

    /// Transcription result from speech-to-text (backend -> frontend)
    Transcription {
        session_id: Uuid,
        transcript: String,
        is_final: bool,
        confidence: f32,
    },

    /// Voice error (backend -> frontend)
    VoiceError { session_id: Uuid, message: String },

    /// Voice session ended (backend -> frontend)
    VoiceEnded { session_id: Uuid },
}

fn default_language_code() -> String {
    "en-US".to_string()
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
}

impl SessionStatus {
    pub fn as_str(&self) -> &str {
        match self {
            SessionStatus::Active => "active",
            SessionStatus::Inactive => "inactive",
            SessionStatus::Disconnected => "disconnected",
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
    #[serde(default)]
    pub updated_at: Option<String>,
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
    /// Which agent CLI backs this session (claude or codex)
    #[serde(default)]
    pub agent_type: AgentType,
    /// Proxy client version string (e.g. "1.3.39")
    #[serde(default)]
    pub client_version: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    /// Whether voice input is enabled for this user (admin-controlled)
    #[serde(default)]
    pub voice_enabled: bool,
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

impl std::fmt::Display for MessageRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::System => "system",
            Self::Assistant => "assistant",
            Self::User => "user",
            Self::Result => "result",
            Self::Error => "error",
            Self::Portal => "portal",
            Self::Unknown => "unknown",
        };
        f.write_str(s)
    }
}

impl MessageRole {
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
    pub fn text(text: String) -> Self {
        Self {
            message_type: "portal".to_string(),
            content: vec![PortalContent::Text { text }],
        }
    }

    pub fn image(media_type: String, data: String) -> Self {
        Self {
            message_type: "portal".to_string(),
            content: vec![PortalContent::Image {
                media_type,
                data,
                file_path: None,
                file_size: None,
            }],
        }
    }

    pub fn image_with_info(
        media_type: String,
        data: String,
        file_path: Option<String>,
        file_size: Option<u64>,
    ) -> Self {
        Self {
            message_type: "portal".to_string(),
            content: vec![PortalContent::Image {
                media_type,
                data,
                file_path,
                file_size,
            }],
        }
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
            } => f
                .debug_struct("Image")
                .field("media_type", media_type)
                .field("data", &format_args!("<{} bytes base64>", data.len()))
                .field("file_path", file_path)
                .field("file_size", file_size)
                .finish(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageInfo {
    pub id: Uuid,
    pub role: MessageRole,
    pub content: String,
    pub created_at: String,
}

// ============================================================================
// Device Flow Types (shared between backend and proxy)
// ============================================================================

/// Request to poll for device flow completion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePollRequest {
    pub device_code: String,
}

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_status_serialization() {
        assert_eq!(SessionStatus::Active.as_str(), "active");
        assert_eq!(SessionStatus::Inactive.as_str(), "inactive");
        assert_eq!(SessionStatus::Disconnected.as_str(), "disconnected");

        let json = serde_json::to_string(&SessionStatus::Active).unwrap();
        assert_eq!(json, "\"active\"");
    }
}
