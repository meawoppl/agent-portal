//! Local health-break timer.
//!
//! Settings live entirely in browser localStorage. The app-level reminder
//! listens for same-window and cross-window storage changes so editing Settings
//! takes effect immediately without a backend round trip.

use gloo::events::EventListener;
use gloo::timers::callback::Timeout;
use serde::{Deserialize, Serialize};
use yew::prelude::*;

use crate::{
    hooks::use_focus_trap,
    utils::{self, On401},
};
use shared::api::MeResponse;
use std::{cell::Cell, rc::Rc};
use wasm_bindgen_futures::spawn_local;

pub const STORAGE_KEY: &str = "agent_portal.health_timer.v1";

// Logged-out wall time must not become an overdue reminder after login.
const RESET_AFTER_LOGIN_KEY: &str = "agent_portal.health_timer.reset_after_login";

const SETTINGS_CHANGED_EVENT: &str = "agent-portal-health-timer-settings-changed";
const DEFAULT_CADENCE_MINUTES: u32 = 30;
const DEFAULT_MESSAGE: &str = "Time for a quick break.";
const MINUTE_MS: f64 = 60_000.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthTimerSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_cadence_minutes")]
    pub cadence_minutes: u32,
    #[serde(default)]
    pub message: String,
    #[serde(default = "current_timestamp")]
    pub last_confirmed_at: String,
}

impl Default for HealthTimerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            cadence_minutes: DEFAULT_CADENCE_MINUTES,
            message: String::new(),
            last_confirmed_at: current_timestamp(),
        }
    }
}

impl HealthTimerSettings {
    pub fn normalized(mut self) -> Self {
        self.cadence_minutes = self.cadence_minutes.max(1);
        if timestamp_to_ms(&self.last_confirmed_at).is_none() {
            self.last_confirmed_at = current_timestamp();
        }
        self
    }

    pub fn reminder_message(&self) -> &str {
        let trimmed = self.message.trim();
        if trimmed.is_empty() {
            DEFAULT_MESSAGE
        } else {
            trimmed
        }
    }

    fn reset_from_now(mut self) -> Self {
        self.last_confirmed_at = current_timestamp();
        self.normalized()
    }
}

pub fn load_health_timer_settings() -> HealthTimerSettings {
    let Some(raw) = crate::utils::storage_get(STORAGE_KEY) else {
        return HealthTimerSettings::default();
    };
    serde_json::from_str::<HealthTimerSettings>(&raw)
        .map(HealthTimerSettings::normalized)
        .unwrap_or_default()
}

pub fn save_health_timer_settings(settings: &HealthTimerSettings) {
    if let Ok(raw) = serde_json::to_string(&settings.clone().normalized()) {
        crate::utils::storage_set(STORAGE_KEY, &raw);
    }
    notify_settings_changed();
}

pub fn save_health_timer_settings_reset(settings: &HealthTimerSettings) {
    save_health_timer_settings(&settings.clone().reset_from_now());
}

fn default_cadence_minutes() -> u32 {
    DEFAULT_CADENCE_MINUTES
}

#[cfg(not(test))]
fn current_timestamp() -> String {
    js_sys::Date::new_0()
        .to_iso_string()
        .as_string()
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

#[cfg(test)]
fn current_timestamp() -> String {
    "2026-07-19T00:00:00.000Z".to_string()
}

fn now_ms() -> f64 {
    js_sys::Date::now()
}

fn timestamp_to_ms(value: &str) -> Option<f64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp_millis() as f64)
}

fn delay_until_due_ms(settings: &HealthTimerSettings, now: f64) -> u32 {
    let cadence_ms = f64::from(settings.cadence_minutes.max(1)) * MINUTE_MS;
    let last = timestamp_to_ms(&settings.last_confirmed_at).unwrap_or(now);
    let due = last + cadence_ms;
    let delay = (due - now).max(0.0);
    delay.min(f64::from(u32::MAX)).round() as u32
}

