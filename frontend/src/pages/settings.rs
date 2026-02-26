use crate::audio::{self, EventSound, SoundConfig, SoundEvent, Waveform, STORAGE_KEY};
use crate::components::{ProxyTokenSetup, ShareDialog};
use crate::utils;
use gloo_net::http::Request;
use shared::{
    CreateProxyTokenRequest, CreateProxyTokenResponse, ProxyTokenInfo, ProxyTokenListResponse,
    SessionInfo,
};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Settings page tabs
#[derive(Clone, Copy, PartialEq)]
enum SettingsTab {
    Sessions,
    Tokens,
    Sounds,
}

/// Calculate days until expiration from ISO date string
fn days_until_expiration(expires_at: &str) -> Option<i64> {
    // Parse ISO date and compare with current time
    // expires_at format: "2026-02-14T19:58:20.769821Z" or similar
    let now = js_sys::Date::now();
    let expires = js_sys::Date::parse(expires_at);
    if expires.is_nan() {
        return None;
    }
    let diff_ms = expires - now;
    let diff_days = (diff_ms / (1000.0 * 60.0 * 60.0 * 24.0)).floor() as i64;
    Some(diff_days)
}

/// Token row component
#[derive(Properties, PartialEq)]
struct TokenRowProps {
    token: ProxyTokenInfo,
    on_revoke: Callback<Uuid>,
}

#[function_component(TokenRow)]
fn token_row(props: &TokenRowProps) -> Html {
    let token = &props.token;
    let on_revoke = props.on_revoke.clone();
    let token_id = token.id;

    let days_left = days_until_expiration(&token.expires_at);
    let is_expired = days_left.map(|d| d < 0).unwrap_or(false);
    let is_expiring_soon = days_left.map(|d| (0..=7).contains(&d)).unwrap_or(false);

    let status_class = if token.revoked {
        "token-status revoked"
    } else if is_expired {
        "token-status expired"
    } else if is_expiring_soon {
        "token-status expiring-soon"
    } else {
        "token-status active"
    };

    let status_text = if token.revoked {
        "Revoked".to_string()
    } else if is_expired {
        "Expired".to_string()
    } else if let Some(days) = days_left {
        if days == 0 {
            "Expires today".to_string()
        } else if days == 1 {
            "Expires tomorrow".to_string()
        } else if days <= 7 {
            format!("Expires in {} days", days)
        } else {
            "Active".to_string()
        }
    } else {
        "Active".to_string()
    };

    let on_revoke_click = Callback::from(move |_| {
        on_revoke.emit(token_id);
    });

    html! {
        <tr class={if token.revoked || is_expired { "token-row disabled" } else { "token-row" }}>
            <td class="token-name">{ &token.name }</td>
            <td class="token-created">{ utils::format_timestamp(&token.created_at) }</td>
            <td class="token-last-used">
                { token.last_used_at.as_ref().map(|t| utils::format_timestamp(t)).unwrap_or_else(|| "Never".to_string()) }
            </td>
            <td class="token-expires">{ utils::format_timestamp(&token.expires_at) }</td>
            <td class={status_class}>{ status_text }</td>
            <td class="token-actions">
                if !token.revoked && !is_expired {
                    <button class="revoke-button" onclick={on_revoke_click}>
                        { "Revoke" }
                    </button>
                }
            </td>
        </tr>
    }
}

/// Session row component
#[derive(Properties, PartialEq)]
struct SessionRowProps {
    session: SessionInfo,
    on_delete: Callback<Uuid>,
    on_share: Callback<Uuid>,
}

