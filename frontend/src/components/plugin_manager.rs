use crate::components::ConfirmModal;
use crate::utils;
use gloo_net::http::Request;
use shared::api::*;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct PluginManagerProps {
    pub launcher_id: Uuid,
    #[prop_or_default]
    pub session_id: Option<Uuid>,
    pub working_directory: String,
    #[prop_or_default]
    pub on_open_surface: Callback<u16>,
    #[prop_or_default]
    pub on_open_surface_tab: Callback<u16>,
}

/// Keep this component keyed by host/project at call sites: an operation result
/// must never appear to describe a different project's policy.
#[function_component(PluginManager)]
pub fn plugin_manager(props: &PluginManagerProps) -> Html {
    let inventory = use_state(PluginInventoryResponse::default);
    let error = use_state(|| None::<String>);
    let inventory_error = use_state(|| None::<String>);
    let output = use_state(String::new);
    let diagnostics = use_state(Vec::<PluginDiagnostic>::new);
    let busy = use_state(|| false);
    let loading = use_state(|| true);
    let revision = use_state(|| 0u32);
    let source = use_state(String::new);
    let pending = use_state(|| None::<(PluginAction, String)>);
    {
        let inventory = inventory.clone();
        let error = inventory_error.clone();
        let loading = loading.clone();
        use_effect_with(
            (
                props.launcher_id,
                props.session_id,
                props.working_directory.clone(),
                *revision,
            ),
            move |(host, session, cwd, _)| {
                let mut path = format!(
                    "/api/plugins?launcherId={host}&workingDirectory={}",
                    js_sys::encode_uri_component(cwd)
                );
                if let Some(id) = session {
                    path.push_str(&format!("&sessionId={id}"));
                }
                loading.set(true);
                let alive = std::rc::Rc::new(std::cell::Cell::new(true));
                let active = alive.clone();
                spawn_local(async move {
                    let result = async {
                        let response = Request::get(&utils::api_url(&path))
                            .send()
                            .await
                            .map_err(|error| error.to_string())?;
                        if !response.ok() {
                            return Err(utils::error_body(response).await);
                        }
                        response
                            .json::<PluginInventoryResponse>()
                            .await
                            .map_err(|error| error.to_string())
                    }
                    .await;
                    if active.get() {
                        match result {
                            Ok(value) => {
                                inventory.set(value);
                                error.set(None);
                            }
                            Err(e) => error.set(Some(e.to_string())),
                        }
                        loading.set(false);
                    }
                });
                move || alive.set(false)
            },
        );
    }
    let execute = {
        let busy = busy.clone();
        let output = output.clone();
        let diagnostics = diagnostics.clone();
        let error = error.clone();
        let revision = revision.clone();
        let pending = pending.clone();
        let host = props.launcher_id;
        let session_id = props.session_id;
        let cwd = props.working_directory.clone();
        Callback::from(move |action: PluginAction| {
            if *busy {
                return;
            }
            busy.set(true);
            output.set(String::new());
            diagnostics.set(Vec::new());
            error.set(None);
            pending.set(None);
            let busy = busy.clone();
            let output = output.clone();
            let diagnostics = diagnostics.clone();
            let error = error.clone();
            let revision = revision.clone();
            let request = PluginRequest {
                agent_type: None,
                working_directory: Some(cwd.clone()),
                session_id,
                action,
            };
            spawn_local(async move {
                let result = async {
                    let response = utils::send_json(
                        Request::post(&utils::api_url(&format!("/api/launchers/{host}/plugins"))),
                        &request,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                    if !response.ok() {
                        return Err(utils::error_body(response).await);
                    }
                    response
                        .json::<PluginResponse>()
                        .await
                        .map_err(|e| e.to_string())
                }
                .await;
                match result {
                    Ok(response) => {
                        diagnostics.set(response.diagnostics);
                        output.set(format!(
                            "{}\n{}",
                            response
                                .exit_code
                                .map(|c| format!("Exit code: {c}"))
                                .unwrap_or_default(),
                            response.output
                        ));
                        if !response.success {
                            error.set(Some(
                                response
                                    .error
                                    .unwrap_or_else(|| "Plugin operation failed".into()),
                            ));
                        }
                        revision.set(*revision + 1);
                    }
                    Err(e) => error.set(Some(e)),
                }
                busy.set(false);
            });
        })
    };
    let request_action = {
        let pending = pending.clone();
        Callback::from(move |value: (PluginAction, String)| pending.set(Some(value)))
    };
    let install = {
        let request_action = request_action.clone();
        let source = source.clone();
        Callback::from(move |_| {
            request_action.emit((PluginAction::Install { source: (*source).clone() }, format!("Install {}? Installation can run the repository's setup commands on this host.", *source)))
        })
    };
    html! {
        <section class="plugin-manager" aria-busy={(*busy || *loading).to_string()}>
            <style>{include_str!("../../styles/plugins.css")}</style>
            <div class="plugin-toolbar">
                <button disabled={*busy || *loading} onclick={{let revision = revision.clone(); Callback::from(move |_| revision.set(*revision + 1))}}>{"Refresh"}</button>
                <label>{"Install source"}<input placeholder="Repository URL or local path" value={(*source).clone()} disabled={*busy} oninput={{let source = source.clone(); Callback::from(move |e: InputEvent| source.set(e.target_unchecked_into::<HtmlInputElement>().value()))}} /></label>
                <button disabled={*busy || source.trim().is_empty()} onclick={install}>{"Install…"}</button>
            </div>
            if *busy { <p role="status">{"Running plugin operation… Setup and updates can take several minutes."}</p> }
            if *loading { <p role="status">{"Loading plugins…"}</p> }
            if let Some(message) = &*error { <p role="alert">{message}</p> }
            if let Some(message) = &*inventory_error { <p role="alert">{message}</p> }
            if !output.trim().is_empty() { <details open=true><summary>{"Operation output"}</summary><pre>{&*output}</pre></details> }
            if !diagnostics.is_empty() {
                <section aria-label="Doctor diagnostics"><h3>{"Diagnostics"}</h3>
                    {for diagnostics.iter().map(|item| html! {<div role={if item.success {"status"} else {"alert"}}><strong>{format!("{} · {}", item.name, if item.success {"Passed"} else {"Failed"})}</strong><p>{&item.message}</p></div>})}
                </section>
            }
            {for inventory.warnings.iter().map(|warning| html!{<p role="status">{warning}</p>})}
            if !*loading && inventory_error.is_none() && inventory.plugins.is_empty() { <p>{"No plugins installed on this host. Install one from a repository URL or local path."}</p> }
            {for inventory.plugins.iter().map(|plugin| {
                let name = plugin.name.clone();
                let action_button = |label: &'static str, action: PluginAction| {
                    let request_action = request_action.clone();
                    let question = if label == "Reinstall" {
                        format!("Reinstall plugin {}? This re-fetches its recorded source and re-runs setup; it may also install an available update.", plugin.name)
                    } else { format!("{label} plugin {}?", plugin.name) };
                    html!{<button disabled={*busy} onclick={Callback::from(move |_| request_action.emit((action.clone(), question.clone())))}>{label}</button>}
                };
                let set_policy = { let execute = execute.clone(); let name = name.clone(); Callback::from(move |e: Event| {
                    let policy = match e.target_unchecked_into::<HtmlSelectElement>().value().as_str() { "always" => PluginPolicy::Always, "never" => PluginPolicy::Never, _ => PluginPolicy::Ask };
                    execute.emit(PluginAction::SetPolicy { name: name.clone(), policy });
                })};
                html!{<article class="plugin-card" key={name.clone()}>
                    <h2>{&plugin.display_name}<small>{format!(" · {}", plugin.version.as_deref().unwrap_or("local"))}</small></h2>
                    <p>{plugin.description.as_deref().unwrap_or_default()}</p>
                    <p>{format!("{} · {}", if plugin.enabled {"Enabled"} else {"Disabled"}, if plugin.active {"Active for this project"} else {"Not active"})}</p>
                    if let Some(reason) = &plugin.reason {<p>{reason}</p>}
                    <dl><dt>{"Source"}</dt><dd>{plugin.source.as_deref().unwrap_or("Local")}</dd><dt>{"Installed path"}</dt><dd>{plugin.path.as_deref().unwrap_or("Unknown")}</dd></dl>
                    if let Some(url) = plugin.homepage.as_ref().filter(|url| url.starts_with("https://") || url.starts_with("http://")) { <a href={url.clone()} target="_blank" rel="noopener noreferrer">{"Documentation / repository"}</a> }
                    if let Some(license) = &plugin.license {<p>{format!("License: {license}")}</p>}
                    {for plugin.compatibility.iter().chain(plugin.warnings.iter()).map(|w| html!{<p role="status">{w}</p>})}
                    if plugin.update_available == Some(true) {<p>{"Update available"}</p>}
                    <label>{"Project policy "}<select disabled={*busy || props.working_directory.trim().is_empty()} value={match plugin.policy {PluginPolicy::Ask => "ask", PluginPolicy::Always => "always", PluginPolicy::Never => "never"}} onchange={set_policy}><option value="ask">{"Ask / detect"}</option><option value="always">{"Always"}</option><option value="never">{"Never"}</option></select></label>
                    <p class="muted">{"Policy changes apply to future launches in this project. Existing session context is unchanged."}</p>
                    <div class="plugin-toolbar">{action_button("Update", PluginAction::Update {name: name.clone()})}{action_button("Reinstall", PluginAction::Update {name: name.clone()})}{action_button("Uninstall", PluginAction::Remove {name: name.clone()})}
                    if plugin.has_doctor { {action_button("Doctor", PluginAction::Doctor {name: name.clone()})} }</div>
                    <details><summary>{format!("Skills and prompts · {} bytes · ~{} tokens", plugin.context_bytes, plugin.estimated_tokens)}</summary>
                    {for plugin.skills.iter().chain(plugin.prompts.iter()).map(|skill| html!{<p><strong>{&skill.name}</strong>{format!(" — {} ({} bytes, ~{} tokens)", skill.path, skill.context_bytes, skill.estimated_tokens)}
                    if !skill.agents.is_empty() {<span>{format!(" · Agents: {}", skill.agents.join(", "))}</span>}<br/>{skill.description.as_deref().unwrap_or_default()}</p>})}</details>
                    if props.session_id.is_none() && !plugin.commands.is_empty() {<p>{"Open this plugin in a session dock to run commands and keep results in its transcript"}</p>}
                    {for plugin.commands.iter().map(|command| html!{<PluginCommand key={command.name.clone()} plugin={name.clone()} command={command.clone()} disabled={*busy || props.session_id.is_none()} on_request={request_action.clone()} />})}
                    if plugin.surface_kind.is_some() || plugin.surface.is_some() {
                        <h3>{plugin.surface_title.as_deref().unwrap_or("Plugin surface")}</h3>
                        if let Some(kind) = &plugin.surface_kind {<p>{format!("Surface kind: {kind}")}</p>}
                        if let Some(surface) = &plugin.surface {<p>{format!("{:?} · PID {} · port {}", surface.state, surface.pid.map(|n| n.to_string()).unwrap_or_else(|| "—".into()), surface.port.map(|n| n.to_string()).unwrap_or_else(|| "—".into()))}</p>
                        if let Some(started) = &surface.started_at {<p>{format!("Started: {started}")}</p>}
                        if let Some(path) = &surface.health_path {<p>{format!("Health check: {path}")}</p>}
                        if let Some(error) = &surface.last_error {<p role="alert">{error}</p>}
                        <details><summary>{"Surface logs"}</summary><pre>{surface.log_tail.join("\n")}</pre></details>}
                        <div class="plugin-toolbar">{for [("Start", PluginSurfaceAction::Start), ("Stop", PluginSurfaceAction::Stop), ("Restart", PluginSurfaceAction::Restart)].into_iter().map(|(label, action)| action_button(label, PluginAction::Surface {name:name.clone(), action}))}
                        if props.session_id.is_some() {
                            if let Some(port) = plugin.surface.as_ref().and_then(|surface| surface.port) {<button disabled={*busy} onclick={{let cb = props.on_open_surface.clone(); Callback::from(move |_| cb.emit(port))}}>{"Open in dock"}</button>
                            <button disabled={*busy} onclick={{let cb = props.on_open_surface_tab.clone(); Callback::from(move |_| cb.emit(port))}}>{"Open in new tab"}</button>}
                        }
                        </div>
                    }
                </article>}
            })}
            if let Some((action, message)) = &*pending {
                <ConfirmModal title="Confirm plugin action" message={format!("{message}\nHost: {}\nProject: {}", props.launcher_id, props.working_directory)} on_cancel={{let pending = pending.clone(); Callback::from(move |_| pending.set(None))}} on_confirm={{let execute = execute.clone(); let action = action.clone(); Callback::from(move |_| execute.emit(action.clone()))}} />
            }
        </section>
    }
}

