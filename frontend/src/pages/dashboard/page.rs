//! Dashboard page - Main session management interface

use super::page_bootstrap::use_dashboard_bootstrap;
use super::page_focus::use_dashboard_focus;
use super::page_spend::use_spend_badge_animation;
use super::page_state::{
    active_session_ids, DashboardSessionAction, DashboardSessionState, DashboardUiAction,
    DashboardUiState,
};
use super::session_order;
use super::session_rail::{ActivityRef, SessionRail};
use super::session_view::SessionView;
use super::types::{
    load_hidden_sessions, load_inactive_hidden, load_rail_position, load_show_cost,
    save_hidden_sessions, save_inactive_hidden, save_show_cost,
};
use crate::components::{
    ConfirmModal, ConfirmModalStyle, HelpOverlay, LaunchDialog, TurnMetricsHeaderPill,
};
use crate::hooks::{use_client_websocket, use_keyboard_nav, use_sessions, KeyboardNavConfig};
use crate::pages::admin::AdminPage;
use crate::pages::settings::SettingsPage;
use crate::utils;
use gloo_net::http::Request;
use serde::Deserialize;
use shared::SessionInfo;
use std::collections::HashSet;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::MouseEvent;
use yew::prelude::*;
use yew_router::prelude::*;

// =============================================================================
// Dashboard Page - Main Orchestrating Component
// =============================================================================

