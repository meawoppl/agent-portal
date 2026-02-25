mod renderers;
pub mod types;

use gloo_net::http::Request;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

use types::{ClaudeMessage, ContentBlock};

/// A group of messages to render together
#[derive(Debug, Clone, PartialEq)]
pub enum MessageGroup {
    /// A single non-assistant message
    Single(String),
    /// Multiple consecutive assistant messages grouped together
    AssistantGroup(Vec<String>),
}

/// Check if a message should be grouped with assistant messages
/// This includes assistant messages AND tool result messages (user messages containing only tool results)
fn should_group_with_assistant(json: &str) -> bool {
    match serde_json::from_str::<ClaudeMessage>(json) {
        Ok(ClaudeMessage::Assistant(_)) => true,
        Ok(ClaudeMessage::User(msg)) => {
            if msg.content.is_some() {
                return false;
            }
            if let Some(message) = &msg.message {
                if let Some(blocks) = &message.content {
                    return !blocks.is_empty()
                        && blocks
                            .iter()
                            .all(|b| matches!(b, ContentBlock::ToolResult { .. }));
                }
            }
            false
        }
        _ => false,
    }
}

/// Group consecutive assistant messages (and their tool results) together
pub fn group_messages(messages: &[String]) -> Vec<MessageGroup> {
    let mut groups = Vec::new();
    let mut current_assistant_group: Vec<String> = Vec::new();

    for json in messages {
        if should_group_with_assistant(json) {
            current_assistant_group.push(json.clone());
        } else {
            if !current_assistant_group.is_empty() {
                groups.push(MessageGroup::AssistantGroup(std::mem::take(
                    &mut current_assistant_group,
                )));
            }
            groups.push(MessageGroup::Single(json.clone()));
        }
    }

    if !current_assistant_group.is_empty() {
        groups.push(MessageGroup::AssistantGroup(current_assistant_group));
    }

    groups
}

// --- Components ---

#[derive(Properties, PartialEq)]
pub struct MessageRendererProps {
    pub json: String,
    #[prop_or_default]
    pub session_id: Option<Uuid>,
    #[prop_or_default]
    pub agent_type: shared::AgentType,
    #[prop_or_default]
    pub current_user_id: Option<String>,
}

