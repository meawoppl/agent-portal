//! Local, keyed drafts survive the rail's frequent refreshes without losing focus.
use crate::components::FloatingPane;
use crate::portal_client::use_portal;
use crate::utils;
use shared::api::{validate_session_name, RenameSessionRequest, RenameSessionResponse};
use shared::{SessionInfo, SessionRole};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct RenameSessionDialogProps {
    pub session_id: Uuid,
    pub current_name: String,
    pub on_close: Callback<()>,
    pub on_renamed: Callback<()>,
}

#[function_component(RenameSessionDialog)]
pub fn rename_session_dialog(props: &RenameSessionDialogProps) -> Html {
    let portal = use_portal();
    let draft = use_state(|| props.current_name.clone());
    let busy = use_state(|| false);
    let error = use_state(|| None::<String>);
    let close = {
        let busy = busy.clone();
        let on_close = props.on_close.clone();
        Callback::from(move |()| {
            if !*busy {
                on_close.emit(());
            }
        })
    };
    let submit = {
        let draft = draft.clone();
        let busy = busy.clone();
        let error = error.clone();
        let on_close = props.on_close.clone();
        let on_renamed = props.on_renamed.clone();
        let session_id = props.session_id;
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            if *busy {
                return;
            }
            let name = match validate_session_name(&draft) {
                Ok(name) => name.to_string(),
                Err(message) => {
                    error.set(Some(message.into()));
                    return;
                }
            };
            busy.set(true);
            error.set(None);
            let (busy, error, on_close, on_renamed) = (
                busy.clone(),
                error.clone(),
                on_close.clone(),
                on_renamed.clone(),
            );
            let request = portal.patch(&format!("/api/sessions/{session_id}/name"));
            spawn_local(async move {
                let result = async {
                    let response =
                        utils::send_json(request, &RenameSessionRequest { session_name: name })
                            .await
                            .map_err(|e| e.to_string())?;
                    if !response.ok() {
                        return Err(utils::error_body(response).await);
                    }
                    response
                        .json::<RenameSessionResponse>()
                        .await
                        .map_err(|e| e.to_string())
                }
                .await;
                busy.set(false);
                match result {
                    Ok(_) => {
                        on_renamed.emit(());
                        on_close.emit(());
                    }
                    Err(message) => error.set(Some(message)),
                }
            });
        })
    };
    html! {
        <FloatingPane overlay_class="launch-dialog-backdrop" pane_class="launch-dialog rename-session-dialog" on_close={close.clone()}>
            <form onsubmit={submit} aria-label="Rename session">
                <h3>{"Rename session"}</h3>
                <p class="launch-info">{"Updates the name shown to everyone in Portal."}</p>
                <div class="launch-field">
                    <label for="rename-session-name">{"Session name"}</label>
                    <input id="rename-session-name" value={(*draft).clone()} disabled={*busy}
                        oninput={{ let draft = draft.clone(); Callback::from(move |e: InputEvent| draft.set(e.target_unchecked_into::<HtmlInputElement>().value())) }} />
                </div>
                if let Some(message) = &*error { <p class="launch-error" role="alert">{message}</p> }
                <div class="launch-actions">
                    <button type="button" class="launch-button-cancel" disabled={*busy} onclick={Callback::from(move |_| close.emit(()))}>{"Cancel"}</button>
                    <button type="submit" class="launch-button" disabled={*busy}>{if *busy {"Saving…"} else {"Save name"}}</button>
                </div>
            </form>
        </FloatingPane>
    }
}

#[derive(Properties, PartialEq)]
pub struct SessionNameProps {
    pub session: SessionInfo,
    pub on_renamed: Callback<()>,
}

/// The title remains a rename entry point when the rail is hidden on phones.
#[function_component(SessionName)]
pub fn session_name(props: &SessionNameProps) -> Html {
    let editing = use_state(|| false);
    let can_rename = matches!(
        props.session.my_role,
        SessionRole::Owner | SessionRole::Editor
    );
    html! {
        <>
            if can_rename {
                <button type="button" class="session-name session-name-button" title="Rename session" aria-label="Rename session"
                    onclick={{ let editing = editing.clone(); Callback::from(move |_| editing.set(true)) }}>
                    <span class="session-name-label">{&props.session.session_name}</span><span class="session-rename-label">{"Rename"}</span><span aria-hidden="true">{" ✎"}</span>
                </button>
            } else {
                <span class="session-name">{&props.session.session_name}</span>
            }
            if *editing {
                <RenameSessionDialog key={props.session.id.to_string()} session_id={props.session.id} current_name={props.session.session_name.clone()}
                    on_close={{let editing = editing.clone(); Callback::from(move |_| editing.set(false))}} on_renamed={props.on_renamed.clone()} />
            }
        </>
    }
}
