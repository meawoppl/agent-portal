//! Dashboard-header pill showing per-(model, service_tier) trend of a
//! chosen metric over the user's recent turns. Hand-rolled SVG sparkline
//! ([`super::sparkline`]) + dropdown to switch metric.
//!
//! The pill picks the **most-recently-active** (model, service_tier) pair
//! from the recent-turns buffer and renders a single line for that pair.
//! Multi-line / multi-pair rendering is intentionally deferred to a later
//! PR — the v1 chrome is one chip, one trend.

use shared::TurnMetrics;
use wasm_bindgen::JsCast;
use web_sys::Element;
use yew::prelude::*;

use super::sparkline::Sparkline;

/// Which metric the sparkline plots. Selectable via the dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SparklineMetric {
    TokPerSec,
    Ttft,
    MaxGap,
    CacheHit,
    Thinking,
    Subagent,
}

impl SparklineMetric {
    fn label(self) -> &'static str {
        match self {
            Self::TokPerSec => "tok/s",
            Self::Ttft => "TTFT",
            Self::MaxGap => "max gap",
            Self::CacheHit => "cache hit",
            Self::Thinking => "thinking",
            Self::Subagent => "subagent",
        }
    }

    fn all() -> &'static [SparklineMetric] {
        &[
            SparklineMetric::TokPerSec,
            SparklineMetric::Ttft,
            SparklineMetric::MaxGap,
            SparklineMetric::CacheHit,
            SparklineMetric::Thinking,
            SparklineMetric::Subagent,
        ]
    }
}

/// Pick the (model, service_tier) pair of the *most recent* turn in the
/// buffer. Returns `None` when the buffer is empty or the newest turn has
/// neither field populated.
///
/// The buffer is sorted oldest → newest, so the newest turn is the last
/// element. The pair is the (model, tier) tuple — both `Option` because
/// proxies can omit either, and turns with `model == None` get rolled into
/// a single "unknown model" pill when there is nothing else to show.
pub(crate) fn pick_most_recent_model_tier(
    buf: &[TurnMetrics],
) -> Option<(Option<String>, Option<String>)> {
    let last = buf.last()?;
    if last.model.is_none() && last.service_tier.is_none() {
        return None;
    }
    Some((last.model.clone(), last.service_tier.clone()))
}

/// Build a human-readable label from a (model, tier) pair. The model name
/// is cheap-shortened by stripping a leading vendor prefix (`claude-`,
/// `gpt-`, `o-`) and a trailing date stamp (`-2026-…`) so the chip stays
/// compact in the dashboard header. The tier is appended in lowercase
/// when present and not `"standard"` (the default tier is never worth
/// chip space).
pub(crate) fn format_model_tier_label(model: &Option<String>, tier: &Option<String>) -> String {
    let short_model = model
        .as_deref()
        .map(compact_model_label)
        .unwrap_or_else(|| "unknown".to_string());
    match tier.as_deref() {
        Some(t) if !t.is_empty() && !t.eq_ignore_ascii_case("standard") => {
            format!("{short_model} {}", t.to_ascii_lowercase())
        }
        _ => short_model,
    }
}

/// Strip a vendor prefix + trailing dated suffix so a model name fits the
/// chip. `claude-opus-4-5-20260301` → `opus-4-5`; `gpt-5-mini` → `5-mini`.
/// Named distinctly from `message_renderer::shorten_model_name` (which
/// produces display names like "Opus 4.5") to avoid import mix-ups.
fn compact_model_label(model: &str) -> String {
    let trimmed = model
        .strip_prefix("claude-")
        .or_else(|| model.strip_prefix("gpt-"))
        .or_else(|| model.strip_prefix("o"))
        .unwrap_or(model);
    // Drop a trailing -YYYYMMDD if present (Anthropic dated checkpoints).
    let mut parts: Vec<&str> = trimmed.split('-').collect();
    if let Some(last) = parts.last() {
        if last.len() == 8 && last.chars().all(|c| c.is_ascii_digit()) {
            parts.pop();
        }
    }
    parts.join("-")
}