#[function_component(SessionRow)]
fn session_row(props: &SessionRowProps) -> Html {
    let session = &props.session;
    let on_delete = props.on_delete.clone();
    let on_share = props.on_share.clone();
    let session_id = session.id;

    let status_class = match session.status {
        shared::SessionStatus::Active => "session-status active",
        shared::SessionStatus::Inactive => "session-status inactive",
        shared::SessionStatus::Disconnected => "session-status disconnected",
    };

    let on_delete_click = Callback::from(move |_| {
        on_delete.emit(session_id);
    });

    let session_id_for_share = session.id;
    let on_share_click = Callback::from(move |_| {
        on_share.emit(session_id_for_share);
    });

    let project = utils::extract_folder(&session.working_directory);
    let hostname = &session.hostname;

    // Only owners can share
    let is_owner = session.my_role == "owner";

    // Format session ID as short form (first 8 chars)
    let short_id = &session.id.to_string()[..8];

    html! {
        <tr class="session-row">
            <td class="session-name" title={session.session_name.clone()}>{ project }</td>
            <td class="session-id" title={session.id.to_string()}>{ short_id }</td>
            <td class="session-hostname">{ hostname }</td>
            <td class="session-directory" title={session.working_directory.clone()}>
                { if session.working_directory.is_empty() { "—" } else { &session.working_directory } }
            </td>
            <td class="session-branch">
                { session.git_branch.as_deref().unwrap_or("—") }
            </td>
            <td class="session-activity">{ utils::format_timestamp(&session.last_activity) }</td>
            <td class="session-created">{ utils::format_timestamp(&session.created_at) }</td>
            <td class={status_class}>{ session.status.as_str() }</td>
            <td class="session-actions">
                if is_owner {
                    <button class="share-button" onclick={on_share_click} title="Share session">
                        { "Share" }
                    </button>
                }
                <button class="delete-button" onclick={on_delete_click}>
                    { "Delete" }
                </button>
            </td>
        </tr>
    }
}

const ALL_EVENTS: [SoundEvent; 4] = [
    SoundEvent::Activity,
    SoundEvent::Error,
    SoundEvent::SessionSwap,
    SoundEvent::AwaitingInput,
];

