mod renderers;
pub mod types;

use serde_json::Value;
use uuid::Uuid;
use yew::prelude::*;

use types::{ClaudeMessage, ContentBlock};

/// Extract `_created_at` from a raw JSON message string and format it as local time.
fn extract_local_timestamp(json: &str) -> Option<String> {
    let val: Value = serde_json::from_str(json).ok()?;
    let iso = val.get("_created_at")?.as_str()?;
    let ms = js_sys::Date::parse(iso);
    if ms.is_nan() {
        return None;
    }
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ms));
    date.to_locale_string("default", &js_sys::Object::new())
        .as_string()
}

/// Extract the raw `_created_at` ISO string from a message JSON, for use with
/// the live-updating TimeAgo component (which parses it itself).
pub(super) fn extract_raw_iso(json: &str) -> Option<String> {
    let val: Value = serde_json::from_str(json).ok()?;
    val.get("_created_at")?.as_str().map(|s| s.to_string())
}

/// A group of messages to render together
#[derive(Debug, Clone, PartialEq)]
pub enum MessageGroup {
    /// A single non-assistant message
    Single(String),
    /// Multiple consecutive assistant messages grouped together
    AssistantGroup(Vec<String>),
    /// One or more consecutive user/agent messages with the same display identity.
    IdentityGroup {
        label: String,
        badge_class: String,
        messages: Vec<String>,
    },
}

impl MessageGroup {
    /// Stable key for this group derived from the first message's identity.
    ///
    /// A positional index would change whenever an earlier group gets added
    /// or removed, causing Yew to throw away the group component and reset
    /// internal state of every expandable/collapsible inside it (bash
    /// command toggle, `ExpandableText`, image viewer, etc.). Using the
    /// first message's `_created_at` keeps the key stable across reorderings.
    /// `index` is used only as a fallback when no timestamp is present.
    pub fn key(&self, index: usize) -> yew::virtual_dom::Key {
        let first = match self {
            MessageGroup::Single(json) => json,
            MessageGroup::AssistantGroup(messages) => match messages.first() {
                Some(j) => j,
                None => return yew::virtual_dom::Key::from(format!("g{}", index)),
            },
            MessageGroup::IdentityGroup { messages, .. } => match messages.first() {
                Some(j) => j,
                None => return yew::virtual_dom::Key::from(format!("i{}", index)),
            },
        };
        let prefix = match self {
            MessageGroup::Single(_) => "s",
            MessageGroup::AssistantGroup(_) => "g",
            MessageGroup::IdentityGroup { .. } => "i",
        };
        match extract_raw_iso(first) {
            Some(iso) => yew::virtual_dom::Key::from(format!("{}-{}", prefix, iso)),
            None => yew::virtual_dom::Key::from(format!("{}{}", prefix, index)),
        }
    }
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
                        && blocks.iter().all(|b| {
                            matches!(
                                b,
                                ContentBlock::ToolResult { .. }
                                    | ContentBlock::WebSearchToolResult { .. }
                                    | ContentBlock::McpToolResult { .. }
                                    | ContentBlock::CodeExecutionToolResult { .. }
                            )
                        });
                }
            }
            false
        }
        _ => false,
    }
}

#[derive(Clone, PartialEq)]
struct MessageIdentity {
    label: String,
    badge_class: String,
}

