use super::permissions::{handle_permission_response, replay_pending_permission};
use super::{SessionId, SessionManager, WebClientSender};
use crate::handlers::session_access::is_session_mutator;
use crate::models::NewPendingInput;
use crate::AppState;
use axum::extract::ws::WebSocket;
use base64::Engine;
use diesel::prelude::*;
use shared::{
    ClientEndpoint, ClientToServer, PortalMessage, SendMode, ServerToClient, ServerToProxy,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Hard ceiling on the number of chunks per upload. With `MAX_CHUNK_BYTES = 64
/// KiB` this caps any single upload at `MAX_TOTAL_CHUNKS * MAX_CHUNK_BYTES`
/// (~3.2 GiB), but the **binding** limit is `effective_max_total_bytes` (the
/// `PORTAL_MAX_IMAGE_MB` cap, see `handle_file_upload_chunk`) — this constant
/// only stops obviously-pathological `total_chunks` values from being accepted
/// at `Start` time. Per-chunk decoded size is what the previous
/// `MAX_TOTAL_CHUNKS = 51_200` constant implicitly (and incorrectly) assumed.
const MAX_TOTAL_CHUNKS: u32 = 65_536;

/// Per-chunk decoded byte cap. Enforced after base64-decoding each chunk so
/// the server's total-byte budget can't be bypassed by a client sending
/// arbitrarily-large chunks (the prior behavior — see #785).
const MAX_CHUNK_BYTES: usize = 64 * 1024; // 64 KiB

/// Tracks metadata for an in-progress upload so we can validate chunks.
/// `received_indices` is the unique set of chunk indices we've accepted —
/// counting raw arrivals (the old `received_count: u32` field) let a client
/// "complete" an upload by re-sending the same `chunk_index` `total_chunks`
/// times. See #785.
struct PendingUpload {
    total_chunks: u32,
    /// Total decoded bytes the client declared up front at `Start`. Used to
    /// detect runaway uploads (running decoded bytes exceeding the declared
    /// total) and to short-circuit the per-server `effective_max_total_bytes`
    /// cap when the client honestly declared a smaller upload.
    total_size: u64,
    /// Per-server byte cap derived from `PORTAL_MAX_IMAGE_MB` at `Start`
    /// time. The binding limit on a single upload's running decoded bytes is
    /// `min(total_size, effective_max_total_bytes)` — declaring a smaller
    /// `total_size` doesn't let you exceed the server cap, and the server cap
    /// doesn't let you exceed an honestly-declared `total_size`.
    effective_max_total_bytes: u64,
    received_indices: HashSet<u32>,
    /// Running total of decoded bytes across all accepted chunks. Compared
    /// against `effective_max_total_bytes` after every chunk to abort the
    /// upload as soon as the cap would be exceeded (rather than only catching
    /// it at finalize time).
    received_bytes: u64,
}

pub async fn handle_web_client_socket(socket: WebSocket, app_state: Arc<AppState>, user_id: Uuid) {
    let session_manager = app_state.session_manager.clone();
    let db_pool = app_state.db_pool.clone();
    let initial_replay_limit = app_state.message_retention_count;
    let conn = ws_bridge::server::into_connection::<ClientEndpoint>(socket);
    let (mut ws_sender, mut ws_receiver) = conn.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerToClient>();

    let mut session_key: Option<SessionId> = None;
    let mut verified_session_id: Option<Uuid> = None;
    let mut pending_uploads: HashMap<String, PendingUpload> = HashMap::new();

    session_manager.add_user_client(user_id, tx.clone());

    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    while let Some(result) = ws_receiver.recv().await {
        match result {
            Ok(client_msg) => {
                let should_break = handle_web_client_message(
                    client_msg,
                    &app_state,
                    &session_manager,
                    &db_pool,
                    &tx,
                    user_id,
                    initial_replay_limit,
                    &mut session_key,
                    &mut verified_session_id,
                    &mut pending_uploads,
                );
                if should_break {
                    break;
                }
            }
            Err(e) => {
                warn!("WebSocket decode error: {}", e);
                continue;
            }
        }
    }

    send_task.abort();
}

/// Returns true if the connection should be closed
#[allow(clippy::too_many_arguments)]
fn handle_web_client_message(
    client_msg: ClientToServer,
    app_state: &AppState,
    session_manager: &SessionManager,
    db_pool: &crate::db::DbPool,
    tx: &WebClientSender,
    user_id: Uuid,
    initial_replay_limit: i64,
    session_key: &mut Option<SessionId>,
    verified_session_id: &mut Option<Uuid>,
    pending_uploads: &mut HashMap<String, PendingUpload>,
) -> bool {
    match client_msg {
        ClientToServer::Register(shared::RegisterFields {
            session_id,
            session_name,
            replay_after,
            ..
        }) => handle_web_register(
            app_state,
            session_manager,
            db_pool,
            tx,
            user_id,
            session_id,
            &session_name,
            replay_after,
            initial_replay_limit,
            session_key,
            verified_session_id,
        ),
        ClientToServer::ClaudeInput { content, send_mode } => {
            // Gate on editor/owner role — re-queried on each input so role
            // revocations take effect immediately for already-connected viewers.
            // See `session_access::is_session_mutator` for the layered rules.
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    let _ = tx.send(ServerToClient::Error {
                        message: "You don't have permission to send input to this session"
                            .to_string(),
                    });
                    return false;
                }
            }
            handle_web_input(
                session_manager,
                db_pool,
                session_key,
                *verified_session_id,
                content,
                send_mode,
                user_id,
            );
            false
        }
        ClientToServer::FileUploadStart(shared::FileUploadStartFields {
            upload_id,
            filename,
            content_type,
            total_chunks,
            total_size,
        }) => {
            // File uploads end up as `ClaudeInput` on the proxy side — they're
            // mutations too, gate them on editor/owner role.
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    warn!(
                        "Viewer-role user {} attempted FileUploadStart on session {}",
                        user_id, session_id
                    );
                    return false;
                }
            }
            handle_file_upload_start(
                session_manager,
                session_key,
                pending_uploads,
                upload_id,
                filename,
                content_type,
                total_chunks,
                total_size,
                app_state.max_image_mb,
            );
            false
        }
        ClientToServer::FileUploadChunk(shared::FileUploadChunkFields {
            upload_id,
            chunk_index,
            data,
        }) => {
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    warn!(
                        "Viewer-role user {} attempted FileUploadChunk on session {}",
                        user_id, session_id
                    );
                    return false;
                }
            }
            handle_file_upload_chunk(
                session_manager,
                session_key,
                pending_uploads,
                upload_id,
                chunk_index,
                data,
            );
            false
        }
        ClientToServer::PermissionResponse(shared::PermissionResponseFields {
            request_id,
            allow,
            input,
            permissions,
            reason,
        }) => {
            if let (Some(ref key), Some(session_id)) = (session_key, *verified_session_id) {
                // Approving/denying tool permissions decides whether the
                // proxy executes the tool — that's a mutation, gate it.
                if !is_session_mutator(app_state, session_id, user_id) {
                    warn!(
                        "Viewer-role user {} attempted PermissionResponse on session {}",
                        user_id, session_id
                    );
                    return false;
                }
                handle_permission_response(
                    session_manager,
                    key,
                    session_id,
                    db_pool,
                    request_id,
                    allow,
                    input,
                    permissions,
                    reason,
                );
            } else {
                warn!("Web client tried to send PermissionResponse without verified session");
            }
            false
        }
        ClientToServer::Interrupt => {
            if let Some(ref key) = session_key {
                // Interrupt cancels the in-flight model turn — a mutation.
                if let Some(session_id) = *verified_session_id {
                    if !is_session_mutator(app_state, session_id, user_id) {
                        warn!(
                            "Viewer-role user {} attempted Interrupt on session {}",
                            user_id, session_id
                        );
                        return false;
                    }
                }
                info!("Web client sending interrupt to session");
                session_manager.send_to_session(key, ServerToProxy::Interrupt);
            } else {
                warn!("Web client tried to send Interrupt without registered session");
            }
            false
        }
    }
}

