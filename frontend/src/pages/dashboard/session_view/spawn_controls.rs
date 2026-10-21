//! Browser-owned approval for agent-created sessions. Draft policy fields live
//! in a keyed component; typing never round-trips through the session renderer.
use crate::{
    components::DismissibleBackdrop,
    hooks::{use_escape_capture, use_focus_trap},
    utils::{self, On401},
};
use gloo_net::http::Request;
use shared::{
    api::{
        SpawnApprovalMode, SpawnPolicy, SpawnPolicyResponse, SpawnRequestInfo, SpawnRequestState,
        SpawnRequestsResponse,
    },
    LauncherInfo, WorktreeMode,
};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct Props {
    pub session_id: Uuid,
}

#[function_component(SpawnControls)]
pub fn spawn_controls(props: &Props) -> Html {
    let open = use_state(|| false);
    let show = {
        let open = open.clone();
        Callback::from(move |_| open.set(true))
    };
    let close = {
        let open = open.clone();
        Callback::from(move |_| open.set(false))
    };
    html! { <>
        <button class="session-header-action" type="button" onclick={show} title="Review agent-created sessions and their limits">{"Children"}</button>
        if *open { <SpawnDialog key={props.session_id.to_string()} session_id={props.session_id} on_close={close} /> }
    </> }
}

#[derive(Properties, PartialEq)]
struct DialogProps {
    session_id: Uuid,
    on_close: Callback<()>,
}

#[function_component(SpawnDialog)]
fn spawn_dialog(props: &DialogProps) -> Html {
    let data = use_state(|| {
        None::<(
            SpawnPolicyResponse,
            Vec<SpawnRequestInfo>,
            Vec<LauncherInfo>,
        )>
    });
    let error = use_state(|| None::<String>);
    let refresh = use_state(|| 0u32);
    let busy = use_state(|| false);
    let dialog = use_node_ref();
    use_focus_trap(dialog.clone());
    use_escape_capture(true, props.on_close.clone());
    {
        let (data, error) = (data.clone(), error.clone());
        use_effect_with((props.session_id, *refresh), move |(id, _)| {
            let id = *id;
            let cancelled = std::rc::Rc::new(std::cell::Cell::new(false));
            let guard = cancelled.clone();
            spawn_local(async move {
                let result = async {
                    let policy = utils::fetch_json::<SpawnPolicyResponse>(
                        &format!("/api/sessions/{id}/spawn-policy"),
                        On401::Ignore,
                    )
                    .await?;
                    let requests = utils::fetch_json::<SpawnRequestsResponse>(
                        &format!("/api/sessions/{id}/spawn-requests"),
                        On401::Ignore,
                    )
                    .await?;
                    let hosts = utils::fetch_launchers().await.unwrap_or_default();
                    Ok::<_, utils::FetchError>((policy, requests.requests, hosts))
                }
                .await;
                if !guard.get() {
                    match result {
                        Ok(value) => {
                            data.set(Some(value));
                            error.set(None);
                        }
                        Err(e) => {
                            error.set(Some(format!("Could not load child-session requests: {e}")))
                        }
                    }
                }
            });
            move || cancelled.set(true)
        });
    }
    let reload = {
        let refresh = refresh.clone();
        Callback::from(move |_| refresh.set(refresh.wrapping_add(1)))
    };
    let decide: Callback<(Uuid, bool)> = {
        let (busy, error, refresh) = (busy.clone(), error.clone(), refresh.clone());
        let parent = props.session_id;
        Callback::from(move |(request_id, approve)| {
            if *busy {
                return;
            }
            busy.set(true);
            error.set(None);
            let (busy, error, refresh) = (busy.clone(), error.clone(), refresh.clone());
            spawn_local(async move {
                let decision = if approve { "approve" } else { "deny" };
                match Request::post(&utils::api_url(&format!(
                    "/api/sessions/{parent}/spawn-requests/{request_id}/{decision}"
                )))
                .send()
                .await
                {
                    Ok(response) if response.ok() => refresh.set(refresh.wrapping_add(1)),
                    Ok(response) => error.set(Some(utils::error_body(response).await)),
                    Err(e) => error.set(Some(format!(
                        "Decision not confirmed: {e}. Refresh before retrying."
                    ))),
                }
                busy.set(false);
            });
        })
    };
    html! {
        <DismissibleBackdrop class="modal-overlay" on_close={props.on_close.clone()}>
            <section class="spawn-dialog" role="dialog" aria-modal="true" aria-label="Child sessions" ref={dialog}>
                <header class="spawn-dialog-header"><h2>{"Child sessions"}</h2>
                    <button type="button" class="header-button" onclick={{let close=props.on_close.clone(); Callback::from(move |_| close.emit(()))}}>{"Close"}</button>
                </header>
                <p>{"Agents can request workers on your computers. Review each request and choose the limits for this parent session."}</p>
                if let Some(message) = &*error { <p role="alert" class="settings-error">{message}</p> }
                <button type="button" class="header-button" disabled={*busy} onclick={reload}>{"Refresh requests"}</button>
                if let Some((policy, requests, hosts)) = &*data {
                    <PolicyEditor key={format!("{}-{}", props.session_id, *refresh)} session_id={props.session_id} initial={policy.clone()} />
                    <h3>{"Requests"}</h3>
                    if requests.is_empty() { <p>{"No child sessions requested yet."}</p> }
                    { for requests.iter().map(|request| html! {
                        <RequestCard key={request.request_id.to_string()} request={request.clone()} hosts={hosts.clone()} busy={*busy} on_decide={decide.clone()} />
                    }) }
                } else if error.is_none() { <p role="status">{"Loading requests…"}</p> }
            </section>
        </DismissibleBackdrop>
    }
}

