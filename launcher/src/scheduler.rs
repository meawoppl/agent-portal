use chrono::{DateTime, Utc};
use shared::{ContinuationConfig, ScheduledTaskConfig};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use uuid::Uuid;

/// Delay before sending the prompt after session spawn.
/// Gives the proxy time to connect and register the session row.
const PROMPT_DELAY: Duration = Duration::from_secs(5);
const CONTINUATION_RESET_SKEW_SECS: i64 = 120;

/// Seconds to wait past a continuation's `reset_at` before firing it.
///
/// Usage-limit resets wait `CONTINUATION_RESET_SKEW_SECS`: the reset time Claude
/// reports is approximate and firing early re-trips the limit. Overload retries
/// fire with no skew — the backend already encoded the intended (immediate/60s/
/// 300s) delay in `reset_at`, and the CLI backs off internally, so any extra
/// wait here would defeat the "retry immediately" intent.
fn continuation_skew_secs(reason: &str) -> i64 {
    match shared::ContinuationReason::from_wire(reason) {
        Some(shared::ContinuationReason::Overloaded) => 0,
        // `Limit` and any legacy/unknown reason wait the full skew — matches the
        // historical `else` default (see `ContinuationReason`'s unknown-value
        // policy).
        Some(shared::ContinuationReason::Limit) | None => CONTINUATION_RESET_SKEW_SECS,
    }
}

