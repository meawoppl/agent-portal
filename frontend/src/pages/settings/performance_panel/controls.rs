//! Filter and scale controls for the Performance settings panel.

use yew::prelude::*;

use crate::components::charts::AxisScale;

use super::model::{pair_label, GroupBy, GroupKey, TimeWindow};

pub(super) struct PerformanceControlsProps<'a> {
    pub window: TimeWindow,
    pub group_by: &'a GroupBy,
    pub axis_scale: AxisScale,
    pub pairs: &'a [GroupKey],
    pub on_window_change: Callback<TimeWindow>,
    pub on_group_change: Callback<Event>,
    pub on_axis_scale_change: Callback<AxisScale>,
}

pub(super) fn render_performance_controls(props: PerformanceControlsProps<'_>) -> Html {
    html! {
        <div class="performance-controls">
            <div class="performance-window-group" role="radiogroup">
                <span class="performance-control-label">{ "Window:" }</span>
                { for TimeWindow::all().iter().copied().map(|w| {
                    let is_active = props.window == w;
                    let on_window_change = props.on_window_change.clone();
                    let on_click = Callback::from(move |_| on_window_change.emit(w));
                    html! {
                        <button
                            class={classes!(
                                "performance-window-button",
                                is_active.then_some("active"),
                            )}
                            onclick={on_click}
                        >
                            { w.label() }
                        </button>
                    }
                }) }
            </div>

            <div class="performance-group-by">
                <label class="performance-control-label" for="performance-group-by-select">
                    { "Group:" }
                </label>
                <select
                    id="performance-group-by-select"
                    onchange={props.on_group_change}
                    value={props.group_by.key()}
                >
                    <option
                        value="__ALL__"
                        selected={matches!(props.group_by, GroupBy::All)}
                    >
                        { "All groups" }
                    </option>
                    { for props.pairs.iter().map(|pair| {
                        let gb = GroupBy::Pair(pair.clone());
                        let selected = matches!(props.group_by, GroupBy::Pair(p) if p == pair);
                        html! {
                            <option value={gb.key()} selected={selected}>
                                { pair_label(pair) }
                            </option>
                        }
                    }) }
                </select>
            </div>

            <div class="performance-scale-group" role="radiogroup">
                <span class="performance-control-label">{ "Y scale:" }</span>
                { for AxisScale::all().iter().copied().map(|scale| {
                    let is_active = props.axis_scale == scale;
                    let on_axis_scale_change = props.on_axis_scale_change.clone();
                    let on_click = Callback::from(move |_| on_axis_scale_change.emit(scale));
                    html! {
                        <button
                            type="button"
                            class={classes!(
                                "performance-window-button",
                                is_active.then_some("active"),
                            )}
                            onclick={on_click}
                        >
                            { scale.label() }
                        </button>
                    }
                }) }
            </div>
        </div>
    }
}
