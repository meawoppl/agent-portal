//! Rizzma-backed figures for Settings › Performance.
//!
//! Plot construction stays native: the browser receives a compact `.riz`
//! artifact and renders it through the same pinned, sandboxed runtime used by
//! transcript figures. This avoids linking Rizzma's renderer into the main
//! frontend WASM bundle.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, Response};
use chrono::{DateTime, Utc};
use rizzma::artist::Line2D;
use rizzma::core::color::Rgba;
use rizzma::{Figure, RcParams};
use serde::Deserialize;
use shared::api::MetricBucket;
use shared::AgentType;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::AppState;

use super::turn_metrics::{list_aggregated_turn_metrics, TurnMetricsAggregateQuery};

const COLORS: &[&str] = &[
    shared::palette::ACCENT_BLUE,
    shared::palette::ACCENT_PURPLE,
    shared::palette::ACCENT_GREEN,
    shared::palette::ACCENT_ORANGE,
    shared::palette::ACCENT_RED,
    shared::palette::ACCENT_TEAL,
    "#ff9e64",
];

#[derive(Debug, Deserialize)]
pub struct PerformanceFigureQuery {
    bucket: Option<String>,
    window: Option<String>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    scale: Option<String>,
    #[serde(default)]
    p95: Option<bool>,
}

type GroupKey = (AgentType, Option<String>, Option<String>);

#[derive(Clone)]
struct Series {
    label: String,
    color: Rgba,
    dashed: bool,
    values: Vec<Option<f64>>,
}

/// Return one authenticated performance dashboard as a portable Rizzma figure.
pub async fn get_performance_figure(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Query(query): Query<PerformanceFigureQuery>,
) -> Result<Response<Body>, AppError> {
    let aggregate = list_aggregated_turn_metrics(
        State(app_state),
        CurrentUserId(user_id),
        Query(TurnMetricsAggregateQuery {
            bucket: query.bucket,
            window: query.window,
        }),
    )
    .await?
    .0;

    let bytes = build_figure(
        &aggregate.buckets,
        query.group.as_deref(),
        query.scale.as_deref() == Some("log"),
        query.p95.unwrap_or(true),
    )?;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/vnd.rizzma.figure")
        .header(header::CACHE_CONTROL, "private, no-store")
        .body(Body::from(bytes))
        .map_err(|error| AppError::Internal(error.to_string()))
}

fn build_figure(
    buckets: &[MetricBucket],
    selected_group: Option<&str>,
    log_scale: bool,
    show_p95: bool,
) -> Result<Vec<u8>, AppError> {
    if buckets.is_empty() {
        return Err(AppError::NotFound("No performance data"));
    }
    let axis: Vec<DateTime<Utc>> = buckets
        .iter()
        .map(|bucket| bucket.bucket_start)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let all_groups: Vec<GroupKey> = buckets
        .iter()
        .map(group_key)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let groups: Vec<GroupKey> = all_groups
        .into_iter()
        .filter(|group| selected_group.is_none_or(|selected| selected == group_wire_key(group)))
        .collect();
    let indexed: BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket> = buckets
        .iter()
        .map(|bucket| ((group_key(bucket), bucket.bucket_start), bucket))
        .collect();

    let charts = [
        (
            "Throughput",
            "tok/s",
            percentile_series(
                &indexed,
                &axis,
                &groups,
                |row| row.throughput_p50_tps,
                |row| row.throughput_p95_tps,
                show_p95,
            ),
            false,
        ),
        (
            "Time to first token",
            "seconds",
            percentile_series(
                &indexed,
                &axis,
                &groups,
                |row| row.ttft_p50_ms.map(|value| value as f64 / 1_000.0),
                |row| row.ttft_p95_ms.map(|value| value as f64 / 1_000.0),
                show_p95,
            ),
            false,
        ),
        (
            "Stop-reason mix",
            "turns",
            stop_reason_series(buckets, &axis, &groups),
            true,
        ),
        (
            "Cache hit rate",
            "%",
            value_series(&indexed, &axis, &groups, |row| {
                let total = row.cache_read_tokens_sum
                    + row.cache_creation_tokens_sum
                    + row.input_tokens_sum;
                (total > 0).then_some(row.cache_read_tokens_sum as f64 / total as f64 * 100.0)
            }),
            false,
        ),
        (
            "Cost per 1k output tokens",
            "USD",
            value_series(&indexed, &axis, &groups, |row| {
                row.total_cost_usd_sum
                    .filter(|cost| *cost > 0.0)
                    .and_then(|cost| {
                        (row.output_tokens_sum > 0)
                            .then_some(cost / row.output_tokens_sum as f64 * 1_000.0)
                    })
            }),
            false,
        ),
        (
            "Auxiliary tokens",
            "tokens",
            auxiliary_series(&indexed, &axis, &groups),
            false,
        ),
    ];

    let mut rc = RcParams::dark();
    rc.figure_facecolor = color("#1a1b26");
    rc.axes_facecolor = color("#16161e");
    let mut figure = Figure::new(8.0, 10.0).with_dpi(100.0).with_rcparams(rc);
    let x: Vec<f64> = axis
        .iter()
        .map(|time| time.timestamp_millis() as f64 / 86_400_000.0)
        .collect();
    for (index, (title, ylabel, series, stacked)) in charts.iter().enumerate() {
        let axes = figure.add_subplot(3, 2, index + 1);
        axes.set_title(*title)
            .set_ylabel(*ylabel)
            .set_xaxis_date()
            .grid_with(color(shared::palette::MUTED_GRAY), 0.7, 0.35)
            .set_prop_cycle(series.iter().map(|item| item.color).collect());
        if log_scale {
            // The existing Portal scale gives zero a visible floor. Symlog is
            // the Rizzma equivalent: positive values read logarithmically
            // while zero remains representable.
            axes.set_yscale_symlog(10.0, 1.0);
        }
        if *stacked {
            let mut lower = vec![0.0; x.len()];
            for item in series {
                let upper: Vec<f64> = lower
                    .iter()
                    .zip(&item.values)
                    .map(|(base, value)| base + value.unwrap_or(0.0))
                    .collect();
                axes.fill_between(&x, &lower, &upper);
                lower = upper;
            }
        } else {
            for item in series {
                plot_segments(axes, &x, item);
            }
        }
        axes.legend(
            series
                .iter()
                .map(|item| (item.color, item.label.clone()))
                .collect(),
        );
    }
    figure
        .to_portable()
        .map_err(|error| AppError::Internal(format!("performance figure export failed: {error}")))
}

