use crate::utils;
use gloo_net::http::Request;
use shared::api::{
    CreateScheduledTaskRequest, ScheduledTaskInfo, ScheduledTaskListResponse,
    UpdateScheduledTaskRequest,
};
use shared::{LauncherInfo, SessionInfo};
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
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

#[derive(Properties, PartialEq)]
pub struct ScheduleDialogProps {
    pub session: SessionInfo,
    pub on_close: Callback<()>,
}

#[derive(Clone, Default)]
struct TaskForm {
    name: String,
    cron_expression: String,
    timezone: String,
    prompt: String,
    max_runtime_minutes: i32,
}

#[derive(Clone, PartialEq)]
enum FormMode {
    Create,
    Edit(Uuid),
}

/// Describe a 5-field cron expression in plain English.
/// Returns None if the expression is invalid / unparseable.
fn describe_cron(expr: &str) -> Option<String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return None;
    }
    let [min, hour, dom, month, dow] = [parts[0], parts[1], parts[2], parts[3], parts[4]];

    let time_desc = match (min, hour) {
        ("*", "*") => "Every minute".to_string(),
        (m, "*") => {
            let m_val: u32 = m.parse().ok()?;
            if m_val > 59 {
                return None;
            }
            format!("Every hour at :{:02}", m_val)
        }
        ("*", h) => {
            // */N pattern on hour
            if let Some(step) = h.strip_prefix("*/") {
                let n: u32 = step.parse().ok()?;
                if n == 0 || n > 23 {
                    return None;
                }
                return Some(format!("Every {} hours", n));
            }
            let _: u32 = h.parse().ok()?;
            format!("Every minute of hour {}", h)
        }
        ("0", h) => {
            let h_val: u32 = h.parse().ok()?;
            if h_val > 23 {
                return None;
            }
            let (display_h, ampm) = if h_val == 0 {
                (12, "AM")
            } else if h_val < 12 {
                (h_val, "AM")
            } else if h_val == 12 {
                (12, "PM")
            } else {
                (h_val - 12, "PM")
            };
            format!("At {}:00 {}", display_h, ampm)
        }
        (m, h) => {
            // Check for */N patterns
            if m.starts_with("*/") || h.starts_with("*/") {
                return None; // Complex, skip
            }
            let h_val: u32 = h.parse().ok()?;
            let m_val: u32 = m.parse().ok()?;
            if h_val > 23 || m_val > 59 {
                return None;
            }
            let (display_h, ampm) = if h_val == 0 {
                (12, "AM")
            } else if h_val < 12 {
                (h_val, "AM")
            } else if h_val == 12 {
                (12, "PM")
            } else {
                (h_val - 12, "PM")
            };
            format!("At {}:{:02} {}", display_h, m_val, ampm)
        }
    };

    let dow_names = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    let month_names = [
        "", "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];

    let day_desc = match (dom, dow) {
        ("*", "*") => String::new(),
        (d, "*") => {
            if let Some(step) = d.strip_prefix("*/") {
                let n: u32 = step.parse().ok()?;
                format!(", every {} days", n)
            } else {
                let d_val: u32 = d.parse().ok()?;
                if d_val == 0 || d_val > 31 {
                    return None;
                }
                let suffix = match d_val {
                    1 | 21 | 31 => "st",
                    2 | 22 => "nd",
                    3 | 23 => "rd",
                    _ => "th",
                };
                format!(", on the {}{}", d_val, suffix)
            }
        }
        ("*", w) => {
            // Could be a list like "1,3,5" or a single value
            let names: Vec<&str> = w
                .split(',')
                .filter_map(|p| {
                    let n: usize = p.parse().ok()?;
                    dow_names.get(n).copied()
                })
                .collect();
            if names.is_empty() {
                return None;
            }
            format!(", on {}", names.join(", "))
        }
        _ => String::new(), // Both specified — complex, skip extra detail
    };

    let month_desc = if month == "*" {
        String::new()
    } else if let Some(step) = month.strip_prefix("*/") {
        let n: u32 = step.parse().ok()?;
        format!(", every {} months", n)
    } else {
        let names: Vec<&str> = month
            .split(',')
            .filter_map(|p| {
                let n: usize = p.parse().ok()?;
                month_names.get(n).copied()
            })
            .collect();
        if names.is_empty() {
            return None;
        }
        format!(", in {}", names.join(", "))
    };

    Some(format!("{}{}{}", time_desc, day_desc, month_desc))
}

