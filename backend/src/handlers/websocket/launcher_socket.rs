use axum::extract::ws::WebSocket;
use shared::{LauncherEndpoint, LauncherToServer, ServerToClient, ServerToLauncher};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use uuid::Uuid;

use super::LauncherConnection;
use crate::AppState;

pub async fn handle_launcher_socket(socket: WebSocket, app_state: Arc<AppState>) {
    let conn = ws_bridge::server::into_connection::<LauncherEndpoint>(socket);
    let (mut ws_sender, mut ws_receiver) = conn.split();

    // Wait for LauncherRegister message
    let (launcher_id, launcher_name, hostname, user_id, working_directory) = loop {
        match ws_receiver.recv().await {
            Some(Ok(LauncherToServer::LauncherRegister {
                launcher_id,
                launcher_name,
                auth_token,
                hostname,
                working_directory,
                ..
            })) => {
                // Authenticate
                let user_id = if let Some(ref token) = auth_token {
                    match app_state.db_pool.get() {
                        Ok(mut conn) => {
                            match crate::handlers::proxy_tokens::verify_and_get_user(
                                &app_state, &mut conn, token,
                            ) {
                                Ok((uid, email)) => {
                                    info!("Launcher authenticated as {} ({})", email, uid);
                                    uid
                                }
                                Err(_) => {
                                    if app_state.dev_mode {
                                        get_dev_user_id(&app_state)
                                    } else {
                                        let _ = ws_sender
                                            .send(ServerToLauncher::LauncherRegisterAck {
                                                success: false,
                                                launcher_id,
                                                error: Some("Authentication failed".to_string()),
                                            })
                                            .await;
                                        return;
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            let _ = ws_sender
                                .send(ServerToLauncher::LauncherRegisterAck {
                                    success: false,
                                    launcher_id,
                                    error: Some("Database error".to_string()),
                                })
                                .await;
                            return;
                        }
                    }
                } else if app_state.dev_mode {
                    get_dev_user_id(&app_state)
                } else {
                    let _ = ws_sender
                        .send(ServerToLauncher::LauncherRegisterAck {
                            success: false,
                            launcher_id,
                            error: Some("No auth token provided".to_string()),
                        })
                        .await;
                    return;
                };

                break (
                    launcher_id,
                    launcher_name,
                    hostname,
                    user_id,
                    working_directory,
                );
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => {
                warn!("Launcher decode error during registration: {}", e);
                continue;
            }
            None => return,
        }
    };

    // Send RegisterAck
    let _ = ws_sender
        .send(ServerToLauncher::LauncherRegisterAck {
            success: true,
            launcher_id,
            error: None,
        })
        .await;

    // Create channel for sending messages to this launcher
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerToLauncher>();

    app_state.session_manager.register_launcher(
        launcher_id,
        LauncherConnection {
            sender: tx,
            launcher_name: launcher_name.clone(),
            hostname,
            user_id,
            running_sessions: Vec::new(),
            working_directory,
        },
    );

    info!(
        "Launcher '{}' registered for user {}",
        launcher_name, user_id
    );

    // Main message loop
    loop {
        tokio::select! {
            // Messages from the launcher
            result = ws_receiver.recv() => {
                match result {
                    Some(Ok(msg)) => {
                        handle_launcher_message(
                            msg,
                            launcher_id,
                            user_id,
                            &app_state,
                        );
                    }
                    Some(Err(e)) => {
                        warn!("Launcher decode error: {}", e);
                        continue;
                    }
                    None => {
                        info!("Launcher '{}' disconnected", launcher_name);
                        break;
                    }
                }
            }

            // Messages to forward to the launcher
            Some(msg) = rx.recv() => {
                if ws_sender.send(msg).await.is_err() {
                    break;
                }
            }
        }
    }

    app_state.session_manager.unregister_launcher(&launcher_id);
}

fn handle_launcher_message(
    msg: LauncherToServer,
    launcher_id: Uuid,
    user_id: Uuid,
    app_state: &AppState,
) {
    match msg {
        LauncherToServer::LaunchSessionResult {
            request_id,
            success,
            session_id,
            pid,
            ref error,
        } => {
            if success {
                info!(
                    "Launch succeeded: request={}, session={:?}, pid={:?}",
                    request_id, session_id, pid
                );
            } else {
                warn!("Launch failed: request={}, error={:?}", request_id, error);
            }
            // Forward to web clients as ServerToClient
            app_state.session_manager.broadcast_to_user(
                &user_id,
                ServerToClient::LaunchSessionResult {
                    request_id,
                    success,
                    session_id,
                    pid,
                    error: error.clone(),
                },
            );
        }
        LauncherToServer::LauncherHeartbeat {
            running_sessions, ..
        } => {
            if let Some(mut launcher) = app_state.session_manager.launchers.get_mut(&launcher_id) {
                launcher.running_sessions = running_sessions;
            }
        }
        LauncherToServer::ProxyLog {
            session_id,
            level,
            ref message,
            ..
        } => match level.as_str() {
            "error" => tracing::error!(session_id = %session_id, "[proxy] {}", message),
            "warn" => tracing::warn!(session_id = %session_id, "[proxy] {}", message),
            "debug" => tracing::debug!(session_id = %session_id, "[proxy] {}", message),
            _ => tracing::info!(session_id = %session_id, "[proxy] {}", message),
        },
        LauncherToServer::SessionExited {
            session_id,
            exit_code,
        } => {
            info!("Proxy exited: session={}, code={:?}", session_id, exit_code);
            app_state.session_manager.broadcast_to_user(
                &user_id,
                ServerToClient::SessionExited {
                    session_id,
                    exit_code,
                },
            );
        }
        LauncherToServer::ListDirectoriesResult { request_id, .. } => {
            app_state
                .session_manager
                .complete_dir_request(request_id, msg);
        }
        LauncherToServer::RequestLaunch {
            request_id,
            working_directory,
            session_name,
            claude_args,
            agent_type,
        } => {
            info!(
                "Launcher requested launch: dir={}, name={:?}",
                working_directory, session_name
            );
            match crate::handlers::launchers::mint_launch_token(app_state, user_id) {
                Ok(auth_token) => {
                    let launch_msg = ServerToLauncher::LaunchSession {
                        request_id,
                        user_id,
                        auth_token,
                        working_directory,
                        session_name,
                        claude_args,
                        agent_type,
                    };
                    if !app_state
                        .session_manager
                        .send_to_launcher(&launcher_id, launch_msg)
                    {
                        error!(
                            "Failed to send LaunchSession back to launcher {}",
                            launcher_id
                        );
                    }
                }
                Err(status) => {
                    error!(
                        "Failed to mint token for launcher RequestLaunch: {:?}",
                        status
                    );
                }
            }
        }
        LauncherToServer::LauncherRegister { .. } => {}
    }
}

fn get_dev_user_id(app_state: &AppState) -> Uuid {
    use crate::schema::users;
    use diesel::prelude::*;

    let mut conn = app_state.db_pool.get().expect("DB connection for dev mode");
    let user: crate::models::User = users::table
        .filter(users::email.eq("testing@testing.local"))
        .first(&mut conn)
        .expect("Test user must exist in dev mode");
    user.id
}
