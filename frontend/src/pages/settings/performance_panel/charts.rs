//! Static SVG chart composition for the Performance settings panel.

use std::collections::{BTreeMap, BTreeSet};

use shared::api::MetricBucket;
use yew::prelude::*;

use super::model::{bucket_group_key, pair_label, AxisScale, GroupBy, GroupKey};

const WIDTH: f64 = 960.0;
const HEIGHT: f64 = 260.0;
const LEFT: f64 = 54.0;
const RIGHT: f64 = 18.0;
const TOP: f64 = 22.0;
const BOTTOM: f64 = 38.0;

#[derive(Clone)]
struct Point {
    label: String,
    value: Option<f64>,
    p95: Option<f64>,
}

#[derive(Clone)]
struct Series {
    label: String,
    color: &'static str,
    points: Vec<Point>,
}

const SERIES_COLORS: &[&str] = &[
    shared::palette::ACCENT_BLUE,
    shared::palette::ACCENT_PURPLE,
    shared::palette::ACCENT_GREEN,
    shared::palette::ACCENT_ORANGE,
    shared::palette::ACCENT_RED,
    shared::palette::ACCENT_TEAL,
    "#ff9e64",
];

/// Render a static dashboard. The controls are local projections over the
/// already-fetched metrics response, so no chart endpoint or executable figure
/// runtime is needed.
pub(super) fn render_charts(
    buckets: &[MetricBucket],
    group_by: &GroupBy,
    axis_scale: AxisScale,
    show_p95: bool,
) -> Html {
    let scoped = scoped_buckets(buckets, group_by);
    let throughput = metric_series(
        &scoped,
        group_by,
        |bucket| bucket.throughput_p50_tps,
        |bucket| bucket.throughput_p95_tps,
    );
    let ttft = metric_series(
        &scoped,
        group_by,
        |bucket| bucket.ttft_p50_ms.map(|v| v as f64 / 1000.0),
        |bucket| bucket.ttft_p95_ms.map(|v| v as f64 / 1000.0),
    );
    let cache = metric_series(
        &scoped,
        group_by,
        |bucket| {
            let total = bucket.cache_read_tokens_sum
                + bucket.cache_creation_tokens_sum
                + bucket.input_tokens_sum;
            (total > 0).then_some(bucket.cache_read_tokens_sum as f64 / total as f64 * 100.0)
        },
        |_| None,
    );
    let cost = metric_series(
        &scoped,
        group_by,
        |bucket| {
            bucket
                .total_cost_usd_sum
                .filter(|cost| *cost > 0.0)
                .and_then(|cost| {
                    (bucket.output_tokens_sum > 0)
                        .then_some(cost / bucket.output_tokens_sum as f64 * 1000.0)
                })
        },
        |_| None,
    );

    let subtitle = match group_by {
        GroupBy::All => "Per model".to_string(),
        GroupBy::Pair(pair) => pair_label(pair),
    };

    html! {
        <div class="performance-charts">
            { render_chart("Throughput", "tok/s", &subtitle, &throughput, axis_scale, show_p95) }
            { render_chart("Time to first token", "seconds", &subtitle, &ttft, axis_scale, show_p95) }
            { render_chart("Cache hit rate", "%", &subtitle, &cache, axis_scale, false) }
            { render_chart("Cost per 1k output tokens", "USD", &subtitle, &cost, axis_scale, false) }
        </div>
    }
}

fn scoped_buckets<'a>(buckets: &'a [MetricBucket], group_by: &GroupBy) -> Vec<&'a MetricBucket> {
    buckets
        .iter()
        .filter(|bucket| match group_by {
            GroupBy::All => true,
            GroupBy::Pair(pair) => bucket_group_key(bucket) == *pair,
        })
        .collect()
}

fn metric_series<F, G>(
    buckets: &[&MetricBucket],
    group_by: &GroupBy,
    value: F,
    p95: G,
) -> Vec<Series>
where
    F: Fn(&MetricBucket) -> Option<f64>,
    G: Fn(&MetricBucket) -> Option<f64>,
{
    let axis = buckets
        .iter()
        .map(|bucket| bucket.bucket_start)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let groups = match group_by {
        GroupBy::All => buckets
            .iter()
            .map(|bucket| bucket_group_key(bucket))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>(),
        GroupBy::Pair(pair) => vec![pair.clone()],
    };

    let mut grouped: BTreeMap<(GroupKey, _), Vec<&MetricBucket>> = BTreeMap::new();
    for bucket in buckets {
        grouped
            .entry((bucket_group_key(bucket), bucket.bucket_start))
            .or_default()
            .push(*bucket);
    }

    groups
        .into_iter()
        .enumerate()
        .filter_map(|(index, group)| {
            let points = axis
                .iter()
                .map(|start| {
                    let buckets = grouped
                        .get(&(group.clone(), *start))
                        .map(Vec::as_slice)
                        .unwrap_or(&[]);
                    Point {
                        label: start.format("%m/%d").to_string(),
                        value: weighted_average(buckets, &value),
                        p95: weighted_average(buckets, &p95),
                    }
                })
                .collect::<Vec<_>>();
            points
                .iter()
                .any(|point| point.value.is_some() || point.p95.is_some())
                .then(|| Series {
                    label: pair_label(&group),
                    color: SERIES_COLORS[index % SERIES_COLORS.len()],
                    points,
                })
        })
        .collect()
}

