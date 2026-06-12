//! Admin dashboard page
//!
//! Restricted to users with is_admin=true. Provides system overview,
//! user management, and session management capabilities.

mod overview_tab;
mod sessions_tab;
mod users_tab;

use overview_tab::AdminOverviewTab;
use sessions_tab::AdminSessionsTab;
use users_tab::AdminUsersTab;

use crate::components::ConfirmModal;
use crate::utils::{self, FetchError, On401};
use crate::Route;
use gloo_net::http::Request;
use shared::api::{AdminSessionsResponse, AdminUsersResponse, MeResponse, UpdateUserRequest};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::MouseEvent;
use yew::prelude::*;
use yew_router::prelude::*;

/// Re-export the shared admin user entry under the legacy frontend name
/// so this module's sub-tabs can keep importing `super::AdminUserInfo`.
pub use shared::api::AdminUserEntry as AdminUserInfo;
/// Re-export so this module's sub-tabs can keep importing
/// `super::AdminSessionInfo` / `super::AdminStats`.
pub use shared::api::{AdminSessionInfo, AdminStats};

/// Admin page tabs
#[derive(Clone, Copy, PartialEq)]
enum AdminTab {
    Overview,
    Users,
    Sessions,
}

// ============================================================================
// Main Admin Page Component
// ============================================================================

#[derive(Properties, PartialEq)]
pub struct AdminPageProps {
    pub on_close: Callback<()>,
}