/// Handle web client registration. Returns true if the connection should be closed.
#[allow(clippy::too_many_arguments)]
fn handle_web_register(
    app_state: &AppState,
    session_manager: &SessionManager,
    db_pool: &crate::db::DbPool,
    tx: &WebClientSender,
    user_id: Uuid,
    session_id: Uuid,
    session_name: &str,
    replay_after: Option<String>,
    initial_replay_limit: i64,
    session_key: &mut Option<SessionId>,
    verified_session_id: &mut Option<Uuid>,
) -> bool {
    match super::auth::verify_session_access(app_state, session_id, user_id) {
        Ok(_session) => {
            let key = session_id.to_string();
            *session_key = Some(key.clone());
            *verified_session_id = Some(session_id);

            session_manager.add_web_client(key, tx.clone());
            info!(
                "Web client connected to session: {} ({}) for user {}",
                session_name, session_id, user_id
            );

            replay_history(db_pool, tx, session_id, replay_after, initial_replay_limit);
            replay_pending_permission(db_pool, session_id, tx);
            false
        }
        Err(_) => {
            warn!(
                "User {} attempted to access session {} they don't own",
                user_id, session_id
            );
            let _ = tx.send(ServerToClient::Error {
                message: "Access denied: you don't own this session".to_string(),
            });
            true // close connection
        }
    }
}