#[derive(Properties, PartialEq)]
struct RequestProps {
    request: SpawnRequestInfo,
    hosts: Vec<LauncherInfo>,
    busy: bool,
    on_decide: Callback<(Uuid, bool)>,
}

fn worktree_label(mode: &WorktreeMode) -> String {
    match mode {
        WorktreeMode::None => "Use the requested directory".into(),
        WorktreeMode::Repo { branch } => format!(
            "Repository worktree: {}",
            branch.as_deref().unwrap_or("generated branch")
        ),
        WorktreeMode::Scratch { branch } => format!(
            "Scratch worktree: {}",
            branch.as_deref().unwrap_or("generated branch")
        ),
    }
}
fn state_label(state: SpawnRequestState) -> &'static str {
    match state {
        SpawnRequestState::Pending => "Needs approval",
        SpawnRequestState::Launching => "Launching",
        SpawnRequestState::Launched => "Launched",
        SpawnRequestState::Denied => "Denied",
        SpawnRequestState::Failed => "Failed",
    }
}

#[function_component(RequestCard)]
fn request_card(props: &RequestProps) -> Html {
    let info = &props.request;
    let request = &info.request;
    let host = props
        .hosts
        .iter()
        .find(|host| host.launcher_id == info.launcher_id)
        .map(|host| host.hostname.clone())
        .unwrap_or_else(|| info.launcher_id.to_string());
    let approve = {
        let callback = props.on_decide.clone();
        let id = info.request_id;
        Callback::from(move |_| callback.emit((id, true)))
    };
    let deny = {
        let callback = props.on_decide.clone();
        let id = info.request_id;
        Callback::from(move |_| callback.emit((id, false)))
    };
    html! { <article class="spawn-request">
        <h4>{request.launch.session_name.as_deref().unwrap_or("New child session")}{" · "}{state_label(info.state)}</h4>
        <dl>
            <dt>{"Agent"}</dt><dd>{request.launch.agent_type.as_str()}</dd>
            <dt>{"Computer"}</dt><dd>{host}<br/><small>{info.launcher_id.to_string()}</small></dd>
            <dt>{"Directory"}</dt><dd><code>{&request.launch.working_directory}</code></dd>
            <dt>{"Worktree"}</dt><dd>{worktree_label(&request.launch.worktree)}</dd>
            <dt>{"Arguments"}</dt><dd><code>{if request.launch.claude_args.is_empty() { "Default model and arguments".into() } else { request.launch.claude_args.join(" ") }}</code></dd>
            <dt>{"Plugins"}</dt><dd>{ match &request.plugins {
                None => "Directory policy and detection".into(),
                Some(plugins) if plugins.is_empty() => "Explicit empty selection".into(),
                Some(plugins) => plugins.iter().map(|(name,on)| format!("{name}: {}", if *on { "on" } else { "off" })).collect::<Vec<_>>().join(", "),
            } }</dd>
        </dl>
        <details><summary>{"Initial instructions"}</summary><pre>{&request.prompt}</pre></details>
        if let Some(code) = &info.error_code { <p class="settings-error">{code}</p> }
        if info.state == SpawnRequestState::Pending {
            <div class="spawn-actions"><button class="header-button" type="button" disabled={props.busy} onclick={approve}>{"Approve worker"}</button>
            <button class="header-button" type="button" disabled={props.busy} onclick={deny}>{"Deny"}</button></div>
        }
        if let Some(child) = info.child_session_id {
            <a href={format!("/dashboard?session={child}")}>{"Open child session"}</a>
        }
    </article> }
}

#[derive(Properties, PartialEq)]
struct PolicyProps {
    session_id: Uuid,
    initial: SpawnPolicyResponse,
}

