use crate::utils;
use gloo_net::http::Request;
use shared::api::{
    CreateScheduledTaskRequest, ScheduledTaskInfo, ScheduledTaskListResponse,
    UpdateScheduledTaskRequest,
};
use shared::LauncherInfo;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Fetch scheduled tasks from API
async fn fetch_tasks() -> Option<Vec<ScheduledTaskInfo>> {
    let url = utils::api_url("/api/scheduled-tasks");
    match Request::get(&url).send().await {
        Ok(response) => {
            if response.status() == 401 {
                if let Some(window) = web_sys::window() {
                    let _ = window.location().set_href("/api/auth/logout");
                }
                return None;
            }
            response
                .json::<ScheduledTaskListResponse>()
                .await
                .ok()
                .map(|r| r.tasks)
        }
        Err(e) => {
            log::error!("Failed to fetch scheduled tasks: {:?}", e);
            None
        }
    }
}

/// Fetch connected launchers for hostname dropdown
async fn fetch_launchers() -> Vec<LauncherInfo> {
    match Request::get("/api/launchers").send().await {
        Ok(resp) => resp.json::<Vec<LauncherInfo>>().await.unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

#[derive(Clone, Default)]
struct TaskForm {
    name: String,
    cron_expression: String,
    timezone: String,
    hostname: String,
    working_directory: String,
    prompt: String,
    max_runtime_minutes: i32,
}

impl TaskForm {
    fn from_task(task: &ScheduledTaskInfo) -> Self {
        Self {
            name: task.name.clone(),
            cron_expression: task.cron_expression.clone(),
            timezone: task.timezone.clone(),
            hostname: task.hostname.clone(),
            working_directory: task.working_directory.clone(),
            prompt: task.prompt.clone(),
            max_runtime_minutes: task.max_runtime_minutes,
        }
    }
}

#[derive(Clone, PartialEq)]
enum FormMode {
    Create,
    Edit(Uuid),
}

#[derive(Properties, PartialEq)]
pub struct TasksPanelProps {
    pub on_tasks_loaded: Callback<Vec<ScheduledTaskInfo>>,
}

#[function_component(TasksPanel)]
pub fn tasks_panel(props: &TasksPanelProps) -> Html {
    let tasks = use_state(Vec::<ScheduledTaskInfo>::new);
    let loading = use_state(|| true);
    let form_mode = use_state(|| None::<FormMode>);
    let form = use_state(TaskForm::default);
    let launchers = use_state(Vec::<LauncherInfo>::new);
    let confirm_action = use_state(|| None::<(String, Callback<MouseEvent>)>);
    let error_msg = use_state(|| None::<String>);

    let reload_tasks = {
        let tasks = tasks.clone();
        let loading = loading.clone();
        let on_tasks_loaded = props.on_tasks_loaded.clone();
        Callback::from(move |_| {
            let tasks = tasks.clone();
            let loading = loading.clone();
            let on_tasks_loaded = on_tasks_loaded.clone();
            spawn_local(async move {
                if let Some(list) = fetch_tasks().await {
                    on_tasks_loaded.emit(list.clone());
                    tasks.set(list);
                }
                loading.set(false);
            });
        })
    };

    // Initial fetch
    {
        let reload_tasks = reload_tasks.clone();
        let launchers = launchers.clone();
        use_effect_with((), move |_| {
            reload_tasks.emit(());
            spawn_local(async move {
                launchers.set(fetch_launchers().await);
            });
            || ()
        });
    }

    let open_create = {
        let form_mode = form_mode.clone();
        let form = form.clone();
        let error_msg = error_msg.clone();
        let launchers = launchers.clone();
        Callback::from(move |_| {
            let new_form = TaskForm {
                timezone: "UTC".to_string(),
                max_runtime_minutes: 30,
                hostname: launchers
                    .first()
                    .map(|l| l.hostname.clone())
                    .unwrap_or_default(),
                ..Default::default()
            };
            form.set(new_form);
            error_msg.set(None);
            form_mode.set(Some(FormMode::Create));
        })
    };

    let open_edit = {
        let form_mode = form_mode.clone();
        let form = form.clone();
        let tasks = tasks.clone();
        let error_msg = error_msg.clone();
        Callback::from(move |task_id: Uuid| {
            if let Some(task) = tasks.iter().find(|t| t.id == task_id) {
                form.set(TaskForm::from_task(task));
                error_msg.set(None);
                form_mode.set(Some(FormMode::Edit(task_id)));
            }
        })
    };

    let close_form = {
        let form_mode = form_mode.clone();
        let error_msg = error_msg.clone();
        Callback::from(move |_| {
            form_mode.set(None);
            error_msg.set(None);
        })
    };

    let on_submit = {
        let form = form.clone();
        let form_mode = form_mode.clone();
        let reload_tasks = reload_tasks.clone();
        let error_msg = error_msg.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let data = (*form).clone();
            let mode = (*form_mode).clone();
            let reload_tasks = reload_tasks.clone();
            let form_mode = form_mode.clone();
            let error_msg = error_msg.clone();

            if data.name.trim().is_empty() || data.cron_expression.trim().is_empty() {
                return;
            }

            spawn_local(async move {
                let result = match mode {
                    Some(FormMode::Create) => {
                        let body = CreateScheduledTaskRequest {
                            name: data.name.trim().to_string(),
                            cron_expression: data.cron_expression.trim().to_string(),
                            timezone: data.timezone.clone(),
                            hostname: data.hostname.clone(),
                            working_directory: data.working_directory.clone(),
                            prompt: data.prompt.clone(),
                            claude_args: vec![],
                            agent_type: shared::AgentType::Claude,
                            max_runtime_minutes: data.max_runtime_minutes,
                        };
                        Request::post(&utils::api_url("/api/scheduled-tasks"))
                            .json(&body)
                            .unwrap()
                            .send()
                            .await
                    }
                    Some(FormMode::Edit(id)) => {
                        let body = UpdateScheduledTaskRequest {
                            name: Some(data.name.trim().to_string()),
                            cron_expression: Some(data.cron_expression.trim().to_string()),
                            timezone: Some(data.timezone.clone()),
                            hostname: Some(data.hostname.clone()),
                            working_directory: Some(data.working_directory.clone()),
                            prompt: Some(data.prompt.clone()),
                            max_runtime_minutes: Some(data.max_runtime_minutes),
                            ..Default::default()
                        };
                        Request::patch(&utils::api_url(&format!("/api/scheduled-tasks/{}", id)))
                            .json(&body)
                            .unwrap()
                            .send()
                            .await
                    }
                    None => return,
                };

                match result {
                    Ok(resp) if resp.status() >= 200 && resp.status() < 300 => {
                        form_mode.set(None);
                        reload_tasks.emit(());
                    }
                    Ok(resp) => {
                        let msg = resp.text().await.unwrap_or_default();
                        error_msg.set(Some(format!("Error ({}): {}", resp.status(), msg)));
                    }
                    Err(e) => {
                        error_msg.set(Some(format!("Request failed: {:?}", e)));
                    }
                }
            });
        })
    };

    let on_toggle_enabled = {
        let tasks = tasks.clone();
        let reload_tasks = reload_tasks.clone();
        Callback::from(move |task_id: Uuid| {
            let tasks = tasks.clone();
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
                let _ = Request::patch(&utils::api_url(&format!(
                    "/api/scheduled-tasks/{}",
                    task_id
                )))
                .json(&body)
                .unwrap()
                .send()
                .await;
                reload_tasks.emit(());
            });
        })
    };

    let on_delete = {
        let confirm_action = confirm_action.clone();
        let reload_tasks = reload_tasks.clone();
        Callback::from(move |task_id: Uuid| {
            let confirm_action_inner = confirm_action.clone();
            let reload_tasks = reload_tasks.clone();

            let action = Callback::from(move |_: MouseEvent| {
                let confirm_action_inner = confirm_action_inner.clone();
                let reload_tasks = reload_tasks.clone();
                spawn_local(async move {
                    let _ = Request::delete(&utils::api_url(&format!(
                        "/api/scheduled-tasks/{}",
                        task_id
                    )))
                    .send()
                    .await;
                    confirm_action_inner.set(None);
                    reload_tasks.emit(());
                });
            });

            confirm_action.set(Some((
                "Delete this scheduled task? This cannot be undone.".to_string(),
                action,
            )));
        })
    };

    let cancel_confirm = {
        let confirm_action = confirm_action.clone();
        Callback::from(move |_| {
            confirm_action.set(None);
        })
    };

    // Form input handlers
    let set_field = |field: &'static str| {
        let form = form.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut f = (*form).clone();
            match field {
                "name" => f.name = input.value(),
                "cron_expression" => f.cron_expression = input.value(),
                "timezone" => f.timezone = input.value(),
                "hostname" => f.hostname = input.value(),
                "working_directory" => f.working_directory = input.value(),
                "max_runtime_minutes" => {
                    f.max_runtime_minutes = input.value().parse().unwrap_or(30)
                }
                _ => {}
            }
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

    let on_hostname_select = {
        let form = form.clone();
        Callback::from(move |e: Event| {
            let select: web_sys::HtmlSelectElement = e.target_unchecked_into();
            let mut f = (*form).clone();
            f.hostname = select.value();
            form.set(f);
        })
    };

    // Collect unique hostnames from launchers
    let hostnames: Vec<String> = {
        let mut names: Vec<String> = launchers.iter().map(|l| l.hostname.clone()).collect();
        names.sort();
        names.dedup();
        names
    };

    html! {
        <>
            <section class="tasks-section">
                <div class="section-header">
                    <h2>{ "Scheduled Tasks" }</h2>
                    <p class="section-description">
                        { "Configure recurring agent tasks that run on a cron schedule." }
                    </p>
                    if form_mode.is_some() {
                        <button class="create-button" onclick={close_form.clone()}>
                            { "Cancel" }
                        </button>
                    } else {
                        <button class="create-button" onclick={open_create}>
                            { "+ New Task" }
                        </button>
                    }
                </div>

                if let Some(mode) = &*form_mode {
                    <div class="create-token-form">
                        <h3 class="task-form-title">
                            { if matches!(mode, FormMode::Create) { "Create Task" } else { "Edit Task" } }
                        </h3>
                        if let Some(err) = &*error_msg {
                            <div class="task-form-error">{ err }</div>
                        }
                        <form class="task-form" onsubmit={on_submit}>
                            <div class="form-group">
                                <label for="task-name">{ "Name" }</label>
                                <input
                                    type="text"
                                    id="task-name"
                                    placeholder="e.g., Nightly Code Review"
                                    value={form.name.clone()}
                                    oninput={set_field("name")}
                                    required=true
                                />
                            </div>
                            <div class="task-form-row">
                                <div class="form-group">
                                    <label for="task-cron">{ "Cron Expression" }</label>
                                    <input
                                        type="text"
                                        id="task-cron"
                                        placeholder="0 3 * * *"
                                        value={form.cron_expression.clone()}
                                        oninput={set_field("cron_expression")}
                                        required=true
                                    />
                                    <span class="form-hint">{ "min hour dom month dow" }</span>
                                </div>
                                <div class="form-group">
                                    <label for="task-tz">{ "Timezone" }</label>
                                    <input
                                        type="text"
                                        id="task-tz"
                                        placeholder="UTC"
                                        value={form.timezone.clone()}
                                        oninput={set_field("timezone")}
                                    />
                                </div>
                                <div class="form-group">
                                    <label for="task-runtime">{ "Max Runtime (min)" }</label>
                                    <input
                                        type="number"
                                        id="task-runtime"
                                        min="1"
                                        max="1440"
                                        value={form.max_runtime_minutes.to_string()}
                                        oninput={set_field("max_runtime_minutes")}
                                    />
                                </div>
                            </div>
                            <div class="form-group">
                                <label for="task-hostname">{ "Launcher Host" }</label>
                                if hostnames.is_empty() {
                                    <input
                                        type="text"
                                        id="task-hostname"
                                        placeholder="hostname"
                                        value={form.hostname.clone()}
                                        oninput={set_field("hostname")}
                                        required=true
                                    />
                                } else {
                                    <select id="task-hostname" onchange={on_hostname_select}>
                                        { for hostnames.iter().map(|h| {
                                            html! {
                                                <option
                                                    value={h.clone()}
                                                    selected={*h == form.hostname}
                                                >{ h }</option>
                                            }
                                        }) }
                                    </select>
                                }
                            </div>
                            <div class="form-group">
                                <label for="task-dir">{ "Working Directory" }</label>
                                <input
                                    type="text"
                                    id="task-dir"
                                    placeholder="/home/user/project"
                                    value={form.working_directory.clone()}
                                    oninput={set_field("working_directory")}
                                    required=true
                                />
                            </div>
                            <div class="form-group">
                                <label for="task-prompt">{ "Prompt" }</label>
                                <textarea
                                    id="task-prompt"
                                    rows="4"
                                    placeholder="What should the agent do each run?"
                                    value={form.prompt.clone()}
                                    oninput={on_prompt_input}
                                    required=true
                                />
                            </div>
                            <button type="submit" class="submit-button">
                                { if matches!(mode, FormMode::Create) { "Create Task" } else { "Save Changes" } }
                            </button>
                        </form>
                    </div>
                }

                if *loading {
                    <div class="loading">
                        <div class="spinner"></div>
                        <p>{ "Loading tasks..." }</p>
                    </div>
                } else if tasks.is_empty() {
                    <div class="empty-state">
                        <p>{ "No scheduled tasks. Create one to run agents on a cron schedule." }</p>
                    </div>
                } else {
                    <div class="task-cards">
                        { for tasks.iter().map(|task| {
                            let task_id = task.id;
                            let on_edit = open_edit.clone();
                            let on_toggle = on_toggle_enabled.clone();
                            let on_del = on_delete.clone();
                            html! {
                                <TaskCard
                                    key={task.id.to_string()}
                                    task={task.clone()}
                                    on_edit={Callback::from(move |_| on_edit.emit(task_id))}
                                    on_toggle={Callback::from(move |_| on_toggle.emit(task_id))}
                                    on_delete={Callback::from(move |_| on_del.emit(task_id))}
                                />
                            }
                        }) }
                    </div>
                }
            </section>

            if let Some((message, action)) = &*confirm_action {
                <div class="modal-overlay" onclick={cancel_confirm.clone()}>
                    <div class="confirm-modal" onclick={Callback::from(|e: MouseEvent| e.stop_propagation())}>
                        <p>{ message }</p>
                        <div class="confirm-actions">
                            <button class="cancel-button" onclick={cancel_confirm.clone()}>
                                { "Cancel" }
                            </button>
                            <button class="confirm-button" onclick={action.clone()}>
                                { "Delete" }
                            </button>
                        </div>
                    </div>
                </div>
            }
        </>
    }
}

