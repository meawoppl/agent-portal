use axum::extract::ws::WebSocket;
use diesel::prelude::*;
use shared::{
    LauncherEndpoint, LauncherRejectReason, LauncherToServer, ServerToClient, ServerToLauncher,
    ServerToProxy, SessionStatus,
};
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::LauncherConnection;
use crate::{
    handlers::proxy_tokens::{
        issue_proxy_token, verify_and_get_user_with_token, TokenPersist, VerifiedProxyToken,
    },
    schema::proxy_auth_tokens,
    AppState,
};

const LAUNCHER_TOKEN_TTL_DAYS: u32 = 30;
const TOKEN_REFRESH_GRACE_MINUTES: i64 = 10;
const TOKEN_REFRESH_MINT_COOLDOWN_HOURS: i64 = 1;

struct PendingTokenRefresh {
    old_token_id: Uuid,
    auth_token: String,
}

fn launcher_token_needs_refresh(
    created_at: chrono::NaiveDateTime,
    expires_at: Option<chrono::NaiveDateTime>,
    now: chrono::NaiveDateTime,
) -> bool {
    let Some(expires_at) = expires_at else {
        return true;
    };

    let lifetime_secs = expires_at.signed_duration_since(created_at).num_seconds();
    if lifetime_secs <= 0 {
        return true;
    }

    let age_secs = now.signed_duration_since(created_at).num_seconds().max(0);
    age_secs >= lifetime_secs / 2
}

fn token_grace_expires_at(now: chrono::NaiveDateTime) -> chrono::NaiveDateTime {
    now + chrono::Duration::minutes(TOKEN_REFRESH_GRACE_MINUTES)
}

fn recent_refresh_cutoff(now: chrono::NaiveDateTime) -> chrono::NaiveDateTime {
    now - chrono::Duration::hours(TOKEN_REFRESH_MINT_COOLDOWN_HOURS)
}

fn maybe_issue_launcher_token_refresh(
    app_state: &AppState,
    conn: &mut diesel::PgConnection,
    verified: &VerifiedProxyToken,
) -> Option<PendingTokenRefresh> {
    let now = chrono::Utc::now().naive_utc();
    if !launcher_token_needs_refresh(verified.token.created_at, verified.token.expires_at, now) {
        return None;
    }

    match launcher_recently_minted_same_name_token(conn, verified, now) {
        Ok(true) => {
            info!(
                "Skipping launcher token refresh for user {} token {}: same-name token minted within {}h",
                verified.user_id,
                verified.token.id,
                TOKEN_REFRESH_MINT_COOLDOWN_HOURS
            );
            return None;
        }
        Ok(false) => {}
        Err(e) => {
            warn!(
                "Could not check launcher token refresh cooldown for user {} token {}: {}",
                verified.user_id, verified.token.id, e
            );
            return None;
        }
    }

    match issue_proxy_token(
        conn,
        app_state.jwt_secret.as_bytes(),
        verified.user_id,
        TokenPersist::Create {
            name: &verified.token.name,
        },
        Some(LAUNCHER_TOKEN_TTL_DAYS),
    ) {
        Ok(issued) => {
            info!(
                "Issued refreshed launcher token row {} for user {} (old row {})",
                issued.row_id, verified.user_id, verified.token.id
            );
            Some(PendingTokenRefresh {
                old_token_id: verified.token.id,
                auth_token: issued.token,
            })
        }
        Err(e) => {
            warn!(
                "Launcher token refresh skipped for user {} token {}: {:?}",
                verified.user_id, verified.token.id, e
            );
            None
        }
    }
}

fn launcher_recently_minted_same_name_token(
    conn: &mut diesel::PgConnection,
    verified: &VerifiedProxyToken,
    now: chrono::NaiveDateTime,
) -> Result<bool, diesel::result::Error> {
    let cutoff = recent_refresh_cutoff(now);
    proxy_auth_tokens::table
        .filter(proxy_auth_tokens::user_id.eq(verified.user_id))
        .filter(proxy_auth_tokens::name.eq(&verified.token.name))
        .filter(proxy_auth_tokens::revoked.eq(false))
        .filter(proxy_auth_tokens::id.ne(verified.token.id))
        .filter(proxy_auth_tokens::created_at.ge(cutoff))
        .select(proxy_auth_tokens::id)
        .first::<Uuid>(conn)
        .optional()
        .map(|row| row.is_some())
}

