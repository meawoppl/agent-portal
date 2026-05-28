use super::super::shorten_model_name;
use super::super::types::{AssistantMessage, ContentBlock, UsageInfo};
use super::render_image_source;
use super::tools::{
    render_code_execution_result, render_container_upload, render_mcp_tool_result,
    render_mcp_tool_use, render_server_tool_use, render_structured_block, render_unknown_block,
    render_web_search_result,
};
use crate::components::copy_button::CopyButton;
use crate::components::expandable::ExpandableText;
use crate::components::markdown::render_markdown;
use crate::components::time_ago::TimeAgo;
use crate::components::tool_renderers::render_tool_use;
use shared::{Citation, ToolResultContent};
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
                u.input_tokens.unwrap_or(0),
                u.output_tokens.unwrap_or(0),
                u.cache_read_input_tokens.unwrap_or(0),
                u.cache_creation_input_tokens.unwrap_or(0)
            );
            let (e1h, e5m) = extract_ephemeral_cache(u);
            if e1h > 0 || e5m > 0 {
                tooltip.push_str(&format!(" | Ephemeral 1h: {} | Ephemeral 5m: {}", e1h, e5m));
            }
            tooltip
        })
        .unwrap_or_default()
}

/// Extract concatenated raw text from a list of content blocks.
/// Used for the message header copy button: pulls out text and thinking
/// blocks as markdown, ignoring tool_use/tool_result internals.
fn content_blocks_to_text(blocks: &[ContentBlock]) -> String {
    let mut out = String::new();
    for block in blocks {
        match block {
            ContentBlock::Text { text, .. } => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(text);
            }
            ContentBlock::Thinking { thinking } => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str("<thinking>\n");
                out.push_str(thinking);
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
) -> Html {
    let blocks = msg
        .message
        .as_ref()
        .and_then(|m| m.content.as_ref())
        .cloned()
        .unwrap_or_default();

    let usage = msg.message.as_ref().and_then(|m| m.usage.as_ref());
    let model = msg
        .message
        .as_ref()
        .and_then(|m| m.model.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("");
    let stop_reason = msg.message.as_ref().and_then(|m| m.stop_reason.as_deref());
    let is_truncated = stop_reason == Some("max_tokens");

    let model_tooltip = build_model_tooltip(model, usage);
    let usage_tooltip = build_usage_tooltip(usage);
    let copy_text = content_blocks_to_text(&blocks);

    html! {
        <div class="claude-message assistant-message">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge assistant">{ "Assistant" }</span>
                {
                    if let Some(short_name) = shorten_model_name(model) {
                        html! { <span class="model-name" title={model_tooltip}>{ short_name }</span> }
                    } else {
                        html! {}
                    }
                }
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
                                <span class="token-count">{ format!("{}↓ {}↑", u.input_tokens.unwrap_or(0), u.output_tokens.unwrap_or(0)) }</span>
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">{ render_assistant_message_content(msg) }</div>
            if let Some(iso) = raw_iso {
                <div class="message-footer">
                    <TimeAgo iso={iso.to_string()} />
                </div>
            }
        </div>
    }
}

pub fn render_assistant_message_content(msg: &AssistantMessage) -> Html {
    let blocks = msg
        .message
        .as_ref()
        .and_then(|m| m.content.as_ref())
        .cloned()
        .unwrap_or_default();
    render_content_blocks(&blocks)
}

pub fn render_content_blocks(blocks: &[ContentBlock]) -> Html {
    html! {
        <>
            {
                blocks.iter().map(|block| {
                    match block {
                        ContentBlock::Text { text, citations } => {
                            html! {
                                <div class="assistant-text">
                                    { render_markdown(text) }
                                    { render_citations(citations) }
                                </div>
                            }
                        }
                        ContentBlock::ToolUse { id: _, name, input } => {
                            render_tool_use(name, input)
                        }
                        ContentBlock::ToolResult { tool_use_id: _, content, is_error } => {
                            let class = if *is_error { "tool-result error" } else { "tool-result" };
                            match content {
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
                        ContentBlock::Image { source } => {
                            render_image_source(source, None)
                        }
                        ContentBlock::Thinking { thinking } => {
                            html! {
                                <div class="thinking-block">
                                    <span class="thinking-label">{ "thinking" }</span>
                                    <div class="thinking-content">{ crate::components::markdown::linkify_urls(thinking) }</div>
                                </div>
                            }
                        }
                        ContentBlock::ServerToolUse { id: _, name, input } => {
                            render_server_tool_use(name, input)
                        }
                        ContentBlock::WebSearchToolResult { tool_use_id: _, content } => {
                            render_web_search_result(content)
                        }
                        ContentBlock::CodeExecutionToolResult { tool_use_id: _, content } => {
                            render_code_execution_result(content)
                        }
                        ContentBlock::McpToolUse { id: _, name, server_name, input } => {
                            render_mcp_tool_use(name, server_name.as_deref(), input)
                        }
                        ContentBlock::McpToolResult { tool_use_id: _, content, is_error } => {
                            render_mcp_tool_result(content, is_error.unwrap_or(false))
                        }
                        ContentBlock::ContainerUpload { data } => {
                            render_container_upload(data)
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
