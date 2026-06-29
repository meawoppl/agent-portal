use super::continuations::handle_session_limit_reached;
use super::message_handlers::{handle_claude_output, replay_pending_inputs_from_db};
use super::permissions::handle_permission_request;
use super::registration::{register_or_update_session, RegistrationParams};
use super::turn_metrics::handle_turn_metrics_report;
use super::{ProxySender, SessionId, SessionManager};
use crate::AppState;
use axum::extract::ws::WebSocket;
use diesel::prelude::*;
use shared::{ProxyToServer, ServerToProxy, SessionEndpoint, SessionStatus};
use std::ops::ControlFlow;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

pub async fn handle_session_socket(socket: WebSocket, app_state: Arc<AppState>) {
    let session_manager = app_state.session_manager.clone();
    let db_pool = app_state.db_pool.clone();
    let conn = ws_bridge::server::into_connection::<SessionEndpoint>(socket);
    let (mut ws_sender, mut ws_receiver) = conn.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerToProxy>();

    let mut session_key: Option<SessionId> = None;
    let mut db_session_id: Option<Uuid> = None;
    let mut connection_gen: Option<u64> = None;

    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    while let Some(result) = ws_receiver.recv().await {
        match result {
            Ok(proxy_msg) => {
                let flow = handle_proxy_message(
                    proxy_msg,
                    &app_state,
                    &session_manager,
                    &db_pool,
                    &tx,
                    &mut session_key,
                    &mut db_session_id,
                    &mut connection_gen,
                );
                if flow.is_break() {
                    // Registration failed (auth bypass / wrong token / no
                    // token) — RegisterAck { success: false } has already
                    // been queued; break so the WS closes cleanly without
                    // accepting any further messages on this connection.
                    break;
                }
            }
            Err(e) => {
                warn!("WebSocket decode error: {}", e);
                continue;
            }
        }
    }

    // Cleanup — only if no newer connection has taken over this session.
    // A reconnecting proxy may have already registered a new connection,
    // in which case our generation will be stale and we must skip cleanup
    // to avoid overwriting the new connection's state.
    let is_stale = session_key
        .as_ref()
        .zip(connection_gen)
        .is_some_and(|(key, gen)| !session_manager.is_current_connection(key, gen));

    if !is_stale {
        if let Some(session_id) = db_session_id {
            match db_pool.get() {
                Ok(mut conn) => {
                    use crate::schema::sessions;
                    let _ = diesel::update(sessions::table.find(session_id))
                        .set(sessions::status.eq(SessionStatus::Disconnected.as_str()))
                        .execute(&mut conn);
                }
                Err(e) => {
                    error!(
                        "Failed to get database connection for session disconnect cleanup: {}",
                        e
                    );
                }
            }
        }

        if let Some(key) = session_key {
            session_manager.unregister_session(&key, connection_gen);
        }
    }

    // Drop our owned channel handle first so the send_task naturally
    // drains any queued messages (e.g. the auth-failure RegisterAck) and
    // exits when the channel closes, rather than being torn down before
    // the proxy sees the response.
    drop(tx);
    let _ = send_task.await;
}