fn grace_old_launcher_token(app_state: &AppState, token_id: Uuid) {
    let Ok(mut conn) = app_state.db_pool.get() else {
        warn!(
            "Could not grace old launcher token {}: database pool unavailable",
            token_id
        );
        return;
    };
    let expires_at = token_grace_expires_at(chrono::Utc::now().naive_utc());
    match diesel::update(
        proxy_auth_tokens::table
            .filter(proxy_auth_tokens::id.eq(token_id))
            .filter(proxy_auth_tokens::revoked.eq(false)),
    )
    .set(proxy_auth_tokens::expires_at.eq(Some(expires_at)))
    .execute(&mut conn)
    {
        Ok(0) => warn!(
            "Launcher token refresh ack referenced unknown/revoked token {}",
            token_id
        ),
        Ok(_) => info!(
            "Old launcher token {} now expires after {} minute grace",
            token_id, TOKEN_REFRESH_GRACE_MINUTES
        ),
        Err(e) => warn!("Failed to grace old launcher token {}: {}", token_id, e),
    }
}

pub async fn handle_launcher_socket(socket: WebSocket, app_state: Arc<AppState>) {
    let conn = ws_bridge::server::into_connection::<LauncherEndpoint>(socket);
    let (mut ws_sender, mut ws_receiver) = conn.split();

    // Wait for LauncherRegister message
    let (
        launcher_id,
        launcher_name,
        hostname,
        user_id,
        working_directory,
        version,
        capabilities,
        pending_token_refresh,
    ) = loop {
        match ws_receiver.recv().await {
            Some(Ok(LauncherToServer::LauncherRegister {
                launcher_id,
                launcher_name,
                auth_token,
                hostname,
                working_directory,
                version,
                capabilities,
            })) => {
                // Authenticate and, for live launcher credentials, opportunistically
                // rotate legacy non-expiring or half-lived tokens while the
                // websocket is healthy (#1237 part 2).
                let (user_id, pending_token_refresh) = if let Some(ref token) = auth_token {
                    match app_state.db_pool.get() {
                        Ok(mut conn) => {
                            match verify_and_get_user_with_token(&app_state, &mut conn, token) {
                                Ok(verified) => {
                                    info!(
                                        "Launcher authenticated as {} ({})",
                                        verified.email, verified.user_id
                                    );
                                    let refresh = maybe_issue_launcher_token_refresh(
                                        &app_state, &mut conn, &verified,
                                    );
                                    (verified.user_id, refresh)
                                }
                                Err(crate::errors::AppError::ServiceUnavailable(_)) => {
                                    // Transient DB failure while checking the
                                    // token — the token was never evaluated.
                                    // Must not be fatal: a fatal AuthFailed
                                    // parks the launcher for a deploy blip
                                    // (#1264). Non-fatal → normal reconnect
                                    // backoff.
                                    let _ = ws_sender
                                        .send(ServerToLauncher::LauncherRegisterAck {
                                            success: false,
                                            fatal: false,
                                            launcher_id,
                                            error: Some(
                                                "Server temporarily unavailable - retrying"
                                                    .to_string(),
                                            ),
                                            reject_reason: None,
                                        })
                                        .await;
                                    return;
                                }
                                Err(_) => {
                                    if app_state.dev_mode {
                                        (get_dev_user_id(&app_state), None)
                                    } else {
                                        let _ = ws_sender
                                            .send(ServerToLauncher::LauncherRegisterAck {
                                                success: false,
                                                fatal: true,
                                                launcher_id,
                                                error: Some("Authentication failed".to_string()),
                                                reject_reason: Some(
                                                    LauncherRejectReason::AuthFailed,
                                                ),
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
                                    reject_reason: None,
                                })
                                .await;
                            return;
                        }
                    }
                } else if app_state.dev_mode {
                    (get_dev_user_id(&app_state), None)
                } else {
                    let _ = ws_sender
                        .send(ServerToLauncher::LauncherRegisterAck {
                            success: false,
                            fatal: true,
                            launcher_id,
                            error: Some("No auth token provided".to_string()),
                            reject_reason: Some(LauncherRejectReason::AuthFailed),
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
                    capabilities,
                    pending_token_refresh,
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
                reject_reason: Some(LauncherRejectReason::TooManyLaunchers),
            })
            .await;
        return;
    }

    // Create channel for sending messages to this launcher
    let (tx, mut rx) =
        super::session_manager::conn_channel::<ServerToLauncher>(super::LAUNCHER_CHANNEL_CAPACITY);
    let tx_for_sync = tx.clone();

    // Server-side kill switch: fired by the SessionManager if it evicts this
    // connection (channel dead / half-open socket, #1256) so the select loop
    // below exits and the launcher's reconnect logic takes over.
    let cancel = tokio_util::sync::CancellationToken::new();

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
            capabilities,
            cancel: cancel.clone(),
            gen: 0, // stamped by try_register_launcher
            last_seen: std::sync::atomic::AtomicU64::new(0), // stamped by try_register_launcher
        },
    );

    let connection_gen = match register_result {
        Ok(gen) => gen,
        Err(existing_name) => {
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
                    reject_reason: Some(LauncherRejectReason::DuplicateLauncher),
                })
                .await;
            return;
        }
    };

    // Send RegisterAck
    let _ = ws_sender
        .send(ServerToLauncher::LauncherRegisterAck {
            success: true,
            launcher_id,
            error: None,
            fatal: false,
            reject_reason: None,
        })
        .await;

    let mut token_refresh_old_id = pending_token_refresh
        .as_ref()
        .map(|refresh| refresh.old_token_id);
    if let Some(refresh) = pending_token_refresh {
        if tx_for_sync
            .send(ServerToLauncher::TokenRefresh {
                auth_token: refresh.auth_token,
            })
            .is_err()
        {
            warn!("Failed to queue token refresh for launcher {}", launcher_id);
        }
    }

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

    let continuation_configs = super::continuations::load_scheduled_continuations(
        &app_state.db_pool,
        launcher_id,
        user_id,
    );
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
                        // Liveness stamp: any decodable inbound frame proves
                        // the transport is alive (see liveness.rs).
                        app_state.session_manager.touch_launcher(&launcher_id);
                        let token_refresh_ack_old_id =
                            if matches!(&msg, LauncherToServer::TokenRefreshAck) {
                                token_refresh_old_id.take()
                            } else {
                                token_refresh_old_id
                            };
                        handle_launcher_message(
                            msg,
                            launcher_id,
                            user_id,
                            token_refresh_ack_old_id,
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

            // Evicted by the SessionManager (dead channel / half-open
            // socket, #1256): close the transport so the launcher's
            // reconnect logic takes over.
            _ = cancel.cancelled() => {
                warn!(
                    "Launcher '{}' connection force-closed by server (evicted)",
                    launcher_name
                );
                break;
            }
        }
    }

    // Generation-guarded: a reconnect reuses the same launcher_id, so this
    // stale socket's cleanup must not remove the successor's registration.
    app_state
        .session_manager
        .unregister_launcher(&launcher_id, Some(connection_gen));
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
    token_refresh_old_id: Option<Uuid>,
    app_state: &AppState,
) {
    match msg {
        LauncherToServer::TokenRefreshAck => {
            if let Some(old_token_id) = token_refresh_old_id {
                grace_old_launcher_token(app_state, old_token_id);
            } else {
                warn!(
                    "Launcher {} sent TokenRefreshAck without a pending refresh",
                    launcher_id
                );
            }
        }
        LauncherToServer::LaunchSessionResult {
            request_id,
            success,
            session_id,
            pid,
            ref error,
        } => {
            let desired_session_id = app_state.session_manager.take_launch_session(request_id);
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
                        // Read prior status + name so we only push on a genuine
                        // active → disconnected drop (mobile-apps plan §8.1); a
                        // launch failure on a not-yet-active session is not a
                        // "your running session dropped" event.
                        let prior: Option<(String, String)> = sessions::table
                            .find(session_id)
                            .select((sessions::status, sessions::session_name))
                            .first::<(String, String)>(&mut conn)
                            .ok();
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
                        } else if let Some((status, session_name)) = prior {
                            if status == SessionStatus::Active.as_str() {
                                app_state.notifications.emit(
                                    crate::push::NotificationEvent::SessionDisconnected {
                                        session_id,
                                        session_name,
                                    },
                                );
                            }
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
            continuation_id,
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
                            if let Some(continuation_id) = continuation_id {
                                super::continuations::mark_continuation_dropped(
                                    app_state,
                                    launcher_id,
                                    user_id,
                                    continuation_id,
                                    session_id,
                                    "session was paused before continuation relaunch".to_string(),
                                );
                            }
                            return;
                        }
                        Ok(Some(false)) => {}
                        Ok(None) => {
                            info!(
                                "Skipping launcher auto-resume for deleted session {}",
                                session_id
                            );
                            if let Some(continuation_id) = continuation_id {
                                super::continuations::mark_continuation_dropped(
                                    app_state,
                                    launcher_id,
                                    user_id,
                                    continuation_id,
                                    session_id,
                                    "session was deleted before continuation relaunch".to_string(),
                                );
                            }
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
                        create_worktree: false,
                        worktree_branch: None,
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
                    client_msg_id: None,
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

            // Continue-mode tasks resume the same conversation on the next
            // firing, so the session (and, for claude, its on-disk transcript)
            // must survive completion. Only fresh-mode runs are auto-deleted.
            let is_continue = {
                use crate::schema::scheduled_tasks;
                scheduled_tasks::table
                    .filter(scheduled_tasks::id.eq(task_id))
                    .filter(scheduled_tasks::user_id.eq(user_id))
                    .select(scheduled_tasks::session_mode)
                    .first::<String>(&mut db_conn)
                    .ok()
                    .and_then(|m| m.parse::<shared::SessionMode>().ok())
                    == Some(shared::SessionMode::Continue)
            };
            if is_continue {
                info!(
                    "Preserving continue-mode scheduled session {} (task {}) for resume",
                    session_id, task_id
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
            .register_launch_session(request_id, session.id);

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
            create_worktree: false,
            worktree_branch: None,
        };

        if !app_state
            .session_manager
            .send_to_launcher(&launcher_id, launch_msg)
        {
            app_state.session_manager.cancel_launch_session(request_id);
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
    use super::{
        launch_backoff, launcher_token_needs_refresh, recent_refresh_cutoff, token_grace_expires_at,
    };

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

    #[test]
    fn launcher_token_refreshes_legacy_non_expiring_tokens() {
        let created = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let now = created + chrono::Duration::days(1);

        assert!(launcher_token_needs_refresh(created, None, now));
    }

    #[test]
    fn launcher_token_refreshes_after_half_life() {
        let created = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let expires = created + chrono::Duration::days(30);

        assert!(!launcher_token_needs_refresh(
            created,
            Some(expires),
            created + chrono::Duration::days(14)
        ));
        assert!(launcher_token_needs_refresh(
            created,
            Some(expires),
            created + chrono::Duration::days(15)
        ));
    }

    #[test]
    fn launcher_token_refreshes_malformed_expiry() {
        let created = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();

        assert!(launcher_token_needs_refresh(
            created,
            Some(created),
            created + chrono::Duration::seconds(1)
        ));
    }

    #[test]
    fn token_refresh_ack_graces_old_token_for_ten_minutes() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();

        assert_eq!(
            token_grace_expires_at(now),
            now + chrono::Duration::minutes(10)
        );
    }

    #[test]
    fn token_refresh_cooldown_looks_back_one_hour() {
        let now = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();

        assert_eq!(recent_refresh_cutoff(now), now - chrono::Duration::hours(1));
    }
}