#[derive(Properties, PartialEq)]
struct PluginCommandProps {
    plugin: String,
    command: PortalPluginCommandInfo,
    disabled: bool,
    on_request: Callback<(PluginAction, String)>,
}

#[function_component(PluginCommand)]
fn plugin_command(props: &PluginCommandProps) -> Html {
    let args = use_state(String::new);
    let run = {
        let args = args.clone();
        let plugin = props.plugin.clone();
        let command = props.command.clone();
        let on_request = props.on_request.clone();
        Callback::from(move |_| {
            let args: Vec<String> = if command.accepts_args {
                args.lines().map(String::from).collect()
            } else {
                Vec::new()
            };
            let message = format!(
                "Run {}:{}?\nCommand: {}\nLiteral arguments: {:?}",
                plugin, command.name, command.run, args
            );
            on_request.emit((
                PluginAction::RunCommand {
                    name: plugin.clone(),
                    command: command.name.clone(),
                    expected_run: command.run.clone(),
                    args,
                    approved: true,
                },
                message,
            ));
        })
    };
    html! {<div class="plugin-command"><h3>{&props.command.name}</h3><p>{props.command.description.as_deref().unwrap_or_default()}</p><code>{&props.command.run}</code>
    if props.command.accepts_args {<label>{"Arguments (one literal argument per line)"}<textarea value={(*args).clone()} disabled={props.disabled} oninput={{let args = args.clone(); Callback::from(move |e: InputEvent| args.set(e.target_unchecked_into::<HtmlTextAreaElement>().value()))}} /></label>}
    <button disabled={props.disabled} onclick={run}>{"Run…"}</button></div>}
}
