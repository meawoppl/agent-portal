use super::{ProxySender, SessionManager};
use crate::db::DbPool;
use diesel::prelude::*;
use shared::{ServerToClient, ServerToProxy};
use tracing::{error, info, warn};
use uuid::Uuid;

/// Replay pending inputs from the database to a reconnected proxy.
/// Returns the number of inputs replayed.
pub fn replay_pending_inputs_from_db(
    db_pool: &DbPool,
    session_id: Uuid,
    sender: &ProxySender,
) -> usize {
    use crate::schema::pending_inputs;

    let mut conn = match db_pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            error!(
                "Failed to get DB connection for pending inputs replay: {}",
                e
            );
            return 0;
        }
    };

    let pending: Vec<crate::models::PendingInput> = match pending_inputs::table
        .filter(pending_inputs::session_id.eq(session_id))
        .order(pending_inputs::seq_num.asc())
        .load(&mut conn)
    {
        Ok(inputs) => inputs,
        Err(e) => {
            error!(
                "Failed to load pending inputs for session {}: {}",
                session_id, e
            );
            return 0;
        }
    };

    let mut replayed = 0;
    for input in pending {
        let content: serde_json::Value = match serde_json::from_str(&input.content) {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse pending input content: {}", e);
                continue;
            }
        };

        let msg = ServerToProxy::SequencedInput {
            session_id,
            seq: input.seq_num,
            content,
            send_mode: None,
        };

        if sender.send(msg).is_ok() {
            replayed += 1;
        } else {
            warn!("Failed to send pending input to proxy, channel closed");
            break;
        }
    }

    if replayed > 0 {
        info!(
            "Replayed {} pending inputs to reconnected proxy for session {}",
            replayed, session_id
        );
    }

    replayed
}

/// Handle Claude output (both legacy ClaudeOutput and new SequencedOutput).
/// Broadcasts to web clients, deduplicates sequenced messages, stores in DB,
/// and sends acknowledgments.
pub fn handle_claude_output(
    session_manager: &SessionManager,
    session_key: &Option<String>,
    db_session_id: Option<Uuid>,
    db_pool: &DbPool,
    tx: &ProxySender,
    content: serde_json::Value,
    seq: Option<u64>,
) {
    // Deduplicate sequenced messages before broadcasting
    if let (Some(session_id), Some(seq_num)) = (db_session_id, seq) {
        let last_ack = session_manager
            .last_ack_seq
            .get(&session_id)
            .map(|v| *v)
            .unwrap_or(0);

        if seq_num <= last_ack {
            info!(
                "Skipping duplicate message seq={} (last_ack={})",
                seq_num, last_ack
            );
            let _ = tx.send(ServerToProxy::OutputAck {
                session_id,
                ack_seq: seq_num,
            });
            return;
        }
    }

    // Extract sender attribution for user-type messages (from last_input_sender, not injected into JSON)
    let role_str = content
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("assistant");
    let sender_info = if role_str == "user" {
        db_session_id.and_then(|sid| {
            session_manager
                .last_input_sender
                .remove(&sid)
                .map(|(_, v)| v)
        })
    } else {
        None
    };

    // Broadcast output to all web clients with sender metadata alongside content
    if let Some(ref key) = session_key {
        session_manager.broadcast_to_web_clients(
            key,
            ServerToClient::ClaudeOutput {
                content: content.clone(),
                sender_user_id: sender_info.as_ref().map(|(id, _)| id.to_string()),
                sender_name: sender_info.as_ref().map(|(_, name)| name.clone()),
            },
        );
    }

    // Store message and update last_activity in DB
    if let (Some(session_id), Ok(mut conn)) = (db_session_id, db_pool.get()) {
        use crate::schema::{messages, sessions};

        if let Ok(session) = sessions::table
            .find(session_id)
            .first::<crate::models::Session>(&mut conn)
        {
            let role = shared::MessageRole::from_type_str(
                content
                    .get("type")
                    .and_then(|t| t.as_str())
                    .unwrap_or("assistant"),
            );

            // Use actual sender's user_id for user messages, fall back to session owner
            let actual_user_id = sender_info
                .as_ref()
                .map(|(id, _)| *id)
                .unwrap_or(session.user_id);

            let new_message = crate::models::NewMessage {
                session_id,
                role: role.to_string(),
                content: content.to_string(),
                user_id: actual_user_id,
            };

            if let Err(e) = diesel::insert_into(messages::table)
                .values(&new_message)
                .execute(&mut conn)
            {
                error!("Failed to store message: {}", e);
            }

            if role == shared::MessageRole::Result {
                store_result_metadata(&mut conn, session_id, &content);
            }

            session_manager.queue_truncation(session_id);
        }

        // Update last_activity
        let _ = diesel::update(sessions::table.find(session_id))
            .set(sessions::last_activity.eq(diesel::dsl::now))
            .execute(&mut conn);

        // Update last_ack tracker and send acknowledgment
        if let Some(seq_num) = seq {
            session_manager
                .last_ack_seq
                .entry(session_id)
                .and_modify(|v| {
                    if seq_num > *v {
                        *v = seq_num;
                    }
                })
                .or_insert(seq_num);

            let _ = tx.send(ServerToProxy::OutputAck {
                session_id,
                ack_seq: seq_num,
            });
        }
    }
}

