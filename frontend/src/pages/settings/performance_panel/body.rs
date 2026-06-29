//! Body-state rendering for the Performance settings panel.

use yew::prelude::*;

use crate::components::charts::AxisScale;

use super::charts::render_charts;
use super::use_metrics::PerformanceMetrics;
use super::{GroupBy, GroupKey, TimeWindow};

pub(super) fn render_performance_body(
    metrics: &PerformanceMetrics,
    group_by: &GroupBy,
    pairs: &[GroupKey],
    window: TimeWindow,
    axis_scale: AxisScale,
) -> Html {
    if metrics.loading {
        html! {
            <div class="chart-empty">{ "Loading…" }</div>
        }
    } else if let Some(msg) = metrics.error_msg.as_ref() {
        html! {
            <div class="chart-empty">{ msg }</div>
        }
    } else if metrics.buckets.is_empty() {
        html! {
            <div class="chart-empty">
                { "No per-turn metrics in the selected window. Start a session to populate the dashboard." }
            </div>
        }
    } else {
        render_charts(&metrics.buckets, group_by, pairs, window, axis_scale)
    }
}
