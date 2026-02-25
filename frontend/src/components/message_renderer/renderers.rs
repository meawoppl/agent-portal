//! Rendering functions for each message type.

use super::types::*;
use super::{format_duration, shorten_model_name, truncate_str};
use crate::components::markdown::render_markdown;
use crate::components::tool_renderers::render_tool_use;
use serde::Deserialize;
use serde_json::Value;
use shared::ToolResultContent;
use wasm_bindgen::JsCast;
use yew::prelude::*;

// --- Message renderers ---

pub fn render_assistant_group(messages: &[String]) -> Html {
    let mut all_blocks: Vec<ContentBlock> = Vec::new();
    let mut total_output_tokens: u64 = 0;
    let mut total_input_tokens: u64 = 0;
    let mut total_cache_read: u64 = 0;
    let mut total_cache_created: u64 = 0;
    let mut model_name = String::new();

    for json in messages {
        match serde_json::from_str::<ClaudeMessage>(json) {
            Ok(ClaudeMessage::Assistant(msg)) => {
                if let Some(message) = &msg.message {
                    if let Some(blocks) = &message.content {
                        all_blocks.extend(blocks.clone());
                    }
                    if let Some(usage) = &message.usage {
                        total_output_tokens += usage.output_tokens.unwrap_or(0);
                        total_input_tokens += usage.input_tokens.unwrap_or(0);
                        total_cache_read += usage.cache_read_input_tokens.unwrap_or(0);
                        total_cache_created += usage.cache_creation_input_tokens.unwrap_or(0);
                    }
                    if model_name.is_empty() {
                        if let Some(m) = &message.model {
                            model_name = m.clone();
                        }
                    }
                }
            }
            Ok(ClaudeMessage::User(msg)) => {
                if let Some(message) = &msg.message {
                    if let Some(blocks) = &message.content {
                        all_blocks.extend(blocks.clone());
                    }
                }
            }
            _ => {}
        }
    }

    let count = messages.len();
    let usage_tooltip = format!(
        "Input: {} | Output: {} | Cache read: {} | Cache created: {} | {} messages",
        total_input_tokens, total_output_tokens, total_cache_read, total_cache_created, count
    );

    html! {
        <div class="claude-message assistant-message">
            <div class="message-header">
                <span class="message-type-badge assistant">{ "Assistant" }</span>
                {
                    if count > 1 {
                        html! { <span class="message-count" title={format!("{} consecutive messages", count)}>{ format!("{} messages", count) }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if let Some(short_name) = shorten_model_name(&model_name) {
                        html! { <span class="model-name" title={model_name.clone()}>{ short_name }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if total_input_tokens > 0 || total_output_tokens > 0 {
                        html! {
                            <span class="usage-badge" title={usage_tooltip}>
                                <span class="token-count">{ format!("{}↓ {}↑", total_input_tokens, total_output_tokens) }</span>
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">
                { render_content_blocks(&all_blocks) }
            </div>
        </div>
    }
}

pub fn render_user_message(msg: &UserMessage) -> Html {
    if let Some(text) = &msg.content {
        html! {
            <div class="claude-message user-message">
                <div class="message-header">
                    <span class="message-type-badge user">{ "You" }</span>
                </div>
                <div class="message-body">
                    <div class="user-text">{ render_markdown(text) }</div>
                </div>
            </div>
        }
    } else if let Some(message) = &msg.message {
        let blocks = message.content.as_ref().cloned().unwrap_or_default();

        let text_content: String = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let has_tool_results = blocks
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { .. }));

        if has_tool_results {
            html! {
                <div class="claude-message user-message tool-result-message">
                    <div class="message-body">
                        { render_content_blocks(&blocks) }
                    </div>
                </div>
            }
        } else if !text_content.is_empty() {
            html! {
                <div class="claude-message user-message">
                    <div class="message-header">
                        <span class="message-type-badge user">{ "You" }</span>
                    </div>
                    <div class="message-body">
                        <div class="user-text">{ render_markdown(&text_content) }</div>
                    </div>
                </div>
            }
        } else {
            html! {}
        }
    } else {
        html! {}
    }
}

pub fn render_error_message(msg: &ErrorMessage) -> Html {
    if msg.is_overload() {
        return render_overload_error(msg);
    }

    let message = msg.display_message();
    let error_type = msg.error_type();

    html! {
        <div class="claude-message error-message-display">
            <div class="message-header">
                <span class="message-type-badge result error">{ "Error" }</span>
                {
                    if let Some(err_type) = error_type {
                        html! { <span class="error-type">{ err_type }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">
                <div class="error-text">{ message }</div>
            </div>
        </div>
    }
}

pub fn render_portal_message(msg: &PortalMessage) -> Html {
    html! {
        <div class="claude-message portal-message">
            <div class="message-header">
                <span class="message-type-badge portal">{ "Portal" }</span>
            </div>
            <div class="message-body">
                { for msg.content.iter().map(render_portal_content) }
            </div>
        </div>
    }
}

fn render_portal_content(content: &shared::PortalContent) -> Html {
    match content {
        shared::PortalContent::Text { text } => render_markdown(text),
        shared::PortalContent::Image {
            media_type,
            data,
            file_path,
            file_size,
        } => {
            let source = ImageSource {
                source_type: "base64".to_string(),
                media_type: media_type.clone(),
                data: data.clone(),
            };
            let filename = file_path
                .as_deref()
                .and_then(|p| p.rsplit('/').next())
                .map(|s| s.to_string());
            html! {
                <>
                    { render_portal_image_header(file_path.as_deref(), *file_size) }
                    { render_image_source(&source, filename) }
                </>
            }
        }
    }
}

fn render_portal_image_header(file_path: Option<&str>, file_size: Option<u64>) -> Html {
    let Some(path) = file_path else {
        return html! {};
    };
    html! {
        <div class="tool-use-header">
            <span class="tool-icon">{ "\u{1f5bc}\u{fe0f}" }</span>
            <span class="read-file-path">{ path }</span>
            {
                if let Some(size) = file_size {
                    html! { <span class="tool-meta">{ format_file_size(size) }</span> }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn render_overload_error(msg: &ErrorMessage) -> Html {
    let request_id = msg.request_id.as_deref().unwrap_or("unknown");

    html! {
        <div class="claude-message overload-message">
            <div class="message-header">
                <span class="message-type-badge overload">{ "API Busy" }</span>
            </div>
            <div class="message-body">
                <div class="overload-content">
                    <div class="overload-icon">{ "⏳" }</div>
                    <div class="overload-text">
                        <div class="overload-title">{ "Claude API is temporarily overloaded" }</div>
                        <div class="overload-description">
                            { "The API is experiencing high demand. Claude Code will automatically retry the request. Please wait a moment." }
                        </div>
                    </div>
                </div>
                <div class="overload-details">
                    <span class="request-id" title="Request ID for debugging">{ format!("Request: {}", request_id) }</span>
                </div>
            </div>
        </div>
    }
}

pub fn render_rate_limit_event(msg: &RateLimitEventMessage) -> Html {
    let info = msg.rate_limit_info.as_ref();
    let status = info.and_then(|i| i.status.as_deref()).unwrap_or("unknown");
    let rate_type = info
        .and_then(|i| i.rate_limit_type.as_deref())
        .unwrap_or("unknown");
    let resets_at = info.and_then(|i| i.resets_at).unwrap_or(0);
    let using_overage = info.and_then(|i| i.is_using_overage).unwrap_or(false);
    let utilization = info.and_then(|i| i.utilization);

    let reset_text = if resets_at > 0 {
        let now = (js_sys::Date::now() / 1000.0) as u64;
        if resets_at > now {
            let mins = (resets_at - now) / 60;
            if mins > 60 {
                format!("Resets in {}h {}m", mins / 60, mins % 60)
            } else {
                format!("Resets in {}m", mins)
            }
        } else {
            "Reset".to_string()
        }
    } else {
        String::new()
    };

    let format_type = rate_type.replace('_', " ");

    html! {
        <div class="claude-message rate-limit-message">
            <div class="message-header">
                <span class="message-type-badge rate-limit">{ "Rate Limit" }</span>
            </div>
            <div class="message-body">
                <div class="overload-content">
                    <div class="overload-icon">{ "\u{23f1}\u{fe0f}" }</div>
                    <div class="overload-text">
                        <div class="overload-title">{ format!("Rate limit: {} ({})", status, format_type) }</div>
                        <div class="overload-description">
                            { &reset_text }
                            { if using_overage { " \u{b7} Using overage" } else { "" } }
                        </div>
                        {
                            if let Some(pct) = utilization {
                                let pct_int = (pct * 100.0).round() as u32;
                                let bar_class = if pct >= 0.9 {
                                    "utilization-bar critical"
                                } else if pct >= 0.7 {
                                    "utilization-bar warning"
                                } else {
                                    "utilization-bar"
                                };
                                html! {
                                    <div class="utilization-row">
                                        <div class={bar_class}>
                                            <div class="utilization-fill" style={format!("width: {}%", pct_int)}></div>
                                        </div>
                                        <span class="utilization-label">{ format!("{}%", pct_int) }</span>
                                    </div>
                                }
                            } else {
                                html! {}
                            }
                        }
                    </div>
                </div>
            </div>
        </div>
    }
}

pub fn render_system_message(msg: &SystemMessage) -> Html {
    let subtype = msg.subtype.as_deref().unwrap_or("system");

    // Check if this is a compaction-related message via subtype or status field
    let status_value = msg
        .extra
        .as_ref()
        .and_then(|v| v.get("status"))
        .and_then(|s| s.as_str())
        .unwrap_or("");

    if status_value == "compacting" {
        return render_compaction_beginning();
    }

    if subtype == "compact_boundary" {
        return render_compaction_completed(msg);
    }

    if subtype == "summary" || subtype == "compaction" || subtype == "context_compaction" {
        return render_compaction_completed(msg);
    }

    if subtype == "task_started" {
        return render_task_started(msg);
    }

    if subtype == "task_progress" {
        return html! {};
    }

    if subtype == "task_notification" {
        return render_task_notification(msg);
    }

    if subtype == "init" || subtype == "status" {
        return html! {};
    }

    html! {
        <div class="claude-message system-message compact">
            <span class="message-type-badge system">{ subtype }</span>
        </div>
    }
}

fn render_compaction_beginning() -> Html {
    html! {
        <div class="claude-message compaction-message compact">
            <div class="message-header">
                <span class="message-type-badge compaction">{ "Compaction Beginning" }</span>
            </div>
        </div>
    }
}

fn render_compaction_completed(msg: &SystemMessage) -> Html {
    let summary_text = msg.summary.as_deref().or_else(|| {
        msg.extra.as_ref().and_then(|v| {
            v.get("summary")
                .and_then(|s| s.as_str())
                .or_else(|| v.get("content").and_then(|s| s.as_str()))
                .or_else(|| v.get("text").and_then(|s| s.as_str()))
        })
    });

    let leaf_count = msg.leaf_message_count.or_else(|| {
        msg.extra.as_ref().and_then(|v| {
            v.get("leaf_message_count")
                .and_then(|n| n.as_u64())
                .map(|n| n as u32)
                .or_else(|| {
                    v.get("message_count")
                        .and_then(|n| n.as_u64())
                        .map(|n| n as u32)
                })
        })
    });

    let duration = msg.duration_ms.or_else(|| {
        msg.extra
            .as_ref()
            .and_then(|v| v.get("duration_ms").and_then(|n| n.as_u64()))
    });

    html! {
        <div class="claude-message compaction-message">
            <div class="message-header">
                <span class="message-type-badge compaction">{ "Compaction Completed" }</span>
                {
                    if let Some(count) = leaf_count {
                        html! {
                            <span class="compaction-stat" title="Messages summarized">
                                { format!("{} messages", count) }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
                {
                    if let Some(ms) = duration {
                        html! {
                            <span class="compaction-stat" title="Compaction duration">
                                { format_duration(ms) }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">
                <div class="compaction-content">
                    <div class="compaction-icon">{ "📦" }</div>
                    <div class="compaction-text">
                        {
                            if let Some(summary) = summary_text {
                                html! {
                                    <div class="compaction-summary">
                                        <div class="summary-label">{ "Summary:" }</div>
                                        <div class="summary-text">{ render_markdown(summary) }</div>
                                    </div>
                                }
                            } else {
                                html! {
                                    <div class="compaction-description">
                                        { "The conversation context has been summarized to free up space. Previous messages have been condensed while preserving important context." }
                                    </div>
                                }
                            }
                        }
                    </div>
                </div>
            </div>
        </div>
    }
}

fn render_task_started(msg: &SystemMessage) -> Html {
    let extra = msg.extra.as_ref();
    let description = extra
        .and_then(|v| v.get("description").and_then(|d| d.as_str()))
        .unwrap_or("Background task");
    let task_id = extra
        .and_then(|v| v.get("task_id").and_then(|t| t.as_str()))
        .unwrap_or("");

    let type_label = extra
        .and_then(|v| v.get("task_type"))
        .and_then(|v| serde_json::from_value::<shared::CCTaskType>(v.clone()).ok())
        .map(|tt| match tt {
            shared::CCTaskType::LocalAgent => "Sub-agent",
            shared::CCTaskType::LocalBash => "Background Bash",
        })
        .unwrap_or("Task");

    html! {
        <div class="claude-message task-message compact" title={format!("Task ID: {}", task_id)}>
            <div class="message-header">
                <span class="message-type-badge task">{ "Task Started" }</span>
                <span class="task-type-badge">{ type_label }</span>
                <span class="task-description-inline">{ description }</span>
            </div>
        </div>
    }
}

fn render_task_notification(msg: &SystemMessage) -> Html {
    let extra = msg.extra.as_ref();
    let typed_status = extra
        .and_then(|v| v.get("status"))
        .and_then(|v| serde_json::from_value::<shared::CCTaskStatus>(v.clone()).ok());
    let summary_text = msg
        .summary
        .as_deref()
        .or_else(|| extra.and_then(|v| v.get("summary").and_then(|s| s.as_str())));
    let task_id = extra
        .and_then(|v| v.get("task_id").and_then(|t| t.as_str()))
        .unwrap_or("");

    let typed_usage = extra
        .and_then(|v| v.get("usage"))
        .and_then(|v| serde_json::from_value::<shared::TaskUsage>(v.clone()).ok());
    let duration = typed_usage.as_ref().map(|u| u.duration_ms);
    let tool_uses = typed_usage.as_ref().map(|u| u.tool_uses);
    let total_tokens = typed_usage.as_ref().map(|u| u.total_tokens);

    let is_failed = matches!(typed_status, Some(shared::CCTaskStatus::Failed));
    let status_class = if is_failed { "failed" } else { "completed" };

    html! {
        <div class={classes!("claude-message", "task-message", status_class)}
             title={format!("Task ID: {}", task_id)}>
            <div class="message-header">
                <span class={classes!("message-type-badge", "task", status_class)}>
                    { if is_failed { "Task Failed" } else { "Task Completed" } }
                </span>
                {
                    if let Some(ms) = duration {
                        html! { <span class="task-stat">{ format_duration(ms) }</span> }
                    } else { html! {} }
                }
                {
                    if let Some(tools) = tool_uses {
                        html! { <span class="task-stat" title="Tool calls">{ format!("{} tools", tools) }</span> }
                    } else { html! {} }
                }
                {
                    if let Some(tokens) = total_tokens {
                        html! { <span class="task-stat" title="Total tokens">{ format!("{}k tokens", tokens / 1000) }</span> }
                    } else { html! {} }
                }
            </div>
            {
                if let Some(summary) = summary_text {
                    html! {
                        <div class="message-body">
                            <div class="task-summary">{ render_markdown(summary) }</div>
                        </div>
                    }
                } else { html! {} }
            }
        </div>
    }
}

pub fn render_assistant_message(msg: &AssistantMessage) -> Html {
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

    let usage_tooltip = usage
        .map(|u| {
            format!(
                "Input: {} | Output: {} | Cache read: {} | Cache created: {}",
                u.input_tokens.unwrap_or(0),
                u.output_tokens.unwrap_or(0),
                u.cache_read_input_tokens.unwrap_or(0),
                u.cache_creation_input_tokens.unwrap_or(0)
            )
        })
        .unwrap_or_default();

    html! {
        <div class="claude-message assistant-message">
            <div class="message-header">
                <span class="message-type-badge assistant">{ "Assistant" }</span>
                {
                    if let Some(short_name) = shorten_model_name(model) {
                        html! { <span class="model-name" title={model.to_string()}>{ short_name }</span> }
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
            <div class="message-body">
                { render_content_blocks(&blocks) }
            </div>
        </div>
    }
}

pub fn render_content_blocks(blocks: &[ContentBlock]) -> Html {
    html! {
        <>
            {
                blocks.iter().map(|block| {
                    match block {
                        ContentBlock::Text { text } => {
                            html! { <div class="assistant-text">{ render_markdown(text) }</div> }
                        }
                        ContentBlock::ToolUse { id: _, name, input } => {
                            render_tool_use(name, input)
                        }
                        ContentBlock::ToolResult { tool_use_id: _, content, is_error } => {
                            let class = if *is_error { "tool-result error" } else { "tool-result" };
                            match content {
                                Some(ToolResultContent::Text(s)) => {
                                    let display = if s.len() > 500 {
                                        format!("{}...", truncate_str(s, 500))
                                    } else {
                                        s.clone()
                                    };
                                    html! {
                                        <div class={class}>
                                            <pre class="tool-result-content">{ display }</pre>
                                        </div>
                                    }
                                }
                                Some(ToolResultContent::Structured(blocks)) => {
                                    html! {
                                        <div class={class}>
                                            { for blocks.iter().map(render_structured_block) }
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
                                    <div class="thinking-content">{ thinking }</div>
                                </div>
                            }
                        }
                        ContentBlock::Other => html! {},
                    }
                }).collect::<Html>()
            }
        </>
    }
}

const ALLOWED_IMAGE_MEDIA_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
];

/// 2 MB limit on base64 data we'll render as an inline image.
/// Base64 encodes at ~1.33x, so 2MB base64 ≈ 1.5MB raw image.
const MAX_IMAGE_BASE64_BYTES: usize = 2 * 1024 * 1024;

fn render_image_source(source: &ImageSource, filename: Option<String>) -> Html {
    if !ALLOWED_IMAGE_MEDIA_TYPES.contains(&source.media_type.as_str()) {
        return html! {
            <pre class="tool-result-content">
                { format!("[unsupported image type: {}]", source.media_type) }
            </pre>
        };
    }
    if source.data.len() > MAX_IMAGE_BASE64_BYTES {
        let size_mb = source.data.len() as f64 / (1024.0 * 1024.0);
        return html! {
            <pre class="tool-result-content">
                { format!("[image too large: {:.1} MB, limit is 2 MB]", size_mb) }
            </pre>
        };
    }
    let src = format!("data:{};base64,{}", source.media_type, source.data);
    html! {
        <ImageViewer src={src} media_type={source.media_type.clone()} {filename} />
    }
}

#[derive(Properties, PartialEq)]
struct ImageViewerProps {
    pub src: String,
    pub media_type: String,
    #[prop_or_default]
    pub filename: Option<String>,
}

#[function_component(ImageViewer)]
fn image_viewer(props: &ImageViewerProps) -> Html {
    let expanded = use_state(|| false);

    // Close lightbox on Escape key (capture phase so it doesn't trigger nav mode)
    {
        let expanded = expanded.clone();
        use_effect_with(*expanded, move |is_expanded| {
            let listener = if *is_expanded {
                let expanded = expanded.clone();
                let options = gloo::events::EventListenerOptions {
                    phase: gloo::events::EventListenerPhase::Capture,
                    passive: false,
                };
                Some(gloo::events::EventListener::new_with_options(
                    &gloo::utils::document(),
                    "keydown",
                    options,
                    move |event| {
                        if let Some(ke) = event.dyn_ref::<web_sys::KeyboardEvent>() {
                            if ke.key() == "Escape" {
                                ke.prevent_default();
                                ke.stop_propagation();
                                expanded.set(false);
                            }
                        }
                    },
                ))
            } else {
                None
            };
            move || drop(listener)
        });
    }

    let on_thumb_click = {
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| expanded.set(true))
    };

    let on_close = {
        let expanded = expanded.clone();
        Callback::from(move |_: MouseEvent| expanded.set(false))
    };

    let ext = match props.media_type.as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        _ => "bin",
    };

    let download_name = props
        .filename
        .clone()
        .unwrap_or_else(|| format!("image.{ext}"));

    html! {
        <>
            <div class="tool-result-image" onclick={on_thumb_click}>
                <img src={props.src.clone()} alt="Tool result image" />
            </div>
            if *expanded {
                <div class="image-lightbox" onclick={on_close.clone()}>
                    <div class="image-lightbox-content" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                        <img src={props.src.clone()} alt="Full size image" />
                        <div class="image-lightbox-controls">
                            <a
                                class="image-lightbox-download"
                                href={props.src.clone()}
                                download={download_name}
                            >
                                { "Download" }
                            </a>
                            <button class="image-lightbox-close" onclick={on_close}>
                                { "\u{00d7}" }
                            </button>
                        </div>
                    </div>
                </div>
            }
        </>
    }
}

fn render_structured_block(block: &Value) -> Html {
    let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match block_type {
        "image" => {
            html! { <span class="tool-result-image-tag">{ "[image]" }</span> }
        }
        "text" => {
            let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let display = if text.len() > 500 {
                format!("{}...", truncate_str(text, 500))
            } else {
                text.to_string()
            };
            html! { <pre class="tool-result-content">{ display }</pre> }
        }
        _ => {
            let json = serde_json::to_string_pretty(block).unwrap_or_default();
            html! { <pre class="tool-result-content">{ json }</pre> }
        }
    }
}

pub fn render_result_message(msg: &ResultMessage) -> Html {
    let is_error = msg.is_error.unwrap_or(false);
    let status_class = if is_error { "error" } else { "success" };

    let duration_ms = msg.duration_ms.unwrap_or(0);
    let api_ms = msg.duration_api_ms.unwrap_or(0);
    let turns = msg.num_turns.unwrap_or(0);

    let timing_tooltip = format!(
        "Total: {}ms | API: {}ms | Turns: {}",
        duration_ms, api_ms, turns
    );

    if is_error {
        if let Some(error_html) = try_render_api_error(msg.result.as_deref()) {
            return html! {
                <>
                    { error_html }
                    <div class={classes!("claude-message", "result-message", status_class)}>
                        <div class="result-stats-bar">
                            <span class={classes!("result-status", status_class)}>{ "✗" }</span>
                            <span class="stat-item duration" title={timing_tooltip.clone()}>
                                { format_duration(duration_ms) }
                            </span>
                        </div>
                    </div>
                </>
            };
        }
    }

    html! {
        <div class={classes!("claude-message", "result-message", status_class)}>
            <div class="result-stats-bar">
                <span class={classes!("result-status", status_class)}>
                    { if is_error { "✗" } else { "✓" } }
                </span>
                <span class="stat-item duration" title={timing_tooltip.clone()}>
                    { format_duration(duration_ms) }
                </span>
                {
                    if let Some(usage) = &msg.usage {
                        html! {
                            <>
                                <span class="stat-item tokens" title="Input tokens">
                                    { format!("{}↓", usage.input_tokens.unwrap_or(0)) }
                                </span>
                                <span class="stat-item tokens" title="Output tokens">
                                    { format!("{}↑", usage.output_tokens.unwrap_or(0)) }
                                </span>
                            </>
                        }
                    } else {
                        html! {}
                    }
                }
                {
                    if turns > 1 {
                        html! {
                            <span class="stat-item turns" title="API turns">
                                { format!("{} turns", turns) }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
        </div>
    }
}

// --- API error rendering ---

#[derive(Debug, Deserialize)]
struct AnthropicApiError {
    #[serde(rename = "type")]
    error_type: Option<String>,
    error: Option<AnthropicErrorDetails>,
    request_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnthropicErrorDetails {
    #[serde(rename = "type")]
    error_type: Option<String>,
    message: Option<String>,
}

fn try_render_api_error(result_text: Option<&str>) -> Option<Html> {
    let text = result_text?;

    let json_start = text.find('{')?;
    let json_str = &text[json_start..];

    let api_error: AnthropicApiError = serde_json::from_str(json_str).ok()?;

    if api_error.error_type.as_deref() != Some("error") {
        return None;
    }

    let error_details = api_error.error.as_ref();
    let error_type = error_details
        .and_then(|e| e.error_type.as_deref())
        .unwrap_or("unknown_error");
    let error_message = error_details
        .and_then(|e| e.message.as_deref())
        .unwrap_or("An error occurred");
    let request_id = api_error.request_id.as_deref();

    let http_status = if text.starts_with("API Error:") {
        text.split_whitespace()
            .nth(2)
            .and_then(|s| s.parse::<u16>().ok())
    } else {
        None
    };

    let display_type = format_error_type(error_type);

    Some(html! {
        <div class="claude-message anthropic-error-message">
            <div class="message-header">
                <span class="message-type-badge anthropic-error">{ "Anthropic API Error" }</span>
                {
                    if let Some(status) = http_status {
                        html! { <span class="http-status">{ format!("HTTP {}", status) }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="message-body">
                <div class="anthropic-error-content">
                    <div class="error-icon">{ "⚠" }</div>
                    <div class="error-details">
                        <div class="error-type-display">{ display_type }</div>
                        <div class="error-message-text">{ error_message }</div>
                    </div>
                </div>
                {
                    if let Some(req_id) = request_id {
                        html! {
                            <div class="error-request-id">
                                <span class="request-id-label">{ "Request ID: " }</span>
                                <code class="request-id-value">{ req_id }</code>
                            </div>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
        </div>
    })
}

fn format_error_type(error_type: &str) -> String {
    match error_type {
        "api_error" => "Internal Server Error".to_string(),
        "authentication_error" => "Authentication Failed".to_string(),
        "invalid_request_error" => "Invalid Request".to_string(),
        "not_found_error" => "Not Found".to_string(),
        "overloaded_error" => "API Overloaded".to_string(),
        "permission_error" => "Permission Denied".to_string(),
        "rate_limit_error" => "Rate Limited".to_string(),
        "request_too_large" => "Request Too Large".to_string(),
        other => other.replace('_', " ").to_string(),
    }
}
