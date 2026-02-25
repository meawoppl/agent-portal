use super::permissions::{handle_permission_response, replay_pending_permission};
use super::{SessionId, SessionManager, WebClientSender};
use crate::models::NewPendingInput;
use crate::AppState;
use axum::extract::ws::WebSocket;
use diesel::prelude::*;
use shared::api::RawMessageFallback;
use shared::{ClientEndpoint, ClientToServer, SendMode, ServerToClient, ServerToProxy};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

/// Maximum total upload size: 50 MB decoded (~68 MB base64)
const MAX_TOTAL_CHUNKS: u32 = 51_200; // 50 MB / 1 KB

/// Tracks metadata for an in-progress upload so we can validate chunks
struct PendingUpload {
    total_chunks: u32,
    received_count: u32,
}

pub async fn handle_web_client_socket(socket: WebSocket, app_state: Arc<AppState>, user_id: Uuid) {
    let session_manager = app_state.session_manager.clone();
    let db_pool = app_state.db_pool.clone();
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
            session_key,
            verified_session_id,
        ),
        ClientToServer::ClaudeInput { content, send_mode } => {
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
            handle_file_upload_start(
                session_manager,
                session_key,
                pending_uploads,
                upload_id,
                filename,
                content_type,
                total_chunks,
                total_size,
            );
            false
        }
        ClientToServer::FileUploadChunk(shared::FileUploadChunkFields {
            upload_id,
            chunk_index,
            data,
        }) => {
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

            replay_history(db_pool, tx, session_id, replay_after);
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

/// Send historical messages from DB to a newly connected web client
fn replay_history(
    db_pool: &crate::db::DbPool,
    tx: &WebClientSender,
    session_id: Uuid,
    replay_after: Option<String>,
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
        messages::table
            .filter(messages::session_id.eq(session_id))
            .order(messages::created_at.asc())
            .load(&mut conn)
            .unwrap_or_default()
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
                    let fallback = RawMessageFallback {
                        message_type: msg.role.clone(),
                        content: msg.content.clone(),
                    };
                    serde_json::to_value(&fallback).unwrap_or_default()
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
) {
    if total_chunks == 0 || total_chunks > MAX_TOTAL_CHUNKS {
        warn!(
            "Invalid total_chunks {} for upload {}",
            total_chunks, upload_id
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
            received_count: 0,
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

    if chunk_index >= upload.total_chunks {
        warn!(
            "Chunk index {} out of range for upload {} (total={})",
            chunk_index, upload_id, upload.total_chunks
        );
        return;
    }

    upload.received_count += 1;

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
    if upload.received_count >= upload.total_chunks {
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
