// TODO(#1165): remove this file-local ratchet after replacing production unwrap/expect paths.
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! `TasksPanel` — sub-component owning sub-agent / background-task UI state.
//!
//! Pulled out of `SessionView` so the parent component no longer carries the
//! `active_tasks` map, the `tool_use_id → task_id` reverse index, the
//! per-second tick interval for elapsed-time updates, or the entering /
//! progress / departing animation state for the tasks-drawer tab. The
//! parent keeps the WebSocket plumbing: when a `shared::ClaudeOutput` lands
//! that carries a task lifecycle signal (task started / task progress /
//! task notification / a tool_result for a tracked tool_use) the parent
//! derives the typed [`TaskEvent`] and forwards it into this handler via
//! the dispatcher callback registered at mount.

use gloo::timers::callback::Interval;
use std::collections::HashMap;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Status of a tracked task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Completed,
    Failed,
}

/// In-panel record for a single sub-agent / background-bash task.
#[derive(Debug, Clone)]
pub struct TaskEntry {
    pub task_type: String,
    pub description: String,
    pub started_at: f64,
    pub status: TaskStatus,
    pub duration_ms: Option<u64>,
    pub tool_uses: Option<u64>,
    pub total_tokens: Option<u64>,
    pub completed_at: Option<f64>,
    pub current_activity: Option<String>,
    pub last_tool_name: Option<String>,
}

impl TaskEntry {
    /// Build a fresh `Running` entry with an explicit start timestamp.
    /// The parent supplies the row's server-assigned `created_at` for the
    /// replay path and `js_sys::Date::now()` for live events; the panel
    /// stays out of the timestamp-source decision.
    pub fn with_started_at(
        task_type: impl Into<String>,
        description: impl Into<String>,
        started_at: f64,
    ) -> Self {
        Self {
            task_type: task_type.into(),
            description: description.into(),
            started_at,
            status: TaskStatus::Running,
            duration_ms: None,
            tool_uses: None,
            total_tokens: None,
            completed_at: None,
            current_activity: None,
            last_tool_name: None,
        }
    }
}

/// Per-task lifecycle event derived from a `shared::ClaudeOutput` by the
/// parent. The panel reacts to each variant by mutating its internal map;
/// the parent stays out of the task-state model.
#[derive(Debug, Clone)]
pub enum TaskEvent {
    /// New task announced via `system.task_started`. `started_at` is the
    /// row's server-assigned `created_at` when present, falling back to
    /// `js_sys::Date::now()` only for live frames without metadata.
    Started {
        task_id: String,
        tool_use_id: String,
        task_type: String,
        description: String,
        started_at: f64,
    },
    /// Progress tick via `system.task_progress`. `fallback_started_at` is
    /// only consulted when the panel has no record for this `task_id` yet
    /// (an out-of-order replay or a progress that arrived before its
    /// `started` cousin).
    Progress {
        task_id: String,
        description: String,
        last_tool_name: String,
        duration_ms: u64,
        tool_uses: u64,
        total_tokens: u64,
        fallback_started_at: f64,
    },
    /// Terminal status via `system.task_notification`. `completed_at` is
    /// the row's `created_at` when present, falling back to `Date.now()` only
    /// for live frames without metadata. When the notification arrives before
    /// any matching `Started`, the panel inserts a placeholder so the row
    /// still shows up.
    Notification {
        task_id: String,
        summary: String,
        status: TaskStatus,
        completed_at: f64,
        /// `(duration_ms, tool_uses, total_tokens)` — only present when
        /// the upstream notification carried a `usage` payload.
        usage: Option<(u64, u64, u64)>,
    },
    /// Fallback completion via a `tool_result` block in a User message.
    /// Used because `--print` mode skips `task_notification`. The panel
    /// looks up the task via its `tool_use_id → task_id` reverse index
    /// and only marks it `Completed` if it was still `Running`.
    ToolResult {
        tool_use_id: String,
        completed_at: f64,
    },
}

