use super::{ProxySender, SessionManager};
use crate::db::DbPool;
use diesel::prelude::*;
use serde::Deserialize;
use shared::{
    AgentType, MessageOrigin, PortalContent, PortalMessage, SendMode, ServerToClient, ServerToProxy,
};
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
            send_mode: input.send_mode.as_deref().and_then(parse_send_mode),
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

fn parse_send_mode(value: &str) -> Option<SendMode> {
    match value {
        "normal" => Some(SendMode::Normal),
        "wiggum" => Some(SendMode::Wiggum),
        other => {
            warn!("Ignoring unknown pending input send_mode: {}", other);
            None
        }
    }
}

/// Handle Claude output (both legacy ClaudeOutput and new SequencedOutput).
/// Broadcasts to web clients, deduplicates sequenced messages, stores in DB,
/// and sends acknowledgments.
#[allow(clippy::too_many_arguments)]
pub fn handle_claude_output(
    session_manager: &SessionManager,
    session_key: &Option<String>,
    db_session_id: Option<Uuid>,
    db_pool: &DbPool,
    tx: &ProxySender,
    content: serde_json::Value,
    seq: Option<u64>,
    image_store: &crate::handlers::images::ImageStore,
    agent_type: AgentType,
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

    // Validate that content roundtrips through ClaudeOutput parsing (frontend depends on this)
    match shared::ClaudeOutput::deserialize(&content) {
        Ok(parsed) => {
            if let shared::ClaudeOutput::System(ref sys) = parsed {
                if sys.is_task_started() && sys.as_task_started().is_none() {
                    warn!(
                        "task_started message matched subtype but failed struct parse: {}",
                        content
                    );
                }
                if sys.is_task_progress() && sys.as_task_progress().is_none() {
                    warn!(
                        "task_progress message matched subtype but failed struct parse: {}",
                        content
                    );
                }
                if sys.is_task_notification() {
                    match sys.as_task_notification() {
                        Some(notif) => {
                            // A sub-agent (Task tool) just finished. Its tokens
                            // aren't in the parent's `result.usage`, so fold the
                            // completed task's cumulative `total_tokens` into the
                            // session's running sub-agent total (see
                            // `SessionManager::subagent_tokens`). `task_notification`
                            // fires once per task, so summing is exact.
                            if let (Some(sid), Some(usage)) = (db_session_id, notif.usage.as_ref())
                            {
                                session_manager
                                    .subagent_tokens
                                    .entry(sid)
                                    .and_modify(|v| *v += usage.total_tokens as i64)
                                    .or_insert(usage.total_tokens as i64);
                            }
                        }
                        None => warn!(
                            "task_notification message matched subtype but failed struct parse: {}",
                            content
                        ),
                    }
                }
            }
        }
        Err(e) => {
            warn!(
                "ClaudeOutput parse failed for message: {} — raw: {}",
                e, content
            );
        }
    }

    // Resolve the session owner ONCE for both consumers below: image
    // extraction (auth check on `/api/images/{id}`, #786) and the message
    // insert (fallback `user_id`). A single cheap owner-only `.select`
    // replaces the previous pair of per-message session lookups (#977).
    let session_user_id: Option<Uuid> = db_session_id.and_then(|sid| {
        use crate::schema::sessions;
        let mut conn = db_pool.get().ok()?;
        sessions::table
            .find(sid)
            .select(sessions::user_id)
            .first::<Uuid>(&mut conn)
            .ok()
    });

    // Extract base64 images from portal messages and replace with URLs.
    // This keeps WebSocket messages small — browsers fetch images via HTTP.
    //
    // If we couldn't resolve a user_id we *skip* image extraction rather
    // than silently drop ownership: the original base64 just stays inline
    // in the broadcast (slower but correct), and nothing un-owned ever
    // lands in the cache.
    let content = match session_user_id {
        Some(uid) => extract_portal_images(content, image_store, uid, db_session_id),
        None => content,
    };
    let normalized = normalize_output_content(content);
    let content = normalized.content;
    let origin = normalized.origin.clone();

    // Insert the message FIRST and recover the server-assigned `created_at`
    // so the live broadcast carries the same timestamp the historical-read
    // path would surface (closes #784 — silent data-loss on reconnect when
    // the frontend used `Date.now()` as the replay watermark). Doing the
    // insert before the broadcast is the only way to make the persisted
    // row's `created_at` available to the wire frame; if the insert fails
    // we fall back to broadcasting without a timestamp rather than
    // silently dropping the message (the frontend keeps its prior
    // watermark and a future message will heal it).
    let mut row_created_at: Option<String> = None;
    if let (Some(session_id), Ok(mut conn)) = (db_session_id, db_pool.get()) {
        use crate::schema::{messages, sessions};

        // Only insert when the owner lookup above resolved — same gating the
        // previous per-insert `Session` load provided (no row, no insert).
        if let Some(owner_user_id) = session_user_id {
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
                .unwrap_or(owner_user_id);

            let new_message = crate::models::NewMessage {
                session_id,
                role: role.to_string(),
                content: content.to_string(),
                user_id: actual_user_id,
                agent_type: agent_type.as_str().to_string(),
                provenance_kind: normalized.provenance_kind.clone(),
                provenance_session_id: normalized.provenance_session_id,
                provenance_agent_type: normalized.provenance_agent_type.clone(),
            };

            match diesel::insert_into(messages::table)
                .values(&new_message)
                .get_result::<crate::models::Message>(&mut conn)
            {
                Ok(inserted) => {
                    // Format matches `replay_history`'s parser
                    // (`%Y-%m-%dT%H:%M:%S%.f`) and the frontend's
                    // `last_message_timestamp` watermark shape.
                    row_created_at = Some(
                        inserted
                            .created_at
                            .format("%Y-%m-%dT%H:%M:%S%.6f")
                            .to_string(),
                    );
                }
                Err(e) => {
                    error!("Failed to store message: {}", e);
                }
            }

            if role == shared::MessageRole::Result {
                let subagent_tokens = session_manager
                    .subagent_tokens
                    .get(&session_id)
                    .map(|v| *v)
                    .unwrap_or(0);
                store_result_metadata(&mut conn, session_id, &content, subagent_tokens);
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

    // Broadcast output to all web clients with sender metadata + server
    // timestamp alongside content. The `created_at` here is the same
    // timestamp the message landed in the DB with; the frontend uses it
    // as the watermark for reconnect replay (closes #784).
    if let Some(ref key) = session_key {
        session_manager.broadcast_to_web_clients(
            key,
            ServerToClient::AgentOutput {
                content,
                sender_user_id: sender_info.as_ref().map(|(id, _)| id.to_string()),
                sender_name: sender_info.as_ref().map(|(_, name)| name.clone()),
                agent_type,
                created_at: row_created_at,
                origin,
            },
        );
    }
}

struct NormalizedOutputContent {
    content: serde_json::Value,
    origin: Option<MessageOrigin>,
    provenance_kind: Option<String>,
    provenance_session_id: Option<Uuid>,
    provenance_agent_type: Option<String>,
}

fn normalize_output_content(content: serde_json::Value) -> NormalizedOutputContent {
    let base = NormalizedOutputContent {
        content,
        origin: None,
        provenance_kind: None,
        provenance_session_id: None,
        provenance_agent_type: None,
    };

    let Ok(portal) = serde_json::from_value::<PortalMessage>(base.content.clone()) else {
        return base;
    };
    let [PortalContent::AgentMessage {
        from_agent_type,
        from_session_id,
        text,
    }] = portal.content.as_slice()
    else {
        return base;
    };
    let Ok(from_session_id) = from_session_id.parse::<Uuid>() else {
        return base;
    };

    let origin = MessageOrigin::InterAgent {
        from_session_id,
        from_agent_type: from_agent_type.clone(),
    };
    NormalizedOutputContent {
        content: PortalMessage::text(text.clone()).to_json(),
        origin: Some(origin),
        provenance_kind: Some("inter_agent".to_string()),
        provenance_session_id: Some(from_session_id),
        provenance_agent_type: Some(from_agent_type.clone()),
    }
}

/// Extract and store cost and token usage from result messages.
/// Tries typed deserialization via `claude_codes::io::ResultMessage` first,
/// falls back to manual JSON extraction for forward compatibility.
/// `subagent_tokens` is the session's running total of sub-agent (Task tool)
/// tokens, folded into `output_tokens` because `result.usage` covers only the
/// parent conversation. See `SessionManager::subagent_tokens`.
fn store_result_metadata(
    conn: &mut diesel::PgConnection,
    session_id: Uuid,
    content: &serde_json::Value,
    subagent_tokens: i64,
) {
    use crate::schema::sessions;

    // Try typed deserialization first
    if let Ok(result) = claude_codes::io::ResultMessage::deserialize(content) {
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
                    sessions::output_tokens.eq(usage.output_tokens as i64 + subagent_tokens),
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
                sessions::output_tokens.eq(output_tokens.unwrap_or(0) + subagent_tokens),
                sessions::cache_creation_tokens.eq(cache_creation.unwrap_or(0)),
                sessions::cache_read_tokens.eq(cache_read.unwrap_or(0)),
            ))
            .execute(conn)
        {
            error!("Failed to update session tokens: {}", e);
        }
    }
}

/// If the content is a portal message with base64 image data, extract the image
/// into the store and replace the data field with a URL path.
///
/// `inserting_user_id` is the session owner (looked up by the caller). Stored
/// images are bound to that user + the optional `session_id` so the
/// `/api/images/{id}` route can gate fetches by ownership/membership (#786).
fn extract_portal_images(
    mut content: serde_json::Value,
    image_store: &crate::handlers::images::ImageStore,
    inserting_user_id: Uuid,
    session_id: Option<Uuid>,
) -> serde_json::Value {
    // Only process portal messages
    if content.get("type").and_then(|t| t.as_str()) != Some("portal") {
        return content;
    }

    let Some(content_array) = content.get_mut("content").and_then(|c| c.as_array_mut()) else {
        return content;
    };

    for item in content_array.iter_mut() {
        if item.get("type").and_then(|t| t.as_str()) != Some("image") {
            continue;
        }

        let media_type = item
            .get("media_type")
            .and_then(|m| m.as_str())
            .unwrap_or("image/png")
            .to_string();

        let Some(data_str) = item.get("data").and_then(|d| d.as_str()) else {
            continue;
        };

        // Only extract images larger than 64KB base64 (roughly 48KB decoded)
        if data_str.len() < 65536 {
            continue;
        }

        if let Some(id) =
            image_store.store_base64(&media_type, data_str, inserting_user_id, session_id)
        {
            let url = format!("/api/images/{}", id);
            item["data"] = serde_json::Value::String(url);
            item["source_type"] = serde_json::Value::String("url".to_string());
        }
    }

    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_send_mode_accepts_persisted_wire_values() {
        assert_eq!(parse_send_mode("normal"), Some(SendMode::Normal));
        assert_eq!(parse_send_mode("wiggum"), Some(SendMode::Wiggum));
        assert_eq!(parse_send_mode("unknown"), None);
    }

    /// Guards the wire contract the sub-agent token fold relies on: a
    /// `task_notification` system message must parse and expose
    /// `usage.total_tokens`. If the SDK reshapes this, the fold silently stops
    /// counting — so pin it.
    #[test]
    fn task_notification_exposes_total_tokens() {
        let content = serde_json::json!({
            "type": "system",
            "subtype": "task_notification",
            "session_id": "s1",
            "task_id": "t1",
            "status": "completed",
            "summary": "done",
            "usage": { "duration_ms": 1000, "tool_uses": 3, "total_tokens": 68694 }
        });
        let parsed = shared::ClaudeOutput::deserialize(&content).expect("parses as ClaudeOutput");
        let shared::ClaudeOutput::System(sys) = parsed else {
            panic!("expected a system message");
        };
        assert!(sys.is_task_notification());
        let notif = sys
            .as_task_notification()
            .expect("task_notification struct parse");
        assert_eq!(notif.usage.expect("usage present").total_tokens, 68694);
    }

    #[test]
    fn normalize_output_content_moves_agent_message_attribution_to_origin() {
        let sender = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
        let content = PortalMessage::agent_message(
            "claude".to_string(),
            sender.to_string(),
            "hello from peer".to_string(),
        )
        .to_json();

        let normalized = normalize_output_content(content);

        assert_eq!(normalized.provenance_kind.as_deref(), Some("inter_agent"));
        assert_eq!(normalized.provenance_session_id, Some(sender));
        assert_eq!(normalized.provenance_agent_type.as_deref(), Some("claude"));
        assert_eq!(
            normalized.origin,
            Some(MessageOrigin::InterAgent {
                from_session_id: sender,
                from_agent_type: "claude".to_string()
            })
        );
        assert_eq!(normalized.content["type"], "portal");
        assert_eq!(normalized.content["content"][0]["type"], "text");
        assert_eq!(normalized.content["content"][0]["text"], "hello from peer");
    }
}
