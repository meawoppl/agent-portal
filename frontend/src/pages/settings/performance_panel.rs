//! Settings → Performance page.
//!
//! Drills into the user's per-turn metrics with five hand-rolled SVG plots
//! backed by `GET /api/metrics/turns?bucket=…&window=…`. The page hangs off
//! the existing settings nav (alongside Launchers / Tokens / Sounds / Sessions
//! / Appearance) — same chrome, same back-button pattern.
//!
//! On mount, fetches the default high-resolution `bucket=hour&window=30d`
//! aggregation, then refetches when the user switches the time-window radio.
//! A group-by dropdown filters to one `(agent_type, model, service_tier)`
//! group, or "All" to render one line per group.

use chrono::{DateTime, Utc};
use shared::api::MetricBucket;
use yew::prelude::*;

use crate::components::charts::AxisScale;

mod body;
mod charts;
mod controls;
mod series;
mod use_metrics;
use body::render_performance_body;
use controls::{render_performance_controls, PerformanceControlsProps};
use use_metrics::use_performance_metrics;

/// (agent_type, model, service_tier) tuple used as the group-by key. Codex
/// currently reports no model or tier; keeping the agent in the key lets the
/// UI label that shape explicitly without colliding with missing Claude
/// metadata.
type GroupKey = (String, Option<String>, Option<String>);

/// Pure helper: list the distinct (agent, model, tier) groups present in the
/// bucket list.
fn distinct_pairs(buckets: &[MetricBucket]) -> Vec<GroupKey> {
    let mut seen: std::collections::BTreeSet<GroupKey> = std::collections::BTreeSet::new();
    for b in buckets {
        seen.insert(bucket_group_key(b));
    }
    seen.into_iter().collect()
}

fn bucket_group_key(bucket: &MetricBucket) -> GroupKey {
    (
        bucket.agent_type.clone(),
        bucket.model.clone(),
        bucket.service_tier.clone(),
    )
}

/// Format an (agent, model, tier) group as a human-readable label for the
/// dropdown and legend.
///
/// Deliberately not `turn_metrics_pill::format_model_tier_label`: this page
/// shows the full model id (no vendor-prefix shortening), keeps the tier's
/// original case, and adds codex / agent-without-model handling.
fn pair_label(pair: &GroupKey) -> String {
    let base = match (pair.0.as_str(), pair.1.as_deref()) {
        ("codex", None) => "Codex".to_string(),
        (_, Some(model)) => model.to_string(),
        (agent, None) if !agent.is_empty() => format!("{agent} unknown"),
        _ => "unknown".to_string(),
    };
    match pair.2.as_deref() {
        Some(t) if !t.is_empty() && !t.eq_ignore_ascii_case("standard") => {
            format!("{base} {t}")
        }
        _ => base,
    }
}

/// Pick a stable color from the Tokyo-Night palette. We cycle through a
/// fixed palette by pair-index so the same pair always gets the same color
/// across re-renders.
fn pair_color(idx: usize) -> &'static str {
    const PALETTE: &[&str] = &[
        "#7aa2f7", // accent blue
        "#bb9af7", // purple
        "#9ece6a", // green
        "#e0af68", // yellow
        "#f7768e", // red (used by max_tokens band)
        "#7dcfff", // cyan
        "#ff9e64", // orange
    ];
    PALETTE[idx % PALETTE.len()]
}

/// Build distinct bucket-start timestamps (the x-axis) preserving order.
fn distinct_bucket_starts(buckets: &[MetricBucket]) -> Vec<DateTime<Utc>> {
    let mut seen: std::collections::BTreeSet<DateTime<Utc>> = std::collections::BTreeSet::new();
    for b in buckets {
        seen.insert(b.bucket_start);
    }
    seen.into_iter().collect()
}

/// Index a bucket-start timestamp to its position in the x-axis, returning
/// `None` if missing.
fn bucket_index(buckets: &[DateTime<Utc>], ts: DateTime<Utc>) -> Option<usize> {
    buckets.iter().position(|b| *b == ts)
}

/// Build the bucket-granularity query string for the selected window.
/// Pick high-fidelity buckets for the selected window. The charts only render
/// a handful of x-axis labels, so dense buckets preserve real per-turn shape
/// without cluttering the axis.
fn bucket_param(window: TimeWindow) -> &'static str {
    match window {
        TimeWindow::Hours1 => "1m",
        TimeWindow::Hours6 => "1m",
        TimeWindow::Days1 => "5m",
        TimeWindow::Days7 | TimeWindow::Days30 | TimeWindow::Days90 => "hour",
    }
}

/// Selectable time window for the radio group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeWindow {
    Hours1,
    Hours6,
    Days1,
    Days7,
    Days30,
    Days90,
}