#[function_component(MessageRenderer)]
pub fn message_renderer(props: &MessageRendererProps) -> Html {
    if props.agent_type == shared::AgentType::Codex {
        return html! {
            <super::codex_renderer::CodexMessageRenderer json={props.json.clone()} />
        };
    }

    let parsed: Result<ClaudeMessage, _> = serde_json::from_str(&props.json);

    match parsed {
        Ok(ClaudeMessage::System(msg)) => renderers::render_system_message(&msg),
        Ok(ClaudeMessage::Assistant(msg)) => renderers::render_assistant_message(&msg),
        Ok(ClaudeMessage::Result(msg)) => renderers::render_result_message(&msg),
        Ok(ClaudeMessage::User(msg)) => {
            renderers::render_user_message(&msg, props.current_user_id.as_deref())
        }
        Ok(ClaudeMessage::Error(msg)) => renderers::render_error_message(&msg),
        Ok(ClaudeMessage::Portal(msg)) => renderers::render_portal_message(&msg),
        Ok(ClaudeMessage::RateLimitEvent(msg)) => renderers::render_rate_limit_event(&msg),
        Ok(ClaudeMessage::Unknown) | Err(_) => {
            html! { <RawMessageRenderer json={props.json.clone()} session_id={props.session_id} /> }
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct MessageGroupRendererProps {
    pub group: MessageGroup,
    #[prop_or_default]
    pub session_id: Option<Uuid>,
    #[prop_or_default]
    pub agent_type: shared::AgentType,
    #[prop_or_default]
    pub current_user_id: Option<String>,
}

#[function_component(MessageGroupRenderer)]
pub fn message_group_renderer(props: &MessageGroupRendererProps) -> Html {
    match &props.group {
        MessageGroup::Single(json) => {
            html! { <MessageRenderer json={json.clone()} session_id={props.session_id} agent_type={props.agent_type} current_user_id={props.current_user_id.clone()} /> }
        }
        MessageGroup::AssistantGroup(messages) => renderers::render_assistant_group(messages),
    }
}

// --- Raw message logging ---

#[derive(Serialize)]
struct LogRawMessageRequest {
    session_id: Option<Uuid>,
    message_content: Value,
    message_source: String,
    render_reason: Option<String>,
}

fn log_raw_message(session_id: Option<Uuid>, json: &str, reason: &str) {
    let message_content =
        serde_json::from_str::<Value>(json).unwrap_or_else(|_| Value::String(json.to_string()));

    let request_body = LogRawMessageRequest {
        session_id,
        message_content,
        message_source: "frontend".to_string(),
        render_reason: Some(reason.to_string()),
    };

    spawn_local(async move {
        let result = Request::post("/api/raw-messages")
            .header("Content-Type", "application/json")
            .json(&request_body)
            .map_err(|e| format!("Failed to serialize: {:?}", e))
            .map(|req| req.send());

        if let Ok(future) = result {
            if let Err(e) = future.await {
                log::warn!("Failed to log raw message: {:?}", e);
            }
        }
    });
}

#[derive(Properties, PartialEq)]
pub struct RawMessageRendererProps {
    pub json: String,
    #[prop_or_default]
    pub session_id: Option<Uuid>,
}

#[function_component(RawMessageRenderer)]
pub fn raw_message_renderer(props: &RawMessageRendererProps) -> Html {
    let json = props.json.clone();
    let session_id = props.session_id;

    use_effect_with(json.clone(), move |json| {
        let reason = match serde_json::from_str::<Value>(json) {
            Ok(val) => {
                if let Some(msg_type) = val.get("type").and_then(|t| t.as_str()) {
                    format!("Unknown message type: {}", msg_type)
                } else {
                    "Message has no 'type' field".to_string()
                }
            }
            Err(e) => format!("JSON parse error: {}", e),
        };

        log_raw_message(session_id, json, &reason);
        || ()
    });

    render_raw_json(&props.json)
}

fn render_raw_json(json: &str) -> Html {
    let display = serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| json.to_string());

    html! {
        <div class="claude-message raw-message">
            <div class="message-header">
                <span class="message-type-badge raw">{ "Raw" }</span>
            </div>
            <div class="message-body">
                <pre class="raw-json">{ display }</pre>
            </div>
        </div>
    }
}

// --- Utility functions (used by renderers and tool_renderers) ---

pub fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

pub(crate) fn shorten_model_name(model: &str) -> Option<String> {
    if model.is_empty() || model.starts_with('<') {
        return None;
    }

    let extract_version = |model: &str| -> Option<String> {
        let parts: Vec<&str> = model.split('-').collect();
        for i in 0..parts.len().saturating_sub(1) {
            if let (Ok(major), Ok(minor)) = (parts[i].parse::<u32>(), parts[i + 1].parse::<u32>()) {
                if parts[i + 1].len() >= 8 {
                    continue;
                }
                return Some(format!("{}.{}", major, minor));
            }
        }
        None
    };

    let version = extract_version(model);

    Some(if model.contains("opus") {
        match version {
            Some(v) => format!("Opus {}", v),
            None => "Opus".to_string(),
        }
    } else if model.contains("sonnet") {
        match version {
            Some(v) => format!("Sonnet {}", v),
            None => "Sonnet".to_string(),
        }
    } else if model.contains("haiku") {
        match version {
            Some(v) => format!("Haiku {}", v),
            None => "Haiku".to_string(),
        }
    } else {
        model.split('-').next().unwrap_or(model).to_string()
    })
}

pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60000;
        let secs = (ms % 60000) / 1000;
        format!("{}m {}s", mins, secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shorten_model_name() {
        assert_eq!(
            shorten_model_name("claude-opus-4-5-20251101"),
            Some("Opus 4.5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-sonnet-4-5-20250929"),
            Some("Sonnet 4.5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-haiku-4-5-20251001"),
            Some("Haiku 4.5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-3-5-sonnet-20241022"),
            Some("Sonnet 3.5".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-opus-4-6"),
            Some("Opus 4.6".to_string())
        );
        assert_eq!(
            shorten_model_name("claude-sonnet-4-5"),
            Some("Sonnet 4.5".to_string())
        );
        assert_eq!(shorten_model_name("claude-opus"), Some("Opus".to_string()));
        assert_eq!(shorten_model_name(""), None);
        assert_eq!(shorten_model_name("<unknown>"), None);
        assert_eq!(shorten_model_name("gpt-4-turbo"), Some("gpt".to_string()));
    }
}