/// Channel into the panel from the parent. The parent registers a
/// `Live` dispatcher for events that arrived off the wire (animations fire)
/// and a separate `Replay` dispatcher for events derived from the REST
/// history hydration path (no animations, no audible cues — just state
/// hydration). The replay path also fires a `ClearForReplay` first to
/// reset the panel's state so a fresh REST batch can't append to a stale
/// `active_tasks` map.
#[derive(Debug, Clone)]
pub enum TasksInbound {
    /// Clear `active_tasks` and the reverse index — fired by the parent
    /// once at the start of a `LoadHistory` batch so the replay loop can
    /// re-hydrate from scratch.
    ClearForReplay,
    /// Live wire event: state mutation + animation hint.
    Live(TaskEvent),
    /// Replay event: state mutation only.
    Replay(TaskEvent),
}

#[derive(Properties, PartialEq)]
pub struct TasksPanelProps {
    /// Fired exactly once on `create`, handing the parent a callback it
    /// can invoke to push task events into the panel. Same dispatcher-
    /// registration pattern as `PermissionHandler::on_register`.
    pub on_register: Callback<Callback<TasksInbound>>,
}

pub enum TasksPanelMsg {
    Inbound(TasksInbound),
    /// 1-second tick: drop tasks that completed more than 10 s ago and
    /// stop the interval when the map empties.
    TaskTick,
    /// Toggle the slide-out panel.
    Toggle,
    /// Clear the entering / progress pulse class.
    ClearTabPulse,
    /// End the "departing" animation and hide the drawer.
    FinishDeparture,
}

pub struct TasksPanel {
    active_tasks: HashMap<String, TaskEntry>,
    /// Maps `tool_use_id → task_id` so a `ToolResult` event can find its
    /// task without the parent having to thread the mapping.
    tool_use_to_task: HashMap<String, String>,
    tasks_panel_open: bool,
    task_tick_handle: Option<Interval>,
    /// Animation state for the tasks tab: "entering", "progress", or
    /// "departing". `None` when idle.
    tab_anim: Option<&'static str>,
    /// Whether the drawer is still visible during the departure animation.
    tab_departing: bool,
}

impl Component for TasksPanel {
    type Message = TasksPanelMsg;
    type Properties = TasksPanelProps;

