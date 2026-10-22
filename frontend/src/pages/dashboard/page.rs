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
    load_group_by_host, load_header_collapsed, load_hidden_sessions, load_inactive_hidden,
    load_rail_position, save_header_collapsed, save_hidden_sessions, save_inactive_hidden,
    PortalSession,
};
use crate::components::{
    ConfirmModal, ConfirmModalStyle, HelpOverlay, LaunchDialog, OnboardingTutorial, ServiceMonitor,
};
use crate::hooks::{
    use_client_websocket, use_interrupt_hotkey, use_keyboard_nav, use_sessions, KeyboardNavConfig,
};
use crate::pages::admin::AdminPage;
use crate::pages::history::{HistoryBrowserPage, HistoryTranscriptPage};
use crate::pages::settings::SettingsPage;
use crate::portal_client::{use_portal, PortalClient, SessionKey};
use crate::utils;
use gloo::events::EventListener;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlSelectElement, MouseEvent};
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

const MOBILE_DASHBOARD_BREAKPOINT_PX: f64 = 768.0;

fn mobile_dashboard_viewport() -> bool {
    web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|width| width.as_f64())
        .is_some_and(|width| width <= MOBILE_DASHBOARD_BREAKPOINT_PX)
}

/// Mirror the dashboard's CSS breakpoint in component state because mobile
/// session selection controls whether a SessionView (and therefore its REST
/// hydration and websocket subscription) exists at all.
#[hook]
fn use_mobile_dashboard() -> bool {
    let is_mobile = use_state_eq(mobile_dashboard_viewport);
    {
        let is_mobile = is_mobile.clone();
        use_effect_with((), move |_| {
            let listener = web_sys::window().map(|window| {
                EventListener::new(&window.clone(), "resize", move |_| {
                    is_mobile.set(mobile_dashboard_viewport());
                })
            });
            move || drop(listener)
        });
    }
    *is_mobile
}

/// The session behind `key`, for page-level actions that must reach the
/// portal it lives on.
fn find_session(sessions: &[PortalSession], key: SessionKey) -> Option<&PortalSession> {
    sessions.iter().find(|s| s.key == key)
}

/// The home portal plus each other portal the dashboard holds sessions on,
/// once each.
fn distinct_portals(home: PortalClient, sessions: &[PortalSession]) -> Vec<PortalClient> {
    let mut portals = vec![home];
    for session in sessions {
        if !portals.iter().any(|p| p.conn() == session.portal.conn()) {
            portals.push(session.portal.clone());
        }
    }
    portals
}

