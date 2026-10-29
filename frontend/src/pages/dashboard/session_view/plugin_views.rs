//! Controlled plugin views. SessionView owns inventory fetching, surface state,
//! and persisted notice expansion; these components only render and emit intent.
use crate::components::plugin_discovery::{
    run_plugin_action, suggested_plugins, PluginCard, PLUGINS_BROWSER,
};
use shared::api::{
    ForwardInfo, PluginAction, PluginActivation, PluginContextEntry, PluginContextKind,
    PluginPolicy, PluginRequest, PluginResponse, PluginSurfaceAction, PluginSurfaceState,
    PluginSurfaceStatus, PortalPluginCommandInfo, PortalPluginInfo,
};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub(super) struct PluginContextNoticeProps {
    /// The launcher's record of what this session's process was given.
    pub entries: Vec<PluginContextEntry>,
    /// A posted change hasn't reached the agent yet.
    #[prop_or_default]
    pub pending: bool,
    /// Current inventory, for display names and project policy.
    pub plugins: Vec<PortalPluginInfo>,
    pub expanded: bool,
    pub on_toggle: Callback<MouseEvent>,
    /// Set when the viewer may change this computer's project policy.
    pub launcher_id: Option<Uuid>,
    pub working_directory: String,
}

#[derive(Properties, PartialEq)]
pub(super) struct PluginDiscoverySurfaceProps {
    pub plugins: Vec<PortalPluginInfo>,
    pub error: Option<String>,
    pub collapsed: bool,
    pub fullscreen: bool,
    pub on_toggle_collapsed: Callback<MouseEvent>,
    pub on_toggle_mode: Callback<MouseEvent>,
    pub on_close: Callback<MouseEvent>,
    /// This session's computer and id, when the viewer may run its plugins'
    /// surfaces (the owner, on the home portal).
    pub surface_host: Option<(Uuid, Uuid)>,
    pub on_open_forward: Callback<ForwardInfo>,
    /// The session's computer name and directory, shown when approving a
    /// plugin command.
    pub session_place: (String, String),
}

fn source_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    }
}

fn activation_label(activation: PluginActivation) -> &'static str {
    match activation {
        PluginActivation::Explicit => "chosen for this session",
        PluginActivation::Policy => "project policy",
        PluginActivation::Detected => "detected",
    }
}