fn message_identity(
    json: &str,
    agent_type: shared::AgentType,
    current_user_id: Option<&str>,
) -> Option<MessageIdentity> {
    match serde_json::from_str::<ClaudeMessage>(json) {
        Ok(ClaudeMessage::User(msg)) if msg.content.is_some() => {
            let label = match &msg.sender {
                Some(sender) if current_user_id != Some(sender.user_id.as_str()) => {
                    sender.name.clone()
                }
                _ => "You".to_string(),
            };
            Some(MessageIdentity {
                label,
                badge_class: "user".to_string(),
            })
        }
        Ok(ClaudeMessage::Assistant(_)) => Some(MessageIdentity {
            label: if agent_type == shared::AgentType::Codex {
                "Codex".to_string()
            } else {
                "Assistant".to_string()
            },
            badge_class: "assistant".to_string(),
        }),
        Ok(ClaudeMessage::Portal(_)) => Some(MessageIdentity {
            label: "Portal".to_string(),
            badge_class: "portal".to_string(),
        }),
        _ if agent_type == shared::AgentType::Codex
            && super::codex_renderer::is_codex_agent_message(json) =>
        {
            Some(MessageIdentity {
                label: "Codex".to_string(),
                badge_class: "assistant".to_string(),
            })
        }
        _ => None,
    }
}

fn push_identity_group(
    groups: &mut Vec<MessageGroup>,
    identity: &mut Option<MessageIdentity>,
    messages: &mut Vec<String>,
) {
    if messages.is_empty() {
        return;
    }
    if let Some(id) = identity.take() {
        groups.push(MessageGroup::IdentityGroup {
            label: id.label,
            badge_class: id.badge_class,
            messages: std::mem::take(messages),
        });
    }
}

/// Group consecutive user/agent messages by display identity, and group Claude
/// assistant tool-result runs for the legacy Claude renderer.
pub fn group_messages(
    messages: &[String],
    agent_type: shared::AgentType,
    current_user_id: Option<&str>,
) -> Vec<MessageGroup> {
    let mut groups = Vec::new();
    let mut current_assistant_group: Vec<String> = Vec::new();
    let mut current_identity: Option<MessageIdentity> = None;
    let mut current_identity_group: Vec<String> = Vec::new();

    for json in messages {
        if agent_type != shared::AgentType::Codex && should_group_with_assistant(json) {
            push_identity_group(
                &mut groups,
                &mut current_identity,
                &mut current_identity_group,
            );
            current_assistant_group.push(json.clone());
        } else if let Some(identity) = message_identity(json, agent_type, current_user_id) {
            if !current_assistant_group.is_empty() {
                groups.push(MessageGroup::AssistantGroup(std::mem::take(
                    &mut current_assistant_group,
                )));
            }
            if current_identity.as_ref() != Some(&identity) {
                push_identity_group(
                    &mut groups,
                    &mut current_identity,
                    &mut current_identity_group,
                );
                current_identity = Some(identity);
            }
            current_identity_group.push(json.clone());
        } else {
            push_identity_group(
                &mut groups,
                &mut current_identity,
                &mut current_identity_group,
            );
            if !current_assistant_group.is_empty() {
                groups.push(MessageGroup::AssistantGroup(std::mem::take(
                    &mut current_assistant_group,
                )));
            }
            groups.push(MessageGroup::Single(json.clone()));
        }
    }

    push_identity_group(
        &mut groups,
        &mut current_identity,
        &mut current_identity_group,
    );
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
    let ts = extract_local_timestamp(&props.json);
    let raw_iso = extract_raw_iso(&props.json);
    let parsed: Result<ClaudeMessage, _> = serde_json::from_str(&props.json);

    // Dispatch on the message shape, not the agent. `User` (the proxy's
    // synthetic echo) and `Portal` (the backend's portal-content envelope)
    // are protocol-agnostic and must render the same way on Claude and
    // Codex sessions — otherwise the Codex renderer's catch-all turns them
    // into raw JSON blocks. Codex-specific shapes (`item.started`,
    // `turn.completed`, …) don't match any `ClaudeMessage` variant and fall
    // through to the codex renderer below.
    match parsed {
        Ok(ClaudeMessage::System(msg)) => {
            return renderers::render_system_message(&msg, ts.as_deref());
        }
        Ok(ClaudeMessage::Assistant(msg)) => {
            return renderers::render_assistant_message(&msg, ts.as_deref(), raw_iso.as_deref());
        }
        Ok(ClaudeMessage::Result(msg)) => return renderers::render_result_message(&msg),
        Ok(ClaudeMessage::User(msg)) => {
            return renderers::render_user_message(
                &msg,
                props.current_user_id.as_deref(),
                ts.as_deref(),
            );
        }
        Ok(ClaudeMessage::Error(msg)) => {
            return renderers::render_error_message(&msg, ts.as_deref());
        }
        Ok(ClaudeMessage::Portal(msg)) => {
            return renderers::render_portal_message(&msg, ts.as_deref());
        }
        Ok(ClaudeMessage::RateLimitEvent(msg)) => {
            return renderers::render_rate_limit_event(&msg, ts.as_deref());
        }
        Ok(ClaudeMessage::Unknown) | Err(_) => {}
    }

    if props.agent_type == shared::AgentType::Codex {
        html! { <super::codex_renderer::CodexMessageRenderer json={props.json.clone()} /> }
    } else {
        render_raw_json(&props.json)
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
        MessageGroup::IdentityGroup {
            label,
            badge_class,
            messages,
        } => {
            let ts = messages
                .first()
                .and_then(|json| extract_local_timestamp(json));
            let last_iso = messages.last().and_then(|json| extract_raw_iso(json));
            html! {
                <div class={classes!("claude-message", if badge_class == "user" { "user-message" } else { "assistant-message" })}>
                    <div class="message-header" title={ts.unwrap_or_default()}>
                        <span class={classes!("message-type-badge", badge_class.clone())}>{ label }</span>
                        if messages.len() > 1 {
                            <span class="message-count" title={format!("{} consecutive messages", messages.len())}>
                                { format!("× {}", messages.len()) }
                            </span>
                        }
                    </div>
                    <div class="message-body grouped-message-body">
                        { for messages.iter().enumerate().map(|(i, json)| {
                            let key = extract_raw_iso(json)
                                .map(|iso| format!("m-{}", iso))
                                .unwrap_or_else(|| format!("m{}", i));
                            html! { <div {key} class="grouped-message-part">{ render_identity_group_part(json, props.agent_type) }</div> }
                        })}
                    </div>
                    if let Some(iso) = last_iso {
                        <div class="message-footer">
                            <crate::components::time_ago::TimeAgo iso={iso} />
                        </div>
                    }
                </div>
            }
        }
        MessageGroup::AssistantGroup(messages) => {
            let ts = messages
                .first()
                .and_then(|json| extract_local_timestamp(json));
            renderers::render_assistant_group(messages, ts.as_deref())
        }
    }
}

