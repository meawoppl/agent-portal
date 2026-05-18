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

/// Category for a run of consecutive related messages — drives both the
/// grouping decision (`classify`) and the wrapper style on the rendered
/// group (`MessageGroupRenderer`). PR 1 of the roadmap in #758 introduces
/// `Assistant` only; PR 2 / PR 3 add `Portal`, `User`, `Codex`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupCategory {
    /// Assistant messages and the user-shaped envelopes that carry only
    /// tool results back to the agent.
    Assistant,
}

impl GroupCategory {
    /// Short stable prefix for `MessageGroup::key`. Don't change without
    /// understanding the Yew diff implications — these strings end up in
    /// virtual-dom keys and switching them mid-flight would re-mount every
    /// group component on the page.
    fn key_prefix(self) -> &'static str {
        match self {
            GroupCategory::Assistant => "g",
        }
    }
}

/// A group of messages to render together.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageGroup {
    /// A single message that doesn't classify into any group category.
    /// Kept as a distinct variant (rather than a one-element `Grouped`) so
    /// the most common case avoids the group-wrapper render path and keeps
    /// its Yew key stable independent of category.
    Single(String),
    /// Multiple consecutive messages sharing a `GroupCategory`.
    Grouped {
        category: GroupCategory,
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
        let (prefix, first) = match self {
            MessageGroup::Single(json) => ("s", json.as_str()),
            MessageGroup::Grouped { category, messages } => match messages.first() {
                Some(j) => (category.key_prefix(), j.as_str()),
                None => {
                    return yew::virtual_dom::Key::from(format!(
                        "{}{}",
                        category.key_prefix(),
                        index
                    ));
                }
            },
        };
        match extract_raw_iso(first) {
            Some(iso) => yew::virtual_dom::Key::from(format!("{}-{}", prefix, iso)),
            None => yew::virtual_dom::Key::from(format!("{}{}", prefix, index)),
        }
    }
}

/// Check if a message should be grouped with assistant messages.
///
/// Groups together: assistant turns + the user-shaped envelopes that carry
/// only tool results back to Claude. The decision is made on the **nested**
/// `message.content` blocks alone — we deliberately do NOT short-circuit on
/// the top-level `content` field, because that field is the optimistic-send
/// envelope shape and can leak onto real echoes through the cross-process
/// wire wrapping. Gating on it broke serial Read tool-use grouping in the
/// wild (#758).
fn should_group_with_assistant(json: &str) -> bool {
    match serde_json::from_str::<ClaudeMessage>(json) {
        Ok(ClaudeMessage::Assistant(_)) => true,
        Ok(ClaudeMessage::User(msg)) => {
            let Some(message) = &msg.message else {
                return false;
            };
            let Some(blocks) = &message.content else {
                return false;
            };
            !blocks.is_empty()
                && blocks.iter().all(|b| {
                    matches!(
                        b,
                        ContentBlock::ToolResult { .. }
                            | ContentBlock::WebSearchToolResult { .. }
                            | ContentBlock::McpToolResult { .. }
                            | ContentBlock::CodeExecutionToolResult { .. }
                    )
                })
        }
        _ => false,
    }
}

/// Classify a single wire message into the group category it belongs to,
/// or `None` if it shouldn't roll into any group (renders as `Single`).
///
/// Sole entry point for "what kind of group does this message belong to"
/// across the codebase — add new categories here, not at the `group_messages`
/// loop level.
fn classify(json: &str) -> Option<GroupCategory> {
    if should_group_with_assistant(json) {
        return Some(GroupCategory::Assistant);
    }
    None
}

