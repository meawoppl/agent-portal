use crate::pages::dashboard::{load_rail_position, save_rail_position, RailPosition};
use yew::prelude::*;

const ALL_POSITIONS: &[(RailPosition, &str)] = &[
    (RailPosition::Top, "Top"),
    (RailPosition::Bottom, "Bottom"),
    (RailPosition::Left, "Left"),
    (RailPosition::Right, "Right"),
];

#[function_component(AppearancePanel)]
pub fn appearance_panel() -> Html {
    let position = use_state(load_rail_position);

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
                    { "Pick where the session pill rail sits on the dashboard." }
                </p>
                <div class="orientation-choices">
                    { for ALL_POSITIONS.iter().copied().map(|(pos, label)| {
                        let active = *position == pos;
                        let position_setter = position.clone();
                        let on_click = Callback::from(move |_: MouseEvent| {
                            save_rail_position(pos);
                            position_setter.set(pos);
                        });
                        html! {
                            <button
                                key={pos.as_str()}
                                class={classes!("orientation-choice", active.then_some("active"))}
                                onclick={on_click}
                            >
                                <div class={classes!("orientation-preview", pos.as_str())}>
                                    { preview_inner(pos) }
                                </div>
                                <span class="orientation-label">{ label }</span>
                            </button>
                        }
                    })}
                </div>
            </div>
        </section>
    }
}

/// Schematic preview inside each choice button — a tiny mockup of the
/// dashboard showing where the rail (accent stripe) sits relative to
/// the message body (hatched pattern).
fn preview_inner(pos: RailPosition) -> Html {
    let rail_class = match pos {
        RailPosition::Top | RailPosition::Bottom => "preview-rail-h",
        RailPosition::Left | RailPosition::Right => "preview-rail-v",
    };
    html! {
        <>
            <div class={rail_class}></div>
            <div class="preview-body"></div>
        </>
    }
}
