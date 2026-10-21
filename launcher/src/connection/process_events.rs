//! Local lifecycle cleanup happens even if its report cannot reach the hub.
use super::supervisor::Delivery;
use crate::process_manager::{ProcessManager, SessionExited};
use crate::scheduler::Scheduler;
use shared::LauncherToServer;

pub(super) fn exited(
    process_manager: &mut ProcessManager,
    scheduler: &mut Scheduler,
    exited: SessionExited,
) -> Delivery {
    tracing::info!(session_id = %exited.session_id, exit_code = ?exited.exit_code, "Session exited");
    process_manager.remove_finished(&exited.session_id);
    let mut frames = vec![LauncherToServer::SessionExited {
        session_id: exited.session_id,
        exit_code: exited.exit_code,
        reason: exited.reason,
    }];
    if let Some(run) = scheduler.on_session_exited(&exited.session_id) {
        frames.push(LauncherToServer::ScheduledRunCompleted {
            task_id: run.task_id,
            session_id: exited.session_id,
            exit_code: exited.exit_code,
            duration_secs: run.started_at.elapsed().as_secs(),
        });
    }
    Delivery {
        label: format!("session exit {}", exited.session_id),
        frames,
    }
}