/// The plugin context this session is using, from the launcher's record (set at
/// launch, replaced when a selection change reaches the running session),
/// with each file's text one click away rather than in the transcript.
#[function_component(PluginContextNotice)]
pub(super) fn plugin_context_notice(props: &PluginContextNoticeProps) -> Html {
    let disabled = use_state(BTreeSet::<String>::new);
    let error = use_state(|| None::<String>);
    if props.entries.is_empty() {
        return html! {};
    }
    let mut by_plugin: BTreeMap<&str, Vec<&PluginContextEntry>> = BTreeMap::new();
    for entry in &props.entries {
        by_plugin
            .entry(entry.plugin.as_str())
            .or_default()
            .push(entry);
    }
    let display_name = |name: &str| {
        props
            .plugins
            .iter()
            .find(|plugin| plugin.name == name)
            .map_or_else(|| name.to_string(), |plugin| plugin.display_name.clone())
    };
    let estimated_tokens = props
        .entries
        .iter()
        .map(|e| e.estimated_tokens)
        .sum::<u64>();
    let label = by_plugin
        .keys()
        .map(|name| display_name(name))
        .collect::<Vec<_>>()
        .join(", ");
    let expanded = props.expanded;

    let disable = |name: &str| -> Option<Callback<MouseEvent>> {
        let launcher_id = props.launcher_id?;
        let never = props
            .plugins
            .iter()
            .any(|plugin| plugin.name == name && plugin.policy == PluginPolicy::Never);
        if never || disabled.contains(name) {
            return None;
        }
        let name = name.to_string();
        let directory = props.working_directory.clone();
        let (disabled, error) = (disabled.clone(), error.clone());
        Some(Callback::from(move |_: MouseEvent| {
            let request = PluginRequest {
                working_directory: Some(directory.clone()),
                session_id: None,
                action: PluginAction::SetPolicy {
                    name: name.clone(),
                    policy: PluginPolicy::Never,
                },
            };
            let (name, disabled, error) = (name.clone(), disabled.clone(), error.clone());
            spawn_local(async move {
                match run_plugin_action(launcher_id, &request).await {
                    Ok(_) => {
                        let mut names = (*disabled).clone();
                        names.insert(name);
                        disabled.set(names);
                        error.set(None);
                    }
                    Err(e) => error.set(Some(format!("Could not change the project policy: {e}"))),
                }
            });
        }))
    };

    html! {
        <section class={classes!("plugin-context-card", expanded.then_some("expanded"))}>
            <button
                type="button"
                class="plugin-context-summary"
                onclick={props.on_toggle.clone()}
                aria-expanded={expanded.to_string()}
            >
                <span class="plugin-context-caret">{ if expanded { "▾" } else { "▸" } }</span>
                <span class="plugin-context-title">{ "Plugins available to this session" }</span>
                <span class="plugin-context-names">{ label }</span>
                <span class="plugin-context-size">
                    { format!("~{estimated_tokens} estimated catalog tokens") }
                </span>
            </button>
            if expanded {
                <div class="plugin-context-body">
                    if props.pending {
                        <div class="plugin-context-detail">{ "A plugin change is waiting for the session's next turn." }</div>
                    }
                    if let Some(error) = &*error {
                        <div class="edit-stack-error">{ error }</div>
                    }
                    { for by_plugin.iter().map(|(name, entries)| {
                        let tokens = entries.iter().map(|e| e.estimated_tokens).sum::<u64>();
                        let first = entries[0];
                        html! {
                            <article class="plugin-context-plugin" key={name.to_string()}>
                                <div class="plugin-context-plugin-title">
                                    <span>{ display_name(name) }</span>
                                    <span>{ format!("~{tokens} estimated catalog tokens") }</span>
                                </div>
                                <div class="plugin-context-detail">
                                    { format!("{} · {}", first.reason, activation_label(first.activation)) }
                                </div>
                                { for entries.iter().map(|entry| html! {
                                    <details class="plugin-context-entry" key={entry.path.clone()}>
                                        <summary>
                                            <span class="plugin-context-kind">
                                                { match entry.kind { PluginContextKind::Skill => "skill", PluginContextKind::Prompt => "prompt" } }
                                            </span>
                                            <span>{ &entry.name }</span>
                                            <span class="plugin-context-size">
                                                { format!("{} source · ~{} estimated catalog tokens", source_size(entry.context_bytes), entry.estimated_tokens) }
                                            </span>
                                        </summary>
                                        <code class="plugin-context-source">{ &entry.path }</code>
                                        <pre class="plugin-context-text">{ &entry.text }</pre>
                                    </details>
                                }) }
                                if disabled.contains(*name) {
                                    <div class="plugin-context-detail">{ "Won't load in new sessions in this project." }</div>
                                } else if let Some(onclick) = disable(name) {
                                    <button type="button" class="plugin-context-disable" {onclick}>
                                        { "Disable for this project" }
                                    </button>
                                }
                            </article>
                        }
                    }) }
                </div>
            }
        </section>
    }
}