#[function_component(DashboardPage)]
pub fn dashboard_page() -> Html {
    let is_mobile_dashboard = use_mobile_dashboard();
    // Use the sessions hook for fetching and polling
    let sessions_hook = use_sessions();
    let sessions = sessions_hook.sessions.clone();
    let loading = sessions_hook.loading;

    let ws_hook = use_client_websocket();
    let federation = use_context::<crate::federation::Federation>();
    let home_portal = use_portal();
    let home_user = home_portal.account_id();
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
            load_header_collapsed(),
        )
    });
    // Focus is tracked by `session_id` (the source of truth), not by array
    // index — see `session_order` / issue #1094. The display index is derived
    // from this each render, so a reordered poll never bounces focus onto a
    // different session.
    let session_state =
        use_reducer_eq(|| DashboardSessionState::new(load_hidden_sessions(home_user)));
    // Activity buffer: mutations don't trigger page re-renders.
    // SessionRail reads this on its own 100 ms tick instead.
    let activity_timestamps = use_memo((), |_| ActivityRef::default());
    let agent_message_broadcasts = use_memo((), |_| BroadcastRef::default());

    // Get DB-authoritative sessions in a total, deterministic display order
    // (see `session_order`). A disconnected, unpaused session is
    // desired-running and should stay visible while the launcher reconciles it.
    // Live model overlay for the pill's model watermark. The persisted
    // `last_model` remains the durable fallback when this trend window rolls.
    // The metrics arrive on the home user channel, so they name home sessions.
    let live_models: HashMap<SessionKey, String> = ws_hook
        .recent_turn_metrics
        .iter()
        .filter_map(|m| {
            m.model
                .clone()
                .map(|model| (SessionKey::home(m.session_id), model))
        })
        .collect();

    // Per-session context-window fill from the dedicated durable latest-value
    // map. Rows without a usable window never erase a prior known gauge.
    let context_fractions: HashMap<SessionKey, f64> = ws_hook
        .latest_session_metrics
        .iter()
        .filter_map(|(key, m)| m.context_fraction().map(|frac| (*key, frac)))
        .collect();

    let active_sessions: Vec<PortalSession> = {
        let mut sorted: Vec<PortalSession> = sessions.to_vec();
        // Overlay the live model onto the polled row so the watermark reflects
        // the current turn without waiting for the poll to catch up.
        for session in sorted.iter_mut() {
            if let Some(model) = live_models.get(&session.key) {
                session.info.last_model = Some(model.clone());
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
            sorted.sort_by(|a, b| {
                a.key
                    .conn
                    .cmp(&b.key.conn)
                    .then_with(|| session_order::session_display_cmp_grouped(a, b))
            });
        } else {
            sorted.sort_by(|a, b| {
                a.key
                    .conn
                    .cmp(&b.key.conn)
                    .then_with(|| session_order::session_display_cmp(a, b))
            });
        }
        session_order::group_children(&mut sorted, ui_state.group_by_host);
        sorted
    };

    let effective_hidden_sessions =
        session_order::effective_hidden_sessions(&active_sessions, &session_state.hidden_sessions);

    let focus = use_dashboard_focus(
        active_sessions.clone(),
        effective_hidden_sessions.clone(),
        loading,
        is_mobile_dashboard,
        session_state.clone(),
    );

    // Seed the rail's awaiting flags from the server as soon as the session
    // list lands (#1915). The per-view `is_awaiting` signal only exists once a
    // session hydrates, and background sessions now hydrate on a drip — without
    // this seed, a session blocked on a permission wouldn't show its red flag
    // until its turn in the queue. One batched request per portal; each view's
    // own signal refines (and can clear) the flag as it hydrates.
    {
        let session_state = session_state.clone();
        let portals = distinct_portals(home_portal.clone(), &sessions);
        let loaded_connections = sessions_hook.loaded_connections.clone();
        use_effect_with((portals.clone(), loaded_connections), move |(_, loaded)| {
            let cancelled = std::rc::Rc::new(std::cell::Cell::new(false));
            {
                for portal in portals.into_iter().filter(|p| loaded.contains(&p.conn())) {
                    let cancelled = cancelled.clone();
                    let session_state = session_state.clone();
                    spawn_local(async move {
                        if let Ok(data) = portal
                            .fetch_json::<shared::api::AgentSessionsResponse>(
                                "/api/agent/sessions",
                                utils::On401::Ignore,
                            )
                            .await
                        {
                            if cancelled.get() {
                                return;
                            }
                            for session in data.sessions {
                                if session.awaiting_permission {
                                    session_state.dispatch(DashboardSessionAction::SetAwaiting {
                                        session_id: portal.session_key(session.id),
                                        awaiting: true,
                                    });
                                }
                            }
                        }
                    });
                }
            }
            move || cancelled.set(true)
        });
    }

    // Push-notification deep link (mobile-apps plan D4). `sw.js` opens
    // `/dashboard?session=<uuid>` on notification click; parse that id once at
    // mount and hold it until the target session shows up in `active_sessions`
    // (it may arrive before the list finishes loading). When it does, select it
    // via the same `on_select_session` path a rail click uses, then clear the
    // target *and* strip the query (`navigator.replace(Route::Dashboard)`) so a
    // refresh or back-nav can't re-fire a stale selection. An unknown or
    // inaccessible id simply never matches and is silently ignored. Push
    // notifications come from the home portal, so the id names a home session.
    let navigator = use_navigator();
    let location = use_location();
    let deep_link_target = use_state(move || {
        location
            .and_then(|loc| loc.query::<DeepLinkQuery>().ok())
            .and_then(|q| q.session)
            .and_then(|s| Uuid::parse_str(&s).ok())
            .map(SessionKey::home)
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
        let sessions = sessions.clone();
        Callback::from(move |key: SessionKey| {
            let Some(portal) = find_session(&sessions, key).map(|s| s.portal.clone()) else {
                return;
            };
            let refresh = refresh.clone();
            spawn_local(async move {
                let path = format!("/api/sessions/{}", key.id);
                match portal.delete(&path).send().await {
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
        Callback::from(move |session_id: SessionKey| {
            let hidden = !session_state.hidden_sessions.contains(&session_id);
            let mut set = session_state.hidden_sessions.clone();
            if hidden {
                set.insert(session_id);
            } else {
                set.remove(&session_id);
            }
            save_hidden_sessions(&set, home_user);
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
        on_new_session: on_new_session.clone(),
        on_delete: on_delete.clone(),
        on_jump_to_latest: focus.on_jump_to_latest.clone(),
        on_interrupt: focus.on_interrupt.clone(),
        on_toggle_hidden: on_toggle_hidden.clone(),
    });

    // Ctrl+Z interrupt: a window capture-phase listener so it fires in every
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
    // A restore from the history overlay lands on the restored session.
    let on_history_restored = {
        let ui_state = ui_state.clone();
        let session_state = session_state.clone();
        let refresh = sessions_hook.refresh.clone();
        Callback::from(move |session_id: Uuid| {
            refresh.emit(());
            ui_state.dispatch(DashboardUiAction::CloseHistory);
            session_state.dispatch(DashboardSessionAction::FocusAndActivate(SessionKey::home(
                session_id,
            )));
        })
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
        Callback::from(move |session_id: SessionKey| {
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
        let sessions = sessions.clone();
        Callback::from(move |_| {
            if let Some(key) = ui_state.pending_leave {
                let Some(portal) = find_session(&sessions, key).map(|s| s.portal.clone()) else {
                    ui_state.dispatch(DashboardUiAction::ClearPendingLeave);
                    return;
                };
                let refresh = refresh.clone();
                let ui_state = ui_state.clone();
                let user_id = portal.account_id();
                spawn_local(async move {
                    if let Some(user_id) = user_id {
                        let path = format!("/api/sessions/{}/members/{}", key.id, user_id);
                        match portal.delete(&path).send().await {
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

    let toggle_header_collapsed = {
        let ui_state = ui_state.clone();
        Callback::from(move |_: MouseEvent| {
            save_header_collapsed(!ui_state.header_collapsed);
            ui_state.dispatch(DashboardUiAction::ToggleHeaderCollapsed);
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

    // The launch dialog launches on the home portal.
    let on_launch_success = {
        let session_state = session_state.clone();
        Callback::from(move |session_id: Uuid| {
            session_state.dispatch(DashboardSessionAction::FocusAndActivate(SessionKey::home(
                session_id,
            )));
        })
    };

    // Session state callbacks
    let on_awaiting_change = {
        let session_state = session_state.clone();
        Callback::from(move |(session_id, is_awaiting): (SessionKey, bool)| {
            let currently_awaiting = session_state.awaiting_sessions.contains(&session_id);
            if currently_awaiting == is_awaiting {
                return;
            }
            session_state.dispatch(DashboardSessionAction::SetAwaiting {
                session_id,
                awaiting: is_awaiting,
            });
        })
    };

    let on_connected_change = {
        let session_state = session_state.clone();
        Callback::from(move |(session_id, connected): (SessionKey, bool)| {
            session_state.dispatch(DashboardSessionAction::SetConnected {
                session_id,
                connected,
            });
        })
    };

    let on_stop = {
        let sessions = sessions.clone();
        Callback::from(move |key: SessionKey| {
            let Some(portal) = find_session(&sessions, key).map(|s| s.portal.clone()) else {
                return;
            };
            if !portal.can_write() {
                return;
            }
            spawn_local(async move {
                let path = format!("/api/sessions/{}/stop", key.id);
                match portal.post(&path).send().await {
                    Ok(resp) if resp.status() == 202 => {
                        log::info!("Stop request sent for session {}", key.id);
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
        let sessions = sessions.clone();
        Callback::from(move |(session_id, pause): (SessionKey, bool)| {
            let Some(portal) = find_session(&sessions, session_id).map(|s| s.portal.clone()) else {
                return;
            };
            if !portal.can_write() {
                return;
            }
            let refresh = refresh.clone();
            let session_state = session_state.clone();
            spawn_local(async move {
                let action = if pause { "pause" } else { "resume" };
                let path = format!("/api/sessions/{}/{}", session_id.id, action);
                match portal.post(&path).send().await {
                    Ok(resp) if resp.status() == 202 => {
                        let mut set = session_state.hidden_sessions.clone();
                        if pause {
                            set.insert(session_id);
                        } else {
                            set.remove(&session_id);
                        }
                        save_hidden_sessions(&set, home_user);
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
                            session_id.id,
                            resp.status()
                        );
                    }
                    Err(e) => {
                        log::error!("Failed to {} session {}: {:?}", action, session_id.id, e);
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
        Callback::from(move |current_session_id: SessionKey| {
            session_state.dispatch(DashboardSessionAction::MessageSent(current_session_id));
        })
    };

    let on_activity = {
        let activity_timestamps = (*activity_timestamps).clone();
        Callback::from(
            move |(session_id, tag, timestamp): (
                SessionKey,
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
            move |(from_session_id, to_session_id, timestamp): (SessionKey, SessionKey, f64)| {
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
                SessionKey,
                Option<String>,
                Option<String>,
                Option<String>,
                Vec<shared::PrRef>,
            )| {
                let mut updated = sessions.clone();
                if let Some(session) = updated.iter_mut().find(|s| s.key == session_id) {
                    session.info.git_branch = branch;
                    session.info.pr_url = pr_url;
                    session.info.repo_url = repo_url;
                    session.info.open_prs = open_prs;
                }
                set_sessions.emit(updated);
            },
        )
    };

    // Computed values.
    // The rail's red "needs response" outline and the tab-title count show sessions
    // still awaiting a reply that the user hasn't looked at yet — awaiting minus the
    // ones whose current awaiting state has been seen (see DashboardSessionState
    // `seen_awaiting`). Viewing a session clears it here without disturbing the
    // underlying "agent is parked" flag.
    let effective_awaiting: HashSet<SessionKey> = session_state
        .awaiting_sessions
        .difference(&session_state.seen_awaiting)
        .copied()
        .collect();
    let waiting_count = effective_awaiting
        .iter()
        .filter(|id| !effective_hidden_sessions.contains(id))
        .count();
    // SessionView creation starts its REST history query and then its websocket
    // subscription. This order is deliberately independent of the pill order:
    // keep the focused session first, then hydrate the sessions the user most
    // recently messaged before colder background sessions.
    let session_view_order: Vec<usize> = if active_sessions.is_empty() {
        Vec::new()
    } else if is_mobile_dashboard {
        session_state
            .focused_id
            .and_then(|focused_id| {
                active_sessions
                    .iter()
                    .position(|session| session.key == focused_id)
            })
            .into_iter()
            .collect()
    } else {
        let focused_index = focus.focused_index.min(active_sessions.len() - 1);
        let mut indices = Vec::with_capacity(active_sessions.len());
        indices.push(focused_index);
        let mut background: Vec<usize> = (0..active_sessions.len())
            .filter(|index| *index != focused_index)
            .collect();
        background.sort_by(|a, b| {
            session_order::history_hydration_cmp(&active_sessions[*a], &active_sessions[*b])
        });
        indices.extend(background);
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

    // Mobile picker: live sessions first, then paused/stopped/hidden ones in
    // their own group at the bottom. Stopping a session pauses it, so
    // `effective_hidden_sessions` (hidden ∪ paused) covers all three.
    let (mobile_live, mobile_dormant): (Vec<&PortalSession>, Vec<&PortalSession>) = active_sessions
        .iter()
        .filter(|entry| entry.info.scheduled_task_id.is_none())
        .partition(|entry| !effective_hidden_sessions.contains(&entry.key));
    let mobile_option = |entry: &&PortalSession| {
        let session = &entry.info;
        let connection = if session.paused {
            "paused"
        } else if effective_hidden_sessions.contains(&entry.key) {
            "hidden"
        } else if session_state.connected_sessions.contains(&entry.key) {
            "connected"
        } else {
            session.status.as_str()
        };
        let location = if session.hostname.is_empty() {
            session.working_directory.as_str()
        } else {
            session.hostname.as_str()
        };
        html! {
            <option value={entry.key.to_string()}>
                { format!("{} — {} ({connection}){}", session.session_name, location, if entry.portal.is_linked() { format!(" · {} · {}", entry.portal.origin(), entry.portal.account_label().unwrap_or("")) } else { String::new() }) }
            </option>
        }
    };
    let mobile_options = html! {
        <>
            { for mobile_live.iter().map(mobile_option) }
            if !mobile_dormant.is_empty() {
                <optgroup label="Paused, stopped & hidden — selecting resumes">
                    { for mobile_dormant.iter().map(mobile_option) }
                </optgroup>
            }
        </>
    };

    html! {
        <div class="focus-flow-container" onkeydown={keyboard_nav.on_keydown.clone()} tabindex="0">
            if let Some(registry) = &federation {
                if !registry.rows.is_empty() {
                    <aside class="linked-connections" aria-label="Linked portal connections">
                        { for registry.rows.iter().map(|row| html! {
                            <div class="linked-connection-status"><strong>{&row.origin}</strong>{" · "}{&row.email}{" — "}{row.display_status()}</div>
                        }) }
                    </aside>
                }
            }

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

            // Header. The collapse toggle shrinks it to a slim full-width
            // pull strip (title and actions hidden) for vertical space —
            // most valuable on phones, available everywhere.
            <header class={classes!(
                "focus-flow-header",
                ui_state.header_collapsed.then_some("collapsed"),
            )}>
                <h1>{ app_title.clone() }</h1>
                <div class="header-actions">
                    {
                        // Machine stats are a desktop affordance; phones get the header space back.
                        if is_mobile_dashboard {
                            html! {}
                        } else {
                            html! { <ServiceMonitor services={ws_hook.service_stats.clone()} cpu_history={ws_hook.service_cpu_history.clone()} /> }
                        }
                    }
                    <button
                        class={classes!("new-session-button", if ui_state.show_launch_dialog { "active" } else { "" })}
                        onclick={toggle_launch_dialog.clone()}
                        title={if ui_state.show_launch_dialog { "Close" } else { "Launch a session or install agent-portal" }}
                    >
                        {
                            if ui_state.show_launch_dialog {
                                "Close"
                            } else if is_mobile_dashboard {
                                "+ New"
                            } else {
                                "+ Launch Session"
                            }
                        }
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
                <button
                    class="header-collapse-toggle"
                    onclick={toggle_header_collapsed}
                    title={if ui_state.header_collapsed { "Expand header" } else { "Collapse header" }}
                    aria-label={if ui_state.header_collapsed { "Expand header" } else { "Collapse header" }}
                >
                    { if ui_state.header_collapsed { "▾" } else { "▴" } }
                </button>
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
                    <OnboardingTutorial on_open_launch={on_new_session.clone()} />
                </div>
            } else {
                <>
                    <div class={classes!(
                        "dashboard-body",
                        ui_state.rail_position.body_class(),
                        if is_mobile_dashboard { Some("mobile-session-mode") } else { None },
                    )}>
                    // Session Rail
                    if is_mobile_dashboard {
                        <div class={classes!(
                            "mobile-session-selector",
                            if session_state.focused_id.is_none() { Some("empty") } else { None },
                        )}>
                            <label for="mobile-session-select">
                                { if session_state.focused_id.is_none() { "Choose a session" } else { "Session" } }
                            </label>
                            <select
                                id="mobile-session-select"
                                value={session_state.focused_id.map(|key| key.to_string()).unwrap_or_default()}
                                onchange={{
                                    let sessions = active_sessions.clone();
                                    let hidden = session_state.hidden_sessions.clone();
                                    let on_select = focus.on_select_session.clone();
                                    let on_clear = focus.on_clear_selection.clone();
                                    let on_toggle_pause = on_toggle_pause.clone();
                                    let on_toggle_hidden = on_toggle_hidden.clone();
                                    Callback::from(move |event: Event| {
                                        let value = event.target_unchecked_into::<HtmlSelectElement>().value();
                                        if value.is_empty() {
                                            on_clear.emit(());
                                        } else if let Some(index) = sessions.iter().position(|session| session.key.to_string() == value) {
                                            // Picking a dormant session brings it back: resume
                                            // a paused/stopped one (which also unhides it),
                                            // or just unhide a hidden one.
                                            let entry = &sessions[index];
                                            if entry.info.paused {
                                                on_toggle_pause.emit((entry.key, false));
                                            } else if hidden.contains(&entry.key) {
                                                on_toggle_hidden.emit(entry.key);
                                            }
                                            on_select.emit(index);
                                        }
                                    })
                                }}
                                aria-label="Select a session to open"
                            >
                                <option value="">{ "Select a session…" }</option>
                                { mobile_options }
                            </select>
                            if session_state.focused_id.is_none() {
                                <p>{ "Select a session to load its history and connect to its live stream." }</p>
                            }
                        </div>
                    } else {
                    <SessionRail
                        on_renamed={sessions_hook.refresh.clone()}
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
                        progress_fractions={ws_hook.session_progress.clone()}
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
                    }

                    // Session views
                    <div class={classes!(
                        "session-views-container",
                        if keyboard_nav.nav_mode { Some("nav-mode") } else { None },
                        if is_mobile_dashboard && session_state.focused_id.is_none() {
                            Some("mobile-empty")
                        } else {
                            None
                        },
                    )}>
                        {
                            session_view_order.iter().filter_map(|&index| {
                                let session = active_sessions.get(index)?;
                                let is_focused = index == focus.focused_index;
                                let is_activated = session_state.activated_sessions.contains(&session.key);
                                Some(if is_activated {
                                    html! {
                                        <div
                                            key={session.key.to_string()}
                                            class={classes!("session-view-wrapper", if is_focused { "focused" } else { "hidden" })}
                                        >
                                            <SessionView
                                                on_renamed={sessions_hook.refresh.clone()}
                                                session={session.info.clone()}
                                                portal={session.portal.clone()}
                                                focused={is_focused}
                                                on_awaiting_change={on_awaiting_change.clone()}
                                                on_connected_change={on_connected_change.clone()}
                                                on_message_sent={on_message_sent.clone()}
                                                on_branch_change={on_branch_change.clone()}
                                                on_activity={on_activity.clone()}
                                                on_agent_message={on_agent_message.clone()}
                                                current_user_id={session.portal.account_id().map(|id| id.to_string())}
                                                interrupt_signal={focus.interrupt_signal}
                                                jump_to_latest_signal={focus.jump_to_latest_signal}
                                                stt_enabled={stt_enabled}
                                                failure={ws_hook.session_failures.get(&session.key).cloned()}
                                                on_dismiss_failure={ws_hook.dismiss_failure.clone()}
                                                on_agent_auth_changed={ws_hook.on_agent_auth_changed.clone()}
                                            />
                                        </div>
                                    }
                                } else {
                                    html! {
                                        <div
                                            key={session.key.to_string()}
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
                                            <span>{ "Ctrl+Z = interrupt" }</span>
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
                                            <span>{ "Ctrl+Z = interrupt" }</span>
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
                            {current_user_id}
                            on_restored={on_history_restored.clone()}
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
                    let session_name = find_session(&sessions, session_id)
                        .map(|s| s.info.session_name.as_str())
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
