use crate::components::model_select::{extract_model_arg, model_cli_args};
use crate::components::skip_permissions::{
    skip_permissions_args, skip_permissions_label, strip_skip_permissions_args,
};
use crate::components::{FloatingPane, ModelSelect};
use crate::utils::{self, On401};
use gloo::timers::callback::Timeout;
use gloo_net::http::Request;
use shared::api::{
    CreateScheduledTaskRequest, ScheduledTaskInfo, ScheduledTaskListResponse,
    UpdateScheduledTaskRequest,
};
use shared::{AgentInstall, DirectoryEntry, LauncherInfo, SessionInfo};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use web_sys::HtmlInputElement;
use yew::prelude::*;

/// Minimum launcher version that supports scheduled tasks.
const MIN_LAUNCHER_VERSION: &str = "2.1.2";

fn version_sufficient(version: &str) -> bool {
    let parse = |s: &str| -> Option<(u64, u64, u64)> {
        let mut parts = s.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some((major, minor, patch))
    };
    let Some(have) = parse(version) else {
        return false;
    };
    let Some(need) = parse(MIN_LAUNCHER_VERSION) else {
        return true;
    };
    have >= need
}

/// The browser's IANA timezone (e.g. `America/Los_Angeles`) via
/// `Intl.DateTimeFormat().resolvedOptions().timeZone`, or `"UTC"` if it can't
/// be read. Seeding the schedule field with this starts it as a valid IANA name
/// instead of an abbreviation the launcher can't parse (#1064).
fn detected_timezone() -> String {
    let fmt = js_sys::Intl::DateTimeFormat::new(&js_sys::Array::new(), &js_sys::Object::new());
    js_sys::Reflect::get(
        &fmt.resolved_options(),
        &wasm_bindgen::JsValue::from_str("timeZone"),
    )
    .ok()
    .and_then(|v| v.as_string())
    .filter(|s| !s.is_empty())
    .unwrap_or_else(|| "UTC".to_string())
}

#[derive(Properties, PartialEq)]
pub struct ScheduleDialogProps {
    #[prop_or_default]
    pub session: Option<SessionInfo>,
    pub on_close: Callback<()>,
}

#[derive(Clone, Default)]
struct TaskForm {
    working_directory: String,
    session_name: String,
    hostname: String,
    agent_type: shared::AgentType,
    worktree: shared::WorktreeMode,
    worktree_branch: String,
    name: String,
    cron_expression: String,
    timezone: String,
    prompt: String,
    max_runtime_minutes: i32,
    /// Selected model CLI arg, or "" for the agent's own default.
    model_arg: String,
    extra_args: String,
    skip_permissions: bool,
    /// Fresh session each run vs. continue the prior conversation.
    session_mode: shared::SessionMode,
}

#[derive(Clone, PartialEq)]
enum FormMode {
    Create,
    Edit(Uuid),
}

use super::cron_describe;
use super::launch_dialog::{
    clamp_to_home, dir_entry, ensure_trailing_slash, is_path_home_scoped, load_last_launch_dir_for,
    load_last_launcher, parent_path, probe_agents_for, DirBrowser,
};
use super::launch_target_picker::{AgentTargetSelect, DirectoryTargetSelect, LauncherTargetSelect};