impl TimeWindow {
    /// Radio-button label, which doubles as the exact wire value sent to
    /// `GET /api/metrics/turns?window=…` (the backend's window parser
    /// accepts the same `Nh` / `Nd` suffix form).
    fn label(self) -> &'static str {
        match self {
            Self::Hours1 => "1h",
            Self::Hours6 => "6h",
            Self::Days1 => "1d",
            Self::Days7 => "7d",
            Self::Days30 => "30d",
            Self::Days90 => "90d",
        }
    }
    fn all() -> &'static [TimeWindow] {
        &[
            TimeWindow::Hours1,
            TimeWindow::Hours6,
            TimeWindow::Days1,
            TimeWindow::Days7,
            TimeWindow::Days30,
            TimeWindow::Days90,
        ]
    }
}

/// Group-by selection: either a specific (agent, model, tier) group or "All".
#[derive(Debug, Clone, PartialEq)]
enum GroupBy {
    All,
    Pair(GroupKey),
}

impl GroupBy {
    /// Serialize to a stable string for the `<select>` `value` attribute.
    fn key(&self) -> String {
        match self {
            Self::All => "__ALL__".to_string(),
            Self::Pair((agent, m, t)) => format!(
                "{}|{}|{}",
                agent,
                m.as_deref().unwrap_or(""),
                t.as_deref().unwrap_or("")
            ),
        }
    }
    /// Inverse of [`key`]. Returns `GroupBy::All` for an unrecognized key.
    fn from_key(key: &str, pairs: &[GroupKey]) -> Self {
        if key == "__ALL__" {
            return Self::All;
        }
        for p in pairs {
            if Self::Pair(p.clone()).key() == key {
                return Self::Pair(p.clone());
            }
        }
        Self::All
    }
}

