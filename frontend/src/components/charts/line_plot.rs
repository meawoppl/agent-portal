//! Hand-rolled `<svg>` line-chart component.
//!
//! Built directly on top of the pure helpers in [`super::scale`]. Each chart
//! on the Performance page (throughput, TTFT, cache hit, cost-per-token)
//! mounts one of these with a list of named series; the component computes
//! "nice" y-axis bounds, draws the gridlines + axis labels, and emits one
//! `<polyline>` per series. Width is 100 % of the container via a responsive
//! `viewBox`.
//!
//! No chart library. SVG is plain DOM strings.

use chrono::{DateTime, Utc};
use yew::prelude::*;

use super::scale::{
    nice_y_axis, series_points, time_axis_ticks, y_axis_ticks, BucketKind, TickLabel,
};

/// One labelled line in the chart. `dashed = true` is used for p95 traces
/// drawn alongside the p50 (solid) line for the same series, to keep the
/// legend small.
#[derive(Debug, Clone, PartialEq)]
pub struct LineSeries {
    /// Human-readable label shown in the legend (e.g. `"opus-4-7 p50"`).
    pub label: String,
    /// Hex color (`"#7aa2f7"`).
    pub color: String,
    /// `true` → stroke-dasharray="4,3"; `false` → solid stroke.
    pub dashed: bool,
    /// One value per bucket; the same length as the chart's bucket axis.
    /// `None` is a gap in the line (the renderer breaks the polyline at gaps).
    pub values: Vec<Option<f64>>,
}

#[derive(Properties, PartialEq)]
pub struct LinePlotProps {
    /// Chart title shown above the SVG.
    pub title: String,
    /// y-axis label (e.g. `"tok/s"`, `"seconds"`, `"%"`, `"$ / 1k out"`).
    pub y_label: String,
    /// Ordered bucket-start timestamps. Length must match each series' values
    /// vector length; the renderer asserts in debug.
    pub buckets: Vec<DateTime<Utc>>,
    /// Bucket granularity, used to pick the x-axis label format.
    pub bucket_kind: BucketKind,
    /// Lines to draw. Empty list → renders the "no data" placeholder.
    pub series: Vec<LineSeries>,
}

/// Internal SVG canvas size. We render at 800×260 in the viewBox and let CSS
/// scale to the container width — same approach as the Sparkline.
const VIEW_W: f32 = 800.0;
const VIEW_H: f32 = 260.0;
/// Padding on each side of the plot area to leave room for axis labels.
const PAD_L: f32 = 48.0;
const PAD_R: f32 = 12.0;
const PAD_T: f32 = 12.0;
const PAD_B: f32 = 36.0;

#[function_component(LinePlot)]
pub fn line_plot(props: &LinePlotProps) -> Html {
    if props.buckets.is_empty() || props.series.iter().all(|s| s.values.is_empty()) {
        return html! {
            <div class="performance-chart">
                <h3 class="chart-title">{ &props.title }</h3>
                <div class="chart-empty">{ "No data" }</div>
            </div>
        };
    }

    // Gather all finite y-values across all series for the axis range.
    let mut all_y: Vec<f64> = Vec::new();
    for s in &props.series {
        for v in s.values.iter().flatten().copied() {
            if v.is_finite() {
                all_y.push(v);
            }
        }
    }
    if all_y.is_empty() {
        return html! {
            <div class="performance-chart">
                <h3 class="chart-title">{ &props.title }</h3>
                <div class="chart-empty">{ "No data" }</div>
            </div>
        };
    }
    let (min_v, max_v) = all_y
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |a, &b| {
            (a.0.min(b), a.1.max(b))
        });
    let (y_min, y_max, y_step) = nice_y_axis(min_v, max_v);

    let plot_w = VIEW_W - PAD_L - PAD_R;
    let plot_h = VIEW_H - PAD_T - PAD_B;
    let viewbox = format!("0 0 {VIEW_W} {VIEW_H}");

    let x_ticks = time_axis_ticks(&props.buckets, props.bucket_kind, 6);
    let y_ticks = y_axis_ticks(y_min, y_max, y_step);

    // For each series, split into contiguous runs (skipping `None` gaps), then
    // turn each run into its own polyline via `series_points` against the same
    // y-axis. Lines with `dashed = true` get a `stroke-dasharray`.
    let mut polylines: Vec<Html> = Vec::new();
    for s in &props.series {
        let runs = contiguous_runs(&s.values);
        for (start_idx, run) in runs {
            let scaled_w = (run.len() as f32 - 1.0).max(0.0)
                * (plot_w / (props.buckets.len() as f32 - 1.0).max(1.0));
            let offset_x =
                (start_idx as f32) * (plot_w / (props.buckets.len() as f32 - 1.0).max(1.0));
            let points = series_points(&run, scaled_w.max(0.0), plot_h, y_min, y_max);
            if points.is_empty() {
                continue;
            }
            polylines.push(html! {
                <polyline
                    points={points}
                    fill="none"
                    stroke={s.color.clone()}
                    stroke-width="1.75"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                    stroke-dasharray={if s.dashed { "4,3" } else { "" }.to_string()}
                    transform={format!("translate({:.2},{:.2})", PAD_L + offset_x, PAD_T)}
                />
            });
        }
    }

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
                    <span class="chart-legend-swatch"
                          style={format!(
                              "background: {}; {}",
                              s.color,
                              if s.dashed { "background-image: repeating-linear-gradient(90deg, transparent 0 3px, var(--bg-darker) 3px 6px);" } else { "" }
                          )}>
                    </span>
                    { &s.label }
                </span>
            }
        })
        .collect();

    html! {
        <div class="performance-chart">
            <h3 class="chart-title">{ &props.title }</h3>
            <div class="chart-legend">{ legend }</div>
            <svg
                class="performance-chart-svg"
                viewBox={viewbox}
                preserveAspectRatio="xMidYMid meet"
            >
                <text
                    x="4"
                    y={format!("{:.2}", PAD_T + 4.0)}
                    class="chart-y-axis-title"
                >
                    { &props.y_label }
                </text>
                { gridlines }
                { for polylines }
                { x_labels }
            </svg>
        </div>
    }
}

