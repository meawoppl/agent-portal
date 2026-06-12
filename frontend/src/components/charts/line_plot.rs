//! Hand-rolled `<svg>` line-chart component.
//!
//! Built directly on top of the pure helpers in [`super::scale`] and the
//! shared frame helpers in [`super`]. Each chart on the Performance page
//! (throughput, TTFT, cache hit, cost-per-token) mounts one of these with a
//! list of named series; the component computes "nice" y-axis bounds, draws
//! the gridlines + axis labels, and emits one `<polyline>` per series. Width
//! is 100 % of the container via a responsive `viewBox`.
//!
//! No chart library. SVG is plain DOM strings.

use chrono::{DateTime, Utc};
use yew::prelude::*;

use super::scale::{
    series_points_with_axis, time_axis_ticks, y_axis_for_values, y_axis_tick_labels, AxisScale,
    BucketKind,
};
use super::{
    chart_empty, chart_frame, render_gridlines, render_legend, render_x_labels, PAD_L, PAD_T,
    PLOT_H, PLOT_W,
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
    /// Y-axis projection mode. Defaults are owned by the parent page.
    pub axis_scale: AxisScale,
}

#[function_component(LinePlot)]
pub fn line_plot(props: &LinePlotProps) -> Html {
    if props.buckets.is_empty() || props.series.iter().all(|s| s.values.is_empty()) {
        return chart_empty(&props.title);
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
        return chart_empty(&props.title);
    }
    let y_axis = y_axis_for_values(&all_y, props.axis_scale);

    let x_ticks = time_axis_ticks(&props.buckets, props.bucket_kind, 6);
    let y_ticks = y_axis_tick_labels(&y_axis);

    // For each series, split into contiguous runs (skipping `None` gaps), then
    // turn each run into its own polyline via `series_points` against the same
    // y-axis. Lines with `dashed = true` get a `stroke-dasharray`.
    let mut polylines: Vec<Html> = Vec::new();
    for s in &props.series {
        let runs = contiguous_runs(&s.values);
        for (start_idx, run) in runs {
            let scaled_w = (run.len() as f32 - 1.0).max(0.0)
                * (PLOT_W / (props.buckets.len() as f32 - 1.0).max(1.0));
            let offset_x =
                (start_idx as f32) * (PLOT_W / (props.buckets.len() as f32 - 1.0).max(1.0));
            let points = series_points_with_axis(&run, scaled_w.max(0.0), PLOT_H, &y_axis);
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

    let gridlines = render_gridlines(&y_ticks);
    let x_labels = render_x_labels(&x_ticks);
    let legend = render_legend(props.series.iter().map(|s| {
        (
            s.label.as_str(),
            format!(
                "background: {}; {}",
                s.color,
                if s.dashed { "background-image: repeating-linear-gradient(90deg, transparent 0 3px, var(--bg-darker) 3px 6px);" } else { "" }
            ),
        )
    }));

    chart_frame(
        &props.title,
        props.axis_scale,
        &props.y_label,
        legend,
        html! {
            <>
                { gridlines }
                { for polylines }
                { x_labels }
            </>
        },
    )
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
}
