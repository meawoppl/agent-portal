use crate::components::ProxyTokenSetup;
use gloo::timers::callback::Timeout;
use gloo_net::http::Request;
use serde::Deserialize;
use shared::api::LaunchRequest;
use shared::{DirectoryEntry, LauncherInfo};
use uuid::Uuid;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

#[derive(Deserialize)]
struct DirectoryListingResponse {
    entries: Vec<DirectoryEntry>,
    resolved_path: Option<String>,
}

struct AgentConfig {
    args_placeholder: &'static str,
    skip_permissions_flag: Option<&'static str>,
}

fn agent_config(agent_type: shared::AgentType) -> AgentConfig {
    match agent_type {
        shared::AgentType::Claude => AgentConfig {
            args_placeholder: "--model sonnet --allowedTools \"Bash Edit\"",
            skip_permissions_flag: Some("--dangerously-skip-permissions"),
        },
        shared::AgentType::Codex => AgentConfig {
            args_placeholder: "--model o3 --reasoning-effort high",
            skip_permissions_flag: Some("--full-auto"),
        },
    }
}

/// Bundles the four directory-browser state handles so they travel together.
#[derive(Clone)]
struct DirBrowser {
    path: UseStateHandle<String>,
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
            let url = format!(
                "/api/launchers/{}/directories?path={}",
                launcher_id,
                js_sys::encode_uri_component(&path)
            );
            match Request::get(&url).send().await {
                Ok(resp) if resp.ok() => {
                    if let Ok(listing) = resp.json::<DirectoryListingResponse>().await {
                        browser.entries.set(listing.entries);
                        if update_path {
                            if let Some(resolved) = listing.resolved_path {
                                browser.path.set(resolved);
                            } else {
                                browser.path.set(path);
                            }
                        }
                    } else {
                        browser
                            .error
                            .set(Some("Failed to parse response".to_string()));
                    }
                }
                Ok(resp) => {
                    let status = resp.status();
                    if status == 400 {
                        browser
                            .error
                            .set(Some("Path not found or not readable".to_string()));
                    } else if status == 504 {
                        browser
                            .error
                            .set(Some("Launcher not responding".to_string()));
                    } else {
                        browser.error.set(Some(format!("Error {}", status)));
                    }
                }
                Err(e) => {
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

#[derive(Properties, PartialEq)]
pub struct LaunchDialogProps {
    pub on_close: Callback<()>,
    pub on_launched: Callback<()>,
}

#[function_component(LaunchDialog)]
pub fn launch_dialog(props: &LaunchDialogProps) -> Html {
    let launchers = use_state(Vec::<LauncherInfo>::new);
    let selected_launcher = use_state(|| None::<Uuid>);
    let dir = DirBrowser {
        path: use_state(|| "~".to_string()),
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

    // Fetch launchers on mount
    {
        let launchers = launchers.clone();
        let selected_launcher = selected_launcher.clone();
        let dir = dir.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if let Ok(resp) = Request::get("/api/launchers").send().await {
                    if let Ok(data) = resp.json::<Vec<LauncherInfo>>().await {
                        if let Some(first) = data.first() {
                            let lid = first.launcher_id;
                            selected_launcher.set(Some(lid));
                            dir.fetch(lid, "~".to_string(), true);
                        }
                        launchers.set(data);
                    }
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

    let on_path_keydown = {
        let selected_launcher = selected_launcher.clone();
        let dir = dir.clone();
        Callback::from(move |e: KeyboardEvent| {
            if e.key() != "Tab" || e.shift_key() {
                return;
            }
            e.prevent_default();

            let path = (*dir.path).clone();
            if path.ends_with('/') || path.is_empty() {
                return;
            }

            let base = match path.rfind('/') {
                Some(idx) => &path[..=idx],
                None => "",
            };

            let entries = &*dir.entries;
            if entries.is_empty() {
                return;
            }

            // Longest common prefix of all entry names
            let first = &entries[0].name;
            let mut len = first.len();
            for entry in entries.iter().skip(1) {
                len = first
                    .chars()
                    .zip(entry.name.chars())
                    .take(len)
                    .take_while(|(a, b)| a == b)
                    .count();
            }
            let lcp = &first[..len];

            let completed = if entries.len() == 1 && entries[0].is_dir {
                format!("{}{}/", base, lcp)
            } else {
                format!("{}{}", base, lcp)
            };

            if completed != path {
                dir.path.set(completed.clone());
                if let Some(lid) = *selected_launcher {
                    dir.fetch(lid, completed, false);
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
            dir.navigate(*selected_launcher, path);
        })
    };

    let on_launcher_change = {
        let selected_launcher = selected_launcher.clone();
        let dir = dir.clone();
        Callback::from(move |e: Event| {
            if let Some(select) = e.target_dyn_into::<web_sys::HtmlSelectElement>() {
                if let Ok(id) = select.value().parse::<Uuid>() {
                    selected_launcher.set(Some(id));
                    // Always start at home — don't carry over last-used directory
                    dir.navigate(Some(id), "~".to_string());
                }
            }
        })
    };

    let on_launch = {
        let dir_path = dir.path.clone();
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

            let mut claude_args: Vec<String> = (*extra_args)
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            let cfg = agent_config(*agent_type);
            if *skip_permissions {
                if let Some(flag) = cfg.skip_permissions_flag {
                    claude_args.push(flag.to_string());
                }
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
    {
        let on_close = props.on_close.clone();
        use_effect_with((), move |_| {
            let listener =
                gloo::events::EventListener::new(&gloo::utils::document(), "keydown", move |e| {
                    if let Ok(ke) = e.clone().dyn_into::<web_sys::KeyboardEvent>() {
                        if ke.key() == "Escape" {
                            on_close.emit(());
                        }
                    }
                });
            move || drop(listener)
        });
    }

    // Build breadcrumb segments from current path
    let path_str = (*dir.path).clone();
    let breadcrumbs: Vec<(String, String)> = {
        let mut segs = vec![("/".to_string(), "/".to_string())];
        let trimmed = path_str.trim_start_matches('/');
        if !trimmed.is_empty() {
            let mut built = String::from("/");
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
    };

    // Find selected launcher info for subtitle
    let selected_info: Option<LauncherInfo> = (*selected_launcher)
        .and_then(|lid| launchers.iter().find(|l| l.launcher_id == lid).cloned());

    let cfg = agent_config(*agent_type);

    // Pre-compute directory listing HTML
    let dir_listing_html = if *dir.loading {
        html! { <div class="dir-loading">{ "Loading..." }</div> }
    } else if let Some(ref err) = *dir.error {
        html! { <div class="dir-error-msg">{ err }</div> }
    } else if dir.entries.is_empty() {
        html! { <div class="dir-empty">{ "Empty directory" }</div> }
    } else {
        let parent = parent_path(&dir.path);
        let on_up = {
            let navigate_to = navigate_to.clone();
            Callback::from(move |_: MouseEvent| navigate_to.emit(parent.clone()))
        };
        let entries_html = dir
            .entries
            .iter()
            .map(|entry| {
                if entry.is_dir {
                    let mut child = (*dir.path).clone();
                    if !child.ends_with('/') {
                        child.push('/');
                    }
                    child.push_str(&entry.name);
                    child.push('/');
                    let onclick = {
                        let navigate_to = navigate_to.clone();
                        Callback::from(move |_: MouseEvent| navigate_to.emit(child.clone()))
                    };
                    html! {
                        <div class="dir-entry dir-entry-folder" onclick={onclick}>
                            <span class="dir-entry-icon">{ "\u{1F4C1}" }</span>
                            <span class="dir-entry-name">{ &entry.name }</span>
                        </div>
                    }
                } else {
                    html! {
                        <div class="dir-entry dir-entry-file">
                            <span class="dir-entry-icon">{ "\u{1F4C4}" }</span>
                            <span class="dir-entry-name">{ &entry.name }</span>
                        </div>
                    }
                }
            })
            .collect::<Html>();
        html! {
            <>
                <div class="dir-entry dir-entry-folder" onclick={on_up}>
                    <span class="dir-entry-icon">{ "\u{1F4C1}" }</span>
                    <span class="dir-entry-name">{ ".." }</span>
                </div>
                { entries_html }
            </>
        }
    };

    html! {
        <div class="launch-dialog-backdrop" onclick={on_backdrop}>
            <div class="launch-dialog" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                <h3>{ "Launch Session" }</h3>

                if launchers.is_empty() {
                    <div class="launch-no-launchers">
                        <p>{ "No launchers connected. Install agent-portal on a machine to get started:" }</p>
                        <ProxyTokenSetup />
                    </div>
                } else {
                    // Launcher + Agent selectors (side by side)
                    <div class="launch-row">
                        <div class="launch-field launch-field-half">
                            <label>{ "Launcher" }</label>
                            <select class="launcher-select" onchange={on_launcher_change}>
                                { launchers.iter().map(|l| {
                                    let selected = *selected_launcher == Some(l.launcher_id);
                                    html! {
                                        <option value={l.launcher_id.to_string()} {selected}>
                                            { &l.launcher_name }
                                        </option>
                                    }
                                }).collect::<Html>() }
                            </select>
                            if let Some(ref info) = selected_info {
                                <span class="launcher-subtitle">
                                    { format!("{} running", info.running_sessions) }
                                </span>
                            }
                        </div>
                        <div class="launch-field launch-field-half">
                            <label>{ "Agent" }</label>
                            <select class="launcher-select" onchange={on_agent_type_change}>
                                <option value="claude" selected={*agent_type == shared::AgentType::Claude}>
                                    { "Claude" }
                                </option>
                                <option value="codex" selected={*agent_type == shared::AgentType::Codex}>
                                    { "Codex" }
                                </option>
                            </select>
                        </div>
                    </div>

                    if *agent_type == shared::AgentType::Codex {
                        <div class="launch-note launch-note-warn">
                            { "Codex support is highly experimental." }
                        </div>
                    }

                    // Directory browser
                    <div class="launch-field">
                        <label>{ "Directory" }</label>
                        <input
                            type="text"
                            class="dir-path-input"
                            value={(*dir.path).clone()}
                            oninput={on_path_input}
                            onkeydown={on_path_keydown}
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
                                        if i > 1 {
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
                            placeholder={cfg.args_placeholder}
                            value={(*extra_args).clone()}
                            oninput={on_args_input}
                        />
                    </div>

                    // Permission bypass checkbox (agent-specific)
                    if let Some(flag) = cfg.skip_permissions_flag {
                        <div class="launch-field launch-checkbox">
                            <label>
                                <input
                                    type="checkbox"
                                    checked={*skip_permissions}
                                    onchange={on_skip_permissions.clone()}
                                />
                                { format!(" {}", flag) }
                            </label>
                        </div>
                    }

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