fn plot_segments(axes: &mut rizzma::Axes, x: &[f64], series: &Series) {
    let mut start = 0;
    while start < series.values.len() {
        while start < series.values.len() && series.values[start].is_none() {
            start += 1;
        }
        let mut end = start;
        while end < series.values.len() && series.values[end].is_some() {
            end += 1;
        }
        if end > start {
            let y: Vec<f64> = series.values[start..end]
                .iter()
                .flatten()
                .copied()
                .collect();
            let mut line = Line2D::new(x[start..end].to_vec(), y).with_color(series.color);
            if series.dashed {
                line = line.with_dashes(Some((0.0, vec![6.0, 4.0])));
            }
            axes.add_line(line);
        }
        start = end.saturating_add(1);
    }
}

fn percentile_series(
    indexed: &BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket>,
    axis: &[DateTime<Utc>],
    groups: &[GroupKey],
    p50: impl Fn(&MetricBucket) -> Option<f64>,
    p95: impl Fn(&MetricBucket) -> Option<f64>,
    show_p95: bool,
) -> Vec<Series> {
    groups
        .iter()
        .enumerate()
        .flat_map(|(index, group)| {
            let label = group_label(group);
            let color = palette(index);
            let p50_values = smoothed(&values(indexed, axis, group, &p50));
            let p95_values = smoothed(&values(indexed, axis, group, &p95));
            let mut output = Vec::new();
            if p50_values.iter().any(Option::is_some) {
                output.push(Series {
                    label: format!("{label} p50"),
                    color,
                    dashed: false,
                    values: p50_values,
                });
            }
            if show_p95 && p95_values.iter().any(Option::is_some) {
                output.push(Series {
                    label: format!("{label} p95"),
                    color,
                    dashed: true,
                    values: p95_values,
                });
            }
            output
        })
        .collect()
}

fn value_series(
    indexed: &BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket>,
    axis: &[DateTime<Utc>],
    groups: &[GroupKey],
    value: impl Fn(&MetricBucket) -> Option<f64>,
) -> Vec<Series> {
    groups
        .iter()
        .enumerate()
        .filter_map(|(index, group)| {
            let values = smoothed(&values(indexed, axis, group, &value));
            values.iter().any(Option::is_some).then(|| Series {
                label: group_label(group),
                color: palette(index),
                dashed: false,
                values,
            })
        })
        .collect()
}

fn auxiliary_series(
    indexed: &BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket>,
    axis: &[DateTime<Utc>],
    groups: &[GroupKey],
) -> Vec<Series> {
    groups
        .iter()
        .enumerate()
        .flat_map(|(index, group)| {
            let label = group_label(group);
            let color = palette(index);
            [
                (
                    "thinking",
                    false,
                    values(indexed, axis, group, &|row| {
                        positive(row.thinking_tokens_sum)
                    }),
                ),
                (
                    "subagent",
                    true,
                    values(indexed, axis, group, &|row| {
                        positive(row.subagent_tokens_sum)
                    }),
                ),
            ]
            .into_iter()
            .filter_map(move |(suffix, dashed, raw)| {
                let values = smoothed(&raw);
                values.iter().any(Option::is_some).then(|| Series {
                    label: format!("{label} {suffix}"),
                    color,
                    dashed,
                    values,
                })
            })
        })
        .collect()
}