#[allow(clippy::too_many_arguments)]
fn handle_proxy_message(
    proxy_msg: ProxyToServer,
    app_state: &AppState,
    session_manager: &SessionManager,
    db_pool: &crate::db::DbPool,
    tx: &ProxySender,
    session_key: &mut Option<SessionId>,
    db_session_id: &mut Option<Uuid>,
    connection_gen: &mut Option<u64>,
) -> ControlFlow<()> {
    match proxy_msg {
        ProxyToServer::Register(shared::RegisterFields {
            session_id: claude_session_id,
            session_name,
            auth_token,
            working_directory,
            resuming,
            git_branch,
            replay_after: _,
            client_version,
            replaces_session_id,
            hostname,
            launcher_id,
            agent_type,
            repo_url,
            scheduled_task_id,
            claude_args,
        }) => {
            let key = claude_session_id.to_string();

            // SECURITY (#780): authenticate *before* registering the socket
            // with the SessionManager. Previously the socket was registered
            // first, which meant any client that connected with a known or
            // guessed session UUID could be wired up to receive that
            // session's input messages *before* the DB rejected them — and
            // even if the DB rejected the registration, the unregister-on-
            // drop path could clobber a legitimate proxy that happened to
            // hold the same session UUID. Registering only after auth
            // closes both windows.
            let params = RegistrationParams {
                claude_session_id,
                session_name: &session_name,
                auth_token: auth_token.as_deref(),
                working_directory: &working_directory,
                resuming,
                git_branch: &git_branch,
                client_version: &client_version,
                session_key: &key,
                replaces_session_id,
                hostname: hostname.as_deref().unwrap_or("unknown"),
                launcher_id,
                agent_type,
                repo_url: &repo_url,
                scheduled_task_id,
                claude_args: &claude_args,
            };
            let result = register_or_update_session(app_state, &params);

            if result.success {
                // Only now do we expose the socket via SessionManager and
                // bind cleanup state. The send_task in handle_session_socket
                // owns the WS sender — replies (including the RegisterAck
                // below) flow through `tx` regardless of session-manager
                // registration, so the proxy still gets a response on
                // failure even though we never registered the socket.
                let gen = session_manager.register_session(key.clone(), tx.clone());
                *session_key = Some(key);
                *connection_gen = Some(gen);
                *db_session_id = result.session_id;
            }

            let _ = tx.send(ServerToProxy::RegisterAck {
                success: result.success,
                session_id: claude_session_id,
                error: result.error,
                max_image_mb: app_state.max_image_mb,
            });

            info!(
                "Session registered: {} ({}) - success: {}, client_version: {:?}",
                session_name, claude_session_id, result.success, client_version
            );

            if result.success {
                if let Some(session_id) = *db_session_id {
                    replay_pending_inputs_from_db(db_pool, session_id, tx);
                }
            } else {
                // Auth bypass / wrong token / no token → tell the caller
                // to close the WebSocket after the RegisterAck flushes.
                return ControlFlow::Break(());
            }
        }
        ProxyToServer::AgentOutput {
            content,
            agent_type,
        } => {
            handle_claude_output(
                session_manager,
                session_key,
                *db_session_id,
                db_pool,
                tx,
                content,
                None,
                &app_state.image_store,
                agent_type,
            );
        }
        ProxyToServer::SequencedOutput {
            seq,
            content,
            agent_type,
        } => {
            handle_claude_output(
                session_manager,
                session_key,
                *db_session_id,
                db_pool,
                tx,
                content,
                Some(seq),
                &app_state.image_store,
                agent_type,
            );
        }
        ProxyToServer::Heartbeat => {
            let _ = tx.send(ServerToProxy::Heartbeat);
        }
        ProxyToServer::PermissionRequest {
            request_id,
            tool_name,
            input,
            permission_suggestions,
        } => {
            handle_permission_request(
                session_manager,
                session_key,
                *db_session_id,
                db_pool,
                request_id,
                tool_name,
                input,
                permission_suggestions,
            );
        }
        ProxyToServer::SessionUpdate {
            session_id: update_session_id,
            git_branch,
            pr_url,
            repo_url,
            open_prs,
        } => {
            handle_session_update(
                session_manager,
                session_key,
                *db_session_id,
                db_pool,
                update_session_id,
                git_branch,
                pr_url,
                repo_url,
                open_prs,
            );
        }
        ProxyToServer::InputAck {
            session_id: ack_session_id,
            ack_seq,
        } => {
            handle_input_ack(*db_session_id, db_pool, ack_session_id, ack_seq);
        }
        ProxyToServer::InputProgressAck {
            session_id: ack_session_id,
            client_msg_id,
            stage,
        } => {
            let Some(current_session_id) = *db_session_id else {
                return ControlFlow::Continue(());
            };
            if ack_session_id != current_session_id {
                warn!(
                    "InputProgressAck session_id mismatch: {} != {}",
                    ack_session_id, current_session_id,
                );
                return ControlFlow::Continue(());
            }
            // Relay the proxy's per-stage delivery signal to the session's web
            // clients (#939); each frontend advances the matching optimistic
            // row by client_msg_id. Failure detail isn't carried on this hop.
            if let Some(key) = session_key {
                session_manager.broadcast_to_web_clients(
                    key,
                    shared::ServerToClient::InputProgress {
                        client_msg_id,
                        stage,
                        message: None,
                    },
                );
            }
        }
        ProxyToServer::TurnMetricsReport(metrics) => {
            handle_turn_metrics_report(
                session_manager,
                session_key,
                *db_session_id,
                db_pool,
                *metrics,
            );
        }
        ProxyToServer::SessionLimitReached(fields) => {
            handle_session_limit_reached(
                app_state,
                session_manager,
                session_key,
                *db_session_id,
                db_pool,
                fields,
            );
        }
        ProxyToServer::FileDownloadResponse(response) => {
            let request_id = response.request_id;
            if !session_manager.complete_file_download(request_id, response) {
                warn!(
                    "Received FileDownloadResponse for unknown request {}",
                    request_id
                );
            }
        }
        ProxyToServer::SessionStatus { .. } => {}
    }
    ControlFlow::Continue(())
}

