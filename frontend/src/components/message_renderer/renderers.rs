//! Rendering functions for each message type.

mod assistant;
mod portal;
mod system;
mod tools;

use super::types::*;
use super::{format_duration, shorten_model_name};
use crate::components::copy_button::CopyButton;
use crate::components::markdown::render_markdown;
use serde::Deserialize;
use wasm_bindgen::JsCast;
use yew::prelude::*;

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

// --- Message renderers ---

pub fn render_user_message(
    msg: &UserMessage,
    current_user_id: Option<&str>,
    timestamp: Option<&str>,
) -> Html {
    let label = match &msg.sender {
        Some(sender) if current_user_id != Some(sender.user_id.as_str()) => sender.name.clone(),
        _ => "You".to_string(),
    };
    let pending_class = if msg.pending { " pending" } else { "" };

    if let Some(text) = &msg.content {
        html! {
            <div class={format!("claude-message user-message{}", pending_class)}>
                <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                    <span class="message-type-badge user">{ &label }</span>
                    if msg.pending {
                        <span class="pending-indicator" title="Sending...">{ "\u{2022}" }</span>
                    }
                    <CopyButton text={text.clone()} title="Copy message" />
                </div>
                <div class="message-body">{ render_user_message_content(msg) }</div>
            </div>
        }
    } else if let Some(message) = &msg.message {
        let blocks = message.content.as_ref().cloned().unwrap_or_default();

        let text_content: String = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.clone()),
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
                    <div class="message-body">{ render_user_message_content(msg) }</div>
                </div>
            }
        } else if !text_content.is_empty() {
            html! {
                <div class="claude-message user-message">
                    <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
                        <span class="message-type-badge user">{ &label }</span>
                        <CopyButton text={text_content.clone()} title="Copy message" />
                    </div>
                    <div class="message-body">{ render_user_message_content(msg) }</div>
                </div>
            }
        } else {
            html! {}
        }
    } else {
        html! {}
    }
}

pub fn render_user_message_content(msg: &UserMessage) -> Html {
    if let Some(text) = &msg.content {
        return html! {
            <div class="user-text">{ render_markdown(&preserve_user_newlines(text)) }</div>
        };
    }

    let blocks = msg
        .message
        .as_ref()
        .and_then(|m| m.content.as_ref())
        .cloned()
        .unwrap_or_default();
    let has_tool_results = blocks
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolResult { .. }));

    if has_tool_results {
        render_content_blocks(&blocks)
    } else {
        let text_content = blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        if text_content.is_empty() {
            html! {}
        } else {
            html! {
                <div class="user-text">{ render_markdown(&preserve_user_newlines(&text_content)) }</div>
            }
        }
    }
}

pub fn render_error_message(msg: &ErrorMessage, timestamp: Option<&str>) -> Html {
    if msg.is_overload() {
        return render_overload_error(msg, timestamp);
    }

    let message = msg.display_message();
    let error_type = msg.error_type();

    html! {
        <div class="claude-message error-message-display">
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
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
                <div class="error-text">{ crate::components::markdown::linkify_urls(message) }</div>
            </div>
        </div>
    }
}

fn render_overload_error(msg: &ErrorMessage, timestamp: Option<&str>) -> Html {
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

pub fn render_rate_limit_event(msg: &RateLimitEventMessage, timestamp: Option<&str>) -> Html {
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
            <div class="message-header" title={timestamp.unwrap_or_default().to_string()}>
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

const ALLOWED_IMAGE_MEDIA_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
];

pub(super) fn render_image_source(source: &ImageSource, filename: Option<String>) -> Html {
    if !ALLOWED_IMAGE_MEDIA_TYPES.contains(&source.media_type.as_str()) {
        return html! {
            <pre class="tool-result-content">
                { format!("[unsupported image type: {}]", source.media_type) }
            </pre>
        };
    }
    // Support both URL sources (from backend image store) and base64 data URIs
    let src = if source.source_type == "url" {
        source.data.clone()
    } else {
        format!("data:{};base64,{}", source.media_type, source.data)
    };
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

pub fn render_result_message(
    msg: &ResultMessage,
    turn_metrics: Option<&shared::TurnMetrics>,
) -> Html {
    let is_error = msg.is_error.unwrap_or(false);
    let status_class = if is_error { "error" } else { "success" };

    let duration_ms = msg.duration_ms.unwrap_or(0);
    let api_ms = msg.duration_api_ms.unwrap_or(0);
    let turns = msg.num_turns.unwrap_or(0);

    let mut timing_tooltip = format!(
        "Total: {}ms | API: {}ms | Turns: {}",
        duration_ms, api_ms, turns
    );

    if let Some(model_usage) = &msg.model_usage {
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
            .filter_map(|v| {
                v.get("tool_name")
                    .and_then(|t| t.as_str())
                    .or_else(|| v.as_str())
                    .map(|s| s.to_string())
            })
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        String::new()
    };

    let extra_badges = html! {
        <>
            {
                if let Some(cost) = msg.total_cost_usd {
                    html! {
                        <span class="stat-item cost" title="Total cost">
                            { format!("${:.2}", cost) }
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
    let metrics_footer = match turn_metrics {
        Some(m) => super::turn_metrics_footer::render_turn_metrics_footer(m),
        None => html! {},
    };

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
