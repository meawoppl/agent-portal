// TODO(#1165): remove this file-local ratchet after replacing production unwrap/expect paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Hand-rolled stacked-area chart, used by the Performance page's stop-reason
//! mix plot.
//!
//! Each band is a `<polygon>` of (x, y) points around the stacked region for
//! one series. Stacks are computed in absolute counts; the y-axis is the total
//! across all bands per bucket. The chart chrome (frame, gridlines, labels,
//! legend) comes from the shared helpers in [`super`].

use chrono::{DateTime, Utc};
use yew::prelude::*;

use super::scale::{
    time_axis_ticks, value_to_y, y_axis_for_values, y_axis_tick_labels, AxisScale, BucketKind,
};
use super::{
    chart_empty, chart_frame, render_gridlines, render_legend, render_x_labels, PAD_L, PAD_T,
    PLOT_H, PLOT_W,
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

#[function_component(StackedArea)]
pub fn stacked_area(props: &StackedAreaProps) -> Html {
    if props.buckets.is_empty() || props.series.is_empty() {
        return chart_empty(&props.title);
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
        return chart_empty(&props.title);
    }
    let y_axis = y_axis_for_values(&totals, props.axis_scale);

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
            // x positions: 0..PLOT_W evenly.
            let last_idx = (n_buckets - 1).max(1) as f32;
            let mut points = String::new();
            // top, left → right
            for (i, &t) in tops.iter().enumerate().take(n_buckets) {
                if !points.is_empty() {
                    points.push(' ');
                }
                let x = PAD_L + (i as f32 / last_idx) * PLOT_W;
                let y = PAD_T + value_to_y(t, &y_axis, PLOT_H);
                points.push_str(&format!("{:.2},{:.2}", x, y));
            }
            // bottom, right → left
            for (i, &b) in bottom.iter().enumerate().rev().take(n_buckets) {
                points.push(' ');
                let x = PAD_L + (i as f32 / last_idx) * PLOT_W;
                let y = PAD_T + value_to_y(b, &y_axis, PLOT_H);
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

    let gridlines = render_gridlines(&y_ticks);
    let x_labels = render_x_labels(&x_ticks);
    let legend = render_legend(
        props
            .series
            .iter()
            .map(|s| (s.label.as_str(), format!("background: {};", s.color))),
    );

    chart_frame(
        &props.title,
        props.axis_scale,
        &props.y_label,
        legend,
        html! {
            <>
                { gridlines }
                { polygons }
                { x_labels }
            </>
        },
    )
}
