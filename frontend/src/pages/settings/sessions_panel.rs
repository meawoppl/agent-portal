use crate::components::ShareDialog;
use crate::utils;
use gloo_net::http::Request;
use shared::SessionInfo;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

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

    let is_owner = session.my_role == "owner";

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

#[derive(Properties, PartialEq)]
pub struct SessionsPanelProps {
    pub on_sessions_loaded: Callback<Vec<SessionInfo>>,
}

#[function_component(SessionsPanel)]
pub fn sessions_panel(props: &SessionsPanelProps) -> Html {
    let sessions = use_state(Vec::<SessionInfo>::new);
    let sessions_loading = use_state(|| true);
    let share_session_id = use_state(|| None::<Uuid>);
    let confirm_action = use_state(|| None::<(String, Callback<MouseEvent>)>);

    let fetch_sessions = {
        let sessions = sessions.clone();
        let sessions_loading = sessions_loading.clone();
        let on_sessions_loaded = props.on_sessions_loaded.clone();

        Callback::from(move |_| {
            let sessions = sessions.clone();
            let sessions_loading = sessions_loading.clone();
            let on_sessions_loaded = on_sessions_loaded.clone();

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
                                    on_sessions_loaded.emit(parsed.clone());
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
        let fetch_sessions = fetch_sessions.clone();
        use_effect_with((), move |_| {
            fetch_sessions.emit(());
            || ()
        });
    }

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

    let on_share_session = {
        let share_session_id = share_session_id.clone();
        Callback::from(move |session_id: Uuid| {
            share_session_id.set(Some(session_id));
        })
    };

    let on_close_share = {
        let share_session_id = share_session_id.clone();
        Callback::from(move |_| {
            share_session_id.set(None);
        })
    };

    let cancel_confirm = {
        let confirm_action = confirm_action.clone();
        Callback::from(move |_| {
            confirm_action.set(None);
        })
    };

    html! {
        <>
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

            if let Some(session_id) = *share_session_id {
                <ShareDialog
                    session_id={session_id}
                    on_close={on_close_share.clone()}
                />
            }
        </>
    }
}
