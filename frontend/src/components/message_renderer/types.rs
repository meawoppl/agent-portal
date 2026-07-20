//! Thin frontend-only wrappers around the shared Claude Code wire types.
//!
//! Claude messages should parse through `shared::ClaudeOutput`, which re-exports
//! `claude-codes` types. The local shapes below exist only for Portal's
//! frontend-specific envelope and optimistic user messages synthesized before
//! the proxy echoes a typed Claude user frame.

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, PartialEq)]
pub struct RenderedMessage {
    pub content: String,
    pub meta: Option<shared::PortalMeta>,
}

impl RenderedMessage {
    pub fn new(content: String, meta: Option<shared::PortalMeta>) -> Self {
        Self { content, meta }
    }

    pub fn raw_iso(&self) -> Option<&str> {
        shared::created_at_iso(self.meta.as_ref())
    }

    pub fn delivery(&self) -> Option<&shared::DeliveryMeta> {
        self.meta.as_ref()?.delivery.as_ref()
    }

    pub fn source(&self) -> Option<&shared::MessageSource> {
        self.meta.as_ref()?.source()
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum ClaudeMessage {
    System(shared::SystemMessage),
    Assistant(shared::AssistantMessage),
    Result(shared::ResultMessage),
    User(shared::UserMessage),
    Error(shared::AnthropicError),
    Portal(PortalMessage),
    RateLimitEvent(shared::RateLimitEvent),
    ToolProgress(shared::ToolProgressMessage),
    OptimisticUser(OptimisticUserMessage),
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PortalMessage {
    #[serde(default)]
    pub content: Vec<shared::PortalContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OptimisticUserMessage {
    pub content: String,
}

impl ClaudeMessage {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        if let Ok(output) = serde_json::from_str::<shared::ClaudeOutput>(json) {
            return Ok(match output {
                shared::ClaudeOutput::System(msg) => Self::System(msg),
                shared::ClaudeOutput::User(msg) => Self::User(msg),
                shared::ClaudeOutput::Assistant(msg) => Self::Assistant(msg),
                shared::ClaudeOutput::Result(msg) => Self::Result(msg),
                shared::ClaudeOutput::Error(msg) => Self::Error(msg),
                shared::ClaudeOutput::RateLimitEvent(msg) => Self::RateLimitEvent(msg),
                shared::ClaudeOutput::ToolProgress(msg) => Self::ToolProgress(msg),
                // Wildcard: control frames plus the 2.1.160 wire additions
                // (stream_event, transcript variants, …) that have no
                // dedicated renderer yet.
                _ => Self::Unknown,
            });
        }

        serde_json::from_str::<LocalMessage>(json).map(Into::into)
    }
}

impl<'de> Deserialize<'de> for ClaudeMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Ok(output) = serde_json::from_value::<shared::ClaudeOutput>(value.clone()) {
            return Ok(match output {
                shared::ClaudeOutput::System(msg) => Self::System(msg),
                shared::ClaudeOutput::User(msg) => Self::User(msg),
                shared::ClaudeOutput::Assistant(msg) => Self::Assistant(msg),
                shared::ClaudeOutput::Result(msg) => Self::Result(msg),
                shared::ClaudeOutput::Error(msg) => Self::Error(msg),
                shared::ClaudeOutput::RateLimitEvent(msg) => Self::RateLimitEvent(msg),
                shared::ClaudeOutput::ToolProgress(msg) => Self::ToolProgress(msg),
                // Wildcard: control frames plus the 2.1.160 wire additions
                // (stream_event, transcript variants, …) that have no
                // dedicated renderer yet.
                _ => Self::Unknown,
            });
        }
        serde_json::from_value::<LocalMessage>(value)
            .map(Into::into)
            .map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum LocalMessage {
    #[serde(rename = "portal")]
    Portal(PortalMessage),
    #[serde(rename = "user")]
    OptimisticUser(OptimisticUserMessage),
    #[serde(other)]
    Unknown,
}

impl From<LocalMessage> for ClaudeMessage {
    fn from(value: LocalMessage) -> Self {
        match value {
            LocalMessage::Portal(msg) => Self::Portal(msg),
            LocalMessage::OptimisticUser(msg) => Self::OptimisticUser(msg),
            LocalMessage::Unknown => Self::Unknown,
        }
    }
}
