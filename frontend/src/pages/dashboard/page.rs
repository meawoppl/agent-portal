//! Dashboard page - Main session management interface

use super::page_bootstrap::use_dashboard_bootstrap;
use super::page_focus::use_dashboard_focus;
use super::page_state::{
    active_session_ids, DashboardSessionAction, DashboardSessionState, DashboardUiAction,
    DashboardUiState,
};
use super::session_order;
use super::session_rail::{ActivityRef, AgentMessageBroadcast, BroadcastRef, SessionRail};
use super::session_view::SessionView;
use super::types::{
    load_group_by_host, load_hidden_sessions, load_inactive_hidden, load_rail_position,
    save_hidden_sessions, save_inactive_hidden,
};
use crate::components::{
    ConfirmModal, ConfirmModalStyle, HelpOverlay, LaunchDialog, TurnMetricsHeaderPill,
};
use crate::hooks::{
    use_client_websocket, use_interrupt_hotkey, use_keyboard_nav, use_sessions, KeyboardNavConfig,
};
use crate::pages::admin::AdminPage;
use crate::pages::history::{HistoryBrowserPage, HistoryTranscriptPage};
use crate::pages::settings::SettingsPage;
use crate::utils;
use gloo_net::http::Request;
use serde::Deserialize;
use shared::SessionInfo;
use std::collections::{HashMap, HashSet};
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

    let ws_hook = use_client_websocket();
    let server_shutdown_reason = ws_hook.shutdown_reason.clone();
    let update_available = ws_hook.update_available.clone();
    let bootstrap = use_dashboard_bootstrap();
    let is_admin = bootstrap.is_admin;
    let current_user_id = bootstrap.current_user_id;
    let app_title = bootstrap.app_title;
    let server_version = bootstrap.server_version;
    let git_hash = bootstrap.git_hash;
    let build_time = bootstrap.build_time;
    let archive_enabled = bootstrap.archive_enabled;
    let stt_enabled = bootstrap.stt_enabled;

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
            load_rail_position(),
            load_group_by_host(),
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
    let agent_message_broadcasts = use_memo((), |_| BroadcastRef::default());

    // Get DB-authoritative sessions in a total, deterministic display order
    // (see `session_order`). A disconnected, unpaused session is
    // desired-running and should stay visible while the launcher reconciles it.
    // Live model overlay for the pill's model watermark. The persisted
    // `last_model` remains the durable fallback when this trend window rolls.
    let live_models: HashMap<Uuid, String> = ws_hook
        .recent_turn_metrics
        .iter()
        .filter_map(|m| m.model.clone().map(|model| (m.session_id, model)))
        .collect();

    // Per-session context-window fill from the dedicated durable latest-value
    // map. Rows without a usable window never erase a prior known gauge.
    let context_fractions: HashMap<Uuid, f64> = ws_hook
        .latest_session_metrics
        .values()
        .filter_map(|m| m.context_fraction().map(|frac| (m.session_id, frac)))
        .collect();

    let active_sessions: Vec<SessionInfo> = {
        let mut sorted: Vec<SessionInfo> = sessions.to_vec();
        // Overlay the live model onto the polled row so the watermark reflects
        // the current turn without waiting for the poll to catch up.
        for session in sorted.iter_mut() {
            if let Some(model) = live_models.get(&session.id) {
                session.last_model = Some(model.clone());
            }
        }
        // Total, deterministic order keyed down to the unique session id, so
        // the displayed order is a pure function of the session *set* and never
        // depends on the order `/api/sessions` happened to return (issue #1094).
        //
        // When the "group rail by host" pref is on we swap in the host-first
        // comparator. Both are *total* orders over the same vec, and this vec is
        // the single source of truth for focus resolution, nav-mode numbering,
        // and `j`/`k` traversal — so the logical order always matches the
        // visible top-to-bottom rail (grouped when the pref is on). The rail's
        // host headers are inserted purely visually and carry no display index.
        if ui_state.group_by_host {
            sorted.sort_by(session_order::session_display_cmp_grouped);
        } else {
            sorted.sort_by(session_order::session_display_cmp);
        }
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

    // `?`: open the keyboard-shortcuts help overlay.
    let on_show_help = {
        let ui_state = ui_state.clone();
        Callback::from(move |()| ui_state.dispatch(DashboardUiAction::ShowHelp))
    };

    // Request deletion of a session (shows the confirm modal). Shared by the
    // rail context menu and the nav-mode `d` shortcut.
    // The rail menu confirms this itself (click once to arm, again to fire), so
    // there is no modal round-trip: delete straight away and let the pill leave
    // the rail when the refresh lands.
    let on_delete = {
        let refresh = sessions_hook.refresh.clone();
        Callback::from(move |session_id: Uuid| {
            let refresh = refresh.clone();
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
            });
        })
    };

    // Open a new session (launch dialog). Shared by the nav-mode `n` shortcut.
    let on_new_session = {
        let ui_state = ui_state.clone();
        Callback::from(move |()| ui_state.dispatch(DashboardUiAction::ToggleLaunchDialog))
    };

    // Use the keyboard navigation hook
    // Toggle a session between hidden ("collapsed") and shown. Shared by the
    // rail kebab menu and the nav-mode `c` shortcut; persists to localStorage.
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

    let keyboard_nav = use_keyboard_nav(KeyboardNavConfig {
        sessions: active_sessions.clone(),
        focused_index: focus.focused_index,
        hidden_sessions: effective_hidden_sessions.clone(),
        on_select: focus.on_select_session.clone(),
        on_activate: focus.on_activate.clone(),
        on_show_help,
        on_new_session,
        on_delete: on_delete.clone(),
        on_jump_to_latest: focus.on_jump_to_latest.clone(),
        on_interrupt: focus.on_interrupt.clone(),
        on_toggle_hidden: on_toggle_hidden.clone(),
    });

    // Ctrl+C interrupt: a window capture-phase listener so it fires in every
    // mode (edit, nav, vim NORMAL/INSERT) and can't be swallowed by vim's `c`.
    use_interrupt_hotkey(focus.on_interrupt.clone());

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

    // Separate `use_navigator` handle: the one above is moved into the
    // deep-link effect closure.
    // Overlay, not a route push: navigating away unmounts the dashboard, and
    // coming back refetches every session and reconnects the WS. Same reason
    // Settings and Admin are overlays.
    let go_to_history = {
        let ui_state = ui_state.clone();
        Callback::from(move |_| ui_state.dispatch(DashboardUiAction::ShowHistory))
    };

    let close_admin = {
        let ui_state = ui_state.clone();
        Callback::from(move |_: ()| ui_state.dispatch(DashboardUiAction::CloseAdmin))
    };

    let close_history = {
        let ui_state = ui_state.clone();
        Callback::from(move |_| ui_state.dispatch(DashboardUiAction::CloseHistory))
    };
    let open_history_session = {
        let ui_state = ui_state.clone();
        Callback::from(move |(user, session): (String, String)| {
            ui_state.dispatch(DashboardUiAction::OpenHistorySession(user, session))
        })
    };
    let close_history_session = {
        let ui_state = ui_state.clone();
        Callback::from(move |_| ui_state.dispatch(DashboardUiAction::CloseHistorySession))
    };

    let close_settings = {
        let ui_state = ui_state.clone();
        Callback::from(move |_: ()| {
            // The Appearance panel may have changed this; re-sync from
            // localStorage so the dashboard picks up the new value when
            // the user navigates back.
            ui_state.dispatch(DashboardUiAction::SetRailPosition(load_rail_position()));
            ui_state.dispatch(DashboardUiAction::SetGroupByHost(load_group_by_host()));
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
        Callback::from(move |session_id: Uuid| {
            session_state.dispatch(DashboardSessionAction::FocusAndActivate(session_id));
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

    let on_agent_message = {
        let broadcasts = (*agent_message_broadcasts).clone();
        Callback::from(
            move |(from_session_id, to_session_id, timestamp): (Uuid, Uuid, f64)| {
                broadcasts.push(AgentMessageBroadcast {
                    from_session_id,
                    to_session_id,
                    timestamp,
                });
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

    // Computed values.
    // The rail's red "needs response" outline and this count show sessions still
    // awaiting a reply that the user hasn't looked at yet — awaiting minus the
    // ones whose current awaiting state has been seen (see DashboardSessionState
    // `seen_awaiting`). Viewing a session clears it here without disturbing the
    // underlying "agent is parked" flag.
    let effective_awaiting: HashSet<Uuid> = session_state
        .awaiting_sessions
        .difference(&session_state.seen_awaiting)
        .copied()
        .collect();
    let waiting_count = effective_awaiting
        .iter()
        .filter(|id| !effective_hidden_sessions.contains(id))
        .count();
    // SessionView creation starts each session websocket subscription. Keep the
    // rail order stable, but mount the focused session first so a reload gives
    // the last-active session the first subscription attempt before background
    // sessions connect.
    let session_view_order: Vec<usize> = if active_sessions.is_empty() {
        Vec::new()
    } else {
        let focused_index = focus.focused_index.min(active_sessions.len() - 1);
        let mut indices = Vec::with_capacity(active_sessions.len());
        indices.push(focused_index);
        indices.extend((0..active_sessions.len()).filter(|index| *index != focused_index));
        indices
    };

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
                    // History reads from the long-term archive; when archiving
                    // is disabled the page can only 404, so hide the entry
                    // rather than lead the user to a confusing error. (Config's
                    // `archive_enabled` comes from `/api/config`.)
                    {
                        if archive_enabled {
                            html! {
                                <button class="header-button" onclick={go_to_history.clone()}>
                                    { "History" }
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
                        awaiting_sessions={effective_awaiting.clone()}
                        hidden_sessions={effective_hidden_sessions.clone()}
                        inactive_hidden={ui_state.inactive_hidden}
                        group_by_host={ui_state.group_by_host}
                        connected_sessions={session_state.connected_sessions.clone()}
                        nav_mode={keyboard_nav.nav_mode}
                        activity_timestamps={(*activity_timestamps).clone()}
                        context_fractions={context_fractions.clone()}
                        broadcasts={(*agent_message_broadcasts).clone()}
                        rail_position={ui_state.rail_position}
                        server_version={server_version.clone()}
                        on_select={focus.on_select_session.clone()}
                        on_leave={on_leave.clone()}
                        on_delete={on_delete.clone()}
                        archive_enabled={archive_enabled}
                        on_toggle_hidden={on_toggle_hidden.clone()}
                        on_toggle_inactive_hidden={on_toggle_inactive_hidden.clone()}
                        on_stop={on_stop.clone()}
                        on_toggle_pause={on_toggle_pause.clone()}
                    />

                    // Session views
                    <div class={classes!("session-views-container", if keyboard_nav.nav_mode { Some("nav-mode") } else { None })}>
                        {
                            session_view_order.iter().filter_map(|&index| {
                                let session = active_sessions.get(index)?;
                                let is_focused = index == focus.focused_index;
                                let is_activated = session_state.activated_sessions.contains(&session.id);
                                Some(if is_activated {
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
                                                on_agent_message={on_agent_message.clone()}
                                                current_user_id={current_user_id.map(|id| id.to_string())}
                                                interrupt_signal={focus.interrupt_signal}
                                                jump_to_latest_signal={focus.jump_to_latest_signal}
                                                stt_enabled={stt_enabled}
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
                                })
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
                                            <span>{ "↑↓ hl = navigate" }</span>
                                            <span>{ "jk = scroll" }</span>
                                            <span>{ "gg = top" }</span>
                                            <span>{ "1-9 = select" }</span>
                                            <span>{ "w = next waiting" }</span>
                                            <span>{ "n = new" }</span>
                                            <span>{ "x = interrupt" }</span>
                                            <span>{ "Enter or Ctrl/Cmd+K = edit mode" }</span>
                                            <span>{ "? = shortcuts" }</span>
                                        </>
                                    }
                                } else {
                                    html! {
                                        <>
                                            <span>{ "Ctrl/Cmd+K = nav mode" }</span>
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
                                <span class="server-version">
                                    { format!("v{}", server_version) }
                                    // Short hash (links to the commit) + Pacific
                                    // build time for deploy tracing (#1386);
                                    // each shown only when the backend supplies
                                    // it, so an older server degrades cleanly.
                                    if !git_hash.is_empty() && git_hash != "unknown" {
                                        { " · " }
                                        <a
                                            class="build-hash"
                                            href={format!("https://github.com/meawoppl/agent-portal/commit/{git_hash}")}
                                            target="_blank"
                                            rel="noopener noreferrer"
                                        >{ git_hash.clone() }</a>
                                    }
                                    if !build_time.is_empty() && build_time != "unknown" {
                                        <span class="build-time">{ format!(" · {build_time}") }</span>
                                    }
                                </span>
                            }
                        </div>
                    </div>
                </>
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

            // History overlay — same full-page treatment as Settings, and it
            // hosts the transcript view too so opening one never leaves the
            // dashboard.
            if ui_state.show_history {
                <div class="full-page-modal">
                    if let Some((user, session)) = ui_state.history_session.clone() {
                        <HistoryTranscriptPage
                            {user}
                            {session}
                            on_back={close_history_session.clone()}
                        />
                    } else {
                        <HistoryBrowserPage
                            on_close={close_history.clone()}
                            on_open_session={open_history_session.clone()}
                        />
                    }
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
                        .map(|s| s.session_name.as_str())
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
