use super::super::shorten_model_name;
use super::render_image_source;
use super::tools::{
    render_code_execution_result, render_container_upload, render_mcp_tool_result,
    render_mcp_tool_use, render_server_tool_use, render_structured_block, render_unknown_block,
    render_web_search_result,
};
use crate::components::copy_button::CopyButton;
use crate::components::expandable::ExpandableText;
use crate::components::markdown::render_markdown_for_session;
use crate::components::time_ago::TimeAgo;
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
    raw_iso: Option<&str>,
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
            <div class="message-body">{ render_assistant_message_content(msg, session_id) }</div>
            if let Some(iso) = raw_iso {
                <div class="message-footer">
                    <TimeAgo iso={iso.to_string()} />
                </div>
            }
        </div>
    }
}

pub fn render_assistant_message_content(msg: &AssistantMessage, session_id: Uuid) -> Html {
    let blocks = msg.message.content.clone();
    render_content_blocks(&blocks, session_id)
}

pub fn render_content_blocks(blocks: &[ContentBlock], session_id: Uuid) -> Html {
    html! {
        <>
            {
                blocks.iter().map(|block| {
                    match block {
                        ContentBlock::Text(t) => {
                            html! {
                                <div class="assistant-text">
                                    { render_markdown_for_session(&t.text, session_id) }
                                    { render_citations(&t.citations) }
                                </div>
                            }
                        }
                        ContentBlock::ToolUse(tu) => {
                            render_tool_use(&tu.name, &tu.input)
                        }
                        ContentBlock::ToolResult(tr) => {
                            let class = if tr.is_error.unwrap_or(false) { "tool-result error" } else { "tool-result" };
                            match &tr.content {
                                Some(ToolResultContent::Text(s)) => {
                                    html! {
                                        <div class={class}>
                                            <ExpandableText full_text={s.clone()} max_len={500} class="tool-result-content" />
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
                                None => html! { <div class={class}></div> },
                            }
                        }
                        ContentBlock::Image(img) => {
                            render_image_source(&img.source, None)
                        }
                        ContentBlock::Thinking(th) => {
                            html! {
                                <div class="thinking-block">
                                    <span class="thinking-label">{ "thinking" }</span>
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
                        ContentBlock::ServerToolUse(stu) => {
                            render_server_tool_use(&stu.name, &stu.input)
                        }
                        ContentBlock::WebSearchToolResult(r) => {
                            render_web_search_result(&r.content)
                        }
                        ContentBlock::CodeExecutionToolResult(r) => {
                            render_code_execution_result(&r.content)
                        }
                        ContentBlock::McpToolUse(mtu) => {
                            render_mcp_tool_use(&mtu.name, mtu.server_name.as_deref(), &mtu.input)
                        }
                        ContentBlock::McpToolResult(r) => {
                            render_mcp_tool_result(&r.content, r.is_error.unwrap_or(false))
                        }
                        ContentBlock::ContainerUpload(upload) => {
                            render_container_upload(&upload.data)
                        }
                        ContentBlock::Unknown(value) => {
                            render_unknown_block(value)
                        }
                    }
                }).collect::<Html>()
            }
        </>
    }
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