    fn create(ctx: &Context<Self>) -> Self {
        // Hand the parent a callback it can invoke to push events at us.
        // The parent stores this and calls it from its WS / REST hydration
        // paths — so the parent never has to model task state itself.
        let dispatcher = ctx.link().callback(TasksPanelMsg::Inbound);
        ctx.props().on_register.emit(dispatcher);

        Self {
            active_tasks: HashMap::new(),
            tool_use_to_task: HashMap::new(),
            tasks_panel_open: false,
            task_tick_handle: None,
            tab_anim: None,
            tab_departing: false,
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            TasksPanelMsg::Inbound(TasksInbound::ClearForReplay) => {
                self.active_tasks.clear();
                self.tool_use_to_task.clear();
                true
            }
            TasksPanelMsg::Inbound(TasksInbound::Replay(event)) => {
                self.apply_event(event);
                // Replay batches end with the parent done dispatching.
                // If any tasks remain (a session reconnect mid-task), the
                // tick interval needs to be running so elapsed-time labels
                // keep updating.
                if !self.active_tasks.is_empty() {
                    self.ensure_task_tick(ctx);
                }
                true
            }
            TasksPanelMsg::Inbound(TasksInbound::Live(event)) => {
                self.apply_event_live(ctx, event);
                true
            }
            TasksPanelMsg::TaskTick => {
                let now = js_sys::Date::now();
                // Drop completed tasks older than 10 s — matches the
                // pre-extraction behavior.
                self.active_tasks.retain(|_, task| match task.completed_at {
                    Some(t) => now - t < 10_000.0,
                    None => true,
                });
                if self.active_tasks.is_empty() {
                    self.task_tick_handle = None;
                }
                true
            }
            TasksPanelMsg::Toggle => {
                self.tasks_panel_open = !self.tasks_panel_open;
                true
            }
            TasksPanelMsg::ClearTabPulse => {
                self.tab_anim = None;
                true
            }
            TasksPanelMsg::FinishDeparture => {
                self.tab_departing = false;
                self.tasks_panel_open = false;
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let running_count = self.running_count();

        // Keep rendering during departure animation, otherwise hide when
        // no running tasks remain.
        if running_count == 0 && !self.tab_departing {
            return html! {};
        }

        let link = ctx.link();
        let on_toggle = link.callback(|e: MouseEvent| {
            e.stop_propagation();
            TasksPanelMsg::Toggle
        });

        let open = self.tasks_panel_open && !self.tab_departing;

        let mut classes = vec!["tasks-drawer"];
        if open {
            classes.push("open");
        }
        if let Some(anim) = self.tab_anim {
            classes.push(anim);
        }

        let mut tasks: Vec<_> = self.active_tasks.iter().collect();
        tasks.sort_by(|a, b| a.1.started_at.partial_cmp(&b.1.started_at).unwrap());

        html! {
            <div class={classes.join(" ")}>
                <div class="tasks-tab-hint" onclick={on_toggle}>
                    <span class="tasks-tab-count">{ format!("{}", running_count) }</span>
                    <span class="tasks-tab-label">{ "Tasks" }</span>
                </div>
                <div class="tasks-sidebar-panel">
                    <div class="tasks-sidebar-list">
                        { for tasks.iter().map(|(_, task)| render_task_pill(task)) }
                    </div>
                </div>
            </div>
        }
    }
}

impl TasksPanel {
    fn running_count(&self) -> usize {
        self.active_tasks
            .values()
            .filter(|t| t.status == TaskStatus::Running)
            .count()
    }

    fn ensure_task_tick(&mut self, ctx: &Context<Self>) {
        if self.task_tick_handle.is_some() {
            return;
        }
        let link = ctx.link().clone();
        self.task_tick_handle = Some(Interval::new(1_000, move || {
            link.send_message(TasksPanelMsg::TaskTick);
        }));
    }

    fn schedule_clear_pulse(&self, ctx: &Context<Self>, ms: u32) {
        let link = ctx.link().clone();
        spawn_local(async move {
            gloo::timers::future::TimeoutFuture::new(ms).await;
            link.send_message(TasksPanelMsg::ClearTabPulse);
        });
    }

    fn schedule_finish_departure(&self, ctx: &Context<Self>) {
        let link = ctx.link().clone();
        spawn_local(async move {
            gloo::timers::future::TimeoutFuture::new(500).await;
            link.send_message(TasksPanelMsg::FinishDeparture);
        });
    }

    /// Apply a live event: state mutation + animation/tick scheduling.
    fn apply_event_live(&mut self, ctx: &Context<Self>, event: TaskEvent) {
        let was_running_empty = self.running_count() == 0;
        let event_is_started = matches!(event, TaskEvent::Started { .. });
        let event_is_progress = matches!(event, TaskEvent::Progress { .. });

        self.apply_event(event);

        if !self.active_tasks.is_empty() {
            self.ensure_task_tick(ctx);
        }

        if event_is_started && was_running_empty {
            // Bright pulse when the first task appears.
            self.tab_departing = false;
            self.tab_anim = Some("entering");
            self.schedule_clear_pulse(ctx, 600);
        } else if event_is_progress {
            // Dim pulse on progress.
            self.tab_anim = Some("progress");
            self.schedule_clear_pulse(ctx, 400);
        }

        // Did this event leave us with no running tasks? Kick off the
        // departure animation.
        if self.running_count() == 0 && !self.active_tasks.is_empty() {
            self.tab_anim = Some("departing");
            self.tab_departing = true;
            self.schedule_finish_departure(ctx);
        }
    }

    /// Apply an event to `active_tasks` / `tool_use_to_task` without any
    /// animation or tick scheduling. Used directly by the replay path and
    /// by `apply_event_live` after it has captured the pre-event state it
    /// needs for animation decisions.
    fn apply_event(&mut self, event: TaskEvent) {
        match event {
            TaskEvent::Started {
                task_id,
                tool_use_id,
                task_type,
                description,
                started_at,
            } => {
                self.active_tasks.insert(
                    task_id.clone(),
                    TaskEntry::with_started_at(task_type, description, started_at),
                );
                self.tool_use_to_task.insert(tool_use_id, task_id);
            }
            TaskEvent::Progress {
                task_id,
                description,
                last_tool_name,
                duration_ms,
                tool_uses,
                total_tokens,
                fallback_started_at,
            } => {
                let entry = self.active_tasks.entry(task_id).or_insert_with(|| {
                    TaskEntry::with_started_at(
                        "local_agent",
                        description.clone(),
                        fallback_started_at,
                    )
                });
                entry.current_activity = Some(description);
                entry.last_tool_name = Some(last_tool_name);
                entry.duration_ms = Some(duration_ms);
                entry.tool_uses = Some(tool_uses);
                entry.total_tokens = Some(total_tokens);
            }
            TaskEvent::Notification {
                task_id,
                summary,
                status,
                completed_at,
                usage,
            } => {
                let entry = self.active_tasks.entry(task_id).or_insert_with(|| {
                    TaskEntry::with_started_at("local_agent", summary, completed_at)
                });
                entry.status = status;
                entry.completed_at = Some(completed_at);
                if let Some((duration_ms, tool_uses, total_tokens)) = usage {
                    entry.duration_ms = Some(duration_ms);
                    entry.tool_uses = Some(tool_uses);
                    entry.total_tokens = Some(total_tokens);
                }
            }
            TaskEvent::ToolResult {
                tool_use_id,
                completed_at,
            } => {
                let Some(task_id) = self.tool_use_to_task.get(&tool_use_id).cloned() else {
                    return;
                };
                let Some(entry) = self.active_tasks.get_mut(&task_id) else {
                    return;
                };
                if entry.status == TaskStatus::Running {
                    entry.status = TaskStatus::Completed;
                    entry.completed_at = Some(completed_at);
                }
            }
        }
    }
}

fn render_task_pill(task: &TaskEntry) -> Html {
    let status_class = match task.status {
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
    };

    let type_label = match task.task_type.as_str() {
        "local_agent" => "Sub-agent",
        "local_bash" => "Background Bash",
        _ => "Task",
    };

    let elapsed = format_elapsed(task);

    let fading = if task.completed_at.is_some() {
        " fading"
    } else {
        ""
    };

    html! {
        <div class={format!("task-pill {}{}", status_class, fading)}>
            <div class="task-pill-header">
                <span class={format!("task-status-dot {}", status_class)} />
                <span class="task-type-pill">{ type_label }</span>
                {
                    if let Some(ref tool) = task.last_tool_name {
                        html! { <span class="task-tool-badge">{ tool }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="task-pill-description">{ &task.description }</div>
            {
                if let Some(ref activity) = task.current_activity {
                    html! { <div class="task-pill-activity">{ activity }</div> }
                } else {
                    html! {}
                }
            }
            <div class="task-pill-stats">
                <span class="task-pill-stat">{ elapsed }</span>
                {
                    if let Some(tools) = task.tool_uses {
                        html! { <span class="task-pill-stat">{ format!("{} tools", tools) }</span> }
                    } else {
                        html! {}
                    }
                }
                {
                    if let Some(tokens) = task.total_tokens {
                        let label = if tokens >= 1000 {
                            format!("{:.1}k tok", tokens as f64 / 1000.0)
                        } else {
                            format!("{} tok", tokens)
                        };
                        html! { <span class="task-pill-stat">{ label }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
        </div>
    }
}

fn format_elapsed(task: &TaskEntry) -> String {
    let secs = match task.status {
        TaskStatus::Running => ((js_sys::Date::now() - task.started_at) / 1000.0).max(0.0) as u64,
        _ => match task.duration_ms {
            Some(ms) => ms / 1000,
            None => ((task.completed_at.unwrap_or(task.started_at) - task.started_at) / 1000.0)
                .max(0.0) as u64,
        },
    };
    format_secs(secs)
}

/// Render `secs` as either `"{m}m {s}s"` (>= 60 s) or `"{s}s"`. Pulled
/// out so the elapsed-time format is unit-testable without a `Date`.
fn format_secs(secs: u64) -> String {
    if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

/// Derive zero or more typed [`TaskEvent`]s from a parsed `ClaudeOutput`.
///
/// Used by both the live `WsEvent::Output` path and the REST `LoadHistory`
/// replay path. When the caller supplies the row's server-assigned
/// `created_at`, elapsed-time labels reflect when the event actually happened
/// rather than when the browser received or hydrated it.
pub(super) fn derive_task_events(
    claude_msg: &shared::ClaudeOutput,
    created_at_iso: &str,
    live: bool,
) -> Vec<TaskEvent> {
    let resolve_ts = || -> f64 {
        let parsed = js_sys::Date::parse(created_at_iso);
        if parsed.is_finite() {
            return parsed;
        }
        if live {
            js_sys::Date::now()
        } else {
            0.0
        }
    };

    let mut events = Vec::new();
    match claude_msg {
        shared::ClaudeOutput::System(sys) => {
            if let Some(task) = sys.as_task_started() {
                // 2.1.160: `task_type`/`tool_use_id` became Option and
                // TaskType is an open enum; `as_str` yields the wire tag
                // ("local_agent"/"local_bash"/…) the panel matches on.
                let task_type = task
                    .task_type
                    .as_ref()
                    .map(|tt| tt.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                events.push(TaskEvent::Started {
                    task_id: task.task_id.clone(),
                    tool_use_id: task.tool_use_id.clone().unwrap_or_default(),
                    task_type,
                    description: task.description.clone(),
                    started_at: resolve_ts(),
                });
            } else if let Some(progress) = sys.as_task_progress() {
                events.push(TaskEvent::Progress {
                    task_id: progress.task_id.clone(),
                    description: progress.description.clone(),
                    // Option in 2.1.160; empty string renders as "no tool yet".
                    last_tool_name: progress.last_tool_name.clone().unwrap_or_default(),
                    duration_ms: progress.usage.duration_ms,
                    tool_uses: progress.usage.tool_uses,
                    total_tokens: progress.usage.total_tokens,
                    fallback_started_at: resolve_ts(),
                });
            } else if let Some(notif) = sys.as_task_notification() {
                // 2.1.160: TaskStatus is an open enum with non-terminal
                // states. Only terminal statuses complete a panel entry;
                // killed/stopped read as failure, non-terminal and unknown
                // statuses leave the task running.
                let status = match &notif.status {
                    shared::TaskStatus::Completed => Some(TaskStatus::Completed),
                    shared::TaskStatus::Failed
                    | shared::TaskStatus::Killed
                    | shared::TaskStatus::Stopped => Some(TaskStatus::Failed),
                    _ => None,
                };
                if let Some(status) = status {
                    let usage = notif
                        .usage
                        .as_ref()
                        .map(|u| (u.duration_ms, u.tool_uses, u.total_tokens));
                    events.push(TaskEvent::Notification {
                        task_id: notif.task_id.clone(),
                        summary: notif.summary.clone(),
                        status,
                        completed_at: resolve_ts(),
                        usage,
                    });
                }
            }
        }
        shared::ClaudeOutput::User(user_msg) => {
            // Fallback: --print mode doesn't emit `task_notification`, so
            // any `tool_result` whose `tool_use_id` matches a tracked task
            // implicitly closes it. The panel owns the reverse index and
            // no-ops the event when there's no match.
            for block in &user_msg.message.content {
                if let shared::ContentBlock::ToolResult(tr) = block {
                    events.push(TaskEvent::ToolResult {
                        tool_use_id: tr.tool_use_id.clone(),
                        completed_at: resolve_ts(),
                    });
                }
            }
        }
        _ => {}
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_panel() -> TasksPanel {
        TasksPanel {
            active_tasks: HashMap::new(),
            tool_use_to_task: HashMap::new(),
            tasks_panel_open: false,
            task_tick_handle: None,
            tab_anim: None,
            tab_departing: false,
        }
    }

    fn started(task_id: &str, tool_use_id: &str, started_at: f64) -> TaskEvent {
        TaskEvent::Started {
            task_id: task_id.to_string(),
            tool_use_id: tool_use_id.to_string(),
            task_type: "local_agent".to_string(),
            description: format!("desc-{task_id}"),
            started_at,
        }
    }

    fn progress(task_id: &str, last_tool_name: &str, started_at: f64) -> TaskEvent {
        TaskEvent::Progress {
            task_id: task_id.to_string(),
            description: format!("progress-{task_id}"),
            last_tool_name: last_tool_name.to_string(),
            duration_ms: 2_500,
            tool_uses: 4,
            total_tokens: 1_234,
            fallback_started_at: started_at,
        }
    }

    fn notification(task_id: &str, status: TaskStatus, completed_at: f64) -> TaskEvent {
        TaskEvent::Notification {
            task_id: task_id.to_string(),
            summary: format!("notif-{task_id}"),
            status,
            completed_at,
            usage: Some((3_000, 5, 2_000)),
        }
    }

    fn tool_result(tool_use_id: &str, completed_at: f64) -> TaskEvent {
        TaskEvent::ToolResult {
            tool_use_id: tool_use_id.to_string(),
            completed_at,
        }
    }

    // --- happy-path lifecycle ---

    #[test]
    fn start_progress_complete_drives_task_through_full_lifecycle() {
        let mut panel = mk_panel();

        panel.apply_event(started("t1", "tu1", 1_000.0));
        let e = panel
            .active_tasks
            .get("t1")
            .expect("task missing after start");
        assert_eq!(e.status, TaskStatus::Running);
        assert_eq!(e.started_at, 1_000.0);
        assert_eq!(e.task_type, "local_agent");
        assert_eq!(panel.tool_use_to_task.get("tu1"), Some(&"t1".to_string()));

        panel.apply_event(progress("t1", "Bash", 1_000.0));
        let e = panel.active_tasks.get("t1").unwrap();
        assert_eq!(e.last_tool_name.as_deref(), Some("Bash"));
        assert_eq!(e.tool_uses, Some(4));
        assert_eq!(e.duration_ms, Some(2_500));
        assert_eq!(e.total_tokens, Some(1_234));
        // started_at preserved from the original Started event — Progress
        // never overwrites a known task's `started_at`.
        assert_eq!(e.started_at, 1_000.0);

        panel.apply_event(notification("t1", TaskStatus::Completed, 5_000.0));
        let e = panel.active_tasks.get("t1").unwrap();
        assert_eq!(e.status, TaskStatus::Completed);
        assert_eq!(e.completed_at, Some(5_000.0));
        // Notification usage payload overrode the progress numbers.
        assert_eq!(e.duration_ms, Some(3_000));
        assert_eq!(e.tool_uses, Some(5));
        assert_eq!(e.total_tokens, Some(2_000));
    }

    // --- missing-start race conditions ---

    #[test]
    fn progress_without_start_creates_placeholder_with_fallback_timestamp() {
        // Out-of-order replay: a Progress lands before its Started.
        // The panel must still surface the task, using the fallback
        // timestamp the parent supplied (the row's `created_at`).
        let mut panel = mk_panel();
        panel.apply_event(progress("t1", "Bash", 1_500.0));
        let e = panel.active_tasks.get("t1").expect("placeholder missing");
        assert_eq!(e.status, TaskStatus::Running);
        assert_eq!(e.started_at, 1_500.0);
        assert_eq!(e.last_tool_name.as_deref(), Some("Bash"));
    }

    #[test]
    fn notification_without_start_creates_placeholder_with_completion_timestamp() {
        // The notification arrived before the start (or the start row
        // was pruned). The placeholder uses the completion timestamp as
        // both `started_at` and `completed_at` — matching the pre-
        // extraction behavior in `LoadHistory`.
        let mut panel = mk_panel();
        panel.apply_event(notification("t1", TaskStatus::Failed, 7_777.0));
        let e = panel.active_tasks.get("t1").expect("placeholder missing");
        assert_eq!(e.status, TaskStatus::Failed);
        assert_eq!(e.started_at, 7_777.0);
        assert_eq!(e.completed_at, Some(7_777.0));
    }

    #[test]
    fn tool_result_for_unknown_tool_use_is_noop() {
        // No matching reverse-index entry → ToolResult is silently
        // dropped (the corresponding task was never started or has
        // already been pruned).
        let mut panel = mk_panel();
        panel.apply_event(tool_result("tu-missing", 9_000.0));
        assert!(panel.active_tasks.is_empty());
        assert!(panel.tool_use_to_task.is_empty());
    }

    #[test]
    fn tool_result_completes_running_task_via_reverse_index() {
        // The --print-mode fallback: a tool_result for a tracked
        // tool_use must close the matching task even if no explicit
        // `task_notification` ever arrived.
        let mut panel = mk_panel();
        panel.apply_event(started("t1", "tu1", 1_000.0));
        panel.apply_event(tool_result("tu1", 4_000.0));
        let e = panel.active_tasks.get("t1").unwrap();
        assert_eq!(e.status, TaskStatus::Completed);
        assert_eq!(e.completed_at, Some(4_000.0));
    }

    #[test]
    fn tool_result_does_not_resurrect_already_completed_task() {
        // A duplicate tool_result on an already-completed task must
        // NOT overwrite the existing `completed_at` — guards against
        // the "completion time drifts forward on every duplicate"
        // regression that would shift the 10-second prune window.
        let mut panel = mk_panel();
        panel.apply_event(started("t1", "tu1", 1_000.0));
        panel.apply_event(notification("t1", TaskStatus::Completed, 4_000.0));
        // Now a stray duplicate tool_result for the same tool_use.
        panel.apply_event(tool_result("tu1", 9_999.0));
        let e = panel.active_tasks.get("t1").unwrap();
        assert_eq!(
            e.completed_at,
            Some(4_000.0),
            "completion time must be sticky"
        );
    }

    // --- replay batching ---

    #[test]
    fn clear_for_replay_drops_existing_state() {
        let mut panel = mk_panel();
        panel.apply_event(started("t1", "tu1", 1_000.0));
        assert!(!panel.active_tasks.is_empty());
        assert!(!panel.tool_use_to_task.is_empty());

        // The parent kicks off a fresh REST hydration; the panel must
        // reset both maps so the next replay batch can't append to a
        // stale set.
        panel.active_tasks.clear();
        panel.tool_use_to_task.clear();
        assert!(panel.active_tasks.is_empty());
        assert!(panel.tool_use_to_task.is_empty());
    }

    // --- elapsed-time formatting ---

    #[test]
    fn format_secs_renders_seconds_under_a_minute() {
        assert_eq!(format_secs(0), "0s");
        assert_eq!(format_secs(1), "1s");
        assert_eq!(format_secs(59), "59s");
    }

    #[test]
    fn format_secs_renders_minutes_and_seconds_at_or_above_a_minute() {
        // 60 is the threshold — not "1m 0s as 60s".
        assert_eq!(format_secs(60), "1m 0s");
        assert_eq!(format_secs(61), "1m 1s");
        assert_eq!(format_secs(125), "2m 5s");
        assert_eq!(format_secs(3_600), "60m 0s");
    }

    #[test]
    fn format_elapsed_uses_duration_ms_when_task_completed() {
        let mut entry = TaskEntry::with_started_at("local_agent", "x", 0.0);
        entry.status = TaskStatus::Completed;
        entry.completed_at = Some(10_000.0);
        entry.duration_ms = Some(7_500);
        // Completed → prefer `duration_ms` over `(completed_at - started_at)`.
        assert_eq!(format_elapsed(&entry), "7s");
    }

    #[test]
    fn format_elapsed_falls_back_to_completion_minus_start_when_duration_missing() {
        let mut entry = TaskEntry::with_started_at("local_agent", "x", 1_000.0);
        entry.status = TaskStatus::Failed;
        entry.completed_at = Some(4_500.0);
        entry.duration_ms = None;
        // (4_500 - 1_000) / 1_000 = 3.5 → 3s
        assert_eq!(format_elapsed(&entry), "3s");
    }

    #[test]
    fn format_elapsed_for_failed_task_with_no_completion_uses_started_at_zero() {
        // Defensive: completed_at is None and duration_ms is None and
        // status is terminal. Fall back to `started_at - started_at = 0`.
        let mut entry = TaskEntry::with_started_at("local_agent", "x", 9_999.0);
        entry.status = TaskStatus::Failed;
        entry.completed_at = None;
        entry.duration_ms = None;
        assert_eq!(format_elapsed(&entry), "0s");
    }

    // --- running_count ---

    #[test]
    fn running_count_only_counts_running_tasks() {
        let mut panel = mk_panel();
        panel.apply_event(started("t1", "tu1", 1_000.0));
        panel.apply_event(started("t2", "tu2", 1_000.0));
        panel.apply_event(started("t3", "tu3", 1_000.0));
        panel.apply_event(notification("t2", TaskStatus::Completed, 5_000.0));
        panel.apply_event(notification("t3", TaskStatus::Failed, 5_000.0));
        assert_eq!(panel.running_count(), 1);
    }
}
