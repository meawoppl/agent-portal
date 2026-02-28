//! Admin sessions tab — session management table

use crate::utils;
use uuid::Uuid;
use web_sys::MouseEvent;
use yew::prelude::*;

use super::AdminSessionInfo;

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

#[derive(Properties, PartialEq)]
pub struct AdminSessionsTabProps {
    pub sessions: Vec<AdminSessionInfo>,
    pub on_delete: Callback<Uuid>,
}

#[function_component(AdminSessionsTab)]
pub fn admin_sessions_tab(props: &AdminSessionsTabProps) -> Html {
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
                        props.sessions.iter().map(|session| {
                            html! {
                                <SessionRow
                                    key={session.id.to_string()}
                                    session={session.clone()}
                                    on_delete={props.on_delete.clone()}
                                />
                            }
                        }).collect::<Html>()
                    }
                </tbody>
            </table>
        </div>
    }
}
