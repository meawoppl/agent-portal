//! Hand-rolled stacked-area chart, used by the Performance page's stop-reason
//! mix plot.
//!
//! Each band is a `<polygon>` of (x, y) points around the stacked region for
//! one series. Stacks are computed in absolute counts; the y-axis is the total
//! across all bands per bucket.

use chrono::{DateTime, Utc};
use yew::prelude::*;

use super::scale::{
    format_axis_value, time_axis_ticks, value_to_y as scaled_value_to_y, y_axis_for_values,
    y_axis_tick_labels, AxisScale, BucketKind, TickLabel,
};

/// One band in the stacked area. Order matters — bands are stacked in vector
/// order from the bottom upward.
#[derive(Debug, Clone, PartialEq)]
pub struct StackedSeries {
    pub label: String,
    pub color: String,
    /// One non-negative count per bucket; treat `0` as "no contribution this
    /// bucket" rather than a gap (the band still has zero height there).
    pub values: Vec<f64>,
}

#[derive(Properties, PartialEq)]
pub struct StackedAreaProps {
    pub title: String,
    pub y_label: String,
    pub buckets: Vec<DateTime<Utc>>,
    pub bucket_kind: BucketKind,
    pub series: Vec<StackedSeries>,
    pub axis_scale: AxisScale,
}

const VIEW_W: f32 = 800.0;
const VIEW_H: f32 = 260.0;
const PAD_L: f32 = 66.0;
const PAD_R: f32 = 12.0;
const PAD_T: f32 = 12.0;
const PAD_B: f32 = 36.0;

