use super::permissions::{handle_permission_response, replay_pending_permission};
use super::replay::replay_history;
use super::uploads::{handle_file_upload_chunk, handle_file_upload_start, PendingUpload};
use super::{SessionId, SessionManager, WebClientSender};
use crate::handlers::session_access::is_session_mutator;
use crate::AppState;
use axum::extract::ws::WebSocket;
use diesel::prelude::*;
use shared::{
    ClientEndpoint, ClientToServer, PortalMessage, SendMode, ServerToClient, ServerToProxy,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

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
        ClientToServer::AgentInput {
            content,
            send_mode,
            client_msg_id,
        } => {
            // Gate on editor/owner role — re-queried on each input so role
            // revocations take effect immediately for already-connected viewers.
            // See `session_access::is_session_mutator` for the layered rules.
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    if let Some(id) = client_msg_id {
                        let _ = tx.send(ServerToClient::InputProgress {
                            client_msg_id: id,
                            stage: shared::InputDeliveryStage::Failed,
                            message: Some("permission denied".to_string()),
                        });
                    }
                    let _ = tx.send(ServerToClient::Error {
                        message: "You don't have permission to send input to this session"
                            .to_string(),
                    });
                    return false;
                }
            }
            // First delivery stage: the backend accepted the input frame. Later
            // stages (ProxyReceived / AgentAccepted) come from the proxy ack
            // path — a follow-up slice threads client_msg_id through there.
            if let Some(id) = client_msg_id {
                let _ = tx.send(ServerToClient::InputProgress {
                    client_msg_id: id,
                    stage: shared::InputDeliveryStage::ServerReceived,
                    message: None,
                });
            }
            handle_web_input(
                session_manager,
                db_pool,
                session_key,
                *verified_session_id,
                content,
                send_mode,
                user_id,
                client_msg_id,
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
        ClientToServer::ScheduleLimitContinuation { continuation_id } => {
            if let Some(session_id) = *verified_session_id {
                if !is_session_mutator(app_state, session_id, user_id) {
                    warn!(
                        "Viewer-role user {} attempted ScheduleLimitContinuation on session {}",
                        user_id, session_id
                    );
                    return false;
                }
                super::continuations::schedule_limit_continuation(
                    app_state,
                    session_manager,
                    user_id,
                    session_id,
                    continuation_id,
                );
            } else {
                warn!("Web client tried to schedule continuation without verified session");
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
    let access = app_state.conn().and_then(|mut conn| {
        crate::handlers::session_access::verify_session_reader(&mut conn, session_id, user_id)
    });
    match access {
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
        Err(e) => {
            warn!(
                "User {} attempted to access session {} they don't own: {:?}",
                user_id, session_id, e
            );
            let _ = tx.send(ServerToClient::Error {
                message: "Access denied: you don't own this session".to_string(),
            });
            true // close connection
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_web_input(
    session_manager: &SessionManager,
    db_pool: &crate::db::DbPool,
    session_key: &Option<SessionId>,
    verified_session_id: Option<Uuid>,
    content: serde_json::Value,
    send_mode: Option<SendMode>,
    user_id: Uuid,
    client_msg_id: Option<Uuid>,
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

            // Insert first so the broadcast carries the server-assigned
            // `created_at` (closes #784 — same fix as handle_claude_output).
            let mut row_created_at: Option<String> = None;
            if let (Some(session), Ok(mut conn)) = (session.as_ref(), db_pool.get()) {
                use crate::schema::messages;
                let new_message = crate::models::NewMessage {
                    session_id,
                    role: "portal".to_string(),
                    content: serde_json::to_string(&portal.to_json()).unwrap_or_default(),
                    user_id,
                    agent_type: session.agent_type.clone(),
                    provenance_kind: None,
                    provenance_session_id: None,
                    provenance_agent_type: None,
                };
                match diesel::insert_into(messages::table)
                    .values(&new_message)
                    .get_result::<crate::models::Message>(&mut conn)
                {
                    Ok(inserted) => {
                        row_created_at = Some(
                            inserted
                                .created_at
                                .format("%Y-%m-%dT%H:%M:%S%.6f")
                                .to_string(),
                        );
                    }
                    Err(e) => {
                        error!("Failed to store portal message: {}", e);
                    }
                }
            }

            session_manager.broadcast_to_web_clients(
                key,
                ServerToClient::AgentOutput {
                    content: portal.to_json(),
                    sender_user_id: None,
                    sender_name: None,
                    agent_type,
                    created_at: row_created_at,
                    origin: None,
                },
            );
        }
    }

    // Seq bump + best-effort persist + live delivery, shared with the
    // agent-messaging send path (see SessionManager::enqueue_input).
    let outcome =
        session_manager.enqueue_input(db_pool, key, session_id, content, send_mode, client_msg_id);
    if !outcome.delivered {
        warn!(
            "Failed to send to session '{}', session not found in SessionManager (input queued)",
            key
        );
    }
}
