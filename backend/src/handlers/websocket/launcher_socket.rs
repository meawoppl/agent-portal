use axum::extract::ws::WebSocket;
use diesel::prelude::*;
use shared::{
    AgentType, LauncherEndpoint, LauncherToServer, ScheduledTaskConfig, ServerToClient,
    ServerToLauncher, ServerToProxy,
};
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
                                                fatal: true,
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
                                    fatal: false,
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
                            fatal: true,
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

    // Reject duplicate: only one launcher per (hostname, user) is allowed
    if let Some(existing_name) = app_state
        .session_manager
        .find_duplicate_launcher(&hostname, user_id)
    {
        warn!(
            "Rejecting duplicate launcher '{}' from {} (user {}) — '{}' already connected",
            launcher_name, hostname, user_id, existing_name
        );
        let _ = ws_sender
            .send(ServerToLauncher::LauncherRegisterAck {
                success: false,
                launcher_id,
                fatal: true,
                error: Some(format!(
                    "A launcher named '{}' is already connected from this host. \
                     Stop the existing instance before starting a new one.",
                    existing_name
                )),
            })
            .await;
        return;
    }

    // Send RegisterAck
    let _ = ws_sender
        .send(ServerToLauncher::LauncherRegisterAck {
            success: true,
            launcher_id,
            error: None,
            fatal: false,
        })
        .await;

    // Create channel for sending messages to this launcher
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerToLauncher>();
    let tx_for_sync = tx.clone();

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

    // Send initial ScheduleSync with the user's scheduled tasks
    if let Ok(mut db_conn) = app_state.db_pool.get() {
        use crate::schema::scheduled_tasks;
        let launcher_hostname = app_state
            .session_manager
            .launchers
            .get(&launcher_id)
            .map(|l| l.hostname.clone())
            .unwrap_or_default();

        let tasks: Vec<crate::models::ScheduledTask> = scheduled_tasks::table
            .filter(scheduled_tasks::user_id.eq(user_id))
            .filter(scheduled_tasks::enabled.eq(true))
            .load(&mut db_conn)
            .unwrap_or_default();

        let task_configs: Vec<ScheduledTaskConfig> = tasks
            .iter()
            .filter(|t| t.hostname.is_none() || t.hostname.as_deref() == Some(&launcher_hostname))
            .map(|t| ScheduledTaskConfig {
                id: t.id,
                name: t.name.clone(),
                cron_expression: t.cron_expression.clone(),
                timezone: t.timezone.clone(),
                working_directory: t.working_directory.clone(),
                prompt: t.prompt.clone(),
                claude_args: serde_json::from_value(t.claude_args.clone()).unwrap_or_default(),
                agent_type: t.agent_type.parse().unwrap_or(AgentType::Claude),
                enabled: t.enabled,
                max_runtime_minutes: t.max_runtime_minutes,
                last_session_id: t.last_session_id,
            })
            .collect();

        if !task_configs.is_empty() {
            let count = task_configs.len();
            let _ = tx_for_sync.send(ServerToLauncher::ScheduleSync {
                tasks: task_configs,
            });
            info!(
                "Sent initial ScheduleSync with {} tasks to launcher '{}'",
                count, launcher_name
            );
        }
    }

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
        LauncherToServer::InjectInput {
            session_id,
            content,
        } => {
            info!(
                "InjectInput for session {} from launcher {}",
                session_id, launcher_id
            );
            let session_key = session_id.to_string();
            let content_value = serde_json::Value::String(content);

            // Set sender attribution to "Scheduler"
            app_state
                .session_manager
                .last_input_sender
                .insert(session_id, (user_id, "Scheduler".to_string()));

            // Sequence and send (same pipeline as web client input)
            if let Ok(mut db_conn) = app_state.db_pool.get() {
                use crate::schema::{pending_inputs, sessions};

                let next_seq: i64 = diesel::update(sessions::table.find(session_id))
                    .set(sessions::input_seq.eq(sessions::input_seq + 1))
                    .returning(sessions::input_seq)
                    .get_result(&mut db_conn)
                    .unwrap_or(0);

                if next_seq > 0 {
                    let new_input = crate::models::NewPendingInput {
                        session_id,
                        seq_num: next_seq,
                        content: serde_json::to_string(&content_value).unwrap_or_default(),
                    };
                    let _ = diesel::insert_into(pending_inputs::table)
                        .values(&new_input)
                        .execute(&mut db_conn);

                    app_state.session_manager.send_to_session(
                        &session_key,
                        ServerToProxy::SequencedInput {
                            session_id,
                            seq: next_seq,
                            content: content_value,
                            send_mode: None,
                        },
                    );
                }
            }
        }
        LauncherToServer::ScheduledRunStarted {
            task_id,
            session_id,
        } => {
            info!(
                "Scheduled run started: task={}, session={}",
                task_id, session_id
            );
            if let Ok(mut db_conn) = app_state.db_pool.get() {
                use crate::schema::scheduled_tasks;
                let _ = diesel::update(
                    scheduled_tasks::table
                        .filter(scheduled_tasks::id.eq(task_id))
                        .filter(scheduled_tasks::user_id.eq(user_id)),
                )
                .set((
                    scheduled_tasks::last_run_at.eq(diesel::dsl::now),
                    scheduled_tasks::last_session_id.eq(session_id),
                    scheduled_tasks::updated_at.eq(diesel::dsl::now),
                ))
                .execute(&mut db_conn);
            }
        }
        LauncherToServer::ScheduledRunCompleted {
            task_id,
            session_id,
            exit_code,
            duration_secs,
        } => {
            info!(
                "Scheduled run completed: task={}, session={}, exit={:?}, duration={}s",
                task_id, session_id, exit_code, duration_secs
            );
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