#[allow(clippy::too_many_arguments)]
fn handle_session_update(
    session_manager: &SessionManager,
    session_key: &Option<SessionId>,
    db_session_id: Option<Uuid>,
    db_pool: &crate::db::DbPool,
    update_session_id: Uuid,
    git_branch: Option<String>,
    pr_url: Option<String>,
    repo_url: Option<String>,
    open_prs: Vec<shared::PrRef>,
) {
    let Some(current_session_id) = db_session_id else {
        return;
    };
    // Store as a JSON array; the column is `jsonb NOT NULL DEFAULT '[]'`.
    let open_prs_json =
        serde_json::to_value(&open_prs).unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
    let mut conn = match db_pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            error!(
                "Failed to get database connection for session update: {}",
                e
            );
            return;
        }
    };

    if current_session_id != update_session_id {
        warn!(
            "SessionUpdate session_id mismatch: {} != {}",
            update_session_id, current_session_id
        );
        return;
    }

    use crate::schema::sessions;
    if let Err(e) = diesel::update(sessions::table.find(current_session_id))
        .set((
            sessions::git_branch.eq(&git_branch),
            sessions::pr_url.eq(&pr_url),
            sessions::repo_url.eq(&repo_url),
            sessions::open_prs.eq(&open_prs_json),
        ))
        .execute(&mut conn)
    {
        error!("Failed to update session metadata: {}", e);
    } else {
        info!(
            "Updated session {}: branch={:?} pr_url={:?} repo_url={:?} open_prs={}",
            current_session_id,
            git_branch,
            pr_url,
            repo_url,
            open_prs.len()
        );

        if let Some(ref key) = session_key {
            session_manager.broadcast_to_web_clients(
                key,
                shared::ServerToClient::SessionUpdate {
                    session_id: current_session_id,
                    git_branch,
                    pr_url,
                    repo_url,
                    open_prs,
                },
            );
        }
    }
}

fn handle_input_ack(
    db_session_id: Option<Uuid>,
    db_pool: &crate::db::DbPool,
    ack_session_id: Uuid,
    ack_seq: i64,
) {
    let Some(current_session_id) = db_session_id else {
        return;
    };

    if ack_session_id != current_session_id {
        warn!(
            "InputAck session_id mismatch: {} != {}",
            ack_session_id, current_session_id
        );
        return;
    }

    let mut conn = match db_pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            error!("Failed to get database connection for input ack: {}", e);
            return;
        }
    };

    use crate::schema::pending_inputs;
    let deleted = diesel::delete(
        pending_inputs::table
            .filter(pending_inputs::session_id.eq(current_session_id))
            .filter(pending_inputs::seq_num.le(ack_seq)),
    )
    .execute(&mut conn);

    match deleted {
        Ok(count) => {
            info!(
                "Deleted {} pending inputs for session {} (ack_seq={})",
                count, current_session_id, ack_seq
            );
        }
        Err(e) => {
            error!("Failed to delete pending inputs: {}", e);
        }
    }
}
