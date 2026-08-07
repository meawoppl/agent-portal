//! Turn-terminator renderer: the Claude `result` frame's stats bar, its
//! per-turn metrics footer, and the fast-mode labelling shared with the
//! system init bar.

use super::super::shorten_model_name;
use super::errors::try_render_api_error;
use shared::fmt::format_duration;
use yew::prelude::*;

/// A human-readable phrase for why fast mode couldn't serve (#1475), shown in
/// the "Fast off" tooltip. Shared by the result footer and the init bar so the
/// two never drift. Unknown/novel reasons fall back to the raw code so nothing
/// is silently swallowed.
pub(super) fn fast_mode_disabled_label(reason: &shared::FastModeDisabledReason) -> String {
    use shared::FastModeDisabledReason as R;
    match reason {
        R::Free => "requires a paid plan",
        R::Preference => "turned off in preferences",
        R::ExtraUsageDisabled => "extra usage disabled",
        R::NetworkError => "network error",
        R::NotFirstParty => "unavailable for this provider",
        R::DisabledByEnv => "disabled by environment",
        R::ModelNotAllowed => "model not allowed",
        R::SdkOptInRequired => "SDK opt-in required",
        R::Pending => "warming up",
        R::UnknownReason => "unknown reason",
        R::Unknown(s) => return format!("reason: {s}"),
    }
    .to_string()
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
                } else if let Some(reason) = &msg.fast_mode_disabled_reason {
                    // Fast mode was requested but couldn't serve — say why (#1475).
                    let label = fast_mode_disabled_label(reason);
                    html! {
                        <span
                            class="stat-item fast-mode-off"
                            title={format!("Fast mode unavailable: {label}")}
                        >
                            { "Fast off" }
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
    let metrics_footer =
        super::super::turn_metrics_footer::render_turn_metrics_footer(turn_metrics);

    if is_error {
        if let Some(error_html) = try_render_api_error(msg.result.as_deref()) {
            return html! {
                <>
                    { error_html }
                    <div class={classes!("claude-message", "result-message", status_class)}>
                        <div class="result-stats-bar">
                            <span class={classes!("result-status", status_class)}>{ "✗" }</span>
                            <span class={classes!("result-done-label", status_class)}>{ "failed" }</span>
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
                <span class={classes!("result-done-label", status_class)}>
                    { if is_error { "failed" } else { "completed" } }
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
