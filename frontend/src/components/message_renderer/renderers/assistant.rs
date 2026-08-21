use super::super::shorten_model_name;
use super::media::render_image_source;
use super::tools::{
    render_code_execution_result, render_container_upload, render_mcp_tool_result,
    render_mcp_tool_use, render_server_tool_use, render_structured_block, render_unknown_block,
    render_web_search_result,
};
use crate::components::copy_button::CopyButton;
use crate::components::expandable::ExpandableText;
use crate::components::markdown::render_markdown_for_session;
use crate::components::tool_renderers::render_tool_use;
use shared::{AssistantMessage, AssistantUsage as UsageInfo};
use shared::{Citation, ContentBlock, ToolResultContent};
use uuid::Uuid;
use yew::prelude::*;

fn extract_ephemeral_cache(usage: &UsageInfo) -> (u64, u64) {
    usage
        .cache_creation
        .as_ref()
        .map(|cc| {
            (
                u64::from(cc.ephemeral_1h_input_tokens),
                u64::from(cc.ephemeral_5m_input_tokens),
            )
        })
        .unwrap_or((0, 0))
}

fn build_model_tooltip(model: &str, usage: Option<&UsageInfo>) -> String {
    let mut parts = vec![model.to_string()];
    if let Some(u) = usage {
        if let Some(tier) = &u.service_tier {
            parts.push(tier.clone());
        }
        if let Some(geo) = &u.inference_geo {
            parts.push(geo.clone());
        }
    }
    parts.join(" | ")
}

fn build_usage_tooltip(usage: Option<&UsageInfo>) -> String {
    usage
        .map(|u| {
            let mut tooltip = format!(
                "Input: {} | Output: {} | Cache read: {} | Cache created: {}",
                u.input_tokens,
                u.output_tokens,
                u.cache_read_input_tokens,
                u.cache_creation_input_tokens
            );
            let (e1h, e5m) = extract_ephemeral_cache(u);
            if e1h > 0 || e5m > 0 {
                tooltip.push_str(&format!(" | Ephemeral 1h: {} | Ephemeral 5m: {}", e1h, e5m));
            }
            tooltip
        })
        .unwrap_or_default()
}

pub(crate) fn assistant_label(model: &str) -> String {
    match shorten_model_name(model) {
        // Guard against unknown `claude-*` families shortening to a bare
        // vendor prefix, which would render as "Claude - claude".
        Some(short_name) if !short_name.eq_ignore_ascii_case("claude") => {
            format!("Claude - {short_name}")
        }
        _ => "Claude".to_string(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AssistantCompletionState {
    Interrupted,
    Resumed,
}

impl AssistantCompletionState {
    /// Read completion metadata through claude-codes' typed contract. These
    /// flags live on each assistant frame, so keeping the affordance with the
    /// frame body also preserves it when consecutive frames share one card.
    fn for_message(msg: &AssistantMessage) -> Vec<Self> {
        let mut states = Vec::with_capacity(2);
        if msg.aborted == Some(true) {
            states.push(Self::Interrupted);
        }
        if msg.resumed_from_incomplete_thinking == Some(true) {
            states.push(Self::Resumed);
        }
        states
    }

    fn label(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::Resumed => "resumed after truncation",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Interrupted => "Assistant response was interrupted before it completed",
            Self::Resumed => {
                "Assistant resumed an incomplete thinking block from the previous response"
            }
        }
    }

    fn class(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::Resumed => "resumed",
        }
    }
}

fn render_completion_states(msg: &AssistantMessage) -> Html {
    html! {
        <div class="assistant-completion-states">
            { for AssistantCompletionState::for_message(msg).into_iter().map(|state| html! {
                <span
                    class={classes!("assistant-completion-badge", state.class())}
                    title={state.description()}
                    aria-label={state.description()}
                >
                    { state.label() }
                </span>
            }) }
        </div>
    }
}

/// Extract concatenated raw text from a list of content blocks.
/// Used for the message header copy button: pulls out text and thinking
/// blocks as markdown, ignoring tool_use/tool_result internals.
fn content_blocks_to_text(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        match block {
            ContentBlock::Text(t) => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&t.text);
            }
            ContentBlock::Thinking(th) => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str("<thinking>\n");
                out.push_str(&th.thinking);
                out.push_str("\n</thinking>");
            }
            _ => {}
        }
    }
    out
}