/// Find every contiguous run of `Some(v)` values in the series. Returns
/// `(start_index, values)` pairs so the caller knows where each run begins
/// along the bucket axis. `None` entries break the run.
fn contiguous_runs(values: &[Option<f64>]) -> Vec<(usize, Vec<f64>)> {
    let mut runs = Vec::new();
    let mut current: Vec<f64> = Vec::new();
    let mut current_start = 0;
    for (i, v) in values.iter().enumerate() {
        match v {
            Some(x) if x.is_finite() => {
                if current.is_empty() {
                    current_start = i;
                }
                current.push(*x);
            }
            _ => {
                if !current.is_empty() {
                    runs.push((current_start, std::mem::take(&mut current)));
                }
            }
        }
    }
    if !current.is_empty() {
        runs.push((current_start, current));
    }
    runs
}

/// Format a y-axis tick value as compactly as possible. Whole numbers drop the
/// decimal; small fractions keep 2 decimals; big values use a `k` suffix.
fn format_axis_value(v: f64) -> String {
    if v.abs() >= 1000.0 {
        format!("{:.1}k", v / 1000.0)
    } else if v.fract().abs() < 1e-9 {
        format!("{}", v as i64)
    } else if v.abs() < 1.0 {
        format!("{:.2}", v)
    } else {
        format!("{:.1}", v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contiguous_runs_splits_on_none() {
        let vals = vec![Some(1.0), Some(2.0), None, Some(4.0), None, Some(6.0)];
        let runs = contiguous_runs(&vals);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].0, 0);
        assert_eq!(runs[0].1, vec![1.0, 2.0]);
        assert_eq!(runs[1].0, 3);
        assert_eq!(runs[1].1, vec![4.0]);
        assert_eq!(runs[2].0, 5);
        assert_eq!(runs[2].1, vec![6.0]);
    }

    #[test]
    fn contiguous_runs_all_some() {
        let vals = vec![Some(1.0), Some(2.0), Some(3.0)];
        let runs = contiguous_runs(&vals);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0], (0, vec![1.0, 2.0, 3.0]));
    }

    #[test]
    fn contiguous_runs_all_none() {
        let vals: Vec<Option<f64>> = vec![None, None];
        assert!(contiguous_runs(&vals).is_empty());
    }

    #[test]
    fn contiguous_runs_treats_nan_as_gap() {
        let vals = vec![Some(1.0), Some(f64::NAN), Some(3.0)];
        let runs = contiguous_runs(&vals);
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn format_axis_value_picks_compact_form() {
        assert_eq!(format_axis_value(0.0), "0");
        assert_eq!(format_axis_value(1.0), "1");
        assert_eq!(format_axis_value(0.25), "0.25");
        assert_eq!(format_axis_value(12.5), "12.5");
        assert_eq!(format_axis_value(1500.0), "1.5k");
    }
}