fn weighted_average<F>(buckets: &[&MetricBucket], project: &F) -> Option<f64>
where
    F: Fn(&MetricBucket) -> Option<f64>,
{
    let mut weighted_sum = 0.0;
    let mut weight_sum = 0.0;
    for bucket in buckets {
        let Some(value) = project(bucket).filter(|value| value.is_finite()) else {
            continue;
        };
        let weight = bucket.turn_count.max(1) as f64;
        weighted_sum += value * weight;
        weight_sum += weight;
    }
    (weight_sum > 0.0).then_some(weighted_sum / weight_sum)
}

fn render_chart(
    title: &'static str,
    unit: &'static str,
    subtitle: &str,
    series: &[Series],
    axis_scale: AxisScale,
    show_p95: bool,
) -> Html {
    let values = series
        .iter()
        .flat_map(|item| &item.points)
        .flat_map(|point| [point.value, show_p95.then_some(point.p95).flatten()])
        .flatten()
        .filter(|value| value.is_finite())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return html! {
            <div class="performance-chart">
                <div class="chart-header">
                    <h3 class="chart-title">{ title }</h3>
                    <span class="chart-scale-badge">{ subtitle }</span>
                </div>
                <div class="chart-empty">{ "No data for this metric in the selected window." }</div>
            </div>
        };
    }

    let (min, max) = value_bounds(&values);
    let points = series
        .first()
        .map(|item| item.points.as_slice())
        .unwrap_or(&[]);
    let x_first = points.first().map(|p| p.label.as_str()).unwrap_or_default();
    let x_last = points.last().map(|p| p.label.as_str()).unwrap_or_default();
    let p95_legend = show_p95
        && series
            .iter()
            .flat_map(|item| &item.points)
            .any(|point| point.p95.is_some());

    html! {
        <div class="performance-chart">
            <div class="chart-header">
                <h3 class="chart-title">{ title }</h3>
                <span class="chart-scale-badge">{ format!("{} · {}", subtitle, axis_scale.label()) }</span>
            </div>
            <div class="chart-legend">
                { for series.iter().map(|item| html! {
                    <span class="chart-legend-item">
                        <span class="chart-legend-swatch" style={format!("background: {}", item.color)} />
                        { item.label.as_str() }
                    </span>
                }) }
                if p95_legend {
                    <span class="chart-legend-item">
                        <span class="chart-legend-swatch dashed" />
                        { "p95 dashed" }
                    </span>
                }
            </div>
            <svg class="performance-chart-svg" viewBox={format!("0 0 {WIDTH} {HEIGHT}")} role="img">
                <title>{ format!("{title} performance chart") }</title>
                { for grid_lines(min, max, unit) }
                <text class="chart-x-label" x={LEFT.to_string()} y={(HEIGHT - 10.0).to_string()}>{ x_first }</text>
                <text class="chart-x-label" text-anchor="end" x={(WIDTH - RIGHT).to_string()} y={(HEIGHT - 10.0).to_string()}>{ x_last }</text>
                <text class="chart-y-axis-title" transform={format!("translate(16 {}) rotate(-90)", HEIGHT / 2.0)}>{ unit }</text>
                { for series.iter().map(|item| {
                    let primary = path_data(&item.points, |point| point.value, min, max, axis_scale);
                    html! {
                        <path d={primary} fill="none" stroke={item.color} stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" />
                    }
                }) }
                if p95_legend {
                    { for series
                        .iter()
                        .filter(|item| item.points.iter().any(|point| point.p95.is_some()))
                        .map(|item| {
                            let p95 = path_data(&item.points, |point| point.p95, min, max, axis_scale);
                            html! {
                                <path d={p95} fill="none" stroke={item.color} stroke-width="2" stroke-linecap="round" stroke-linejoin="round" stroke-dasharray="6 5" />
                            }
                    }) }
                }
            </svg>
        </div>
    }
}

fn value_bounds(values: &[f64]) -> (f64, f64) {
    let min = values
        .iter()
        .fold(f64::INFINITY, |acc, value| acc.min(*value));
    let max = values
        .iter()
        .fold(f64::NEG_INFINITY, |acc, value| acc.max(*value));
    if min >= 0.0 {
        return (0.0, nonnegative_axis_upper(max));
    }
    if (max - min).abs() < f64::EPSILON {
        let pad = (max.abs() * 0.1).max(1.0);
        (min - pad, max + pad)
    } else {
        let pad = (max - min) * 0.08;
        (min - pad, max + pad)
    }
}