#[function_component(ScheduleDialog)]
pub fn schedule_dialog(props: &ScheduleDialogProps) -> Html {
    let tasks = use_state(Vec::<ScheduledTaskInfo>::new);
    let loading = use_state(|| true);
    let form_mode = use_state(|| None::<FormMode>);
    let form = use_state(TaskForm::default);
    let error_msg = use_state(|| None::<String>);
    let confirm_delete = use_state(|| None::<Uuid>);
    let launcher_version = use_state(String::new);

    let working_directory = props.session.working_directory.clone();
    let hostname = props.session.hostname.clone();

    let folder = utils::extract_folder(&working_directory);

    // Fetch launcher version for this session's hostname
    {
        let launcher_version = launcher_version.clone();
        let hostname = hostname.clone();
        use_effect_with(hostname.clone(), move |_| {
            spawn_local(async move {
                let url = utils::api_url("/api/launchers");
                if let Ok(resp) = Request::get(&url).send().await {
                    if let Ok(launchers) = resp.json::<Vec<LauncherInfo>>().await {
                        if let Some(l) = launchers.iter().find(|l| l.hostname == hostname) {
                            launcher_version.set(l.version.clone());
                        }
                    }
                }
            });
            || ()
        });
    }

    let can_schedule = version_sufficient(&launcher_version);

    let reload_tasks = {
        let tasks = tasks.clone();
        let loading = loading.clone();
        let wd = working_directory.clone();
        Callback::from(move |_| {
            let tasks = tasks.clone();
            let loading = loading.clone();
            let wd = wd.clone();
            spawn_local(async move {
                let url = utils::api_url("/api/scheduled-tasks");
                if let Ok(resp) = Request::get(&url).send().await {
                    if let Ok(data) = resp.json::<ScheduledTaskListResponse>().await {
                        // Filter to tasks matching this working directory
                        let filtered: Vec<_> = data
                            .tasks
                            .into_iter()
                            .filter(|t| t.working_directory == wd)
                            .collect();
                        tasks.set(filtered);
                    }
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
        Callback::from(move |_| {
            form.set(TaskForm {
                timezone: "UTC".to_string(),
                max_runtime_minutes: 30,
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
        Callback::from(move |task_id: Uuid| {
            if let Some(task) = tasks.iter().find(|t| t.id == task_id) {
                form.set(TaskForm {
                    name: task.name.clone(),
                    cron_expression: task.cron_expression.clone(),
                    timezone: task.timezone.clone(),
                    prompt: task.prompt.clone(),
                    max_runtime_minutes: task.max_runtime_minutes,
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
        let wd = working_directory.clone();
        let host = hostname.clone();
        Callback::from(move |e: SubmitEvent| {
            e.prevent_default();
            let data = (*form).clone();
            let mode = (*form_mode).clone();
            let reload_tasks = reload_tasks.clone();
            let form_mode = form_mode.clone();
            let error_msg = error_msg.clone();
            let wd = wd.clone();
            let host = host.clone();

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
                            hostname: host,
                            working_directory: wd,
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
    let set_field = |field: &'static str| {
        let form = form.clone();
        Callback::from(move |e: InputEvent| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            let mut f = (*form).clone();
            match field {
                "name" => f.name = input.value(),
                "cron_expression" => f.cron_expression = input.value(),
                "timezone" => f.timezone = input.value(),
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

    let on_overlay_click = props.on_close.reform(|_| ());
    let on_dialog_click = Callback::from(|e: MouseEvent| e.stop_propagation());

    html! {
        <div class="sched-overlay" onclick={on_overlay_click}>
            <div class="sched-dialog" onclick={on_dialog_click}>
                <div class="sched-header">
                    <div>
                        <h2 class="sched-title">{ format!("Schedule — {}", folder) }</h2>
                        <div class="sched-context">
                            <span class="sched-host">{ &hostname }</span>
                            <code class="sched-dir">{ &working_directory }</code>
                        </div>
                    </div>
                    <button class="sched-close" onclick={props.on_close.reform(|_| ())}>
                        { "X" }
                    </button>
                </div>

                if !can_schedule {
                    <div class="sched-version-warning">
                        { format!("Requires launcher v{}+. ", MIN_LAUNCHER_VERSION) }
                        if launcher_version.is_empty() {
                            { "No launcher version detected." }
                        } else {
                            { format!("Current: v{}.", *launcher_version) }
                        }
                        { " Update your launcher to enable scheduled tasks." }
                    </div>
                }

                if *loading {
                    <div class="sched-loading">
                        <div class="spinner"></div>
                    </div>
                } else {
                    <div class="sched-body">
                        // Existing tasks list
                        if tasks.is_empty() && form_mode.is_none() {
                            <p class="sched-empty">{ "No scheduled tasks for this session." }</p>
                        }
                        { for tasks.iter().map(|task| {
                            let task_id = task.id;
                            let on_edit = open_edit.clone();
                            let on_toggle = on_toggle_enabled.clone();
                            let on_del = on_delete.clone();
                            let is_confirming = *confirm_delete == Some(task_id);
                            html! {
                                <div class={classes!("sched-task-row", (!task.enabled).then_some("disabled"))}>
                                    <div class="sched-task-info">
                                        <span class="sched-task-name">{ &task.name }</span>
                                        <code class="sched-task-cron">{ &task.cron_expression }</code>
                                        if task.timezone != "UTC" {
                                            <span class="sched-task-tz">{ &task.timezone }</span>
                                        }
                                    </div>
                                    <div class="sched-task-prompt-preview">{ &task.prompt }</div>
                                    <div class="sched-task-actions">
                                        <button class="sched-btn" onclick={Callback::from(move |_| on_edit.emit(task_id))}>
                                            { "Edit" }
                                        </button>
                                        <button class="sched-btn" onclick={Callback::from(move |_| on_toggle.emit(task_id))}>
                                            { if task.enabled { "Disable" } else { "Enable" } }
                                        </button>
                                        <button
                                            class={classes!("sched-btn", "sched-btn-danger", is_confirming.then_some("confirming"))}
                                            onclick={Callback::from(move |_| on_del.emit(task_id))}
                                        >
                                            { if is_confirming { "Confirm?" } else { "Delete" } }
                                        </button>
                                    </div>
                                </div>
                            }
                        }) }

                        // Form
                        if let Some(mode) = &*form_mode {
                            <div class="sched-form-container">
                                <h3 class="sched-form-title">
                                    { if matches!(mode, FormMode::Create) { "New Task" } else { "Edit Task" } }
                                </h3>
                                if let Some(err) = &*error_msg {
                                    <div class="sched-error">{ err }</div>
                                }
                                <form class="sched-form" onsubmit={on_submit}>
                                    <div class="sched-field">
                                        <label>{ "Name" }</label>
                                        <input
                                            type="text"
                                            placeholder="Nightly Code Review"
                                            value={form.name.clone()}
                                            oninput={set_field("name")}
                                            required=true
                                        />
                                    </div>
                                    <div class="sched-field-row">
                                        <div class="sched-field">
                                            <label>{ "Cron" }</label>
                                            <input
                                                type="text"
                                                placeholder="0 3 * * *"
                                                value={form.cron_expression.clone()}
                                                oninput={set_field("cron_expression")}
                                                required=true
                                            />
                                            <span class="sched-hint">{ "min hour dom month dow" }</span>
                                            {
                                                if let Some(desc) = describe_cron(&form.cron_expression) {
                                                    html! { <span class="sched-cron-desc">{ desc }</span> }
                                                } else {
                                                    html! {}
                                                }
                                            }
                                        </div>
                                        <div class="sched-field sched-field-sm">
                                            <label>{ "Timezone" }</label>
                                            <input
                                                type="text"
                                                placeholder="UTC"
                                                value={form.timezone.clone()}
                                                oninput={set_field("timezone")}
                                            />
                                        </div>
                                        <div class="sched-field sched-field-sm">
                                            <label>{ "Timeout (min)" }</label>
                                            <input
                                                type="number"
                                                min="1"
                                                max="1440"
                                                value={form.max_runtime_minutes.to_string()}
                                                oninput={set_field("max_runtime_minutes")}
                                            />
                                        </div>
                                    </div>
                                    <div class="sched-field">
                                        <label>{ "Prompt" }</label>
                                        <textarea
                                            rows="4"
                                            placeholder="What should the agent do?"
                                            value={form.prompt.clone()}
                                            oninput={on_prompt_input}
                                            required=true
                                        />
                                    </div>
                                    <div class="sched-form-actions">
                                        <button type="button" class="sched-btn" onclick={close_form}>
                                            { "Cancel" }
                                        </button>
                                        <button type="submit" class="sched-btn sched-btn-primary">
                                            { if matches!(mode, FormMode::Create) { "Create" } else { "Save" } }
                                        </button>
                                    </div>
                                </form>
                            </div>
                        } else if can_schedule {
                            <button class="sched-btn sched-btn-primary sched-new-btn" onclick={open_create}>
                                { "+ New Task" }
                            </button>
                        }
                    </div>
                }
            </div>
        </div>
    }
}
