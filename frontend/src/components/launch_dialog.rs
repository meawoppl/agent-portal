// TODO(#1165): remove this file-local ratchet after replacing production unwrap/expect paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::components::skip_permissions::{skip_permissions_args, skip_permissions_label};
use crate::components::ProxyTokenSetup;
use crate::hooks::use_escape;
use crate::utils::{self, FetchError, On401};
use gloo::timers::callback::Timeout;
use gloo_net::http::Request;
use shared::api::{DirectoryListingResponse, LaunchRequest, ProbeAgentsResponse};
use shared::{AgentInstall, AgentType, DirectoryEntry, LauncherInfo};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

/// Sentinel value used in the launcher <select> to represent the "connect new host" option.
const CONNECT_NEW: &str = "__install__";

/// Fetch the current install state for both agent CLIs from the given launcher.
/// Stores the result in `agents` and clears `probing` when done.
fn probe_agents_for(
    launcher_id: Uuid,
    agents: UseStateHandle<Vec<AgentInstall>>,
    probing: UseStateHandle<bool>,
) {
    probing.set(true);
    spawn_local(async move {
        let path = format!("/api/launchers/{}/probe-agents", launcher_id);
        match utils::fetch_json::<ProbeAgentsResponse>(&path, On401::Ignore).await {
            Ok(body) => {
                agents.set(body.agents);
            }
            Err(_) => {
                // Probe failures: leave the install list empty. The UI will
                // treat unknown as "not blocking" — better to let the user
                // try and see the spawn-time error than to over-block.
                agents.set(Vec::new());
            }
        }
        probing.set(false);
    });
}

fn agent_installed(installs: &[AgentInstall], agent_type: AgentType) -> Option<bool> {
    installs
        .iter()
        .find(|a| a.agent_type == agent_type)
        .map(|a| a.installed)
}

fn args_placeholder(agent_type: shared::AgentType) -> &'static str {
    match agent_type {
        shared::AgentType::Claude => "ex: --model sonnet --allowedTools \"Bash Edit\"",
        shared::AgentType::Codex => "ex: -c model=gpt-5.5 -c model_reasoning_effort=high",
    }
}

/// One row in the directory browser: folder/file icon plus name, with an
/// optional click handler (folders navigate; files are inert).
fn dir_entry(is_dir: bool, name: &str, onclick: Option<Callback<MouseEvent>>) -> Html {
    let (class, icon) = if is_dir {
        ("dir-entry dir-entry-folder", "\u{1F4C1}")
    } else {
        ("dir-entry dir-entry-file", "\u{1F4C4}")
    };
    html! {
        <div {class} {onclick}>
            <span class="dir-entry-icon">{ icon }</span>
            <span class="dir-entry-name">{ name }</span>
        </div>
    }
}

/// Bundles the four directory-browser state handles so they travel together.
#[derive(Clone)]
struct DirBrowser {
    path: UseStateHandle<String>,
    home_root: UseStateHandle<Option<String>>,
    entries: UseStateHandle<Vec<DirectoryEntry>>,
    loading: UseStateHandle<bool>,
    error: UseStateHandle<Option<String>>,
}

impl DirBrowser {
    /// Navigate to `path`: update the path bar and fetch the listing.
    /// Use this for breadcrumb clicks, directory clicks, and launcher changes.
    fn navigate(&self, launcher_id: Option<Uuid>, path: String) {
        self.path.set(path.clone());
        if let Some(lid) = launcher_id {
            self.fetch(lid, path, true);
        }
    }

    /// Fetch a directory listing for `path` from `launcher_id`.
    /// Pass `update_path = true` when navigating so the path bar is updated to
    /// the server-resolved path (e.g. `~` → `/home/user/`).
    /// Pass `false` when the user is mid-typing so their input isn't overwritten.
    fn fetch(&self, launcher_id: Uuid, path: String, update_path: bool) {
        let browser = self.clone();
        browser.loading.set(true);
        browser.error.set(None);
        spawn_local(async move {
            let api_path = format!(
                "/api/launchers/{}/directories?path={}",
                launcher_id,
                js_sys::encode_uri_component(&path)
            );
            match utils::fetch_json::<DirectoryListingResponse>(&api_path, On401::Ignore).await {
                Ok(listing) => {
                    if update_path && (path == "~" || path == "~/") {
                        browser.home_root.set(listing.resolved_path.clone());
                    }
                    browser.entries.set(listing.entries);
                    if update_path {
                        if let Some(resolved) = listing.resolved_path {
                            browser.path.set(resolved);
                        } else {
                            browser.path.set(path);
                        }
                    }
                }
                Err(FetchError::Decode(_)) => {
                    browser
                        .error
                        .set(Some("Failed to parse response".to_string()));
                }
                Err(FetchError::Status(400)) => {
                    browser.error.set(Some(
                        "Path not found, not readable, or outside home".to_string(),
                    ));
                }
                Err(FetchError::Status(504)) => {
                    browser
                        .error
                        .set(Some("Launcher not responding".to_string()));
                }
                Err(FetchError::Status(status)) => {
                    browser.error.set(Some(format!("Error {}", status)));
                }
                Err(FetchError::Network(e)) => {
                    browser.error.set(Some(format!("Request failed: {}", e)));
                }
            }
            browser.loading.set(false);
        });
    }
}