#[function_component(AdminPage)]
pub fn admin_page(props: &AdminPageProps) -> Html {
    let active_tab = use_state(|| AdminTab::Overview);
    let stats = use_state(|| None::<AdminStats>);
    let users = use_state(Vec::<AdminUserInfo>::new);
    let sessions = use_state(Vec::<AdminSessionInfo>::new);
    let loading = use_state(|| true);
    let error = use_state(|| None::<String>);
    let current_user_id = use_state(|| None::<Uuid>);
    let confirm_action = use_state(|| None::<(String, Callback<MouseEvent>)>);
    // Ban dialog state - when set, shows ban modal with reason input
    let ban_dialog = use_state(|| None::<Uuid>);
    let ban_reason_input = use_state(String::new);

    let navigator = use_navigator().unwrap();

    // Fetch current user to get their ID
    {
        let current_user_id = current_user_id.clone();
        let error = error.clone();
        let navigator = navigator.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                match utils::fetch_json::<MeResponse>("/api/auth/me", On401::Ignore).await {
                    Ok(me) => {
                        current_user_id.set(Some(me.id));
                    }
                    Err(FetchError::Status(401)) => {
                        navigator.push(&Route::Home);
                    }
                    Err(FetchError::Status(403)) => {
                        error.set(Some(
                            "Access denied. Admin privileges required.".to_string(),
                        ));
                    }
                    Err(FetchError::Network(e)) => {
                        error.set(Some(format!("Failed to fetch user: {}", e)));
                    }
                    Err(_) => {}
                }
            });
            || ()
        });
    }

    // Fetch stats
    let fetch_stats = {
        let stats = stats.clone();
        let error = error.clone();
        let loading = loading.clone();
        let navigator = navigator.clone();
        Callback::from(move |_| {
            let stats = stats.clone();
            let error = error.clone();
            let loading = loading.clone();
            let navigator = navigator.clone();
            spawn_local(async move {
                match utils::fetch_json::<AdminStats>("/api/admin/stats", On401::Ignore).await {
                    Ok(data) => {
                        stats.set(Some(data));
                        error.set(None);
                    }
                    Err(FetchError::Status(401)) => {
                        navigator.push(&Route::Home);
                        return;
                    }
                    Err(FetchError::Status(403)) => {
                        error.set(Some(
                            "Access denied. Admin privileges required.".to_string(),
                        ));
                    }
                    Err(FetchError::Status(code)) => {
                        error.set(Some(format!("Server error (HTTP {})", code)));
                    }
                    Err(FetchError::Decode(e)) => {
                        error.set(Some(format!("Failed to parse stats: {}", e)));
                    }
                    Err(FetchError::Network(e)) => {
                        error.set(Some(format!("Failed to fetch stats: {}", e)));
                    }
                }
                loading.set(false);
            });
        })
    };

    // Fetch users
    let fetch_users = {
        let users = users.clone();
        let error = error.clone();
        Callback::from(move |_| {
            let users = users.clone();
            let error = error.clone();
            spawn_local(async move {
                match utils::fetch_json::<AdminUsersResponse>("/api/admin/users", On401::Ignore)
                    .await
                {
                    Ok(data) => {
                        users.set(data.users);
                    }
                    Err(FetchError::Status(403)) => {}
                    Err(e) => {
                        error.set(Some(format!("Failed to fetch users: {}", e)));
                    }
                }
            });
        })
    };

    // Fetch sessions
    let fetch_sessions = {
        let sessions = sessions.clone();
        let error = error.clone();
        Callback::from(move |_| {
            let sessions = sessions.clone();
            let error = error.clone();
            spawn_local(async move {
                match utils::fetch_json::<AdminSessionsResponse>(
                    "/api/admin/sessions",
                    On401::Ignore,
                )
                .await
                {
                    Ok(data) => {
                        sessions.set(data.sessions);
                    }
                    Err(FetchError::Status(403)) => {}
                    Err(e) => {
                        error.set(Some(format!("Failed to fetch sessions: {}", e)));
                    }
                }
            });
        })
    };

    // Initial data fetch
    {
        let fetch_stats = fetch_stats.clone();
        let fetch_users = fetch_users.clone();
        let fetch_sessions = fetch_sessions.clone();
        use_effect_with((), move |_| {
            fetch_stats.emit(());
            fetch_users.emit(());
            fetch_sessions.emit(());
            || ()
        });
    }

    // Auto-refresh stats every 10 seconds
    {
        let fetch_stats = fetch_stats.clone();
        use_effect_with((), move |_| {
            let interval = gloo::timers::callback::Interval::new(10_000, move || {
                fetch_stats.emit(());
            });
            move || drop(interval)
        });
    }

    // Toggle admin handler
    let on_toggle_admin = {
        let users = users.clone();
        let confirm_action = confirm_action.clone();
        Callback::from(move |user_id: Uuid| {
            let users_inner = users.clone();
            let confirm_inner = confirm_action.clone();

            let target_user = users_inner.iter().find(|u| u.id == user_id).cloned();
            let is_currently_admin = target_user.as_ref().map(|u| u.is_admin).unwrap_or(false);
            let action_text = if is_currently_admin {
                "Remove admin privileges from this user?"
            } else {
                "Grant admin privileges to this user?"
            };

            let action = Callback::from(move |_: MouseEvent| {
                let users = users_inner.clone();
                let confirm = confirm_inner.clone();
                let new_admin_status = !is_currently_admin;
                spawn_local(async move {
                    let api_endpoint = utils::api_url(&format!("/api/admin/users/{}", user_id));
                    let body = UpdateUserRequest {
                        is_admin: Some(new_admin_status),
                        ..Default::default()
                    };
                    match Request::patch(&api_endpoint)
                        .json(&body)
                        .unwrap()
                        .send()
                        .await
                    {
                        Ok(response) => {
                            if response.status() == 204 {
                                let mut updated = (*users).clone();
                                if let Some(user) = updated.iter_mut().find(|u| u.id == user_id) {
                                    user.is_admin = new_admin_status;
                                }
                                users.set(updated);
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to update user: {:?}", e);
                        }
                    }
                    confirm.set(None);
                });
            });

            confirm_action.set(Some((action_text.to_string(), action)));
        })
    };

    // Toggle disabled/ban handler
    let on_toggle_disabled = {
        let users = users.clone();
        let confirm_action = confirm_action.clone();
        let ban_dialog = ban_dialog.clone();
        let ban_reason_input = ban_reason_input.clone();
        Callback::from(move |user_id: Uuid| {
            let users_inner = users.clone();
            let confirm_inner = confirm_action.clone();
            let ban_dialog = ban_dialog.clone();
            let ban_reason_input = ban_reason_input.clone();

            let target_user = users_inner.iter().find(|u| u.id == user_id).cloned();
            let is_currently_disabled = target_user.as_ref().map(|u| u.disabled).unwrap_or(false);

            if is_currently_disabled {
                // Unbanning - use simple confirmation
                let action_text = "Enable this user account?";
                let action = Callback::from(move |_: MouseEvent| {
                    let users = users_inner.clone();
                    let confirm = confirm_inner.clone();
                    spawn_local(async move {
                        let api_endpoint = utils::api_url(&format!("/api/admin/users/{}", user_id));
                        let body = UpdateUserRequest {
                            disabled: Some(false),
                            ban_reason: Some(None),
                            ..Default::default()
                        };
                        match Request::patch(&api_endpoint)
                            .json(&body)
                            .unwrap()
                            .send()
                            .await
                        {
                            Ok(response) => {
                                if response.status() == 204 {
                                    let mut updated = (*users).clone();
                                    if let Some(user) = updated.iter_mut().find(|u| u.id == user_id)
                                    {
                                        user.disabled = false;
                                    }
                                    users.set(updated);
                                }
                            }
                            Err(e) => {
                                log::error!("Failed to update user: {:?}", e);
                            }
                        }
                        confirm.set(None);
                    });
                });
                confirm_action.set(Some((action_text.to_string(), action)));
            } else {
                // Banning - show ban dialog with reason input
                ban_reason_input.set(String::new());
                ban_dialog.set(Some(user_id));
            }
        })
    };

    // Ban confirmation handler (called from ban dialog)
    let on_confirm_ban = {
        let users = users.clone();
        let ban_dialog = ban_dialog.clone();
        let ban_reason_input = ban_reason_input.clone();
        Callback::from(move |_: MouseEvent| {
            let users = users.clone();
            let ban_dialog = ban_dialog.clone();
            let reason = (*ban_reason_input).clone();

            if let Some(user_id) = *ban_dialog {
                spawn_local(async move {
                    let api_endpoint = utils::api_url(&format!("/api/admin/users/{}", user_id));
                    let body = UpdateUserRequest {
                        disabled: Some(true),
                        ban_reason: Some(if reason.is_empty() {
                            None
                        } else {
                            Some(reason)
                        }),
                        ..Default::default()
                    };
                    match Request::patch(&api_endpoint)
                        .json(&body)
                        .unwrap()
                        .send()
                        .await
                    {
                        Ok(response) => {
                            if response.status() == 204 {
                                let mut updated = (*users).clone();
                                if let Some(user) = updated.iter_mut().find(|u| u.id == user_id) {
                                    user.disabled = true;
                                }
                                users.set(updated);
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to ban user: {:?}", e);
                        }
                    }
                    ban_dialog.set(None);
                });
            }
        })
    };

    // Ban dialog cancel
    let on_cancel_ban = {
        let ban_dialog = ban_dialog.clone();
        Callback::from(move |_: MouseEvent| {
            ban_dialog.set(None);
        })
    };

    // Ban reason input change
    let on_ban_reason_change = {
        let ban_reason_input = ban_reason_input.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            ban_reason_input.set(input.value());
        })
    };

    // Delete session handler
    let on_delete_session = {
        let sessions = sessions.clone();
        let confirm_action = confirm_action.clone();
        let fetch_stats = fetch_stats.clone();
        Callback::from(move |session_id: Uuid| {
            let sessions_inner = sessions.clone();
            let confirm_inner = confirm_action.clone();
            let fetch_stats = fetch_stats.clone();

            let action = Callback::from(move |_: MouseEvent| {
                let sessions = sessions_inner.clone();
                let confirm = confirm_inner.clone();
                let fetch_stats = fetch_stats.clone();
                spawn_local(async move {
                    let api_endpoint =
                        utils::api_url(&format!("/api/admin/sessions/{}", session_id));
                    match Request::delete(&api_endpoint).send().await {
                        Ok(response) => {
                            if response.status() == 204 {
                                let updated: Vec<_> = (*sessions)
                                    .iter()
                                    .filter(|s| s.id != session_id)
                                    .cloned()
                                    .collect();
                                sessions.set(updated);
                                fetch_stats.emit(());
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to delete session: {:?}", e);
                        }
                    }
                    confirm.set(None);
                });
            });

            confirm_action.set(Some((
                "Delete this session? All message history will be lost.".to_string(),
                action,
            )));
        })
    };

    // Tab click handlers — one-line closure factory, see `make_sort_handler`
    // in `users_tab.rs` for the pattern.
    let make_tab_handler = |tab: AdminTab| {
        let active_tab = active_tab.clone();
        Callback::from(move |_: MouseEvent| active_tab.set(tab))
    };
    let on_overview_tab = make_tab_handler(AdminTab::Overview);
    let on_users_tab = make_tab_handler(AdminTab::Users);
    let on_sessions_tab = make_tab_handler(AdminTab::Sessions);

    // Cancel confirmation
    let on_cancel_confirm = {
        let confirm_action = confirm_action.clone();
        Callback::from(move |_| confirm_action.set(None))
    };

    // Back to dashboard
    let go_back = {
        let on_close = props.on_close.clone();
        Callback::from(move |_| on_close.emit(()))
    };

    html! {
        <div class="admin-container">
            <header class="admin-header">
                <button class="header-button" onclick={go_back}>
                    { "< Back" }
                </button>
                <h1>{ "Admin Dashboard" }</h1>
            </header>

            {
                if let Some(ref err) = *error {
                    html! {
                        <div class="admin-error">
                            { err }
                        </div>
                    }
                } else {
                    html! {}
                }
            }

            {
                if *loading {
                    html! {
                        <div class="admin-loading">
                            <div class="spinner"></div>
                            <p>{ "Loading admin data..." }</p>
                        </div>
                    }
                } else {
                    html! {
                        <>
                            <nav class="admin-tabs">
                                <button
                                    class={classes!("tab-btn", if *active_tab == AdminTab::Overview { Some("active") } else { None })}
                                    onclick={on_overview_tab}
                                >
                                    { "Overview" }
                                </button>
                                <button
                                    class={classes!("tab-btn", if *active_tab == AdminTab::Users { Some("active") } else { None })}
                                    onclick={on_users_tab}
                                >
                                    { format!("Users ({})", users.len()) }
                                </button>
                                <button
                                    class={classes!("tab-btn", if *active_tab == AdminTab::Sessions { Some("active") } else { None })}
                                    onclick={on_sessions_tab}
                                >
                                    { format!("Sessions ({})", sessions.len()) }
                                </button>
                            </nav>

                            <div class="admin-content">
                                {
                                    match *active_tab {
                                        AdminTab::Overview => {
                                            html! {
                                                <AdminOverviewTab stats={(*stats).clone()} />
                                            }
                                        }
                                        AdminTab::Users => {
                                            html! {
                                                <AdminUsersTab
                                                    users={(*users).clone()}
                                                    on_toggle_admin={on_toggle_admin.clone()}
                                                    on_toggle_disabled={on_toggle_disabled.clone()}
                                                    current_user_id={current_user_id.unwrap_or_default()}
                                                />
                                            }
                                        }
                                        AdminTab::Sessions => {
                                            html! {
                                                <AdminSessionsTab
                                                    sessions={(*sessions).clone()}
                                                    on_delete={on_delete_session.clone()}
                                                />
                                            }
                                        }
                                    }
                                }
                            </div>
                        </>
                    }
                }
            }

            // Confirmation modal
            {
                if let Some((ref message, ref action)) = *confirm_action {
                    html! {
                        <ConfirmModal
                            message={message.clone()}
                            on_confirm={action.clone()}
                            on_cancel={on_cancel_confirm.clone()}
                        />
                    }
                } else {
                    html! {}
                }
            }

            // Ban dialog modal
            {
                if ban_dialog.is_some() {
                    html! {
                        <div class="modal-overlay" onclick={on_cancel_ban.clone()}>
                            <div class="modal-content ban-modal" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                                <h3>{ "Ban User" }</h3>
                                <p>{ "This will disable the user account and revoke all their access tokens. They will be unable to log in." }</p>
                                <div class="ban-reason-input">
                                    <label for="ban-reason">{ "Reason for ban (shown to user):" }</label>
                                    <input
                                        type="text"
                                        id="ban-reason"
                                        placeholder="e.g., Violation of terms of service"
                                        value={(*ban_reason_input).clone()}
                                        oninput={on_ban_reason_change.clone()}
                                    />
                                </div>
                                <div class="modal-actions">
                                    <button class="modal-cancel" onclick={on_cancel_ban.clone()}>{ "Cancel" }</button>
                                    <button class="modal-confirm ban-confirm" onclick={on_confirm_ban.clone()}>{ "Ban User" }</button>
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