pub fn render_assistant_message(
    msg: &AssistantMessage,
    timestamp: Option<&str>,
    session_id: Uuid,
) -> Html {
    let blocks = msg.message.content.clone();

    let usage = msg.message.usage.as_ref();
    let model = msg.message.model.as_str();
    let is_truncated = msg.message.stop_reason.as_ref().map(|r| r.as_str()) == Some("max_tokens");

    let model_tooltip = build_model_tooltip(model, usage);
    let usage_tooltip = build_usage_tooltip(usage);
    let label = assistant_label(model);
    let copy_text = content_blocks_to_text(&blocks);

    html! {
        <div class="claude-message assistant-message">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge assistant" title={model_tooltip}>{ label }</span>
                if !copy_text.is_empty() {
                    <CopyButton text={copy_text} title="Copy assistant text" />
                }
                {
                    if is_truncated {
                        html! { <span class="truncated-badge" title="Response was cut off (max_tokens)">{ "truncated" }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if let Some(u) = usage {
                        html! {
                            <span class="usage-badge" title={usage_tooltip}>
                                <span class="token-count">{ format!("{}↓ {}↑", u.input_tokens, u.output_tokens) }</span>
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">{ render_assistant_message_content(msg, session_id).unwrap_or_default() }</div>
        </div>
    }
}

pub fn render_assistant_message_content(msg: &AssistantMessage, session_id: Uuid) -> Option<Html> {
    let completion_states = AssistantCompletionState::for_message(msg);
    let content = render_content_blocks(&msg.message.content, session_id);
    if completion_states.is_empty() && content.is_none() {
        return None;
    }

    Some(html! {
        <>
            if !completion_states.is_empty() {
                { render_completion_states(msg) }
            }
            { content.unwrap_or_default() }
        </>
    })
}

/// Render a run of content blocks, returning `None` when *no* block produces
/// visible output (so callers can omit the surrounding wrapper/card entirely
/// rather than emit a spaced-but-empty element).
pub fn render_content_blocks(blocks: &[ContentBlock], session_id: Uuid) -> Option<Html> {
    let rendered: Vec<Html> = blocks
        .iter()
        .filter_map(|block| render_block(block, session_id))
        .collect();
    (!rendered.is_empty()).then(|| html! { <>{ for rendered.into_iter() }</> })
}

/// Render one content block. `None` means the block renders nothing (e.g. a
/// tool result that returned no content) — the caller drops it rather than
/// emitting an empty box.
fn render_block(block: &ContentBlock, session_id: Uuid) -> Option<Html> {
    let rendered = match block {
        ContentBlock::Text(t) => {
            html! {
                <div class="assistant-text">
                    { render_markdown_for_session(&t.text, session_id) }
                    { render_citations(&t.citations) }
                </div>
            }
        }
        ContentBlock::ToolUse(tu) => render_tool_use(&tu.name, &tu.input),
        ContentBlock::ToolResult(tr) => {
            let class = if tr.is_error.unwrap_or(false) {
                "tool-result error"
            } else {
                "tool-result"
            };
            match &tr.content {
                Some(ToolResultContent::Text(s)) => {
                    html! {
                        <div class={class}>
                            <ExpandableText full_text={s.clone()} max_len={500} class="tool-result-content" ansi=true />
                        </div>
                    }
                }
                Some(ToolResultContent::Structured(blocks)) => {
                    html! {
                        <div class={class}>
                            { for blocks.iter().map(|v| {
                                match serde_json::from_value::<shared::ContentBlock>(v.clone()) {
                                    Ok(typed) => render_structured_block(&typed),
                                    Err(_) => {
                                        let json = serde_json::to_string_pretty(v).unwrap_or_default();
                                        html! { <pre class="tool-result-content">{ json }</pre> }
                                    }
                                }
                            }) }
                        </div>
                    }
                }
                // A tool result with no content renders nothing —
                // drop it instead of emitting an empty box.
                None => return None,
            }
        }
        ContentBlock::Image(img) => render_image_source(&img.source, None),
        ContentBlock::Thinking(th) => {
            let sig_title = if th.signature.is_empty() {
                "No signature on this thinking block.".to_string()
            } else {
                format!("Encrypted thinking signature:\n{}", th.signature)
            };
            html! {
                <div class="thinking-block">
                    <span class="thinking-label" title={sig_title}>{ "thinking" }</span>
                    if th.thinking.trim().is_empty() {
                        <div class="thinking-content muted" title="Thinking text was omitted by the model; the encrypted signature is preserved in the raw message.">
                            { "thinking omitted" }
                        </div>
                    } else {
                        <div class="thinking-content">{ crate::components::markdown::linkify_urls(&th.thinking) }</div>
                    }
                </div>
            }
        }
        ContentBlock::ServerToolUse(stu) => render_server_tool_use(&stu.name, &stu.input),
        ContentBlock::WebSearchToolResult(r) => render_web_search_result(&r.content),
        ContentBlock::CodeExecutionToolResult(r) => render_code_execution_result(&r.content),
        ContentBlock::McpToolUse(mtu) => {
            render_mcp_tool_use(&mtu.name, mtu.server_name.as_deref(), &mtu.input)
        }
        ContentBlock::McpToolResult(r) => {
            render_mcp_tool_result(&r.content, r.is_error.unwrap_or(false))
        }
        ContentBlock::ContainerUpload(upload) => render_container_upload(&upload.data),
        ContentBlock::Fallback(fb) => {
            // Typed model-fallback notice: the response switched
            // models mid-stream (e.g. overload fallback). Render
            // the from → to transition rather than a raw blob.
            html! {
                <div class="assistant-text model-fallback-notice">
                    { format!("Model fallback: {} → {}", fb.from.model, fb.to.model) }
                </div>
            }
        }
        ContentBlock::Unknown(value) => render_unknown_block(value),
    };
    Some(rendered)
}

fn render_citations(citations: &[Citation]) -> Html {
    if citations.is_empty() {
        return html! {};
    }
    html! {
        <div class="citation-list">
            { for citations.iter().enumerate().map(|(i, cite)| {
                let url = cite.url.as_deref().unwrap_or("#");
                let title = cite.title.as_deref()
                    .or(cite.cited_text.as_deref())
                    .or(cite.document_title.as_deref())
                    .unwrap_or("source");
                html! {
                    <a class="citation-link"
                       href={url.to_string()}
                       target="_blank"
                       rel="noopener noreferrer"
                       title={title.to_string()}>
                        { format!("[{}]", i + 1) }
                    </a>
                }
            })}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(extra_fields: &str) -> AssistantMessage {
        let json = format!(
            r#"{{
                "type": "assistant",
                "message": {{
                    "id": "msg_test",
                    "role": "assistant",
                    "model": "claude-sonnet-4-5-20250929",
                    "content": []
                }},
                "session_id": "01890000-0000-7000-8000-000000000001"
                {extra_fields}
            }}"#
        );
        serde_json::from_str(&json).expect("valid typed assistant fixture")
    }

    #[test]
    fn completion_states_distinguish_interrupted_and_resumed_frames() {
        let interrupted = assistant(r#", "aborted": true"#);
        assert_eq!(
            AssistantCompletionState::for_message(&interrupted),
            vec![AssistantCompletionState::Interrupted]
        );

        let resumed = assistant(r#", "resumed_from_incomplete_thinking": true"#);
        assert_eq!(
            AssistantCompletionState::for_message(&resumed),
            vec![AssistantCompletionState::Resumed]
        );
    }

    #[test]
    fn false_or_absent_completion_flags_do_not_mark_normal_frames() {
        let normal = assistant("");
        assert!(AssistantCompletionState::for_message(&normal).is_empty());

        let explicitly_false =
            assistant(r#", "aborted": false, "resumed_from_incomplete_thinking": false"#);
        assert!(AssistantCompletionState::for_message(&explicitly_false).is_empty());
    }

    #[test]
    fn both_typed_flags_are_preserved_when_present() {
        let message = assistant(r#", "aborted": true, "resumed_from_incomplete_thinking": true"#);
        assert_eq!(
            AssistantCompletionState::for_message(&message),
            vec![
                AssistantCompletionState::Interrupted,
                AssistantCompletionState::Resumed
            ]
        );
    }
}