fn parent_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(idx) => format!("{}/", &trimmed[..idx]),
    }
}

fn clamp_to_home(path: String, home_root: Option<&str>) -> String {
    let Some(home_root) = home_root else {
        return path;
    };
    let root = ensure_trailing_slash(home_root);
    if path == "/" || !path.starts_with(&root) {
        root
    } else {
        path
    }
}

fn ensure_trailing_slash(path: &str) -> String {
    if path.ends_with('/') {
        path.to_string()
    } else {
        format!("{}/", path)
    }
}

fn is_path_home_scoped(path: &str, home_root: Option<&str>) -> bool {
    if path == "~" || path.starts_with("~/") {
        return true;
    }

    let Some(home_root) = home_root else {
        return !path.starts_with('/');
    };
    path.starts_with(&ensure_trailing_slash(home_root)) || path == home_root.trim_end_matches('/')
}

#[derive(Properties, PartialEq)]
pub struct LaunchDialogProps {
    pub on_close: Callback<()>,
    pub on_launched: Callback<()>,
}

#[function_component(LaunchDialog)]
pub fn launch_dialog(props: &LaunchDialogProps) -> Html {
    let launchers = use_state(Vec::<LauncherInfo>::new);
    let selected_launcher = use_state(|| None::<Uuid>);
    // When true the dialog shows ProxyTokenSetup instead of the launch form.
    // Auto-set to true when no launchers are connected; set by the dropdown sentinel.
    let show_install = use_state(|| false);
    let dir = DirBrowser {
        path: use_state(|| "~".to_string()),
        home_root: use_state(|| None::<String>),
        entries: use_state(Vec::<DirectoryEntry>::new),
        loading: use_state(|| false),
        error: use_state(|| None::<String>),
    };
    let extra_args = use_state(String::new);
    let agent_type = use_state(|| shared::AgentType::Claude);
    let skip_permissions = use_state(|| false);
    let launching = use_state(|| false);
    let error_msg = use_state(|| None::<String>);
    let debounce_handle = use_mut_ref(|| None::<Timeout>);
    let agent_installs = use_state(Vec::<AgentInstall>::new);
    let probing_agents = use_state(|| false);

    // Fetch launchers on mount; auto-select install mode when none are connected
    {
        let launchers = launchers.clone();
        let selected_launcher = selected_launcher.clone();
        let show_install = show_install.clone();
        let dir = dir.clone();
        let agent_installs = agent_installs.clone();
        let probing_agents = probing_agents.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if let Ok(data) =
                    utils::fetch_json::<Vec<LauncherInfo>>("/api/launchers", On401::Ignore).await
                {
                    if let Some(first) = data.first() {
                        let lid = first.launcher_id;
                        selected_launcher.set(Some(lid));
                        dir.fetch(lid, "~".to_string(), true);
                        probe_agents_for(lid, agent_installs.clone(), probing_agents.clone());
                    } else {
                        show_install.set(true);
                    }
                    launchers.set(data);
                }
            });
            || ()
        });
    }

    let on_path_input = {
        let selected_launcher = selected_launcher.clone();
        let dir = dir.clone();
        let debounce_handle = debounce_handle.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
                let path = input.value();
                dir.path.set(path.clone());

                // Debounce: cancel previous timer, start new one
                if let Some(lid) = *selected_launcher {
                    let dir = dir.clone();
                    let handle = Timeout::new(300, move || {
                        dir.fetch(lid, path, false); // user is typing — don't overwrite the input
                    });
                    *debounce_handle.borrow_mut() = Some(handle);
                }
            }
        })
    };

    let on_args_input = {
        let extra_args = extra_args.clone();
        Callback::from(move |e: InputEvent| {
            if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
                extra_args.set(input.value());
            }
        })
    };

    let on_agent_type_change = {
        let agent_type = agent_type.clone();
        Callback::from(move |e: Event| {
            if let Some(select) = e.target_dyn_into::<web_sys::HtmlSelectElement>() {
                let val = select.value();
                agent_type.set(if val == "codex" {
                    shared::AgentType::Codex
                } else {
                    shared::AgentType::Claude
                });
            }
        })
    };

    let on_skip_permissions = {
        let skip_permissions = skip_permissions.clone();
        Callback::from(move |e: Event| {
            if let Some(input) = e.target_dyn_into::<HtmlInputElement>() {
                skip_permissions.set(input.checked());
            }
        })
    };

    // navigate_to: Yew's Callback<String> is Rc-backed and cheap to clone,
    // replacing the previous Rc<dyn Fn(String)>. Call sites use .emit(path).
    let navigate_to: Callback<String> = {
        let selected_launcher = selected_launcher.clone();
        let dir = dir.clone();
        Callback::from(move |path: String| {
            let path = clamp_to_home(path, (*dir.home_root).as_deref());
            dir.navigate(*selected_launcher, path);
        })
    };

    let on_path_keydown = {
        let dir = dir.clone();
        let navigate_to = navigate_to.clone();
        Callback::from(move |e: KeyboardEvent| {
            if e.key() == "Tab" {
                let dirs: Vec<&DirectoryEntry> =
                    dir.entries.iter().filter(|ent| ent.is_dir).collect();
                if dirs.len() == 1 {
                    e.prevent_default();
                    let base = if (*dir.path).ends_with('/') {
                        (*dir.path).clone()
                    } else {
                        parent_path(&dir.path)
                    };
                    let child = format!("{}{}/", base, dirs[0].name);
                    navigate_to.emit(child);
                }
            }
        })
    };

    let on_launcher_change = {
        let selected_launcher = selected_launcher.clone();
        let show_install = show_install.clone();
        let dir = dir.clone();
        let agent_installs = agent_installs.clone();
        let probing_agents = probing_agents.clone();
        Callback::from(move |e: Event| {
            if let Some(select) = e.target_dyn_into::<web_sys::HtmlSelectElement>() {
                if select.value() == CONNECT_NEW {
                    show_install.set(true);
                    selected_launcher.set(None);
                } else if let Ok(id) = select.value().parse::<Uuid>() {
                    show_install.set(false);
                    selected_launcher.set(Some(id));
                    dir.navigate(Some(id), "~".to_string());
                    probe_agents_for(id, agent_installs.clone(), probing_agents.clone());
                }
            }
        })
    };

    let on_launch = {
        let dir_path = dir.path.clone();
        let home_root = dir.home_root.clone();
        let extra_args = extra_args.clone();
        let agent_type = agent_type.clone();
        let skip_permissions = skip_permissions.clone();
        let selected_launcher = selected_launcher.clone();
        let launching = launching.clone();
        let error_msg = error_msg.clone();
        let on_close = props.on_close.clone();
        let on_launched = props.on_launched.clone();
        Callback::from(move |_| {
            let working_dir = (*dir_path).clone();
            if working_dir.is_empty() {
                error_msg.set(Some("Working directory is required".to_string()));
                return;
            }
            if !is_path_home_scoped(&working_dir, (*home_root).as_deref()) {
                error_msg.set(Some(
                    "Choose a directory under the launcher's home folder".to_string(),
                ));
                return;
            }

            let mut claude_args: Vec<String> = (*extra_args)
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            if *skip_permissions {
                claude_args.extend(
                    skip_permissions_args(*agent_type)
                        .iter()
                        .map(|arg| arg.to_string()),
                );
            }

            let launcher_id = *selected_launcher;
            let selected_agent_type = *agent_type;
            let launching = launching.clone();
            let error_msg = error_msg.clone();
            let on_close = on_close.clone();
            let on_launched = on_launched.clone();

            launching.set(true);
            error_msg.set(None);

            spawn_local(async move {
                let body = LaunchRequest {
                    working_directory: working_dir,
                    launcher_id,
                    claude_args,
                    agent_type: selected_agent_type,
                };

                match Request::post("/api/launch")
                    .json(&body)
                    .unwrap()
                    .send()
                    .await
                {
                    Ok(resp) if resp.ok() => {
                        on_launched.emit(());
                        on_close.emit(());
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        let text = resp.text().await.unwrap_or_default();
                        if status == 404 {
                            error_msg.set(Some("No connected launchers".to_string()));
                        } else {
                            error_msg.set(Some(format!("Error {}: {}", status, text)));
                        }
                    }
                    Err(e) => {
                        error_msg.set(Some(format!("Request failed: {}", e)));
                    }
                }
                launching.set(false);
            });
        })
    };

    let on_backdrop = {
        let on_close = props.on_close.clone();
        Callback::from(move |_| on_close.emit(()))
    };

    // Close on Escape key
    use_escape(props.on_close.clone());

    // Build breadcrumb segments from current path
    let path_str = (*dir.path).clone();
    let breadcrumbs: Vec<(String, String)> = if let Some(home_root) = (*dir.home_root).as_deref() {
        let root = ensure_trailing_slash(home_root);
        let mut segs = vec![(root.clone(), "~".to_string())];
        let trimmed = path_str
            .strip_prefix(&root)
            .unwrap_or("")
            .trim_start_matches('/');
        if !trimmed.is_empty() {
            let mut built = root;
            for part in trimmed.split('/') {
                if part.is_empty() {
                    continue;
                }
                built.push_str(part);
                built.push('/');
                segs.push((built.clone(), part.to_string()));
            }
        }
        segs
    } else {
        vec![("~".to_string(), "~".to_string())]
    };

    // Find selected launcher info for subtitle
    let selected_info: Option<LauncherInfo> = (*selected_launcher)
        .and_then(|lid| launchers.iter().find(|l| l.launcher_id == lid).cloned());

    // Per-agent install hints for the dropdown labels and the inline warning.
    let claude_label = match agent_installed(&agent_installs, AgentType::Claude) {
        Some(false) => "Claude (not installed)".to_string(),
        _ => "Claude".to_string(),
    };
    let codex_label = match agent_installed(&agent_installs, AgentType::Codex) {
        Some(false) => "Codex (not installed)".to_string(),
        _ => "Codex".to_string(),
    };
    let selected_agent_missing = agent_installed(&agent_installs, *agent_type) == Some(false);
    let still_probing = *probing_agents && agent_installs.is_empty();
    let selected_agent_label = match *agent_type {
        AgentType::Claude => "Claude",
        AgentType::Codex => "Codex",
    };

    // Pre-compute directory listing HTML
    let dir_listing_html = if *dir.loading {
        html! { <div class="dir-loading">{ "Loading..." }</div> }
    } else if let Some(ref err) = *dir.error {
        html! { <div class="dir-error-msg">{ err }</div> }
    } else if dir.entries.is_empty() {
        html! { <div class="dir-empty">{ "Empty directory" }</div> }
    } else {
        let parent = clamp_to_home(parent_path(&dir.path), (*dir.home_root).as_deref());
        let on_up = {
            let navigate_to = navigate_to.clone();
            Callback::from(move |_: MouseEvent| navigate_to.emit(parent.clone()))
        };
        let entries_html = dir
            .entries
            .iter()
            .map(|entry| {
                let onclick = entry.is_dir.then(|| {
                    let base = if (*dir.path).ends_with('/') {
                        (*dir.path).clone()
                    } else {
                        parent_path(&dir.path)
                    };
                    let child = format!("{}{}/", base, entry.name);
                    let navigate_to = navigate_to.clone();
                    Callback::from(move |_: MouseEvent| navigate_to.emit(child.clone()))
                });
                dir_entry(entry.is_dir, &entry.name, onclick)
            })
            .collect::<Html>();
        html! {
            <>
                { dir_entry(true, "..", Some(on_up)) }
                { entries_html }
            </>
        }
    };

    // Launcher dropdown — always visible regardless of mode.
    // Real launchers are listed first; a disabled divider and "+ Connect New Host"
    // sentinel option follow so the user can switch to the install flow.
    let launcher_select_html = html! {
        <div class="launch-field">
            <label>{ "Launcher" }</label>
            <select class="launcher-select" onchange={on_launcher_change}>
                { launchers.iter().map(|l| {
                    let selected = !*show_install && *selected_launcher == Some(l.launcher_id);
                    html! {
                        <option value={l.launcher_id.to_string()} {selected}>
                            { &l.launcher_name }
                        </option>
                    }
                }).collect::<Html>() }
                if !launchers.is_empty() {
                    <option disabled=true value="">{ "──────────────" }</option>
                }
                <option value={CONNECT_NEW} selected={*show_install}>
                    { "+ Connect New Host" }
                </option>
            </select>
            if let Some(ref info) = selected_info {
                <span class="launcher-subtitle">
                    { format!("{} running", info.running_sessions) }
                </span>
            }
        </div>
    };

    html! {
        <div class="launch-dialog-backdrop" onclick={on_backdrop}>
            <div class="launch-dialog" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                <h3>{ "Launch Session" }</h3>

                { launcher_select_html }

                if *show_install {
                    // Install mode: show setup instructions
                    <ProxyTokenSetup />
                    <div class="launch-actions">
                        <button
                            class="launch-button-cancel"
                            onclick={
                                let on_close = props.on_close.clone();
                                Callback::from(move |_| on_close.emit(()))
                            }
                        >
                            { "Close" }
                        </button>
                    </div>
                } else {
                    // Launch mode: agent selector, directory browser, args, actions
                    <div class="launch-field">
                        <label>{ "Agent" }</label>
                        <select class="launcher-select" onchange={on_agent_type_change}>
                            <option value="claude" selected={*agent_type == AgentType::Claude}>
                                { &claude_label }
                            </option>
                            <option value="codex" selected={*agent_type == AgentType::Codex}>
                                { &codex_label }
                            </option>
                        </select>
                    </div>

                    if selected_agent_missing {
                        <div class="launch-note launch-note-warn">
                            { format!(
                                "{} isn't installed on this launcher — sessions will fail to start. Install it on the host and retry.",
                                selected_agent_label,
                            ) }
                        </div>
                    } else if still_probing {
                        <div class="launch-note">
                            { "Checking installed agents..." }
                        </div>
                    }

                    if *agent_type == AgentType::Codex {
                        <div class="launch-note launch-note-warn">
                            { "Codex support is highly experimental." }
                        </div>
                    }

                    // Directory browser
                    <div class="launch-field">
                        <label>{ "Directory (home folder only)" }</label>
                        <input
                            type="text"
                            class="dir-path-input"
                            placeholder="~/project"
                            value={(*dir.path).clone()}
                            oninput={on_path_input}
                            onkeydown={on_path_keydown.clone()}
                        />
                        <div class="dir-breadcrumb">
                            { breadcrumbs.iter().enumerate().map(|(i, (full_path, label))| {
                                let p = full_path.clone();
                                let is_last = i == breadcrumbs.len() - 1;
                                let onclick = {
                                    let navigate_to = navigate_to.clone();
                                    Callback::from(move |e: MouseEvent| {
                                        e.prevent_default();
                                        navigate_to.emit(p.clone());
                                    })
                                };
                                html! {
                                    <>
                                        if i > 0 {
                                            <span class="dir-breadcrumb-sep">{ "/" }</span>
                                        }
                                        <a
                                            class={classes!("dir-breadcrumb-seg", is_last.then_some("active"))}
                                            href="#"
                                            {onclick}
                                        >
                                            { label }
                                        </a>
                                    </>
                                }
                            }).collect::<Html>() }
                        </div>
                        <div class="dir-browser">
                            { dir_listing_html }
                        </div>
                    </div>

                    // Extra CLI arguments
                    <div class="launch-field">
                        <label>{ "Extra CLI Arguments (optional)" }</label>
                        <input
                            type="text"
                            placeholder={args_placeholder(*agent_type)}
                            value={(*extra_args).clone()}
                            oninput={on_args_input}
                        />
                    </div>

                    // Permission bypass checkbox (agent-specific)
                    <div class="launch-field launch-checkbox">
                        <label>
                            <input
                                type="checkbox"
                                checked={*skip_permissions}
                                onchange={on_skip_permissions.clone()}
                            />
                            { format!(" {}", skip_permissions_label(*agent_type)) }
                        </label>
                    </div>

                    if let Some(ref err) = *error_msg {
                        <p class="launch-error">{ err }</p>
                    }

                    <div class="launch-actions">
                        <button
                            class="launch-button-cancel"
                            onclick={
                                let on_close = props.on_close.clone();
                                Callback::from(move |_| on_close.emit(()))
                            }
                        >
                            { "Cancel" }
                        </button>
                        <button
                            class="launch-button"
                            onclick={on_launch}
                            disabled={*launching}
                        >
                            { if *launching { "Launching..." } else { "Launch" } }
                        </button>
                    </div>
                }
            </div>
        </div>
    }
}