#[function_component(PerformancePanel)]
pub fn performance_panel() -> Html {
    let window = use_state(|| TimeWindow::Days30);
    let group_by = use_state(|| GroupBy::All);
    let axis_scale = use_state(|| AxisScale::Linear);
    let metrics = use_performance_metrics(*window);

    let pairs = distinct_pairs(&metrics.buckets);

    let on_window_change = {
        let window = window.clone();
        Callback::from(move |new_window: TimeWindow| {
            window.set(new_window);
        })
    };

    let on_group_change = {
        let group_by = group_by.clone();
        let pairs = pairs.clone();
        Callback::from(move |e: Event| {
            let target: web_sys::HtmlSelectElement = e.target_unchecked_into();
            let next = GroupBy::from_key(&target.value(), &pairs);
            group_by.set(next);
        })
    };

    let on_axis_scale_change = {
        let axis_scale = axis_scale.clone();
        Callback::from(move |scale: AxisScale| {
            axis_scale.set(scale);
        })
    };

    html! {
        <section class="performance-panel">
            <div class="section-header">
                <h2>{ "Performance" }</h2>
                <p class="section-description">
                    { "Per-turn latency, throughput, cache usage, and cost trends. \
                      Aggregated across all sessions you own." }
                </p>
            </div>

            { render_performance_controls(PerformanceControlsProps {
                window: *window,
                group_by: &group_by,
                axis_scale: *axis_scale,
                pairs: &pairs,
                on_window_change,
                on_group_change,
                on_axis_scale_change,
            }) }

            { render_performance_body(&metrics, &group_by, &pairs, *window, *axis_scale) }
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::collections::BTreeMap;

    use super::series::{
        build_auxiliary_token_series, build_cache_hit_series, build_cost_per_token_series,
        build_stop_reason_series,
    };

    fn mk_bucket(
        ts: DateTime<Utc>,
        model: Option<&str>,
        tier: Option<&str>,
        ttft_p50: Option<i64>,
        throughput_p50: Option<f64>,
        stop_counts: Vec<(&str, i64)>,
    ) -> MetricBucket {
        let mut counts = BTreeMap::new();
        for (k, v) in stop_counts {
            counts.insert(k.to_string(), v);
        }
        MetricBucket {
            bucket_start: ts,
            agent_type: "claude".to_string(),
            model: model.map(|s| s.to_string()),
            service_tier: tier.map(|s| s.to_string()),
            turn_count: 1,
            error_count: 0,
            ttft_p50_ms: ttft_p50,
            ttft_p95_ms: None,
            throughput_p50_tps: throughput_p50,
            throughput_p95_tps: None,
            input_tokens_sum: 1000,
            output_tokens_sum: 200,
            cache_read_tokens_sum: 500,
            cache_creation_tokens_sum: 100,
            thinking_tokens_sum: 0,
            subagent_tokens_sum: 0,
            total_cost_usd_sum: Some(0.05),
            stop_reason_counts: counts,
        }
    }

    #[test]
    fn distinct_pairs_dedupes_and_sorts() {
        let ts = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        let buckets = vec![
            mk_bucket(
                ts,
                Some("claude-opus-4-7"),
                Some("standard"),
                None,
                None,
                vec![],
            ),
            mk_bucket(
                ts,
                Some("claude-opus-4-7"),
                Some("standard"),
                None,
                None,
                vec![],
            ),
            mk_bucket(
                ts,
                Some("claude-sonnet-4-5"),
                Some("standard"),
                None,
                None,
                vec![],
            ),
        ];
        let pairs = distinct_pairs(&buckets);
        assert_eq!(pairs.len(), 2);
    }

    #[test]
    fn pair_label_appends_non_standard_tier() {
        let label = pair_label(&(
            "claude".to_string(),
            Some("claude-opus-4-7".to_string()),
            Some("priority".to_string()),
        ));
        assert_eq!(label, "claude-opus-4-7 priority");
    }

    #[test]
    fn pair_label_drops_standard_tier() {
        let label = pair_label(&(
            "claude".to_string(),
            Some("claude-opus-4-7".to_string()),
            Some("standard".to_string()),
        ));
        assert_eq!(label, "claude-opus-4-7");
    }

    #[test]
    fn pair_label_codex_when_no_model() {
        let label = pair_label(&("codex".to_string(), None, None));
        assert_eq!(label, "Codex");
    }

    #[test]
    fn pair_label_unknown_when_no_claude_model() {
        let label = pair_label(&("claude".to_string(), None, None));
        assert_eq!(label, "claude unknown");
    }

    #[test]
    fn pair_color_cycles_palette() {
        // Index 0 and index 7 (palette has 7 entries) should land on the same color.
        assert_eq!(pair_color(0), pair_color(7));
    }

    #[test]
    fn group_by_key_roundtrips_through_string() {
        let pairs = vec![(
            "claude".to_string(),
            Some("claude-opus-4-7".to_string()),
            Some("standard".to_string()),
        )];
        let gb = GroupBy::Pair(pairs[0].clone());
        let key = gb.key();
        let back = GroupBy::from_key(&key, &pairs);
        assert_eq!(back, gb);

        let all = GroupBy::All;
        assert_eq!(all.key(), "__ALL__");
        assert_eq!(GroupBy::from_key("__ALL__", &pairs), GroupBy::All);
    }

    #[test]
    fn group_by_unknown_key_falls_back_to_all() {
        let pairs: Vec<GroupKey> = vec![];
        assert_eq!(GroupBy::from_key("garbage", &pairs), GroupBy::All);
    }

    #[test]
    fn distinct_bucket_starts_sorted() {
        let t1 = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 5, 2, 0, 0, 0).unwrap();
        let buckets = vec![
            mk_bucket(t2, None, None, None, None, vec![]),
            mk_bucket(t1, None, None, None, None, vec![]),
            mk_bucket(t2, Some("x"), None, None, None, vec![]),
        ];
        let axis = distinct_bucket_starts(&buckets);
        assert_eq!(axis, vec![t1, t2]);
    }

    #[test]
    fn build_stop_reason_series_orders_and_drops_empty_bands() {
        let t1 = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        let buckets = vec![mk_bucket(
            t1,
            Some("m"),
            None,
            None,
            None,
            vec![("end_turn", 5), ("max_tokens", 1)],
        )];
        let axis = distinct_bucket_starts(&buckets);
        let pairs = distinct_pairs(&buckets);
        let series = build_stop_reason_series(&buckets, &axis, &pairs);
        // Only `end_turn` and `max_tokens` got counts; `tool_use` and `error` should not appear.
        let labels: Vec<&str> = series.iter().map(|s| s.label.as_str()).collect();
        assert!(labels.contains(&"end_turn"));
        assert!(labels.contains(&"max_tokens"));
        assert!(!labels.contains(&"tool_use"));
        assert!(!labels.contains(&"error"));
    }

    #[test]
    fn build_stop_reason_series_folds_unknown_reason_to_other() {
        let t1 = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        let buckets = vec![mk_bucket(
            t1,
            Some("m"),
            None,
            None,
            None,
            vec![("weird_reason", 3)],
        )];
        let axis = distinct_bucket_starts(&buckets);
        let pairs = distinct_pairs(&buckets);
        let series = build_stop_reason_series(&buckets, &axis, &pairs);
        let labels: Vec<&str> = series.iter().map(|s| s.label.as_str()).collect();
        assert!(labels.contains(&"other"));
    }

    #[test]
    fn build_cache_hit_series_skips_zero_denominator_rows() {
        let t1 = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        let mut b = mk_bucket(t1, Some("m"), None, None, None, vec![]);
        b.input_tokens_sum = 0;
        b.cache_read_tokens_sum = 0;
        b.cache_creation_tokens_sum = 0;
        let buckets = vec![b];
        let axis = distinct_bucket_starts(&buckets);
        let pairs = distinct_pairs(&buckets);
        let mut indexed: BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket> = BTreeMap::new();
        for bb in &buckets {
            indexed.insert((bucket_group_key(bb), bb.bucket_start), bb);
        }
        let series = build_cache_hit_series(&indexed, &axis, &pairs);
        // Only had a single bucket with denom=0 → no values → no series at all.
        assert!(series.is_empty());
    }

    #[test]
    fn build_cost_per_token_series_skips_zero_cost_rows() {
        let t1 = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        let mut b = mk_bucket(t1, Some("codex"), None, None, None, vec![]);
        b.total_cost_usd_sum = None;
        let buckets = vec![b];
        let axis = distinct_bucket_starts(&buckets);
        let pairs = distinct_pairs(&buckets);
        let mut indexed: BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket> = BTreeMap::new();
        for bb in &buckets {
            indexed.insert((bucket_group_key(bb), bb.bucket_start), bb);
        }
        let series = build_cost_per_token_series(&indexed, &axis, &pairs);
        assert!(series.is_empty());
    }

    #[test]
    fn build_cost_per_token_series_computes_per_1k_out() {
        let t1 = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        let mut b = mk_bucket(t1, Some("claude"), Some("standard"), None, None, vec![]);
        b.total_cost_usd_sum = Some(0.10);
        b.output_tokens_sum = 100;
        let buckets = vec![b];
        let axis = distinct_bucket_starts(&buckets);
        let pairs = distinct_pairs(&buckets);
        let mut indexed: BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket> = BTreeMap::new();
        for bb in &buckets {
            indexed.insert((bucket_group_key(bb), bb.bucket_start), bb);
        }
        let series = build_cost_per_token_series(&indexed, &axis, &pairs);
        assert_eq!(series.len(), 1);
        // $0.10 / 100 out * 1000 = $1.00 per 1k out
        let v = series[0].values[0].unwrap();
        assert!((v - 1.0).abs() < 1e-9);
    }

    #[test]
    fn build_auxiliary_token_series_splits_thinking_and_subagent() {
        let t1 = Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap();
        let mut b = mk_bucket(t1, Some("gpt-5"), Some("standard"), None, None, vec![]);
        b.thinking_tokens_sum = 1500;
        b.subagent_tokens_sum = 275;
        let buckets = vec![b];
        let axis = distinct_bucket_starts(&buckets);
        let pairs = distinct_pairs(&buckets);
        let mut indexed: BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket> = BTreeMap::new();
        for bb in &buckets {
            indexed.insert((bucket_group_key(bb), bb.bucket_start), bb);
        }

        let series = build_auxiliary_token_series(&indexed, &axis, &pairs);
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].label, "gpt-5 thinking");
        assert_eq!(series[0].values, vec![Some(1500.0)]);
        assert_eq!(series[1].label, "gpt-5 subagent");
        assert!(series[1].dashed);
        assert_eq!(series[1].values, vec![Some(275.0)]);
    }

    #[test]
    fn window_label_strings() {
        // label() doubles as the wire `window=` param — keep both shapes valid.
        assert_eq!(TimeWindow::Hours1.label(), "1h");
        assert_eq!(TimeWindow::Hours6.label(), "6h");
        assert_eq!(TimeWindow::Days1.label(), "1d");
        assert_eq!(TimeWindow::Days7.label(), "7d");
        assert_eq!(TimeWindow::Days30.label(), "30d");
        assert_eq!(TimeWindow::Days90.label(), "90d");
    }

    /// Windows should stay granular enough to show shape; the chart axis
    /// itself is already subsampled to a few readable labels.
    #[test]
    fn bucket_param_dispatches_on_window_length() {
        assert_eq!(bucket_param(TimeWindow::Hours1), "1m");
        assert_eq!(bucket_param(TimeWindow::Hours6), "1m");
        assert_eq!(bucket_param(TimeWindow::Days1), "5m");
        assert_eq!(bucket_param(TimeWindow::Days7), "hour");
        assert_eq!(bucket_param(TimeWindow::Days30), "hour");
        assert_eq!(bucket_param(TimeWindow::Days90), "hour");
    }

    #[test]
    fn time_window_all_lists_every_variant_in_chronological_order() {
        // The radio button order matches this slice — short windows first
        // so the most-recent view sits on the left.
        let all = TimeWindow::all();
        assert_eq!(all.len(), 6);
        assert!(matches!(all[0], TimeWindow::Hours1));
        assert!(matches!(all[5], TimeWindow::Days90));
    }
}
