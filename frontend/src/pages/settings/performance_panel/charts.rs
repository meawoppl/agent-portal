//! Chart composition for the Performance settings panel.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use shared::api::MetricBucket;
use yew::prelude::*;

use crate::components::charts::{AxisScale, BucketKind, LinePlot, StackedArea};

use super::model::{
    bucket_group_key, bucket_param, distinct_bucket_starts, GroupBy, GroupKey, TimeWindow,
};
use super::series::{
    build_auxiliary_token_series, build_cache_hit_series, build_cost_per_token_series,
    build_p50_p95_series, build_stop_reason_series,
};

/// Render the performance charts from a non-empty `buckets` slice.
pub(super) fn render_charts(
    buckets: &[MetricBucket],
    group_by: &GroupBy,
    pairs: &[GroupKey],
    window: TimeWindow,
    axis_scale: AxisScale,
) -> Html {
    let bucket_axis = distinct_bucket_starts(buckets);
    // Resolve the bucket-kind from the same wire param we sent on the request,
    // so the time-axis label format stays in lockstep with the data. Defaults
    // to `Day` if the param ever drifts to something `from_wire` doesn't know.
    let bucket_kind = BucketKind::from_wire(bucket_param(window)).unwrap_or(BucketKind::Day);

    // For per-pair plots: filter pairs by group-by.
    let active_pairs: Vec<GroupKey> = match group_by {
        GroupBy::All => pairs.to_vec(),
        GroupBy::Pair(p) => vec![p.clone()],
    };

    // Bucketed by (pair, ts) for O(1) lookup.
    let mut indexed: BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket> = BTreeMap::new();
    for b in buckets {
        indexed.insert((bucket_group_key(b), b.bucket_start), b);
    }

    // ------------ a) Throughput trend: p50 (solid) + p95 (dashed) ------------
    let throughput_series = build_p50_p95_series(
        &indexed,
        &bucket_axis,
        &active_pairs,
        |r| r.throughput_p50_tps,
        |r| r.throughput_p95_tps,
    );

    // ------------ b) TTFT trend: p50 (solid) + p95 (dashed) seconds ----------
    let ttft_series = build_p50_p95_series(
        &indexed,
        &bucket_axis,
        &active_pairs,
        |r| r.ttft_p50_ms.map(|ms| ms as f64 / 1000.0),
        |r| r.ttft_p95_ms.map(|ms| ms as f64 / 1000.0),
    );

    // ------------ c) Stop-reason stacked area ---------------------------------
    let stop_reason_series = build_stop_reason_series(buckets, &bucket_axis, &active_pairs);

    // ------------ d) Cache hit rate (Claude rows only) ------------------------
    let cache_hit_series = build_cache_hit_series(&indexed, &bucket_axis, &active_pairs);

    // ------------ e) Cost per output token (skips codex / no-cost rows) ------
    let cost_series = build_cost_per_token_series(&indexed, &bucket_axis, &active_pairs);

    // ------------ f) Auxiliary token volume (reasoning + subagents) ----------
    let auxiliary_token_series =
        build_auxiliary_token_series(&indexed, &bucket_axis, &active_pairs);

    html! {
        <div class="performance-charts">
            <LinePlot
                title="Throughput"
                y_label="tok/s"
                buckets={bucket_axis.clone()}
                bucket_kind={bucket_kind}
                series={throughput_series}
                axis_scale={axis_scale}
            />
            <LinePlot
                title="Time to first token"
                y_label="seconds"
                buckets={bucket_axis.clone()}
                bucket_kind={bucket_kind}
                series={ttft_series}
                axis_scale={axis_scale}
            />
            <StackedArea
                title="Stop-reason mix"
                y_label="turns"
                buckets={bucket_axis.clone()}
                bucket_kind={bucket_kind}
                series={stop_reason_series}
                axis_scale={axis_scale}
            />
            <LinePlot
                title="Cache hit rate"
                y_label="%"
                buckets={bucket_axis.clone()}
                bucket_kind={bucket_kind}
                series={cache_hit_series}
                axis_scale={axis_scale}
            />
            <LinePlot
                title="Cost per 1k output tokens"
                y_label="USD"
                buckets={bucket_axis.clone()}
                bucket_kind={bucket_kind}
                series={cost_series}
                axis_scale={axis_scale}
            />
            <LinePlot
                title="Auxiliary tokens"
                y_label="tokens"
                buckets={bucket_axis}
                bucket_kind={bucket_kind}
                series={auxiliary_token_series}
                axis_scale={axis_scale}
            />
        </div>
    }
}
