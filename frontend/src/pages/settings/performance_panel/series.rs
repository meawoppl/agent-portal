//! Chart series builders for the Performance settings panel.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use shared::api::MetricBucket;

use crate::components::charts::{LineSeries, StackedSeries};

use super::{bucket_group_key, bucket_index, pair_color, pair_label, GroupKey};

/// Build paired p50 (solid) / p95 (dashed) line series per active pair,
/// like the existing [`build_cache_hit_series`]. `p50` / `p95` extract the
/// already-scaled value from a bucket row; series with no values are dropped.
pub(super) fn build_p50_p95_series(
    indexed: &BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket>,
    bucket_axis: &[DateTime<Utc>],
    active_pairs: &[GroupKey],
    p50: impl Fn(&MetricBucket) -> Option<f64>,
    p95: impl Fn(&MetricBucket) -> Option<f64>,
) -> Vec<LineSeries> {
    let mut out: Vec<LineSeries> = Vec::new();
    for (idx, pair) in active_pairs.iter().enumerate() {
        let color = pair_color(idx);
        let label = pair_label(pair);
        let mut p50_vals: Vec<Option<f64>> = Vec::with_capacity(bucket_axis.len());
        let mut p95_vals: Vec<Option<f64>> = Vec::with_capacity(bucket_axis.len());
        for ts in bucket_axis {
            let row = indexed.get(&(pair.clone(), *ts));
            p50_vals.push(row.and_then(|r| p50(r)));
            p95_vals.push(row.and_then(|r| p95(r)));
        }
        if p50_vals.iter().any(Option::is_some) {
            out.push(LineSeries {
                label: format!("{label} p50"),
                color: color.to_string(),
                dashed: false,
                values: p50_vals,
            });
        }
        if p95_vals.iter().any(Option::is_some) {
            out.push(LineSeries {
                label: format!("{label} p95"),
                color: color.to_string(),
                dashed: true,
                values: p95_vals,
            });
        }
    }
    out
}

/// Aggregate stop-reason counts across the active pairs into a fixed-order
/// stack: `end_turn`, `tool_use`, `max_tokens`, `error`, `other`. The
/// `max_tokens` band is red (`#f7768e`) so spikes pop.
pub(super) fn build_stop_reason_series(
    buckets: &[MetricBucket],
    bucket_axis: &[DateTime<Utc>],
    active_pairs: &[GroupKey],
) -> Vec<StackedSeries> {
    const REASONS: &[(&str, &str, &str)] = &[
        ("end_turn", "end_turn", "#9ece6a"),
        ("tool_use", "tool_use", "#7aa2f7"),
        ("max_tokens", "max_tokens", "#f7768e"),
        ("error", "error", "#bb9af7"),
    ];
    let active_set: std::collections::HashSet<GroupKey> = active_pairs.iter().cloned().collect();
    let mut by_reason: BTreeMap<&'static str, Vec<f64>> = BTreeMap::new();
    let mut other_vals: Vec<f64> = vec![0.0; bucket_axis.len()];
    for (_, key, _) in REASONS {
        by_reason.insert(key, vec![0.0; bucket_axis.len()]);
    }
    for b in buckets {
        let pair = bucket_group_key(b);
        if !active_set.contains(&pair) {
            continue;
        }
        let Some(idx) = bucket_index(bucket_axis, b.bucket_start) else {
            continue;
        };
        for (raw_reason, count) in &b.stop_reason_counts {
            if let Some(vec_for_reason) =
                REASONS
                    .iter()
                    .find_map(|(_, k, _)| if k == raw_reason { Some(*k) } else { None })
            {
                if let Some(vals) = by_reason.get_mut(vec_for_reason) {
                    vals[idx] += *count as f64;
                }
            } else {
                other_vals[idx] += *count as f64;
            }
        }
    }

    let mut series: Vec<StackedSeries> = REASONS
        .iter()
        .filter_map(|(label, key, color)| {
            let vals = by_reason.remove(*key).unwrap_or_default();
            if vals.iter().all(|v| *v <= 0.0) {
                None
            } else {
                Some(StackedSeries {
                    label: (*label).to_string(),
                    color: (*color).to_string(),
                    values: vals,
                })
            }
        })
        .collect();
    if other_vals.iter().any(|v| *v > 0.0) {
        series.push(StackedSeries {
            label: "other".to_string(),
            color: "#565f89".to_string(),
            values: other_vals,
        });
    }
    series
}

