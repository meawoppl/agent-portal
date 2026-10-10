use gloo_net::http::Request;
use shared::api::{RenameSessionRequest, MAX_SESSION_NAME_CHARS};
use shared::SessionInfo;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

use crate::components::FloatingPane;
use crate::utils;

#[derive(Properties, PartialEq)]
pub struct RenameDialogProps {
    pub session: SessionInfo,
    pub on_close: Callback<()>,
    /// Fired with the saved (trimmed) name once the backend accepts it.
    pub on_renamed: Callback<(Uuid, String)>,
}

/// Rename a session's display name. The draft lives in this dialog's own state
/// rather than round-tripping through a parent, so the input keeps focus and
/// caret while typing.
#[function_component(RenameDialog)]
pub fn rename_dialog(props: &RenameDialogProps) -> Html {
    let name = use_state(|| props.session.session_name.clone());
    let submitting = use_state(|| false);
    let error = use_state(|| None::<String>);
    let input_ref = use_node_ref();

    // Land in the field with the current name selected, ready to overtype.
    {
        let input_ref = input_ref.clone();
        use_effect_with((), move |_| {
            if let Some(input) = input_ref.cast::<HtmlInputElement>() {
                let _ = input.focus();
                input.select();
            }
            || ()
        });
    }

    let submit = {
        let session_id = props.session.id;
        let current = props.session.session_name.clone();
        let name = name.clone();
        let submitting = submitting.clone();
        let error = error.clone();
        let on_close = props.on_close.clone();
        let on_renamed = props.on_renamed.clone();
        Callback::from(move |event: SubmitEvent| {
            event.prevent_default();
            let Some(trimmed) = shared::strings::trimmed_non_blank(Some(name.as_str())) else {
                error.set(Some("Name is required".into()));
                return;
            };
            let trimmed = trimmed.to_string();
            if trimmed == current {
                on_close.emit(());
                return;
            }
            submitting.set(true);
            error.set(None);
            let submitting = submitting.clone();
            let error = error.clone();
            let on_close = on_close.clone();
            let on_renamed = on_renamed.clone();
            spawn_local(async move {
                let body = RenameSessionRequest {
                    name: trimmed.clone(),
                };
                let url = utils::api_url(&format!("/api/sessions/{session_id}/name"));
                match utils::send_json(Request::patch(&url), &body).await {
                    Ok(response) if response.ok() => {
                        on_renamed.emit((session_id, trimmed));
                        on_close.emit(());
                    }
                    Ok(response) => error.set(Some(utils::error_body(response).await)),
                    Err(e) => error.set(Some(format!("Rename request failed: {e}"))),
                }
                submitting.set(false);
            });
        })
    };
    let close = {
        let on_close = props.on_close.clone();
        Callback::from(move |_| on_close.emit(()))
    };
    let oninput = {
        let name = name.clone();
        Callback::from(move |e: InputEvent| {
            name.set(e.target_unchecked_into::<HtmlInputElement>().value())
        })
    };

    html! {
        <FloatingPane
            overlay_class="launch-dialog-backdrop"
            pane_class="launch-dialog rename-dialog"
            on_close={props.on_close.clone()}
        >
            <form onsubmit={submit}>
                <h3>{ "Rename session" }</h3>
                <div class="launch-field">
                    <label>{ "Name" }</label>
                    <input
                        ref={input_ref}
                        value={(*name).clone()}
                        maxlength={MAX_SESSION_NAME_CHARS.to_string()}
                        {oninput}
                    />
                </div>
                if let Some(message) = &*error { <div class="launch-error">{ message }</div> }
                <div class="launch-actions">
                    <button type="button" class="launch-button-cancel" onclick={close}>{ "Cancel" }</button>
                    <button type="submit" class="launch-button" disabled={*submitting}>
                        { if *submitting { "Renaming…" } else { "Rename" } }
                    </button>
                </div>
            </form>
        </FloatingPane>
    }
}
