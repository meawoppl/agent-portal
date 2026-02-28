//! Admin users tab — user management table

use crate::utils;
use uuid::Uuid;
use web_sys::MouseEvent;
use yew::prelude::*;

use super::AdminUserInfo;

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

#[derive(Properties, PartialEq)]
pub struct AdminUsersTabProps {
    pub users: Vec<AdminUserInfo>,
    pub on_toggle_admin: Callback<Uuid>,
    pub on_toggle_disabled: Callback<Uuid>,
    pub on_toggle_voice: Callback<Uuid>,
    pub current_user_id: Uuid,
}

#[function_component(AdminUsersTab)]
pub fn admin_users_tab(props: &AdminUsersTabProps) -> Html {
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
                        props.users.iter().map(|user| {
                            html! {
                                <UserRow
                                    key={user.id.to_string()}
                                    user={user.clone()}
                                    on_toggle_admin={props.on_toggle_admin.clone()}
                                    on_toggle_disabled={props.on_toggle_disabled.clone()}
                                    on_toggle_voice={props.on_toggle_voice.clone()}
                                    current_user_id={props.current_user_id}
                                />
                            }
                        }).collect::<Html>()
                    }
                </tbody>
            </table>
        </div>
    }
}
