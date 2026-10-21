use super::supervisor::Delivery;
use crate::scheduler::Scheduler;
use shared::LauncherToServer;
use tracing::{info, warn};
use uuid::Uuid;

pub(super) fn due_tasks(scheduler: &mut Scheduler) -> Vec<Delivery> {
    let mut deliveries = Vec::new();

    for task_to_fire in scheduler.fire_due_tasks() {
        let mut frames = Vec::new();
        info!(
            "Firing scheduled task '{}' ({})",
            task_to_fire.config.fields.name, task_to_fire.config.id
        );
        let msg = LauncherToServer::RequestLaunch {
            request_id: task_to_fire.request_id,
            working_directory: task_to_fire.config.fields.launch.working_directory.clone(),
            session_name: task_to_fire
                .config
                .fields
                .launch
                .session_name
                .clone()
                .or_else(|| Some(task_to_fire.config.fields.name.clone())),
            claude_args: task_to_fire.config.fields.launch.claude_args.clone(),
            agent_type: task_to_fire.config.fields.launch.agent_type,
            scheduled_task_id: Some(task_to_fire.config.id),
            last_session_id: task_to_fire.config.last_session_id,
            continuation_id: None,
            worktree: task_to_fire.config.fields.launch.worktree.clone(),
        };
        frames.push(msg);
        deliveries.push(Delivery {
            label: format!("scheduled task {}", task_to_fire.config.id),
            frames,
        });
    }

    deliveries
}

pub(super) fn due_prompts(scheduler: &mut Scheduler) -> Vec<Delivery> {
    let mut deliveries = Vec::new();

    for (session_id, task_id, content) in scheduler.ready_prompts() {
        let mut frames = Vec::new();
        info!(
            "Injecting prompt for task {} into session {}",
            task_id, session_id
        );

        let started = LauncherToServer::ScheduledRunStarted {
            task_id,
            session_id,
        };
        frames.push(started);

        let inject = LauncherToServer::InjectInput {
            session_id,
            content,
        };
        frames.push(inject);
        deliveries.push(Delivery {
            label: format!("scheduled prompt {task_id} for {session_id}"),
            frames,
        });
    }

    deliveries
}

pub(super) fn due_continuations(scheduler: &mut Scheduler, running: &[Uuid]) -> Vec<Delivery> {
    let mut deliveries = Vec::new();

    for continuation in scheduler.ready_continuations() {
        let mut frames = Vec::new();
        if running.contains(&continuation.session_id) {
            info!(
                "Injecting continuation {} into still-running session {}",
                continuation.id, continuation.session_id
            );
            let inject = LauncherToServer::InjectInput {
                session_id: continuation.session_id,
                content: continuation.prompt.clone(),
            };
            frames.push(inject);
            let fired = LauncherToServer::ContinuationFired {
                continuation_id: continuation.id,
                session_id: continuation.session_id,
            };
            frames.push(fired);
        } else if let Some(working_directory) = continuation.working_directory.clone() {
            info!(
                "Relaunching session {} for continuation {}",
                continuation.session_id, continuation.id
            );
            let request_id = scheduler.register_continuation_launch(&continuation);
            let relaunch = LauncherToServer::RequestLaunch {
                request_id,
                working_directory,
                session_name: continuation.session_name.clone(),
                claude_args: continuation.claude_args.clone(),
                agent_type: continuation.agent_type,
                scheduled_task_id: None,
                last_session_id: Some(continuation.session_id),
                continuation_id: Some(continuation.id),
                worktree: shared::WorktreeMode::None,
            };
            frames.push(relaunch);
        } else {
            let reason = "local agent process was no longer running and this launcher did not receive resume metadata".to_string();
            warn!(
                "Dropping continuation {} for session {}: {}",
                continuation.id, continuation.session_id, reason
            );
            let dropped = LauncherToServer::ContinuationDropped {
                continuation_id: continuation.id,
                session_id: continuation.session_id,
                reason,
            };
            frames.push(dropped);
        }
        deliveries.push(Delivery {
            label: format!("continuation {}", continuation.id),
            frames,
        });
    }

    deliveries
}