#[derive(Properties, PartialEq)]
struct TaskCardProps {
    task: ScheduledTaskInfo,
    on_edit: Callback<()>,
    on_toggle: Callback<()>,
    on_delete: Callback<()>,
}

#[function_component(TaskCard)]
fn task_card(props: &TaskCardProps) -> Html {
    let task = &props.task;

    let status_class = if task.enabled {
        "task-status active"
    } else {
        "task-status disabled"
    };

    html! {
        <div class={classes!("task-card", (!task.enabled).then_some("task-card-disabled"))}>
            <div class="task-card-header">
                <div class="task-card-title-row">
                    <span class="task-card-name">{ &task.name }</span>
                    <span class={status_class}>
                        { if task.enabled { "Active" } else { "Disabled" } }
                    </span>
                </div>
                <div class="task-card-meta">
                    <code class="task-cron-badge">{ &task.cron_expression }</code>
                    <span class="task-tz">{ &task.timezone }</span>
                </div>
            </div>
            <div class="task-card-body">
                <div class="task-card-detail">
                    <span class="task-detail-label">{ "Host" }</span>
                    <span class="task-detail-value">{ &task.hostname }</span>
                </div>
                <div class="task-card-detail">
                    <span class="task-detail-label">{ "Directory" }</span>
                    <span class="task-detail-value task-dir-value">{ &task.working_directory }</span>
                </div>
                <div class="task-card-detail">
                    <span class="task-detail-label">{ "Prompt" }</span>
                    <span class="task-detail-value task-prompt-value">{ &task.prompt }</span>
                </div>
                if let Some(last_run) = &task.last_run_at {
                    <div class="task-card-detail">
                        <span class="task-detail-label">{ "Last Run" }</span>
                        <span class="task-detail-value">{ utils::format_timestamp(last_run) }</span>
                    </div>
                }
            </div>
            <div class="task-card-actions">
                <button class="task-action-btn edit-btn" onclick={props.on_edit.reform(|_| ())}>
                    { "Edit" }
                </button>
                <button
                    class={classes!("task-action-btn", if task.enabled { "disable-btn" } else { "enable-btn" })}
                    onclick={props.on_toggle.reform(|_| ())}
                >
                    { if task.enabled { "Disable" } else { "Enable" } }
                </button>
                <button class="task-action-btn delete-btn" onclick={props.on_delete.reform(|_| ())}>
                    { "Delete" }
                </button>
            </div>
        </div>
    }
}
