//! Hand-rolled SVG chart primitives for the Settings → Performance page.
//!
//! No chart-library dependency — every visual is built directly from
//! `<svg>` / `<polyline>` / `<polygon>` elements and the pure helpers in
//! [`scale`]. Two reusable components:
//!
//! - [`LinePlot`] — `n` line series sharing one (x, y) axis pair; supports
//!   dashed traces for p95 alongside solid p50.
//! - [`StackedArea`] — cumulative-band area chart, used for the stop-reason
//!   mix.
//!
//! Pure math (axis bounds, tick formatting, polyline-point projection) lives
//! in [`scale`]; both components import their helpers from there so they
//! share the same nice-number / time-axis behavior. The shared chart frame
//! (canvas constants, empty card, gridlines, x-axis labels, legend, and the
//! header + svg wrapper) lives in this module so both components render the
//! exact same chrome around their series geometry.

pub mod line_plot;
pub mod scale;
pub mod stacked_area;

pub use line_plot::{LinePlot, LineSeries};
pub use scale::{AxisScale, BucketKind};
pub use stacked_area::{StackedArea, StackedSeries};

use yew::prelude::*;

use scale::{format_axis_value, TickLabel};

/// Internal SVG canvas size. We render at 800×260 in the viewBox and let CSS
/// scale to the container width — same approach as the Sparkline.
const VIEW_W: f32 = 800.0;
const VIEW_H: f32 = 260.0;
/// Padding on each side of the plot area to leave room for axis labels.
const PAD_L: f32 = 66.0;
const PAD_R: f32 = 12.0;
const PAD_T: f32 = 12.0;
const PAD_B: f32 = 36.0;
/// Plot-area size inside the axis-label padding.
const PLOT_W: f32 = VIEW_W - PAD_L - PAD_R;
const PLOT_H: f32 = VIEW_H - PAD_T - PAD_B;

/// The "No data" placeholder card, shown when a chart has nothing to plot.
fn chart_empty(title: &str) -> Html {
    html! {
        <div class="performance-chart">
            <h3 class="chart-title">{ title }</h3>
            <div class="chart-empty">{ "No data" }</div>
        </div>
    }
}

/// Horizontal gridlines plus their y-axis tick labels, one pair per tick.
fn render_gridlines(y_ticks: &[(f64, f32)]) -> Html {
    y_ticks
        .iter()
        .map(|(v, frac)| {
            let y = PAD_T + PLOT_H - frac * PLOT_H;
            html! {
                <>
                    <line
                        x1={format!("{:.2}", PAD_L)}
                        x2={format!("{:.2}", PAD_L + PLOT_W)}
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
        .collect()
}

/// X-axis tick labels along the bottom edge of the plot area.
fn render_x_labels(x_ticks: &[TickLabel]) -> Html {
    x_ticks
        .iter()
        .map(|TickLabel { frac, label }| {
            let x = PAD_L + frac * PLOT_W;
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
        .collect()
}

/// Legend row: one swatch + label per series. Each caller supplies the full
/// swatch `style` string, since swatch styling differs per chart (dashed
/// overlay for p95 lines vs plain fill for stacked bands).
fn render_legend<'a>(items: impl Iterator<Item = (&'a str, String)>) -> Html {
    items
        .map(|(label, style)| {
            html! {
                <span class="chart-legend-item">
                    <span class="chart-legend-swatch" style={style}></span>
                    { label }
                </span>
            }
        })
        .collect()
}

/// The common chart chrome: title + scale badge header, legend row, and the
/// responsive `<svg>` with the rotated y-axis title. `body` is the chart's
/// own geometry (gridlines, series shapes, x labels) rendered inside the svg.
fn chart_frame(
    title: &str,
    axis_scale: AxisScale,
    y_label: &str,
    legend: Html,
    body: Html,
) -> Html {
    let viewbox = format!("0 0 {VIEW_W} {VIEW_H}");
    html! {
        <div class="performance-chart">
            <div class="chart-header">
                <h3 class="chart-title">{ title }</h3>
                <span class="chart-scale-badge">{ axis_scale.label() }</span>
            </div>
            <div class="chart-legend">{ legend }</div>
            <svg
                class="performance-chart-svg"
                viewBox={viewbox}
                preserveAspectRatio="xMidYMid meet"
            >
                <text
                    x={format!("{:.2}", 14.0)}
                    y={format!("{:.2}", PAD_T + PLOT_H / 2.0)}
                    class="chart-y-axis-title"
                    text-anchor="middle"
                    transform={format!("rotate(-90 14 {:.2})", PAD_T + PLOT_H / 2.0)}
                >
                    { y_label }
                </text>
                { body }
            </svg>
        </div>
    }
}