#[function_component(PluginDiscoverySurface)]
pub(super) fn plugin_discovery_surface(props: &PluginDiscoverySurfaceProps) -> Html {
    let suggested_count = suggested_plugins(&props.plugins).len();
    let collapsed = props.collapsed;
    let fullscreen = props.fullscreen;
    let title = if suggested_count > 0 {
        format!(
            "Plugins · {suggested_count} suggested · {} installed",
            props.plugins.len()
        )
    } else {
        format!("Plugins · {} installed", props.plugins.len())
    };
    let status_class = (suggested_count > 0).then_some("is-up");
    let status_title = if suggested_count > 0 {
        "Plugins match this session directory"
    } else {
        "No plugin matched this session directory"
    };
    let toggle_collapsed = props.on_toggle_collapsed.clone();
    let toggle_mode = props.on_toggle_mode.clone();
    let close = props.on_close.clone();

    html! {
        <aside
            id="session-plugins"
            class={classes!(
                "session-forward-surface",
                "session-plugins-surface",
                collapsed.then_some("collapsed"),
                fullscreen.then_some("fullscreen"),
            )}
            aria-label="Plugins"
        >
            <div class="session-forward-toolbar session-plugins-toolbar">
                <button
                    type="button"
                    class="surface-icon-button"
                    title={ if collapsed { "Expand plugins" } else { "Collapse plugins" } }
                    onclick={toggle_collapsed}
                >
                    { if collapsed { "▸" } else { "▾" } }
                </button>
                <span class={classes!("surface-status", status_class)} title={status_title}></span>
                <span class="session-forward-title" title={title.clone()}>{ title }</span>
                <button
                    type="button"
                    class="surface-icon-button surface-mode-button"
                    title={ if fullscreen { "Return to split view" } else { "Full screen" } }
                    onclick={toggle_mode}
                >
                    { if fullscreen { "⇲" } else { "⛶" } }
                </button>
                <button
                    type="button"
                    class="surface-icon-button surface-close-button"
                    title="Close plugins"
                    onclick={close}
                >
                    { "×" }
                </button>
            </div>
            if !collapsed {
                <section class="plugin-panel">
                    <div class="plugin-panel-header">
                        <div class="edit-stack-title">
                            <span class="edit-stack-kicker">{ "Plugin discovery" }</span>
                            <span class="edit-stack-count">
                                { format!("{suggested_count} suggested · {} installed", props.plugins.len()) }
                            </span>
                        </div>
                        <a class="plugin-browse-link" href={PLUGINS_BROWSER} target="_blank" rel="noopener noreferrer">{ "Browse plugins" }</a>
                    </div>
                    if let Some(error) = props.error.as_deref() {
                        <div class="edit-stack-error">{ error }</div>
                    }
                    <div class="plugin-list">
                        if props.plugins.is_empty() {
                            <div class="edit-stack-empty">{ "No installed Portal plugins were discovered on this host." }</div>
                        }
                        { for props.plugins.iter().map(|plugin| html! {
                            <PluginCard key={plugin.name.clone()} plugin={plugin.clone()}>
                                if let (Some((launcher_id, session_id)), Some(_)) = (props.surface_host, plugin.surface_kind.as_ref()) {
                                    <SurfaceControls
                                        plugin={plugin.name.clone()}
                                        {launcher_id}
                                        {session_id}
                                        on_open={props.on_open_forward.clone()}
                                    />
                                }
                                if let (Some((launcher_id, session_id)), false) = (props.surface_host, plugin.commands.is_empty()) {
                                    <CommandRunner
                                        key={format!("{launcher_id}:{session_id}:{}", plugin.name)}
                                        plugin={plugin.name.clone()}
                                        commands={plugin.commands.clone()}
                                        {launcher_id}
                                        {session_id}
                                        host={props.session_place.0.clone()}
                                        working_directory={props.session_place.1.clone()}
                                    />
                                }
                            </PluginCard>
                        }) }
                    </div>
                </section>
            }
        </aside>
    }
}

#[derive(Properties, PartialEq)]
pub(super) struct SurfaceControlsProps {
    pub plugin: String,
    pub launcher_id: Uuid,
    pub session_id: Uuid,
    pub on_open: Callback<ForwardInfo>,
}

fn surface_state_label(state: &PluginSurfaceState) -> &'static str {
    match state {
        PluginSurfaceState::Stopped => "stopped",
        PluginSurfaceState::Starting => "starting",
        PluginSurfaceState::Healthy => "healthy",
        PluginSurfaceState::Unhealthy => "unhealthy",
        PluginSurfaceState::Exited => "exited",
    }
}

