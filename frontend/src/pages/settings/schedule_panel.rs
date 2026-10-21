//! Settings ▸ Schedule: a local-time calendar of enabled firings in the next 72 hours.

use std::collections::BTreeMap;

use shared::api::{
    ScheduledTaskInfo, ScheduledTaskListResponse, ScheduledTaskOccurrence, ScheduledTaskRun,
    ScheduledTaskRunsResponse, UpcomingScheduledTasksResponse,
};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;
use yew_router::prelude::Link;

use crate::components::ScheduleDialog;
use crate::utils::{self, On401};
use crate::Route;

/// Runs shown per task; the endpoint returns up to 50.
const RUNS_PER_TASK: usize = 10;

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn local_date(iso: &str) -> js_sys::Date {
    js_sys::Date::new(&wasm_bindgen::JsValue::from_str(iso))
}

fn day_key(date: &js_sys::Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.get_full_year(),
        date.get_month() + 1,
        date.get_date()
    )
}

fn day_label(date: &js_sys::Date) -> String {
    let weekday = WEEKDAYS.get(date.get_day() as usize).copied().unwrap_or("");
    let month = MONTHS.get(date.get_month() as usize).copied().unwrap_or("");
    format!("{weekday}, {month} {}", date.get_date())
}

fn time_label(iso: &str) -> String {
    let date = local_date(iso);
    let hour = date.get_hours();
    let (display_hour, suffix) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    format!("{display_hour}:{:02} {suffix}", date.get_minutes())
}

fn calendar_days(data: &UpcomingScheduledTasksResponse) -> Vec<(String, String)> {
    let cursor = local_date(&data.starts_at);
    let end = local_date(&data.ends_at).get_time();
    let mut days = Vec::new();
    while cursor.get_time() <= end {
        days.push((day_key(&cursor), day_label(&cursor)));
        cursor.set_date(cursor.get_date() + 1);
    }
    days
}

fn run_link(run: &ScheduledTaskRun) -> Html {
    let started = local_date(&run.started_at);
    let label = format!("{} · {}", day_label(&started), time_label(&run.started_at));
    // Runs have no rail pill, so this is the way back to them: the archived
    // transcript once the sweep has it, the live dashboard view before that.
    let target = if run.archived {
        html! {
            <Link<Route> to={Route::HistorySession {
                user: run.user_id.to_string(),
                session: run.session_id.to_string(),
            }}>{ "View log" }</Link<Route>>
        }
    } else {
        html! {
            <a href={format!("/dashboard?session={}", run.session_id)}>{ "Open session" }</a>
        }
    };
    html! {
        <li class="schedule-run" key={run.session_id.to_string()}>
            <time datetime={run.started_at.clone()}>{ label }</time>
            if run.active {
                <span class="schedule-run-live">{ "running" }</span>
            }
            { target }
        </li>
    }
}

#[derive(Properties, PartialEq)]
struct RecentRunsProps {
    refresh: u32,
}

/// Past runs of each scheduled task, linking to their logs.
#[function_component(RecentRuns)]
fn recent_runs(props: &RecentRunsProps) -> Html {
    let runs =
        use_state(|| None::<Result<Vec<(ScheduledTaskInfo, Vec<ScheduledTaskRun>)>, String>>);
    {
        let runs = runs.clone();
        use_effect_with(props.refresh, move |_| {
            spawn_local(async move {
                let result = async {
                    let tasks = utils::fetch_json::<ScheduledTaskListResponse>(
                        "/api/scheduled-tasks",
                        On401::Logout,
                    )
                    .await?
                    .tasks;
                    let mut out = Vec::with_capacity(tasks.len());
                    for task in tasks {
                        let task_runs = utils::fetch_json::<ScheduledTaskRunsResponse>(
                            &format!("/api/scheduled-tasks/{}/runs", task.id),
                            On401::Logout,
                        )
                        .await?
                        .runs;
                        out.push((task, task_runs));
                    }
                    Ok::<_, utils::FetchError>(out)
                }
                .await;
                runs.set(Some(result.map_err(|e| e.to_string())));
            });
            || ()
        });
    }

    let body = match &*runs {
        None => html! { <p class="schedule-day-empty">{ "Loading runs…" }</p> },
        Some(Err(error)) => {
            html! { <p class="settings-error">{ format!("Could not load runs: {error}") }</p> }
        }
        Some(Ok(tasks)) if tasks.is_empty() => {
            html! { <p class="schedule-day-empty">{ "No scheduled tasks yet." }</p> }
        }
        Some(Ok(tasks)) => html! {
            { for tasks.iter().map(|(task, task_runs)| html! {
                <section class="schedule-runs-task" key={task.id.to_string()}>
                    <header class="schedule-day-header">
                        <h3 title={task.fields.name.clone()}>{ &task.fields.name }</h3>
                        <span>{ format!("{} · {}", task.fields.launch.agent_type.as_str(), task.hostname) }</span>
                    </header>
                    if task_runs.is_empty() {
                        <p class="schedule-day-empty">{ "Has not run yet" }</p>
                    } else {
                        <ul class="schedule-run-list">
                            { for task_runs.iter().take(RUNS_PER_TASK).map(run_link) }
                        </ul>
                    }
                </section>
            }) }
        },
    };

    html! {
        <div class="schedule-runs">
            <div class="section-header">
                <h2>{ "Recent Runs" }</h2>
                <p class="section-description">
                    { "Scheduled runs stay out of the session rail. Open a run here; its log moves to History once archived." }
                </p>
            </div>
            { body }
        </div>
    }
}

