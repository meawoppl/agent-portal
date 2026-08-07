//! Error-family renderers: Anthropic error frames, the overload notice, rate
//! limit events, and the API-error card lifted out of a failed result body.

use crate::components::copy_button::CopyButton;
use serde::Deserialize;
use yew::prelude::*;

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
                <CopyButton text={message.to_string()} title="Copy error" />
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
                    if using_overage.unwrap_or(false) {
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

pub(super) fn try_render_api_error(result_text: Option<&str>) -> Option<Html> {
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
