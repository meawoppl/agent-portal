//! Rizzma chart composition for the Performance settings panel.

use yew::prelude::*;

use crate::components::RizzmaChart;

use super::model::{bucket_param, AxisScale, GroupBy, TimeWindow};

/// Render one authenticated `.riz` dashboard containing all six plots. Query
/// controls are part of its URL, so Yew remounts a fresh sandboxed figure
/// whenever they change.
pub(super) fn render_charts(
    group_by: &GroupBy,
    window: TimeWindow,
    axis_scale: AxisScale,
    show_p95: bool,
) -> Html {
    let mut url = format!(
        "/api/metrics/turns/figure?bucket={}&window={}&scale={}&p95={show_p95}",
        bucket_param(window),
        window.label(),
        axis_scale.wire_name(),
    );
    if !matches!(group_by, GroupBy::All) {
        if let Some(group) = js_sys::encode_uri_component(&group_by.key()).as_string() {
            url.push_str("&group=");
            url.push_str(&group);
        }
    }
    html! {
        <div class="performance-charts">
            <RizzmaChart
                key={url.clone()}
                title="Interactive performance trends"
                artifact_url={url.clone()}
            />
        </div>
    }
}