fn stop_reason_series(
    buckets: &[MetricBucket],
    axis: &[DateTime<Utc>],
    groups: &[GroupKey],
) -> Vec<Series> {
    let definitions = [
        ("end_turn", shared::palette::ACCENT_GREEN),
        ("tool_use", shared::palette::ACCENT_BLUE),
        ("max_tokens", shared::palette::ACCENT_RED),
        ("error", shared::palette::ACCENT_PURPLE),
        ("other", shared::palette::MUTED_GRAY),
    ];
    definitions
        .into_iter()
        .filter_map(|(reason, hex)| {
            let values: Vec<Option<f64>> = axis
                .iter()
                .map(|time| {
                    let count = buckets
                        .iter()
                        .filter(|bucket| {
                            bucket.bucket_start == *time && groups.contains(&group_key(bucket))
                        })
                        .flat_map(|bucket| &bucket.stop_reason_counts)
                        .filter(|(key, _)| {
                            if reason == "other" {
                                !matches!(
                                    key.as_str(),
                                    "end_turn" | "tool_use" | "max_tokens" | "error"
                                )
                            } else {
                                key.as_str() == reason
                            }
                        })
                        .map(|(_, count)| *count as f64)
                        .sum::<f64>();
                    Some(count)
                })
                .collect();
            values
                .iter()
                .any(|value| value.is_some_and(|value| value > 0.0))
                .then(|| Series {
                    label: reason.to_string(),
                    color: color(hex),
                    dashed: false,
                    values,
                })
        })
        .collect()
}

fn values(
    indexed: &BTreeMap<(GroupKey, DateTime<Utc>), &MetricBucket>,
    axis: &[DateTime<Utc>],
    group: &GroupKey,
    value: &impl Fn(&MetricBucket) -> Option<f64>,
) -> Vec<Option<f64>> {
    axis.iter()
        .map(|time| {
            indexed
                .get(&(group.clone(), *time))
                .and_then(|row| value(row))
        })
        .collect()
}

fn smoothed(values: &[Option<f64>]) -> Vec<Option<f64>> {
    (0..values.len())
        .map(|index| {
            values[index]?;
            let lo = index.saturating_sub(1);
            let hi = (index + 2).min(values.len());
            let present: Vec<f64> = values[lo..hi].iter().filter_map(|value| *value).collect();
            Some(present.iter().sum::<f64>() / present.len() as f64)
        })
        .collect()
}

fn positive(value: i64) -> Option<f64> {
    (value > 0).then_some(value as f64)
}

fn group_key(bucket: &MetricBucket) -> GroupKey {
    let model = bucket
        .model
        .as_deref()
        .filter(|model| !matches!(*model, "<synthetic>" | "synthetic" | "unknown" | ""))
        .map(str::to_owned);
    (bucket.agent_type, model, bucket.service_tier.clone())
}

fn group_wire_key(group: &GroupKey) -> String {
    format!(
        "{}|{}|{}",
        group.0,
        group.1.as_deref().unwrap_or(""),
        group.2.as_deref().unwrap_or("")
    )
}

fn group_label(group: &GroupKey) -> String {
    let base = group.1.clone().unwrap_or_else(|| match group.0 {
        AgentType::Claude => "Claude".to_string(),
        AgentType::Codex => "Codex".to_string(),
        AgentType::Muse => "Muse".to_string(),
    });
    match group.2.as_deref().filter(|tier| *tier != "standard") {
        Some(tier) => format!("{base} {tier}"),
        None => base,
    }
}

fn palette(index: usize) -> Rgba {
    color(COLORS[index % COLORS.len()])
}

fn color(hex: &str) -> Rgba {
    Rgba::from_hex(hex).unwrap_or(Rgba::WHITE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn bucket(time: DateTime<Utc>) -> MetricBucket {
        MetricBucket {
            bucket_start: time,
            agent_type: AgentType::Claude,
            model: Some("claude-test".to_string()),
            service_tier: Some("standard".to_string()),
            turn_count: 1,
            error_count: 0,
            ttft_p50_ms: Some(100),
            ttft_p95_ms: Some(200),
            throughput_p50_tps: Some(10.0),
            throughput_p95_tps: Some(20.0),
            input_tokens_sum: 10,
            output_tokens_sum: 20,
            cache_read_tokens_sum: 5,
            cache_creation_tokens_sum: 0,
            thinking_tokens_sum: 2,
            subagent_tokens_sum: 1,
            total_cost_usd_sum: Some(0.2),
            stop_reason_counts: BTreeMap::from([("end_turn".to_string(), 1)]),
        }
    }

    #[test]
    fn performance_dashboard_exports_a_valid_portable_figure() {
        let buckets = vec![bucket(Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap())];
        let bytes = build_figure(&buckets, None, false, true).unwrap();
        assert!(bytes.starts_with(b"RZFG"));
        let metadata =
            rizzma::portable::inspect(&bytes, &rizzma::portable::Limits::default()).unwrap();
        assert!(metadata.renderable());
    }
}