/// Send historical messages from DB to a newly connected web client.
///
/// When the client supplies `replay_after`, we ship every message strictly
/// newer than that cursor — that's the reconnect / focus-regain path and the
/// frontend needs the full delta to stay consistent.
///
/// When `replay_after` is `None` (initial connection or hard refresh), we
/// only ship the trailing `initial_replay_limit` messages. Pre-#788 we
/// shipped the full session history every time and the frontend trimmed to
/// `MAX_MESSAGES_PER_SESSION = 100` locally — long sessions paid the full
/// `O(session_lifetime)` wire cost just to discard most of it. SQL now does
/// the trim. The DB query is `created_at DESC` with a `LIMIT` (so Postgres
/// returns the tail without sorting the whole table); the in-memory `.reverse()`
/// restores the chronological order `ServerToClient::HistoryBatch` consumers
/// expect (the frontend's `WsEvent::HistoryBatch` arm extends the message
/// vector and trims from the front).
fn replay_history(
    db_pool: &crate::db::DbPool,
    tx: &WebClientSender,
    session_id: Uuid,
    replay_after: Option<String>,
    initial_replay_limit: i64,
) {
    let mut conn = match db_pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            error!(
                "Failed to get database connection for history replay: {}",
                e
            );
            return;
        }
    };

    use crate::schema::messages;

    let replay_after_time = replay_after.as_ref().and_then(|ts| {
        chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%.f")
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S"))
            .ok()
    });

    let history: Vec<crate::models::Message> = if let Some(after) = replay_after_time {
        messages::table
            .filter(messages::session_id.eq(session_id))
            .filter(messages::created_at.gt(after))
            .order(messages::created_at.asc())
            .load(&mut conn)
            .unwrap_or_default()
    } else {
        let mut tail: Vec<crate::models::Message> = messages::table
            .filter(messages::session_id.eq(session_id))
            .order(messages::created_at.desc())
            .limit(initial_replay_limit)
            .load(&mut conn)
            .unwrap_or_default();
        tail.reverse();
        tail
    };

    info!(
        "Sending {} historical messages to web client (replay_after: {:?})",
        history.len(),
        replay_after
    );

    if history.is_empty() {
        return;
    }

    // Look up sender names for user-role messages
    use crate::schema::users;
    let user_ids: Vec<Uuid> = history
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| m.user_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let user_names: std::collections::HashMap<Uuid, String> = if !user_ids.is_empty() {
        users::table
            .filter(users::id.eq_any(&user_ids))
            .select((users::id, users::name, users::email))
            .load::<(Uuid, Option<String>, String)>(&mut conn)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, name, email)| (id, name.unwrap_or(email)))
            .collect()
    } else {
        std::collections::HashMap::new()
    };

    let messages: Vec<serde_json::Value> = history
        .into_iter()
        .map(|msg| {
            let mut val =
                serde_json::from_str::<serde_json::Value>(&msg.content).unwrap_or_else(|_| {
                    serde_json::json!({
                        "type": msg.role,
                        "content": msg.content,
                    })
                });
            // Reconstruct _sender for user messages from DB user_id
            if msg.role == "user" {
                if let Some(name) = user_names.get(&msg.user_id) {
                    if let Some(obj) = val.as_object_mut() {
                        obj.insert(
                            "_sender".to_string(),
                            serde_json::json!({
                                "user_id": msg.user_id.to_string(),
                                "name": name,
                            }),
                        );
                    }
                }
            }
            val
        })
        .collect();

    let _ = tx.send(ServerToClient::HistoryBatch { messages });
}