/// Compute cache-hit % per bucket per pair. Skips rows where the denominator
/// is zero (codex / no-cache turns), so codex pairs naturally produce empty
/// series and get filtered out.
pub(super) fn build_cache_hit_series(
    indexed: &BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket>,
    bucket_axis: &[DateTime<Utc>],
    active_pairs: &[GroupKey],
) -> Vec<LineSeries> {
    let mut out: Vec<LineSeries> = Vec::new();
    for (idx, pair) in active_pairs.iter().enumerate() {
        let color = pair_color(idx);
        let label = pair_label(pair);
        let mut vals: Vec<Option<f64>> = Vec::with_capacity(bucket_axis.len());
        for ts in bucket_axis {
            let row = indexed.get(&(pair.clone(), *ts));
            let v = row.and_then(|r| {
                let denom = (r.cache_read_tokens_sum
                    + r.cache_creation_tokens_sum
                    + r.input_tokens_sum) as f64;
                if denom <= 0.0 {
                    None
                } else {
                    Some((r.cache_read_tokens_sum as f64 / denom) * 100.0)
                }
            });
            vals.push(v);
        }
        if vals.iter().any(Option::is_some) {
            out.push(LineSeries {
                label,
                color: color.to_string(),
                dashed: false,
                values: vals,
            });
        }
    }
    out
}

/// Compute cost per 1k output tokens per bucket per pair. Skips rows where
/// either the cost sum is `None`/`<= 0` or the output-token sum is `0` —
/// codex pairs naturally produce empty series.
pub(super) fn build_cost_per_token_series(
    indexed: &BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket>,
    bucket_axis: &[DateTime<Utc>],
    active_pairs: &[GroupKey],
) -> Vec<LineSeries> {
    let mut out: Vec<LineSeries> = Vec::new();
    for (idx, pair) in active_pairs.iter().enumerate() {
        let color = pair_color(idx);
        let label = pair_label(pair);
        let mut vals: Vec<Option<f64>> = Vec::with_capacity(bucket_axis.len());
        for ts in bucket_axis {
            let row = indexed.get(&(pair.clone(), *ts));
            let v = row.and_then(|r| match r.total_cost_usd_sum {
                Some(cost) if cost > 0.0 && r.output_tokens_sum > 0 => {
                    Some((cost / r.output_tokens_sum as f64) * 1000.0)
                }
                _ => None,
            });
            vals.push(v);
        }
        if vals.iter().any(Option::is_some) {
            out.push(LineSeries {
                label,
                color: color.to_string(),
                dashed: false,
                values: vals,
            });
        }
    }
    out
}

/// Plot reasoning/thinking tokens and subagent tokens per bucket. The two
/// lines share the model/tier color; subagent tokens use the dashed variant so
/// the relationship stays readable even when multiple pairs are active.
pub(super) fn build_auxiliary_token_series(
    indexed: &BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket>,
    bucket_axis: &[DateTime<Utc>],
    active_pairs: &[GroupKey],
) -> Vec<LineSeries> {
    let mut out: Vec<LineSeries> = Vec::new();
    for (idx, pair) in active_pairs.iter().enumerate() {
        let color = pair_color(idx);
        let label = pair_label(pair);
        let mut thinking_vals: Vec<Option<f64>> = Vec::with_capacity(bucket_axis.len());
        let mut subagent_vals: Vec<Option<f64>> = Vec::with_capacity(bucket_axis.len());
        for ts in bucket_axis {
            let row = indexed.get(&(pair.clone(), *ts));
            thinking_vals.push(row.and_then(|r| positive_i64(r.thinking_tokens_sum)));
            subagent_vals.push(row.and_then(|r| positive_i64(r.subagent_tokens_sum)));
        }
        if thinking_vals.iter().any(Option::is_some) {
            out.push(LineSeries {
                label: format!("{label} thinking"),
                color: color.to_string(),
                dashed: false,
                values: thinking_vals,
            });
        }
        if subagent_vals.iter().any(Option::is_some) {
            out.push(LineSeries {
                label: format!("{label} subagent"),
                color: color.to_string(),
                dashed: true,
                values: subagent_vals,
            });
        }
    }
    out
}

fn positive_i64(value: i64) -> Option<f64> {
    (value > 0).then_some(value as f64)
}