/// Filter the buffer to turns that match the chosen (model, tier) pair,
/// then project the chosen metric per turn, skipping turns where the
/// metric is `None` or zero (so the sparkline doesn't render misleading
/// flat zeros for codex turns when the chosen metric is cache hit, etc).
pub(crate) fn project_metric(
    buf: &[TurnMetrics],
    model: &Option<String>,
    tier: &Option<String>,
    metric: SparklineMetric,
) -> Vec<f64> {
    buf.iter()
        .filter(|m| &m.model == model && &m.service_tier == tier)
        .filter_map(|m| metric_value(m, metric))
        .collect()
}

/// Pull the chosen scalar from one row, returning `None` when the row
/// doesn't have meaningful data for that metric (so it's skipped from the
/// sparkline rather than rendered as a misleading zero).
fn metric_value(m: &TurnMetrics, metric: SparklineMetric) -> Option<f64> {
    match metric {
        SparklineMetric::TokPerSec => {
            let gen_ms = m.generation_duration_ms?;
            if gen_ms <= 0 {
                return None;
            }
            let secs = (gen_ms as f64) / 1000.0;
            let toks = m.output_tokens as f64;
            if toks <= 0.0 {
                return None;
            }
            Some(toks / secs)
        }
        SparklineMetric::Ttft => {
            let ms = m.ttft_ms?;
            if ms <= 0 {
                return None;
            }
            Some(ms as f64 / 1000.0)
        }
        SparklineMetric::MaxGap => {
            let ms = m.max_inter_token_gap_ms?;
            if ms <= 0 {
                return None;
            }
            Some(ms as f64 / 1000.0)
        }
        SparklineMetric::CacheHit => {
            let denom = (m.cache_read_tokens + m.cache_creation_tokens + m.input_tokens) as f64;
            if denom <= 0.0 {
                return None;
            }
            Some((m.cache_read_tokens as f64 / denom) * 100.0)
        }
        SparklineMetric::Thinking => (m.thinking_tokens > 0).then_some(m.thinking_tokens as f64),
        SparklineMetric::Subagent => (m.subagent_tokens > 0).then_some(m.subagent_tokens as f64),
    }
}

/// Format the most-recent value of the chosen metric for the right side of
/// the pill. Mirrors the per-turn footer formatting but in compact form.
fn format_current_value(metric: SparklineMetric, value: f64) -> String {
    match metric {
        SparklineMetric::TokPerSec => format!("{value:.1} tok/s"),
        SparklineMetric::Ttft => format!("TTFT {value:.2}s"),
        SparklineMetric::MaxGap => format!("gap {value:.1}s"),
        SparklineMetric::CacheHit => format!("cache {:.0}%", value),
        SparklineMetric::Thinking => format!("{} thinking", compact_metric_count(value)),
        SparklineMetric::Subagent => format!("{} subagent", compact_metric_count(value)),
    }
}

fn compact_metric_count(value: f64) -> String {
    if value < 1000.0 {
        format!("{value:.0}")
    } else {
        format!("{:.1}k", value / 1000.0)
    }
}

/// Props for the header pill.
#[derive(Properties, PartialEq)]
pub struct TurnMetricsHeaderPillProps {
    /// The user's recent turn-metrics buffer (oldest → newest).
    pub metrics: Vec<TurnMetrics>,
}