#[function_component(SoundsPanel)]
fn sounds_panel() -> Html {
    let config = use_state(SoundConfig::default);
    let dirty = use_state(|| false);
    let saving = use_state(|| false);
    let save_feedback = use_state(|| None::<&'static str>);
    let loading = use_state(|| true);

    // Fetch from API on mount
    {
        let config = config.clone();
        let loading = loading.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                let url = utils::api_url("/api/settings/sound");
                if let Ok(resp) = Request::get(&url).send().await {
                    if let Ok(data) = resp.json::<shared::SoundSettingsResponse>().await {
                        if let Some(json) = data.sound_config {
                            if let Ok(cfg) = serde_json::from_value::<SoundConfig>(json) {
                                // Sync to localStorage for play_sound() runtime
                                if let Ok(s) = serde_json::to_string(&cfg) {
                                    if let Some(storage) = web_sys::window()
                                        .and_then(|w| w.local_storage().ok())
                                        .flatten()
                                    {
                                        let _ = storage.set_item(STORAGE_KEY, &s);
                                    }
                                }
                                config.set(cfg);
                            }
                        }
                    }
                }
                loading.set(false);
            });
        });
    }

    let on_change = {
        let config = config.clone();
        let dirty = dirty.clone();
        let save_feedback = save_feedback.clone();
        Callback::from(move |new_config: SoundConfig| {
            config.set(new_config);
            dirty.set(true);
            save_feedback.set(None);
        })
    };

    let on_toggle = {
        let config = config.clone();
        let on_change = on_change.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut cfg = (*config).clone();
            cfg.enabled = input.checked();
            on_change.emit(cfg);
        })
    };

    let on_save = {
        let config = config.clone();
        let dirty = dirty.clone();
        let saving = saving.clone();
        let save_feedback = save_feedback.clone();
        Callback::from(move |_: MouseEvent| {
            let config = config.clone();
            let dirty = dirty.clone();
            let saving = saving.clone();
            let save_feedback = save_feedback.clone();
            spawn_local(async move {
                saving.set(true);
                let cfg = (*config).clone();
                let url = utils::api_url("/api/settings/sound");
                let json = serde_json::to_value(&cfg).unwrap_or_default();
                let result = Request::put(&url)
                    .json(&json)
                    .expect("json body")
                    .send()
                    .await;
                match result {
                    Ok(resp) if resp.ok() => {
                        // Update localStorage so play_sound() uses new settings
                        if let Ok(s) = serde_json::to_string(&cfg) {
                            if let Some(storage) = web_sys::window()
                                .and_then(|w| w.local_storage().ok())
                                .flatten()
                            {
                                let _ = storage.set_item(STORAGE_KEY, &s);
                            }
                        }
                        dirty.set(false);
                        save_feedback.set(Some("Saved!"));
                    }
                    _ => {
                        save_feedback.set(Some("Save failed"));
                    }
                }
                saving.set(false);
            });
        })
    };

    if *loading {
        return html! {
            <section class="sounds-section">
                <div class="loading">
                    <div class="spinner"></div>
                    <p>{ "Loading sound settings..." }</p>
                </div>
            </section>
        };
    }

    html! {
        <section class="sounds-section">
            <div class="section-header">
                <h2>{ "Sound Notifications" }</h2>
                <p class="section-description">
                    { "Configure synthesized sounds for different events." }
                </p>
                <div class="sound-save-area">
                    if let Some(msg) = *save_feedback {
                        <span class={classes!(
                            "save-feedback",
                            (*dirty).then_some("unsaved"),
                            (!*dirty).then_some("saved"),
                        )}>{ msg }</span>
                    }
                    <button
                        class={classes!("create-button", (!*dirty).then_some("disabled"))}
                        onclick={on_save}
                        disabled={!*dirty || *saving}
                    >
                        { if *saving { "Saving..." } else { "Save" } }
                    </button>
                </div>
            </div>

            <div class="sound-toggle">
                <label class="toggle-label">
                    <span>{ "Enable Sounds" }</span>
                    <input
                        type="checkbox"
                        checked={config.enabled}
                        onchange={on_toggle}
                    />
                </label>
            </div>

            <div class="sound-event-cards">
                { for ALL_EVENTS.iter().map(|&event| {
                    let sound = config.get_sound(event).clone();
                    let config = config.clone();
                    let on_change = on_change.clone();
                    let on_sound_change = Callback::from(move |new_sound: EventSound| {
                        let mut cfg = (*config).clone();
                        cfg.set_sound(event, new_sound);
                        on_change.emit(cfg);
                    });
                    html! {
                        <EventCard
                            key={event.label()}
                            event={event}
                            sound={sound}
                            on_change={on_sound_change}
                        />
                    }
                }) }
            </div>
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct EventCardProps {
    event: SoundEvent,
    sound: EventSound,
    on_change: Callback<EventSound>,
}

#[function_component(EventCard)]
fn event_card(props: &EventCardProps) -> Html {
    let event = props.event;
    let sound = &props.sound;
    let on_change = &props.on_change;

    let on_preview = {
        let sound = sound.clone();
        Callback::from(move |_: MouseEvent| {
            audio::play_preview(&sound);
        })
    };

    let on_waveform = {
        let sound = sound.clone();
        let on_change = on_change.clone();
        Callback::from(move |e: Event| {
            let select: web_sys::HtmlSelectElement = e.target_unchecked_into();
            let mut s = sound.clone();
            s.waveform = match select.value().as_str() {
                "Square" => Waveform::Square,
                "Sawtooth" => Waveform::Sawtooth,
                "Triangle" => Waveform::Triangle,
                _ => Waveform::Sine,
            };
            on_change.emit(s);
        })
    };

    // Macro-style helper to avoid repeating range slider code
    let make_slider = |label: &str,
                       value: f64,
                       min: f64,
                       max: f64,
                       step: f64,
                       unit: &str,
                       field: &'static str|
     -> Html {
        let sound = sound.clone();
        let on_change = on_change.clone();
        let display = if unit == "Hz" {
            format!("{:.0}{}", value, unit)
        } else {
            format!("{:.3}{}", value, unit)
        };
        let on_input = Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let v: f64 = input.value().parse().unwrap_or(value);
            let mut s = sound.clone();
            match field {
                "attack" => s.attack = v,
                "decay" => s.decay = v,
                "sustain" => s.sustain = v,
                "release" => s.release = v,
                "frequency" => s.frequency = v,
                "volume" => s.volume = v,
                _ => {}
            }
            on_change.emit(s);
        });
        html! {
            <div class="sound-control">
                <label>{ label }</label>
                <input
                    type="range"
                    min={min.to_string()}
                    max={max.to_string()}
                    step={step.to_string()}
                    value={value.to_string()}
                    oninput={on_input}
                />
                <span class="sound-value">{ display }</span>
            </div>
        }
    };

    html! {
        <div class="sound-event-card">
            <div class="sound-card-header">
                <div>
                    <h3>{ event.label() }</h3>
                    <p>{ event.description() }</p>
                </div>
                <button class="preview-button" onclick={on_preview}>
                    { "Preview" }
                </button>
            </div>
            <div class="sound-controls">
                <div class="sound-control">
                    <label>{ "Waveform" }</label>
                    <select onchange={on_waveform}>
                        { for Waveform::all().iter().map(|w| {
                            html! {
                                <option
                                    value={w.label()}
                                    selected={*w == sound.waveform}
                                >
                                    { w.label() }
                                </option>
                            }
                        }) }
                    </select>
                </div>
                { make_slider("Frequency", sound.frequency, 100.0, 2000.0, 10.0, "Hz", "frequency") }
                { make_slider("Attack", sound.attack, 0.001, 1.0, 0.001, "s", "attack") }
                { make_slider("Decay", sound.decay, 0.001, 1.0, 0.001, "s", "decay") }
                { make_slider("Sustain", sound.sustain, 0.0, 1.0, 0.01, "", "sustain") }
                { make_slider("Release", sound.release, 0.001, 2.0, 0.001, "s", "release") }
                { make_slider("Volume", sound.volume, 0.0, 1.0, 0.01, "", "volume") }
            </div>
        </div>
    }
}

