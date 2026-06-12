use axum::extract::ws::WebSocket;
use diesel::prelude::*;
use shared::{
    LauncherEndpoint, LauncherToServer, ScheduledTaskConfig, ServerToClient, ServerToLauncher,
    ServerToProxy, SessionStatus,
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
    let (launcher_id, launcher_name, hostname, user_id, working_directory, version) = loop {
        match ws_receiver.recv().await {
            Some(Ok(LauncherToServer::LauncherRegister {
                launcher_id,
                launcher_name,
                auth_token,
                hostname,
                working_directory,
                version,
            })) => {
                // Authenticate. Launcher tokens never expire (see #932), so
                // there is no expiry to track here.
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
                    version,
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

    // Reject if user already has too many launchers
    const MAX_LAUNCHERS_PER_USER: usize = 10;
    let user_launcher_count = app_state
        .session_manager
        .launchers
        .iter()
        .filter(|entry| entry.value().user_id == user_id)
        .count();

    if user_launcher_count >= MAX_LAUNCHERS_PER_USER {
        error!(
            "User {} has {} launchers, rejecting new registration (max {})",
            user_id, user_launcher_count, MAX_LAUNCHERS_PER_USER
        );
        let _ = ws_sender
            .send(ServerToLauncher::LauncherRegisterAck {
                success: false,
                launcher_id,
                fatal: true,
                error: Some(format!(
                    "You already have {} launchers connected (max {}). \
                     Disconnect an existing launcher before starting a new one.",
                    user_launcher_count, MAX_LAUNCHERS_PER_USER
                )),
            })
            .await;
        return;
    }

    // Create channel for sending messages to this launcher
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerToLauncher>();
    let tx_for_sync = tx.clone();

    // Atomically check for duplicate (user_id, hostname) and register. See
    // `SessionManager::try_register_launcher` — the check + claim happen under
    // a single DashMap shard lock to close the TOCTOU window that existed
    // between separate `find_duplicate_launcher` and `register_launcher`
    // calls (closes #790).
    let register_result = app_state.session_manager.try_register_launcher(
        launcher_id,
        LauncherConnection {
            sender: tx,
            launcher_name: launcher_name.clone(),
            hostname: hostname.clone(),
            user_id,
            running_sessions: Vec::new(),
            working_directory,
            version: version.unwrap_or_default(),
        },
    );

    if let Err(existing_name) = register_result {
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
            .filter(|t| t.hostname == launcher_hostname)
            .map(crate::handlers::scheduled_tasks::task_to_config)
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

    let continuation_configs =
        super::continuations::load_scheduled_continuations(&app_state, launcher_id, user_id);
    let _ = tx_for_sync.send(ServerToLauncher::ContinuationSync {
        continuations: continuation_configs,
    });

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

/// Verify that this launcher is authorized to mutate `session_id`. A
/// session is mutable by a launcher iff (a) the session's owner matches
/// the launcher's authenticated user, AND (b) the session was spawned
/// by this very launcher (matching `launcher_id`).
///
/// Without this check, a compromised or buggy launcher could pass any
/// session UUID and the backend would happily inject input into it or
/// delete it (see #782). Returns `Ok(Session)` if authorized, else logs
/// a warn with all the IDs for forensics and returns `Err(())`.
fn authorize_launcher_session(
    db_conn: &mut diesel::PgConnection,
    launcher_id: Uuid,
    user_id: Uuid,
    session_id: Uuid,
) -> Result<crate::models::Session, ()> {
    use crate::schema::sessions;
    let session = sessions::table
        .find(session_id)
        .first::<crate::models::Session>(db_conn)
        .map_err(|e| {
            warn!(
                "Launcher {} (user {}) referenced unknown session {}: {}",
                launcher_id, user_id, session_id, e
            );
        })?;

    if session.user_id != user_id {
        warn!(
            "Launcher {} (user {}) attempted to act on session {} owned by user {} — denied",
            launcher_id, user_id, session_id, session.user_id
        );
        return Err(());
    }

    if session.launcher_id != Some(launcher_id) {
        warn!(
            "Launcher {} (user {}) attempted to act on session {} spawned by launcher {:?} — denied",
            launcher_id, user_id, session_id, session.launcher_id
        );
        return Err(());
    }

    Ok(session)
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
            let desired_session_id = app_state
                .session_manager
                .pending_launch_sessions
                .remove(&request_id)
                .map(|(_, id)| id);
            if success {
                info!(
                    "Launch succeeded: request={}, session={:?}, pid={:?}",
                    request_id, session_id, pid
                );
            } else {
                warn!("Launch failed: request={}, error={:?}", request_id, error);
                if let Some(session_id) = desired_session_id {
                    if let Ok(mut conn) = app_state.db_pool.get() {
                        use crate::schema::sessions;
                        use diesel::prelude::*;
                        if let Err(e) = diesel::update(sessions::table.find(session_id))
                            .set((
                                sessions::paused.eq(true),
                                sessions::status.eq(SessionStatus::Disconnected.as_str()),
                                sessions::updated_at.eq(diesel::dsl::now),
                            ))
                            .execute(&mut conn)
                        {
                            warn!("Failed to mark failed launch {} paused: {}", session_id, e);
                        }
                    }
                }
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
            reconcile_desired_sessions(app_state, launcher_id, user_id);
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
            // The proxy holding this session's token has exited; revoke the
            // token so it can't be reused. A resume mints a fresh one. See #932.
            if let Ok(mut conn) = app_state.db_pool.get() {
                crate::handlers::proxy_tokens::revoke_tokens_for_session(&mut conn, session_id);
            }
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
        LauncherToServer::ProbeAgentsResult { request_id, .. } => {
            app_state
                .session_manager
                .complete_probe_request(request_id, msg);
        }
        LauncherToServer::RequestLaunch {
            request_id,
            working_directory,
            session_name,
            claude_args,
            agent_type,
            scheduled_task_id,
            last_session_id,
        } => {
            info!(
                "Launcher requested launch: dir={}, name={:?}",
                working_directory, session_name
            );

            if scheduled_task_id.is_none() {
                if let Some(session_id) = last_session_id {
                    match get_session_paused(app_state, session_id, user_id) {
                        Ok(Some(true)) => {
                            info!(
                                "Skipping launcher auto-resume for paused session {}",
                                session_id
                            );
                            return;
                        }
                        Ok(Some(false)) => {}
                        Ok(None) => {
                            info!(
                                "Skipping launcher auto-resume for deleted session {}",
                                session_id
                            );
                            return;
                        }
                        Err(e) => {
                            warn!(
                                "Failed to check pause state for session {} before launch: {}",
                                session_id, e
                            );
                        }
                    }
                }
            }
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
                        scheduled_task_id,
                        resume_session_id: last_session_id,
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

            let Ok(mut db_conn) = app_state.db_pool.get() else {
                return;
            };

            // Ownership precheck: only this launcher (for its own user) may
            // inject input into a session it itself spawned. See #782.
            if authorize_launcher_session(&mut db_conn, launcher_id, user_id, session_id).is_err() {
                return;
            }

            let session_key = session_id.to_string();
            let content_value = serde_json::Value::String(content);

            // Set sender attribution to "Scheduler"
            app_state
                .session_manager
                .last_input_sender
                .insert(session_id, (user_id, "Scheduler".to_string()));

            // Sequence and send (same pipeline as web client input)
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
                    send_mode: None,
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
        LauncherToServer::ContinuationFired {
            continuation_id,
            session_id,
        } => {
            super::continuations::mark_continuation_fired(
                app_state,
                launcher_id,
                user_id,
                continuation_id,
                session_id,
            );
        }
        LauncherToServer::ContinuationDropped {
            continuation_id,
            session_id,
            reason,
        } => {
            warn!(
                "Continuation {} for session {} dropped by launcher {}: {}",
                continuation_id, session_id, launcher_id, reason
            );
            super::continuations::mark_continuation_dropped(
                app_state,
                launcher_id,
                user_id,
                continuation_id,
                session_id,
                reason,
            );
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

            // Auto-delete completed scheduled sessions to avoid cluttering the UI.
            // Costs are preserved in deleted_session_costs.
            let Ok(mut db_conn) = app_state.db_pool.get() else {
                return;
            };

            // Ownership precheck: only this launcher (for its own user) may
            // mark its own scheduled run complete. The session must also have
            // been spawned for this exact task — otherwise a launcher could
            // pass a sibling user's session UUID with its own task_id. See #782.
            let session =
                match authorize_launcher_session(&mut db_conn, launcher_id, user_id, session_id) {
                    Ok(s) => s,
                    Err(_) => return,
                };

            if session.scheduled_task_id != Some(task_id) {
                warn!(
                    "Launcher {} (user {}) reported ScheduledRunCompleted with task {} \
                     for session {} (its scheduled_task_id is {:?}) — denied",
                    launcher_id, user_id, task_id, session_id, session.scheduled_task_id
                );
                return;
            }

            if let Err(e) =
                super::super::helpers::delete_session_with_data(&mut db_conn, &session, true)
            {
                error!(
                    "Failed to auto-delete scheduled session {}: {:?}",
                    session_id, e
                );
            } else {
                info!("Auto-deleted completed scheduled session {}", session_id);
            }
        }
        LauncherToServer::LauncherRegister { .. } => {}
    }
}

fn get_session_paused(
    app_state: &AppState,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<Option<bool>, String> {
    use crate::schema::sessions;

    let mut conn = app_state.db_pool.get().map_err(|e| e.to_string())?;
    sessions::table
        .find(session_id)
        .filter(sessions::user_id.eq(user_id))
        .select(sessions::paused)
        .first::<bool>(&mut conn)
        .optional()
        .map_err(|e| e.to_string())
}

fn reconcile_desired_sessions(app_state: &AppState, launcher_id: Uuid, user_id: Uuid) {
    use crate::models::Session;
    use crate::schema::sessions;
    use diesel::prelude::*;

    let running_sessions = app_state
        .session_manager
        .launchers
        .get(&launcher_id)
        .map(|launcher| launcher.running_sessions.clone())
        .unwrap_or_default();

    let Ok(mut conn) = app_state.db_pool.get() else {
        warn!("Failed to get DB connection for desired-session reconciliation");
        return;
    };

    let desired: Vec<Session> = match sessions::table
        .filter(sessions::user_id.eq(user_id))
        .filter(sessions::launcher_id.eq(launcher_id))
        .filter(sessions::paused.eq(false))
        .filter(sessions::scheduled_task_id.is_null())
        .filter(sessions::status.ne(SessionStatus::Replaced.as_str()))
        .select(Session::as_select())
        .load(&mut conn)
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(
                "Failed to load desired sessions for launcher {}: {}",
                launcher_id, e
            );
            return;
        }
    };

    for session in desired {
        if running_sessions.contains(&session.id)
            || app_state
                .session_manager
                .pending_launch_sessions
                .iter()
                .any(|entry| *entry.value() == session.id)
        {
            continue;
        }

        let Ok(auth_token) = crate::handlers::launchers::mint_launch_token(app_state, user_id)
        else {
            warn!("Failed to mint token for desired session {}", session.id);
            continue;
        };

        let request_id = Uuid::new_v4();
        app_state
            .session_manager
            .pending_launch_sessions
            .insert(request_id, session.id);

        let claude_args =
            serde_json::from_value::<Vec<String>>(session.claude_args.clone()).unwrap_or_default();
        let agent_type = session
            .agent_type
            .parse()
            .unwrap_or(shared::AgentType::Claude);

        let launch_msg = ServerToLauncher::LaunchSession {
            request_id,
            user_id,
            auth_token,
            working_directory: session.working_directory.clone(),
            session_name: Some(session.session_name.clone()),
            claude_args,
            agent_type,
            scheduled_task_id: None,
            resume_session_id: Some(session.id),
        };

        if !app_state
            .session_manager
            .send_to_launcher(&launcher_id, launch_msg)
        {
            app_state
                .session_manager
                .pending_launch_sessions
                .remove(&request_id);
            warn!(
                "Failed to send desired session {} to launcher {}",
                session.id, launcher_id
            );
        }
    }
}

fn get_dev_user_id(app_state: &AppState) -> Uuid {
    let mut conn = app_state.db_pool.get().expect("DB connection for dev mode");
    crate::auth::dev_user(&mut conn)
        .expect("Test user must exist in dev mode")
        .id
}
