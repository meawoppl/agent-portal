//! Connection-owned delivery state. Only work never attempted on a socket may
//! be retried without a backend delivery identity. Each batch is one logical
//! operation (for example InjectInput followed by ContinuationFired).
use shared::{ContinuationConfig, LauncherToServer, ScheduledTaskConfig, ScheduledTaskFields};
use std::collections::{HashMap, VecDeque};
use uuid::Uuid;

pub(super) enum WorkIdentity {
    Notification,
    Task {
        id: Uuid,
        fields: ScheduledTaskFields,
    },
    Continuation(ContinuationConfig),
}

pub(super) struct Delivery {
    pub identity: WorkIdentity,
    pub label: String,
    pub frames: Vec<LauncherToServer>,
}

#[derive(Default)]
pub(super) struct LauncherConnectionSupervisor {
    pending: VecDeque<Delivery>,
    tasks: Option<HashMap<Uuid, ScheduledTaskConfig>>,
    continuations: Option<HashMap<Uuid, ContinuationConfig>>,
    cancelled_launches: Vec<Uuid>,
}

impl LauncherConnectionSupervisor {
    /// RegisterAck is not a schedule snapshot. Gate queued work until its
    /// authoritative sync arrives, including empty snapshots after cancellation.
    pub fn reconnect(&mut self) {
        self.tasks = None;
        self.continuations = None;
    }

    pub fn observe(&mut self, message: &shared::ServerToLauncher) {
        match message {
            shared::ServerToLauncher::ScheduleSync { tasks } => {
                self.tasks = Some(tasks.iter().map(|task| (task.id, task.clone())).collect());
            }
            shared::ServerToLauncher::ContinuationSync { continuations } => {
                self.continuations =
                    Some(continuations.iter().map(|c| (c.id, c.clone())).collect());
            }
            _ => {}
        }
    }

    pub fn schedule_ready(&self) -> bool {
        self.tasks.is_some()
    }
    pub fn continuations_ready(&self) -> bool {
        self.continuations.is_some()
    }
    pub fn cancelled_launches(&mut self) -> Vec<Uuid> {
        std::mem::take(&mut self.cancelled_launches)
    }

    fn authorized(&self, identity: &WorkIdentity) -> Option<bool> {
        match identity {
            WorkIdentity::Notification => Some(true),
            WorkIdentity::Task { id, fields } => self.tasks.as_ref().map(|tasks| {
                tasks
                    .get(id)
                    .is_some_and(|task| task.enabled && task.fields == *fields)
            }),
            WorkIdentity::Continuation(original) => self
                .continuations
                .as_ref()
                .map(|continuations| continuations.get(&original.id) == Some(original)),
        }
    }

    pub fn extend(&mut self, deliveries: impl IntoIterator<Item = Delivery>) {
        self.pending.extend(deliveries);
    }

