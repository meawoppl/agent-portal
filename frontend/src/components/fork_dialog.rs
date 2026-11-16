//! Fork a session, or hand its work off to a different agent.
//!
//! Choosing the source's own agent forks: the new session continues the
//! agent-native conversation. Choosing another agent hands off: native state
//! does not carry across agents, so the backend launches a fresh session whose
//! first input points it at the source's Portal transcript (and, by default,
//! stops the source). One dialog serves both because the choices — name,
//! directory, model — are the same, and the agent picker is what decides.

use shared::api::{
    ForkDirectoryMode, ForkSessionRequest, ForkSessionResponse, HandoffSessionRequest,
    HandoffSessionResponse,
};
use shared::{AgentType, SessionInfo};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

use crate::components::model_select::model_cli_args;
use crate::components::skip_permissions::{
    auto_mode_args, skip_permissions_args, skip_permissions_label, strip_auto_mode_args,
    strip_skip_permissions_args, AUTO_MODE_LABEL,
};
use crate::components::{FloatingPane, ModelSelect};
use crate::portal_client::use_portal;
use crate::utils;

const AGENTS: [AgentType; 4] = [
    AgentType::Claude,
    AgentType::Codex,
    AgentType::Muse,
    AgentType::Antigravity,
];

#[derive(Properties, PartialEq)]
pub struct ForkDialogProps {
    pub session: SessionInfo,
    pub on_close: Callback<()>,
    #[prop_or_default]
    pub on_forked: Callback<Uuid>,
    /// Agent preselected when the dialog opens; the source's own when `None`.
    #[prop_or_default]
    pub initial_agent: Option<AgentType>,
}

fn default_name(source: &SessionInfo, target: AgentType) -> String {
    if target == source.agent_type {
        format!("{} (fork)", source.session_name)
    } else {
        format!("{} ({})", source.session_name, target.display_name())
    }
}

fn source_model(session: &SessionInfo) -> String {
    session
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
}

/// The target's permission preset args, carrying over how much the source was
/// trusted: a full-access Codex hands off to a Claude that also skips prompts.
fn permission_args(target: AgentType, skip: bool, auto: bool) -> Vec<String> {
    let args: &[&str] = if skip {
        skip_permissions_args(target)
    } else if auto {
        auto_mode_args(target).unwrap_or_default()
    } else {
        &[]
    };
    args.iter().map(|arg| arg.to_string()).collect()
}