/// New token form state
#[derive(Clone, Default)]
struct NewTokenForm {
    name: String,
    expires_in_days: u32,
}

#[derive(Properties, PartialEq)]
pub struct SettingsPageProps {
    pub on_close: Callback<()>,
}

#[function_component(SettingsPage)]
pub fn settings_page(props: &SettingsPageProps) -> Html {
    let active_tab = use_state(|| SettingsTab::Sessions);

    // Token state
    let tokens = use_state(Vec::<ProxyTokenInfo>::new);
    let tokens_loading = use_state(|| true);
    let new_token_form = use_state(NewTokenForm::default);
    let created_token = use_state(|| None::<CreateProxyTokenResponse>);
    let show_create_form = use_state(|| false);

    // Session state
    let sessions = use_state(Vec::<SessionInfo>::new);
    let sessions_loading = use_state(|| true);
    let share_session_id = use_state(|| None::<Uuid>);

    // Confirmation modal state
    let confirm_action = use_state(|| None::<(String, Callback<MouseEvent>)>);

    // Fetch tokens
    let fetch_tokens = {
        let tokens = tokens.clone();
        let tokens_loading = tokens_loading.clone();

        Callback::from(move |_| {
            let tokens = tokens.clone();
            let tokens_loading = tokens_loading.clone();

            spawn_local(async move {
                let api_endpoint = utils::api_url("/api/proxy-tokens");
                match Request::get(&api_endpoint).send().await {
                    Ok(response) => {
                        if response.status() == 401 {
                            if let Some(window) = web_sys::window() {
                                let _ = window.location().set_href("/api/auth/logout");
                            }
                            return;
                        }
                        if let Ok(data) = response.json::<ProxyTokenListResponse>().await {
                            tokens.set(data.tokens);
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to fetch tokens: {:?}", e);
                    }
                }
                tokens_loading.set(false);
            });
        })
    };

    // Fetch sessions
    let fetch_sessions = {
        let sessions = sessions.clone();
        let sessions_loading = sessions_loading.clone();

        Callback::from(move |_| {
            let sessions = sessions.clone();
            let sessions_loading = sessions_loading.clone();

            spawn_local(async move {
                let api_endpoint = utils::api_url("/api/sessions");
                match Request::get(&api_endpoint).send().await {
                    Ok(response) => {
                        if response.status() == 401 {
                            if let Some(window) = web_sys::window() {
                                let _ = window.location().set_href("/api/auth/logout");
                            }
                            return;
                        }
                        if let Ok(data) = response.json::<serde_json::Value>().await {
                            if let Some(session_list) = data.get("sessions") {
                                if let Ok(parsed) =
                                    serde_json::from_value::<Vec<SessionInfo>>(session_list.clone())
                                {
                                    sessions.set(parsed);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to fetch sessions: {:?}", e);
                    }
                }
                sessions_loading.set(false);
            });
        })
    };

    // Initial fetch
    {
        let fetch_tokens = fetch_tokens.clone();
        let fetch_sessions = fetch_sessions.clone();
        use_effect_with((), move |_| {
            fetch_tokens.emit(());
            fetch_sessions.emit(());
            || ()
        });
    }

    // Revoke token handler
    let on_revoke_token = {
        let tokens = tokens.clone();
        let confirm_action = confirm_action.clone();

        Callback::from(move |token_id: Uuid| {
            let tokens = tokens.clone();
            let confirm_action_inner = confirm_action.clone();

            let action = Callback::from(move |_: MouseEvent| {
                let tokens = tokens.clone();
                let confirm_action_inner = confirm_action_inner.clone();

                spawn_local(async move {
                    let api_endpoint = utils::api_url(&format!("/api/proxy-tokens/{}", token_id));
                    match Request::delete(&api_endpoint).send().await {
                        Ok(response) => {
                            if response.status() == 204 || response.status() == 200 {
                                // Update local state to mark as revoked
                                let mut updated: Vec<ProxyTokenInfo> = (*tokens).to_vec();
                                if let Some(token) = updated.iter_mut().find(|t| t.id == token_id) {
                                    token.revoked = true;
                                }
                                tokens.set(updated);
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to revoke token: {:?}", e);
                        }
                    }
                    confirm_action_inner.set(None);
                });
            });

            confirm_action.set(Some((
                "Revoke this token? Connected proxies will be disconnected.".to_string(),
                action,
            )));
        })
    };

    // Delete session handler
    let on_delete_session = {
        let sessions = sessions.clone();
        let confirm_action = confirm_action.clone();

        Callback::from(move |session_id: Uuid| {
            let sessions = sessions.clone();
            let confirm_action_inner = confirm_action.clone();

            let action = Callback::from(move |_: MouseEvent| {
                let sessions = sessions.clone();
                let confirm_action_inner = confirm_action_inner.clone();

                spawn_local(async move {
                    let api_endpoint = utils::api_url(&format!("/api/sessions/{}", session_id));
                    match Request::delete(&api_endpoint).send().await {
                        Ok(response) => {
                            if response.status() == 204 || response.status() == 200 {
                                // Remove from local state
                                let updated: Vec<SessionInfo> = (*sessions)
                                    .iter()
                                    .filter(|s| s.id != session_id)
                                    .cloned()
                                    .collect();
                                sessions.set(updated);
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to delete session: {:?}", e);
                        }
                    }
                    confirm_action_inner.set(None);
                });
            });

            confirm_action.set(Some((
                "Delete this session? All message history will be lost.".to_string(),
                action,
            )));
        })
    };

    // Share session handler
    let on_share_session = {
        let share_session_id = share_session_id.clone();
        Callback::from(move |session_id: Uuid| {
            share_session_id.set(Some(session_id));
        })
    };

    // Close share dialog handler
    let on_close_share = {
        let share_session_id = share_session_id.clone();
        Callback::from(move |_| {
            share_session_id.set(None);
        })
    };

    // Create token handler
    let on_create_token = {
        let new_token_form = new_token_form.clone();
        let created_token = created_token.clone();
        let fetch_tokens = fetch_tokens.clone();

        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let form_data = (*new_token_form).clone();
            let created_token = created_token.clone();
            let fetch_tokens = fetch_tokens.clone();

            if form_data.name.trim().is_empty() {
                return;
            }

            spawn_local(async move {
                let api_endpoint = utils::api_url("/api/proxy-tokens");
                let request_body = CreateProxyTokenRequest {
                    name: form_data.name.trim().to_string(),
                    expires_in_days: if form_data.expires_in_days > 0 {
                        form_data.expires_in_days
                    } else {
                        30
                    },
                };

                match Request::post(&api_endpoint)
                    .json(&request_body)
                    .unwrap()
                    .send()
                    .await
                {
                    Ok(response) => {
                        if let Ok(data) = response.json::<CreateProxyTokenResponse>().await {
                            created_token.set(Some(data));
                            // Refresh token list
                            fetch_tokens.emit(());
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to create token: {:?}", e);
                    }
                }
            });
        })
    };

    // Form input handlers
    let on_name_input = {
        let new_token_form = new_token_form.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut form = (*new_token_form).clone();
            form.name = input.value();
            new_token_form.set(form);
        })
    };

    let on_days_input = {
        let new_token_form = new_token_form.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut form = (*new_token_form).clone();
            form.expires_in_days = input.value().parse().unwrap_or(30);
            new_token_form.set(form);
        })
    };

    // Tab click handlers
    let on_tokens_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(SettingsTab::Tokens))
    };

    let on_sessions_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(SettingsTab::Sessions))
    };

    let on_sounds_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(SettingsTab::Sounds))
    };

    // Toggle create form
    let toggle_create_form = {
        let show_create_form = show_create_form.clone();
        let created_token = created_token.clone();
        let new_token_form = new_token_form.clone();
        Callback::from(move |_| {
            if *show_create_form {
                // Closing - reset form
                created_token.set(None);
                new_token_form.set(NewTokenForm::default());
            }
            show_create_form.set(!*show_create_form);
        })
    };

    // Cancel confirmation
    let cancel_confirm = {
        let confirm_action = confirm_action.clone();
        Callback::from(move |_| {
            confirm_action.set(None);
        })
    };

    // Back to dashboard
    let go_back = {
        let on_close = props.on_close.clone();
        Callback::from(move |_| on_close.emit(()))
    };

    // Count expiring tokens (within 7 days)
    let expiring_count = tokens
        .iter()
        .filter(|t| !t.revoked)
        .filter(|t| {
            days_until_expiration(&t.expires_at)
                .map(|d| (0..=7).contains(&d))
                .unwrap_or(false)
        })
        .count();

    html! {
        <div class="settings-container">
            <header class="settings-header">
                <button class="header-button" onclick={go_back}>
                    { "< Back" }
                </button>
                <h1>{ "Settings" }</h1>
                <button class="header-button logout" onclick={Callback::from(|_| {
                    if let Some(window) = web_sys::window() {
                        let _ = window.location().set_href("/api/auth/logout");
                    }
                })}>
                    { "Logout" }
                </button>
            </header>

            <nav class="settings-tabs">
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Sessions).then_some("active"))}
                    onclick={on_sessions_tab}
                >
                    { "Sessions" }
                    <span class="count-badge">{ sessions.len() }</span>
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Tokens).then_some("active"))}
                    onclick={on_tokens_tab}
                >
                    { "Credentials" }
                    if expiring_count > 0 {
                        <span class="expiring-badge">{ expiring_count }</span>
                    }
                </button>
                <button
                    class={classes!("tab-button", (*active_tab == SettingsTab::Sounds).then_some("active"))}
                    onclick={on_sounds_tab}
                >
                    { "Sounds" }
                </button>
            </nav>

            <main class="settings-content">
                // Token Management Tab
                if *active_tab == SettingsTab::Tokens {
                    <section class="tokens-section">
                        <div class="section-header">
                            <h2>{ "Proxy Credentials" }</h2>
                            <p class="section-description">
                                { "Manage authentication tokens for your Claude Code proxy connections." }
                            </p>
                            <button class="create-button" onclick={toggle_create_form.clone()}>
                                { if *show_create_form { "Cancel" } else { "+ Create Token" } }
                            </button>
                        </div>

                        // Create token form
                        if *show_create_form {
                            <div class="create-token-form">
                                if let Some(token_response) = &*created_token {
                                    <div class="token-created-success">
                                        <h3>{ "Token Created Successfully" }</h3>
                                        <p class="warning">
                                            { "Copy this token now. It will not be shown again!" }
                                        </p>
                                        <div class="token-display">
                                            <code>{ &token_response.token }</code>
                                        </div>
                                        <div class="init-url">
                                            <label>{ "Or use this initialization URL:" }</label>
                                            <code>{ &token_response.init_url }</code>
                                        </div>
                                        <p class="expires-info">
                                            { format!("Expires: {}", utils::format_timestamp(&token_response.expires_at)) }
                                        </p>
                                        <button onclick={toggle_create_form.clone()}>{ "Done" }</button>
                                    </div>
                                } else {
                                    <form onsubmit={on_create_token}>
                                        <div class="form-group">
                                            <label for="token-name">{ "Token Name" }</label>
                                            <input
                                                type="text"
                                                id="token-name"
                                                placeholder="e.g., My Laptop, Work Machine"
                                                value={new_token_form.name.clone()}
                                                oninput={on_name_input}
                                                required=true
                                            />
                                        </div>
                                        <div class="form-group">
                                            <label for="token-days">{ "Expires In (days)" }</label>
                                            <input
                                                type="number"
                                                id="token-days"
                                                min="1"
                                                max="365"
                                                value={new_token_form.expires_in_days.to_string()}
                                                oninput={on_days_input}
                                            />
                                        </div>
                                        <button type="submit" class="submit-button">
                                            { "Create Token" }
                                        </button>
                                    </form>
                                }
                            </div>
                        }

                        // Token list
                        if *tokens_loading {
                            <div class="loading">
                                <div class="spinner"></div>
                                <p>{ "Loading tokens..." }</p>
                            </div>
                        } else if tokens.is_empty() {
                            <div class="empty-state">
                                <p>{ "No tokens found. Create one to connect a proxy." }</p>
                            </div>
                        } else {
                            <div class="table-container">
                                <table class="tokens-table">
                                    <thead>
                                        <tr>
                                            <th>{ "Name" }</th>
                                            <th>{ "Created" }</th>
                                            <th>{ "Last Used" }</th>
                                            <th>{ "Expires" }</th>
                                            <th>{ "Status" }</th>
                                            <th>{ "Actions" }</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        { for tokens.iter().map(|token| {
                                            html! {
                                                <TokenRow
                                                    key={token.id.to_string()}
                                                    token={token.clone()}
                                                    on_revoke={on_revoke_token.clone()}
                                                />
                                            }
                                        }) }
                                    </tbody>
                                </table>
                            </div>
                            <p class="section-note">
                                { "Credentials expired for more than 7 days are automatically deleted." }
                            </p>
                        }
                    </section>

                    <section class="service-section">
                        <div class="section-header">
                            <h2>{ "Service" }</h2>
                            <p class="section-description">
                                { "Install and configure the agent on your machines." }
                            </p>
                        </div>
                        <ProxyTokenSetup />
                    </section>
                }

                // Sounds Tab
                if *active_tab == SettingsTab::Sounds {
                    <SoundsPanel />
                }

                // Session Management Tab
                if *active_tab == SettingsTab::Sessions {
                    <section class="sessions-section">
                        <div class="section-header">
                            <h2>{ "Session History" }</h2>
                            <p class="section-description">
                                { "View and manage your Claude Code sessions across all machines." }
                            </p>
                        </div>

                        if *sessions_loading {
                            <div class="loading">
                                <div class="spinner"></div>
                                <p>{ "Loading sessions..." }</p>
                            </div>
                        } else if sessions.is_empty() {
                            <div class="empty-state">
                                <p>{ "No sessions found." }</p>
                            </div>
                        } else {
                            <div class="table-container">
                                <table class="sessions-table">
                                    <thead>
                                        <tr>
                                            <th>{ "Project" }</th>
                                            <th>{ "ID" }</th>
                                            <th>{ "Host" }</th>
                                            <th>{ "Directory" }</th>
                                            <th>{ "Branch" }</th>
                                            <th>{ "Last Activity" }</th>
                                            <th>{ "Created" }</th>
                                            <th>{ "Status" }</th>
                                            <th>{ "Actions" }</th>
                                        </tr>
                                    </thead>
                                    <tbody>
                                        { for sessions.iter().map(|session| {
                                            html! {
                                                <SessionRow
                                                    key={session.id.to_string()}
                                                    session={session.clone()}
                                                    on_delete={on_delete_session.clone()}
                                                    on_share={on_share_session.clone()}
                                                />
                                            }
                                        }) }
                                    </tbody>
                                </table>
                            </div>
                        }
                    </section>
                }
            </main>

            // Confirmation Modal
            if let Some((message, action)) = &*confirm_action {
                <div class="modal-overlay" onclick={cancel_confirm.clone()}>
                    <div class="confirm-modal" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                        <p>{ message }</p>
                        <div class="confirm-actions">
                            <button class="cancel-button" onclick={cancel_confirm.clone()}>
                                { "Cancel" }
                            </button>
                            <button class="confirm-button" onclick={action.clone()}>
                                { "Confirm" }
                            </button>
                        </div>
                    </div>
                </div>
            }

            // Share dialog
            if let Some(session_id) = *share_session_id {
                <ShareDialog
                    session_id={session_id}
                    on_close={on_close_share.clone()}
                />
            }
        </div>
    }
}