/// Query string consumed on dashboard mount for push-notification deep links
/// (mobile-apps plan item D4). `sw.js`'s `notificationclick` handler opens
/// `/dashboard?session=<uuid>`; we read `session` once, select that session as
/// a rail click would, then strip the query so a refresh / back-nav can't
/// re-fire a stale selection.
#[derive(Clone, PartialEq, Eq, Deserialize)]
struct DeepLinkQuery {
    #[serde(default)]
    session: Option<String>,
}

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
    let update_available = ws_hook.update_available.clone();
    let bootstrap = use_dashboard_bootstrap();
    let is_admin = bootstrap.is_admin;
    let current_user_id = bootstrap.current_user_id;
    let app_title = bootstrap.app_title;
    let server_version = bootstrap.server_version;

    // Push-driven session refresh: the backend broadcasts
    // `ServerToClient::LaunchSessionResult` the moment the launcher's
    // proxy registers (or fails). The WS hook ticks
    // `launch_event_counter` on each such frame; we hang a
    // `use_effect_with` on it so the freshly-launched session shows up in
    // the rail at the exact moment it becomes findable, instead of
    // waiting up to the 5 s steady-poll tick. Initial value 0 is skipped
    // so the mount doesn't fire a redundant refresh on top of the hook's
    // own initial fetch.
    {
        let refresh = sessions_hook.refresh.clone();
        use_effect_with(ws_hook.launch_event_counter, move |&c| {
            if c > 0 {
                refresh.emit(());
            }
            || ()
        });
    }

    // UI state
    let ui_state = use_reducer_eq(|| {
        DashboardUiState::new(
            load_inactive_hidden(),
            load_show_cost(),
            load_rail_position(),
        )
    });
    // Focus is tracked by `session_id` (the source of truth), not by array
    // index — see `session_order` / issue #1094. The display index is derived
    // from this each render, so a reordered poll never bounces focus onto a
    // different session.
    let session_state = use_reducer_eq(|| DashboardSessionState::new(load_hidden_sessions()));
    // Activity buffer: mutations don't trigger page re-renders.
    // SessionRail reads this on its own 100 ms tick instead.
    let activity_timestamps = use_memo((), |_| ActivityRef::default());
    let spend_animation = use_spend_badge_animation(total_user_spend);

    // Get DB-authoritative sessions in a total, deterministic display order
    // (see `session_order`). A disconnected, unpaused session is
    // desired-running and should stay visible while the launcher reconciles it.
    let active_sessions: Vec<SessionInfo> = {
        let mut sorted: Vec<SessionInfo> = sessions.to_vec();
        // Total, deterministic order keyed down to the unique session id, so
        // the displayed order is a pure function of the session *set* and never
        // depends on the order `/api/sessions` happened to return (issue #1094).
        sorted.sort_by(session_order::session_display_cmp);
        sorted
    };

    // Paused sessions follow the same frontend convention as manually hidden
    // sessions: they remain available in the hidden rail section but do not
    // participate in focus, activation, waiting counts, or keyboard rotation.
    let effective_hidden_sessions: HashSet<Uuid> = {
        let mut hidden = session_state.hidden_sessions.clone();
        hidden.extend(active_sessions.iter().filter(|s| s.paused).map(|s| s.id));
        hidden
    };

    let focus = use_dashboard_focus(
        active_sessions.clone(),
        effective_hidden_sessions.clone(),
        loading,
        session_state.clone(),
    );

    // Push-notification deep link (mobile-apps plan D4). `sw.js` opens
    // `/dashboard?session=<uuid>` on notification click; parse that id once at
    // mount and hold it until the target session shows up in `active_sessions`
    // (it may arrive before the list finishes loading). When it does, select it
    // via the same `on_select_session` path a rail click uses, then clear the
    // target *and* strip the query (`navigator.replace(Route::Dashboard)`) so a
    // refresh or back-nav can't re-fire a stale selection. An unknown or
    // inaccessible id simply never matches and is silently ignored.
    let navigator = use_navigator();
    let location = use_location();
    let deep_link_target = use_state(move || {
        location
            .and_then(|loc| loc.query::<DeepLinkQuery>().ok())
            .and_then(|q| q.session)
            .and_then(|s| Uuid::parse_str(&s).ok())
    });
    {
        let on_select = focus.on_select_session.clone();
        let deep_link_target = deep_link_target.clone();
        use_effect_with(
            (active_session_ids(&active_sessions), *deep_link_target),
            move |(session_ids, target)| {
                if let Some(target_id) = *target {
                    if let Some(index) = session_ids.iter().position(|id| *id == target_id) {
                        on_select.emit(index);
                        if let Some(navigator) = navigator {
                            navigator.replace(&crate::Route::Dashboard);
                        }
                        deep_link_target.set(None);
                    }
                }
                || ()
            },
        );
    }

    // Toggle a session's hidden state (persisted to localStorage). Shared by
    // the rail's "Hide Session" action and the nav-mode `x` shortcut.
    let on_toggle_hidden = {
        let session_state = session_state.clone();
        Callback::from(move |session_id: Uuid| {
            let hidden = !session_state.hidden_sessions.contains(&session_id);
            let mut set = session_state.hidden_sessions.clone();
            if hidden {
                set.insert(session_id);
            } else {
                set.remove(&session_id);
            }
            save_hidden_sessions(&set);
            session_state.dispatch(DashboardSessionAction::SetHidden { session_id, hidden });
        })
    };

    // Nav-mode `n`: open the launch dialog to start a new session.
    let on_new_session = {
        let ui_state = ui_state.clone();
        Callback::from(move |()| ui_state.dispatch(DashboardUiAction::ToggleLaunchDialog))
    };

    // `?`: open the keyboard-shortcuts help overlay.
    let on_show_help = {
        let ui_state = ui_state.clone();
        Callback::from(move |()| ui_state.dispatch(DashboardUiAction::ShowHelp))
    };

    // Use the keyboard navigation hook
    let keyboard_nav = use_keyboard_nav(KeyboardNavConfig {
        sessions: active_sessions.clone(),
        focused_index: focus.focused_index,
        hidden_sessions: effective_hidden_sessions.clone(),
        connected_sessions: session_state.connected_sessions.clone(),
        inactive_hidden: ui_state.inactive_hidden,
        on_select: focus.on_select_session.clone(),
        on_activate: focus.on_activate.clone(),
        on_interrupt: focus.on_interrupt.clone(),
        on_toggle_hidden: on_toggle_hidden.clone(),
        on_new_session,
        on_show_help,
    });

    let close_help = {
        let ui_state = ui_state.clone();
        Callback::from(move |_: ()| ui_state.dispatch(DashboardUiAction::CloseHelp))
    };

    // Modal open callbacks
    let go_to_admin = {
        let ui_state = ui_state.clone();
        Callback::from(move |_| ui_state.dispatch(DashboardUiAction::ShowAdmin))
    };

    let go_to_settings = {
        let ui_state = ui_state.clone();
        Callback::from(move |_| ui_state.dispatch(DashboardUiAction::ShowSettings))
    };

    let close_admin = {
        let ui_state = ui_state.clone();
        Callback::from(move |_: ()| ui_state.dispatch(DashboardUiAction::CloseAdmin))
    };

    let close_settings = {
        let ui_state = ui_state.clone();
        Callback::from(move |_: ()| {
            // The Appearance panel may have changed this; re-sync from
            // localStorage so the dashboard picks up the new value when
            // the user navigates back.
            ui_state.dispatch(DashboardUiAction::SetRailPosition(load_rail_position()));
            ui_state.dispatch(DashboardUiAction::CloseSettings);
        })
    };

    let do_logout = Callback::from(move |_| utils::logout());

    // Leave session callbacks
    let on_leave = {
        let ui_state = ui_state.clone();
        Callback::from(move |session_id: Uuid| {
            ui_state.dispatch(DashboardUiAction::RequestLeave(session_id));
        })
    };

    let on_cancel_leave = {
        let ui_state = ui_state.clone();
        Callback::from(move |_| {
            ui_state.dispatch(DashboardUiAction::ClearPendingLeave);
        })
    };

    let on_confirm_leave = {
        let ui_state = ui_state.clone();
        let refresh = sessions_hook.refresh.clone();
        Callback::from(move |_| {
            if let Some(session_id) = ui_state.pending_leave {
                let refresh = refresh.clone();
                let ui_state = ui_state.clone();
                let user_id = current_user_id;
                spawn_local(async move {
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
                    ui_state.dispatch(DashboardUiAction::ClearPendingLeave);
                });
            }
        })
    };

    let on_delete = {
        let ui_state = ui_state.clone();
        Callback::from(move |session_id: Uuid| {
            ui_state.dispatch(DashboardUiAction::RequestDelete(session_id));
        })
    };

    let on_cancel_delete = {
        let ui_state = ui_state.clone();
        Callback::from(move |_| {
            ui_state.dispatch(DashboardUiAction::ClearPendingDelete);
        })
    };

    let on_confirm_delete = {
        let ui_state = ui_state.clone();
        let refresh = sessions_hook.refresh.clone();
        Callback::from(move |_| {
            if let Some(session_id) = ui_state.pending_delete {
                let refresh = refresh.clone();
                let ui_state = ui_state.clone();
                spawn_local(async move {
                    let api_endpoint = utils::api_url(&format!("/api/sessions/{}", session_id));
                    match Request::delete(&api_endpoint).send().await {
                        Ok(response) if response.status() == 204 => {
                            refresh.emit(());
                        }
                        Ok(response) => {
                            log::error!("Failed to delete session: status {}", response.status());
                        }
                        Err(e) => {
                            log::error!("Failed to delete session: {:?}", e);
                        }
                    }
                    ui_state.dispatch(DashboardUiAction::ClearPendingDelete);
                });
            }
        })
    };

    let toggle_launch_dialog = {
        let ui_state = ui_state.clone();
        Callback::from(move |_: MouseEvent| {
            ui_state.dispatch(DashboardUiAction::ToggleLaunchDialog);
        })
    };

    let on_launch_close = {
        let ui_state = ui_state.clone();
        Callback::from(move |_| {
            ui_state.dispatch(DashboardUiAction::CloseLaunchDialog);
        })
    };

    let on_launch_success = {
        let session_state = session_state.clone();
        let active_sessions = active_sessions.clone();
        Callback::from(move |_| {
            session_state.dispatch(DashboardSessionAction::StoreLaunchSnapshot(
                active_session_ids(&active_sessions),
            ));
        })
    };

    // Session state callbacks
    let on_awaiting_change = {
        let session_state = session_state.clone();
        Callback::from(move |(session_id, is_awaiting): (Uuid, bool)| {
            let currently_awaiting = session_state.awaiting_sessions.contains(&session_id);
            if currently_awaiting == is_awaiting {
                return;
            }
            if is_awaiting {
                crate::audio::play_sound(crate::audio::SoundEvent::AwaitingInput);
            }
            session_state.dispatch(DashboardSessionAction::SetAwaiting {
                session_id,
                awaiting: is_awaiting,
            });
        })
    };

    let on_connected_change = {
        let session_state = session_state.clone();
        Callback::from(move |(session_id, connected): (Uuid, bool)| {
            session_state.dispatch(DashboardSessionAction::SetConnected {
                session_id,
                connected,
            });
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

    let on_toggle_pause = {
        let refresh = sessions_hook.refresh.clone();
        let session_state = session_state.clone();
        Callback::from(move |(session_id, pause): (Uuid, bool)| {
            let refresh = refresh.clone();
            let session_state = session_state.clone();
            spawn_local(async move {
                let action = if pause { "pause" } else { "resume" };
                let url = utils::api_url(&format!("/api/sessions/{}/{}", session_id, action));
                match Request::post(&url).send().await {
                    Ok(resp) if resp.status() == 202 => {
                        let mut set = session_state.hidden_sessions.clone();
                        if pause {
                            set.insert(session_id);
                        } else {
                            set.remove(&session_id);
                        }
                        save_hidden_sessions(&set);
                        session_state.dispatch(DashboardSessionAction::SetHidden {
                            session_id,
                            hidden: pause,
                        });
                        refresh.emit(());
                    }
                    Ok(resp) => {
                        log::error!(
                            "Failed to {} session {}: status {}",
                            action,
                            session_id,
                            resp.status()
                        );
                    }
                    Err(e) => {
                        log::error!("Failed to {} session {}: {:?}", action, session_id, e);
                    }
                }
            });
        })
    };

    let on_toggle_inactive_hidden = {
        let ui_state = ui_state.clone();
        Callback::from(move |_: MouseEvent| {
            let new_val = !ui_state.inactive_hidden;
            save_inactive_hidden(new_val);
            ui_state.dispatch(DashboardUiAction::SetInactiveHidden(new_val));
        })
    };

    let on_toggle_show_cost = {
        let ui_state = ui_state.clone();
        Callback::from(move |_: MouseEvent| {
            let new_val = !ui_state.show_cost;
            save_show_cost(new_val);
            ui_state.dispatch(DashboardUiAction::SetShowCost(new_val));
        })
    };

    let on_message_sent = {
        let session_state = session_state.clone();
        Callback::from(move |current_session_id: Uuid| {
            session_state.dispatch(DashboardSessionAction::MessageSent(current_session_id));
        })
    };

    let on_activity = {
        let activity_timestamps = (*activity_timestamps).clone();
        Callback::from(
            move |(session_id, tag, timestamp): (
                Uuid,
                crate::pages::dashboard::session_view::ActivityTag,
                f64,
            )| {
                activity_timestamps.push(session_id, tag, timestamp);
            },
        )
    };

    let on_branch_change = {
        let set_sessions = sessions_hook.set_sessions.clone();
        let sessions = sessions.clone();
        Callback::from(
            move |(session_id, branch, pr_url, repo_url, open_prs): (
                Uuid,
                Option<String>,
                Option<String>,
                Option<String>,
                Vec<shared::PrRef>,
            )| {
                let mut updated = sessions.clone();
                if let Some(session) = updated.iter_mut().find(|s| s.id == session_id) {
                    session.git_branch = branch;
                    session.pr_url = pr_url;
                    session.repo_url = repo_url;
                    session.open_prs = open_prs;
                }
                set_sessions.emit(updated);
            },
        )
    };

    // Computed values
    let waiting_count = session_state
        .awaiting_sessions
        .iter()
        .filter(|id| !effective_hidden_sessions.contains(id))
        .count();

    // Update browser tab title
    {
        let app_title = app_title.clone();
        use_effect_with((waiting_count, app_title.clone()), move |(count, title)| {
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
        });
    }

    html! {
        <div class="focus-flow-container" onkeydown={keyboard_nav.on_keydown.clone()} tabindex="0">
            // Update-available banner (post-reconnect, server version advanced)
            // takes precedence over the transient shutdown banner.
            {
                if let Some(version) = update_available.as_ref() {
                    let on_reload = Callback::from(|_: MouseEvent| {
                        if let Some(window) = web_sys::window() {
                            let _ = window.location().reload();
                        }
                    });
                    html! {
                        <div class="update-available-banner" role="status">
                            <span class="update-banner-text">
                                { format!("New version available: v{version}") }
                            </span>
                            <button
                                class="update-banner-button"
                                onclick={on_reload}
                                aria-label={format!("Reload to v{version}")}
                            >
                                { format!("Reload to v{version}") }
                            </button>
                        </div>
                    }
                } else if let Some(reason) = server_shutdown_reason.as_ref() {
                    html! {
                        <div class="server-shutdown-banner" role="status">
                            <span class="shutdown-banner-dot" aria-hidden="true"></span>
                            <span class="shutdown-banner-text">
                                { format!("Server restarting ({reason}) — reconnecting…") }
                            </span>
                        </div>
                    }
                } else {
                    html! {}
                }
            }

            // Header
            <header class="focus-flow-header">
                <h1>{ app_title.clone() }</h1>
                <div class="header-actions">
                    <TurnMetricsHeaderPill metrics={ws_hook.recent_turn_metrics.clone()} />
                    {
                        if total_user_spend > 0.0 {
                            let spend_class = classes!(
                                "total-spend-badge",
                                spend_animation.tier_class,
                                if spend_animation.animating { Some("spend-animating") } else { None },
                            );
                            html! {
                                <>
                                    if ui_state.show_cost {
                                        <span class={spend_class} title="Total spend across all sessions">
                                            { utils::format_dollars(total_user_spend) }
                                        </span>
                                    }
                                    <button
                                        class="cost-toggle-btn"
                                        onclick={on_toggle_show_cost.clone()}
                                        title={if ui_state.show_cost { "Hide cost" } else { "Show cost" }}
                                    >
                                        { if ui_state.show_cost { "$" } else { "$?" } }
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
                        class={classes!("new-session-button", if ui_state.show_launch_dialog { "active" } else { "" })}
                        onclick={toggle_launch_dialog.clone()}
                        title={if ui_state.show_launch_dialog { "Close" } else { "Launch a session or install agent-portal" }}
                    >
                        { if ui_state.show_launch_dialog { "Close" } else { "+ Launch Session" } }
                    </button>
                    {
                        if is_admin {
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
            if ui_state.show_launch_dialog {
                <LaunchDialog
                    on_close={on_launch_close.clone()}
                    on_launched={on_launch_success.clone()}
                    launcher_refresh={ws_hook.launcher_event_counter}
                />
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
                    <div class={classes!("dashboard-body", ui_state.rail_position.body_class())}>
                    // Session Rail
                    <SessionRail
                        sessions={active_sessions.clone()}
                        focused_index={focus.focused_index}
                        awaiting_sessions={session_state.awaiting_sessions.clone()}
                        hidden_sessions={effective_hidden_sessions.clone()}
                        inactive_hidden={ui_state.inactive_hidden}
                        connected_sessions={session_state.connected_sessions.clone()}
                        nav_mode={keyboard_nav.nav_mode}
                        activity_timestamps={(*activity_timestamps).clone()}
                        server_version={server_version.clone()}
                        on_select={focus.on_select_session.clone()}
                        on_leave={on_leave.clone()}
                        on_delete={on_delete.clone()}
                        on_toggle_hidden={on_toggle_hidden.clone()}
                        on_toggle_inactive_hidden={on_toggle_inactive_hidden.clone()}
                        on_stop={on_stop.clone()}
                        on_toggle_pause={on_toggle_pause.clone()}
                    />

                    // Session views
                    <div class={classes!("session-views-container", if keyboard_nav.nav_mode { Some("nav-mode") } else { None })}>
                        {
                            active_sessions.iter().enumerate().map(|(index, session)| {
                                let is_focused = index == focus.focused_index;
                                let is_activated = session_state.activated_sessions.contains(&session.id);
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
                                                on_connected_change={on_connected_change.clone()}
                                                on_message_sent={on_message_sent.clone()}
                                                on_branch_change={on_branch_change.clone()}
                                                on_activity={on_activity.clone()}
                                                current_user_id={current_user_id.map(|id| id.to_string())}
                                                interrupt_signal={focus.interrupt_signal}
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
                                            <span>{ "n = new" }</span>
                                            <span>{ "x = hide" }</span>
                                            <span>{ "Enter/Esc = edit mode" }</span>
                                            <span>{ "? = shortcuts" }</span>
                                        </>
                                    }
                                } else {
                                    html! {
                                        <>
                                            <span>{ "Esc = nav mode" }</span>
                                            <span>{ "Shift+Tab = next active" }</span>
                                            <span>{ "Ctrl+M = voice" }</span>
                                            <span>{ "Enter = send" }</span>
                                            <span>{ "? = shortcuts" }</span>
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
                            if !server_version.is_empty() {
                                <span class="server-version">{ format!("v{}", server_version) }</span>
                            }
                        </div>
                    </div>
                </>
            }

            // Delete confirmation modal
            {
                if let Some(session_id) = ui_state.pending_delete {
                    let session_name = sessions.iter()
                        .find(|s| s.id == session_id)
                        .map(|s| utils::extract_folder(&s.working_directory))
                        .unwrap_or("this session");

                    html! {
                        <ConfirmModal
                            title="Delete Session?"
                            message={format!("Are you sure you want to delete \"{}\"?", session_name)}
                            warning="All message history and session metadata will be permanently removed."
                            confirm_label="Delete"
                            style={ConfirmModalStyle::Danger}
                            on_confirm={on_confirm_delete.clone()}
                            on_cancel={on_cancel_delete.clone()}
                        />
                    }
                } else {
                    html! {}
                }
            }

            // Admin modal — full-page overlay preserves dashboard state
            if ui_state.show_admin {
                <div class="full-page-modal">
                    <AdminPage on_close={close_admin.clone()} current_user_id={current_user_id} />
                </div>
            }

            // Settings modal — full-page overlay preserves dashboard state
            if ui_state.show_settings {
                <div class="full-page-modal">
                    <SettingsPage on_close={close_settings.clone()} />
                </div>
            }

            // Keyboard shortcuts help overlay (press `?`)
            if ui_state.show_help {
                <HelpOverlay on_close={close_help.clone()} />
            }

            // Leave confirmation modal
            {
                if let Some(session_id) = ui_state.pending_leave {
                    let session_name = sessions.iter()
                        .find(|s| s.id == session_id)
                        .map(|s| utils::extract_folder(&s.working_directory))
                        .unwrap_or("this session");

                    html! {
                        <ConfirmModal
                            title="Leave Session?"
                            message={format!("Are you sure you want to leave \"{}\"?", session_name)}
                            warning="You will need to be re-invited to access this session again."
                            confirm_label="Leave"
                            style={ConfirmModalStyle::Danger}
                            on_confirm={on_confirm_leave.clone()}
                            on_cancel={on_cancel_leave.clone()}
                        />
                    }
                } else {
                    html! {}
                }
            }
        </div>
    }
}
