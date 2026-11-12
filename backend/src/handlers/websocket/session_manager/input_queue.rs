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
//! Sequence allocation and sanitized pending-input insertion commit together.
//! Only a durable accepted input is handed to the live proxy; a database or
//! redaction failure is explicitly rejected rather than forwarded as plaintext.

use diesel::prelude::*;
use shared::{ReasoningEffort, SendMode, ServerToProxy};
use tracing::error;
use uuid::Uuid;

use crate::markers::{INPUT_SEQ_BUMP_FAILED, PENDING_INPUT_PERSIST_FAILED};

use crate::db::DbPool;
use crate::models::NewPendingInput;

use super::pending_queue::Sent;
use super::SessionManager;

/// Outcome of [`SessionManager::enqueue_input`].
pub(crate) struct EnqueueOutcome {
    /// Durable sequence number assigned (0 when input was rejected).
    pub seq: i64,
    /// Whether the message was handed off: to the live proxy, or held in
    /// memory for it to receive on reconnect.
    pub delivered: bool,
    /// Whether it was handed to a connected proxy's channel now. Receipt is
    /// only known from the proxy's `ProxyReceived` / `AgentAccepted` acks.
    pub live: bool,
    /// Whether the pending-input row was persisted. `false` ⇒ it won't replay
    /// if the proxy reconnects; logged as `PENDING_INPUT_PERSIST_FAILED`.
    pub persisted: bool,
    /// Whether the input is durably queued for replay: a real sequence was
    /// allocated and its sanitized row committed in the same transaction.
    pub durable: bool,
}

pub(crate) struct EnqueueInput {
    pub content: serde_json::Value,
    pub send_mode: Option<SendMode>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub client_msg_id: Option<Uuid>,
}

impl SessionManager {
    /// Commit a fenced, sanitized input and sequence before forwarding it.
    /// Failed persistence never becomes an unsequenced raw-content fallback.
    pub(crate) fn enqueue_input(
        &self,
        db_pool: &DbPool,
        session_key: &str,
        session_id: Uuid,
        input: EnqueueInput,
    ) -> EnqueueOutcome {
        use crate::schema::{pending_inputs, sessions};

        let EnqueueInput {
            content,
            send_mode,
            reasoning_effort,
            client_msg_id,
        } = input;

        let stored = (|| -> Result<(i64, serde_json::Value), crate::errors::AppError> {
            let mut conn = db_pool.get()?;
            conn.transaction(|conn| {
                crate::secrets::redaction::lock_generation(conn)?;
                let owner = sessions::table
                    .find(session_id)
                    .select(sessions::user_id)
                    .first::<Uuid>(conn)?;
                let rules = crate::secrets::redaction::snapshot_for_user(conn, owner)?;
                let mut content = content;
                rules.redact_agent_json(&mut content);
                let seq = diesel::update(sessions::table.find(session_id))
                    .set((
                        sessions::input_seq.eq(sessions::input_seq + 1),
                        sessions::last_messaged_at.eq(diesel::dsl::now),
                    ))
                    .returning(sessions::input_seq)
                    .get_result::<i64>(conn)
                    .map_err(|error| {
                        error!("{INPUT_SEQ_BUMP_FAILED} session={session_id}");
                        crate::errors::AppError::from(error)
                    })?;
                let row = NewPendingInput {
                    session_id,
                    seq_num: seq,
                    content: serde_json::to_string(&content).map_err(|_| {
                        crate::errors::AppError::Internal("Cannot encode sanitized input".into())
                    })?,
                    send_mode: send_mode.unwrap_or_default().as_str().to_owned(),
                    client_msg_id,
                    reasoning_effort: reasoning_effort.map(|effort| effort.as_str().to_owned()),
                };
                diesel::insert_into(pending_inputs::table)
                    .values(row)
                    .execute(conn)?;
                Ok((seq, content))
            })
        })();
        let (seq, content) = match stored {
            Ok(stored) => stored,
            Err(_) => {
                error!("{PENDING_INPUT_PERSIST_FAILED}: input rejected before delivery for session {session_id}");
                error!(
                    "{}: input requires a durable sanitized commit",
                    crate::markers::SECRET_REDACTION_FAILED
                );
                return EnqueueOutcome {
                    seq: 0,
                    delivered: false,
                    live: false,
                    persisted: false,
                    durable: false,
                };
            }
        };

        let (content, automated) = shared::PortalMessage::input_wire_parts(content);
        let sent = if seq > 0 {
            self.send_or_queue(
                session_key,
                ServerToProxy::SequencedInput {
                    session_id,
                    seq,
                    content,
                    automated,
                    send_mode,
                    reasoning_effort,
                    client_msg_id,
                },
            )
        } else {
            self.send_or_queue(
                session_key,
                ServerToProxy::AgentInput {
                    content,
                    automated,
                    send_mode,
                    reasoning_effort,
                },
            )
        };
        self.mark_turn_input_delivered(session_id);

        EnqueueOutcome {
            seq,
            delivered: true,
            live: sent == Sent::Live,
            persisted: true,
            durable: true,
        }
    }
}