#[function_component(ForkDialog)]
pub fn fork_dialog(props: &ForkDialogProps) -> Html {
    let source_agent = props.session.agent_type;
    let target = use_state(|| props.initial_agent.unwrap_or(source_agent));
    let handoff = *target != source_agent;
    let name = use_state(|| default_name(&props.session, *target));
    let mode = use_state(|| {
        if props.session.repo_url.is_some() && props.initial_agent.is_none() {
            ForkDirectoryMode::Worktree
        } else {
            // A handoff usually picks up half-finished work in place.
            ForkDirectoryMode::Same
        }
    });
    let other_directory = use_state(String::new);
    let model = use_state(|| {
        if handoff {
            String::new()
        } else {
            source_model(&props.session)
        }
    });
    let skip_permissions =
        use_state(|| strip_skip_permissions_args(&props.session.claude_args, source_agent).0);
    let auto_mode = use_state(|| strip_auto_mode_args(&props.session.claude_args, source_agent).0);
    let stop_source = use_state(|| true);
    let prompt = use_state(String::new);
    let submitting = use_state(|| false);
    let error = use_state(|| None::<String>);

    let set_mode = |next: ForkDirectoryMode| {
        let mode = mode.clone();
        Callback::from(move |_| mode.set(next))
    };
    let on_agent_change = {
        let target = target.clone();
        let name = name.clone();
        let model = model.clone();
        let session = props.session.clone();
        Callback::from(move |e: Event| {
            let value = e.target_unchecked_into::<HtmlSelectElement>().value();
            let next = AgentType::parse_or_default(&value);
            target.set(next);
            name.set(default_name(&session, next));
            model.set(if next == session.agent_type {
                source_model(&session)
            } else {
                String::new()
            });
        })
    };
    let portal = use_portal();
    let submit = {
        let source_id = props.session.id;
        let target = target.clone();
        let name = name.clone();
        let mode = mode.clone();
        let other_directory = other_directory.clone();
        let model = model.clone();
        let skip_permissions = skip_permissions.clone();
        let auto_mode = auto_mode.clone();
        let stop_source = stop_source.clone();
        let prompt = prompt.clone();
        let submitting = submitting.clone();
        let error = error.clone();
        let on_close = props.on_close.clone();
        let on_forked = props.on_forked.clone();
        Callback::from(move |_| {
            if !utils::is_non_blank(&name) {
                error.set(Some("Name is required".into()));
                return;
            }
            if *mode == ForkDirectoryMode::Other && !utils::is_non_blank(&other_directory) {
                error.set(Some("Choose the other working directory".into()));
                return;
            }
            let working_directory = (*mode == ForkDirectoryMode::Other)
                .then(|| utils::owned_non_blank(&other_directory))
                .flatten();
            submitting.set(true);
            error.set(None);
            let submitting = submitting.clone();
            let error = error.clone();
            let on_close = on_close.clone();
            let on_forked = on_forked.clone();
            if *target == source_agent {
                let body = ForkSessionRequest {
                    name: name.trim().to_string(),
                    directory_mode: *mode,
                    working_directory,
                    model: utils::owned_non_blank(&model),
                    divergence_prompt: utils::owned_non_blank(&prompt),
                    fork_point_turn_id: None,
                };
                let request = portal.post(&format!("/api/sessions/{source_id}/fork"));
                spawn_local(async move {
                    match submit_json::<_, ForkSessionResponse>(request, &body, "Fork").await {
                        Ok(result) => {
                            on_forked.emit(result.session_id);
                            on_close.emit(());
                        }
                        Err(message) => error.set(Some(message)),
                    }
                    submitting.set(false);
                });
            } else {
                let mut claude_args = model_cli_args(*target, &model);
                claude_args.extend(permission_args(*target, *skip_permissions, *auto_mode));
                let body = HandoffSessionRequest {
                    target_agent: *target,
                    name: name.trim().to_string(),
                    directory_mode: *mode,
                    working_directory,
                    claude_args,
                    instructions: utils::owned_non_blank(&prompt),
                    stop_source: *stop_source,
                };
                let request = portal.post(&format!("/api/sessions/{source_id}/handoff"));
                spawn_local(async move {
                    match submit_json::<_, HandoffSessionResponse>(request, &body, "Handoff").await
                    {
                        Ok(result) => {
                            on_forked.emit(result.session_id);
                            if result.source_stopped || !body.stop_source {
                                on_close.emit(());
                            } else {
                                error.set(Some(
                                    "The new session started, but the source could not be stopped. Stop it from the session rail.".into(),
                                ));
                            }
                        }
                        Err(message) => error.set(Some(message)),
                    }
                    submitting.set(false);
                });
            }
        })
    };
    let close = {
        let on_close = props.on_close.clone();
        Callback::from(move |_| on_close.emit(()))
    };
    let checkbox = |state: &UseStateHandle<bool>, others: Vec<UseStateHandle<bool>>| {
        let state = state.clone();
        Callback::from(move |e: Event| {
            let checked = e.target_unchecked_into::<HtmlInputElement>().checked();
            state.set(checked);
            if checked {
                others.iter().for_each(|other| other.set(false));
            }
        })
    };

    let source_label = source_agent.display_name();
    let target_label = target.display_name();
    html! {
        <FloatingPane
            overlay_class="launch-dialog-backdrop"
            pane_class="launch-dialog fork-dialog"
            on_close={props.on_close.clone()}
        >
                <h3>{ if handoff { "Hand off session" } else { "Fork session" } }</h3>
                <p class="launch-info">
                    if handoff {
                        { format!("Start a new {target_label} session to continue {}'s work. {source_label}'s conversation state can't be loaded by {target_label}, so the new agent is told how to read this session's transcript instead.", props.session.session_name) }
                    } else {
                        { format!("Create a new {source_label} session from {}'s latest persisted turn.", props.session.session_name) }
                    }
                </p>
                <div class="launch-field">
                    <label>{ "Agent" }</label>
                    <select class="launcher-select" onchange={on_agent_change}>
                        { for AGENTS.iter().map(|agent| html! {
                            <option value={agent.as_str()} selected={*agent == *target}>
                                { if *agent == source_agent {
                                    format!("{} (fork)", agent.display_name())
                                } else {
                                    format!("{} (hand off)", agent.display_name())
                                } }
                            </option>
                        }) }
                    </select>
                </div>
                <div class="launch-field">
                    <label>{ "Name" }</label>
                    <input value={(*name).clone()} oninput={{ let name = name.clone(); Callback::from(move |e: InputEvent| name.set(e.target_unchecked_into::<HtmlInputElement>().value())) }} />
                </div>

                <div class="launch-field">
                    <label>{ "Working directory" }</label>
                    <div class="fork-directory-options">
                        if props.session.repo_url.is_some() {
                            <label><input type="radio" name="fork-dir" checked={*mode == ForkDirectoryMode::Worktree} onchange={set_mode(ForkDirectoryMode::Worktree)} /> { if handoff { "New git worktree" } else { "New git worktree (recommended)" } }</label>
                        }
                        <label><input type="radio" name="fork-dir" checked={*mode == ForkDirectoryMode::Same} onchange={set_mode(ForkDirectoryMode::Same)} /> { if handoff { "Same directory (keeps uncommitted work)" } else { "Same directory" } }</label>
                        if *mode == ForkDirectoryMode::Same && !(handoff && *stop_source) {
                            <p class="launch-note launch-note-warn">{ "Warning: both agents will share one checkout and may overwrite each other's work." }</p>
                        }
                        <label><input type="radio" name="fork-dir" checked={*mode == ForkDirectoryMode::Other} onchange={set_mode(ForkDirectoryMode::Other)} /> { "Other directory" }</label>
                        if *mode == ForkDirectoryMode::Other {
                            <input placeholder="/path/on/source/launcher" value={(*other_directory).clone()} oninput={{ let value = other_directory.clone(); Callback::from(move |e: InputEvent| value.set(e.target_unchecked_into::<HtmlInputElement>().value())) }} />
                        }
                    </div>
                </div>

                <div class="launch-field">
                    <label>{ if handoff { "Model" } else { "Model override" } }</label>
                    <ModelSelect key={target.as_str()} agent_type={*target} value={(*model).clone()} on_change={{ let model = model.clone(); Callback::from(move |value| model.set(value)) }} />
                </div>
                if handoff {
                    <div class="launch-field fork-directory-options">
                        if !skip_permissions_args(*target).is_empty() {
                            <label><input type="checkbox" checked={*skip_permissions} onchange={checkbox(&skip_permissions, vec![auto_mode.clone()])} /> { skip_permissions_label(*target) }</label>
                        }
                        if auto_mode_args(*target).is_some() {
                            <label><input type="checkbox" checked={*auto_mode} onchange={checkbox(&auto_mode, vec![skip_permissions.clone()])} /> { AUTO_MODE_LABEL }</label>
                        }
                        <label><input type="checkbox" checked={*stop_source} onchange={checkbox(&stop_source, Vec::new())} /> { format!("Stop {} (recommended; resume it later)", props.session.session_name) }</label>
                    </div>
                }
                <div class="launch-field">
                    <label>{ if handoff { "Instructions for the new agent (optional)" } else { "Divergence prompt (optional)" } }</label>
                    <textarea
                        placeholder={handoff.then_some("Defaults to: continue the most recent request")}
                        value={(*prompt).clone()}
                        oninput={{ let prompt = prompt.clone(); Callback::from(move |e: InputEvent| prompt.set(e.target_unchecked_into::<HtmlTextAreaElement>().value())) }}
                    />
                </div>
                if let Some(message) = &*error { <div class="launch-error">{ message }</div> }
                <div class="launch-actions">
                    <button type="button" class="launch-button-cancel" onclick={close}>{ "Cancel" }</button>
                    <button type="button" class="launch-button" disabled={*submitting} onclick={submit}>
                        { match (handoff, *submitting) {
                            (false, false) => "Fork session".to_string(),
                            (false, true) => "Forking…".to_string(),
                            (true, false) => format!("Hand off to {target_label}"),
                            (true, true) => "Handing off…".to_string(),
                        } }
                    </button>
                </div>
        </FloatingPane>
    }
}

/// POST `body` and decode the success response, or return the user-facing
/// error (the server's message when it sent one).
async fn submit_json<B: serde::Serialize, R: serde::de::DeserializeOwned>(
    request: gloo_net::http::RequestBuilder,
    body: &B,
    action: &str,
) -> Result<R, String> {
    match utils::send_json(request, body).await {
        Ok(response) if response.ok() => response
            .json::<R>()
            .await
            .map_err(|e| format!("{action} response was malformed: {e}")),
        Ok(response) => Err(response
            .text()
            .await
            .unwrap_or_else(|_| format!("{action} failed"))),
        Err(e) => Err(format!("{action} request failed: {e}")),
    }
}