fn handle_web_input(
    session_manager: &SessionManager,
    db_pool: &crate::db::DbPool,
    session_key: &Option<SessionId>,
    verified_session_id: Option<Uuid>,
    content: serde_json::Value,
    send_mode: Option<SendMode>,
    user_id: Uuid,
) {
    let Some(ref key) = session_key else {
        warn!("Web client tried to send ClaudeInput but no session_key set (not registered?)");
        return;
    };
    let Some(session_id) = verified_session_id else {
        warn!("Attempted ClaudeInput without verified session ownership");
        return;
    };

    info!("Web client sending ClaudeInput to session: {}", key);

    // Track who sent this input so we can attribute the echoed user message
    if let Ok(mut conn) = db_pool.get() {
        use crate::schema::users;
        let display_name: String = users::table
            .find(user_id)
            .select(users::name)
            .first::<Option<String>>(&mut conn)
            .ok()
            .flatten()
            .or_else(|| {
                users::table
                    .find(user_id)
                    .select(users::email)
                    .first::<String>(&mut conn)
                    .ok()
            })
            .unwrap_or_else(|| "Unknown".to_string());
        session_manager
            .last_input_sender
            .insert(session_id, (user_id, display_name));
    }

    // For slash commands, broadcast a portal message so the user sees feedback
    if let serde_json::Value::String(text) = &content {
        if text.starts_with('/') {
            let portal = PortalMessage::text(format!("`{}`", text));

            // Look up session once so the broadcast and DB insert share the
            // same agent_type (the session row is the source of truth here —
            // these messages don't originate from a proxy emission).
            let session = db_pool.get().ok().and_then(|mut conn| {
                use crate::schema::sessions;
                sessions::table
                    .find(session_id)
                    .first::<crate::models::Session>(&mut conn)
                    .ok()
            });

            let agent_type = session
                .as_ref()
                .and_then(|s| s.agent_type.parse::<shared::AgentType>().ok())
                .unwrap_or_default();

            session_manager.broadcast_to_web_clients(
                key,
                ServerToClient::ClaudeOutput {
                    content: portal.to_json(),
                    sender_user_id: None,
                    sender_name: None,
                    agent_type,
                },
            );
            // Store in DB so it appears in history
            if let (Some(session), Ok(mut conn)) = (session, db_pool.get()) {
                use crate::schema::messages;
                let new_message = crate::models::NewMessage {
                    session_id,
                    role: "portal".to_string(),
                    content: serde_json::to_string(&portal.to_json()).unwrap_or_default(),
                    user_id: session.user_id,
                    agent_type: session.agent_type.clone(),
                };
                let _ = diesel::insert_into(messages::table)
                    .values(&new_message)
                    .execute(&mut conn);
            }
        }
    }

    let seq = match db_pool.get() {
        Ok(mut conn) => {
            use crate::schema::{pending_inputs, sessions};

            let next_seq: i64 = diesel::update(sessions::table.find(session_id))
                .set(sessions::input_seq.eq(sessions::input_seq + 1))
                .returning(sessions::input_seq)
                .get_result(&mut conn)
                .unwrap_or(1);

            let new_input = NewPendingInput {
                session_id,
                seq_num: next_seq,
                content: serde_json::to_string(&content).unwrap_or_default(),
            };
            if let Err(e) = diesel::insert_into(pending_inputs::table)
                .values(&new_input)
                .execute(&mut conn)
            {
                error!("Failed to store pending input: {}", e);
            }
            next_seq
        }
        Err(e) => {
            error!("Failed to get db connection for pending input: {}", e);
            0
        }
    };

    if seq > 0 {
        if !session_manager.send_to_session(
            key,
            ServerToProxy::SequencedInput {
                session_id,
                seq,
                content,
                send_mode,
            },
        ) {
            warn!(
                "Failed to send to session '{}', session not found in SessionManager (input queued)",
                key
            );
        }
    } else if !session_manager
        .send_to_session(key, ServerToProxy::ClaudeInput { content, send_mode })
    {
        warn!(
            "Failed to send to session '{}', session not found in SessionManager",
            key
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_file_upload_start(
    session_manager: &SessionManager,
    session_key: &Option<SessionId>,
    pending_uploads: &mut HashMap<String, PendingUpload>,
    upload_id: String,
    filename: String,
    content_type: String,
    total_chunks: u32,
    total_size: u64,
    max_image_mb: u32,
) {
    if total_chunks == 0 || total_chunks > MAX_TOTAL_CHUNKS {
        warn!(
            "Invalid total_chunks {} for upload {}",
            total_chunks, upload_id
        );
        return;
    }

    // Server-side hard cap on total decoded bytes, derived from
    // `PORTAL_MAX_IMAGE_MB`. Saturating to avoid overflow on absurd configs.
    let effective_max_total_bytes = (max_image_mb as u64).saturating_mul(1024 * 1024);

    if total_size > effective_max_total_bytes {
        warn!(
            "Upload {} declared total_size {} > server cap {} bytes; rejecting",
            upload_id, total_size, effective_max_total_bytes
        );
        return;
    }

    let safe_filename = sanitize_filename(&filename);
    info!(
        "File upload started: {} ({} chunks, {} bytes) upload_id={}",
        safe_filename, total_chunks, total_size, upload_id
    );

    pending_uploads.insert(
        upload_id.clone(),
        PendingUpload {
            total_chunks,
            total_size,
            effective_max_total_bytes,
            received_indices: HashSet::new(),
            received_bytes: 0,
        },
    );

    // Forward start message to proxy
    if let Some(ref key) = session_key {
        let msg = ServerToProxy::FileUploadStart(shared::FileUploadStartFields {
            upload_id,
            filename: safe_filename,
            content_type,
            total_chunks,
            total_size,
        });
        if !session_manager.send_to_session(key, msg) {
            warn!("Session not connected for file upload start");
        }
    }
}

/// Result of validating an incoming chunk against the `PendingUpload` state.
/// Extracted so the test suite can pin the validation contract directly
/// without standing up a `SessionManager`.
#[derive(Debug, PartialEq, Eq)]
enum ChunkOutcome {
    /// Forward this chunk to the proxy.
    Accept,
    /// Forward this chunk and then drop the upload (final unique chunk).
    AcceptAndComplete,
    /// Silently ignore: duplicate index or out-of-range. Don't bump counters.
    Ignore,
    /// Abort the upload: per-chunk cap exceeded, running-total cap exceeded,
    /// declared total exceeded, or invalid base64. Drop the `PendingUpload`
    /// entry so subsequent chunks for the same `upload_id` are rejected.
    Abort(&'static str),
}

/// Pure chunk-validation step: separated from `handle_file_upload_chunk` so
/// the tests can exercise it without a `SessionManager`. Mutates `upload`
/// only on `Accept` / `AcceptAndComplete`.
fn validate_and_record_chunk(
    upload: &mut PendingUpload,
    chunk_index: u32,
    decoded_len: usize,
) -> ChunkOutcome {
    if chunk_index >= upload.total_chunks {
        return ChunkOutcome::Ignore;
    }
    if upload.received_indices.contains(&chunk_index) {
        return ChunkOutcome::Ignore;
    }
    if decoded_len > MAX_CHUNK_BYTES {
        return ChunkOutcome::Abort("per-chunk byte cap exceeded");
    }

    // Running-total check: compare against the binding cap, which is the
    // smaller of (declared total_size, server PORTAL_MAX_IMAGE_MB cap).
    // Honest clients can't exceed what they declared; dishonest clients
    // (declared small, sending big) can't exceed the server's hard cap.
    let binding_cap = upload.total_size.min(upload.effective_max_total_bytes);
    let new_total = upload.received_bytes.saturating_add(decoded_len as u64);
    if new_total > binding_cap {
        return ChunkOutcome::Abort("running-total byte cap exceeded");
    }

    upload.received_indices.insert(chunk_index);
    upload.received_bytes = new_total;

    if upload.received_indices.len() as u32 >= upload.total_chunks {
        ChunkOutcome::AcceptAndComplete
    } else {
        ChunkOutcome::Accept
    }
}

fn handle_file_upload_chunk(
    session_manager: &SessionManager,
    session_key: &Option<SessionId>,
    pending_uploads: &mut HashMap<String, PendingUpload>,
    upload_id: String,
    chunk_index: u32,
    data: String,
) {
    let Some(upload) = pending_uploads.get_mut(&upload_id) else {
        warn!("Received chunk for unknown upload_id={}", upload_id);
        return;
    };

    // Decode upfront so we can enforce the per-chunk and running-total caps
    // on the *decoded* payload (the wire shape is base64). Aborting here on
    // a decode failure prevents the proxy from seeing garbage chunks the
    // backend already accepted into its byte budget.
    let decoded_len = match base64::engine::general_purpose::STANDARD.decode(data.as_bytes()) {
        Ok(bytes) => bytes.len(),
        Err(_) => {
            warn!(
                "Upload {} chunk {} is not valid base64; aborting",
                upload_id, chunk_index
            );
            pending_uploads.remove(&upload_id);
            return;
        }
    };

    let outcome = validate_and_record_chunk(upload, chunk_index, decoded_len);
    match outcome {
        ChunkOutcome::Ignore => {
            warn!(
                "Ignoring chunk {} for upload {} (duplicate or out-of-range; total={})",
                chunk_index, upload_id, upload.total_chunks
            );
            return;
        }
        ChunkOutcome::Abort(reason) => {
            warn!(
                "Aborting upload {} on chunk {}: {}",
                upload_id, chunk_index, reason
            );
            pending_uploads.remove(&upload_id);
            return;
        }
        ChunkOutcome::Accept | ChunkOutcome::AcceptAndComplete => {}
    }

    // Forward chunk directly to proxy
    if let Some(ref key) = session_key {
        let msg = ServerToProxy::FileUploadChunk(shared::FileUploadChunkFields {
            upload_id: upload_id.clone(),
            chunk_index,
            data,
        });
        if !session_manager.send_to_session(key, msg) {
            warn!("Session not connected for file upload chunk");
        }
    }

    // Clean up tracking when all chunks forwarded
    if outcome == ChunkOutcome::AcceptAndComplete {
        info!(
            "All {} chunks forwarded for upload_id={}",
            upload.total_chunks, upload_id
        );
        pending_uploads.remove(&upload_id);
    }
}

fn sanitize_filename(name: &str) -> String {
    let base = name
        .rsplit('/')
        .next()
        .or_else(|| name.rsplit('\\').next())
        .unwrap_or(name);

    let clean: String = base
        .chars()
        .filter(|c| *c != '/' && *c != '\\' && *c != '\0')
        .collect();

    if clean.is_empty() || clean == "." || clean == ".." {
        "uploaded_file".to_string()
    } else {
        clean
    }
}

#[cfg(test)]
mod tests {
    //! These tests pin the chunk-validation contract described in #785: the
    //! prior implementation bumped `received_count` on every arrival (so a
    //! client sending the same chunk index N times would "complete" the
    //! upload with a single chunk) and enforced only a chunk-count budget
    //! (`MAX_TOTAL_CHUNKS = 51_200`) — never the per-chunk decoded byte size
    //! — so a client sending 10 MB chunks got `10 * 51_200 = 512 GB` of
    //! effective budget. The new validation rules:
    //!
    //!   * duplicate `chunk_index` → ignored, no counter bump
    //!   * `chunk_index >= total_chunks` → ignored
    //!   * decoded chunk > `MAX_CHUNK_BYTES` → abort
    //!   * running decoded bytes > `min(total_size, server cap)` → abort
    //!   * complete only when **distinct** indices == `total_chunks`
    //!
    //! Verified directly against `validate_and_record_chunk` (pure) plus one
    //! end-to-end test that drives `handle_file_upload_chunk` with a real
    //! `SessionManager` and asserts the forwarded `ServerToProxy` shape.
    use super::*;
    use base64::Engine;
    use shared::ServerToProxy;
    use tokio::sync::mpsc;

    fn upload_with(total_chunks: u32, total_size: u64, server_cap_bytes: u64) -> PendingUpload {
        PendingUpload {
            total_chunks,
            total_size,
            effective_max_total_bytes: server_cap_bytes,
            received_indices: HashSet::new(),
            received_bytes: 0,
        }
    }

    #[test]
    fn duplicate_chunk_index_is_ignored_and_does_not_complete() {
        // Two chunks expected; client sends index 0 twice. The upload must
        // NOT complete on the second arrival — the prior `received_count +=
        // 1` path would incorrectly hit `received_count >= total_chunks`
        // after two identical chunks.
        let mut upload = upload_with(2, 100, 100);
        assert_eq!(
            validate_and_record_chunk(&mut upload, 0, 10),
            ChunkOutcome::Accept
        );
        assert_eq!(
            validate_and_record_chunk(&mut upload, 0, 10),
            ChunkOutcome::Ignore
        );
        assert_eq!(upload.received_indices.len(), 1);
        assert_eq!(upload.received_bytes, 10);
    }

    #[test]
    fn out_of_range_chunk_index_is_ignored() {
        let mut upload = upload_with(2, 100, 100);
        assert_eq!(
            validate_and_record_chunk(&mut upload, 5, 10),
            ChunkOutcome::Ignore
        );
        assert_eq!(upload.received_indices.len(), 0);
        assert_eq!(upload.received_bytes, 0);
    }

    #[test]
    fn per_chunk_byte_cap_aborts_upload() {
        let mut upload = upload_with(2, u64::MAX, u64::MAX);
        let outcome = validate_and_record_chunk(&mut upload, 0, MAX_CHUNK_BYTES + 1);
        assert!(
            matches!(outcome, ChunkOutcome::Abort(_)),
            "expected Abort, got {:?}",
            outcome
        );
        // State must not have been mutated on an abort path.
        assert_eq!(upload.received_indices.len(), 0);
        assert_eq!(upload.received_bytes, 0);
    }

    #[test]
    fn running_total_byte_cap_aborts_upload() {
        // Server cap is 100 bytes total; two 60-byte chunks would put us at
        // 120 > 100. First accepts, second aborts.
        let mut upload = upload_with(2, 200, 100);
        assert_eq!(
            validate_and_record_chunk(&mut upload, 0, 60),
            ChunkOutcome::Accept
        );
        let outcome = validate_and_record_chunk(&mut upload, 1, 60);
        assert!(matches!(outcome, ChunkOutcome::Abort(_)));
        // The aborted chunk's bytes must not have been credited.
        assert_eq!(upload.received_bytes, 60);
        assert_eq!(upload.received_indices.len(), 1);
    }

    #[test]
    fn distinct_chunks_in_any_order_complete_the_upload() {
        let mut upload = upload_with(3, 30, 30);
        assert_eq!(
            validate_and_record_chunk(&mut upload, 2, 10),
            ChunkOutcome::Accept
        );
        assert_eq!(
            validate_and_record_chunk(&mut upload, 0, 10),
            ChunkOutcome::Accept
        );
        assert_eq!(
            validate_and_record_chunk(&mut upload, 1, 10),
            ChunkOutcome::AcceptAndComplete
        );
        assert_eq!(upload.received_indices.len(), 3);
        assert_eq!(upload.received_bytes, 30);
    }

    #[test]
    fn declared_total_size_is_the_binding_cap_when_smaller_than_server_cap() {
        // Client declared 50 bytes, server cap is 1 MiB. A 60-byte chunk
        // must abort even though the server cap is huge.
        let mut upload = upload_with(2, 50, 1024 * 1024);
        let outcome = validate_and_record_chunk(&mut upload, 0, 60);
        assert!(matches!(outcome, ChunkOutcome::Abort(_)));
    }

    #[tokio::test]
    async fn handle_chunk_e2e_forwards_distinct_chunks_and_clears_pending() {
        // End-to-end: drive `handle_file_upload_chunk` against a real
        // SessionManager and assert (a) every distinct chunk gets forwarded
        // to the proxy receiver, (b) duplicate-index chunks do NOT get
        // forwarded, and (c) `pending_uploads` is cleared once the unique
        // chunk count hits `total_chunks`.
        let session_manager = SessionManager::new();
        let session_key: SessionId = "test-session".to_string();
        let (proxy_tx, mut proxy_rx) = mpsc::unbounded_channel();
        session_manager.register_session(session_key.clone(), proxy_tx);

        let mut pending_uploads: HashMap<String, PendingUpload> = HashMap::new();
        let upload_id = "u1".to_string();
        pending_uploads.insert(upload_id.clone(), upload_with(3, 30, 1024 * 1024));

        let b64 = base64::engine::general_purpose::STANDARD.encode([0u8; 10]);

        // Three distinct chunks, with one duplicate sandwiched in.
        handle_file_upload_chunk(
            &session_manager,
            &Some(session_key.clone()),
            &mut pending_uploads,
            upload_id.clone(),
            0,
            b64.clone(),
        );
        // Duplicate — must be ignored, must NOT complete the upload.
        handle_file_upload_chunk(
            &session_manager,
            &Some(session_key.clone()),
            &mut pending_uploads,
            upload_id.clone(),
            0,
            b64.clone(),
        );
        assert!(
            pending_uploads.contains_key(&upload_id),
            "duplicate chunk completed upload"
        );

        handle_file_upload_chunk(
            &session_manager,
            &Some(session_key.clone()),
            &mut pending_uploads,
            upload_id.clone(),
            1,
            b64.clone(),
        );
        handle_file_upload_chunk(
            &session_manager,
            &Some(session_key.clone()),
            &mut pending_uploads,
            upload_id.clone(),
            2,
            b64.clone(),
        );
        assert!(
            !pending_uploads.contains_key(&upload_id),
            "upload not cleared after completion"
        );

        // Exactly three chunks must have been forwarded to the proxy (the
        // duplicate was dropped). Indices must match what we sent.
        let mut forwarded_indices = Vec::new();
        while let Ok(msg) = proxy_rx.try_recv() {
            match msg {
                ServerToProxy::FileUploadChunk(f) => forwarded_indices.push(f.chunk_index),
                other => panic!("unexpected forwarded message: {:?}", other),
            }
        }
        forwarded_indices.sort_unstable();
        assert_eq!(forwarded_indices, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn handle_chunk_e2e_oversize_chunk_aborts_and_drops_pending() {
        // A chunk decoding to > MAX_CHUNK_BYTES must abort the upload and
        // remove the pending entry, so subsequent chunks for the same
        // upload_id are silently dropped (the unknown-upload_id path).
        let session_manager = SessionManager::new();
        let session_key: SessionId = "test-session-2".to_string();
        let (proxy_tx, mut proxy_rx) = mpsc::unbounded_channel();
        session_manager.register_session(session_key.clone(), proxy_tx);

        let mut pending_uploads: HashMap<String, PendingUpload> = HashMap::new();
        let upload_id = "u2".to_string();
        pending_uploads.insert(upload_id.clone(), upload_with(2, u64::MAX, u64::MAX));

        let oversize = vec![0u8; MAX_CHUNK_BYTES + 1];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&oversize);

        handle_file_upload_chunk(
            &session_manager,
            &Some(session_key.clone()),
            &mut pending_uploads,
            upload_id.clone(),
            0,
            b64,
        );

        assert!(
            !pending_uploads.contains_key(&upload_id),
            "oversize chunk did not abort"
        );
        // The oversize chunk must NOT have been forwarded.
        assert!(
            proxy_rx.try_recv().is_err(),
            "aborted chunk was forwarded anyway"
        );
    }
}

// =============================================================================
// DB-touching test for the initial-replay limit (closes #788).
//
// Mirrors the harness in `handlers::messages::db_tests` / `session_access::db_tests`:
// auto-skips when `DATABASE_URL` is not set so CI without a DB stays green.
// =============================================================================
#[cfg(test)]
mod replay_tests {
    use crate::models::{NewSessionWithId, NewUser, Session, User};
    use chrono::Utc;
    use diesel::prelude::*;
    use diesel::r2d2::{ConnectionManager, Pool};

    type DbPool = Pool<ConnectionManager<diesel::pg::PgConnection>>;

    fn try_pool() -> Option<DbPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let manager = ConnectionManager::<diesel::pg::PgConnection>::new(url);
        Pool::builder().build(manager).ok()
    }

    fn make_user(conn: &mut diesel::pg::PgConnection, label: &str) -> User {
        use crate::schema::users;
        let nonce = uuid::Uuid::new_v4();
        let new_user = NewUser {
            google_id: format!("test_replay_{}_{}", label, nonce),
            email: format!("test_replay_{}_{}@example.invalid", label, nonce),
            name: Some(format!("Test {}", label)),
            avatar_url: None,
        };
        diesel::insert_into(users::table)
            .values(&new_user)
            .get_result::<User>(conn)
            .expect("insert test user")
    }

    fn make_session(conn: &mut diesel::pg::PgConnection, owner_id: uuid::Uuid) -> Session {
        use crate::schema::sessions;
        let session_id = uuid::Uuid::new_v4();
        let new_session = NewSessionWithId {
            id: session_id,
            user_id: owner_id,
            session_name: format!("test-replay-{}", session_id),
            session_key: session_id.to_string(),
            working_directory: "/tmp".to_string(),
            status: "active".to_string(),
            git_branch: None,
            client_version: None,
            hostname: "test-host".to_string(),
            launcher_id: None,
            agent_type: "claude".to_string(),
            repo_url: None,
            scheduled_task_id: None,
        };
        diesel::insert_into(sessions::table)
            .values(&new_session)
            .get_result::<Session>(conn)
            .expect("insert test session")
    }

    fn seed_messages(
        conn: &mut diesel::pg::PgConnection,
        session_id: uuid::Uuid,
        user_id: uuid::Uuid,
        count: usize,
    ) -> Vec<chrono::NaiveDateTime> {
        use diesel::sql_query;
        let base = Utc::now().naive_utc();
        let mut stamps = Vec::with_capacity(count);
        for i in 0..count {
            let ts = base + chrono::Duration::microseconds((i as i64 + 1) * 1000);
            stamps.push(ts);
            sql_query(
                "INSERT INTO messages (id, session_id, role, content, created_at, user_id, agent_type)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind::<diesel::sql_types::Uuid, _>(uuid::Uuid::new_v4())
            .bind::<diesel::sql_types::Uuid, _>(session_id)
            .bind::<diesel::sql_types::VarChar, _>("assistant")
            .bind::<diesel::sql_types::Text, _>(format!("msg #{}", i))
            .bind::<diesel::sql_types::Timestamp, _>(ts)
            .bind::<diesel::sql_types::Uuid, _>(user_id)
            .bind::<diesel::sql_types::VarChar, _>("claude")
            .execute(conn)
            .expect("seed message");
        }
        stamps
    }

    fn cleanup(
        conn: &mut diesel::pg::PgConnection,
        session_id: uuid::Uuid,
        user_ids: &[uuid::Uuid],
    ) {
        use crate::schema::{messages, sessions, users};
        let _ = diesel::delete(messages::table.filter(messages::session_id.eq(session_id)))
            .execute(conn);
        let _ = diesel::delete(sessions::table.find(session_id)).execute(conn);
        for uid in user_ids {
            let _ = diesel::delete(users::table.find(uid)).execute(conn);
        }
    }

    /// `replay_after = None` returns at most `initial_replay_limit` rows in
    /// chronological order — the exact query shape `replay_history` builds.
    /// Pre-#788 this path loaded the full session history; now SQL trims to
    /// the render-window default so the wire payload is bounded.
    #[test]
    fn replay_after_none_caps_to_retention_count() {
        let Some(pool) = try_pool() else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let mut conn = pool.get().expect("conn");

        let user = make_user(&mut conn, "limit");
        let session = make_session(&mut conn, user.id);
        let stamps = seed_messages(&mut conn, session.id, user.id, 200);

        let limit: i64 = 100;
        use crate::schema::messages;
        let mut tail: Vec<crate::models::Message> = messages::table
            .filter(messages::session_id.eq(session.id))
            .order(messages::created_at.desc())
            .limit(limit)
            .load(&mut conn)
            .expect("load");
        tail.reverse();

        cleanup(&mut conn, session.id, &[user.id]);

        assert_eq!(tail.len(), limit as usize);
        // Tail is the newest 100, presented oldest-first.
        assert_eq!(tail.first().unwrap().created_at, stamps[100]);
        assert_eq!(tail.last().unwrap().created_at, stamps[199]);
        for w in tail.windows(2) {
            assert!(w[0].created_at < w[1].created_at);
        }
    }
}