#[function_component(StackedArea)]
pub fn stacked_area(props: &StackedAreaProps) -> Html {
    if props.buckets.is_empty() || props.series.is_empty() {
        return html! {
            <div class="performance-chart">
                <h3 class="chart-title">{ &props.title }</h3>
                <div class="chart-empty">{ "No data" }</div>
            </div>
        };
    }

    // Per-bucket total: sum of all bands at that x position.
    let n_buckets = props.buckets.len();
    let mut totals = vec![0.0_f64; n_buckets];
    for s in &props.series {
        for (i, &v) in s.values.iter().enumerate().take(n_buckets) {
            totals[i] += v.max(0.0);
        }
    }
    let max_total = totals.iter().copied().fold(0.0_f64, f64::max);
    if max_total <= 0.0 {
        return html! {
            <div class="performance-chart">
                <h3 class="chart-title">{ &props.title }</h3>
                <div class="chart-empty">{ "No data" }</div>
            </div>
        };
    }
    let y_axis = y_axis_for_values(&totals, props.axis_scale);

    let plot_w = VIEW_W - PAD_L - PAD_R;
    let plot_h = VIEW_H - PAD_T - PAD_B;
    let viewbox = format!("0 0 {VIEW_W} {VIEW_H}");

    let x_ticks = time_axis_ticks(&props.buckets, props.bucket_kind, 6);
    let y_ticks = y_axis_tick_labels(&y_axis);

    // Build cumulative bottoms per series. `bottoms[i][b]` is the y-stack
    // floor for series i at bucket b — the previous series' top.
    let mut bottoms: Vec<Vec<f64>> = vec![vec![0.0; n_buckets]];
    for s in &props.series[..props.series.len() - 1] {
        let prev = bottoms.last().unwrap().clone();
        let mut next = prev.clone();
        for (i, b) in next.iter_mut().enumerate().take(n_buckets) {
            *b += s.values.get(i).copied().unwrap_or(0.0).max(0.0);
        }
        bottoms.push(next);
    }

    let polygons: Html = props
        .series
        .iter()
        .enumerate()
        .map(|(idx, s)| {
            let bottom = &bottoms[idx];
            let mut tops = bottom.clone();
            for (i, t) in tops.iter_mut().enumerate().take(n_buckets) {
                *t += s.values.get(i).copied().unwrap_or(0.0).max(0.0);
            }
            // x positions: 0..plot_w evenly.
            let last_idx = (n_buckets - 1).max(1) as f32;
            let mut points = String::new();
            // top, left → right
            for (i, &t) in tops.iter().enumerate().take(n_buckets) {
                if !points.is_empty() {
                    points.push(' ');
                }
                let x = PAD_L + (i as f32 / last_idx) * plot_w;
                let y = value_to_y(t, &y_axis, plot_h);
                points.push_str(&format!("{:.2},{:.2}", x, y));
            }
            // bottom, right → left
            for (i, &b) in bottom.iter().enumerate().rev().take(n_buckets) {
                points.push(' ');
                let x = PAD_L + (i as f32 / last_idx) * plot_w;
                let y = value_to_y(b, &y_axis, plot_h);
                points.push_str(&format!("{:.2},{:.2}", x, y));
            }
            html! {
                <polygon
                    points={points}
                    fill={s.color.clone()}
                    fill-opacity="0.85"
                    stroke="none"
                />
            }
        })
        .collect();

    let gridlines: Html = y_ticks
        .iter()
        .map(|(v, frac)| {
            let y = PAD_T + plot_h - frac * plot_h;
            html! {
                <>
                    <line
                        x1={format!("{:.2}", PAD_L)}
                        x2={format!("{:.2}", PAD_L + plot_w)}
                        y1={format!("{:.2}", y)}
                        y2={format!("{:.2}", y)}
                        class="chart-gridline"
                    />
                    <text
                        x={format!("{:.2}", PAD_L - 6.0)}
                        y={format!("{:.2}", y + 4.0)}
                        class="chart-y-label"
                        text-anchor="end"
                    >
                        { format_axis_value(*v) }
                    </text>
                </>
            }
        })
        .collect();

    let x_labels: Html = x_ticks
        .iter()
        .map(|TickLabel { frac, label }| {
            let x = PAD_L + frac * plot_w;
            html! {
                <text
                    x={format!("{:.2}", x)}
                    y={format!("{:.2}", VIEW_H - PAD_B + 18.0)}
                    class="chart-x-label"
                    text-anchor="middle"
                >
                    { label.clone() }
                </text>
            }
        })
        .collect();

    let legend: Html = props
        .series
        .iter()
        .map(|s| {
            html! {
                <span class="chart-legend-item">
                    <span
                        class="chart-legend-swatch"
                        style={format!("background: {};", s.color)}
                    ></span>
                    { &s.label }
                </span>
            }
        })
        .collect();

    html! {
        <div class="performance-chart">
            <div class="chart-header">
                <h3 class="chart-title">{ &props.title }</h3>
                <span class="chart-scale-badge">{ props.axis_scale.label() }</span>
            </div>
            <div class="chart-legend">{ legend }</div>
            <svg
                class="performance-chart-svg"
                viewBox={viewbox}
                preserveAspectRatio="xMidYMid meet"
            >
                <text
                    x={format!("{:.2}", 14.0)}
                    y={format!("{:.2}", PAD_T + plot_h / 2.0)}
                    class="chart-y-axis-title"
                    text-anchor="middle"
                    transform={format!("rotate(-90 14 {:.2})", PAD_T + plot_h / 2.0)}
                >
                    { &props.y_label }
                </text>
                { gridlines }
                { polygons }
                { x_labels }
            </svg>
        </div>
    }
}

fn value_to_y(v: f64, axis: &super::scale::YAxis, plot_h: f32) -> f32 {
    PAD_T + scaled_value_to_y(v, axis, plot_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_count_for_axis() {
        assert_eq!(format_axis_value(0.0), "0");
        assert_eq!(format_axis_value(5.0), "5");
        assert_eq!(format_axis_value(2_500.0), "2.5k");
        assert_eq!(format_axis_value(7.5), "7.5");
    }
}
