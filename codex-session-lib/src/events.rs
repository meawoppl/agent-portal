use codex_codes::TokenUsageBreakdown;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ThreadStartedEvent<'a> {
    #[serde(rename = "type")]
    pub event_type: &'static str,
    pub thread_id: &'a str,
}

impl<'a> ThreadStartedEvent<'a> {
    pub fn new(thread_id: &'a str) -> Self {
        Self {
            event_type: "thread.started",
            thread_id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TurnCompletedEvent {
    #[serde(rename = "type")]
    pub event_type: &'static str,
    pub turn_id: String,
    pub status: String,
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<CodexUsageEvent>,
}

impl TurnCompletedEvent {
    pub fn new(
        turn_id: String,
        status: String,
        duration_ms: Option<i64>,
        usage: Option<CodexUsageEvent>,
    ) -> Self {
        Self {
            event_type: "turn.completed",
            turn_id,
            status,
            duration_ms,
            usage,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TurnFailedEvent {
    #[serde(rename = "type")]
    pub event_type: &'static str,
    pub error: CodexErrorEvent,
}

impl TurnFailedEvent {
    pub fn new(message: String) -> Self {
        Self {
            event_type: "turn.failed",
            error: CodexErrorEvent { message },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CodexErrorEvent {
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ItemEvent {
    #[serde(rename = "type")]
    pub event_type: &'static str,
    pub item: serde_json::Value,
}

impl ItemEvent {
    pub fn started(item: serde_json::Value) -> Self {
        Self {
            event_type: "item.started",
            item,
        }
    }

    pub fn completed(item: serde_json::Value) -> Self {
        Self {
            event_type: "item.completed",
            item,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ErrorEvent {
    #[serde(rename = "type")]
    pub event_type: &'static str,
    pub message: String,
}

impl ErrorEvent {
    pub fn new(message: String) -> Self {
        Self {
            event_type: "error",
            message,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct PassthroughEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub params: serde_json::Value,
}

impl PassthroughEvent {
    pub fn new(event_type: impl Into<String>, params: serde_json::Value) -> Self {
        Self {
            event_type: event_type.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CodexUsageEvent {
    pub last: TokenUsageBreakdown,
    pub total: TokenUsageBreakdown,
    pub model_context_window: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UserEchoEvent {
    #[serde(rename = "type")]
    pub event_type: &'static str,
    pub message: UserEchoMessage,
    pub content: String,
}

impl UserEchoEvent {
    pub fn new(prompt: String) -> Self {
        Self {
            event_type: "user",
            message: UserEchoMessage {
                role: "user",
                content: vec![TextContentBlock {
                    block_type: "text",
                    text: prompt.clone(),
                }],
            },
            content: prompt,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UserEchoMessage {
    pub role: &'static str,
    pub content: Vec<TextContentBlock>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct TextContentBlock {
    #[serde(rename = "type")]
    pub block_type: &'static str,
    pub text: String,
}

pub(crate) fn to_raw_output<T: Serialize>(event: &T) -> serde_json::Value {
    serde_json::to_value(event).unwrap_or(serde_json::Value::Null)
}