#[function_component(TurnMetricsHeaderPill)]
pub fn turn_metrics_header_pill(props: &TurnMetricsHeaderPillProps) -> Html {
    let selected_metric = use_state(|| SparklineMetric::TokPerSec);
    let dropdown_open = use_state(|| false);
    let pill_ref = use_node_ref();

    {
        let dropdown_open = dropdown_open.clone();
        let pill_ref = pill_ref.clone();
        let is_open = *dropdown_open;
        use_effect_with(is_open, move |is_open| {
            let listener = if *is_open {
                let document = gloo::utils::document();
                Some(gloo::events::EventListener::new(
                    &document,
                    "click",
                    move |e| {
                        if let Some(pill_el) = pill_ref.cast::<Element>() {
                            if let Some(target) =
                                e.target().and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                            {
                                if !pill_el.contains(Some(&target)) {
                                    dropdown_open.set(false);
                                }
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

    // Pick the (model, tier) pair from the most recent turn. If nothing
    // qualifies (empty buffer, or no model/tier on the newest row), render
    // nothing — the empty state is "no chip in the header at all".
    let Some((model, tier)) = pick_most_recent_model_tier(&props.metrics) else {
        return html! {};
    };

    let values = project_metric(&props.metrics, &model, &tier, *selected_metric);
    let label = format_model_tier_label(&model, &tier);
    let current_value = values.last().copied();

    let on_toggle = {
        let dropdown_open = dropdown_open.clone();
        Callback::from(move |e: MouseEvent| {
            e.stop_propagation();
            dropdown_open.set(!*dropdown_open);
        })
    };

    let chevron_class = if *dropdown_open {
        "turn-metrics-pill-chevron open"
    } else {
        "turn-metrics-pill-chevron"
    };

    html! {
        <div ref={pill_ref} class="turn-metrics-pill" onclick={on_toggle.clone()}>
            <span class="turn-metrics-pill-label">{ label }</span>
            <Sparkline values={values} />
            <span class="turn-metrics-pill-value">
                { current_value
                    .map(|v| format_current_value(*selected_metric, v))
                    .unwrap_or_else(|| selected_metric.label().to_string()) }
            </span>
            <span class={chevron_class} aria-hidden="true">{ "\u{25be}" }</span>
            if *dropdown_open {
                <div class="turn-metrics-pill-menu" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                    { for SparklineMetric::all().iter().copied().map(|m| {
                        let is_active = *selected_metric == m;
                        let selected_metric = selected_metric.clone();
                        let dropdown_open = dropdown_open.clone();
                        let on_pick = Callback::from(move |_| {
                            selected_metric.set(m);
                            dropdown_open.set(false);
                        });
                        let class = if is_active {
                            "turn-metrics-pill-menu-item active"
                        } else {
                            "turn-metrics-pill-menu-item"
                        };
                        html! {
                            <button class={class} onclick={on_pick}>
                                { m.label() }
                            </button>
                        }
                    })}
                </div>
            }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    fn mk(model: Option<&str>, tier: Option<&str>, idx: i64) -> TurnMetrics {
        TurnMetrics {
            id: Some(Uuid::new_v4()),
            session_id: Uuid::new_v4(),
            user_message_id: None,
            started_at: Utc
                .with_ymd_and_hms(2026, 5, 27, 12, 0, (idx as u32) % 60)
                .unwrap(),
            first_token_at: None,
            completed_at: None,
            ttft_ms: Some(500 + idx),
            total_duration_ms: Some(2000 + idx),
            generation_duration_ms: Some(1000 + idx),
            max_inter_token_gap_ms: Some(100 + idx),
            input_tokens: 100,
            output_tokens: 200,
            cache_creation_tokens: 0,
            cache_read_tokens: 50,
            thinking_tokens: 0,
            subagent_tokens: 0,
            stop_reason: None,
            is_error: false,
            tool_call_count: 0,
            stream_restarts: 0,
            agent_type: "claude".to_string(),
            model: model.map(|s| s.to_string()),
            service_tier: tier.map(|s| s.to_string()),
            total_cost_usd: Some(0.012),
        }
    }

    #[test]
    fn pick_pair_empty_buffer_returns_none() {
        assert!(pick_most_recent_model_tier(&[]).is_none());
    }

    #[test]
    fn pick_pair_uses_newest_row() {
        let buf = vec![
            mk(Some("claude-sonnet-4-5"), Some("standard"), 0),
            mk(Some("claude-opus-4-7"), Some("priority"), 1),
        ];
        let pair = pick_most_recent_model_tier(&buf).unwrap();
        assert_eq!(pair.0.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(pair.1.as_deref(), Some("priority"));
    }

    #[test]
    fn pick_pair_returns_none_when_newest_has_no_model_or_tier() {
        let buf = vec![mk(None, None, 0)];
        assert!(pick_most_recent_model_tier(&buf).is_none());
    }

    #[test]
    fn compact_model_label_strips_vendor_prefix_and_dated_suffix() {
        assert_eq!(compact_model_label("claude-opus-4-7-20260301"), "opus-4-7");
        assert_eq!(compact_model_label("claude-sonnet-4-5"), "sonnet-4-5");
        assert_eq!(compact_model_label("gpt-5-mini"), "5-mini");
        // No prefix, no suffix → unchanged.
        assert_eq!(compact_model_label("haiku-4-5"), "haiku-4-5");
    }

    #[test]
    fn format_label_skips_standard_tier_appends_others() {
        assert_eq!(
            format_model_tier_label(
                &Some("claude-sonnet-4-5".to_string()),
                &Some("standard".to_string())
            ),
            "sonnet-4-5"
        );
        assert_eq!(
            format_model_tier_label(
                &Some("claude-opus-4-7".to_string()),
                &Some("priority".to_string())
            ),
            "opus-4-7 priority"
        );
        assert_eq!(
            format_model_tier_label(&None, &Some("priority".to_string())),
            "unknown priority"
        );
    }

    #[test]
    fn project_filters_by_model_tier_pair() {
        let buf = vec![
            mk(Some("claude-sonnet-4-5"), Some("standard"), 0),
            mk(Some("claude-opus-4-7"), Some("priority"), 1),
            mk(Some("claude-sonnet-4-5"), Some("standard"), 2),
        ];
        // Project tok/s for sonnet+standard — should pick rows 0 and 2 (not 1).
        let series = project_metric(
            &buf,
            &Some("claude-sonnet-4-5".to_string()),
            &Some("standard".to_string()),
            SparklineMetric::TokPerSec,
        );
        assert_eq!(series.len(), 2);
    }

    #[test]
    fn project_skips_codex_rows_for_cache_hit_when_no_input_tokens() {
        // Codex turns with no input/cache tokens at all should be skipped
        // from the cache-hit projection.
        let mut row = mk(Some("gpt-5"), Some("standard"), 0);
        row.input_tokens = 0;
        row.cache_creation_tokens = 0;
        row.cache_read_tokens = 0;
        let buf = vec![row];
        let series = project_metric(
            &buf,
            &Some("gpt-5".to_string()),
            &Some("standard".to_string()),
            SparklineMetric::CacheHit,
        );
        assert!(series.is_empty());
    }

    #[test]
    fn project_tok_per_sec_math() {
        // output_tokens=200, generation_duration_ms=1001 → 200 / 1.001 ≈ 199.8
        let buf = vec![mk(Some("claude-opus-4-7"), Some("priority"), 1)];
        let series = project_metric(
            &buf,
            &Some("claude-opus-4-7".to_string()),
            &Some("priority".to_string()),
            SparklineMetric::TokPerSec,
        );
        assert_eq!(series.len(), 1);
        assert!((series[0] - 199.8).abs() < 0.1);
    }

    #[test]
    fn project_auxiliary_tokens_skips_zero_and_keeps_positive_values() {
        let mut first = mk(Some("gpt-5"), Some("standard"), 0);
        first.thinking_tokens = 0;
        first.subagent_tokens = 0;
        let mut second = mk(Some("gpt-5"), Some("standard"), 1);
        second.thinking_tokens = 1500;
        second.subagent_tokens = 275;
        let buf = vec![first, second];

        let model = Some("gpt-5".to_string());
        let tier = Some("standard".to_string());
        assert_eq!(
            project_metric(&buf, &model, &tier, SparklineMetric::Thinking),
            vec![1500.0]
        );
        assert_eq!(
            project_metric(&buf, &model, &tier, SparklineMetric::Subagent),
            vec![275.0]
        );
    }

    #[test]
    fn format_current_value_formats_each_metric() {
        assert_eq!(
            format_current_value(SparklineMetric::TokPerSec, 47.234),
            "47.2 tok/s"
        );
        assert_eq!(
            format_current_value(SparklineMetric::Ttft, 1.314),
            "TTFT 1.31s"
        );
        assert_eq!(
            format_current_value(SparklineMetric::MaxGap, 0.45),
            "gap 0.5s"
        );
        assert_eq!(
            format_current_value(SparklineMetric::CacheHit, 83.7),
            "cache 84%"
        );
        assert_eq!(
            format_current_value(SparklineMetric::Thinking, 1500.0),
            "1.5k thinking"
        );
        assert_eq!(
            format_current_value(SparklineMetric::Subagent, 275.0),
            "275 subagent"
        );
    }
}
