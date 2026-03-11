use chrono::{DateTime, Utc};
use shared::ScheduledTaskConfig;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use uuid::Uuid;

/// Delay before sending the prompt after session spawn.
/// Gives the proxy time to connect and register the session row.
const PROMPT_DELAY: Duration = Duration::from_secs(5);

struct ActiveTask {
    config: ScheduledTaskConfig,
    next_fire: Option<DateTime<Utc>>,
}

pub struct PendingLaunch {
    pub task_id: Uuid,
    pub last_session_id: Option<Uuid>,
    prompt: String,
}

struct PendingPrompt {
    session_id: Uuid,
    task_id: Uuid,
    prompt: String,
    send_at: Instant,
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
}

impl Scheduler {
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            pending_launches: HashMap::new(),
            running: HashMap::new(),
            pending_prompts: Vec::new(),
        }
    }

    /// Replace task configs from ScheduleSync. Preserves running session state.
    pub fn update_tasks(&mut self, configs: Vec<ScheduledTaskConfig>) {
        let running_task_ids: HashSet<Uuid> = self.running.values().map(|r| r.task_id).collect();

        self.tasks = configs
            .into_iter()
            .map(|config| {
                let next_fire = if config.enabled && !running_task_ids.contains(&config.id) {
                    compute_next_fire(&config.cron_expression, &config.timezone)
                } else {
                    None
                };
                if let Some(ref next) = next_fire {
                    info!("Task '{}': next fire at {}", config.name, next);
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

    /// Find and return tasks that are due to fire. Advances next_fire times.
    pub fn fire_due_tasks(&mut self) -> Vec<TaskToFire> {
        let now = Utc::now();
        let running_task_ids: HashSet<Uuid> = self.running.values().map(|r| r.task_id).collect();

        let mut to_fire = Vec::new();
        let mut new_pending = Vec::new();

        for task in &mut self.tasks {
            let Some(next) = task.next_fire else {
                continue;
            };
            if next > now {
                continue;
            }

            if running_task_ids.contains(&task.config.id) {
                info!(
                    "Skipping task '{}': previous run still active",
                    task.config.name
                );
                task.next_fire =
                    compute_next_fire(&task.config.cron_expression, &task.config.timezone);
                continue;
            }

            let request_id = Uuid::new_v4();
            new_pending.push((
                request_id,
                PendingLaunch {
                    task_id: task.config.id,
                    last_session_id: task.config.last_session_id,
                    prompt: task.config.prompt.clone(),
                },
            ));
            to_fire.push(TaskToFire {
                request_id,
                config: task.config.clone(),
            });

            info!("Firing task '{}' ({})", task.config.name, task.config.id);
            task.next_fire = compute_next_fire(&task.config.cron_expression, &task.config.timezone);
            if let Some(ref next) = task.next_fire {
                info!("Task '{}': next fire at {}", task.config.name, next);
            }
        }

        for (request_id, launch) in new_pending {
            self.pending_launches.insert(request_id, launch);
        }

        to_fire
    }

    /// Check if a request_id corresponds to a scheduled launch.
    /// Returns (last_session_id, task_id) if so.
    pub fn get_pending_launch_info(&self, request_id: &Uuid) -> Option<(Option<Uuid>, Uuid)> {
        self.pending_launches
            .get(request_id)
            .map(|p| (p.last_session_id, p.task_id))
    }

    /// Called after a scheduled session is spawned successfully.
    pub fn on_session_spawned(&mut self, request_id: Uuid, session_id: Uuid) {
        if let Some(pending) = self.pending_launches.remove(&request_id) {
            self.pending_prompts.push(PendingPrompt {
                session_id,
                task_id: pending.task_id,
                prompt: pending.prompt,
                send_at: Instant::now() + PROMPT_DELAY,
            });

            let max_minutes = self
                .tasks
                .iter()
                .find(|t| t.config.id == pending.task_id)
                .map(|t| t.config.max_runtime_minutes)
                .unwrap_or(30);

            self.running.insert(
                session_id,
                RunningInfo {
                    task_id: pending.task_id,
                    started_at: Instant::now(),
                    max_runtime: Duration::from_secs(max_minutes as u64 * 60),
                },
            );
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

    let tz: chrono_tz::Tz = match timezone.parse() {
        Ok(t) => t,
        Err(_) => {
            error!("Invalid timezone '{}', falling back to UTC", timezone);
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
            name: name.to_string(),
            cron_expression: cron.to_string(),
            timezone: "UTC".to_string(),
            working_directory: "/tmp".to_string(),
            prompt: "test prompt".to_string(),
            claude_args: vec![],
            agent_type: AgentType::Claude,
            enabled: true,
            max_runtime_minutes: 30,
            last_session_id: None,
        }
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