fn nonnegative_axis_upper(max: f64) -> f64 {
    if max.is_finite() && max > 0.0 {
        max * 1.08
    } else {
        1.0
    }
}

fn path_data<F>(points: &[Point], project: F, min: f64, max: f64, scale: AxisScale) -> String
where
    F: Fn(&Point) -> Option<f64>,
{
    let mut path = String::new();
    let denom = (points.len().saturating_sub(1)).max(1) as f64;
    let plot_width = WIDTH - LEFT - RIGHT;
    for (index, point) in points.iter().enumerate() {
        let Some(value) = project(point).filter(|value| value.is_finite()) else {
            continue;
        };
        let x = LEFT + plot_width * index as f64 / denom;
        let y = y_for(value, min, max, scale);
        if path.is_empty() {
            path.push_str(&format!("M {x:.2} {y:.2}"));
        } else {
            path.push_str(&format!(" L {x:.2} {y:.2}"));
        }
    }
    path
}

fn y_for(value: f64, min: f64, max: f64, scale: AxisScale) -> f64 {
    let transform = |v: f64| match scale {
        AxisScale::Linear => v,
        AxisScale::Log => (v.max(0.0) + 1.0).log10(),
    };
    let min = transform(min);
    let max = transform(max);
    let value = transform(value);
    let span = (max - min).abs().max(f64::EPSILON);
    let frac = ((value - min) / span).clamp(0.0, 1.0);
    let plot_height = HEIGHT - TOP - BOTTOM;
    TOP + plot_height * (1.0 - frac)
}

fn grid_lines(min: f64, max: f64, unit: &'static str) -> Vec<Html> {
    (0..=3)
        .map(|index| {
            let frac = index as f64 / 3.0;
            let y = TOP + (HEIGHT - TOP - BOTTOM) * frac;
            let value = max - (max - min) * frac;
            html! {
                <>
                    <line class="chart-gridline" x1={LEFT.to_string()} x2={(WIDTH - RIGHT).to_string()} y1={y.to_string()} y2={y.to_string()} />
                    <text class="chart-y-label" x="8" y={(y + 4.0).to_string()}>{ format_value(value, unit) }</text>
                </>
            }
        })
        .collect()
}

fn format_value(value: f64, unit: &str) -> String {
    if unit == "USD" {
        format!("${value:.3}")
    } else if value.abs() >= 100.0 {
        format!("{value:.0}")
    } else if value.abs() >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::MetricBucketBuilder;
    use chrono::TimeZone;
    use shared::AgentType;

    fn bucket(model: &str, throughput: f64, turns: i64) -> MetricBucket {
        let mut bucket =
            MetricBucketBuilder::new(chrono::Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap())
                .agent_type(AgentType::Claude)
                .model(Some(model))
                .service_tier(Some("standard"))
                .throughput_p50(Some(throughput))
                .build();
        bucket.turn_count = turns;
        bucket.throughput_p95_tps = Some(throughput * 2.0);
        bucket
    }

    #[test]
    fn nonnegative_values_use_zero_baseline() {
        let (min, max) = value_bounds(&[4.0, 7.0, 10.0]);

        assert_eq!(min, 0.0);
        assert!(max > 10.0);
    }

    #[test]
    fn zero_values_keep_visible_headroom() {
        assert_eq!(value_bounds(&[0.0, 0.0]), (0.0, 1.0));
    }

    #[test]
    fn negative_values_keep_padded_bounds() {
        let (min, max) = value_bounds(&[-2.0, 6.0]);

        assert!(min < -2.0);
        assert!(max > 6.0);
    }

    #[test]
    fn all_groups_render_one_series_per_model() {
        let opus = bucket("claude-opus-test", 10.0, 1);
        let sonnet = bucket("claude-sonnet-test", 30.0, 5);
        let buckets = vec![&opus, &sonnet];

        let series = metric_series(
            &buckets,
            &GroupBy::All,
            |bucket| bucket.throughput_p50_tps,
            |bucket| bucket.throughput_p95_tps,
        );

        assert_eq!(series.len(), 2);
        assert!(series
            .iter()
            .any(|series| series.label.contains("claude-opus-test")));
        assert!(series
            .iter()
            .any(|series| series.label.contains("claude-sonnet-test")));
        assert_eq!(series[0].points[0].value, Some(10.0));
        assert_eq!(series[1].points[0].value, Some(30.0));
    }

    #[test]
    fn selected_group_renders_only_that_model() {
        let opus = bucket("claude-opus-test", 10.0, 1);
        let sonnet = bucket("claude-sonnet-test", 30.0, 5);
        let buckets = vec![&opus, &sonnet];
        let selected = GroupBy::Pair(bucket_group_key(&sonnet));

        let series = metric_series(
            &buckets,
            &selected,
            |bucket| bucket.throughput_p50_tps,
            |bucket| bucket.throughput_p95_tps,
        );

        assert_eq!(series.len(), 1);
        assert!(series[0].label.contains("claude-sonnet-test"));
        assert_eq!(series[0].points[0].value, Some(30.0));
    }
}