/// Extract and store cost and token usage from result messages.
/// Tries typed deserialization via `claude_codes::io::ResultMessage` first,
/// falls back to manual JSON extraction for forward compatibility.
fn store_result_metadata(
    conn: &mut diesel::PgConnection,
    session_id: Uuid,
    content: &serde_json::Value,
) {
    use crate::schema::sessions;

    // Try typed deserialization first
    if let Ok(result) = serde_json::from_value::<claude_codes::io::ResultMessage>(content.clone()) {
        if let Err(e) = diesel::update(sessions::table.find(session_id))
            .set(sessions::total_cost_usd.eq(result.total_cost_usd))
            .execute(conn)
        {
            error!("Failed to update session cost: {}", e);
        }

        if let Some(usage) = &result.usage {
            if let Err(e) = diesel::update(sessions::table.find(session_id))
                .set((
                    sessions::input_tokens.eq(usage.input_tokens as i64),
                    sessions::output_tokens.eq(usage.output_tokens as i64),
                    sessions::cache_creation_tokens.eq(usage.cache_creation_input_tokens as i64),
                    sessions::cache_read_tokens.eq(usage.cache_read_input_tokens as i64),
                ))
                .execute(conn)
            {
                error!("Failed to update session tokens: {}", e);
            }
        }
        return;
    }

    // Fallback: manual JSON extraction
    let cost = content.get("total_cost_usd").and_then(|c| c.as_f64());
    let usage = content.get("usage");
    let input_tokens = usage
        .and_then(|u| u.get("input_tokens"))
        .and_then(|t| t.as_i64());
    let output_tokens = usage
        .and_then(|u| u.get("output_tokens"))
        .and_then(|t| t.as_i64());
    let cache_creation = usage
        .and_then(|u| u.get("cache_creation_input_tokens"))
        .and_then(|t| t.as_i64());
    let cache_read = usage
        .and_then(|u| u.get("cache_read_input_tokens"))
        .and_then(|t| t.as_i64());

    if let Some(cost_val) = cost {
        if let Err(e) = diesel::update(sessions::table.find(session_id))
            .set(sessions::total_cost_usd.eq(cost_val))
            .execute(conn)
        {
            error!("Failed to update session cost: {}", e);
        }
    }

    if input_tokens.is_some()
        || output_tokens.is_some()
        || cache_creation.is_some()
        || cache_read.is_some()
    {
        if let Err(e) = diesel::update(sessions::table.find(session_id))
            .set((
                sessions::input_tokens.eq(input_tokens.unwrap_or(0)),
                sessions::output_tokens.eq(output_tokens.unwrap_or(0)),
                sessions::cache_creation_tokens.eq(cache_creation.unwrap_or(0)),
                sessions::cache_read_tokens.eq(cache_read.unwrap_or(0)),
            ))
            .execute(conn)
        {
            error!("Failed to update session tokens: {}", e);
        }
    }
}
