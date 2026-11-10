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

use crate::components::turn_metrics_display::compact_labeled;
use gloo::timers::callback::Interval;
use std::collections::{HashMap, HashSet};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// A task's own id (from `system.task_started.task_id`).
///
/// Distinct from [`ToolUseId`] so the reverse index below cannot be indexed
/// with the wrong kind of id: `active_tasks` is keyed by `TaskId`, the reverse
/// index maps `ToolUseId → TaskId`, and the compiler now rejects looking one up
/// with the other. Both were bare `String` (#921); a `tool_use_to_task.get(task_id)`
/// slip used to compile and silently miss.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub String);

/// A resumed subagent keeps its task id but opens a distinct run. Missing run
/// ids remain a separate legacy identity, never aliases for a newer run.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskKey {
    task_id: TaskId,
    run_id: Option<String>,
}

impl TaskKey {
    fn new(task_id: impl Into<String>, run_id: Option<String>) -> Self {
        Self {
            task_id: TaskId(task_id.into()),
            run_id,
        }
    }
}

/// The `tool_use_id` a task is spawned under, used to close the task when its
/// `tool_result` arrives in `--print` mode (which skips `task_notification`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ToolUseId(pub String);

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
    pub parent_task_id: Option<TaskId>,
    pub awaited: bool,
    pub report: Option<String>,
    pub warning: Option<String>,
    pub handback_label: Option<String>,
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
            parent_task_id: None,
            awaited: false,
            report: None,
            warning: None,
            handback_label: None,
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
        task_id: TaskKey,
        tool_use_id: Option<ToolUseId>,
        parent_task_id: Option<TaskId>,
        awaited: bool,
        task_type: String,
        description: String,
        started_at: f64,
    },
    /// Progress tick via `system.task_progress`. `fallback_started_at` is
    /// only consulted when the panel has no record for this `task_id` yet
    /// (an out-of-order replay or a progress that arrived before its
    /// `started` cousin).
    Progress {
        task_id: TaskKey,
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
        task_id: TaskKey,
        summary: String,
        report: Option<String>,
        warning: Option<String>,
        handback_label: Option<String>,
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
        tool_use_id: ToolUseId,
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
    active_tasks: HashMap<TaskKey, TaskEntry>,
    /// Maps `tool_use_id → task_id` so a `ToolResult` event can find its
    /// task without the parent having to thread the mapping.
    tool_use_to_task: HashMap<ToolUseId, HashSet<TaskKey>>,
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
                self.tool_use_to_task.retain(|_, keys| {
                    keys.retain(|key| self.active_tasks.contains_key(key));
                    !keys.is_empty()
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

        let tasks = ordered_tasks(&self.active_tasks);

        html! {
            <div class={classes.join(" ")}>
                <div class="tasks-tab-hint" onclick={on_toggle}>
                    <span class="tasks-tab-count">{ format!("{}", running_count) }</span>
                    <span class="tasks-tab-label">{ "Tasks" }</span>
                </div>
                <div class="tasks-sidebar-panel">
                    <div class="tasks-sidebar-list">
                        { for tasks.iter().map(|(key, task, depth)| html! {
                            <div key={format!("{key:?}")} class="task-tree-entry"
                                style={format!("margin-left: {}px", (*depth).min(8) * 12)}>
                                { render_task_pill(task) }
                            </div>
                        }) }
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
                parent_task_id,
                awaited,
                task_type,
                description,
                started_at,
            } => {
                // A delayed Started frame fills in a placeholder without
                // erasing progress or a terminal notification already seen.
                let entry = self.active_tasks.entry(task_id.clone()).or_insert_with(|| {
                    TaskEntry::with_started_at(task_type.clone(), description.clone(), started_at)
                });
                entry.task_type = task_type;
                entry.description = description;
                entry.started_at = started_at;
                entry.parent_task_id = parent_task_id;
                entry.awaited = awaited;
                if let Some(tool_use_id) = tool_use_id {
                    self.tool_use_to_task
                        .entry(tool_use_id)
                        .or_default()
                        .insert(task_id);
                }
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
                report,
                warning,
                handback_label,
                status,
                completed_at,
                usage,
            } => {
                let entry = self.active_tasks.entry(task_id).or_insert_with(|| {
                    TaskEntry::with_started_at("local_agent", summary, completed_at)
                });
                entry.status = status;
                entry.report = report;
                entry.warning = warning;
                entry.handback_label = handback_label;
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
                let Some(keys) = self.tool_use_to_task.get(&tool_use_id) else {
                    return;
                };
                // A reused tool id cannot identify which run completed.
                // Wait for an explicit notification instead of closing a
                // resumed run because an old result arrived late.
                if keys.len() != 1 {
                    return;
                }
                let Some(task_id) = keys.iter().next().cloned() else {
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

/// Build a deterministic, cycle-safe tree. A child's parent id does not name a
/// run, so associate it with the latest known parent run opened by the child's
/// start time. Missing/pruned parents remain roots instead of hiding the child.
fn ordered_tasks(tasks: &HashMap<TaskKey, TaskEntry>) -> Vec<(&TaskKey, &TaskEntry, usize)> {
    let mut sorted: Vec<_> = tasks.iter().collect();
    sorted.sort_by(|a, b| {
        a.1.started_at
            .total_cmp(&b.1.started_at)
            .then_with(|| a.0.cmp(b.0))
    });
    let mut children = vec![Vec::new(); sorted.len()];
    let mut roots = Vec::new();
    for (index, (key, task)) in sorted.iter().enumerate() {
        let parent = task.parent_task_id.as_ref().and_then(|id| {
            sorted
                .iter()
                .enumerate()
                .filter(|(_, (candidate, entry))| {
                    candidate != key
                        && &candidate.task_id == id
                        && entry.started_at <= task.started_at
                })
                .max_by(|(_, a), (_, b)| {
                    a.1.started_at
                        .total_cmp(&b.1.started_at)
                        .then_with(|| a.0.run_id.cmp(&b.0.run_id))
                })
                .map(|(index, _)| index)
        });
        if let Some(parent) = parent {
            children[parent].push(index);
        } else {
            roots.push(index);
        }
    }
    let mut ordered = Vec::with_capacity(sorted.len());
    let mut visited = HashSet::new();
    // The second pass includes orphaned cyclic components exactly once.
    for root in roots.into_iter().chain(0..sorted.len()) {
        let mut stack = vec![(root, 0)];
        while let Some((index, depth)) = stack.pop() {
            if !visited.insert(index) {
                continue;
            }
            ordered.push((sorted[index].0, sorted[index].1, depth));
            stack.extend(
                children[index]
                    .iter()
                    .rev()
                    .map(|child| (*child, depth + 1)),
            );
        }
    }
    ordered
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
                if task.awaited {
                    <span class="task-tool-badge">{ "awaited" }</span>
                }
                if let Some(label) = &task.handback_label {
                    <span class="task-report-status">{ label }</span>
                }
                {
                    if let Some(ref tool) = task.last_tool_name {
                        html! { <span class="task-tool-badge">{ tool }</span> }
                    } else {
                        html! {}
                    }
                }
            </div>
            <div class="task-pill-description">{ &task.description }</div>
            if let Some(warning) = &task.warning {
                <div class="task-handback-warning" role="alert">{ warning }</div>
            }
            if let Some(report) = &task.report {
                <div class="task-pill-report">{ report }</div>
            }
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
                        html! { <span class="task-pill-stat">{ compact_labeled(tokens as i64, "tok") }</span> }
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
                    task_id: TaskKey::new(task.task_id.clone(), task.run_id.clone()),
                    tool_use_id: task.tool_use_id.clone().map(ToolUseId),
                    parent_task_id: task.parent_task_id.clone().map(TaskId),
                    awaited: task.awaited.unwrap_or(false),
                    task_type,
                    description: task.description.clone(),
                    started_at: resolve_ts(),
                });
            } else if let Some(progress) = sys.as_task_progress() {
                events.push(TaskEvent::Progress {
                    task_id: TaskKey::new(progress.task_id.clone(), progress.run_id.clone()),
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
                    let report =
                        crate::components::task_report::TaskReport::from_notification(&notif);
                    events.push(TaskEvent::Notification {
                        task_id: TaskKey::new(notif.task_id.clone(), notif.run_id.clone()),
                        summary: if report.label.is_some() {
                            "Subagent report".to_string()
                        } else {
                            notif.summary.clone()
                        },
                        report: report.text.map(str::to_string),
                        warning: report.warning.map(str::to_string),
                        handback_label: report.label.map(str::to_string),
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
                        tool_use_id: ToolUseId(tr.tool_use_id.clone()),
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
            task_id: TaskKey::new(task_id, None),
            tool_use_id: Some(ToolUseId(tool_use_id.to_string())),
            parent_task_id: None,
            awaited: false,
            task_type: "local_agent".to_string(),
            description: format!("desc-{task_id}"),
            started_at,
        }
    }

    fn progress(task_id: &str, last_tool_name: &str, started_at: f64) -> TaskEvent {
        TaskEvent::Progress {
            task_id: TaskKey::new(task_id, None),
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
            task_id: TaskKey::new(task_id, None),
            summary: format!("notif-{task_id}"),
            report: Some(format!("report-{task_id}")),
            warning: None,
            handback_label: None,
            status,
            completed_at,
            usage: Some((3_000, 5, 2_000)),
        }
    }

    fn tool_result(tool_use_id: &str, completed_at: f64) -> TaskEvent {
        TaskEvent::ToolResult {
            tool_use_id: ToolUseId(tool_use_id.to_string()),
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
            .get(&TaskKey::new("t1", None))
            .expect("task missing after start");
        assert_eq!(e.status, TaskStatus::Running);
        assert_eq!(e.started_at, 1_000.0);
        assert_eq!(e.task_type, "local_agent");
        assert_eq!(
            panel.tool_use_to_task.get(&ToolUseId("tu1".to_string())),
            Some(&HashSet::from([TaskKey::new("t1", None)]))
        );

        panel.apply_event(progress("t1", "Bash", 1_000.0));
        let e = panel.active_tasks.get(&TaskKey::new("t1", None)).unwrap();
        assert_eq!(e.last_tool_name.as_deref(), Some("Bash"));
        assert_eq!(e.tool_uses, Some(4));
        assert_eq!(e.duration_ms, Some(2_500));
        assert_eq!(e.total_tokens, Some(1_234));
        // started_at preserved from the original Started event — Progress
        // never overwrites a known task's `started_at`.
        assert_eq!(e.started_at, 1_000.0);

        panel.apply_event(notification("t1", TaskStatus::Completed, 5_000.0));
        let e = panel.active_tasks.get(&TaskKey::new("t1", None)).unwrap();
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
        let e = panel
            .active_tasks
            .get(&TaskKey::new("t1", None))
            .expect("placeholder missing");
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
        let e = panel
            .active_tasks
            .get(&TaskKey::new("t1", None))
            .expect("placeholder missing");
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
        let e = panel.active_tasks.get(&TaskKey::new("t1", None)).unwrap();
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
        let e = panel.active_tasks.get(&TaskKey::new("t1", None)).unwrap();
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

    // --- task token formatting ---

    #[test]
    fn task_tokens_keep_exact_count_below_1000() {
        assert_eq!(compact_labeled(547, "tok"), "547 tok");
        assert_eq!(compact_labeled(0, "tok"), "0 tok");
    }

    #[test]
    fn task_tokens_use_one_decimal_k_at_or_above_1000() {
        assert_eq!(compact_labeled(1000, "tok"), "1.0k tok");
        assert_eq!(compact_labeled(1500, "tok"), "1.5k tok");
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
    fn with_run(mut event: TaskEvent, run: &str) -> TaskEvent {
        match &mut event {
            TaskEvent::Started { task_id, .. }
            | TaskEvent::Progress { task_id, .. }
            | TaskEvent::Notification { task_id, .. } => task_id.run_id = Some(run.into()),
            _ => panic!("expected task event"),
        }
        event
    }

    #[test]
    fn resumed_runs_keep_progress_notifications_and_tool_results_separate() {
        let mut panel = mk_panel();
        panel.apply_event(with_run(started("task", "old-tool", 1_000.0), "run-1"));
        panel.apply_event(with_run(started("task", "new-tool", 2_000.0), "run-2"));
        panel.apply_event(with_run(progress("task", "Read", 2_000.0), "run-2"));
        panel.apply_event(tool_result("old-tool", 3_000.0));
        let old = TaskKey::new("task", Some("run-1".into()));
        let new = TaskKey::new("task", Some("run-2".into()));
        assert_eq!(panel.active_tasks[&old].status, TaskStatus::Completed);
        assert_eq!(panel.active_tasks[&new].status, TaskStatus::Running);
        assert_eq!(
            panel.active_tasks[&new].last_tool_name.as_deref(),
            Some("Read")
        );
        panel.apply_event(with_run(
            notification("task", TaskStatus::Failed, 4_000.0),
            "run-1",
        ));
        assert_eq!(panel.active_tasks[&old].status, TaskStatus::Failed);
        assert_eq!(panel.active_tasks[&new].status, TaskStatus::Running);
    }

    #[test]
    fn ambiguous_tool_results_do_not_close_a_resumed_run() {
        let mut panel = mk_panel();
        panel.apply_event(with_run(started("task", "shared-tool", 1_000.0), "run-1"));
        panel.apply_event(with_run(started("task", "shared-tool", 2_000.0), "run-2"));
        panel.apply_event(tool_result("shared-tool", 3_000.0));
        assert_eq!(panel.running_count(), 2);
    }

    #[test]
    fn late_start_does_not_resurrect_completed_run_or_lose_warning() {
        let mut panel = mk_panel();
        let mut completion = notification("task", TaskStatus::Completed, 4_000.0);
        if let TaskEvent::Notification {
            warning,
            handback_label,
            ..
        } = &mut completion
        {
            *warning = Some("Review could not run".into());
            *handback_label = Some("Report flagged".into());
        }
        panel.apply_event(completion);
        panel.apply_event(started("task", "tool", 1_000.0));
        let entry = &panel.active_tasks[&TaskKey::new("task", None)];
        assert_eq!(entry.status, TaskStatus::Completed);
        assert_eq!(entry.warning.as_deref(), Some("Review could not run"));
        assert_eq!(entry.report.as_deref(), Some("report-task"));
        assert_eq!(entry.started_at, 1_000.0);
    }

    #[test]
    fn tree_uses_parent_run_at_child_start_and_keeps_orphans_and_cycles_visible() {
        let mut tasks = HashMap::new();
        let old_parent = TaskKey::new("parent", Some("run-1".into()));
        let new_parent = TaskKey::new("parent", Some("run-2".into()));
        let child = TaskKey::new("child", Some("run-1".into()));
        let orphan = TaskKey::new("orphan", None);
        tasks.insert(
            old_parent.clone(),
            TaskEntry::with_started_at("local_agent", "parent", 1_000.0),
        );
        tasks.insert(
            new_parent.clone(),
            TaskEntry::with_started_at("local_agent", "parent resumed", 3_000.0),
        );
        let mut entry = TaskEntry::with_started_at("local_agent", "child", 2_000.0);
        entry.parent_task_id = Some(TaskId("parent".into()));
        tasks.insert(child.clone(), entry);
        let mut entry = TaskEntry::with_started_at("local_agent", "orphan", 4_000.0);
        entry.parent_task_id = Some(TaskId("missing".into()));
        tasks.insert(orphan.clone(), entry);
        let ordered: Vec<_> = ordered_tasks(&tasks)
            .into_iter()
            .map(|(key, _, depth)| (key.clone(), depth))
            .collect();
        assert_eq!(
            ordered,
            vec![(old_parent, 0), (child, 1), (new_parent, 0), (orphan, 0)]
        );

        for (id, parent) in [("cycle-a", "cycle-b"), ("cycle-b", "cycle-a")] {
            let mut entry = TaskEntry::with_started_at("local_agent", id, 5_000.0);
            entry.parent_task_id = Some(TaskId(parent.into()));
            tasks.insert(TaskKey::new(id, None), entry);
        }
        let ordered = ordered_tasks(&tasks);
        assert_eq!(ordered.len(), tasks.len());
        assert_eq!(
            ordered
                .iter()
                .map(|(key, _, _)| *key)
                .collect::<HashSet<_>>()
                .len(),
            tasks.len()
        );
    }
}
