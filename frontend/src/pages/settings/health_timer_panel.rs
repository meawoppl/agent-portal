use crate::health_timer::{
    load_health_timer_settings, save_health_timer_settings_reset, HealthTimerSettings,
};
use yew::prelude::*;

#[function_component(HealthTimerPanel)]
pub fn health_timer_panel() -> Html {
    let settings = use_state(load_health_timer_settings);

    let persist = {
        let settings = settings.clone();
        Callback::from(move |next: HealthTimerSettings| {
            save_health_timer_settings_reset(&next);
            settings.set(next.normalized());
        })
    };

    let on_enabled_change = {
        let settings = settings.clone();
        let persist = persist.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut next = (*settings).clone();
            next.enabled = input.checked();
            persist.emit(next);
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
        Callback::from(move |e: web_sys::InputEvent| {
            let textarea: web_sys::HtmlTextAreaElement = e.target_unchecked_into();
            let mut next = (*settings).clone();
            next.message = textarea.value();
            persist.emit(next);
        })
    };

    html! {
        <section class="health-settings-section">
            <div class="section-header">
                <h2>{ "Health timer" }</h2>
                <p class="section-description">
                    { "Periodic local reminders. Saved in this browser." }
                </p>
            </div>

            <div class="health-settings-card">
                <label class="toggle-label health-toggle">
                    <input
                        type="checkbox"
                        checked={settings.enabled}
                        onchange={on_enabled_change}
                    />
                    <span>{ if settings.enabled { "Enabled" } else { "Disabled" } }</span>
                </label>

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
                    <textarea
                        rows="4"
                        placeholder="Stop/stretch and take a break"
                        value={settings.message.clone()}
                        oninput={on_message_change}
                    />
                </label>
            </div>
        </section>
    }
}
