use crate::health_timer::{
    load_health_timer_settings, save_health_timer_settings, save_health_timer_settings_reset,
    HealthTimerSettings,
};
use yew::prelude::*;

#[function_component(HealthTimerPanel)]
pub fn health_timer_panel() -> Html {
    let settings = use_state(load_health_timer_settings);

    // Save without touching `last_confirmed_at`. Editing the message fires per
    // keystroke, and resetting here pushed the next reminder further away on
    // every character typed.
    let persist = {
        let settings = settings.clone();
        Callback::from(move |next: HealthTimerSettings| {
            save_health_timer_settings(&next);
            settings.set(next.normalized());
        })
    };

    // Switching the timer on is the one edit that should start the clock.
    let on_enabled_change = {
        let settings = settings.clone();
        Callback::from(move |_: MouseEvent| {
            let mut next = (*settings).clone();
            next.enabled = !next.enabled;
            if next.enabled {
                save_health_timer_settings_reset(&next);
            } else {
                save_health_timer_settings(&next);
            }
            settings.set(next.normalized());
        })
    };

    let on_cadence_change = {
        let settings = settings.clone();
        let persist = persist.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut next = (*settings).clone();
            next.cadence_minutes = input.value().parse::<u32>().unwrap_or(1).max(1);
            persist.emit(next);
        })
    };

    let on_message_change = {
        let settings = settings.clone();
        let persist = persist.clone();
        Callback::from(move |message: String| {
            let mut next = (*settings).clone();
            next.message = message;
            persist.emit(next);
        })
    };

    html! {
        <section class="section-stack health-settings-section">
            <div class="section-header">
                <h2>{ "Health timer" }</h2>
                <p class="section-description">
                    { "Periodic local reminders. Saved in this browser." }
                </p>
            </div>

            <div class="health-settings-card">
                <button
                    type="button"
                    class={classes!(
                        "health-toggle-button",
                        if settings.enabled { "on" } else { "off" }
                    )}
                    aria-pressed={settings.enabled.to_string()}
                    onclick={on_enabled_change}
                >
                    <span class="health-toggle-glyph">
                        { if settings.enabled { "\u{2713}" } else { "\u{2715}" } }
                    </span>
                    { if settings.enabled { "Enabled" } else { "Disabled" } }
                </button>

                <label class="health-setting-field">
                    <span>{ "Reminder interval" }</span>
                    <div class="health-minutes-input">
                        <input
                            type="number"
                            min="1"
                            step="1"
                            value={settings.cadence_minutes.to_string()}
                            onchange={on_cadence_change}
                        />
                        <span>{ "minutes" }</span>
                    </div>
                </label>

                <label class="health-setting-field">
                    <span>{ "Reminder message" }</span>
                    <MessageInput
                        initial={settings.message.clone()}
                        on_input={on_message_change}
                    />
                </label>
            </div>
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct MessageInputProps {
    initial: String,
    on_input: Callback<String>,
}

/// The message box owns its own draft.
///
/// Binding `value` straight to parent state round-trips every keystroke through
/// a re-render, which tears down and rebuilds the DOM node and bounces the caret
/// out — see the focus-stable input note in CLAUDE.md. Re-seed only when the
/// stored value changes from outside (another tab), which is a no-op while typing.
#[function_component(MessageInput)]
fn message_input(props: &MessageInputProps) -> Html {
    let draft = use_state(|| props.initial.clone());

    {
        let draft = draft.clone();
        use_effect_with(props.initial.clone(), move |initial| {
            if *draft != *initial {
                draft.set(initial.clone());
            }
            || ()
        });
    }

    let oninput = {
        let draft = draft.clone();
        let on_input = props.on_input.clone();
        Callback::from(move |e: web_sys::InputEvent| {
            let textarea: web_sys::HtmlTextAreaElement = e.target_unchecked_into();
            let value = textarea.value();
            draft.set(value.clone());
            on_input.emit(value);
        })
    };

    html! {
        <textarea
            rows="4"
            placeholder="Stop/stretch and take a break"
            value={(*draft).clone()}
            {oninput}
        />
    }
}
