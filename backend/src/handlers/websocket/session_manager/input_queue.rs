//! Shared enqueue path for session input.
//!
//! Both the web-client input handler (`handle_web_input`) and the
//! agent-messaging send endpoint (`agent_comms::send_agent_message`) route a
//! message into a session through here, so they can't drift in their
//! seq/persist/deliver semantics. (They previously had two copies of this
//! logic with *different* error handling — one swallowed DB errors, one `?`'d
//! them — which is why a single write fault 500'd one path and was silent on
//! the other.)
//!
//! Semantics: bump the per-session input sequence, best-effort persist a
//! pending-input row (the reconnect-replay buffer), then deliver to the live
//! proxy. DB hiccups are logged, never fatal — delivering to the live agent is
//! what matters, and persistence only governs replay if the proxy reconnects.

use diesel::prelude::*;
use shared::{SendMode, ServerToProxy};
use tracing::error;
use uuid::Uuid;

use crate::markers::{INPUT_SEQ_BUMP_FAILED, PENDING_INPUT_PERSIST_FAILED};

use crate::db::DbPool;
use crate::models::NewPendingInput;

use super::{SessionId, SessionManager};

/// Outcome of [`SessionManager::enqueue_input`].
pub(crate) struct EnqueueOutcome {
    /// Sequence number assigned (0 if no DB connection was available).
    pub seq: i64,
    /// Whether the message reached a live proxy (vs. queued for reconnect).
    pub delivered: bool,
    /// Whether the pending-input row was persisted. `false` ⇒ it won't replay
    /// if the proxy reconnects; logged as `PENDING_INPUT_PERSIST_FAILED`.
    pub persisted: bool,
}

impl SessionManager {
    /// Bump the session's input sequence, best-effort persist a pending-input
    /// row, and forward the message to the live proxy (queued if disconnected).
    pub(crate) fn enqueue_input(
        &self,
        db_pool: &DbPool,
        session_key: &SessionId,
        session_id: Uuid,
        content: serde_json::Value,
        send_mode: Option<SendMode>,
        // Browser-assigned delivery-tracking id (#939); forwarded to the proxy
        // on `SequencedInput` so it can echo per-stage `InputProgressAck`s.
        // `None` for non-browser inputs (inter-agent, replay).
        client_msg_id: Option<Uuid>,
    ) -> EnqueueOutcome {
        use crate::schema::{pending_inputs, sessions};

        let mut persisted = false;
        let seq = match db_pool.get() {
            Ok(mut conn) => {
                let next_seq: i64 = match diesel::update(sessions::table.find(session_id))
                    .set(sessions::input_seq.eq(sessions::input_seq + 1))
                    .returning(sessions::input_seq)
                    .get_result(&mut conn)
                {
                    Ok(s) => s,
                    Err(e) => {
                        error!("{INPUT_SEQ_BUMP_FAILED} session={}: {}", session_id, e);
                        1
                    }
                };

                let new_input = NewPendingInput {
                    session_id,
                    seq_num: next_seq,
                    content: serde_json::to_string(&content).unwrap_or_default(),
                    send_mode: send_mode.as_ref().map(|m| m.as_str().to_string()),
                    client_msg_id,
                };
                match diesel::insert_into(pending_inputs::table)
                    .values(&new_input)
                    .execute(&mut conn)
                {
                    Ok(_) => persisted = true,
                    Err(e) => {
                        error!(
                            "{PENDING_INPUT_PERSIST_FAILED} session={}: {}",
                            session_id, e
                        )
                    }
                }
                next_seq
            }
            Err(e) => {
                // No DB connection ⇒ the pending-input row is never written and
                // the sequence is never assigned: same durable-loss outcome as a
                // failed INSERT, so it carries the same marker (see markers.rs).
                error!(
                    "{PENDING_INPUT_PERSIST_FAILED} no DB connection to enqueue input for session {}: {}",
                    session_id, e
                );
                0
            }
        };

        let delivered = if seq > 0 {
            self.send_to_session(
                session_key,
                ServerToProxy::SequencedInput {
                    session_id,
                    seq,
                    content,
                    send_mode,
                    client_msg_id,
                },
            )
        } else {
            self.send_to_session(
                session_key,
                ServerToProxy::AgentInput { content, send_mode },
            )
        };

        EnqueueOutcome {
            seq,
            delivered,
            persisted,
        }
    }
}