fn notify_settings_changed() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(event) = web_sys::Event::new(SETTINGS_CHANGED_EVENT) else {
        return;
    };
    let _ = window.dispatch_event(&event);
}

#[derive(Properties, PartialEq)]
struct HealthTimerDialogProps {
    message: AttrValue,
    on_confirm: Callback<MouseEvent>,
}

/// The modal itself, in its own component so that its hooks (`use_node_ref` +
/// `use_focus_trap`) are only reached when the modal is actually rendered.
///
/// They must **not** live in `HealthTimerReminder`: that component returns early
/// while the reminder is hidden, so hooks placed after the early return run on
/// some renders and not others — Yew's `assert_hook_context` then panics with
/// "Hooks are called conditionally" the moment the timer fires and flips the
/// component onto the visible path. Conditionally rendering a *child component*
/// is the supported way to express this; a conditional hook never is.
#[function_component(HealthTimerDialog)]
fn health_timer_dialog(props: &HealthTimerDialogProps) -> Html {
    let dialog_ref = use_node_ref();
    use_focus_trap(dialog_ref.clone());

    html! {
        <div class="health-timer-overlay" role="presentation">
            <section
                ref={dialog_ref}
                class="health-timer-dialog"
                role="dialog"
                aria-modal="true"
                aria-labelledby="health-timer-title"
            >
                <h2 id="health-timer-title">{ "Health timer" }</h2>
                <p>{ props.message.clone() }</p>
                <button
                    type="button"
                    class="health-timer-confirm"
                    onclick={props.on_confirm.clone()}
                >
                    { "Confirm" }
                </button>
            </section>
        </div>
    }
}

