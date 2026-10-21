//! Connection-owned delivery state. Only work never attempted on a socket may
//! be retried without a backend delivery identity. Each batch is one logical
//! operation (for example InjectInput followed by ContinuationFired).
use shared::LauncherToServer;
use std::collections::VecDeque;

pub(super) struct Delivery {
    pub label: String,
    pub frames: Vec<LauncherToServer>,
}

#[derive(Default)]
pub(super) struct LauncherConnectionSupervisor {
    pending: VecDeque<Delivery>,
}

impl LauncherConnectionSupervisor {
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
        while let Some(delivery) = self.pending.pop_front() {
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
}