struct ActiveTask {
    config: ScheduledTaskConfig,
    next_fire: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingLaunchKind {
    ScheduledTask {
        task_id: Uuid,
    },
    Continuation {
        continuation_id: Uuid,
        session_id: Uuid,
        prompt: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingLaunchInfo {
    pub last_session_id: Option<Uuid>,
    pub kind: PendingLaunchKind,
}

struct PendingLaunch {
    kind: PendingLaunchKind,
    pub last_session_id: Option<Uuid>,
    prompt: String,
}

struct PendingPrompt {
    session_id: Uuid,
    task_id: Uuid,
    prompt: String,
    send_at: Instant,
}

#[derive(Clone)]
pub struct ReadyContinuation {
    pub id: Uuid,
    pub session_id: Uuid,
    pub prompt: String,
    pub working_directory: Option<String>,
    pub session_name: Option<String>,
    pub claude_args: Vec<String>,
    pub agent_type: shared::AgentType,
}

pub struct RunningInfo {
    pub task_id: Uuid,
    pub started_at: Instant,
    max_runtime: Duration,
}

pub struct TaskToFire {
    pub request_id: Uuid,
    pub config: ScheduledTaskConfig,
}

pub struct Scheduler {
    tasks: Vec<ActiveTask>,
    pending_launches: HashMap<Uuid, PendingLaunch>,
    running: HashMap<Uuid, RunningInfo>,
    pending_prompts: Vec<PendingPrompt>,
    continuations: Vec<ContinuationConfig>,
    /// Last session spawned per task, tracked locally so continue-mode firings
    /// resume the right conversation even within a single launcher connection.
    ///
    /// `ScheduledTaskConfig.last_session_id` only refreshes on `ScheduleSync`
    /// (task edit / launcher reconnect), so after a run completes the config
    /// copy is stale until the next sync. This map is updated on every spawn and
    /// takes precedence, with the config value as the cross-reconnect fallback.
    last_session_by_task: HashMap<Uuid, Uuid>,
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            pending_launches: HashMap::new(),
            running: HashMap::new(),
            pending_prompts: Vec::new(),
            continuations: Vec::new(),
            last_session_by_task: HashMap::new(),
        }
    }

    /// Replace task configs from ScheduleSync. Preserves running session state.
    pub fn update_tasks(&mut self, configs: Vec<ScheduledTaskConfig>) {
        let running_task_ids: HashSet<Uuid> = self.running.values().map(|r| r.task_id).collect();

        self.tasks = configs
            .into_iter()
            .map(|config| {
                let next_fire = if config.enabled && !running_task_ids.contains(&config.id) {
                    compute_next_fire(&config.fields.cron_expression, &config.fields.timezone)
                } else {
                    None
                };
                if let Some(ref next) = next_fire {
                    info!("Task '{}': next fire at {}", config.fields.name, next);
                }
                ActiveTask { config, next_fire }
            })
            .collect();

        info!("Schedule updated: {} task(s)", self.tasks.len());
    }

    /// Duration until the next task fires. None if no tasks are scheduled.
    pub fn next_fire_duration(&self) -> Option<Duration> {
        let now = Utc::now();
        self.tasks
            .iter()
            .filter_map(|t| t.next_fire)
            .filter_map(|next| {
                if next <= now {
                    Some(Duration::ZERO)
                } else {
                    (next - now).to_std().ok()
                }
            })
            .min()
    }

    /// Duration until the next pending prompt is ready. None if no prompts pending.
    pub fn next_prompt_duration(&self) -> Option<Duration> {
        let now = Instant::now();
        self.pending_prompts
            .iter()
            .map(|p| p.send_at.saturating_duration_since(now))
            .min()
    }

    pub fn update_continuations(&mut self, continuations: Vec<ContinuationConfig>) {
        info!(
            "Continuation schedule updated: {} one-shot continuation(s)",
            continuations.len()
        );
        self.continuations = continuations;
    }

    pub fn next_continuation_duration(&self) -> Option<Duration> {
        let now = Utc::now();
        self.continuations
            .iter()
            .filter_map(|c| {
                DateTime::parse_from_rfc3339(&c.reset_at).ok().map(|dt| {
                    dt.with_timezone(&Utc)
                        + chrono::Duration::seconds(continuation_skew_secs(&c.reason))
                })
            })
            .map(|next| {
                if next <= now {
                    Duration::ZERO
                } else {
                    (next - now).to_std().unwrap_or(Duration::ZERO)
                }
            })
            .min()
    }

    pub fn ready_continuations(&mut self) -> Vec<ReadyContinuation> {
        let now = Utc::now();
        let mut ready = Vec::new();
        self.continuations.retain(|c| {
            let due = DateTime::parse_from_rfc3339(&c.reset_at)
                .ok()
                .map(|dt| {
                    dt.with_timezone(&Utc)
                        + chrono::Duration::seconds(continuation_skew_secs(&c.reason))
                        <= now
                })
                .unwrap_or(false);
            if due {
                ready.push(ReadyContinuation {
                    id: c.id,
                    session_id: c.session_id,
                    prompt: c.prompt.clone(),
                    working_directory: c.working_directory.clone(),
                    session_name: c.session_name.clone(),
                    claude_args: c.claude_args.clone(),
                    agent_type: c.agent_type,
                });
                false
            } else {
                true
            }
        });
        ready
    }

    /// Find and return tasks that are due to fire. Advances next_fire times.
    ///
    /// Session-mode decides what a due firing does:
    /// - `Fresh` (default): launch a brand-new session (`last_session_id: None`).
    ///   If a prior run is still active, skip this firing (overlap policy).
    /// - `Continue`: if the task's current session is still active, inject the
    ///   prompt into it (no new launch). Otherwise relaunch resuming the prior
    ///   session id when one exists (the agent's native `--resume` / thread
    ///   resume), or launch fresh on the very first run.
    pub fn fire_due_tasks(&mut self) -> Vec<TaskToFire> {
        let now = Utc::now();
        // task_id -> session_id of a currently-running run for that task.
        let active_session_by_task: HashMap<Uuid, Uuid> = self
            .running
            .iter()
            .map(|(session_id, info)| (info.task_id, *session_id))
            .collect();

        let mut to_fire = Vec::new();
        let mut new_pending = Vec::new();
        let mut new_prompts = Vec::new();

        for task in &mut self.tasks {
            let Some(next) = task.next_fire else {
                continue;
            };
            if next > now {
                continue;
            }

            let task_id = task.config.id;
            let is_continue = task.config.fields.session_mode == shared::SessionMode::Continue;
            let active_session = active_session_by_task.get(&task_id).copied();

            // Always advance next_fire; whether we launch, inject, or skip below
            // this firing is consumed either way.
            let advance = |task: &mut ActiveTask| {
                task.next_fire = compute_next_fire(
                    &task.config.fields.cron_expression,
                    &task.config.fields.timezone,
                );
                if let Some(ref next) = task.next_fire {
                    info!("Task '{}': next fire at {}", task.config.fields.name, next);
                }
            };

            match (is_continue, active_session) {
                // Continue mode, prior run still active: inject into it instead
                // of launching a second session.
                (true, Some(session_id)) => {
                    info!(
                        "Continue task '{}': injecting into active session {}",
                        task.config.fields.name, session_id
                    );
                    new_prompts.push(PendingPrompt {
                        session_id,
                        task_id,
                        prompt: task.config.fields.prompt.clone(),
                        // No spawn delay: the session is already registered.
                        send_at: Instant::now(),
                    });
                    advance(task);
                    continue;
                }
                // Fresh mode with a prior run still active: overlap policy skips.
                (false, Some(_)) => {
                    info!(
                        "Skipping task '{}': previous run still active",
                        task.config.fields.name
                    );
                    advance(task);
                    continue;
                }
                // No active run: launch. Continue mode resumes the prior session
                // (or launches fresh on the first run); fresh mode always starts
                // a brand-new session.
                (_, None) => {}
            }

            let last_session_id = if is_continue {
                self.last_session_by_task
                    .get(&task_id)
                    .copied()
                    .or(task.config.last_session_id)
            } else {
                None
            };

            let request_id = Uuid::new_v4();
            new_pending.push((
                request_id,
                PendingLaunch {
                    kind: PendingLaunchKind::ScheduledTask { task_id },
                    last_session_id,
                    prompt: task.config.fields.prompt.clone(),
                },
            ));
            to_fire.push(TaskToFire {
                request_id,
                config: task.config.clone(),
            });

            info!(
                "Firing task '{}' ({}) [{}{}]",
                task.config.fields.name,
                task_id,
                task.config.fields.session_mode,
                match last_session_id {
                    Some(id) => format!(", resuming {id}"),
                    None => String::new(),
                }
            );
            advance(task);
        }

        for (request_id, launch) in new_pending {
            self.pending_launches.insert(request_id, launch);
        }
        self.pending_prompts.extend(new_prompts);

        to_fire
    }

    pub fn register_continuation_launch(&mut self, continuation: &ReadyContinuation) -> Uuid {
        let request_id = Uuid::new_v4();
        self.pending_launches.insert(
            request_id,
            PendingLaunch {
                kind: PendingLaunchKind::Continuation {
                    continuation_id: continuation.id,
                    session_id: continuation.session_id,
                    prompt: continuation.prompt.clone(),
                },
                last_session_id: Some(continuation.session_id),
                prompt: continuation.prompt.clone(),
            },
        );
        request_id
    }

    /// Check if a request_id corresponds to a scheduler-owned launch.
    pub fn get_pending_launch_info(&self, request_id: &Uuid) -> Option<PendingLaunchInfo> {
        self.pending_launches
            .get(request_id)
            .map(|p| PendingLaunchInfo {
                last_session_id: p.last_session_id,
                kind: p.kind.clone(),
            })
    }

    /// Called after a scheduler-owned session is spawned successfully.
    pub fn on_session_spawned(
        &mut self,
        request_id: Uuid,
        session_id: Uuid,
    ) -> Option<PendingLaunchKind> {
        if let Some(pending) = self.pending_launches.remove(&request_id) {
            let kind = pending.kind;
            let PendingLaunchKind::ScheduledTask { task_id } = kind else {
                return Some(kind);
            };
            // Remember the session this task just launched so a later
            // continue-mode firing (within this connection) resumes it.
            self.last_session_by_task.insert(task_id, session_id);
            self.pending_prompts.push(PendingPrompt {
                session_id,
                task_id,
                prompt: pending.prompt,
                send_at: Instant::now() + PROMPT_DELAY,
            });

            let max_minutes = self
                .tasks
                .iter()
                .find(|t| t.config.id == task_id)
                .map(|t| t.config.fields.max_runtime_minutes)
                .unwrap_or(30);

            self.running.insert(
                session_id,
                RunningInfo {
                    task_id,
                    started_at: Instant::now(),
                    max_runtime: Duration::from_secs(max_minutes as u64 * 60),
                },
            );
            Some(PendingLaunchKind::ScheduledTask { task_id })
        } else {
            None
        }
    }

    /// Remove a pending launch that failed.
    pub fn clear_pending_launch(&mut self, request_id: &Uuid) {
        self.pending_launches.remove(request_id);
    }

    /// Return prompts that are ready to send. Removes them from the queue.
    /// Returns (session_id, task_id, prompt_content).
    pub fn ready_prompts(&mut self) -> Vec<(Uuid, Uuid, String)> {
        let now = Instant::now();
        let mut ready = Vec::new();
        self.pending_prompts.retain(|p| {
            if p.send_at <= now {
                ready.push((p.session_id, p.task_id, p.prompt.clone()));
                false
            } else {
                true
            }
        });
        ready
    }

    /// Called when a session exits. Returns RunningInfo if it was a scheduled session.
    pub fn on_session_exited(&mut self, session_id: &Uuid) -> Option<RunningInfo> {
        self.pending_prompts.retain(|p| p.session_id != *session_id);
        self.running.remove(session_id)
    }

    /// Return session_ids that have exceeded their max runtime.
    pub fn timed_out_sessions(&self) -> Vec<Uuid> {
        self.running
            .iter()
            .filter(|(_, info)| info.started_at.elapsed() >= info.max_runtime)
            .map(|(session_id, info)| {
                warn!(
                    "Scheduled session {} (task {}) exceeded max runtime of {}m",
                    session_id,
                    info.task_id,
                    info.max_runtime.as_secs() / 60
                );
                *session_id
            })
            .collect()
    }
}

fn compute_next_fire(cron_expr: &str, timezone: &str) -> Option<DateTime<Utc>> {
    let cron = match croner::Cron::from_str(cron_expr) {
        Ok(c) => c,
        Err(e) => {
            error!("Invalid cron expression '{}': {}", cron_expr, e);
            return None;
        }
    };

    // Canonicalize common abbreviations (PST/EST/…) to IANA names first;
    // chrono_tz only accepts IANA. Unknown values pass through and fail the
    // parse below, falling back to UTC. See issue #1064.
    let canonical = shared::timezone::canonicalize_timezone(timezone);
    let tz: chrono_tz::Tz = match canonical.parse() {
        Ok(t) => t,
        Err(_) => {
            error!(
                "Unrecognized timezone '{}' (resolved '{}'), falling back to UTC",
                timezone, canonical
            );
            chrono_tz::UTC
        }
    };

    let now = Utc::now().with_timezone(&tz);
    match cron.find_next_occurrence(&now, false) {
        Ok(next) => Some(next.with_timezone(&Utc)),
        Err(e) => {
            error!(
                "Failed to compute next fire time for '{}': {}",
                cron_expr, e
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::AgentType;

    fn make_task(name: &str, cron: &str) -> ScheduledTaskConfig {
        ScheduledTaskConfig {
            id: Uuid::new_v4(),
            fields: shared::ScheduledTaskFields {
                name: name.to_string(),
                cron_expression: cron.to_string(),
                timezone: "UTC".to_string(),
                working_directory: "/tmp".to_string(),
                prompt: "test prompt".to_string(),
                claude_args: vec![],
                agent_type: AgentType::Claude,
                max_runtime_minutes: 30,
                session_mode: shared::SessionMode::Fresh,
            },
            enabled: true,
            last_session_id: None,
        }
    }

    /// Force a task's `next_fire` into the past so `fire_due_tasks()` sees it due.
    fn make_due(scheduler: &mut Scheduler) {
        scheduler.tasks[0].next_fire = Some(Utc::now() - chrono::Duration::seconds(1));
    }

    #[test]
    fn update_tasks_computes_next_fire() {
        let mut scheduler = Scheduler::new();
        let task = make_task("test", "0 3 * * *");
        scheduler.update_tasks(vec![task]);
        assert_eq!(scheduler.tasks.len(), 1);
        assert!(scheduler.tasks[0].next_fire.is_some());
    }

    #[test]
    fn disabled_task_has_no_next_fire() {
        let mut scheduler = Scheduler::new();
        let mut task = make_task("test", "0 3 * * *");
        task.enabled = false;
        scheduler.update_tasks(vec![task]);
        assert_eq!(scheduler.tasks.len(), 1);
        assert!(scheduler.tasks[0].next_fire.is_none());
    }

    #[test]
    fn next_fire_duration_returns_none_when_empty() {
        let scheduler = Scheduler::new();
        assert!(scheduler.next_fire_duration().is_none());
    }

    #[test]
    fn compute_next_fire_with_utc() {
        let result = compute_next_fire("* * * * *", "UTC");
        assert!(result.is_some());
        assert!(result.unwrap() > Utc::now());
    }

    #[test]
    fn compute_next_fire_with_timezone() {
        let result = compute_next_fire("0 3 * * *", "America/New_York");
        assert!(result.is_some());
    }

    #[test]
    fn compute_next_fire_invalid_cron() {
        let result = compute_next_fire("invalid", "UTC");
        assert!(result.is_none());
    }

    #[test]
    fn compute_next_fire_invalid_timezone_falls_back_to_utc() {
        let result = compute_next_fire("0 3 * * *", "Invalid/Timezone");
        assert!(result.is_some());
    }

    #[test]
    fn compute_next_fire_accepts_abbreviation() {
        // "PST" used to silently fall back to UTC (issue #1064); it must now
        // resolve to the same instant as its IANA equivalent.
        let abbrev = compute_next_fire("0 3 * * *", "PST");
        let iana = compute_next_fire("0 3 * * *", "America/Los_Angeles");
        assert!(abbrev.is_some());
        // Compare at second granularity: croner carries the sub-second fraction
        // of each call's `Utc::now()` into the result, so the two instants differ
        // by microseconds even though they resolve to the same scheduled second.
        assert_eq!(abbrev.map(|d| d.timestamp()), iana.map(|d| d.timestamp()));
    }

    #[test]
    fn overlap_policy_skips_running_task() {
        let mut scheduler = Scheduler::new();
        let task = make_task("test", "* * * * *"); // fires every minute
        let task_id = task.id;
        scheduler.update_tasks(vec![task]);

        // Force next_fire into the past so the task would be due
        scheduler.tasks[0].next_fire = Some(Utc::now() - chrono::Duration::seconds(1));

        // Simulate a running session for this task
        scheduler.running.insert(
            Uuid::new_v4(),
            RunningInfo {
                task_id,
                started_at: Instant::now(),
                max_runtime: Duration::from_secs(1800),
            },
        );

        // Should not fire — overlap policy: skip
        let fired = scheduler.fire_due_tasks();
        assert!(fired.is_empty());
    }

    #[test]
    fn pending_prompt_lifecycle() {
        let mut scheduler = Scheduler::new();
        let task = make_task("test", "* * * * *");
        let task_id = task.id;
        scheduler.update_tasks(vec![task]);

        // Force next_fire into the past so fire_due_tasks() finds it due
        scheduler.tasks[0].next_fire = Some(Utc::now() - chrono::Duration::seconds(1));

        // Simulate firing and spawning
        let to_fire = scheduler.fire_due_tasks();
        assert!(!to_fire.is_empty());
        let request_id = to_fire[0].request_id;

        let session_id = Uuid::new_v4();
        scheduler.on_session_spawned(request_id, session_id);

        // Prompt not ready yet (PROMPT_DELAY hasn't elapsed)
        assert!(scheduler.ready_prompts().is_empty());
        assert!(scheduler.next_prompt_duration().is_some());

        // Verify running session is tracked
        assert!(scheduler.running.contains_key(&session_id));
        assert_eq!(scheduler.running[&session_id].task_id, task_id);
    }

    #[test]
    fn fresh_mode_fires_without_resume() {
        let mut scheduler = Scheduler::new();
        let task = make_task("test", "* * * * *"); // Fresh by default
        scheduler.update_tasks(vec![task]);
        make_due(&mut scheduler);

        let fired = scheduler.fire_due_tasks();
        assert_eq!(fired.len(), 1);
        let info = scheduler
            .get_pending_launch_info(&fired[0].request_id)
            .unwrap();
        // Fresh mode never resumes — a brand-new session each run.
        assert_eq!(info.last_session_id, None);
    }

    #[test]
    fn continue_mode_first_run_launches_fresh() {
        let mut scheduler = Scheduler::new();
        let mut task = make_task("test", "* * * * *");
        task.fields.session_mode = shared::SessionMode::Continue;
        scheduler.update_tasks(vec![task]);
        make_due(&mut scheduler);

        let fired = scheduler.fire_due_tasks();
        assert_eq!(fired.len(), 1);
        let info = scheduler
            .get_pending_launch_info(&fired[0].request_id)
            .unwrap();
        // No prior session → first run is fresh.
        assert_eq!(info.last_session_id, None);
    }

    #[test]
    fn continue_mode_dead_session_relaunches_with_resume() {
        let mut scheduler = Scheduler::new();
        let mut task = make_task("test", "* * * * *");
        task.fields.session_mode = shared::SessionMode::Continue;
        scheduler.update_tasks(vec![task]);

        // First run: fires fresh, spawns S1, then S1 exits.
        make_due(&mut scheduler);
        let first = scheduler.fire_due_tasks();
        assert_eq!(first.len(), 1);
        let s1 = Uuid::new_v4();
        scheduler.on_session_spawned(first[0].request_id, s1);
        assert!(scheduler.on_session_exited(&s1).is_some());

        // Second run: prior session is dead → relaunch resuming S1.
        make_due(&mut scheduler);
        let second = scheduler.fire_due_tasks();
        assert_eq!(second.len(), 1);
        let info = scheduler
            .get_pending_launch_info(&second[0].request_id)
            .unwrap();
        assert_eq!(info.last_session_id, Some(s1));
    }

    #[test]
    fn continue_mode_active_session_injects_instead_of_launching() {
        let mut scheduler = Scheduler::new();
        let mut task = make_task("test", "* * * * *");
        task.fields.session_mode = shared::SessionMode::Continue;
        let task_id = task.id;
        scheduler.update_tasks(vec![task]);

        // First run spawns S1 and leaves it running.
        make_due(&mut scheduler);
        let first = scheduler.fire_due_tasks();
        let s1 = Uuid::new_v4();
        scheduler.on_session_spawned(first[0].request_id, s1);

        // Second run while S1 is still active: no new launch, prompt injected.
        make_due(&mut scheduler);
        let second = scheduler.fire_due_tasks();
        assert!(second.is_empty(), "must not launch a second session");

        let ready = scheduler.ready_prompts();
        assert!(
            ready
                .iter()
                .any(|(sid, tid, _)| *sid == s1 && *tid == task_id),
            "prompt should be queued for the active session"
        );
    }

    fn continuation(reset_at: DateTime<Utc>) -> ContinuationConfig {
        continuation_with_reason(reset_at, shared::CONTINUATION_REASON_LIMIT)
    }

    fn continuation_with_reason(reset_at: DateTime<Utc>, reason: &str) -> ContinuationConfig {
        ContinuationConfig {
            id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            reset_at: reset_at.to_rfc3339(),
            prompt: "continue".to_string(),
            working_directory: Some("/home/test/project".to_string()),
            session_name: Some("project".to_string()),
            claude_args: vec!["--model".to_string(), "sonnet".to_string()],
            agent_type: shared::AgentType::Claude,
            reason: reason.to_string(),
        }
    }

    #[test]
    fn overloaded_continuation_fires_at_reset_with_no_skew() {
        let mut scheduler = Scheduler::new();
        // reset_at is "now": a limit continuation would wait the skew, but an
        // overload retry must be due immediately.
        let due = continuation_with_reason(Utc::now(), shared::CONTINUATION_REASON_OVERLOADED);
        let id = due.id;
        scheduler.update_continuations(vec![due]);
        let ready = scheduler.ready_continuations();
        assert_eq!(ready.len(), 1, "overloaded continuation should fire now");
        assert_eq!(ready[0].id, id);
    }

    #[test]
    fn limit_continuation_still_waits_skew_at_reset() {
        let mut scheduler = Scheduler::new();
        // Same "now" reset_at, but a limit continuation must NOT be due yet.
        scheduler.update_continuations(vec![continuation_with_reason(
            Utc::now(),
            shared::CONTINUATION_REASON_LIMIT,
        )]);
        assert!(scheduler.ready_continuations().is_empty());
        assert!(scheduler.next_continuation_duration() > Some(Duration::ZERO));
    }

    #[test]
    fn continuation_waits_two_minutes_after_reset() {
        let mut scheduler = Scheduler::new();
        scheduler.update_continuations(vec![continuation(
            Utc::now() - chrono::Duration::seconds(CONTINUATION_RESET_SKEW_SECS - 1),
        )]);

        assert!(scheduler.ready_continuations().is_empty());
        assert!(
            scheduler.next_continuation_duration() > Some(Duration::ZERO),
            "continuation should not be due until two minutes after reset_at"
        );
    }

    #[test]
    fn continuation_is_ready_after_two_minute_skew() {
        let mut scheduler = Scheduler::new();
        let due =
            continuation(Utc::now() - chrono::Duration::seconds(CONTINUATION_RESET_SKEW_SECS + 1));
        let continuation_id = due.id;
        scheduler.update_continuations(vec![due]);

        let ready = scheduler.ready_continuations();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, continuation_id);
        assert!(scheduler.next_continuation_duration().is_none());
    }

    #[test]
    fn continuation_relaunch_tracks_prompt_for_same_session() {
        let mut scheduler = Scheduler::new();
        let due =
            continuation(Utc::now() - chrono::Duration::seconds(CONTINUATION_RESET_SKEW_SECS + 1));
        let continuation_id = due.id;
        let session_id = due.session_id;
        scheduler.update_continuations(vec![due]);

        let ready = scheduler.ready_continuations();
        assert_eq!(ready.len(), 1);
        assert_eq!(
            ready[0].working_directory.as_deref(),
            Some("/home/test/project")
        );
        let request_id = scheduler.register_continuation_launch(&ready[0]);

        let launch = scheduler.get_pending_launch_info(&request_id).unwrap();
        assert_eq!(launch.last_session_id, Some(session_id));
        assert_eq!(
            launch.kind,
            PendingLaunchKind::Continuation {
                continuation_id,
                session_id,
                prompt: "continue".to_string(),
            }
        );
        assert_eq!(
            scheduler.on_session_spawned(request_id, session_id),
            Some(PendingLaunchKind::Continuation {
                continuation_id,
                session_id,
                prompt: "continue".to_string(),
            })
        );
    }

    #[test]
    fn session_exit_clears_running_state() {
        let mut scheduler = Scheduler::new();
        let session_id = Uuid::new_v4();
        let task_id = Uuid::new_v4();
        scheduler.running.insert(
            session_id,
            RunningInfo {
                task_id,
                started_at: Instant::now(),
                max_runtime: Duration::from_secs(1800),
            },
        );

        let info = scheduler.on_session_exited(&session_id);
        assert!(info.is_some());
        assert_eq!(info.unwrap().task_id, task_id);
        assert!(!scheduler.running.contains_key(&session_id));
    }
}
