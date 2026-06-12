//! Rendering functions for each message type.

mod assistant;
mod portal;
mod system;
mod tools;

use super::types::{OptimisticUserMessage, UserMessageMeta};
use super::{format_duration, shorten_model_name};
use crate::components::copy_button::CopyButton;
use crate::components::markdown::render_markdown_for_session;
use crate::components::tool_renderers::{
    has_askuserquestion_answers, render_askuserquestion_result,
};
use crate::hooks::use_escape_capture;
use serde::Deserialize;
use uuid::Uuid;
use yew::prelude::*;

pub(crate) use assistant::assistant_label;
pub use assistant::{
    render_assistant_message, render_assistant_message_content, render_content_blocks,
};
pub use portal::{render_portal_message, render_portal_message_content};
pub use system::render_system_message;

/// Convert single newlines to markdown hard breaks (trailing two spaces)
/// so that user-typed line breaks are preserved when rendered as markdown.
fn preserve_user_newlines(text: &str) -> String {
    text.replace('\n', "  \n")
}

/// Extract the joined text content and tool-result presence from a user
/// message's content blocks. Shared by [`render_user_message`] and
/// [`render_user_message_content`].
fn user_message_text_and_tool_results(msg: &shared::UserMessage) -> (String, bool) {
    let blocks = &msg.message.content;

    let text_content: String = blocks
        .iter()
        .filter_map(|block| match block {
            shared::ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let has_tool_results = blocks
        .iter()
        .any(|b| matches!(b, shared::ContentBlock::ToolResult(_)));

    (text_content, has_tool_results)
}

// --- Message renderers ---

pub fn render_optimistic_user_message(
    msg: &OptimisticUserMessage,
    current_user_id: Option<&str>,
    timestamp: Option<&str>,
    session_id: Uuid,
) -> Html {
    let label = match &msg.sender {
        Some(sender) if current_user_id != Some(sender.user_id.as_str()) => sender.name.clone(),
        _ => "You".to_string(),
    };
    let pending_class = if msg.pending { " pending" } else { "" };

    html! {
        <div class={format!("claude-message user-message{}", pending_class)}>
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge user">{ &label }</span>
                if msg.pending {
                    <span class="pending-indicator" title="Sending...">{ "\u{2022}" }</span>
                }
                <CopyButton text={msg.content.clone()} title="Copy message" />
            </div>
            <div class="message-body">{ render_optimistic_user_message_content(msg, session_id) }</div>
        </div>
    }
}

pub fn render_user_message(
    msg: &shared::UserMessage,
    meta: &UserMessageMeta,
    current_user_id: Option<&str>,
    timestamp: Option<&str>,
    session_id: Uuid,
) -> Html {
    let label = match &meta.sender {
        Some(sender) if current_user_id != Some(sender.user_id.as_str()) => sender.name.clone(),
        _ => "You".to_string(),
    };
    let pending_class = if meta.pending { " pending" } else { "" };
    let (text_content, has_tool_results) = user_message_text_and_tool_results(msg);

    if has_tool_results {
        html! {
            <div class="claude-message user-message tool-result-message">
                <div class="message-body">{ render_user_message_content(msg, session_id) }</div>
            </div>
        }
    } else if !text_content.is_empty() {
        html! {
            <div class={format!("claude-message user-message{}", pending_class)}>
                <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                    <span class="message-type-badge user">{ &label }</span>
                    if meta.pending {
                        <span class="pending-indicator" title="Sending...">{ "\u{2022}" }</span>
                    }
                    <CopyButton text={text_content.clone()} title="Copy message" />
                </div>
                <div class="message-body">{ render_user_message_content(msg, session_id) }</div>
            </div>
        }
    } else {
        html! {}
    }
}

pub fn render_optimistic_user_message_content(
    msg: &OptimisticUserMessage,
    session_id: Uuid,
) -> Html {
    html! {
        <div class="user-text">{ render_markdown_for_session(&preserve_user_newlines(&msg.content), session_id) }</div>
    }
}

pub fn render_user_message_content(msg: &shared::UserMessage, session_id: Uuid) -> Html {
    if let Some(Ok(input)) = msg.tool_use_result_as::<shared::AskUserQuestionInput>() {
        if has_askuserquestion_answers(&input) {
            return render_askuserquestion_result(&input);
        }
    }

    let (text_content, has_tool_results) = user_message_text_and_tool_results(msg);

    if has_tool_results {
        render_content_blocks(&msg.message.content, session_id)
    } else if text_content.is_empty() {
        html! {}
    } else {
        html! {
            <div class="user-text">{ render_markdown_for_session(&preserve_user_newlines(&text_content), session_id) }</div>
        }
    }
}

pub fn render_error_message(msg: &shared::AnthropicError, timestamp: Option<&str>) -> Html {
    if msg.is_overloaded() {
        return render_overload_error(msg, timestamp);
    }

    let message = msg.error.message.as_str();
    let error_type = msg.error.error_type.as_str();

    html! {
        <div class="claude-message error-message-display">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge result error">{ "Error" }</span>
                {
                    html! { <span class="error-type">{ error_type }</span> }
                }
            </div>
            <div class="message-body">
                <div class="error-text">{ crate::components::markdown::linkify_urls(message) }</div>
            </div>
        </div>
    }
}

fn render_overload_error(msg: &shared::AnthropicError, timestamp: Option<&str>) -> Html {
    let request_id = msg.request_id.as_deref().unwrap_or("unknown");

    html! {
        <div class="claude-message overload-message">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
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

pub fn render_rate_limit_event(msg: &shared::RateLimitEvent, timestamp: Option<&str>) -> Html {
    let info = &msg.rate_limit_info;
    let status = info.status.as_str();
    let rate_type = info
        .rate_limit_type
        .as_ref()
        .map(|t| t.as_str())
        .unwrap_or("unknown");
    let resets_at = info.resets_at.unwrap_or(0);
    let using_overage = info.is_using_overage;
    let utilization = info.utilization;

    let reset_text = if resets_at > 0 {
        let now = (js_sys::Date::now() / 1000.0) as u64;
        if resets_at > now {
            let mins = (resets_at - now) / 60;
            if mins > 60 {
                Some(format!("resets in {}h {}m", mins / 60, mins % 60))
            } else {
                Some(format!("resets in {}m", mins))
            }
        } else {
            Some("reset".to_string())
        }
    } else {
        None
    };

    let format_type = rate_type.replace('_', " ");
    let utilization_text = utilization.map(|pct| format!("{}%", (pct * 100.0).round() as u32));

    html! {
        <div class="claude-message rate-limit-message">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                <span class="message-type-badge rate-limit">{ "Rate Limit" }</span>
                <span class="rate-limit-inline">
                    <span class="rate-limit-status">{ status }</span>
                    <span class="rate-limit-detail">{ format_type }</span>
                    if let Some(text) = reset_text {
                        <span class="rate-limit-detail">{ text }</span>
                    }
                    if using_overage {
                        <span class="rate-limit-detail">{ "using overage" }</span>
                    }
                    if let Some(text) = utilization_text {
                        <span class="rate-limit-detail">{ text }</span>
                    }
                </span>
            </div>
        </div>
    }
}

const ALLOWED_IMAGE_MEDIA_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
];

pub(super) fn render_image_source(source: &shared::ImageSource, filename: Option<String>) -> Html {
    if !ALLOWED_IMAGE_MEDIA_TYPES.contains(&source.media_type.as_str()) {
        return html! {
            <pre class="tool-result-content">
                { format!("[unsupported image type: {}]", source.media_type) }
            </pre>
        };
    }
    // Support both URL sources (from backend image store) and base64 data URIs
    let src = if source.source_type.as_str() == "url" {
        source.data.clone()
    } else {
        format!("data:{};base64,{}", source.media_type, source.data)
    };
    html! {
        <ImageViewer src={src} media_type={source.media_type.as_str().to_string()} {filename} />
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
        use_escape_capture(*expanded, Callback::from(move |()| expanded.set(false)));
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

pub fn render_result_message(
    msg: &shared::ResultMessage,
    turn_metrics: Option<&shared::TurnMetrics>,
) -> Html {
    let is_error = msg.is_error;
    let status_class = if is_error { "error" } else { "success" };

    let duration_ms = msg.duration_ms;
    let api_ms = msg.duration_api_ms;
    let turns = msg.num_turns;

    let mut timing_tooltip = format!(
        "Total: {}ms | API: {}ms | Turns: {}",
        duration_ms, api_ms, turns
    );

    if let Some(model_usage) = msg.model_usage.as_ref() {
        for (model, entry) in model_usage {
            timing_tooltip.push_str(&format!(
                " | {}: ${:.4}",
                shorten_model_name(model).unwrap_or_else(|| model.clone()),
                entry.cost_usd
            ));
        }
    }

    let errors_tooltip = if !msg.errors.is_empty() {
        msg.errors.join("\n")
    } else {
        String::new()
    };

    let denials_tooltip = if !msg.permission_denials.is_empty() {
        msg.permission_denials
            .iter()
            .map(|v| v.tool_name.clone())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        String::new()
    };

    let extra_badges = html! {
        <>
            {
                    if msg.total_cost_usd > 0.0 {
                    html! {
                        <span class="stat-item cost" title="Total cost">
                            { format!("${:.2}", msg.total_cost_usd) }
                        </span>
                    }
                } else {
                    html! {}
                }
            }
            {
                if msg.stop_reason.as_deref() == Some("max_tokens") {
                    html! {
                        <span class="stat-item stop-reason" title="Session stopped: max tokens reached">
                            { "max tokens" }
                        </span>
                    }
                } else {
                    html! {}
                }
            }
            {
                if msg.fast_mode_state.as_deref() == Some("on") {
                    html! {
                        <span class="stat-item fast-mode" title="Fast mode enabled">
                            { "Fast" }
                        </span>
                    }
                } else {
                    html! {}
                }
            }
            {
                if !msg.errors.is_empty() {
                    html! {
                        <span class="stat-item errors" title={errors_tooltip.clone()}>
                            { format!("{} error{}", msg.errors.len(), if msg.errors.len() == 1 { "" } else { "s" }) }
                        </span>
                    }
                } else {
                    html! {}
                }
            }
            {
                if !msg.permission_denials.is_empty() {
                    html! {
                        <span class="stat-item denials" title={denials_tooltip.clone()}>
                            { format!("{} denied", msg.permission_denials.len()) }
                        </span>
                    }
                } else {
                    html! {}
                }
            }
        </>
    };

    // Per-turn metrics footer (PR 2 of N) — sits directly below the result
    // stats bar. `None` for sessions on the live path before the first
    // metrics frame arrives, for pre-PR-1 historical rows, and during the
    // brief window between a turn's terminator landing and the metrics
    // broadcast for that turn (the wire order is "Result frame first,
    // metrics broadcast second"). Renders nothing in those cases — the
    // chip strip lights up retroactively on the next render.
    let metrics_footer = super::turn_metrics_footer::render_turn_metrics_footer(turn_metrics);

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
                            { extra_badges.clone() }
                        </div>
                        { metrics_footer.clone() }
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
                                    { format!("{}↓", usage.input_tokens) }
                                </span>
                                <span class="stat-item tokens" title="Output tokens">
                                    { format!("{}↑", usage.output_tokens) }
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
                { extra_badges }
            </div>
            { metrics_footer }
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
                        <div class="error-message-text">{ crate::components::markdown::linkify_urls(error_message) }</div>
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
