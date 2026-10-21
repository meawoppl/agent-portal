//! Import picker owns its drafts and is keyed by launcher, so changing machines
//! cannot apply an old response or selected thread to the new machine.
use shared::api::{
    DashboardRequest, DashboardResponse, ExternalSession, ExternalSessionQuery,
    ImportSessionRequest,
};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub launcher_id: Uuid,
    pub supported: bool,
    pub on_open: Callback<Uuid>,
}

#[function_component(SessionImport)]
pub fn session_import(props: &Props) -> Html {
    let sessions = use_state(Vec::<ExternalSession>::new);
    let cursor = use_state(|| None::<String>);
    let search = use_state(String::new);
    let directory = use_state(String::new);
    let selected = use_state(HashSet::<String>::new);
    let failures = use_state(HashMap::<String, String>::new);
    let error = use_state(|| None::<String>);
    let busy = use_state(|| false);
    let loaded = use_state(|| false);
    let include_history = use_state(|| true);
    let imported_count = use_state(|| 0usize);
    let applied_filters = use_state(|| (String::new(), String::new()));
    let launcher_id = props.launcher_id;
    let fetch: Callback<bool> = {
        let applied_filters = applied_filters.clone();
        let (sessions, cursor, search, directory, selected, failures, error, busy, loaded) = (
            sessions.clone(),
            cursor.clone(),
            search.clone(),
            directory.clone(),
            selected.clone(),
            failures.clone(),
            error.clone(),
            busy.clone(),
            loaded.clone(),
        );
        Callback::from(move |more: bool| {
            if *busy {
                return;
            }
            if more && *applied_filters != ((*search).clone(), (*directory).clone()) {
                return;
            }
            if !more {
                applied_filters.set(((*search).clone(), (*directory).clone()));
            }
            let query = ExternalSessionQuery {
                search: Some((*search).clone()),
                working_directory: Some((*directory).clone()),
                cursor: if more { (*cursor).clone() } else { None },
            };
            let (sessions, cursor, selected, failures, error, busy, loaded) = (
                sessions.clone(),
                cursor.clone(),
                selected.clone(),
                failures.clone(),
                error.clone(),
                busy.clone(),
                loaded.clone(),
            );
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                let result =
                    match crate::dashboard_rpc::request(DashboardRequest::ListExternalSessions {
                        launcher_id,
                        agent_type: shared::AgentType::Codex,
                        query,
                    })
                    .await
                    {
                        Ok(DashboardResponse::ExternalSessions(page)) => Ok(page),
                        Ok(_) => Err("Unexpected session listing reply".into()),
                        Err(e) => Err(e.message),
                    };
                match result {
                    Ok(page) => {
                        let mut rows = if more { (*sessions).clone() } else { vec![] };
                        for row in page.sessions {
                            if !rows.iter().any(|s| s.source_id == row.source_id) {
                                rows.push(row);
                            }
                        }
                        sessions.set(rows);
                        cursor.set(page.next_cursor);
                        loaded.set(true);
                        if !more {
                            selected.set(HashSet::new());
                            failures.set(HashMap::new());
                        }
                    }
                    Err(e) => error.set(Some(e)),
                }
                busy.set(false);
            });
        })
    };
    // Browse only on explicit entry into this component, never in the ordinary
    // new-session flow. A keyed remount resets all state on a machine change.
    {
        let fetch = fetch.clone();
        use_effect_with(props.supported, move |supported| {
            if *supported {
                fetch.emit(false);
            }
            || ()
        });
    }
    let import_selected = {
        let (sessions, selected, failures, error, busy, include_history, imported_count) = (
            sessions.clone(),
            selected.clone(),
            failures.clone(),
            error.clone(),
            busy.clone(),
            include_history.clone(),
            imported_count.clone(),
        );
        Callback::from(move |_: MouseEvent| {
            if *busy {
                return;
            }
            let targets: Vec<_> = sessions
                .iter()
                .filter(|s| selected.contains(&s.source_id) && s.portal_session_id.is_none())
                .cloned()
                .collect();
            let (sessions, selected, failures, error, busy, imported_count) = (
                sessions.clone(),
                selected.clone(),
                failures.clone(),
                error.clone(),
                busy.clone(),
                imported_count.clone(),
            );
            let include_history = *include_history;
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                let mut rows = (*sessions).clone();
                let mut remaining = (*selected).clone();
                let mut errors = HashMap::new();
                let mut count = *imported_count;
                // Sequential requests keep large batches from spawning many
                // native app-servers or monopolizing the launcher's memory.
                for target in targets {
                    let body = ImportSessionRequest {
                        source_id: target.source_id.clone(),
                        include_history,
                    };
                    let result =
                        match crate::dashboard_rpc::request(DashboardRequest::ImportSession {
                            launcher_id,
                            agent_type: shared::AgentType::Codex,
                            request: body,
                        })
                        .await
                        {
                            Ok(DashboardResponse::Imported(imported)) => Ok(imported),
                            Ok(_) => Err("Unexpected import reply".into()),
                            Err(e) => Err(e.message),
                        };
                    match result {
                        Ok(imported) => {
                            if let Some(row) =
                                rows.iter_mut().find(|s| s.source_id == target.source_id)
                            {
                                row.portal_session_id = Some(imported.session_id);
                            }
                            remaining.remove(&target.source_id);
                            count += 1;
                        }
                        Err(e) => {
                            errors.insert(target.source_id, e);
                        }
                    }
                    sessions.set(rows.clone());
                    selected.set(remaining.clone());
                    failures.set(errors.clone());
                    imported_count.set(count);
                }
                busy.set(false);
            });
        })
    };
    let on_search = {
        let search = search.clone();
        Callback::from(move |e: InputEvent| {
            search.set(e.target_unchecked_into::<HtmlInputElement>().value())
        })
    };
    let on_directory = {
        let directory = directory.clone();
        Callback::from(move |e: InputEvent| {
            directory.set(e.target_unchecked_into::<HtmlInputElement>().value())
        })
    };
    let on_history = {
        let include_history = include_history.clone();
        Callback::from(move |e: Event| {
            include_history.set(e.target_unchecked_into::<HtmlInputElement>().checked())
        })
    };
    let select_all = {
        let sessions = sessions.clone();
        let selected = selected.clone();
        Callback::from(move |_: MouseEvent| {
            selected.set(
                sessions
                    .iter()
                    .filter(|s| s.portal_session_id.is_none())
                    .map(|s| s.source_id.clone())
                    .collect(),
            )
        })
    };
    html! {
        <section class="session-import" aria-label="Import existing Codex sessions">
            <p class="launch-note">{"Bring saved Codex conversations from this computer into Portal. Imported sessions start paused. Close the original conversation before resuming here."}</p>
            if !props.supported {
                <p class="launch-error">{"Update agent-portal on this computer to enable session import. Codex is supported; other agents are not yet supported."}</p>
            } else {
                <div class="launch-field"><label for="import-search">{"Search conversations"}</label>
                    <input id="import-search" value={(*search).clone()} oninput={on_search} disabled={*busy} placeholder="Name or conversation text" />
                </div>
                <div class="launch-field"><label for="import-directory">{"Workspace directory (optional)"}</label>
                    <input id="import-directory" value={(*directory).clone()} oninput={on_directory} disabled={*busy} placeholder="All workspaces, or an exact directory" />
                </div>
                <div class="launch-actions">
                    <button type="button" class="launch-button-cancel" onclick={let fetch = fetch.clone(); Callback::from(move |_| fetch.emit(false))} disabled={*busy}>{"Search"}</button>
                    <button type="button" class="launch-button-cancel" onclick={select_all} disabled={*busy || sessions.is_empty()}>{"Select listed"}</button>
                </div>
                if let Some(e) = &*error { <p class="launch-error" role="alert">{e}</p> }
                <div class="import-session-list" aria-busy={busy.to_string()}>
                    { for sessions.iter().map(|session| {
                        let source = session.source_id.clone();
                        let checked = selected.contains(&source);
                        let onchange = { let selected = selected.clone(); let source = source.clone(); Callback::from(move |e: Event| {
                            let mut set = (*selected).clone();
                            if e.target_unchecked_into::<HtmlInputElement>().checked() { set.insert(source.clone()); } else { set.remove(&source); }
                            selected.set(set);
                        }) };
                        let updated = chrono::DateTime::from_timestamp(session.updated_at, 0).map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_default();
                        html! { <div class="import-session-row" key={source.clone()}>
                            <label class="import-session-choice">
                                <input type="checkbox" {checked} {onchange} disabled={*busy || session.portal_session_id.is_some()} />
                                <span><strong>{&session.name}</strong><small>{&session.working_directory}</small><small>{updated}</small></span>
                            </label>
                            if let Some(id) = session.portal_session_id { <button type="button" class="launch-button-cancel" disabled={*busy} onclick={let open = props.on_open.clone(); Callback::from(move |_| open.emit(id))}>{"Open in Portal"}</button> }
                            if let Some(e) = failures.get(&source) { <p class="launch-error" role="alert">{e}</p> }
                        </div> }
                    }) }
                </div>
                if *loaded && sessions.is_empty() { <p class="launch-note">{"No saved conversations on this page. Try another search or the next page."}</p> }
                if cursor.is_some() { <button type="button" class="launch-button-cancel" disabled={*busy || *applied_filters != ((*search).clone(), (*directory).clone())} onclick={let fetch = fetch.clone(); Callback::from(move |_| fetch.emit(true))}>{"Load more"}</button> }
                <div class="launch-field launch-checkbox"><label><input type="checkbox" checked={*include_history} onchange={on_history} disabled={*busy} />{" Copy conversation history"}</label></div>
                <p class="launch-note">{"Codex keeps the full context either way. Copying history adds earlier messages to Portal; attachments remain in Codex."}</p>
                <div class="launch-actions"><button type="button" class="launch-button" disabled={*busy || selected.is_empty()} onclick={import_selected}>
                    {if *busy { "Working…".into() } else { format!("Import selected ({})", selected.len()) }}
                </button></div>
                if *imported_count > 0 { <p role="status">{format!("{} conversations imported. Choose Open in Portal to review and resume.", *imported_count)}</p> }
            }
        </section>
    }
}
