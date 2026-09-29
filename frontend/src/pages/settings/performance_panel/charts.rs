//! Static SVG chart composition for the Performance settings panel.

use std::collections::BTreeMap;

use shared::api::MetricBucket;
use yew::prelude::*;

use super::model::{bucket_group_key, pair_label, AxisScale, GroupBy};

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
    let throughput = metric_points(
        &scoped,
        |bucket| bucket.throughput_p50_tps,
        |bucket| bucket.throughput_p95_tps,
    );
    let ttft = metric_points(
        &scoped,
        |bucket| bucket.ttft_p50_ms.map(|v| v as f64 / 1000.0),
        |bucket| bucket.ttft_p95_ms.map(|v| v as f64 / 1000.0),
    );
    let cache = metric_points(
        &scoped,
        |bucket| {
            let total = bucket.cache_read_tokens_sum
                + bucket.cache_creation_tokens_sum
                + bucket.input_tokens_sum;
            (total > 0).then_some(bucket.cache_read_tokens_sum as f64 / total as f64 * 100.0)
        },
        |_| None,
    );
    let cost = metric_points(
        &scoped,
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
        GroupBy::All => "All groups".to_string(),
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

fn metric_points<F, G>(buckets: &[&MetricBucket], value: F, p95: G) -> Vec<Point>
where
    F: Fn(&MetricBucket) -> Option<f64>,
    G: Fn(&MetricBucket) -> Option<f64>,
{
    let mut grouped: BTreeMap<_, Vec<&MetricBucket>> = BTreeMap::new();
    for bucket in buckets {
        grouped
            .entry(bucket.bucket_start)
            .or_default()
            .push(*bucket);
    }
    grouped
        .into_iter()
        .map(|(start, buckets)| Point {
            label: start.format("%m/%d").to_string(),
            value: weighted_average(&buckets, &value),
            p95: weighted_average(&buckets, &p95),
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
    points: &[Point],
    axis_scale: AxisScale,
    show_p95: bool,
) -> Html {
    let values = points
        .iter()
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
    let primary = path_data(points, |point| point.value, min, max, axis_scale);
    let p95 = path_data(points, |point| point.p95, min, max, axis_scale);
    let x_first = points.first().map(|p| p.label.as_str()).unwrap_or_default();
    let x_last = points.last().map(|p| p.label.as_str()).unwrap_or_default();
    let p95_legend = show_p95 && points.iter().any(|point| point.p95.is_some());

    html! {
        <div class="performance-chart">
            <div class="chart-header">
                <h3 class="chart-title">{ title }</h3>
                <span class="chart-scale-badge">{ format!("{} · {}", subtitle, axis_scale.label()) }</span>
            </div>
            <div class="chart-legend">
                <span class="chart-legend-item">
                    <span class="chart-legend-swatch" style={format!("background: {}", shared::palette::ACCENT_BLUE)} />
                    { "p50 / value" }
                </span>
                if p95_legend {
                    <span class="chart-legend-item">
                        <span class="chart-legend-swatch dashed" style={format!("background: {}", shared::palette::ACCENT_ORANGE)} />
                        { "p95" }
                    </span>
                }
            </div>
            <svg class="performance-chart-svg" viewBox={format!("0 0 {WIDTH} {HEIGHT}")} role="img">
                <title>{ format!("{title} performance chart") }</title>
                { for grid_lines(min, max, unit) }
                <text class="chart-x-label" x={LEFT.to_string()} y={(HEIGHT - 10.0).to_string()}>{ x_first }</text>
                <text class="chart-x-label" text-anchor="end" x={(WIDTH - RIGHT).to_string()} y={(HEIGHT - 10.0).to_string()}>{ x_last }</text>
                <text class="chart-y-axis-title" transform={format!("translate(16 {}) rotate(-90)", HEIGHT / 2.0)}>{ unit }</text>
                <path d={primary} fill="none" stroke={shared::palette::ACCENT_BLUE} stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" />
                if p95_legend {
                    <path d={p95} fill="none" stroke={shared::palette::ACCENT_ORANGE} stroke-width="2" stroke-linecap="round" stroke-linejoin="round" stroke-dasharray="6 5" />
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
    if (max - min).abs() < f64::EPSILON {
        let pad = (max.abs() * 0.1).max(1.0);
        (min - pad, max + pad)
    } else {
        let pad = (max - min) * 0.08;
        (min - pad, max + pad)
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
