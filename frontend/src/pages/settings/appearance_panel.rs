use crate::components::{load_voice_hold_open, save_voice_hold_open};
use crate::pages::dashboard::{
    load_group_by_host, load_nav_mode_shortcut, load_pill_width, load_rail_position, load_vim_mode,
    save_group_by_host, save_nav_mode_shortcut, save_pill_width, save_rail_position, save_vim_mode,
    NavModeShortcut, RailPosition, COMPACT_PILL_WIDTH, ORIGINAL_PILL_WIDTH,
};
use crate::ui_theme::{load_ui_theme, set_ui_theme, UiTheme};
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
    let pill_width = use_state(load_pill_width);
    let on_pill_width = {
        let pill_width = pill_width.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            if let Ok(width) = input.value().parse::<u16>() {
                let width = width.clamp(COMPACT_PILL_WIDTH, ORIGINAL_PILL_WIDTH);
                save_pill_width(width);
                pill_width.set(width);
            }
        })
    };
    let vim_enabled = use_state(load_vim_mode);
    let nav_mode_shortcut = use_state(load_nav_mode_shortcut);
    let on_nav_mode_shortcut = {
        let nav_mode_shortcut = nav_mode_shortcut.clone();
        Callback::from(move |e: Event| {
            let select: web_sys::HtmlSelectElement = e.target_unchecked_into();
            if let Some(shortcut) = NavModeShortcut::from_str(&select.value()) {
                save_nav_mode_shortcut(shortcut);
                nav_mode_shortcut.set(shortcut);
            }
        })
    };
    let group_by_host = use_state(load_group_by_host);
    let voice_hold_open = use_state(load_voice_hold_open);
    let ui_theme = use_state(load_ui_theme);

    let on_toggle_group_by_host = {
        let group_by_host = group_by_host.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let enabled = input.checked();
            save_group_by_host(enabled);
            group_by_host.set(enabled);
        })
    };

    let on_toggle_voice_hold_open = {
        let voice_hold_open = voice_hold_open.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let enabled = input.checked();
            save_voice_hold_open(enabled);
            voice_hold_open.set(enabled);
        })
    };

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
        <div class="section-stack">
        <section class="section-stack">
            <div class="section-header">
                <h2>{ "Appearance" }</h2>
                <p class="section-description">
                    { "Layout preferences. Saved in this browser." }
                </p>
            </div>

            <div class="appearance-setting">
                <h3>{ "Theme" }</h3>
                <p class="setting-description">
                    { "How should Agent Portal look? Applies instantly in this browser." }
                </p>
                <div class="theme-choices" role="radiogroup">
                    { for UiTheme::ALL.into_iter().map(|theme| {
                        let active = *ui_theme == theme;
                        let setter = ui_theme.clone();
                        let onclick = Callback::from(move |_: MouseEvent| {
                            set_ui_theme(theme);
                            setter.set(theme);
                        });
                        html! {
                            <button
                                key={theme.as_str()}
                                type="button"
                                role="radio"
                                aria-checked={active.to_string()}
                                class={classes!("theme-choice", theme.as_str(), active.then_some("active"))}
                                {onclick}
                            >
                                <span class="theme-choice-swatch" aria-hidden="true"></span>
                                <span class="theme-choice-label">{ format!("\u{201c}{}\u{201d}", theme.label()) }</span>
                                <span class="theme-choice-caption">{ theme.caption() }</span>
                            </button>
                        }
                    })}
                </div>
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
                <h3><label for="session-pill-width">{ "Session pill size" }</label></h3>
                <p class="setting-description" id="session-pill-width-help">
                    { "Choose how small top and bottom tabs can shrink. Compact fits more sessions; original keeps the full width. Side rails keep their existing size." }
                </p>
                <div class="pill-size-control">
                    <input id="session-pill-width" type="range"
                        min={COMPACT_PILL_WIDTH.to_string()} max={ORIGINAL_PILL_WIDTH.to_string()}
                        step="1" value={pill_width.to_string()} oninput={on_pill_width}
                        aria-describedby="session-pill-width-help"
                        aria-valuetext={format!("{} pixels minimum", *pill_width)} />
                    <div class="pill-size-labels">
                        <span>{ "Compact" }</span>
                        <output for="session-pill-width">{ format!("{} px", *pill_width) }</output>
                        <span>{ "Original" }</span>
                    </div>
                    <div class="pill-size-preview" aria-hidden="true">
                        <span style={format!("width: {}px", *pill_width)}>{ "My session" }</span>
                    </div>
                </div>
            </div>

            <div class="appearance-setting">
                <h3>{ "Group session rail by host" }</h3>
                <p class="setting-description">
                    { "Split the session rail into sections by the machine each \
                       session runs on. Sections are ordered alphabetically by \
                       hostname; sessions with no hostname group under \
                       \"unknown host\". Off by default." }
                </p>
                <label class="toggle-label">
                    <input
                        type="checkbox"
                        checked={*group_by_host}
                        onchange={on_toggle_group_by_host}
                    />
                    <span>{ if *group_by_host { "Enabled" } else { "Disabled" } }</span>
                </label>
            </div>

            <div class="appearance-setting">
                <h3><label for="nav-mode-shortcut">{ "Nav mode shortcut" }</label></h3>
                <p class="setting-description" id="nav-mode-shortcut-help">
                    { "The key combination that enters and leaves nav mode. \
                       Ctrl+K is also delete-to-end-of-line in macOS and \
                       emacs-style text fields; choose Cmd+K or Alt/Option+K \
                       to keep it for editing. Takes effect immediately." }
                </p>
                <select
                    id="nav-mode-shortcut"
                    aria-describedby="nav-mode-shortcut-help"
                    onchange={on_nav_mode_shortcut}
                >
                    { for NavModeShortcut::ALL.into_iter().map(|shortcut| html! {
                        <option
                            value={shortcut.as_str()}
                            selected={*nav_mode_shortcut == shortcut}
                        >
                            { shortcut.label() }
                        </option>
                    }) }
                </select>
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

            <div class="appearance-setting">
                <h3>{ "Voice input: hold mic open" }</h3>
                <p class="setting-description">
                    { "By default the browser stops listening after a short pause                        in speech, and that silence gap is not adjustable. Enable                        this to keep the mic open across pauses — dictation                        continues until you tap the mic again (or 5 minutes                        elapse). Applies to browser speech recognition;                        server-side transcription already records until tapped.                        Takes effect on the next recording." }
                </p>
                <label class="toggle-label">
                    <input
                        type="checkbox"
                        checked={*voice_hold_open}
                        onchange={on_toggle_voice_hold_open}
                    />
                    <span>{ if *voice_hold_open { "Enabled" } else { "Disabled" } }</span>
                </label>
            </div>
        </section>
        <super::health_timer_panel::HealthTimerPanel />
        </div>
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
