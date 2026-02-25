//! Message type definitions for parsing Claude Code JSON output.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use shared::ToolResultContent;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeMessage {
    #[serde(rename = "system")]
    System(SystemMessage),
    #[serde(rename = "assistant")]
    Assistant(AssistantMessage),
    #[serde(rename = "result")]
    Result(ResultMessage),
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "error")]
    Error(ErrorMessage),
    #[serde(rename = "portal")]
    Portal(PortalMessage),
    #[serde(rename = "rate_limit_event")]
    RateLimitEvent(RateLimitEventMessage),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PortalMessage {
    #[serde(default)]
    pub content: Vec<shared::PortalContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSender {
    pub user_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserMessage {
    pub content: Option<String>,
    pub message: Option<UserMessageContent>,
    #[serde(default, rename = "_sender")]
    pub sender: Option<MessageSender>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserMessageContent {
    pub content: Option<Vec<ContentBlock>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ErrorDetails {
    #[serde(rename = "type")]
    pub error_type: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ErrorMessage {
    pub message: Option<String>,
    pub error: Option<ErrorDetails>,
    pub request_id: Option<String>,
}

impl ErrorMessage {
    pub fn is_overload(&self) -> bool {
        self.error
            .as_ref()
            .and_then(|e| e.error_type.as_deref())
            .map(|t| t == "overloaded_error")
            .unwrap_or(false)
    }

    pub fn display_message(&self) -> &str {
        self.error
            .as_ref()
            .and_then(|e| e.message.as_deref())
            .or(self.message.as_deref())
            .unwrap_or("Unknown error")
    }

    pub fn error_type(&self) -> Option<&str> {
        self.error.as_ref().and_then(|e| e.error_type.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RateLimitEventMessage {
    pub rate_limit_info: Option<RateLimitInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RateLimitInfo {
    pub status: Option<String>,
    #[serde(rename = "resetsAt")]
    pub resets_at: Option<u64>,
    #[serde(rename = "rateLimitType")]
    pub rate_limit_type: Option<String>,
    pub utilization: Option<f64>,
    #[serde(rename = "overageStatus")]
    pub overage_status: Option<String>,
    #[serde(rename = "overageDisabledReason")]
    pub overage_disabled_reason: Option<String>,
    #[serde(rename = "isUsingOverage")]
    pub is_using_overage: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SystemMessage {
    pub subtype: Option<String>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub claude_code_version: Option<String>,
    pub tools: Option<Vec<String>>,
    pub agents: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub slash_commands: Option<Vec<String>>,
    pub mcp_servers: Option<Vec<Value>>,
    pub plugins: Option<Vec<Value>>,
    pub summary: Option<String>,
    pub leaf_message_count: Option<u32>,
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub extra: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AssistantMessage {
    pub message: Option<MessageContent>,
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MessageContent {
    pub id: Option<String>,
    pub model: Option<String>,
    pub role: Option<String>,
    pub content: Option<Vec<ContentBlock>>,
    pub usage: Option<UsageInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { source: ImageSource },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: Option<ToolResultContent>,
        #[serde(default)]
        is_error: bool,
    },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageInfo {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_creation_input_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResultMessage {
    pub subtype: Option<String>,
    pub session_id: Option<String>,
    pub result: Option<String>,
    pub is_error: Option<bool>,
    pub duration_ms: Option<u64>,
    pub duration_api_ms: Option<u64>,
    pub total_cost_usd: Option<f64>,
    pub num_turns: Option<u64>,
    pub usage: Option<UsageInfo>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_message_overload_detection() {
        let msg = ErrorMessage {
            message: None,
            error: Some(ErrorDetails {
                error_type: Some("overloaded_error".to_string()),
                message: Some("Overloaded".to_string()),
            }),
            request_id: Some("req_123".to_string()),
        };
        assert!(msg.is_overload());
        assert_eq!(msg.display_message(), "Overloaded");
        assert_eq!(msg.error_type(), Some("overloaded_error"));
    }

    #[test]
    fn test_error_message_regular_error() {
        let msg = ErrorMessage {
            message: Some("Something went wrong".to_string()),
            error: None,
            request_id: None,
        };
        assert!(!msg.is_overload());
        assert_eq!(msg.display_message(), "Something went wrong");
        assert_eq!(msg.error_type(), None);
    }

    #[test]
    fn test_error_message_api_error() {
        let msg = ErrorMessage {
            message: None,
            error: Some(ErrorDetails {
                error_type: Some("invalid_request_error".to_string()),
                message: Some("Invalid API key".to_string()),
            }),
            request_id: Some("req_456".to_string()),
        };
        assert!(!msg.is_overload());
        assert_eq!(msg.display_message(), "Invalid API key");
        assert_eq!(msg.error_type(), Some("invalid_request_error"));
    }

    #[test]
    fn test_error_message_empty() {
        let msg = ErrorMessage::default();
        assert!(!msg.is_overload());
        assert_eq!(msg.display_message(), "Unknown error");
        assert_eq!(msg.error_type(), None);
    }
}