#[derive(Clone, PartialEq)]
struct PolicyDraft {
    mode: SpawnApprovalMode,
    children: String,
    depth: String,
    idle: String,
    stop: bool,
}
impl From<&SpawnPolicy> for PolicyDraft {
    fn from(p: &SpawnPolicy) -> Self {
        Self {
            mode: p.approval_mode,
            children: p.max_children.to_string(),
            depth: p.max_depth.to_string(),
            idle: p.idle_timeout_seconds.to_string(),
            stop: p.stop_on_parent_end,
        }
    }
}
impl PolicyDraft {
    fn parse(&self) -> Result<SpawnPolicy, String> {
        let value = |text: &str, name: &str, min, max| -> Result<u32, String> {
            let n = text
                .parse::<u32>()
                .map_err(|_| format!("{name} must be a whole number"))?;
            if !(min..=max).contains(&n) {
                return Err(format!("{name} must be between {min} and {max}"));
            }
            Ok(n)
        };
        Ok(SpawnPolicy {
            approval_mode: self.mode,
            max_children: value(&self.children, "Child limit", 1, 32)?,
            max_depth: value(&self.depth, "Depth limit", 1, 8)?,
            idle_timeout_seconds: value(&self.idle, "Idle timeout", 60, 604800)?,
            stop_on_parent_end: self.stop,
        })
    }
}
#[function_component(PolicyEditor)]
fn policy_editor(props: &PolicyProps) -> Html {
    let draft = use_state(|| PolicyDraft::from(&props.initial.policy));
    let saving = use_state(|| false);
    let message = use_state(|| None::<Result<String, String>>);
    let select = {
        let draft = draft.clone();
        Callback::from(move |e: Event| {
            let input: HtmlSelectElement = e.target_unchecked_into();
            let mut next = (*draft).clone();
            next.mode = match input.value().as_str() {
                "auto" => SpawnApprovalMode::Auto,
                "off" => SpawnApprovalMode::Off,
                _ => SpawnApprovalMode::FirstSpawn,
            };
            draft.set(next);
        })
    };
    let change = |field: &'static str| {
        let draft = draft.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            let mut next = (*draft).clone();
            match field {
                "children" => next.children = input.value(),
                "depth" => next.depth = input.value(),
                _ => next.idle = input.value(),
            };
            draft.set(next);
        })
    };
    let toggle = {
        let draft = draft.clone();
        Callback::from(move |e: Event| {
            let input: HtmlInputElement = e.target_unchecked_into();
            let mut next = (*draft).clone();
            next.stop = input.checked();
            draft.set(next);
        })
    };
    let save = {
        let (draft, saving, message) = (draft.clone(), saving.clone(), message.clone());
        let id = props.session_id;
        Callback::from(move |_| {
            if *saving {
                return;
            }
            let policy = match draft.parse() {
                Ok(policy) => policy,
                Err(e) => {
                    message.set(Some(Err(e)));
                    return;
                }
            };
            saving.set(true);
            message.set(None);
            let (saving, message) = (saving.clone(), message.clone());
            spawn_local(async move {
                let result = match utils::send_json(
                    Request::patch(&utils::api_url(&format!("/api/sessions/{id}/spawn-policy"))),
                    &policy,
                )
                .await
                {
                    Ok(response) if response.ok() => {
                        Ok("Policy saved for future child requests.".into())
                    }
                    Ok(response) => Err(utils::error_body(response).await),
                    Err(e) => Err(format!("Could not save policy: {e}")),
                };
                message.set(Some(result));
                saving.set(false);
            });
        })
    };
    let mode = match draft.mode {
        SpawnApprovalMode::FirstSpawn => "first_spawn",
        SpawnApprovalMode::Auto => "auto",
        SpawnApprovalMode::Off => "off",
    };
    html! { <details class="spawn-policy"><summary>{"Worker policy and limits"}</summary>
        <p>{if props.initial.first_spawn_approved {"You have approved this parent's first worker."} else {"This parent has not received first-worker approval."}}</p>
        <label>{"Approval"}<select onchange={select} disabled={*saving}>
            <option value="first_spawn" selected={mode == "first_spawn"}>{"Approve first worker; later requests follow these limits"}</option><option value="auto" selected={mode == "auto"}>{"Automatically allow workers within these limits"}</option><option value="off" selected={mode == "off"}>{"Do not allow workers"}</option>
        </select></label>
        <label>{"Maximum children"}<input type="number" min="1" max="32" value={draft.children.clone()} oninput={change("children")} disabled={*saving}/></label>
        <label>{"Maximum depth (root starts at 1)"}<input type="number" min="1" max="8" value={draft.depth.clone()} oninput={change("depth")} disabled={*saving}/></label>
        <label>{"Stop idle children after (seconds)"}<input type="number" min="60" max="604800" value={draft.idle.clone()} oninput={change("idle")} disabled={*saving}/></label>
        <label><input type="checkbox" checked={draft.stop} onchange={toggle} disabled={*saving}/>{"Stop children when their parent ends"}</label>
        <button class="header-button" type="button" disabled={*saving} onclick={save}>{if *saving {"Saving…"} else {"Save worker policy"}}</button>
        if let Some(result)=&*message { {match result { Ok(text)=>html!{<p role="status">{text}</p>},Err(text)=>html!{<p role="alert" class="settings-error">{text}</p>} }} }
    </details> }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_draft_preserves_defaults_and_rejects_invalid_limits() {
        let mut draft = PolicyDraft::from(&SpawnPolicy::default());
        assert_eq!(draft.parse().unwrap(), SpawnPolicy::default());
        draft.children = "0".into();
        assert!(draft.parse().is_err());
        draft.children = "4".into();
        draft.depth = "9".into();
        assert!(draft.parse().is_err());
        draft.depth = "2".into();
        draft.idle = "59".into();
        assert!(draft.parse().is_err());
        draft.idle = "1.5".into();
        assert!(draft.parse().is_err());
    }
}
