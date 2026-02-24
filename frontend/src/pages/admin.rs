//! Admin dashboard page
//!
//! Restricted to users with is_admin=true. Provides system overview,
//! user management, and session management capabilities.

use crate::utils;
use crate::Route;
use gloo_net::http::Request;
use serde::Deserialize;
use shared::api::UpdateUserRequest;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::MouseEvent;
use yew::prelude::*;
use yew_router::prelude::*;

/// Admin page tabs
#[derive(Clone, Copy, PartialEq)]
enum AdminTab {
    Overview,
    Users,
    Sessions,
    RawMessages,
}

// ============================================================================
// API Response Types
// ============================================================================

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct AdminStats {
    total_users: i64,
    admin_users: i64,
    disabled_users: i64,
    total_sessions: i64,
    active_sessions: i64,
    connected_proxy_clients: usize,
    connected_web_clients: usize,
    total_spend_usd: f64,
    total_input_tokens: i64,
    total_output_tokens: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct AdminUserInfo {
    id: Uuid,
    email: String,
    name: Option<String>,
    is_admin: bool,
    disabled: bool,
    voice_enabled: bool,
    created_at: String,
    session_count: i64,
    total_spend_usd: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct AdminUsersResponse {
    users: Vec<AdminUserInfo>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct AdminSessionInfo {
    id: Uuid,
    user_email: String,
    session_name: String,
    working_directory: String,
    git_branch: Option<String>,
    status: String,
    total_cost_usd: f64,
    last_activity: String,
    is_connected: bool,
    #[serde(default)]
    hostname: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AdminSessionsResponse {
    sessions: Vec<AdminSessionInfo>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
struct RawMessageLogInfo {
    id: Uuid,
    session_id: Option<Uuid>,
    message_content: serde_json::Value,
    message_source: String,
    render_reason: Option<String>,
    created_at: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RawMessagesResponse {
    logs: Vec<RawMessageLogInfo>,
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Format token count with K/M suffix for readability
fn format_tokens(count: i64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}K", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

// ============================================================================
// Stats Card Component
// ============================================================================

#[derive(Properties, PartialEq)]
struct StatCardProps {
    label: String,
    value: String,
    #[prop_or_default]
    subvalue: Option<String>,
    #[prop_or_default]
    class: Option<String>,
}

#[function_component(StatCard)]
fn stat_card(props: &StatCardProps) -> Html {
    let class = classes!("admin-stat-card", props.class.clone());
    html! {
        <div class={class}>
            <div class="stat-value">{ &props.value }</div>
            <div class="stat-label">{ &props.label }</div>
            {
                if let Some(ref sub) = props.subvalue {
                    html! { <div class="stat-subvalue">{ sub }</div> }
                } else {
                    html! {}
                }
            }
        </div>
    }
}

// ============================================================================
// User Row Component
// ============================================================================

#[derive(Properties, PartialEq)]
struct UserRowProps {
    user: AdminUserInfo,
    on_toggle_admin: Callback<Uuid>,
    on_toggle_disabled: Callback<Uuid>,
    on_toggle_voice: Callback<Uuid>,
    current_user_id: Uuid,
}

#[function_component(UserRow)]
fn user_row(props: &UserRowProps) -> Html {
    let user = &props.user;
    let is_self = user.id == props.current_user_id;

    let on_toggle_admin = {
        let callback = props.on_toggle_admin.clone();
        let user_id = user.id;
        Callback::from(move |_: MouseEvent| callback.emit(user_id))
    };

    let on_toggle_disabled = {
        let callback = props.on_toggle_disabled.clone();
        let user_id = user.id;
        Callback::from(move |_: MouseEvent| callback.emit(user_id))
    };

    let on_toggle_voice = {
        let callback = props.on_toggle_voice.clone();
        let user_id = user.id;
        Callback::from(move |_: MouseEvent| callback.emit(user_id))
    };

    let status_class = if user.disabled {
        "user-status disabled"
    } else if user.is_admin {
        "user-status admin"
    } else {
        "user-status active"
    };

    let status_text = if user.disabled {
        "Disabled"
    } else if user.is_admin {
        "Admin"
    } else {
        "User"
    };

    html! {
        <tr class={classes!(if is_self { Some("self-row") } else { None })}>
            <td class="user-email">
                { &user.email }
                { if is_self { html! { <span class="you-badge">{ "(you)" }</span> } } else { html! {} } }
            </td>
            <td>{ user.name.as_deref().unwrap_or("-") }</td>
            <td class={status_class}>{ status_text }</td>
            <td class="numeric">{ user.session_count }</td>
            <td class="numeric">{ utils::format_dollars(user.total_spend_usd) }</td>
            <td class="timestamp">{ utils::format_timestamp(&user.created_at) }</td>
            <td class="actions">
                <button
                    class={classes!("admin-toggle", if user.is_admin { Some("active") } else { None })}
                    onclick={on_toggle_admin}
                    disabled={is_self}
                    title={if is_self { "Cannot change your own admin status" } else if user.is_admin { "Remove admin" } else { "Make admin" }}
                >
                    { if user.is_admin { "Remove Admin" } else { "Make Admin" } }
                </button>
                <button
                    class={classes!("ban-toggle", if user.disabled { Some("active") } else { None })}
                    onclick={on_toggle_disabled}
                    disabled={is_self}
                    title={if is_self { "Cannot ban your own account" } else if user.disabled { "Unban user" } else { "Ban user" }}
                >
                    { if user.disabled { "Unban" } else { "Ban" } }
                </button>
                <button
                    class={classes!("voice-toggle", if user.voice_enabled { Some("active") } else { None })}
                    onclick={on_toggle_voice}
                    title={if user.voice_enabled { "Disable voice input" } else { "Enable voice input" }}
                >
                    { if user.voice_enabled { "Voice: On" } else { "Voice: Off" } }
                </button>
            </td>
        </tr>
    }
}

// ============================================================================
// Session Row Component
// ============================================================================

#[derive(Properties, PartialEq)]
struct SessionRowProps {
    session: AdminSessionInfo,
    on_delete: Callback<Uuid>,
}

#[function_component(SessionRow)]
fn session_row(props: &SessionRowProps) -> Html {
    let session = &props.session;

    let on_delete = {
        let callback = props.on_delete.clone();
        let session_id = session.id;
        Callback::from(move |_: MouseEvent| callback.emit(session_id))
    };

    let status_class = if session.is_connected {
        "session-status connected"
    } else if session.status == "active" {
        "session-status active"
    } else {
        "session-status disconnected"
    };

    let status_text = if session.is_connected {
        "Connected"
    } else {
        &session.status
    };

    let hostname = &session.hostname;
    let project_name = utils::extract_folder(&session.working_directory);

    html! {
        <tr>
            <td class="session-user">{ &session.user_email }</td>
            <td class="session-hostname">{ hostname }</td>
            <td class="session-project">{ project_name }</td>
            <td class="session-branch">{ session.git_branch.as_deref().unwrap_or("-") }</td>
            <td class={status_class}>{ status_text }</td>
            <td class="numeric">{ utils::format_dollars(session.total_cost_usd) }</td>
            <td class="timestamp">{ utils::format_timestamp(&session.last_activity) }</td>
            <td class="actions">
                <button class="delete-btn" onclick={on_delete} title="Delete session">
                    { "Delete" }
                </button>
            </td>
        </tr>
    }
}

// ============================================================================
// Raw Message Row Component
// ============================================================================

#[derive(Properties, PartialEq)]
struct RawMessageRowProps {
    message: RawMessageLogInfo,
    on_delete: Callback<Uuid>,
    on_view: Callback<RawMessageLogInfo>,
}

#[function_component(RawMessageRow)]
fn raw_message_row(props: &RawMessageRowProps) -> Html {
    let msg = &props.message;

    let on_delete = {
        let callback = props.on_delete.clone();
        let msg_id = msg.id;
        Callback::from(move |_: MouseEvent| callback.emit(msg_id))
    };

    let on_view = {
        let callback = props.on_view.clone();
        let message = msg.clone();
        Callback::from(move |_: MouseEvent| callback.emit(message.clone()))
    };

    // Get message type from content if available
    let msg_type = msg
        .message_content
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("unknown");

    // Truncate session ID for display
    let session_id_display = msg
        .session_id
        .map(|id| format!("{}...", &id.to_string()[..8]))
        .unwrap_or_else(|| "-".to_string());

    html! {
        <tr>
            <td class="timestamp">{ utils::format_timestamp(&msg.created_at) }</td>
            <td class="raw-msg-type">{ msg_type }</td>
            <td class="raw-msg-source">{ &msg.message_source }</td>
            <td class="raw-msg-reason">{ msg.render_reason.as_deref().unwrap_or("-") }</td>
            <td class="raw-msg-session" title={msg.session_id.map(|id| id.to_string()).unwrap_or_default()}>
                { session_id_display }
            </td>
            <td class="actions">
                <button class="view-btn" onclick={on_view} title="View message content">
                    { "View" }
                </button>
                <button class="delete-btn" onclick={on_delete} title="Delete">
                    { "Delete" }
                </button>
            </td>
        </tr>
    }
}

// ============================================================================
// Main Admin Page Component
// ============================================================================

#[function_component(AdminPage)]
pub fn admin_page() -> Html {
    let active_tab = use_state(|| AdminTab::Overview);
    let stats = use_state(|| None::<AdminStats>);
    let users = use_state(Vec::<AdminUserInfo>::new);
    let sessions = use_state(Vec::<AdminSessionInfo>::new);
    let raw_messages = use_state(Vec::<RawMessageLogInfo>::new);
    let viewing_raw_message = use_state(|| None::<RawMessageLogInfo>);
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
                let api_endpoint = utils::api_url("/api/auth/me");
                match Request::get(&api_endpoint).send().await {
                    Ok(response) => {
                        if response.status() == 401 {
                            navigator.push(&Route::Home);
                            return;
                        }
                        if response.status() == 403 {
                            error.set(Some(
                                "Access denied. Admin privileges required.".to_string(),
                            ));
                            return;
                        }
                        if let Ok(data) = response.json::<serde_json::Value>().await {
                            if let Some(id) = data.get("id").and_then(|v| v.as_str()) {
                                if let Ok(uuid) = id.parse::<Uuid>() {
                                    current_user_id.set(Some(uuid));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error.set(Some(format!("Failed to fetch user: {:?}", e)));
                    }
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
                let api_endpoint = utils::api_url("/api/admin/stats");
                match Request::get(&api_endpoint).send().await {
                    Ok(response) => {
                        if response.status() == 401 {
                            navigator.push(&Route::Home);
                            return;
                        }
                        if response.status() == 403 {
                            error.set(Some(
                                "Access denied. Admin privileges required.".to_string(),
                            ));
                            loading.set(false);
                            return;
                        }
                        match response.json::<AdminStats>().await {
                            Ok(data) => {
                                stats.set(Some(data));
                                error.set(None);
                            }
                            Err(e) => {
                                error.set(Some(format!("Failed to parse stats: {:?}", e)));
                            }
                        }
                    }
                    Err(e) => {
                        error.set(Some(format!("Failed to fetch stats: {:?}", e)));
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
                let api_endpoint = utils::api_url("/api/admin/users");
                match Request::get(&api_endpoint).send().await {
                    Ok(response) => {
                        if response.status() == 403 {
                            return;
                        }
                        match response.json::<AdminUsersResponse>().await {
                            Ok(data) => {
                                users.set(data.users);
                            }
                            Err(e) => {
                                error.set(Some(format!("Failed to parse users: {:?}", e)));
                            }
                        }
                    }
                    Err(e) => {
                        error.set(Some(format!("Failed to fetch users: {:?}", e)));
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
                let api_endpoint = utils::api_url("/api/admin/sessions");
                match Request::get(&api_endpoint).send().await {
                    Ok(response) => {
                        if response.status() == 403 {
                            return;
                        }
                        match response.json::<AdminSessionsResponse>().await {
                            Ok(data) => {
                                sessions.set(data.sessions);
                            }
                            Err(e) => {
                                error.set(Some(format!("Failed to parse sessions: {:?}", e)));
                            }
                        }
                    }
                    Err(e) => {
                        error.set(Some(format!("Failed to fetch sessions: {:?}", e)));
                    }
                }
            });
        })
    };

    // Fetch raw messages
    let fetch_raw_messages = {
        let raw_messages = raw_messages.clone();
        let error = error.clone();
        Callback::from(move |_| {
            let raw_messages = raw_messages.clone();
            let error = error.clone();
            spawn_local(async move {
                let api_endpoint = utils::api_url("/api/admin/raw-messages");
                match Request::get(&api_endpoint).send().await {
                    Ok(response) => {
                        if response.status() == 403 {
                            return;
                        }
                        match response.json::<RawMessagesResponse>().await {
                            Ok(data) => {
                                raw_messages.set(data.logs);
                            }
                            Err(e) => {
                                error.set(Some(format!("Failed to parse raw messages: {:?}", e)));
                            }
                        }
                    }
                    Err(e) => {
                        error.set(Some(format!("Failed to fetch raw messages: {:?}", e)));
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
        let fetch_raw_messages = fetch_raw_messages.clone();
        use_effect_with((), move |_| {
            fetch_stats.emit(());
            fetch_users.emit(());
            fetch_sessions.emit(());
            fetch_raw_messages.emit(());
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

    // Toggle voice handler
    let on_toggle_voice = {
        let users = users.clone();
        let confirm_action = confirm_action.clone();
        Callback::from(move |user_id: Uuid| {
            let users_inner = users.clone();
            let confirm_inner = confirm_action.clone();

            let target_user = users_inner.iter().find(|u| u.id == user_id).cloned();
            let is_currently_enabled = target_user
                .as_ref()
                .map(|u| u.voice_enabled)
                .unwrap_or(false);
            let action_text = if is_currently_enabled {
                "Disable voice input for this user?"
            } else {
                "Enable voice input for this user?"
            };

            let action = Callback::from(move |_: MouseEvent| {
                let users = users_inner.clone();
                let confirm = confirm_inner.clone();
                let new_voice_status = !is_currently_enabled;
                spawn_local(async move {
                    let api_endpoint = utils::api_url(&format!("/api/admin/users/{}", user_id));
                    let body = UpdateUserRequest {
                        voice_enabled: Some(new_voice_status),
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
                                    user.voice_enabled = new_voice_status;
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

    // Delete raw message handler (no confirmation needed)
    let on_delete_raw_message = {
        let raw_messages = raw_messages.clone();
        Callback::from(move |msg_id: Uuid| {
            let raw_messages = raw_messages.clone();
            spawn_local(async move {
                let api_endpoint = utils::api_url(&format!("/api/admin/raw-messages/{}", msg_id));
                match Request::delete(&api_endpoint).send().await {
                    Ok(response) => {
                        if response.status() == 204 {
                            let updated: Vec<_> = (*raw_messages)
                                .iter()
                                .filter(|m| m.id != msg_id)
                                .cloned()
                                .collect();
                            raw_messages.set(updated);
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to delete raw message: {:?}", e);
                    }
                }
            });
        })
    };

    // View raw message handler
    let on_view_raw_message = {
        let viewing_raw_message = viewing_raw_message.clone();
        Callback::from(move |msg: RawMessageLogInfo| {
            viewing_raw_message.set(Some(msg));
        })
    };

    // Close raw message viewer
    let on_close_raw_message_viewer = {
        let viewing_raw_message = viewing_raw_message.clone();
        Callback::from(move |_| viewing_raw_message.set(None))
    };

    // Tab click handlers
    let on_overview_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(AdminTab::Overview))
    };
    let on_users_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(AdminTab::Users))
    };
    let on_sessions_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(AdminTab::Sessions))
    };
    let on_raw_messages_tab = {
        let active_tab = active_tab.clone();
        Callback::from(move |_| active_tab.set(AdminTab::RawMessages))
    };

    // Cancel confirmation
    let on_cancel_confirm = {
        let confirm_action = confirm_action.clone();
        Callback::from(move |_| confirm_action.set(None))
    };

    // Back to dashboard
    let go_back = {
        let navigator = navigator.clone();
        Callback::from(move |_| navigator.push(&Route::Dashboard))
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
                                <button
                                    class={classes!("tab-btn", if *active_tab == AdminTab::RawMessages { Some("active") } else { None })}
                                    onclick={on_raw_messages_tab}
                                >
                                    { format!("Raw Messages ({})", raw_messages.len()) }
                                </button>
                            </nav>

                            <div class="admin-content">
                                {
                                    match *active_tab {
                                        AdminTab::Overview => {
                                            if let Some(ref s) = *stats {
                                                html! {
                                                    <div class="admin-overview">
                                                        <div class="stats-grid">
                                                            <StatCard
                                                                label="Total Users"
                                                                value={s.total_users.to_string()}
                                                                subvalue={Some(format!("{} admins, {} disabled", s.admin_users, s.disabled_users))}
                                                            />
                                                            <StatCard
                                                                label="Total Sessions"
                                                                value={s.total_sessions.to_string()}
                                                                subvalue={Some(format!("{} active", s.active_sessions))}
                                                            />
                                                            <StatCard
                                                                label="Connected Clients"
                                                                value={format!("{}", s.connected_proxy_clients + s.connected_web_clients)}
                                                                subvalue={Some(format!("{} proxy, {} web", s.connected_proxy_clients, s.connected_web_clients))}
                                                            />
                                                            <StatCard
                                                                label="Total API Spend"
                                                                value={utils::format_dollars(s.total_spend_usd)}
                                                                class="spend-card"
                                                            />
                                                            <StatCard
                                                                label="Input Tokens"
                                                                value={format_tokens(s.total_input_tokens)}
                                                            />
                                                            <StatCard
                                                                label="Output Tokens"
                                                                value={format_tokens(s.total_output_tokens)}
                                                            />
                                                        </div>
                                                    </div>
                                                }
                                            } else {
                                                html! { <p>{ "No stats available" }</p> }
                                            }
                                        }
                                        AdminTab::Users => {
                                            html! {
                                                <div class="admin-users">
                                                    <table class="admin-table">
                                                        <thead>
                                                            <tr>
                                                                <th>{ "Email" }</th>
                                                                <th>{ "Name" }</th>
                                                                <th>{ "Status" }</th>
                                                                <th>{ "Sessions" }</th>
                                                                <th>{ "Spend" }</th>
                                                                <th>{ "Created" }</th>
                                                                <th>{ "Actions" }</th>
                                                            </tr>
                                                        </thead>
                                                        <tbody>
                                                            {
                                                                users.iter().map(|user| {
                                                                    html! {
                                                                        <UserRow
                                                                            key={user.id.to_string()}
                                                                            user={user.clone()}
                                                                            on_toggle_admin={on_toggle_admin.clone()}
                                                                            on_toggle_disabled={on_toggle_disabled.clone()}
                                                                            on_toggle_voice={on_toggle_voice.clone()}
                                                                            current_user_id={current_user_id.unwrap_or_default()}
                                                                        />
                                                                    }
                                                                }).collect::<Html>()
                                                            }
                                                        </tbody>
                                                    </table>
                                                </div>
                                            }
                                        }
                                        AdminTab::Sessions => {
                                            html! {
                                                <div class="admin-sessions">
                                                    <table class="admin-table">
                                                        <thead>
                                                            <tr>
                                                                <th>{ "User" }</th>
                                                                <th>{ "Hostname" }</th>
                                                                <th>{ "Project" }</th>
                                                                <th>{ "Branch" }</th>
                                                                <th>{ "Status" }</th>
                                                                <th>{ "Cost" }</th>
                                                                <th>{ "Last Activity" }</th>
                                                                <th>{ "Actions" }</th>
                                                            </tr>
                                                        </thead>
                                                        <tbody>
                                                            {
                                                                sessions.iter().map(|session| {
                                                                    html! {
                                                                        <SessionRow
                                                                            key={session.id.to_string()}
                                                                            session={session.clone()}
                                                                            on_delete={on_delete_session.clone()}
                                                                        />
                                                                    }
                                                                }).collect::<Html>()
                                                            }
                                                        </tbody>
                                                    </table>
                                                </div>
                                            }
                                        }
                                        AdminTab::RawMessages => {
                                            html! {
                                                <div class="admin-raw-messages">
                                                    <p class="raw-messages-description">
                                                        { "Messages that failed to parse and were rendered as raw JSON are logged here for debugging." }
                                                    </p>
                                                    {
                                                        if raw_messages.is_empty() {
                                                            html! {
                                                                <p class="no-raw-messages">{ "No raw messages logged yet." }</p>
                                                            }
                                                        } else {
                                                            html! {
                                                                <table class="admin-table">
                                                                    <thead>
                                                                        <tr>
                                                                            <th>{ "Time" }</th>
                                                                            <th>{ "Type" }</th>
                                                                            <th>{ "Source" }</th>
                                                                            <th>{ "Reason" }</th>
                                                                            <th>{ "Session" }</th>
                                                                            <th>{ "Actions" }</th>
                                                                        </tr>
                                                                    </thead>
                                                                    <tbody>
                                                                        {
                                                                            raw_messages.iter().map(|msg| {
                                                                                html! {
                                                                                    <RawMessageRow
                                                                                        key={msg.id.to_string()}
                                                                                        message={msg.clone()}
                                                                                        on_delete={on_delete_raw_message.clone()}
                                                                                        on_view={on_view_raw_message.clone()}
                                                                                    />
                                                                                }
                                                                            }).collect::<Html>()
                                                                        }
                                                                    </tbody>
                                                                </table>
                                                            }
                                                        }
                                                    }
                                                </div>
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
                        <div class="modal-overlay" onclick={on_cancel_confirm.clone()}>
                            <div class="modal-content confirm-modal" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                                <p>{ message }</p>
                                <div class="modal-actions">
                                    <button class="modal-cancel" onclick={on_cancel_confirm.clone()}>{ "Cancel" }</button>
                                    <button class="modal-confirm" onclick={action.clone()}>{ "Confirm" }</button>
                                </div>
                            </div>
                        </div>
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

            // Raw message viewer modal
            {
                if let Some(ref msg) = *viewing_raw_message {
                    // Display as plain text - if it's a JSON string, extract the string content,
                    // otherwise show the raw JSON representation. This avoids confusing escape
                    // sequences when the original content wasn't valid JSON.
                    let display_content = match &msg.message_content {
                        serde_json::Value::String(s) => s.clone(),
                        other => serde_json::to_string_pretty(other)
                            .unwrap_or_else(|_| other.to_string()),
                    };
                    html! {
                        <div class="modal-overlay" onclick={on_close_raw_message_viewer.clone()}>
                            <div class="modal-content raw-message-modal" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                                <div class="raw-message-modal-header">
                                    <h3>{ "Raw Message Content" }</h3>
                                    <button class="modal-close" onclick={on_close_raw_message_viewer.clone()}>{ "×" }</button>
                                </div>
                                <div class="raw-message-modal-meta">
                                    <span><strong>{ "Source: " }</strong>{ &msg.message_source }</span>
                                    <span><strong>{ "Reason: " }</strong>{ msg.render_reason.as_deref().unwrap_or("-") }</span>
                                    <span><strong>{ "Time: " }</strong>{ utils::format_timestamp(&msg.created_at) }</span>
                                </div>
                                <pre class="raw-message-modal-content">{ display_content }</pre>
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
