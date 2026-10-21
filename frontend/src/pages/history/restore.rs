//! Keyed History recovery form. Drafts belong here, not to the transcript.
use gloo_net::http::Request;
use shared::{
    api::{
        HistoryManifest, HistoryRestoreMode, HistoryRestoreRequest, HistoryRestoreResponse,
        HistoryRestoreStatus,
    },
    LauncherInfo,
};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub manifest: HistoryManifest,
    #[prop_or_default]
    pub on_open: Option<Callback<Uuid>>,
}

async fn response<T: serde::de::DeserializeOwned>(
    r: gloo_net::http::Response,
) -> Result<T, String> {
    if r.ok() {
        r.json().await.map_err(|e| e.to_string())
    } else {
        Err(crate::utils::error_body(r).await)
    }
}
async fn get<T: serde::de::DeserializeOwned>(url: &str) -> Result<T, String> {
    response(Request::get(url).send().await.map_err(|e| e.to_string())?).await
}

#[function_component(HistoryRestore)]
pub fn history_restore(props: &Props) -> Html {
    let status = use_state(|| None::<HistoryRestoreStatus>);
    let launchers = use_state(Vec::<LauncherInfo>::new);
    let selected = use_state(|| None::<Uuid>);
    let directory = use_state(|| props.manifest.working_directory.clone());
    let mode = use_state(|| {
        if matches!(props.manifest.agent_type.as_str(), "claude" | "codex") {
            HistoryRestoreMode::Native
        } else {
            HistoryRestoreMode::Context
        }
    });
    let expanded = use_state(|| false);
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let restored = use_state(|| None::<Uuid>);
    let endpoint = format!(
        "/api/history/sessions/{}/{}/recovery",
        props.manifest.user_id, props.manifest.session_id
    );
    let reload = {
        let (status, launchers, selected, error) = (
            status.clone(),
            launchers.clone(),
            selected.clone(),
            error.clone(),
        );
        let endpoint = endpoint.clone();
        let hostname = props.manifest.hostname.clone();
        Callback::from(move |_: ()| {
            let (status, launchers, selected, error) = (
                status.clone(),
                launchers.clone(),
                selected.clone(),
                error.clone(),
            );
            let (endpoint, hostname) = (endpoint.clone(), hostname.clone());
            spawn_local(async move {
                error.set(None);
                match get::<HistoryRestoreStatus>(&endpoint).await {
                    Ok(s) => {
                        if s.can_restore && s.existing_session_id.is_none() {
                            match crate::dashboard_rpc::launchers()
                                .await
                                .map_err(|e| e.to_string())
                            {
                                Ok(hosts) => {
                                    if selected.is_none() {
                                        selected.set(
                                            hosts
                                                .iter()
                                                .find(|h| h.hostname == hostname && h.connected)
                                                .map(|h| h.launcher_id),
                                        );
                                    }
                                    launchers.set(hosts);
                                }
                                Err(e) => error.set(Some(e)),
                            }
                        }
                        status.set(Some(s));
                    }
                    Err(e) => error.set(Some(e)),
                }
            });
        })
    };
    {
        let reload = reload.clone();
        use_effect_with((), move |_| {
            reload.emit(());
            || ()
        });
    }
    let on_restore = {
        let (selected, directory, mode, busy, error, restored) = (
            selected.clone(),
            directory.clone(),
            mode.clone(),
            busy.clone(),
            error.clone(),
            restored.clone(),
        );
        Callback::from(move |_: MouseEvent| {
            if *busy {
                return;
            }
            let Some(launcher_id) = *selected else {
                error.set(Some("Select a connected host.".into()));
                return;
            };
            let body = HistoryRestoreRequest {
                launcher_id,
                working_directory: (*directory).clone(),
                mode: *mode,
            };
            let endpoint = endpoint.clone();
            let (busy, error, restored) = (busy.clone(), error.clone(), restored.clone());
            busy.set(true);
            error.set(None);
            spawn_local(async move {
                let result = async {
                    let r = Request::post(&endpoint)
                        .json(&body)
                        .map_err(|e| e.to_string())?
                        .send()
                        .await
                        .map_err(|e| e.to_string())?;
                    response::<HistoryRestoreResponse>(r).await
                }
                .await;
                match result {
                    Ok(r) => restored.set(Some(r.session_id)),
                    Err(e) => error.set(Some(e)),
                }
                busy.set(false);
            });
        })
    };
    let chosen = selected.and_then(|id| launchers.iter().find(|h| h.launcher_id == id));
    let supported = chosen.is_some_and(|h| {
        h.connected
            && h.capabilities
                .iter()
                .any(|c| c == shared::LAUNCHER_CAPABILITY_HISTORY_RESTORE)
    });
    let existing = (*restored).or_else(|| status.as_ref().and_then(|s| s.existing_session_id));
    html! {
        <section class="history-restore" aria-label="Restore from History">
            if let Some(id) = existing {
                <a class="history-page-button" href={format!("/dashboard?session={id}")} onclick={let on_open = props.on_open.clone(); Callback::from(move |e: MouseEvent| { if let Some(cb) = &on_open { e.prevent_default(); cb.emit(id); } })}>{ if restored.is_some() { "Open restored session" } else { "Open existing session" } }</a>
                <p>{ if restored.is_some() { "Restored and paused. Open the session, choose Resume, and send a new instruction. The original archive is preserved." } else { "This conversation is already in Portal. Open it to continue or resume it." } }</p>
            } else if let Some(s) = &*status {
                if !s.can_restore {
                    <p>{"Only the archive owner can restore this conversation."}</p>
                } else if !*expanded {
                    <button class="history-page-button" onclick={let expanded = expanded.clone(); Callback::from(move |_| expanded.set(true))}>{"Restore from History"}</button>
                } else {
                    <h3>{"Restore from History"}</h3>
                    <p>{"Create a paused session while keeping this archive. Nothing runs until you resume and send a new instruction."}</p>
                    <div class="history-restore-fields">
                        <label>{"Recovery method"}<select disabled={*busy} value={if *mode == HistoryRestoreMode::Native { "native" } else { "context" }} onchange={let mode = mode.clone(); Callback::from(move |e: Event| mode.set(if e.target_unchecked_into::<HtmlSelectElement>().value() == "native" { HistoryRestoreMode::Native } else { HistoryRestoreMode::Context }))}>
                            <option value="native" selected={*mode == HistoryRestoreMode::Native} disabled={!matches!(props.manifest.agent_type.as_str(), "claude" | "codex")}>{"Continue saved native conversation"}</option>
                            <option value="context" selected={*mode == HistoryRestoreMode::Context} disabled={!s.transcript_available}>{"Recover from archived context"}</option>
                        </select></label>
                        <label>{"Host"}<select disabled={*busy} value={selected.map(|id| id.to_string()).unwrap_or_default()} onchange={let selected = selected.clone(); Callback::from(move |e: Event| selected.set(e.target_unchecked_into::<HtmlSelectElement>().value().parse().ok()))}>
                            <option value="" selected={selected.is_none()}>{"Select a connected host"}</option>
                            { for launchers.iter().map(|h| html! { <option value={h.launcher_id.to_string()} selected={*selected == Some(h.launcher_id)} disabled={!h.connected}>{format!("{} ({}){}", h.launcher_name, h.hostname, if h.connected { "" } else { " — offline" })}</option> }) }
                        </select></label>
                        <label>{"Working directory (on selected host)"}<input disabled={*busy} value={(*directory).clone()} placeholder="~/project" oninput={let directory = directory.clone(); Callback::from(move |e: InputEvent| directory.set(e.target_unchecked_into::<HtmlInputElement>().value()))} /></label>
                    </div>
                    if *mode == HistoryRestoreMode::Native {
                        <p>{"Uses saved Claude or Codex state on the original host and branches it into the restored session. If that state is missing, restoration stops so you can choose archived context instead."}</p>
                    } else {
                        <p>{"Starts a new agent conversation with a bounded recent excerpt of archived messages. This is not an exact resume: files, attachments, running tools and unsaved native state are not recovered. Historical commands are not replayed."}</p>
                    }
                    if !chosen.is_some_and(|h| h.connected) { <p role="status">{format!("Reconnect {} for native continuation, or select another connected host and recover from archived context.", props.manifest.hostname)}</p> }
                    if chosen.is_some_and(|h| h.connected) && !supported { <p role="status">{"Update agent-portal on the selected host before restoring."}</p> }
                    <div class="history-restore-actions">
                        <button class="history-page-button" disabled={*busy || !supported || directory.trim().is_empty()} onclick={on_restore}>{if *busy { "Checking and restoring…" } else { "Restore paused session" }}</button>
                        <button class="history-page-button" disabled={*busy} onclick={let reload = reload.clone(); Callback::from(move |_| reload.emit(()))}>{"Refresh hosts"}</button>
                    </div>
                }
            } else { <p>{"Checking restore options…"}</p> }
            if let Some(e) = &*error { <p class="history-error" role="alert">{e}</p><button class="history-page-button" disabled={*busy} onclick={Callback::from(move |_| reload.emit(()))}>{"Retry checks"}</button> }
        </section>
    }
}