#[function_component(ScheduleDialog)]
pub fn schedule_dialog(props: &ScheduleDialogProps) -> Html {
    let tasks = use_state(Vec::<ScheduledTaskInfo>::new);
    let loading = use_state(|| true);
    let form_mode = use_state(|| None::<FormMode>);
    let form = use_state(TaskForm::default);
    let error_msg = use_state(|| None::<String>);
    let confirm_delete = use_state(|| None::<Uuid>);
    let launchers = use_state(Vec::<LauncherInfo>::new);
    let selected_launcher = use_state(|| None::<Uuid>);
    let agent_installs = use_state(Vec::<AgentInstall>::new);
    let probing_agents = use_state(|| false);
    let dir = DirBrowser {
        path: use_state(|| "~".to_string()),
        home_root: use_state(|| None::<String>),
        entries: use_state(Vec::<DirectoryEntry>::new),
        loading: use_state(|| false),
        error: use_state(|| None::<String>),
    };
    let debounce_handle = use_mut_ref(|| None::<Timeout>);

    let working_directory = props
        .session
        .as_ref()
        .map(|s| s.working_directory.clone())
        .unwrap_or_default();
    let hostname = props
        .session
        .as_ref()
        .map(|s| s.hostname.clone())
        .unwrap_or_default();
    let session_agent_type = props
        .session
        .as_ref()
        .map(|s| s.agent_type)
        .unwrap_or_default();

    let folder = utils::extract_folder(&working_directory).to_string();

    // Use the same live launcher inventory as the session launcher. A schedule
    // targets a hostname on the wire, while directory browsing needs the live
    // launcher's UUID, so the form keeps both in sync.
    {
        let launchers = launchers.clone();
        let selected_launcher = selected_launcher.clone();
        let dir = dir.clone();
        let agent_installs = agent_installs.clone();
        let probing_agents = probing_agents.clone();
        let hostname = hostname.clone();
        let working_directory = working_directory.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if let Ok(data) = utils::fetch_launchers().await {
                    let chosen = data
                        .iter()
                        .find(|launcher| !hostname.is_empty() && launcher.hostname == hostname)
                        .map(|launcher| launcher.launcher_id)
                        .or_else(|| {
                            load_last_launcher().filter(|id| {
                                data.iter().any(|launcher| launcher.launcher_id == *id)
                            })
                        })
                        .or_else(|| data.first().map(|launcher| launcher.launcher_id));
                    if let Some(launcher_id) = chosen {
                        selected_launcher.set(Some(launcher_id));
                        let initial_dir = utils::owned_non_blank(&working_directory)
                            .or_else(|| load_last_launch_dir_for(launcher_id));
                        dir.fetch_initial(launcher_id, initial_dir);
                        probe_agents_for(
                            launcher_id,
                            agent_installs.clone(),
                            probing_agents.clone(),
                        );
                    }
                    launchers.set(data);
                }
            });
            || ()
        });
    }

    let selected_launcher_info = (*selected_launcher)
        .and_then(|id| launchers.iter().find(|launcher| launcher.launcher_id == id));
    let can_schedule =
        selected_launcher_info.is_some_and(|launcher| version_sufficient(&launcher.version));

    // The browser owns path resolution/listing state; mirror only its current
    // resolved path into the task draft. This is the same split used by the
    // launch dialog and keeps typed paths stable while an async listing loads.
    {
        let form = form.clone();
        let form_mode = form_mode.clone();
        let path = (*dir.path).clone();
        use_effect_with(path.clone(), move |_| {
            if form_mode.is_some() {
                let mut next = (*form).clone();
                next.working_directory = path;
                form.set(next);
            }
            || ()
        });
    }

    let reload_tasks = {
        let tasks = tasks.clone();
        let loading = loading.clone();
        let wd = working_directory.clone();
        Callback::from(move |_| {
            let tasks = tasks.clone();
            let loading = loading.clone();
            let wd = wd.clone();
            spawn_local(async move {
                if let Ok(data) = utils::fetch_json::<ScheduledTaskListResponse>(
                    "/api/scheduled-tasks",
                    On401::Ignore,
                )
                .await
                {
                    let filtered: Vec<_> = data
                        .tasks
                        .into_iter()
                        .filter(|t| wd.is_empty() || t.fields.launch.working_directory == wd)
                        .collect();
                    tasks.set(filtered);
                }
                loading.set(false);
            });
        })
    };

    {
        let reload_tasks = reload_tasks.clone();
        use_effect_with((), move |_| {
            reload_tasks.emit(());
            || ()
        });
    }

    let open_create = {
        let form_mode = form_mode.clone();
        let form = form.clone();
        let error_msg = error_msg.clone();
        let working_directory = working_directory.clone();
        let hostname = hostname.clone();
        let launchers = launchers.clone();
        let selected_launcher = selected_launcher.clone();
        let dir = dir.clone();
        let agent_installs = agent_installs.clone();
        let probing_agents = probing_agents.clone();
        Callback::from(move |_| {
            let launcher = (*selected_launcher)
                .and_then(|id| launchers.iter().find(|item| item.launcher_id == id));
            let selected_hostname = launcher
                .map(|item| item.hostname.clone())
                .unwrap_or_else(|| hostname.clone());
            let selected_directory = if working_directory.is_empty() {
                (*dir.path).clone()
            } else {
                working_directory.clone()
            };
            if let Some(launcher) = launcher {
                dir.fetch_initial(
                    launcher.launcher_id,
                    utils::owned_non_blank(&selected_directory)
                        .or_else(|| load_last_launch_dir_for(launcher.launcher_id)),
                );
                probe_agents_for(
                    launcher.launcher_id,
                    agent_installs.clone(),
                    probing_agents.clone(),
                );
            }
            form.set(TaskForm {
                working_directory: selected_directory,
                hostname: selected_hostname,
                agent_type: session_agent_type,
                timezone: detected_timezone(),
                max_runtime_minutes: 30,
                // Preserve the established auto-enabled setting for agents
                // that have a permission bypass. Muse's broader YOLO mode must
                // be opt-in, and Antigravity is a read-only preview with no
                // permission-widening flag at all.
                skip_permissions: !matches!(
                    session_agent_type,
                    shared::AgentType::Muse | shared::AgentType::Antigravity
                ),
                ..Default::default()
            });
            error_msg.set(None);
            form_mode.set(Some(FormMode::Create));
        })
    };

    let open_edit = {
        let form_mode = form_mode.clone();
        let form = form.clone();
        let tasks = tasks.clone();
        let error_msg = error_msg.clone();
        let launchers = launchers.clone();
        let selected_launcher = selected_launcher.clone();
        let dir = dir.clone();
        let agent_installs = agent_installs.clone();
        let probing_agents = probing_agents.clone();
        Callback::from(move |task_id: Uuid| {
            if let Some(task) = tasks.iter().find(|t| t.id == task_id) {
                let (has_skip, other_args) = strip_skip_permissions_args(
                    &task.fields.launch.claude_args,
                    task.fields.launch.agent_type,
                );
                // Pull a picker-selectable model out of the remaining args so it
                // pre-selects in the picker instead of sitting in the extra-args
                // field (where it would double-apply on save). An unrecognized
                // model value stays in `extra_args` untouched.
                let (model_arg, extra_args) =
                    extract_model_arg(&other_args, task.fields.launch.agent_type);
                let live_launcher = launchers
                    .iter()
                    .find(|launcher| launcher.hostname == task.hostname);
                selected_launcher.set(live_launcher.map(|launcher| launcher.launcher_id));
                dir.path.set(task.fields.launch.working_directory.clone());
                dir.entries.set(Vec::new());
                dir.error.set(None);
                if let Some(launcher) = live_launcher {
                    dir.fetch_initial(
                        launcher.launcher_id,
                        Some(task.fields.launch.working_directory.clone()),
                    );
                    probe_agents_for(
                        launcher.launcher_id,
                        agent_installs.clone(),
                        probing_agents.clone(),
                    );
                }
                form.set(TaskForm {
                    name: task.fields.name.clone(),
                    cron_expression: task.fields.cron_expression.clone(),
                    timezone: task.fields.timezone.clone(),
                    prompt: task.fields.prompt.clone(),
                    max_runtime_minutes: task.fields.max_runtime_minutes,
                    model_arg: model_arg.unwrap_or_default(),
                    extra_args: extra_args.join(" "),
                    skip_permissions: has_skip,
                    session_mode: task.fields.session_mode,
                    working_directory: task.fields.launch.working_directory.clone(),
                    session_name: task.fields.launch.session_name.clone().unwrap_or_default(),
                    hostname: task.hostname.clone(),
                    agent_type: task.fields.launch.agent_type,
                    worktree_branch: task
                        .fields
                        .launch
                        .worktree
                        .branch()
                        .unwrap_or_default()
                        .to_string(),
                    worktree: task.fields.launch.worktree.clone(),
                });
                error_msg.set(None);
                form_mode.set(Some(FormMode::Edit(task_id)));
            }
        })
    };

    let close_form = {
        let form_mode = form_mode.clone();
        Callback::from(move |_| form_mode.set(None))
    };

    let on_submit = {
        let form = form.clone();
        let form_mode = form_mode.clone();
        let reload_tasks = reload_tasks.clone();
        let error_msg = error_msg.clone();
        let selected_launcher = selected_launcher.clone();
        let home_root = dir.home_root.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let data = (*form).clone();
            let mode = (*form_mode).clone();
            let reload_tasks = reload_tasks.clone();
            let form_mode = form_mode.clone();
            let error_msg = error_msg.clone();
            let wd = data.working_directory.trim().to_string();
            let host = data.hostname.trim().to_string();
            let agent_type = data.agent_type;

            if !utils::is_non_blank(&data.name)
                || !utils::is_non_blank(&data.cron_expression)
                || !utils::is_non_blank(&data.hostname)
                || !utils::is_non_blank(&data.working_directory)
                || !utils::is_non_blank(&data.prompt)
            {
                return;
            }
            if selected_launcher.is_some() && !is_path_home_scoped(&wd, (*home_root).as_deref()) {
                error_msg.set(Some(
                    "Choose a directory under the selected host's home folder".to_string(),
                ));
                return;
            }

            spawn_local(async move {
                // Picker args go first so an explicit --model / -c model=… typed
                // into the extra-args field still wins (both CLIs take the last
                // occurrence).
                let mut claude_args: Vec<String> = model_cli_args(agent_type, &data.model_arg);
                let extra = data.extra_args.trim();
                if !extra.is_empty() {
                    claude_args.extend(extra.split_whitespace().map(String::from));
                }
                if data.skip_permissions {
                    claude_args.extend(
                        skip_permissions_args(agent_type)
                            .iter()
                            .map(|arg| arg.to_string()),
                    );
                }

                let result = match mode {
                    Some(FormMode::Create) => {
                        let body = CreateScheduledTaskRequest {
                            fields: shared::ScheduledTaskFields {
                                name: data.name.trim().to_string(),
                                cron_expression: data.cron_expression.trim().to_string(),
                                timezone: shared::timezone::canonicalize_timezone(&data.timezone),
                                launch: shared::LaunchSpec {
                                    working_directory: wd,
                                    session_name: utils::owned_non_blank(&data.session_name),
                                    claude_args: claude_args.clone(),
                                    agent_type,
                                    worktree: match data.worktree {
                                        shared::WorktreeMode::Repo { .. } => {
                                            shared::WorktreeMode::Repo {
                                                branch: utils::owned_non_blank(
                                                    &data.worktree_branch,
                                                ),
                                            }
                                        }
                                        shared::WorktreeMode::Scratch { .. } => {
                                            shared::WorktreeMode::Scratch {
                                                branch: utils::owned_non_blank(
                                                    &data.worktree_branch,
                                                ),
                                            }
                                        }
                                        shared::WorktreeMode::None => shared::WorktreeMode::None,
                                    },
                                },
                                prompt: data.prompt.clone(),
                                max_runtime_minutes: data.max_runtime_minutes,
                                session_mode: data.session_mode,
                            },
                            hostname: host,
                        };
                        utils::send_json(
                            Request::post(&utils::api_url("/api/scheduled-tasks")),
                            &body,
                        )
                        .await
                    }
                    Some(FormMode::Edit(id)) => {
                        let body = UpdateScheduledTaskRequest {
                            name: Some(data.name.trim().to_string()),
                            cron_expression: Some(data.cron_expression.trim().to_string()),
                            timezone: Some(shared::timezone::canonicalize_timezone(&data.timezone)),
                            prompt: Some(data.prompt.clone()),
                            max_runtime_minutes: Some(data.max_runtime_minutes),
                            claude_args: Some(claude_args.clone()),
                            agent_type: Some(agent_type),
                            working_directory: Some(wd),
                            hostname: Some(host),
                            session_name: Some(data.session_name.trim().to_string()),
                            worktree: Some(match data.worktree {
                                shared::WorktreeMode::None => shared::WorktreeMode::None,
                                shared::WorktreeMode::Repo { .. } => shared::WorktreeMode::Repo {
                                    branch: utils::owned_non_blank(&data.worktree_branch),
                                },
                                shared::WorktreeMode::Scratch { .. } => {
                                    shared::WorktreeMode::Scratch {
                                        branch: utils::owned_non_blank(&data.worktree_branch),
                                    }
                                }
                            }),
                            session_mode: Some(data.session_mode),
                            ..Default::default()
                        };
                        utils::send_json(
                            Request::patch(&utils::api_url(&format!(
                                "/api/scheduled-tasks/{}",
                                id
                            ))),
                            &body,
                        )
                        .await
                    }
                    None => return,
                };

                match result {
                    Ok(resp) if resp.ok() => {
                        form_mode.set(None);
                        reload_tasks.emit(());
                    }
                    Ok(resp) => {
                        let status = resp.status();
                        let msg = utils::error_body(resp).await;
                        error_msg.set(Some(format!("Error ({}): {}", status, msg)));
                    }
                    Err(e) => {
                        error_msg.set(Some(format!("Request failed: {:?}", e)));
                    }
                }
            });
        })
    };

    let on_toggle_enabled = {
        let reload_tasks = reload_tasks.clone();
        let tasks = tasks.clone();
        Callback::from(move |task_id: Uuid| {
            let reload_tasks = reload_tasks.clone();
            let enabled = tasks
                .iter()
                .find(|t| t.id == task_id)
                .map(|t| t.enabled)
                .unwrap_or(true);
            spawn_local(async move {
                let body = UpdateScheduledTaskRequest {
                    enabled: Some(!enabled),
                    ..Default::default()
                };
                let _ = utils::send_json(
                    Request::patch(&utils::api_url(&format!(
                        "/api/scheduled-tasks/{}",
                        task_id
                    ))),
                    &body,
                )
                .await;
                reload_tasks.emit(());
            });
        })
    };

    let on_delete = {
        let confirm_delete = confirm_delete.clone();
        let reload_tasks = reload_tasks.clone();
        Callback::from(move |task_id: Uuid| {
            let reload_tasks = reload_tasks.clone();
            let confirm_delete = confirm_delete.clone();
            if *confirm_delete == Some(task_id) {
                // Second click — actually delete
                spawn_local(async move {
                    let _ = Request::delete(&utils::api_url(&format!(
                        "/api/scheduled-tasks/{}",
                        task_id
                    )))
                    .send()
                    .await;
                    confirm_delete.set(None);
                    reload_tasks.emit(());
                });
            } else {
                confirm_delete.set(Some(task_id));
            }
        })
    };

    // Form input handlers
    let set_field = |setter: fn(&mut TaskForm, String)| {
        let form = form.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut f = (*form).clone();
            setter(&mut f, input.value());
            form.set(f);
        })
    };

    let on_prompt_input = {
        let form = form.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlTextAreaElement = e.target_unchecked_into();
            let mut f = (*form).clone();
            f.prompt = input.value();
            form.set(f);
        })
    };

    let on_skip_permissions = {
        let form = form.clone();
        Callback::from(move |_: Event| {
            let mut f = (*form).clone();
            f.skip_permissions = !f.skip_permissions;
            form.set(f);
        })
    };

    let on_model_change = {
        let form = form.clone();
        Callback::from(move |value: String| {
            let mut f = (*form).clone();
            f.model_arg = value;
            form.set(f);
        })
    };

    let set_session_mode = |mode: shared::SessionMode| {
        let form = form.clone();
        Callback::from(move |_: MouseEvent| {
            let mut f = (*form).clone();
            f.session_mode = mode;
            form.set(f);
        })
    };

    let on_agent_type = {
        let form = form.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
            let mut f = (*form).clone();
            f.agent_type = shared::AgentType::parse_or_default(&input.value());
            f.model_arg.clear();
            f.skip_permissions = !matches!(
                f.agent_type,
                shared::AgentType::Muse | shared::AgentType::Antigravity
            );
            form.set(f);
        })
    };

    let on_launcher_change = {
        let launchers = launchers.clone();
        let selected_launcher = selected_launcher.clone();
        let form = form.clone();
        let dir = dir.clone();
        let agent_installs = agent_installs.clone();
        let probing_agents = probing_agents.clone();
        Callback::from(move |e: Event| {
            let select: web_sys::HtmlSelectElement = e.target_unchecked_into();
            let Ok(launcher_id) = select.value().parse::<Uuid>() else {
                return;
            };
            let Some(launcher) = launchers
                .iter()
                .find(|item| item.launcher_id == launcher_id)
            else {
                return;
            };
            selected_launcher.set(Some(launcher_id));
            let mut next = (*form).clone();
            next.hostname = launcher.hostname.clone();
            form.set(next);
            dir.fetch_initial(launcher_id, load_last_launch_dir_for(launcher_id));
            probe_agents_for(launcher_id, agent_installs.clone(), probing_agents.clone());
        })
    };

    let on_path_input = {
        let selected_launcher = selected_launcher.clone();
        let dir = dir.clone();
        let debounce_handle = debounce_handle.clone();
        Callback::from(move |e: InputEvent| {
            let input: HtmlInputElement = e.target_unchecked_into();
            let path = input.value();
            dir.path.set(path.clone());
            if let Some(launcher_id) = *selected_launcher {
                let dir = dir.clone();
                *debounce_handle.borrow_mut() = Some(Timeout::new(300, move || {
                    dir.fetch(launcher_id, path, false);
                }));
            }
        })
    };

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
                let directories: Vec<&DirectoryEntry> =
                    dir.entries.iter().filter(|entry| entry.is_dir).collect();
                if directories.len() == 1 {
                    e.prevent_default();
                    let base = if dir.path.ends_with('/') {
                        (*dir.path).clone()
                    } else {
                        parent_path(&dir.path)
                    };
                    navigate_to.emit(format!("{}{}/", base, directories[0].name));
                }
            }
        })
    };
    let on_worktree_mode = {
        let form = form.clone();
        Callback::from(move |e: Event| {
            let input: web_sys::HtmlSelectElement = e.target_unchecked_into();
            let mut f = (*form).clone();
            f.worktree = match input.value().as_str() {
                "repo" => shared::WorktreeMode::Repo { branch: None },
                "scratch" => shared::WorktreeMode::Scratch { branch: None },
                _ => shared::WorktreeMode::None,
            };
            form.set(f);
        })
    };

    let path = (*dir.path).clone();
    let breadcrumbs: Vec<(String, String)> = if let Some(home_root) = (*dir.home_root).as_deref() {
        let root = ensure_trailing_slash(home_root);
        let mut segments = vec![(root.clone(), "~".to_string())];
        let relative = path
            .strip_prefix(&root)
            .unwrap_or("")
            .trim_start_matches('/');
        if !relative.is_empty() {
            let mut built = root;
            for part in relative.split('/').filter(|part| !part.is_empty()) {
                built.push_str(part);
                built.push('/');
                segments.push((built.clone(), part.to_string()));
            }
        }
        segments
    } else {
        vec![("~".to_string(), "~".to_string())]
    };

    let directory_listing = if *dir.loading {
        html! { <div class="dir-loading">{ "Loading..." }</div> }
    } else if let Some(error) = &*dir.error {
        html! { <div class="dir-error-msg">{ error }</div> }
    } else if dir.entries.is_empty() {
        html! { <div class="dir-empty">{ "Choose a connected host to browse directories" }</div> }
    } else {
        let parent = clamp_to_home(parent_path(&dir.path), (*dir.home_root).as_deref());
        let on_up = {
            let navigate_to = navigate_to.clone();
            Callback::from(move |_: MouseEvent| navigate_to.emit(parent.clone()))
        };
        html! {
            <>
                { dir_entry(true, "..", Some(on_up)) }
                { for dir.entries.iter().map(|entry| {
                    let onclick = entry.is_dir.then(|| {
                        let base = if dir.path.ends_with('/') {
                            (*dir.path).clone()
                        } else {
                            parent_path(&dir.path)
                        };
                        let child = format!("{}{}/", base, entry.name);
                        let navigate_to = navigate_to.clone();
                        Callback::from(move |_: MouseEvent| navigate_to.emit(child.clone()))
                    });
                    dir_entry(entry.is_dir, &entry.name, onclick)
                }) }
            </>
        }
    };

    let can_save = !matches!(*form_mode, Some(FormMode::Create)) || can_schedule;
    let modal_close = if form_mode.is_some() {
        close_form.reform(|_| ())
    } else {
        props.on_close.clone()
    };
    let pane_class = if form_mode.is_some() {
        "sched-dialog sched-editor-dialog"
    } else {
        "sched-dialog"
    };

    html! {
        <FloatingPane
            overlay_class="sched-overlay"
            pane_class={pane_class}
            on_close={modal_close}
        >
            if let Some(mode) = &*form_mode {
                <div class="sched-header">
                    <div>
                        <h2 class="sched-title">
                            { if matches!(mode, FormMode::Create) { "New scheduled task" } else { "Edit scheduled task" } }
                        </h2>
                        <div class="sched-context">
                            <span class="sched-host">{ "Choose where and how this task runs." }</span>
                        </div>
                    </div>
                    <button class="sched-close" onclick={close_form.reform(|_: MouseEvent| ())}>{ "X" }</button>
                </div>
                <div class="sched-body sched-editor-body">
                    if let Some(err) = &*error_msg {
                        <div class="sched-error">{ err }</div>
                    }
                    <form class="sched-form" onsubmit={on_submit}>
                        <section class="sched-form-section">
                            <h3>{ "Run target" }</h3>
                            <div class="sched-field-row">
                                <LauncherTargetSelect
                                    launchers={(*launchers).clone()}
                                    selected={*selected_launcher}
                                    on_change={on_launcher_change}
                                    show_hostname={true}
                                    show_version={true}
                                    offline_hostname={utils::owned_non_blank(&form.hostname)}
                                />
                                <AgentTargetSelect
                                    selected={form.agent_type}
                                    installs={(*agent_installs).clone()}
                                    probing={*probing_agents}
                                    on_change={on_agent_type}
                                />
                            </div>
                            if !can_schedule && matches!(mode, FormMode::Create) {
                                <div class="sched-version-warning sched-version-warning-inline">
                                    { format!("Choose a connected host running launcher v{} or newer.", MIN_LAUNCHER_VERSION) }
                                </div>
                            }
                            <DirectoryTargetSelect
                                path={(*dir.path).clone()}
                                {breadcrumbs}
                                listing={directory_listing}
                                on_path_input={on_path_input}
                                {on_path_keydown}
                                on_navigate={navigate_to.clone()}
                                browser_class={classes!("sched-dir-browser")}
                            />
                        </section>

                        <section class="sched-form-section">
                            <h3>{ "Task" }</h3>
                            <div class="sched-field-row">
                                <div class="sched-field">
                                    <label>{ "Name" }</label>
                                    <input type="text" placeholder="Nightly code review" value={form.name.clone()} oninput={set_field(|f, v| f.name = v)} required=true />
                                </div>
                                <div class="sched-field">
                                    <label>{ "Session name (optional)" }</label>
                                    <input type="text" placeholder="Defaults to the task name" value={form.session_name.clone()} oninput={set_field(|f, v| f.session_name = v)} />
                                </div>
                            </div>
                            <div class="sched-field-row">
                                <div class="sched-field">
                                    <label>{ "Cron schedule" }</label>
                                    <input type="text" placeholder="0 3 * * *" value={form.cron_expression.clone()} oninput={set_field(|f, v| f.cron_expression = v)} required=true />
                                    <span class="sched-hint">{ "minute hour day-of-month month day-of-week" }</span>
                                    if let Some(description) = cron_describe::describe(&form.cron_expression) {
                                        <span class="sched-cron-desc">{ description }</span>
                                    }
                                </div>
                                <div class="sched-field sched-field-sm">
                                    <label>{ "Timezone" }</label>
                                    <input type="text" list="sched-tz-list" placeholder="America/Los_Angeles" value={form.timezone.clone()} oninput={set_field(|f, v| f.timezone = v)} />
                                    <datalist id="sched-tz-list">
                                        { for shared::timezone::COMMON_IANA_ZONES.iter().map(|timezone| html! { <option value={*timezone} /> }) }
                                    </datalist>
                                </div>
                                <div class="sched-field sched-field-sm">
                                    <label>{ "Timeout (min)" }</label>
                                    <input type="number" min="1" max="1440" value={form.max_runtime_minutes.to_string()} oninput={set_field(|f, v| f.max_runtime_minutes = v.parse().unwrap_or(30))} />
                                </div>
                            </div>
                            <div class="sched-field">
                                <label>{ "Prompt" }</label>
                                <textarea rows="5" placeholder="What should the agent do?" value={form.prompt.clone()} oninput={on_prompt_input} required=true />
                            </div>
                        </section>

                        <section class="sched-form-section sched-form-section-compact">
                            <h3>{ "Session options" }</h3>
                            <div class="sched-field-row">
                                <div class="sched-field">
                                    <label>{ "Model" }</label>
                                    <ModelSelect agent_type={form.agent_type} value={form.model_arg.clone()} on_change={on_model_change} class="launcher-select" />
                                </div>
                                <div class="sched-field">
                                    <label>{ "Worktree" }</label>
                                    <select class="launcher-select" onchange={on_worktree_mode} value={match &form.worktree { shared::WorktreeMode::None => "none", shared::WorktreeMode::Repo { .. } => "repo", shared::WorktreeMode::Scratch { .. } => "scratch" }}>
                                        <option value="none">{ "Use working directory" }</option>
                                        <option value="repo">{ "Repository worktree" }</option>
                                        <option value="scratch">{ "Automatic scratch worktree" }</option>
                                    </select>
                                </div>
                                if !form.worktree.is_none() {
                                    <div class="sched-field">
                                        <label>{ "Branch (optional)" }</label>
                                        <input type="text" placeholder="sched-task-id" value={form.worktree_branch.clone()} oninput={set_field(|f, v| f.worktree_branch = v)} />
                                    </div>
                                }
                            </div>
                            <div class="sched-field">
                                <label>{ "Each run" }</label>
                                <div class="sched-mode-toggle">
                                    <button type="button" class={classes!("sched-btn", (form.session_mode == shared::SessionMode::Fresh).then_some("sched-btn-primary"))} onclick={set_session_mode(shared::SessionMode::Fresh)}>{ "Fresh session" }</button>
                                    <button type="button" class={classes!("sched-btn", (form.session_mode == shared::SessionMode::Continue).then_some("sched-btn-primary"))} onclick={set_session_mode(shared::SessionMode::Continue)}>{ "Continue previous" }</button>
                                </div>
                                <span class="sched-hint">{ "Continue resumes the same conversation and accumulates context across runs." }</span>
                            </div>
                            <div class="sched-field">
                                <label>{ "Extra CLI arguments (optional)" }</label>
                                <input type="text" placeholder="--verbose" value={form.extra_args.clone()} oninput={set_field(|f, v| f.extra_args = v)} />
                            </div>
                            if !skip_permissions_args(form.agent_type).is_empty() {
                                <div class="sched-field sched-checkbox">
                                    <label>
                                        <input type="checkbox" checked={form.skip_permissions} onchange={on_skip_permissions} />
                                        { format!(" {}", skip_permissions_label(form.agent_type)) }
                                    </label>
                                </div>
                            }
                        </section>

                        <div class="sched-form-actions sched-editor-actions">
                            <button type="button" class="sched-btn" onclick={close_form.reform(|_: MouseEvent| ())}>{ "Back" }</button>
                            <button type="submit" class="sched-btn sched-btn-primary" disabled={!can_save}>
                                { if matches!(mode, FormMode::Create) { "Create scheduled task" } else { "Save changes" } }
                            </button>
                        </div>
                    </form>
                </div>
            } else {
                <div class="sched-header">
                    <div>
                        <h2 class="sched-title">{ if folder.is_empty() { "Scheduled Tasks".to_string() } else { format!("Schedule — {folder}") } }</h2>
                        <div class="sched-context">
                            if !hostname.is_empty() { <span class="sched-host">{ &hostname }</span> }
                            if !working_directory.is_empty() { <code class="sched-dir">{ &working_directory }</code> }
                        </div>
                    </div>
                    <button class="sched-close" onclick={props.on_close.reform(|_| ())}>{ "X" }</button>
                </div>
                if *loading {
                    <div class="sched-loading"><div class="spinner"></div></div>
                } else {
                    <div class="sched-body">
                        if tasks.is_empty() { <p class="sched-empty">{ "No scheduled tasks." }</p> }
                        { for tasks.iter().map(|task| {
                            let task_id = task.id;
                            let on_edit = open_edit.clone();
                            let on_toggle = on_toggle_enabled.clone();
                            let on_delete = on_delete.clone();
                            let is_confirming = *confirm_delete == Some(task_id);
                            html! {
                                <div class={classes!("sched-task-row", (!task.enabled).then_some("disabled"))}>
                                    <div class="sched-task-info">
                                        <span class="sched-task-name">{ &task.fields.name }</span>
                                        <code class="sched-task-cron">{ &task.fields.cron_expression }</code>
                                        if task.fields.timezone != "UTC" { <span class="sched-task-tz">{ &task.fields.timezone }</span> }
                                        if task.fields.session_mode == shared::SessionMode::Continue { <span class="sched-task-tz">{ "continue" }</span> }
                                    </div>
                                    <div class="sched-task-prompt-preview">{ &task.fields.prompt }</div>
                                    <div class="sched-task-actions">
                                        <button class="sched-btn" onclick={Callback::from(move |_| on_edit.emit(task_id))}>{ "Edit" }</button>
                                        <button class="sched-btn" onclick={Callback::from(move |_| on_toggle.emit(task_id))}>{ if task.enabled { "Disable" } else { "Enable" } }</button>
                                        <button class={classes!("sched-btn", "sched-btn-danger", is_confirming.then_some("confirming"))} onclick={Callback::from(move |_| on_delete.emit(task_id))}>
                                            { if is_confirming { "Confirm?" } else { "Delete" } }
                                        </button>
                                    </div>
                                </div>
                            }
                        }) }
                        <button class="sched-btn sched-btn-primary sched-new-btn" onclick={open_create} disabled={launchers.is_empty()}>
                            { "+ New scheduled task" }
                        </button>
                        if launchers.is_empty() {
                            <p class="sched-hint">{ "Connect a launcher before creating a scheduled task." }</p>
                        }
                    </div>
                }
            }
        </FloatingPane>
    }
}

#[cfg(test)]
mod tests {
    use super::version_sufficient;

    #[test]
    fn schedule_host_version_gate_matches_launcher_capability_floor() {
        assert!(!version_sufficient("2.1.1"));
        assert!(version_sufficient("2.1.2"));
        assert!(version_sufficient("2.15.16"));
        assert!(!version_sufficient("unknown"));
    }
}