#[function_component(HealthTimerReminder)]
pub fn health_timer_reminder() -> Html {
    let settings = use_state(load_health_timer_settings);
    let showing = use_state(|| false);
    let account = crate::portal_client::use_portal().account_id();
    let active_account = use_state(|| None::<uuid::Uuid>);
    let retry = use_state(|| 0u64);

    {
        let settings = settings.clone();
        let showing = showing.clone();
        let active_account = active_account.clone();
        use_effect_with(account, move |account| {
            showing.set(false);
            if account.is_none() {
                utils::storage_set(RESET_AFTER_LOGIN_KEY, "1");
                save_health_timer_settings_reset(&load_health_timer_settings());
            } else if utils::storage_get(RESET_AFTER_LOGIN_KEY).is_some() {
                utils::storage_remove(RESET_AFTER_LOGIN_KEY);
                save_health_timer_settings_reset(&load_health_timer_settings());
            }
            settings.set(load_health_timer_settings());
            active_account.set(*account);
            || ()
        });
    }

    {
        let settings = settings.clone();
        let showing = showing.clone();
        let active_account = active_account.clone();
        use_effect_with((), move |_| {
            let listeners = web_sys::window().map(|window| {
                let same_window = {
                    let settings = settings.clone();
                    let showing = showing.clone();
                    let active_account = active_account.clone();
                    EventListener::new(&window, SETTINGS_CHANGED_EVENT, move |_| {
                        let loaded = load_health_timer_settings();
                        if utils::storage_get(RESET_AFTER_LOGIN_KEY).is_some() {
                            active_account.set(None);
                            showing.set(false);
                        }
                        if !loaded.enabled {
                            showing.set(false);
                        }
                        settings.set(loaded);
                    })
                };
                let cross_window = {
                    let settings = settings.clone();
                    let showing = showing.clone();
                    let active_account = active_account.clone();
                    EventListener::new(&window, "storage", move |_| {
                        let loaded = load_health_timer_settings();
                        if utils::storage_get(RESET_AFTER_LOGIN_KEY).is_some() {
                            active_account.set(None);
                            showing.set(false);
                        }
                        if !loaded.enabled {
                            showing.set(false);
                        }
                        settings.set(loaded);
                    })
                };

                (same_window, cross_window)
            });

            move || drop(listeners)
        });
    }

    {
        let settings_value = (*settings).clone();
        let is_showing = *showing;
        let showing = showing.clone();
        let active = *active_account;
        let active_account = active_account.clone();
        let retry_value = *retry;
        let retry = retry.clone();
        use_effect_with(
            (settings_value, is_showing, account, active, retry_value),
            move |(settings, is_showing, account, active, _)| {
                let cancelled = Rc::new(Cell::new(false));
                let guard = cancelled.clone();
                let account = *account;
                let timeout = if settings.enabled
                    && !*is_showing
                    && account.is_some()
                    && account == *active
                {
                    let delay = delay_until_due_ms(settings, now_ms());
                    Some(Timeout::new(delay, move || {
                        spawn_local(async move {
                            // The cookie may have expired since app bootstrap.
                            // Never deliver based only on a cached account id.
                            let request =
                                utils::fetch_json::<MeResponse>("/api/auth/me", On401::Ignore);
                            let deadline = gloo::timers::future::TimeoutFuture::new(5_000);
                            futures_util::pin_mut!(request, deadline);
                            let result = match futures_util::future::select(request, deadline).await
                            {
                                futures_util::future::Either::Left((result, _)) => Some(result),
                                futures_util::future::Either::Right(_) => None,
                            };
                            if guard.get() {
                                return;
                            }
                            match result {
                                Some(Ok(me)) if Some(me.id) == account => showing.set(true),
                                Some(Ok(_)) | Some(Err(utils::FetchError::Status(401 | 403))) => {
                                    active_account.set(None);
                                    showing.set(false);
                                    utils::storage_set(RESET_AFTER_LOGIN_KEY, "1");
                                    save_health_timer_settings_reset(&load_health_timer_settings());
                                }
                                _ => {
                                    // A transient outage is not a logout. Keep
                                    // the due time but retry without a hot loop.
                                    gloo::timers::future::TimeoutFuture::new(30_000).await;
                                    if !guard.get() {
                                        retry.set(retry_value.wrapping_add(1));
                                    }
                                }
                            }
                        });
                    }))
                } else {
                    None
                };
                move || {
                    cancelled.set(true);
                    drop(timeout);
                }
            },
        );
    }

    if account.is_none() || account != *active_account || !settings.enabled || !*showing {
        return html! {};
    }

    let message = AttrValue::from(settings.reminder_message().to_string());
    let on_confirm = {
        let settings = settings.clone();
        let showing = showing.clone();
        Callback::from(move |_: MouseEvent| {
            let next = (*settings).clone().reset_from_now();
            save_health_timer_settings(&next);
            settings.set(next);
            showing.set(false);
        })
    };

    html! { <HealthTimerDialog {message} {on_confirm} /> }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_normalize_clamps_cadence_and_repairs_timestamp() {
        let settings = HealthTimerSettings {
            enabled: true,
            cadence_minutes: 0,
            message: String::new(),
            last_confirmed_at: "not a timestamp".to_string(),
        }
        .normalized();

        assert_eq!(settings.cadence_minutes, 1);
        assert!(timestamp_to_ms(&settings.last_confirmed_at).is_some());
    }

    #[test]
    fn blank_message_uses_default() {
        let settings = HealthTimerSettings {
            message: "   ".to_string(),
            ..HealthTimerSettings::default()
        };

        assert_eq!(settings.reminder_message(), DEFAULT_MESSAGE);
    }

    #[test]
    fn due_delay_counts_from_last_confirmation() {
        let settings = HealthTimerSettings {
            enabled: true,
            cadence_minutes: 15,
            message: String::new(),
            last_confirmed_at: "2026-07-19T12:00:00.000Z".to_string(),
        };

        let now = timestamp_to_ms("2026-07-19T12:10:00.000Z").unwrap();
        assert_eq!(delay_until_due_ms(&settings, now), 5 * 60 * 1000);

        let overdue = timestamp_to_ms("2026-07-19T12:16:00.000Z").unwrap();
        assert_eq!(delay_until_due_ms(&settings, overdue), 0);
    }
}