/// A plugin surface as a managed pane: its state, start/stop/restart, and
/// opening it docked or in a tab. It runs as a launcher-managed forward on
/// this session, so the forward's usual privacy rules apply.
#[function_component(SurfaceControls)]
pub(super) fn surface_controls(props: &SurfaceControlsProps) -> Html {
    let status = use_state(|| None::<PluginSurfaceStatus>);
    let busy = use_state(|| None::<&'static str>);
    let error = use_state(|| None::<String>);
    // Every request gets a number; only the latest one's answer is applied,
    // so a delayed status can't overwrite a finished start, and an answer
    // for an earlier computer, session or plugin is dropped whole.
    let latest = use_mut_ref(|| 0_u64);
    // The change in progress, claimed synchronously so two clicks before the
    // next render start one; released only by the request that claimed it.
    let in_flight = use_mut_ref(|| None::<u64>);
    let target = (props.launcher_id, props.session_id, props.plugin.clone());
    let run = {
        let (status, busy, error) = (status.clone(), busy.clone(), error.clone());
        let (latest, in_flight) = (latest.clone(), in_flight.clone());
        let (launcher_id, session_id, plugin) = target.clone();
        Callback::from(
            move |(label, action): (&'static str, PluginSurfaceAction)| {
                let mutation = action != PluginSurfaceAction::Status;
                if mutation && in_flight.borrow().is_some() {
                    return;
                }
                let seq = {
                    let mut latest = latest.borrow_mut();
                    *latest += 1;
                    *latest
                };
                if mutation {
                    *in_flight.borrow_mut() = Some(seq);
                }
                let request = PluginRequest {
                    working_directory: None,
                    session_id: Some(session_id),
                    action: PluginAction::Surface {
                        name: plugin.clone(),
                        action,
                    },
                };
                let (status, busy, error) = (status.clone(), busy.clone(), error.clone());
                let (latest, in_flight) = (latest.clone(), in_flight.clone());
                busy.set(Some(label));
                spawn_local(async move {
                    let result = run_plugin_action(launcher_id, &request).await;
                    if *in_flight.borrow() == Some(seq) {
                        *in_flight.borrow_mut() = None;
                    }
                    if *latest.borrow() != seq {
                        return;
                    }
                    busy.set(None);
                    match result {
                        Ok(response) => {
                            status.set(response.surface);
                            error.set(None);
                        }
                        Err(e) => error.set(Some(e)),
                    }
                });
            },
        )
    };
    {
        let run = run.clone();
        let (status, error, busy) = (status.clone(), error.clone(), busy.clone());
        let in_flight = in_flight.clone();
        use_effect_with(target, move |_| {
            // A new target: nothing in flight belongs to it any more.
            *in_flight.borrow_mut() = None;
            status.set(None);
            error.set(None);
            busy.set(None);
            run.emit(("Checking", PluginSurfaceAction::Status));
            || ()
        });
    }
    let button = |label: &'static str, verb: &'static str, action: PluginSurfaceAction| {
        let run = run.clone();
        html! {
            <button
                type="button"
                class="surface-control"
                disabled={busy.is_some()}
                onclick={Callback::from(move |_| run.emit((verb, action)))}
            >{ label }</button>
        }
    };
    let current = (*status).clone();
    let state = current
        .as_ref()
        .map_or(PluginSurfaceState::Stopped, |s| s.state.clone());
    let here = current
        .as_ref()
        .is_none_or(|s| s.session_id.is_none_or(|id| id == props.session_id));
    let running = !matches!(
        state,
        PluginSurfaceState::Stopped | PluginSurfaceState::Exited
    );
    let forward = current.as_ref().and_then(|s| {
        Some(ForwardInfo {
            port: s.port?,
            url: s.url.clone()?,
            created_at: s.started_at.clone().unwrap_or_default(),
            public: false,
            listening: Some(s.state == PluginSurfaceState::Healthy),
            process: None,
        })
    });
    html! {
        <div class="plugin-surface-controls">
            <div class="plugin-surface-status">
                <span class={classes!("plugin-surface-state", surface_state_label(&state))}>
                    { busy.map_or_else(|| surface_state_label(&state).to_string(), |b| format!("{b}…")) }
                </span>
                if !here {
                    <span class="plugin-surface-note">{ "running for another session" }</span>
                }
                if let Some(path) = current.as_ref().and_then(|s| s.health_path.clone()) {
                    <span class="plugin-surface-note">{ format!("health {path}") }</span>
                }
            </div>
            <div class="plugin-card-actions">
                if running && here {
                    { button("Restart", "Restarting", PluginSurfaceAction::Restart) }
                    { button("Stop", "Stopping", PluginSurfaceAction::Stop) }
                } else {
                    { button("Start", "Starting", PluginSurfaceAction::Start) }
                }
                if let (Some(forward), true) = (forward, here) {
                    <button type="button" class="surface-control" onclick={{
                        let (on_open, forward) = (props.on_open.clone(), forward.clone());
                        Callback::from(move |_| on_open.emit(forward.clone()))
                    }}>{ "Open docked" }</button>
                    <a class="surface-control" href={forward.url.clone()} target="_blank" rel="noopener noreferrer">
                        { "Open in new tab" }
                    </a>
                }
            </div>
            if let Some(error) = &*error {
                <div class="edit-stack-error">{ error }</div>
            }
            if let Some(last) = current.as_ref().and_then(|s| s.last_error.clone()) {
                <div class="edit-stack-error">{ last }</div>
            }
            if let Some(s) = current.as_ref().filter(|s| {
                !s.log_tail.is_empty() && s.state != PluginSurfaceState::Healthy
            }) {
                <details class="plugin-surface-log">
                    <summary>{ "Log" }</summary>
                    <pre>{ s.log_tail.join("\n") }</pre>
                </details>
            }
        </div>
    }
}

#[derive(Properties, PartialEq)]
pub(super) struct CommandRunnerProps {
    pub plugin: String,
    pub commands: Vec<PortalPluginCommandInfo>,
    pub launcher_id: Uuid,
    pub session_id: Uuid,
    /// Where it runs, for the approval prompt.
    pub host: String,
    pub working_directory: String,
}

/// A command waiting for the user's approval: the exact line the launcher
/// resolved, shown with where it will run, and the launcher's fingerprint of
/// it, which is what approving sends back.
#[derive(Clone, PartialEq)]
struct Proposal {
    command: String,
    args: Vec<String>,
    line: String,
    fingerprint: String,
}

/// A plugin's named commands, run through Portal (#2161): pick one, give its
/// arguments if it takes them, approve the exact command line, see the result.
/// The run is also recorded in the transcript as a plugin command card.
///
/// Rendered keyed by launcher, session and plugin, so a proposal never
/// survives a change of target: approving runs only what was shown here.
#[function_component(CommandRunner)]
pub(super) fn command_runner(props: &CommandRunnerProps) -> Html {
    let picked = use_state(|| None::<String>);
    let args = use_state(String::new);
    let proposal = use_state(|| None::<Proposal>);
    let result = use_state(|| None::<(String, PluginResponse)>);
    let error = use_state(|| None::<String>);
    let busy = use_state(|| false);
    let in_flight = use_mut_ref(|| false);

    let send = {
        let (launcher_id, session_id, plugin) =
            (props.launcher_id, props.session_id, props.plugin.clone());
        let (proposal, result, error, busy, in_flight) = (
            proposal.clone(),
            result.clone(),
            error.clone(),
            busy.clone(),
            in_flight.clone(),
        );
        Callback::from(
            move |(command, args, approval): (String, Vec<String>, Option<String>)| {
                if in_flight.replace(true) {
                    return;
                }
                let request = PluginRequest {
                    working_directory: None,
                    session_id: Some(session_id),
                    action: PluginAction::RunCommand {
                        name: plugin.clone(),
                        command: command.clone(),
                        args: args.clone(),
                        approval: approval.clone(),
                    },
                };
                let (proposal, result, error, busy, in_flight) = (
                    proposal.clone(),
                    result.clone(),
                    error.clone(),
                    busy.clone(),
                    in_flight.clone(),
                );
                busy.set(true);
                error.set(None);
                spawn_local(async move {
                    let response = run_plugin_action(launcher_id, &request).await;
                    *in_flight.borrow_mut() = false;
                    busy.set(false);
                    match (response, approval) {
                        (Ok(response), None) => match response.proposal {
                            Some(fingerprint) => proposal.set(Some(Proposal {
                                command,
                                args,
                                line: response.output,
                                fingerprint,
                            })),
                            None => error.set(Some(
                                "This computer's launcher can't preview commands; update it".into(),
                            )),
                        },
                        (Ok(response), Some(_)) => {
                            proposal.set(None);
                            result.set(Some((command, response)));
                        }
                        (Err(e), _) => error.set(Some(e)),
                    }
                });
            },
        )
    };
    let review = {
        let (send, picked, args, error) =
            (send.clone(), picked.clone(), args.clone(), error.clone());
        Callback::from(move |_: SubmitEvent| {
            let Some(command) = (*picked).clone() else {
                return;
            };
            match split_args(&args) {
                Ok(parsed) => send.emit((command, parsed, None)),
                Err(problem) => error.set(Some(problem.to_string())),
            }
        })
    };

    html! {
        <div class="plugin-command-runner">
            <div class="plugin-skill-list">
                { for props.commands.iter().map(|command| {
                    let (name, takes_args) = (command.name.clone(), command.accepts_args);
                    let onclick = {
                        let (send, picked, args, proposal, result) =
                            (send.clone(), picked.clone(), args.clone(), proposal.clone(), result.clone());
                        Callback::from(move |_| {
                            proposal.set(None);
                            result.set(None);
                            args.set(String::new());
                            if takes_args {
                                picked.set(Some(name.clone()));
                            } else {
                                picked.set(None);
                                send.emit((name.clone(), Vec::new(), None));
                            }
                        })
                    };
                    html! {
                        <button
                            type="button"
                            class="surface-control"
                            title={command.description.clone().unwrap_or_default()}
                            disabled={*busy}
                            {onclick}
                        >{ format!("Run {}", command.name) }</button>
                    }
                }) }
            </div>
            if let Some(command) = &*picked {
                <form class="plugin-command-args" onsubmit={{
                    let review = review.clone();
                    Callback::from(move |e: SubmitEvent| { e.prevent_default(); review.emit(e); })
                }}>
                    <label>
                        { format!("Arguments for {command}") }
                        <input
                            type="text"
                            value={(*args).clone()}
                            oninput={{
                                let args = args.clone();
                                Callback::from(move |e: InputEvent| {
                                    args.set(e.target_unchecked_into::<web_sys::HtmlInputElement>().value());
                                })
                            }}
                        />
                    </label>
                    <button type="submit" class="surface-control" disabled={*busy}>{ "Review" }</button>
                </form>
            }
            if let Some(p) = &*proposal {
                <div class="plugin-command-approve" role="alertdialog">
                    <p>{ format!("Run {}:{} on {} in {}?", props.plugin, p.command, props.host, props.working_directory) }</p>
                    <code>{ &p.line }</code>
                    <div class="plugin-card-actions">
                        <button type="button" class="surface-control" disabled={*busy} onclick={{
                            let (send, p) = (send.clone(), p.clone());
                            Callback::from(move |_| {
                                send.emit((p.command.clone(), p.args.clone(), Some(p.fingerprint.clone())))
                            })
                        }}>{ "Run" }</button>
                        <button type="button" class="surface-control" onclick={{
                            let proposal = proposal.clone();
                            Callback::from(move |_| proposal.set(None))
                        }}>{ "Cancel" }</button>
                    </div>
                </div>
            }
            if *busy {
                <div class="plugin-surface-note">{ "Running…" }</div>
            }
            if let Some(error) = &*error {
                <div class="edit-stack-error">{ error }</div>
            }
            if let Some((command, response)) = &*result {
                <div class={classes!("plugin-command-result", (!response.success).then_some("failed"))}>
                    <strong>{ format!("{command}: {}", match (response.success, response.exit_code) {
                        (true, _) => "done".to_string(),
                        (false, Some(code)) => format!("failed (exit {code})"),
                        (false, None) => response.error.clone().unwrap_or_else(|| "failed".into()),
                    }) }</strong>
                    if let Some(problem) = &response.transcript_error {
                        <div class="edit-stack-error">{ problem }</div>
                    }
                    if !response.output.trim().is_empty() {
                        <details open={!response.success}>
                            <summary>{ "Output" }</summary>
                            <pre>{ &response.output }</pre>
                        </details>
                    }
                </div>
            }
        </div>
    }
}

/// Split a typed argument string into arguments: whitespace separates them,
/// and single or double quotes keep spaces inside one. Each argument is
/// shell-quoted again on the launcher, so nothing here is interpreted.
fn split_args(input: &str) -> Result<Vec<String>, &'static str> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote = None;
    for c in input.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    args.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            (None, c) => {
                current.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err("an argument's quote is never closed");
    }
    if started {
        args.push(current);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::split_args;

    #[test]
    fn arguments_split_on_spaces_and_keep_quoted_ones_whole() {
        assert_eq!(split_args("").unwrap(), Vec::<String>::new());
        assert_eq!(split_args("  export  --all ").unwrap(), ["export", "--all"]);
        assert_eq!(
            split_args(r#"render "board v2.kicad_pcb" 'it''s'"#).unwrap(),
            ["render", "board v2.kicad_pcb", "its"]
        );
        assert_eq!(split_args("--name=''").unwrap(), ["--name="]);
        assert_eq!(split_args("''").unwrap(), [""]);
        assert!(split_args("\"open").is_err());
    }
}