#[function_component(SchedulePanel)]
pub fn schedule_panel() -> Html {
    let schedule = use_state(|| None::<UpcomingScheduledTasksResponse>);
    let error = use_state(|| None::<String>);
    let manage_open = use_state(|| false);
    let refresh = use_state(|| 0_u32);

    {
        let schedule = schedule.clone();
        let error = error.clone();
        use_effect_with(*refresh, move |_| {
            spawn_local(async move {
                match utils::fetch_json::<UpcomingScheduledTasksResponse>(
                    "/api/scheduled-tasks/upcoming",
                    On401::Logout,
                )
                .await
                {
                    Ok(data) => schedule.set(Some(data)),
                    Err(err) => error.set(Some(err.to_string())),
                }
            });
            || ()
        });
    }

    let content = if let Some(error) = &*error {
        html! { <p class="settings-error">{ format!("Could not load the schedule: {error}") }</p> }
    } else if let Some(data) = &*schedule {
        let mut by_day: BTreeMap<String, Vec<&ScheduledTaskOccurrence>> = BTreeMap::new();
        let today = day_key(&js_sys::Date::new_0());
        for occurrence in &data.occurrences {
            by_day
                .entry(day_key(&local_date(&occurrence.scheduled_for)))
                .or_default()
                .push(occurrence);
        }
        html! {
            <>
                if data.truncated {
                    <p class="schedule-warning">
                        { "This schedule is unusually dense; showing the first 10,000 firings." }
                    </p>
                }
                <div class="schedule-calendar" aria-label="Scheduled task firings for the next 72 hours">
                    { for calendar_days(data).into_iter().map(|(key, label)| {
                        let occurrences = by_day.get(&key).cloned().unwrap_or_default();
                        let is_today = key == today;
                        html! {
                            <section class={classes!("schedule-day", is_today.then_some("is-today"))} key={key}>
                                <header class="schedule-day-header">
                                    <h3>{ if is_today { format!("Today · {label}") } else { label } }</h3>
                                    <span>{ format!("{} run{}", occurrences.len(), if occurrences.len() == 1 { "" } else { "s" }) }</span>
                                </header>
                                <div class="schedule-day-events">
                                    if occurrences.is_empty() {
                                        <p class="schedule-day-empty">{ "No runs" }</p>
                                    } else {
                                        { for occurrences.into_iter().map(|occurrence| html! {
                                            <article class={classes!("schedule-event", format!("agent-{}", occurrence.agent_type.as_str()))}>
                                                <time datetime={occurrence.scheduled_for.clone()}>
                                                    { time_label(&occurrence.scheduled_for) }
                                                </time>
                                                <div class="schedule-event-details">
                                                    <strong title={occurrence.task_name.clone()}>{ &occurrence.task_name }</strong>
                                                    <span>{ format!("{} · {}", occurrence.agent_type.as_str(), occurrence.hostname) }</span>
                                                </div>
                                            </article>
                                        }) }
                                    }
                                </div>
                            </section>
                        }
                    }) }
                </div>
            </>
        }
    } else {
        html! {
            <div class="loading">
                <div class="spinner"></div>
                <p>{ "Loading schedule…" }</p>
            </div>
        }
    };

    html! {
        <section class="section-stack schedule-section">
            <div class="section-header">
                <h2>{ "Upcoming Schedule" }</h2>
                <p class="section-description">
                    { "Enabled scheduled-task runs during the next 72 hours, shown in your local time." }
                </p>
                <button class="sched-btn sched-btn-primary" onclick={{
                    let manage_open = manage_open.clone();
                    Callback::from(move |_| manage_open.set(true))
                }}>{ "Manage scheduled tasks" }</button>
            </div>
            { content }
            <RecentRuns refresh={*refresh} />
            if *manage_open {
                <ScheduleDialog session={None::<shared::SessionInfo>} on_close={{
                    let manage_open = manage_open.clone();
                    let refresh = refresh.clone();
                    Callback::from(move |_| {
                        manage_open.set(false);
                        refresh.set((*refresh).wrapping_add(1));
                    })
                }} />
            }
        </section>
    }
}
