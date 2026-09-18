//! Settings → Performance page.
//!
//! The backend turns the selected metrics window into one portable Rizzma
//! dashboard. The frontend keeps only the query controls and mounts the `.riz`
//! through Portal's verified, sandboxed runtime.

use yew::prelude::*;

mod body;
mod charts;
mod controls;
mod model;
mod use_metrics;
use body::render_performance_body;
use controls::{render_performance_controls, PerformanceControlsProps};
use model::{distinct_pairs, AxisScale, GroupBy, TimeWindow};
use use_metrics::use_performance_metrics;

#[function_component(PerformancePanel)]
pub fn performance_panel() -> Html {
    let window = use_state(|| TimeWindow::Days30);
    let group_by = use_state(|| GroupBy::All);
    let axis_scale = use_state(|| AxisScale::Linear);
    let show_p95 = use_state(|| true);
    let metrics = use_performance_metrics(*window);

    let pairs = distinct_pairs(&metrics.buckets);

    let on_window_change = {
        let window = window.clone();
        Callback::from(move |new_window: TimeWindow| window.set(new_window))
    };
    let on_group_change = {
        let group_by = group_by.clone();
        let pairs = pairs.clone();
        Callback::from(move |event: Event| {
            let target: web_sys::HtmlSelectElement = event.target_unchecked_into();
            group_by.set(GroupBy::from_key(&target.value(), &pairs));
        })
    };
    let on_axis_scale_change = {
        let axis_scale = axis_scale.clone();
        Callback::from(move |scale: AxisScale| axis_scale.set(scale))
    };
    let on_show_p95_change = {
        let show_p95 = show_p95.clone();
        Callback::from(move |_| show_p95.set(!*show_p95))
    };

    html! {
        <section class="section-stack">
            <div class="section-header">
                <h2>{ "Performance" }</h2>
                <p class="section-description">
                    { "Per-turn latency, throughput, cache usage, and cost trends. \
                      Aggregated across all sessions you own." }
                </p>
            </div>

            { render_performance_controls(PerformanceControlsProps {
                window: *window,
                group_by: &group_by,
                axis_scale: *axis_scale,
                show_p95: *show_p95,
                pairs: &pairs,
                on_window_change,
                on_group_change,
                on_axis_scale_change,
                on_show_p95_change,
            }) }

            { render_performance_body(
                &metrics,
                &group_by,
                *window,
                *axis_scale,
                *show_p95,
            ) }
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::model::{bucket_param, AxisScale, GroupBy, TimeWindow};
    use shared::AgentType;

    #[test]
    fn chart_query_controls_have_stable_wire_names() {
        assert_eq!(AxisScale::Linear.wire_name(), "linear");
        assert_eq!(AxisScale::Log.wire_name(), "log");
        assert_eq!(bucket_param(TimeWindow::Hours1), "5m");
        assert_eq!(bucket_param(TimeWindow::Days30), "day");
    }

    #[test]
    fn selected_group_round_trips_for_the_figure_endpoint() {
        let pairs = vec![(
            AgentType::Claude,
            Some("claude-opus-4-7".to_string()),
            Some("priority".to_string()),
        )];
        let selected = GroupBy::Pair(pairs[0].clone());
        assert_eq!(GroupBy::from_key(&selected.key(), &pairs), selected);
    }
}
