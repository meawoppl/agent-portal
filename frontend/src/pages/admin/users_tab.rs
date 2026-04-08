//! Admin users tab — user management table with sortable columns

use crate::utils;
use uuid::Uuid;
use web_sys::MouseEvent;
use yew::prelude::*;

use super::AdminUserInfo;

#[derive(Clone, Copy, PartialEq)]
enum SortColumn {
    Email,
    Name,
    Status,
    Sessions,
    Spend,
    Created,
}

#[derive(Clone, Copy, PartialEq)]
enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    fn toggle(self) -> Self {
        match self {
            SortDirection::Asc => SortDirection::Desc,
            SortDirection::Desc => SortDirection::Asc,
        }
    }

    fn indicator(self) -> &'static str {
        match self {
            SortDirection::Asc => " \u{25b2}",
            SortDirection::Desc => " \u{25bc}",
        }
    }
}

fn sort_users(
    users: &[AdminUserInfo],
    column: SortColumn,
    direction: SortDirection,
) -> Vec<AdminUserInfo> {
    let mut sorted = users.to_vec();
    sorted.sort_by(|a, b| {
        let cmp = match column {
            SortColumn::Email => a.email.to_lowercase().cmp(&b.email.to_lowercase()),
            SortColumn::Name => {
                let a_name = a.name.as_deref().unwrap_or("");
                let b_name = b.name.as_deref().unwrap_or("");
                a_name.to_lowercase().cmp(&b_name.to_lowercase())
            }
            SortColumn::Status => {
                fn rank(u: &AdminUserInfo) -> u8 {
                    if u.disabled {
                        2
                    } else if u.is_admin {
                        0
                    } else {
                        1
                    }
                }
                rank(a).cmp(&rank(b))
            }
            SortColumn::Sessions => a.session_count.cmp(&b.session_count),
            SortColumn::Spend => a
                .total_spend_usd
                .partial_cmp(&b.total_spend_usd)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortColumn::Created => a.created_at.cmp(&b.created_at),
        };
        match direction {
            SortDirection::Asc => cmp,
            SortDirection::Desc => cmp.reverse(),
        }
    });
    sorted
}

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
    let sort_column = use_state(|| None::<SortColumn>);
    let sort_direction = use_state(|| SortDirection::Desc);

    let sorted_users = {
        match *sort_column {
            Some(col) => sort_users(&props.users, col, *sort_direction),
            None => props.users.clone(),
        }
    };

    let make_sort_handler = |col: SortColumn| {
        let sort_column = sort_column.clone();
        let sort_direction = sort_direction.clone();
        Callback::from(move |_: MouseEvent| {
            if *sort_column == Some(col) {
                sort_direction.set((*sort_direction).toggle());
            } else {
                sort_column.set(Some(col));
                sort_direction.set(SortDirection::Asc);
            }
        })
    };

    let header = |label: &str, col: SortColumn| {
        let active = *sort_column == Some(col);
        let indicator = if active {
            (*sort_direction).indicator()
        } else {
            ""
        };
        html! {
            <th class="sortable" onclick={make_sort_handler(col)}>
                { label }{ indicator }
            </th>
        }
    };

    html! {
        <div class="admin-users">
            <table class="admin-table">
                <thead>
                    <tr>
                        { header("Email", SortColumn::Email) }
                        { header("Name", SortColumn::Name) }
                        { header("Status", SortColumn::Status) }
                        { header("Sessions", SortColumn::Sessions) }
                        { header("Spend", SortColumn::Spend) }
                        { header("Created", SortColumn::Created) }
                        <th>{ "Actions" }</th>
                    </tr>
                </thead>
                <tbody>
                    {
                        sorted_users.iter().map(|user| {
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
