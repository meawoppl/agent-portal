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
//! share the same nice-number / time-axis behavior.

pub mod line_plot;
pub mod scale;
pub mod stacked_area;

pub use line_plot::{LinePlot, LineSeries};
pub use scale::{AxisScale, BucketKind};
pub use stacked_area::{StackedArea, StackedSeries};