/// Walk `messages` and collapse consecutive same-category runs into
/// `MessageGroup::Grouped`. Mixed / `None` messages become `MessageGroup::Single`.
pub fn group_messages(messages: &[String]) -> Vec<MessageGroup> {
    let mut groups = Vec::new();
    let mut current: Option<(GroupCategory, Vec<String>)> = None;

    fn flush(out: &mut Vec<MessageGroup>, slot: &mut Option<(GroupCategory, Vec<String>)>) {
        if let Some((category, messages)) = slot.take() {
            out.push(MessageGroup::Grouped { category, messages });
        }
    }

    for json in messages {
        match classify(json) {
            Some(cat) => match current.as_mut() {
                Some((cur_cat, msgs)) if *cur_cat == cat => msgs.push(json.clone()),
                _ => {
                    flush(&mut groups, &mut current);
                    current = Some((cat, vec![json.clone()]));
                }
            },
            None => {
                flush(&mut groups, &mut current);
                groups.push(MessageGroup::Single(json.clone()));
            }
        }
    }

    flush(&mut groups, &mut current);
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
        MessageGroup::Grouped {
            category: GroupCategory::Assistant,
            messages,
        } => {
            let ts = messages
                .first()
                .and_then(|json| extract_local_timestamp(json));
            renderers::render_assistant_group(messages, ts.as_deref())
        }
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

    /// Realistic Claude wire shape for a user message containing a single
    /// `tool_result` content block (the kind Read / Bash / Edit etc. produce).
    /// Matches `claude-codes` 2.1.x `ClaudeOutput::User(UserMessage)`
    /// serialization with the backend's wire envelope additions
    /// (`_created_at` etc.).
    fn read_tool_result_user_message(tool_use_id: &str) -> String {
        serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": "file contents...",
                }]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
            "_created_at": "2026-05-17T10:00:00.000Z",
        })
        .to_string()
    }

    /// Realistic Claude assistant message with a single `tool_use` block.
    fn assistant_with_tool_use(tool_use_id: &str, tool_name: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": tool_use_id,
                    "name": tool_name,
                    "input": {"file_path": "/some/path"},
                }]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
            "_created_at": "2026-05-17T10:00:00.000Z",
        })
        .to_string()
    }

    /// A tool-result user message coming from a Claude session MUST classify
    /// into the assistant group — otherwise serial Read tool uses don't roll
    /// together with their preceding assistant turn.
    ///
    /// This is the regression target for the "serial Read tool uses don't
    /// group" symptom on Claude sessions.
    #[test]
    fn user_tool_result_classifies_with_assistant() {
        let user_tool_result = read_tool_result_user_message("toolu_01abc");
        assert!(
            should_group_with_assistant(&user_tool_result),
            "user-tool-result message should group with assistant; got false"
        );
    }

    /// Sanity: two consecutive (assistant tool_use + user tool_result) pairs
    /// must collapse into a single `AssistantGroup` of length 4. If the
    /// classifier above is broken, this falls apart.
    #[test]
    fn serial_read_tool_uses_collapse_into_one_group() {
        let messages = vec![
            assistant_with_tool_use("toolu_01", "Read"),
            read_tool_result_user_message("toolu_01"),
            assistant_with_tool_use("toolu_02", "Read"),
            read_tool_result_user_message("toolu_02"),
        ];
        let groups = group_messages(&messages);
        assert_eq!(
            groups.len(),
            1,
            "expected one AssistantGroup carrying all 4 messages, got {} groups",
            groups.len()
        );
        match &groups[0] {
            MessageGroup::Grouped {
                category: GroupCategory::Assistant,
                messages,
            } => assert_eq!(messages.len(), 4),
            other => panic!("expected an Assistant Grouped run, got {:?}", other),
        }
    }

    /// Edge case: top-level `content` field on a user-tool-result message
    /// (e.g. from the optimistic-send envelope leaking onto a real echo)
    /// trips the existing `msg.content.is_some()` early-bail and breaks the
    /// run. This is a candidate root cause for the reported regression on
    /// production Claude sessions even though the canonical wire shape
    /// doesn't carry top-level `content`.
    #[test]
    fn user_tool_result_with_top_level_content_still_groups() {
        let with_top_level_content = serde_json::json!({
            "type": "user",
            "content": "stale optimistic content",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_01",
                    "content": "file contents...",
                }]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
            "_created_at": "2026-05-17T10:00:00.000Z",
        })
        .to_string();
        assert!(
            should_group_with_assistant(&with_top_level_content),
            "user-tool-result with a stale top-level `content` field should \
             still group with the assistant; the predicate must look at the \
             nested message blocks, not the envelope's top-level field"
        );
    }

    /// Edge case: real user input (plain text typed by the human, not a
    /// tool result) must NOT join the assistant group, otherwise prose
    /// would silently get rolled into a previous assistant block.
    #[test]
    fn real_user_text_does_not_group_with_assistant() {
        let plain_user = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": "hello agent"}]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
            "_created_at": "2026-05-17T10:00:00.000Z",
        })
        .to_string();
        assert!(
            !should_group_with_assistant(&plain_user),
            "plain-text user message must NOT group with assistant"
        );
    }

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
