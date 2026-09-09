use gloo_net::http::Request;
use shared::api::{ForkDirectoryMode, ForkSessionRequest, ForkSessionResponse};
use shared::SessionInfo;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlTextAreaElement};
use yew::prelude::*;

use crate::components::{FloatingPane, ModelSelect};
use crate::utils;

#[derive(Properties, PartialEq)]
pub struct ForkDialogProps {
    pub session: SessionInfo,
    pub on_close: Callback<()>,
    #[prop_or_default]
    pub on_forked: Callback<Uuid>,
}

#[function_component(ForkDialog)]
pub fn fork_dialog(props: &ForkDialogProps) -> Html {
    let name = use_state(|| format!("{} (fork)", props.session.session_name));
    let mode = use_state(|| {
        if props.session.repo_url.is_some() {
            ForkDirectoryMode::Worktree
        } else {
            ForkDirectoryMode::Same
        }
    });
    let other_directory = use_state(String::new);
    let model = use_state(|| {
        props
            .session
            .claude_args
            .windows(2)
            .find_map(|pair| {
                if pair[0] == "--model" {
                    Some(pair[1].clone())
                } else if pair[0] == "-c" {
                    pair[1].strip_prefix("model=").map(str::to_string)
                } else {
                    None
                }
            })
            .unwrap_or_default()
    });
    let prompt = use_state(String::new);
    let submitting = use_state(|| false);
    let error = use_state(|| None::<String>);

    let set_mode = |next: ForkDirectoryMode| {
        let mode = mode.clone();
        Callback::from(move |_| mode.set(next))
    };
    let submit = {
        let source_id = props.session.id;
        let name = name.clone();
        let mode = mode.clone();
        let other_directory = other_directory.clone();
        let model = model.clone();
        let prompt = prompt.clone();
        let submitting = submitting.clone();
        let error = error.clone();
        let on_close = props.on_close.clone();
        let on_forked = props.on_forked.clone();
        Callback::from(move |_| {
            if name.trim().is_empty() {
                error.set(Some("Name is required".into()));
                return;
            }
            if *mode == ForkDirectoryMode::Other && other_directory.trim().is_empty() {
                error.set(Some("Choose the other working directory".into()));
                return;
            }
            let body = ForkSessionRequest {
                name: name.trim().to_string(),
                directory_mode: *mode,
                working_directory: (*mode == ForkDirectoryMode::Other)
                    .then(|| other_directory.trim().to_string()),
                model: (!model.trim().is_empty()).then(|| (*model).clone()),
                divergence_prompt: (!prompt.trim().is_empty()).then(|| prompt.trim().to_string()),
                fork_point_turn_id: None,
            };
            submitting.set(true);
            error.set(None);
            let submitting = submitting.clone();
            let error = error.clone();
            let on_close = on_close.clone();
            let on_forked = on_forked.clone();
            spawn_local(async move {
                let response = utils::send_json(
                    Request::post(&format!("/api/sessions/{source_id}/fork")),
                    &body,
                )
                .await;
                match response {
                    Ok(response) if response.ok() => {
                        match response.json::<ForkSessionResponse>().await {
                            Ok(result) => {
                                on_forked.emit(result.session_id);
                                on_close.emit(());
                            }
                            Err(e) => error.set(Some(format!("Fork response was malformed: {e}"))),
                        }
                    }
                    Ok(response) => {
                        let message = response
                            .text()
                            .await
                            .unwrap_or_else(|_| "Fork failed".into());
                        error.set(Some(message));
                    }
                    Err(e) => error.set(Some(format!("Fork request failed: {e}"))),
                }
                submitting.set(false);
            });
        })
    };
    let close = {
        let on_close = props.on_close.clone();
        Callback::from(move |_| on_close.emit(()))
    };

    html! {
        <FloatingPane
            overlay_class="launch-dialog-backdrop"
            pane_class="launch-dialog fork-dialog"
            on_close={props.on_close.clone()}
        >
                <h3>{ "Fork session" }</h3>
                <p class="launch-info">
                    { format!("Create a new {} session from {}'s latest persisted turn.", props.session.agent_type, props.session.session_name) }
                </p>
                <div class="launch-field">
                    <label>{ "Name" }</label>
                    <input value={(*name).clone()} oninput={{ let name = name.clone(); Callback::from(move |e: InputEvent| name.set(e.target_unchecked_into::<HtmlInputElement>().value())) }} />
                </div>

                <div class="launch-field">
                    <label>{ "Working directory" }</label>
                    <div class="fork-directory-options">
                        if props.session.repo_url.is_some() {
                            <label><input type="radio" name="fork-dir" checked={*mode == ForkDirectoryMode::Worktree} onchange={set_mode(ForkDirectoryMode::Worktree)} /> { "New git worktree (recommended)" }</label>
                        }
                        <label><input type="radio" name="fork-dir" checked={*mode == ForkDirectoryMode::Same} onchange={set_mode(ForkDirectoryMode::Same)} /> { "Same directory" }</label>
                        if *mode == ForkDirectoryMode::Same {
                            <p class="launch-note launch-note-warn">{ "Warning: both agents will share one checkout and may overwrite each other's work." }</p>
                        }
                        <label><input type="radio" name="fork-dir" checked={*mode == ForkDirectoryMode::Other} onchange={set_mode(ForkDirectoryMode::Other)} /> { "Other directory" }</label>
                        if *mode == ForkDirectoryMode::Other {
                            <input placeholder="/path/on/source/launcher" value={(*other_directory).clone()} oninput={{ let value = other_directory.clone(); Callback::from(move |e: InputEvent| value.set(e.target_unchecked_into::<HtmlInputElement>().value())) }} />
                        }
                    </div>
                </div>

                <div class="launch-field">
                    <label>{ "Model override" }</label>
                    <ModelSelect agent_type={props.session.agent_type} value={(*model).clone()} on_change={{ let model = model.clone(); Callback::from(move |value| model.set(value)) }} />
                </div>
                <div class="launch-field">
                    <label>{ "Divergence prompt (optional)" }</label>
                    <textarea value={(*prompt).clone()} oninput={{ let prompt = prompt.clone(); Callback::from(move |e: InputEvent| prompt.set(e.target_unchecked_into::<HtmlTextAreaElement>().value())) }} />
                </div>
                if let Some(message) = &*error { <div class="launch-error">{ message }</div> }
                <div class="launch-actions">
                    <button type="button" class="launch-button-cancel" onclick={close}>{ "Cancel" }</button>
                    <button type="button" class="launch-button" disabled={*submitting} onclick={submit}>{ if *submitting { "Forking…" } else { "Fork session" } }</button>
                </div>
        </FloatingPane>
    }
}