fn render_identity_group_part(json: &str, agent_type: shared::AgentType) -> Html {
    match serde_json::from_str::<ClaudeMessage>(json) {
        Ok(ClaudeMessage::User(msg)) => renderers::render_user_message_content(&msg),
        Ok(ClaudeMessage::Assistant(msg)) => renderers::render_assistant_message_content(&msg),
        Ok(ClaudeMessage::Portal(msg)) => renderers::render_portal_message_content(&msg),
        _ if agent_type == shared::AgentType::Codex => {
            super::codex_renderer::render_codex_message_content(json)
        }
        _ => html! {},
    }
}

fn render_raw_json(json: &str) -> Html {
    let display = serde_json::from_str::<Value>(json)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
        .unwrap_or_else(|| json.to_string());

    html! {
        <div class="claude-message raw-message">
            <div class="message-header">
                <span class="message-type-badge raw">{ "Unrecognized Message" }</span>
            </div>
            <div class="message-body">
                <pre class="raw-json">{ display }</pre>
                <p class="raw-message-hint">
                    { "This message type is not yet supported by the portal. " }
                    <a href="https://github.com/meawoppl/rust-code-agent-sdks/issues"
                       target="_blank" rel="noopener noreferrer">
                        { "Report this issue" }
                    </a>
                </p>
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
