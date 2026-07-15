use crate::pages::dashboard::{
    load_rail_position, load_vim_mode, save_rail_position, save_vim_mode, RailPosition,
};
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
    let vim_enabled = use_state(load_vim_mode);

    let on_toggle_vim = {
        let vim_enabled = vim_enabled.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let enabled = input.checked();
            save_vim_mode(enabled);
            vim_enabled.set(enabled);
        })
    };

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

            <div class="appearance-setting">
                <h3>{ "Message input: vim mode" }</h3>
                <p class="setting-description">
                    { "Modal (vim-like) editing in the message box: NORMAL/INSERT \
                       modes with h j k l, w b, 0 $, i a o O, x, dd, dw, and Esc. \
                       Starts in INSERT so the box still types normally. Takes \
                       effect on newly opened sessions or after reload." }
                </p>
                <label class="toggle-label">
                    <input
                        type="checkbox"
                        checked={*vim_enabled}
                        onchange={on_toggle_vim}
                    />
                    <span>{ if *vim_enabled { "Enabled" } else { "Disabled" } }</span>
                </label>
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
