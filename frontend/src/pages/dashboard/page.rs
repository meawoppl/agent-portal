//! Dashboard page - Main session management interface

use super::session_rail::{ActivityRef, SessionRail};
use super::session_view::SessionView;
use super::types::{
    load_hidden_sessions, load_inactive_hidden, load_show_cost, save_hidden_sessions,
    save_inactive_hidden, save_show_cost,
};
use crate::components::LaunchDialog;
use crate::hooks::{use_client_websocket, use_keyboard_nav, use_sessions, KeyboardNavConfig};
use crate::pages::admin::AdminPage;
use crate::pages::settings::SettingsPage;
use crate::utils;
use gloo_net::http::Request;
use shared::{AppConfig, SessionInfo};
use std::collections::HashSet;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::MouseEvent;
use yew::prelude::*;

// =============================================================================
// Dashboard Page - Main Orchestrating Component
// =============================================================================

#[function_component(DashboardPage)]
pub fn dashboard_page() -> Html {
    // Use the sessions hook for fetching and polling
    let sessions_hook = use_sessions();
    let sessions = sessions_hook.sessions.clone();
    let loading = sessions_hook.loading;

    // Use the client websocket hook for spend updates
    let ws_hook = use_client_websocket();
    let total_user_spend = ws_hook.total_spend;
    let server_shutdown_reason = ws_hook.shutdown_reason.clone();

    // Track spend tier for timed animations
    let prev_spend_tier = use_state(|| 0u8);
    let spend_animating = use_state(|| false);
    let spend_initialized = use_state(|| false);

    // UI state
    let show_launch_dialog = use_state(|| false);
    let show_admin = use_state(|| false);
    let show_settings = use_state(|| false);
    let focused_index = use_state(|| 0usize);
    let awaiting_sessions = use_state(HashSet::<Uuid>::new);
    let hidden_sessions = use_state(load_hidden_sessions);
    let inactive_hidden = use_state(load_inactive_hidden);
    let show_cost = use_state(load_show_cost);
    let connected_sessions = use_state(HashSet::<Uuid>::new);
    let pending_leave = use_state(|| None::<Uuid>);
    let is_admin = use_state(|| false);
    let voice_enabled = use_state(|| false);
    let current_user_id = use_state(|| None::<String>);
    let app_title = use_state(|| "Agent Portal".to_string());
    let server_version = use_state(String::new);
    let activated_sessions = use_state(HashSet::<Uuid>::new);
    // Activity buffer: mutations don't trigger page re-renders.
    // SessionRail reads this on its own 100 ms tick instead.
    let activity_timestamps = use_memo((), |_| ActivityRef::default());
    let initial_focus_set = use_state(|| false);
    let sessions_at_launch = use_state(|| None::<HashSet<Uuid>>);

    // Detect spend tier changes and trigger timed animation
    {
        let spend_animating = spend_animating.clone();
        let prev_spend_tier = prev_spend_tier.clone();
        let spend_initialized = spend_initialized.clone();
        let current_tier = if total_user_spend >= 10000.0 {
            5u8
        } else if total_user_spend >= 1000.0 {
            4
        } else if total_user_spend >= 100.0 {
            3
        } else if total_user_spend >= 10.0 {
            2
        } else if total_user_spend >= 1.0 {
            1
        } else {
            0
        };
        use_effect_with(current_tier, move |tier| {
            let tier = *tier;
            if !*spend_initialized {
                // First tier value from page load — record it, don't animate
                spend_initialized.set(true);
                prev_spend_tier.set(tier);
            } else if tier > *prev_spend_tier {
                spend_animating.set(true);
                let duration_ms = match tier {
                    1 => 500,
                    2 => 2000,
                    3 => 5000,
                    4 => 10000,
                    _ => 20000,
                };
                let spend_animating = spend_animating.clone();
                let handle = gloo::timers::callback::Timeout::new(duration_ms, move || {
                    spend_animating.set(false);
                });
                prev_spend_tier.set(tier);
                handle.forget();
            } else if tier != *prev_spend_tier {
                prev_spend_tier.set(tier);
            }
            || ()
        });
    }

    // Fetch current user info (to check admin status, voice_enabled, and user_id)
    {
        let is_admin = is_admin.clone();
        let voice_enabled = voice_enabled.clone();
        let current_user_id = current_user_id.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                let api_endpoint = utils::api_url("/api/auth/me");
                if let Ok(response) = Request::get(&api_endpoint).send().await {
                    if let Ok(data) = response.json::<serde_json::Value>().await {
                        if let Some(admin) = data.get("is_admin").and_then(|v| v.as_bool()) {
                            is_admin.set(admin);
                        }
                        if let Some(voice) = data.get("voice_enabled").and_then(|v| v.as_bool()) {
                            voice_enabled.set(voice);
                        }
                        if let Some(id) = data.get("id").and_then(|v| v.as_str()) {
                            current_user_id.set(Some(id.to_string()));
                        }
                    }
                }
            });
            || ()
        });
    }

    // Fetch app configuration (title, version, etc.)
    {
        let app_title = app_title.clone();
        let server_version = server_version.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                let api_endpoint = utils::api_url("/api/config");
                if let Ok(response) = Request::get(&api_endpoint).send().await {
                    if let Ok(config) = response.json::<AppConfig>().await {
                        app_title.set(config.app_title);
                        server_version.set(config.server_version);
                    }
                }
            });
            || ()
        });
    }

    // Get active sessions sorted by repo name, then hostname
    // Disconnected sessions are completely hidden from the UI
    let active_sessions: Vec<SessionInfo> = {
        let mut sorted: Vec<SessionInfo> = sessions
            .iter()
            .filter(|s| s.status.as_str() == "active")
            .cloned()
            .collect();
        sorted.sort_by(|a, b| {
            let folder_a = utils::extract_folder(&a.working_directory);
            let folder_b = utils::extract_folder(&b.working_directory);
            match folder_a.to_lowercase().cmp(&folder_b.to_lowercase()) {
                std::cmp::Ordering::Equal => {
                    let hostname_a = &a.hostname;
                    let hostname_b = &b.hostname;
                    hostname_a.to_lowercase().cmp(&hostname_b.to_lowercase())
                }
                other => other,
            }
        });
        sorted
    };

    // On initial load, focus first non-hidden session and activate all non-hidden sessions
    {
        let active_sessions = active_sessions.clone();
        let hidden_sessions = hidden_sessions.clone();
        let focused_index = focused_index.clone();
        let initial_focus_set = initial_focus_set.clone();
        let activated_sessions = activated_sessions.clone();

        use_effect_with(
            (active_sessions.len(), loading),
            move |(session_count, is_loading)| {
                if !*initial_focus_set && !*is_loading && *session_count > 0 {
                    let first_non_hidden_idx = active_sessions
                        .iter()
                        .position(|s| !hidden_sessions.contains(&s.id))
                        .unwrap_or(0);

                    focused_index.set(first_non_hidden_idx);

                    // Activate all non-hidden sessions so they load in background
                    let mut activated = (*activated_sessions).clone();
                    for s in &active_sessions {
                        if !hidden_sessions.contains(&s.id) {
                            activated.insert(s.id);
                        }
                    }
                    activated_sessions.set(activated);

                    initial_focus_set.set(true);
                }
                || ()
            },
        );
    }

    // Auto-focus newly launched session when it appears in the session list
    {
        let sessions_at_launch = sessions_at_launch.clone();
        let active_sessions = active_sessions.clone();
        let focused_index = focused_index.clone();
        let activated_sessions = activated_sessions.clone();

        use_effect_with(active_sessions.clone(), move |sessions| {
            if let Some(ref snapshot) = *sessions_at_launch {
                if let Some((idx, session)) = sessions
                    .iter()
                    .enumerate()
                    .find(|(_, s)| !snapshot.contains(&s.id))
                {
                    focused_index.set(idx);
                    let mut activated = (*activated_sessions).clone();
                    activated.insert(session.id);
                    activated_sessions.set(activated);
                    sessions_at_launch.set(None);
                }
            }
            || ()
        });
    }

    // Session selection callback
    let on_select_session = {
        let focused_index = focused_index.clone();
        let activated_sessions = activated_sessions.clone();
        let active_sessions = active_sessions.clone();
        Callback::from(move |index: usize| {
            crate::audio::ensure_audio_context();
            crate::audio::play_sound(crate::audio::SoundEvent::SessionSwap);
            focused_index.set(index);
            if let Some(session) = active_sessions.get(index) {
                let mut activated = (*activated_sessions).clone();
                activated.insert(session.id);
                activated_sessions.set(activated);
            }
        })
    };

    // Activation callback for keyboard nav
    let on_activate = {
        let activated_sessions = activated_sessions.clone();
        Callback::from(move |session_id: Uuid| {
            let mut activated = (*activated_sessions).clone();
            activated.insert(session_id);
            activated_sessions.set(activated);
        })
    };

    // Interrupt signal counter — incremented by triple-Escape, passed to focused SessionView
    let interrupt_signal = use_state(|| 0u32);

    let on_interrupt = {
        let interrupt_signal = interrupt_signal.clone();
        Callback::from(move |()| {
            interrupt_signal.set(*interrupt_signal + 1);
        })
    };

    // Use the keyboard navigation hook
    let keyboard_nav = use_keyboard_nav(KeyboardNavConfig {
        sessions: active_sessions.clone(),
        focused_index: *focused_index,
        hidden_sessions: (*hidden_sessions).clone(),
        connected_sessions: (*connected_sessions).clone(),
        inactive_hidden: *inactive_hidden,
        on_select: on_select_session.clone(),
        on_activate,
        on_interrupt,
    });

    // Modal open callbacks
    let go_to_admin = {
        let show_admin = show_admin.clone();
        Callback::from(move |_| show_admin.set(true))
    };

    let go_to_settings = {
        let show_settings = show_settings.clone();
        Callback::from(move |_| show_settings.set(true))
    };

    let close_admin = {
        let show_admin = show_admin.clone();
        Callback::from(move |_: ()| show_admin.set(false))
    };

    let close_settings = {
        let show_settings = show_settings.clone();
        Callback::from(move |_: ()| show_settings.set(false))
    };

    let do_logout = Callback::from(move |_| {
        if let Some(window) = web_sys::window() {
            let _ = window.location().set_href("/api/auth/logout");
        }
    });

    // Leave session callbacks
    let on_leave = {
        let pending_leave = pending_leave.clone();
        Callback::from(move |session_id: Uuid| {
            pending_leave.set(Some(session_id));
        })
    };

    let on_cancel_leave = {
        let pending_leave = pending_leave.clone();
        Callback::from(move |_| {
            pending_leave.set(None);
        })
    };

    let on_confirm_leave = {
        let pending_leave = pending_leave.clone();
        let refresh = sessions_hook.refresh.clone();
        Callback::from(move |_| {
            if let Some(session_id) = *pending_leave {
                let refresh = refresh.clone();
                let pending_leave = pending_leave.clone();
                spawn_local(async move {
                    let me_endpoint = utils::api_url("/api/auth/me");
                    let user_id = match Request::get(&me_endpoint).send().await {
                        Ok(response) => {
                            if let Ok(data) = response.json::<serde_json::Value>().await {
                                data.get("id")
                                    .and_then(|v| v.as_str())
                                    .map(|s| s.to_string())
                            } else {
                                None
                            }
                        }
                        Err(_) => None,
                    };

                    if let Some(user_id) = user_id {
                        let api_endpoint = utils::api_url(&format!(
                            "/api/sessions/{}/members/{}",
                            session_id, user_id
                        ));
                        match Request::delete(&api_endpoint).send().await {
                            Ok(response) if response.status() == 204 => {
                                refresh.emit(());
                            }
                            Ok(response) => {
                                log::error!(
                                    "Failed to leave session: status {}",
                                    response.status()
                                );
                            }
                            Err(e) => {
                                log::error!("Failed to leave session: {:?}", e);
                            }
                        }
                    } else {
                        log::error!("Failed to get current user ID for leave");
                    }
                    pending_leave.set(None);
                });
            }
        })
    };

    let toggle_launch_dialog = {
        let show_launch_dialog = show_launch_dialog.clone();
        Callback::from(move |_: MouseEvent| {
            show_launch_dialog.set(!*show_launch_dialog);
        })
    };

    let on_launch_close = {
        let show_launch_dialog = show_launch_dialog.clone();
        Callback::from(move |_| {
            show_launch_dialog.set(false);
        })
    };

    let on_launch_success = {
        let sessions_at_launch = sessions_at_launch.clone();
        let active_sessions = active_sessions.clone();
        Callback::from(move |_| {
            let snapshot: HashSet<Uuid> = active_sessions.iter().map(|s| s.id).collect();
            sessions_at_launch.set(Some(snapshot));
        })
    };

    // Session state callbacks
    let on_awaiting_change = {
        let awaiting_sessions = awaiting_sessions.clone();
        Callback::from(move |(session_id, is_awaiting): (Uuid, bool)| {
            let currently_awaiting = awaiting_sessions.contains(&session_id);
            if currently_awaiting == is_awaiting {
                return;
            }
            let mut set = (*awaiting_sessions).clone();
            if is_awaiting {
                set.insert(session_id);
                crate::audio::play_sound(crate::audio::SoundEvent::AwaitingInput);
            } else {
                set.remove(&session_id);
            }
            awaiting_sessions.set(set);
        })
    };

    let on_cost_change = {
        Callback::from(move |(_session_id, _cost): (Uuid, f64)| {
            // Costs now come from the websocket hook, so this is a no-op
            // but we keep it for API compatibility with SessionView
        })
    };

    let on_connected_change = {
        let connected_sessions = connected_sessions.clone();
        Callback::from(move |(session_id, connected): (Uuid, bool)| {
            let mut set = (*connected_sessions).clone();
            if connected {
                set.insert(session_id);
            } else {
                set.remove(&session_id);
            }
            connected_sessions.set(set);
        })
    };

    let on_stop = {
        Callback::from(move |session_id: Uuid| {
            spawn_local(async move {
                let url = utils::api_url(&format!("/api/sessions/{}/stop", session_id));
                match Request::post(&url).send().await {
                    Ok(resp) if resp.status() == 202 => {
                        log::info!("Stop request sent for session {}", session_id);
                    }
                    Ok(resp) => {
                        log::error!("Failed to stop session: status {}", resp.status());
                    }
                    Err(e) => {
                        log::error!("Failed to stop session: {:?}", e);
                    }
                }
            });
        })
    };

    let on_toggle_hidden = {
        let hidden_sessions = hidden_sessions.clone();
        Callback::from(move |session_id: Uuid| {
            let mut set = (*hidden_sessions).clone();
            if set.contains(&session_id) {
                set.remove(&session_id);
            } else {
                set.insert(session_id);
            }
            save_hidden_sessions(&set);
            hidden_sessions.set(set);
        })
    };

    let on_toggle_inactive_hidden = {
        let inactive_hidden = inactive_hidden.clone();
        Callback::from(move |_: MouseEvent| {
            let new_val = !*inactive_hidden;
            save_inactive_hidden(new_val);
            inactive_hidden.set(new_val);
        })
    };

    let on_toggle_show_cost = {
        let show_cost = show_cost.clone();
        Callback::from(move |_: MouseEvent| {
            let new_val = !*show_cost;
            save_show_cost(new_val);
            show_cost.set(new_val);
        })
    };

    let on_message_sent = {
        let awaiting_sessions = awaiting_sessions.clone();
        Callback::from(move |current_session_id: Uuid| {
            let mut set = (*awaiting_sessions).clone();
            set.remove(&current_session_id);
            awaiting_sessions.set(set);
        })
    };

    let on_activity = {
        let activity_timestamps = (*activity_timestamps).clone();
        Callback::from(
            move |(session_id, msg_type, timestamp): (Uuid, String, f64)| {
                activity_timestamps.push(session_id, msg_type, timestamp);
            },
        )
    };

    let on_branch_change = {
        let set_sessions = sessions_hook.set_sessions.clone();
        let sessions = sessions.clone();
        Callback::from(
            move |(session_id, branch, pr_url, repo_url): (
                Uuid,
                Option<String>,
                Option<String>,
                Option<String>,
            )| {
                let mut updated = sessions.clone();
                if let Some(session) = updated.iter_mut().find(|s| s.id == session_id) {
                    session.git_branch = branch;
                    session.pr_url = pr_url;
                    session.repo_url = repo_url;
                }
                set_sessions.emit(updated);
            },
        )
    };

    // Computed values
    let waiting_count = awaiting_sessions
        .iter()
        .filter(|id| !hidden_sessions.contains(id))
        .count();

    // Update browser tab title
    {
        let app_title = app_title.clone();
        use_effect_with(
            (waiting_count, (*app_title).clone()),
            move |(count, title)| {
                if let Some(window) = web_sys::window() {
                    if let Some(document) = window.document() {
                        let new_title = if *count > 0 {
                            format!("({}) {}", count, title)
                        } else {
                            title.clone()
                        };
                        document.set_title(&new_title);
                    }
                }
                || ()
            },
        );
    }

    html! {
        <div class="focus-flow-container" onkeydown={keyboard_nav.on_keydown.clone()} tabindex="0">
            // Server shutdown warning banner
            {
                if let Some(reason) = server_shutdown_reason.as_ref() {
                    html! {
                        <div class="server-shutdown-banner">
                            <span class="shutdown-icon">{ "⚠" }</span>
                            <span class="shutdown-text">{ format!("Server shutting down: {} — reconnecting...", reason) }</span>
                        </div>
                    }
                } else {
                    html! {}
                }
            }

            // Header
            <header class="focus-flow-header">
                <h1>{ (*app_title).clone() }</h1>
                <div class="header-actions">
                    {
                        if total_user_spend > 0.0 {
                            let tier_class = if total_user_spend >= 10000.0 {
                                "spend-10000"
                            } else if total_user_spend >= 1000.0 {
                                "spend-1000"
                            } else if total_user_spend >= 100.0 {
                                "spend-100"
                            } else if total_user_spend >= 10.0 {
                                "spend-10"
                            } else if total_user_spend >= 1.0 {
                                "spend-1"
                            } else {
                                ""
                            };
                            let spend_class = classes!(
                                "total-spend-badge",
                                tier_class,
                                if *spend_animating { Some("spend-animating") } else { None },
                            );
                            html! {
                                <>
                                    if *show_cost {
                                        <span class={spend_class} title="Total spend across all sessions">
                                            { utils::format_dollars(total_user_spend) }
                                        </span>
                                    }
                                    <button
                                        class="cost-toggle-btn"
                                        onclick={on_toggle_show_cost.clone()}
                                        title={if *show_cost { "Hide cost" } else { "Show cost" }}
                                    >
                                        { if *show_cost { "$" } else { "$?" } }
                                    </button>
                                </>
                            }
                        } else {
                            html! {}
                        }
                    }
                    {
                        if waiting_count > 0 {
                            html! {
                                <span class="waiting-badge">
                                    { format!("{} waiting", waiting_count) }
                                </span>
                            }
                        } else {
                            html! {}
                        }
                    }
                    <button
                        class={classes!("new-session-button", if *show_launch_dialog { "active" } else { "" })}
                        onclick={toggle_launch_dialog.clone()}
                        title={if *show_launch_dialog { "Close" } else { "Launch a session or install agent-portal" }}
                    >
                        { if *show_launch_dialog { "Close" } else { "+ Launch Session" } }
                    </button>
                    {
                        if *is_admin {
                            html! {
                                <button class="header-button" onclick={go_to_admin.clone()}>
                                    { "Admin" }
                                </button>
                            }
                        } else {
                            html! {}
                        }
                    }
                    <button class="header-button" onclick={go_to_settings.clone()}>
                        { "Settings" }
                    </button>
                    <button class="header-button logout" onclick={do_logout.clone()}>
                        { "Logout" }
                    </button>
                </div>
            </header>

            // Launch session dialog
            if *show_launch_dialog {
                <LaunchDialog on_close={on_launch_close.clone()} on_launched={on_launch_success.clone()} />
            }

            if loading {
                <div class="loading">
                    <div class="spinner"></div>
                    <p>{ "Loading sessions..." }</p>
                </div>
            } else if active_sessions.is_empty() {
                <div class="onboarding-container">
                    <div class="onboarding-content">
                        <h2>{ "No Sessions Connected" }</h2>
                        <div class="onboarding-steps">
                            <div class="onboarding-step">
                                <span class="step-number">{ "1" }</span>
                                <div class="step-content">
                                    <p>{ "Click " }<strong>{ "+ Launch Session" }</strong>{ " to install agent-portal on a machine" }</p>
                                </div>
                            </div>
                            <div class="onboarding-step">
                                <span class="step-number">{ "2" }</span>
                                <div class="step-content">
                                    <p>{ "Once a launcher is connected, use " }<strong>{ "+ Launch Session" }</strong>{ " to start a session" }</p>
                                </div>
                            </div>
                        </div>
                    </div>
                </div>
            } else {
                <>
                    // Session Rail
                    <SessionRail
                        sessions={active_sessions.clone()}
                        focused_index={*focused_index}
                        awaiting_sessions={(*awaiting_sessions).clone()}
                        hidden_sessions={(*hidden_sessions).clone()}
                        inactive_hidden={*inactive_hidden}
                        connected_sessions={(*connected_sessions).clone()}
                        nav_mode={keyboard_nav.nav_mode}
                        activity_timestamps={(*activity_timestamps).clone()}
                        server_version={(*server_version).clone()}
                        on_select={on_select_session.clone()}
                        on_leave={on_leave.clone()}
                        on_toggle_hidden={on_toggle_hidden.clone()}
                        on_toggle_inactive_hidden={on_toggle_inactive_hidden.clone()}
                        on_stop={on_stop.clone()}
                    />

                    // Session views
                    <div class={classes!("session-views-container", if keyboard_nav.nav_mode { Some("nav-mode") } else { None })}>
                        {
                            active_sessions.iter().enumerate().map(|(index, session)| {
                                let is_focused = index == *focused_index;
                                let is_activated = activated_sessions.contains(&session.id);
                                if is_activated {
                                    html! {
                                        <div
                                            key={session.id.to_string()}
                                            class={classes!("session-view-wrapper", if is_focused { "focused" } else { "hidden" })}
                                        >
                                            <SessionView
                                                session={session.clone()}
                                                focused={is_focused}
                                                on_awaiting_change={on_awaiting_change.clone()}
                                                on_cost_change={on_cost_change.clone()}
                                                on_connected_change={on_connected_change.clone()}
                                                on_message_sent={on_message_sent.clone()}
                                                on_branch_change={on_branch_change.clone()}
                                                on_activity={on_activity.clone()}
                                                voice_enabled={*voice_enabled}
                                                current_user_id={(*current_user_id).clone()}
                                                interrupt_signal={*interrupt_signal}
                                            />
                                        </div>
                                    }
                                } else {
                                    html! {
                                        <div
                                            key={session.id.to_string()}
                                            class="session-view-wrapper hidden"
                                        />
                                    }
                                }
                            }).collect::<Html>()
                        }
                    </div>

                    // Keyboard hints
                    <div class={classes!("keyboard-hints", if keyboard_nav.nav_mode { Some("nav-mode") } else { None })}>
                        <div class="hints-content">
                            {
                                if keyboard_nav.nav_mode {
                                    html! {
                                        <>
                                            <span class="mode-indicator">{ "NAV" }</span>
                                            <span>{ "↑↓ or jk = navigate" }</span>
                                            <span>{ "1-9 = select" }</span>
                                            <span>{ "w = next waiting" }</span>
                                            <span>{ "Enter/Esc = edit mode" }</span>
                                        </>
                                    }
                                } else {
                                    html! {
                                        <>
                                            <span>{ "Esc = nav mode" }</span>
                                            <span>{ "Shift+Tab = next active" }</span>
                                            if *voice_enabled {
                                                <span>{ "Ctrl+M = voice" }</span>
                                            }
                                            <span>{ "Enter = send" }</span>
                                        </>
                                    }
                                }
                            }
                        </div>
                        <div class="hints-right">
                            <a
                                href="https://github.com/meawoppl/agent-portal/issues/new"
                                target="_blank"
                                rel="noopener noreferrer"
                                class="bug-report-link"
                            >
                                { "\u{1f41b}" }
                            </a>
                            if !(*server_version).is_empty() {
                                <span class="server-version">{ format!("v{}", *server_version) }</span>
                            }
                        </div>
                    </div>
                </>
            }

            // Admin modal — full-page overlay preserves dashboard state
            if *show_admin {
                <div class="full-page-modal">
                    <AdminPage on_close={close_admin.clone()} />
                </div>
            }

            // Settings modal — full-page overlay preserves dashboard state
            if *show_settings {
                <div class="full-page-modal">
                    <SettingsPage on_close={close_settings.clone()} />
                </div>
            }

            // Leave confirmation modal
            {
                if let Some(session_id) = *pending_leave {
                    let session_name = sessions.iter()
                        .find(|s| s.id == session_id)
                        .map(|s| utils::extract_folder(&s.working_directory))
                        .unwrap_or("this session");

                    html! {
                        <div class="modal-overlay" onclick={on_cancel_leave.clone()}>
                            <div class="modal-content delete-confirm" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                                <h2>{ "Leave Session?" }</h2>
                                <p>{ format!("Are you sure you want to leave \"{}\"?", session_name) }</p>
                                <p class="modal-warning">{ "You will need to be re-invited to access this session again." }</p>
                                <div class="modal-actions">
                                    <button class="modal-cancel" onclick={on_cancel_leave.clone()}>{ "Cancel" }</button>
                                    <button class="modal-confirm" onclick={on_confirm_leave.clone()}>{ "Leave" }</button>
                                </div>
                            </div>
                        </div>
                    }
                } else {
                    html! {}
                }
            }
        </div>
    }
}
