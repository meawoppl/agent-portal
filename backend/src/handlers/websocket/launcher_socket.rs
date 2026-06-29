use axum::extract::ws::WebSocket;
use diesel::prelude::*;
use shared::{
    LauncherEndpoint, LauncherToServer, ServerToClient, ServerToLauncher, ServerToProxy,
    SessionStatus,
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
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
    crate::handlers::scheduled_tasks::send_initial_schedule_sync(
        &app_state,
        user_id,
        launcher_id,
        &hostname,
        &launcher_name,
    );

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
                        // Record the failure WITHOUT pausing. Pausing here used
                        // to wedge the session permanently: `paused` doubles as
                        // the user's "don't relaunch" flag, and reconcile only
                        // ever relaunches `paused = false` sessions, so a single
                        // transient launch failure removed the session from
                        // reconcile forever (#1045). Instead we bump a failure
                        // counter and timestamp; reconcile keeps retrying with
                        // exponential backoff, and a successful registration
                        // resets the counter.
                        if let Err(e) = diesel::update(sessions::table.find(session_id))
                            .set((
                                sessions::launch_failure_count
                                    .eq(sessions::launch_failure_count + 1),
                                sessions::last_launch_attempt_at.eq(diesel::dsl::now),
                                sessions::status.eq(SessionStatus::Disconnected.as_str()),
                                // Release the launch lease so backoff (above) is
                                // the sole gate on the next retry.
                                sessions::launch_lease_until.eq(None::<chrono::NaiveDateTime>),
                                sessions::updated_at.eq(diesel::dsl::now),
                            ))
                            .execute(&mut conn)
                        {
                            warn!("Failed to record launch failure for {}: {}", session_id, e);
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
            reason,
        } => {
            info!(
                "Proxy exited: session={}, code={:?}, reason={:?}",
                session_id, exit_code, reason
            );
            if let Ok(mut conn) = app_state.db_pool.get() {
                // The proxy holding this session's token has exited; revoke the
                // token so it can't be reused. A resume mints a fresh one. See #932.
                crate::handlers::proxy_tokens::revoke_tokens_for_session(&mut conn, session_id);
                // Throttle crash loops: a session that registered then exited
                // near-instantly, or whose resume target was gone, bumps the
                // launch-failure counter so reconcile backs off; a healthy run
                // resets it. See #1045 and the crash-loop investigation.
                record_exit_for_backoff(&mut conn, session_id, reason);
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
            app_state.session_manager.set_last_input_sender(
                session_id,
                user_id,
                "Scheduler".to_string(),
            );

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
                        client_msg_id: None,
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

    let now = chrono::Utc::now().naive_utc();

    for session in desired {
        // Skip sessions the launcher last reported as running. The launch lease
        // below (not the in-memory pending set) is the authoritative guard for
        // the in-flight window.
        if running_sessions.contains(&session.id) {
            continue;
        }

        // Back off relaunching sessions that have recently failed to launch.
        // Without this, a session that can never launch (e.g. a missing
        // working directory) would be relaunched on every heartbeat, minting
        // a fresh token each time (#1045). A successful registration resets
        // `launch_failure_count` to 0, so this only throttles the failing case.
        if let Some(last_attempt) = session.last_launch_attempt_at {
            let backoff = launch_backoff(session.launch_failure_count);
            if now < last_attempt + backoff {
                debug!(
                    "Reconcile skipping session {} on launcher {}: backoff \
                     (launch_failure_count={}, {}s until next attempt)",
                    session.id,
                    launcher_id,
                    session.launch_failure_count,
                    (last_attempt + backoff - now).num_seconds()
                );
                continue;
            }
        }

        // Atomically claim a short launch lease before sending. This conditional
        // UPDATE is the single source of truth for "a launch is in flight": only
        // one reconcile (across heartbeats, or even backend instances) can flip a
        // NULL/expired lease to a fresh deadline, so it closes the double-launch
        // race the eventually-consistent running/pending sets left open. The
        // lease self-expires (LAUNCH_LEASE_SECS), so a launcher that dies
        // mid-launch doesn't wedge the session out of reconcile — unlike the old
        // no-TTL pending set. Registration and launch-failure both clear it.
        let lease_until = now + chrono::Duration::seconds(LAUNCH_LEASE_SECS);
        match diesel::update(
            sessions::table.find(session.id).filter(
                sessions::launch_lease_until
                    .is_null()
                    .or(sessions::launch_lease_until.lt(now)),
            ),
        )
        .set(sessions::launch_lease_until.eq(Some(lease_until)))
        .execute(&mut conn)
        {
            Ok(1) => {}
            Ok(_) => {
                debug!(
                    "Reconcile skipping session {} on launcher {}: launch lease held",
                    session.id, launcher_id
                );
                continue;
            }
            Err(e) => {
                warn!("Failed to claim launch lease for {}: {}", session.id, e);
                continue;
            }
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

        info!(
            "Reconcile relaunching session {} ({}) on launcher {}: resume, \
             launch_failure_count={}, dir={}",
            session.id,
            session.session_name,
            launcher_id,
            session.launch_failure_count,
            session.working_directory
        );

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

/// Adjust a session's launch-backoff state from why its proxy exited.
///
/// `CrashedEarly` / `ResumeTargetMissing` bump `launch_failure_count` (so
/// `launch_backoff` throttles the next relaunch); a healthy `Completed` run
/// resets it. Other reasons are neutral. This is the counterpart to the
/// launch-failure bump in the `LaunchSessionResult` handler — together they make
/// `launch_failure_count` reflect *runtime* health, not just whether the spawn
/// itself succeeded, which is why the reset moved off registration (a crash loop
/// registers every time). See the crash-loop investigation.
fn record_exit_for_backoff(
    conn: &mut diesel::PgConnection,
    session_id: Uuid,
    reason: shared::SessionExitReason,
) {
    use crate::schema::sessions;
    use diesel::prelude::*;
    use shared::SessionExitReason::*;

    let result = match reason {
        CrashedEarly | ResumeTargetMissing => diesel::update(sessions::table.find(session_id))
            .set((
                sessions::launch_failure_count.eq(sessions::launch_failure_count + 1),
                sessions::last_launch_attempt_at.eq(diesel::dsl::now),
                sessions::updated_at.eq(diesel::dsl::now),
            ))
            .execute(conn),
        Completed => diesel::update(sessions::table.find(session_id))
            .set((
                sessions::launch_failure_count.eq(0),
                sessions::updated_at.eq(diesel::dsl::now),
            ))
            .execute(conn),
        RegistrationRejected | Stopped | Error => Ok(0),
    };
    if let Err(e) = result {
        warn!(
            "Failed to record exit backoff for session {}: {}",
            session_id, e
        );
    }
}

/// How long a reconcile launch lease is held before it self-expires. Covers the
/// window between sending `LaunchSession` and the proxy registering (or the
/// launch failing). Long enough to outlast a normal spawn+register, short enough
/// that a launcher dying mid-launch frees the session for retry promptly.
const LAUNCH_LEASE_SECS: i64 = 60;

/// Exponential backoff between relaunch attempts for a session that keeps
/// failing to launch: 30s, 1m, 2m, … capped at 15 minutes. `failure_count`
/// is the number of consecutive failures so far (0 means "never failed", so
/// no backoff). See #1045.
fn launch_backoff(failure_count: i32) -> chrono::Duration {
    if failure_count <= 0 {
        return chrono::Duration::zero();
    }
    // Cap the shift so `30 << n` can't overflow, then cap the result at 15min.
    let shift = (failure_count - 1).min(5) as u32;
    let secs = (30u64 << shift).min(900);
    chrono::Duration::seconds(secs as i64)
}

fn get_dev_user_id(app_state: &AppState) -> Uuid {
    let mut conn = app_state.db_pool.get().expect("DB connection for dev mode");
    crate::auth::dev_user(&mut conn)
        .expect("Test user must exist in dev mode")
        .id
}

#[cfg(test)]
mod tests {
    use super::launch_backoff;

    #[test]
    fn backoff_is_zero_until_first_failure() {
        // Never-failed sessions (count 0, or a defensive negative) get no
        // backoff so reconcile launches them immediately.
        assert_eq!(launch_backoff(0).num_seconds(), 0);
        assert_eq!(launch_backoff(-3).num_seconds(), 0);
    }

    #[test]
    fn backoff_doubles_then_caps_at_15_minutes() {
        assert_eq!(launch_backoff(1).num_seconds(), 30);
        assert_eq!(launch_backoff(2).num_seconds(), 60);
        assert_eq!(launch_backoff(3).num_seconds(), 120);
        assert_eq!(launch_backoff(4).num_seconds(), 240);
        assert_eq!(launch_backoff(5).num_seconds(), 480);
        // 30 << 5 = 960, capped to the 900s (15min) ceiling.
        assert_eq!(launch_backoff(6).num_seconds(), 900);
    }

    #[test]
    fn backoff_saturates_without_overflow_for_large_counts() {
        // The shift is clamped, so even an absurd failure count stays at the
        // cap rather than overflowing the left-shift.
        assert_eq!(launch_backoff(1000).num_seconds(), 900);
        assert_eq!(launch_backoff(i32::MAX).num_seconds(), 900);
    }
}
