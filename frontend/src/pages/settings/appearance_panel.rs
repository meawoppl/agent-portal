use crate::pages::dashboard::{load_rail_orientation, save_rail_orientation, RailOrientation};
use yew::prelude::*;

#[function_component(AppearancePanel)]
pub fn appearance_panel() -> Html {
    let orientation = use_state(load_rail_orientation);

    let set_horizontal = {
        let orientation = orientation.clone();
        Callback::from(move |_: MouseEvent| {
            save_rail_orientation(RailOrientation::Horizontal);
            orientation.set(RailOrientation::Horizontal);
        })
    };

    let set_vertical = {
        let orientation = orientation.clone();
        Callback::from(move |_: MouseEvent| {
            save_rail_orientation(RailOrientation::Vertical);
            orientation.set(RailOrientation::Vertical);
        })
    };

    let is_horizontal = *orientation == RailOrientation::Horizontal;
    let is_vertical = *orientation == RailOrientation::Vertical;

    html! {
        <section class="appearance-section">
            <div class="section-header">
                <h2>{ "Appearance" }</h2>
                <p class="section-description">
                    { "Layout preferences. Saved in this browser." }
                </p>
            </div>

            <div class="appearance-setting">
                <h3>{ "Session pill menu" }</h3>
                <p class="setting-description">
                    { "Place the session pills along the top of the page, or down the left side." }
                </p>
                <div class="orientation-choices">
                    <button
                        class={classes!("orientation-choice", is_horizontal.then_some("active"))}
                        onclick={set_horizontal}
                    >
                        <div class="orientation-preview horizontal">
                            <div class="preview-rail-h"></div>
                            <div class="preview-body"></div>
                        </div>
                        <span class="orientation-label">{ "Horizontal (top)" }</span>
                    </button>
                    <button
                        class={classes!("orientation-choice", is_vertical.then_some("active"))}
                        onclick={set_vertical}
                    >
                        <div class="orientation-preview vertical">
                            <div class="preview-rail-v"></div>
                            <div class="preview-body"></div>
                        </div>
                        <span class="orientation-label">{ "Vertical (left)" }</span>
                    </button>
                </div>
            </div>
        </section>
    }
}
