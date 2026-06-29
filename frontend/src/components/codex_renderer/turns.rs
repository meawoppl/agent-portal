use super::events::{CodexError, CodexUsage};
use shared::fmt::format_duration;
use yew::prelude::*;

pub(super) fn render_turn_completed(
    usage: Option<&CodexUsage>,
    duration_ms: Option<u64>,
    turn_id: Option<&str>,
    status: Option<&str>,
    turn_metrics: Option<&shared::TurnMetrics>,
) -> Html {
    let input = usage.map(CodexUsage::input_tokens).unwrap_or(0);
    let output = usage.map(CodexUsage::output_tokens).unwrap_or(0);
    let cached = usage.map(CodexUsage::cached_input_tokens).unwrap_or(0);
    let reasoning = usage.map(CodexUsage::reasoning_output_tokens).unwrap_or(0);
    let total = usage.map(CodexUsage::total_tokens).unwrap_or(0);
    let thread_total = usage.and_then(CodexUsage::thread_total_tokens);
    let context_window = usage.and_then(|u| u.model_context_window);

    let mut tooltip = format!(
        "Input: {} | Output: {} | Cached: {} | Reasoning: {} | Total: {}",
        input, output, cached, reasoning, total
    );
    if let Some(thread_total) = thread_total {
        tooltip.push_str(&format!(" | Thread total: {}", thread_total));
    }
    if let Some(context_window) = context_window {
        tooltip.push_str(&format!(" | Context window: {}", context_window));
    }
    let status_title = turn_id.unwrap_or("Codex turn").to_string();

    // Per-turn metrics footer (PR 2 of N). Same shape as the Claude
    // `result-message` footer — see
    // `crate::components::message_renderer::turn_metrics_footer`. For Codex,
    // the cost chip universally drops (no per-turn cost on the wire today)
    // and the cache-hit-% chip universally drops (no cache breakdown on the
    // wire today); the chips that do render are tok/s, TTFT, tokens-in/out
    // (and max gap when > 1s).
    let metrics_footer =
        crate::components::message_renderer::turn_metrics_footer::render_turn_metrics_footer(
            turn_metrics,
        );

    html! {
        <div class="claude-message result-message success">
            <div class="result-stats-bar">
                <span class="result-status success">{ "\u{2713}" }</span>
                <span class="result-done-label success">{ "completed" }</span>
                {
                    if let Some(ms) = duration_ms {
                        html! {
                            <span class="stat-item duration" title="Turn duration">
                                { format_duration(ms) }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
                {
                    if input > 0 || output > 0 || cached > 0 || reasoning > 0 {
                        html! {
                            <>
                                <span class="stat-item tokens" title={tooltip}>
                                    { format!("{}\u{2193} {}\u{2191}", input, output) }
                                </span>
                                if cached > 0 {
                                    <span class="stat-item tokens" title="Cached input tokens">
                                        { format!("{} cached", cached) }
                                    </span>
                                }
                                if reasoning > 0 {
                                    <span class="stat-item tokens" title="Reasoning output tokens">
                                        { format!("{} reasoning", reasoning) }
                                    </span>
                                }
                            </>
                        }
                    } else {
                        html! {}
                    }
                }
                {
                    if let (Some(thread_total), Some(context_window)) = (thread_total, context_window) {
                        html! {
                            <span class="stat-item turns" title="Thread tokens / model context window">
                                { format!("{} / {} ctx", thread_total, context_window) }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
                {
                    // The `✓ completed` label already conveys a normal finish;
                    // only surface the raw status when it says something else
                    // (a non-"completed" stop reason), so it isn't shown twice.
                    if let Some(status) = status.filter(|s| !s.eq_ignore_ascii_case("completed")) {
                        html! {
                            <span class="stat-item stop-reason" title={status_title.clone()}>
                                { status }
                            </span>
                        }
                    } else {
                        html! {}
                    }
                }
            </div>
            { metrics_footer }
        </div>
    }
}

pub(super) fn render_turn_failed(
    error: Option<&CodexError>,
    turn_metrics: Option<&shared::TurnMetrics>,
) -> Html {
    let message = error
        .and_then(|e| e.message.as_deref())
        .unwrap_or("Turn failed");

    // Even a failed turn carries useful metrics (TTFT before the failure,
    // tokens consumed, stream_restarts on retried-and-still-failed rate
    // limits, etc.) — render the footer if we have a row for it.
    let metrics_footer =
        crate::components::message_renderer::turn_metrics_footer::render_turn_metrics_footer(
            turn_metrics,
        );

    html! {
        <div class="claude-message error-message-display">
            <div class="message-header">
                <span class="message-type-badge result error">{ "Turn Failed" }</span>
            </div>
            <div class="message-body">
                <div class="error-text">{ message }</div>
            </div>
            { metrics_footer }
        </div>
    }
}