    /// A failed send may have reached the peer. Never resend that operation,
    /// nor its remaining frames (which may falsely acknowledge an injection).
    /// Preserve all later, untouched operations for the next connection.
    pub async fn flush<E: std::fmt::Display>(
        &mut self,
        mut send: impl AsyncFnMut(LauncherToServer) -> Result<(), E>,
    ) -> bool {
        while let Some(front) = self.pending.front() {
            let Some(authorized) = self.authorized(&front.identity) else {
                break;
            };
            let Some(delivery) = self.pending.pop_front() else {
                break;
            };
            if !authorized {
                tracing::info!(operation = %delivery.label, "Discarding queued work removed, disabled or edited in authoritative sync");
                for frame in delivery.frames {
                    if let LauncherToServer::RequestLaunch { request_id, .. } = frame {
                        self.cancelled_launches.push(request_id);
                    }
                }
                continue;
            }
            for frame in delivery.frames {
                if let Err(error) = send(frame).await {
                    tracing::error!(
                        operation = %delivery.label, %error,
                        untouched_operations = self.pending.len(),
                        "LAUNCHER_DELIVERY_AMBIGUOUS: delivery may have reached the server; not replaying this operation; reconnecting with untouched work retained"
                    );
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn delivery(id: Uuid, frames: usize) -> Delivery {
        Delivery {
            label: id.to_string(),
            identity: WorkIdentity::Notification,
            frames: (0..frames)
                .map(|_| LauncherToServer::InjectInput {
                    session_id: id,
                    content: "prompt".into(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn failure_discards_ambiguous_operation_but_retains_untouched_tail() {
        let mut supervisor = LauncherConnectionSupervisor::default();
        let first = Uuid::new_v4();
        let ambiguous = Uuid::new_v4();
        let untouched = Uuid::new_v4();
        supervisor.extend([
            delivery(first, 1),
            delivery(ambiguous, 2),
            delivery(untouched, 1),
        ]);
        let mut attempts = Vec::new();
        assert!(
            !supervisor
                .flush(async |frame| {
                    let LauncherToServer::InjectInput { session_id, .. } = frame else {
                        panic!()
                    };
                    attempts.push(session_id);
                    if session_id == ambiguous {
                        Err(std::io::Error::other("socket lost"))
                    } else {
                        Ok(())
                    }
                })
                .await
        );
        assert_eq!(attempts, [first, ambiguous]);
        assert!(
            supervisor
                .flush(async |frame| {
                    let LauncherToServer::InjectInput { session_id, .. } = frame else {
                        panic!()
                    };
                    attempts.push(session_id);
                    Ok::<_, std::io::Error>(())
                })
                .await
        );
        assert_eq!(attempts, [first, ambiguous, untouched]);
        assert!(supervisor.pending.is_empty());
    }
    #[tokio::test]
    async fn failure_after_first_frame_never_replays_partial_operation() {
        let mut supervisor = LauncherConnectionSupervisor::default();
        let partial = Uuid::new_v4();
        let tail = Uuid::new_v4();
        supervisor.extend([delivery(partial, 3), delivery(tail, 1)]);
        let mut attempts = 0;
        assert!(
            !supervisor
                .flush(async |_| {
                    attempts += 1;
                    if attempts == 2 {
                        Err(std::io::Error::other("ack write failed"))
                    } else {
                        Ok(())
                    }
                })
                .await
        );
        assert_eq!(attempts, 2);
        assert!(
            supervisor
                .flush(async |frame| {
                    let LauncherToServer::InjectInput { session_id, .. } = frame else {
                        panic!()
                    };
                    assert_eq!(session_id, tail);
                    Ok::<_, std::io::Error>(())
                })
                .await
        );
    }
    fn task() -> ScheduledTaskConfig {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(), "name": "test", "cron_expression": "* * * * *",
            "working_directory": "/tmp", "prompt": "original", "enabled": true
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn reconnect_waits_for_authoritative_sync_and_discards_cancelled_or_edited_work() {
        for mutation in ["deleted", "disabled", "edited", "unchanged"] {
            let original = task();
            let mut supervisor = LauncherConnectionSupervisor::default();
            supervisor.observe(&shared::ServerToLauncher::ScheduleSync {
                tasks: vec![original.clone()],
            });
            let mut queued = delivery(original.id, 1);
            queued.identity = WorkIdentity::Task {
                id: original.id,
                fields: original.fields.clone(),
            };
            supervisor.extend([queued]);
            supervisor.reconnect();
            assert!(
                supervisor
                    .flush(async |_| -> Result<(), std::io::Error> { panic!("must wait for sync") })
                    .await
            );
            assert_eq!(supervisor.pending.len(), 1);
            let mut current = original.clone();
            if mutation == "disabled" {
                current.enabled = false;
            }
            if mutation == "edited" {
                current.fields.prompt = "replacement".into();
            }
            supervisor.observe(&shared::ServerToLauncher::ScheduleSync {
                tasks: if mutation == "deleted" {
                    vec![]
                } else {
                    vec![current]
                },
            });
            let mut sent = 0;
            assert!(
                supervisor
                    .flush(async |_| {
                        sent += 1;
                        Ok::<_, std::io::Error>(())
                    })
                    .await
            );
            assert_eq!(sent, usize::from(mutation == "unchanged"), "{mutation}");
            assert!(supervisor.pending.is_empty());
        }
    }

    #[tokio::test]
    async fn cancelled_continuation_cannot_run_after_reconnect() {
        let config: ContinuationConfig = serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(), "session_id": Uuid::new_v4(),
            "reset_at": "2026-01-01T00:00:00Z", "prompt": "continue"
        }))
        .unwrap();
        let mut supervisor = LauncherConnectionSupervisor::default();
        let mut queued = delivery(config.session_id, 2);
        queued.identity = WorkIdentity::Continuation(config);
        supervisor.extend([queued]);
        supervisor.reconnect();
        assert!(
            supervisor
                .flush(async |_| -> Result<(), std::io::Error> { panic!("must wait for sync") })
                .await
        );
        supervisor.observe(&shared::ServerToLauncher::ContinuationSync {
            continuations: vec![],
        });
        assert!(
            supervisor
                .flush(async |_| -> Result<(), std::io::Error> {
                    panic!("cancelled continuation must not send")
                })
                .await
        );
        assert!(supervisor.pending.is_empty());
    }
    #[tokio::test]
    async fn removed_task_releases_its_pending_launch_request() {
        let original = task();
        let request_id = Uuid::new_v4();
        let mut supervisor = LauncherConnectionSupervisor::default();
        supervisor.extend([Delivery {
            label: "cancelled launch".into(),
            identity: WorkIdentity::Task {
                id: original.id,
                fields: original.fields.clone(),
            },
            frames: vec![LauncherToServer::RequestLaunch {
                request_id,
                working_directory: original.fields.launch.working_directory,
                session_name: None,
                claude_args: vec![],
                agent_type: shared::AgentType::Claude,
                scheduled_task_id: Some(original.id),
                last_session_id: None,
                continuation_id: None,
                worktree: shared::WorktreeMode::None,
            }],
        }]);
        supervisor.observe(&shared::ServerToLauncher::ScheduleSync { tasks: vec![] });
        assert!(
            supervisor
                .flush(async |_| -> Result<(), std::io::Error> {
                    panic!("removed task must not launch")
                })
                .await
        );
        assert_eq!(supervisor.cancelled_launches(), [request_id]);
        assert!(supervisor.cancelled_launches().is_empty());
    }
}
