use crate::path_policy;
use crate::process_manager::{ProcessManager, SessionExited, SpawnParams};
use crate::scheduler::{PendingLaunchKind, Scheduler};
use shared::{LauncherEndpoint, LauncherRejectReason, LauncherToServer, ServerToLauncher};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

const HEARTBEAT_INTERVAL: Duration =
    Duration::from_secs(shared::protocol::LAUNCHER_HEARTBEAT_INTERVAL_SECS);
const MAX_BACKOFF: Duration = Duration::from_secs(shared::protocol::MAX_RECONNECT_BACKOFF_SECS);
/// Retry cadence while parked on a fatal rejection: start here, double to the
/// cap. Deliberately much slower than the network-error backoff — a fatal
/// rejection won't clear until something changes (a re-login, a launcher
/// stopping elsewhere), and hammering registration was the #1237 crash loop.
const PARKED_BACKOFF_START: Duration = Duration::from_secs(60);
const PARKED_BACKOFF_MAX: Duration = Duration::from_secs(600);

/// How the launcher should respond to a fatal registration rejection.
/// Prefers the backend's machine-readable reason; falls back to sniffing the
/// error text for old backends that predate `reject_reason`.
fn classify_fatal(reason: Option<LauncherRejectReason>, error_msg: &str) -> LauncherRejectReason {
    if let Some(reason) = reason {
        return reason;
    }
    let msg = error_msg.to_ascii_lowercase();
    if msg.contains("auth") || msg.contains("token") {
        LauncherRejectReason::AuthFailed
    } else if msg.contains("already have") {
        LauncherRejectReason::TooManyLaunchers
    } else {
        LauncherRejectReason::DuplicateLauncher
    }
}

/// The one-line operator instruction for a parked launcher.
fn parked_instruction(reason: LauncherRejectReason) -> &'static str {
    match reason {
        LauncherRejectReason::AuthFailed => {
            "Authentication failed — run `agent-portal login` to re-authenticate. \
             The launcher will keep retrying slowly and recover on its own once \
             you log in."
        }
        LauncherRejectReason::TooManyLaunchers => {
            "Launcher limit reached — disconnect another launcher. Retrying slowly."
        }
        LauncherRejectReason::DuplicateLauncher => {
            "Another launcher is already connected from this host — stop it (or \
             this one). Retrying slowly."
        }
    }
}

pub async fn run_launcher_loop(
    backend_url: &str,
    launcher_id: Uuid,
    launcher_name: &str,
    cli_auth_token: Option<String>,
    mut process_manager: ProcessManager,
    mut exit_rx: mpsc::UnboundedReceiver<SessionExited>,
) -> anyhow::Result<()> {
    process_manager.set_launcher_id(launcher_id);
    let mut backoff = Duration::from_secs(1);
    let mut parked_backoff = PARKED_BACKOFF_START;
    // The reason whose instruction was last shown; re-emit when it changes so
    // an operator who fixes auth and then hits the launcher limit sees the new
    // guidance, not just "Still parked".
    let mut last_parked_reason: Option<LauncherRejectReason> = None;
    let mut scheduler = Scheduler::new();

    loop {
        // Re-resolve the token every attempt (CLI flag still wins): a parked
        // launcher self-heals the moment `agent-portal login` writes a fresh
        // token to launcher.json — no service restart needed (#1237).
        let auth_token = cli_auth_token
            .clone()
            .or_else(|| crate::config::load_config().auth_token);

        info!("Connecting to backend: {}", backend_url);

        match ws_bridge::native_client::connect::<LauncherEndpoint>(backend_url).await {
            Ok(conn) => {
                info!("Connected to backend");
                backoff = Duration::from_secs(1);

                let (mut ws_sender, mut ws_receiver) = conn.split();

                // Send registration
                let register = LauncherToServer::LauncherRegister {
                    launcher_id,
                    launcher_name: launcher_name.to_string(),
                    auth_token: auth_token.clone(),
                    hostname: claude_session_lib::hostname_or_unknown(),
                    version: Some(shared::VERSION.to_string()),
                    working_directory: std::env::current_dir()
                        .ok()
                        .map(|p| p.to_string_lossy().to_string()),
                    capabilities: vec![shared::LAUNCHER_CAPABILITY_CREATE_WORKTREE.to_string()],
                };
                if ws_sender.send(register).await.is_err() {
                    warn!("Failed to send registration");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }

                // Wait for RegisterAck. `Err(reason)` = fatal rejection.
                let ack_ok = loop {
                    match ws_receiver.recv().await {
                        Some(Ok(ServerToLauncher::LauncherRegisterAck {
                            success,
                            error,
                            fatal,
                            reject_reason,
                            ..
                        })) => {
                            if success {
                                info!("Registration successful");
                                break Ok(true);
                            } else {
                                let msg = error.unwrap_or_default();
                                if fatal {
                                    error!("Registration rejected (fatal): {}", msg);
                                    break Err(classify_fatal(reject_reason, &msg));
                                } else {
                                    error!("Registration failed: {}", msg);
                                    break Ok(false);
                                }
                            }
                        }
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => {
                            warn!("Decode error during registration: {}", e);
                            continue;
                        }
                        None => break Ok(false),
                    }
                };

                match ack_ok {
                    Err(reason) => {
                        // Fatal rejection — park with a slow retry instead of
                        // exiting. Exiting under systemd's Restart=on-failure
                        // was a 5s crash loop (76k restarts on one expired
                        // token, #1237); parking keeps one warm process that
                        // recovers by itself when the cause clears — the token
                        // re-read above picks up an `agent-portal login`
                        // without a restart.
                        if last_parked_reason != Some(reason) {
                            error!("{}", parked_instruction(reason));
                            last_parked_reason = Some(reason);
                        } else {
                            info!(
                                "Still parked ({:?}); retrying in {:?}",
                                reason, parked_backoff
                            );
                        }
                        tokio::time::sleep(parked_backoff).await;
                        parked_backoff = (parked_backoff * 2).min(PARKED_BACKOFF_MAX);
                        continue;
                    }
                    Ok(false) => {
                        warn!("Registration failed, will retry");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        continue;
                    }
                    Ok(true) => {
                        // Healthy again: reset the parked state so a future
                        // rejection logs its instruction afresh.
                        parked_backoff = PARKED_BACKOFF_START;
                        last_parked_reason = None;
                    }
                }

                // Main loop
                let mut heartbeat_timer = tokio::time::interval(HEARTBEAT_INTERVAL);
                let start = Instant::now();

                loop {
                    let sched_dur = scheduler
                        .next_fire_duration()
                        .unwrap_or(Duration::from_secs(3600));
                    let prompt_dur = scheduler
                        .next_prompt_duration()
                        .unwrap_or(Duration::from_secs(3600));
                    let continuation_dur = scheduler
                        .next_continuation_duration()
                        .unwrap_or(Duration::from_secs(3600));
                    let sched_sleep = tokio::time::sleep(sched_dur);
                    let prompt_sleep = tokio::time::sleep(prompt_dur);
                    let continuation_sleep = tokio::time::sleep(continuation_dur);
                    tokio::pin!(sched_sleep);
                    tokio::pin!(prompt_sleep);
                    tokio::pin!(continuation_sleep);

                    tokio::select! {
                        result = ws_receiver.recv() => {
                            match result {
                                Some(Ok(msg)) => {
                                    handle_message(
                                        msg,
                                        &mut ws_sender,
                                        &mut process_manager,
                                        &mut scheduler,
                                    ).await;
                                }
                                Some(Err(e)) => {
                                    warn!("Decode error: {}", e);
                                    continue;
                                }
                                None => {
                                    info!("WebSocket closed by server");
                                    break;
                                }
                            }
                        }

                        _ = heartbeat_timer.tick() => {
                            let hb = LauncherToServer::LauncherHeartbeat {
                                launcher_id,
                                running_sessions: process_manager.running_session_ids(),
                                uptime_secs: start.elapsed().as_secs(),
                            };
                            if ws_sender.send(hb).await.is_err() {
                                warn!("Failed to send heartbeat");
                                break;
                            }

                            // Enforce max runtime on scheduled sessions
                            for session_id in scheduler.timed_out_sessions() {
                                process_manager.stop(&session_id).await;
                            }
                        }

                        Some(exited) = exit_rx.recv() => {
                            info!(
                                "Session {} exited with code {:?}",
                                exited.session_id, exited.exit_code
                            );
                            process_manager.remove_finished(&exited.session_id);
                            let msg = LauncherToServer::SessionExited {
                                session_id: exited.session_id,
                                exit_code: exited.exit_code,
                                reason: exited.reason,
                            };
                            if ws_sender.send(msg).await.is_err() {
                                warn!("Failed to send session exited notification");
                                break;
                            }

                            // Report scheduled run completion
                            if let Some(run_info) = scheduler.on_session_exited(&exited.session_id) {
                                let completed = LauncherToServer::ScheduledRunCompleted {
                                    task_id: run_info.task_id,
                                    session_id: exited.session_id,
                                    exit_code: exited.exit_code,
                                    duration_secs: run_info.started_at.elapsed().as_secs(),
                                };
                                if ws_sender.send(completed).await.is_err() {
                                    warn!("Failed to send ScheduledRunCompleted");
                                    break;
                                }
                            }
                        }

                        // Scheduler: fire due tasks
                        _ = &mut sched_sleep => {
                            for task_to_fire in scheduler.fire_due_tasks() {
                                info!(
                                    "Firing scheduled task '{}' ({})",
                                    task_to_fire.config.fields.name, task_to_fire.config.id
                                );
                                let msg = LauncherToServer::RequestLaunch {
                                    request_id: task_to_fire.request_id,
                                    working_directory: task_to_fire.config.fields.working_directory.clone(),
                                    session_name: Some(task_to_fire.config.fields.name.clone()),
                                    claude_args: task_to_fire.config.fields.claude_args.clone(),
                                    agent_type: task_to_fire.config.fields.agent_type,
                                    scheduled_task_id: Some(task_to_fire.config.id),
                                    last_session_id: task_to_fire.config.last_session_id,
                                    continuation_id: None,
                                };
                                if ws_sender.send(msg).await.is_err() {
                                    warn!("Failed to send RequestLaunch for scheduled task");
                                    break;
                                }
                            }
                        }

                        // Scheduler: send pending prompts after delay
                        _ = &mut prompt_sleep => {
                            for (session_id, task_id, content) in scheduler.ready_prompts() {
                                info!(
                                    "Injecting prompt for task {} into session {}",
                                    task_id, session_id
                                );

                                let started = LauncherToServer::ScheduledRunStarted {
                                    task_id,
                                    session_id,
                                };
                                if ws_sender.send(started).await.is_err() {
                                    warn!("Failed to send ScheduledRunStarted");
                                    break;
                                }

                                let inject = LauncherToServer::InjectInput {
                                    session_id,
                                    content,
                                };
                                if ws_sender.send(inject).await.is_err() {
                                    warn!("Failed to send InjectInput");
                                    break;
                                }
                            }
                        }

                        _ = &mut continuation_sleep => {
                            let running = process_manager.running_session_ids();
                            for continuation in scheduler.ready_continuations() {
                                if running.contains(&continuation.session_id) {
                                    info!(
                                        "Injecting continuation {} into still-running session {}",
                                        continuation.id, continuation.session_id
                                    );
                                    let inject = LauncherToServer::InjectInput {
                                        session_id: continuation.session_id,
                                        content: continuation.prompt.clone(),
                                    };
                                    if ws_sender.send(inject).await.is_err() {
                                        warn!("Failed to send continuation InjectInput");
                                        break;
                                    }
                                    let fired = LauncherToServer::ContinuationFired {
                                        continuation_id: continuation.id,
                                        session_id: continuation.session_id,
                                    };
                                    if ws_sender.send(fired).await.is_err() {
                                        warn!("Failed to send ContinuationFired");
                                        break;
                                    }
                                } else if let Some(working_directory) =
                                    continuation.working_directory.clone()
                                {
                                    info!(
                                        "Relaunching session {} for continuation {}",
                                        continuation.session_id, continuation.id
                                    );
                                    let request_id =
                                        scheduler.register_continuation_launch(&continuation);
                                    let relaunch = LauncherToServer::RequestLaunch {
                                        request_id,
                                        working_directory,
                                        session_name: continuation.session_name.clone(),
                                        claude_args: continuation.claude_args.clone(),
                                        agent_type: continuation.agent_type,
                                        scheduled_task_id: None,
                                        last_session_id: Some(continuation.session_id),
                                        continuation_id: Some(continuation.id),
                                    };
                                    if ws_sender.send(relaunch).await.is_err() {
                                        warn!("Failed to send continuation RequestLaunch");
                                        scheduler.clear_pending_launch(&request_id);
                                        break;
                                    }
                                } else {
                                    let reason = "local agent process was no longer running and this launcher did not receive resume metadata".to_string();
                                    warn!(
                                        "Dropping continuation {} for session {}: {}",
                                        continuation.id, continuation.session_id, reason
                                    );
                                    let dropped = LauncherToServer::ContinuationDropped {
                                        continuation_id: continuation.id,
                                        session_id: continuation.session_id,
                                        reason,
                                    };
                                    if ws_sender.send(dropped).await.is_err() {
                                        warn!("Failed to send ContinuationDropped");
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!("Failed to connect: {}", e);
            }
        }

        info!("Reconnecting in {:?}...", backoff);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Probe both supported agent CLIs and convert into the wire shape used in
/// `LauncherToServer::ProbeAgentsResult`. Runs `which::which` + `--version`
/// synchronously, so callers must run this from a `spawn_blocking` task.
fn probe_agents_for_response() -> Vec<shared::AgentInstall> {
    session_lib::probe::probe_all_agents()
        .into_iter()
        .map(|(agent_type, result)| shared::AgentInstall {
            agent_type,
            installed: result.installed,
            resolved_path: result
                .resolved_path
                .map(|p| p.to_string_lossy().to_string()),
            version: result.version,
        })
        .collect()
}

fn list_directory(path: &str, request_id: Uuid) -> LauncherToServer {
    // Resolve ~ to home directory (trailing slash ensures the dir itself is listed,
    // not treated as a filter prefix against its parent)
    let resolved_path = match path_policy::expand_home_path(path) {
        Ok(path) => path,
        Err(e) => {
            return LauncherToServer::ListDirectoriesResult {
                request_id,
                entries: vec![],
                error: Some(e.to_string()),
                resolved_path: None,
            };
        }
    };
    let resolved = resolved_path.to_string_lossy().to_string();

    // Split into (dir_to_list, filter_prefix)
    // If the path ends with '/', list the directory with no filter
    // Otherwise, treat the last component as a prefix filter
    let (dir_path, filter) = if path.ends_with('/') || path == "~" || path == "~/" {
        (resolved.as_str(), "")
    } else {
        let p = std::path::Path::new(&resolved);
        match (p.parent(), p.file_name()) {
            (Some(parent), Some(fname)) => {
                (parent.to_str().unwrap_or("/"), fname.to_str().unwrap_or(""))
            }
            _ => (resolved.as_str(), ""),
        }
    };

    let dir = std::path::Path::new(dir_path);
    let canonical_dir = match dir.canonicalize() {
        Ok(path) => path,
        Err(e) => {
            return LauncherToServer::ListDirectoriesResult {
                request_id,
                entries: vec![],
                error: Some(e.to_string()),
                resolved_path: None,
            };
        }
    };
    if let Err(e) = path_policy::ensure_canonical_path_under_home(&canonical_dir) {
        return LauncherToServer::ListDirectoriesResult {
            request_id,
            entries: vec![],
            error: Some(e.to_string()),
            resolved_path: None,
        };
    }

    // Uses synchronous std::fs::read_dir (blocking I/O). This is acceptable because
    // list_directory is only called for small local directories (UI path completion).
    let read_dir = match std::fs::read_dir(&canonical_dir) {
        Ok(rd) => rd,
        Err(e) => {
            return LauncherToServer::ListDirectoriesResult {
                request_id,
                entries: vec![],
                error: Some(e.to_string()),
                resolved_path: Some(resolved),
            };
        }
    };

    let filter_lower = filter.to_lowercase();
    let mut entries: Vec<shared::DirectoryEntry> = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if !filter_lower.is_empty() && !name.to_lowercase().starts_with(&filter_lower) {
            continue;
        }
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        if is_dir {
            let under_home = entry
                .path()
                .canonicalize()
                .ok()
                .and_then(|path| path_policy::ensure_canonical_path_under_home(&path).ok())
                .is_some();
            if !under_home {
                continue;
            }
        }
        entries.push(shared::DirectoryEntry { name, is_dir });
    }

    // Sort: directories first, then alphabetical
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));

    // Return the dir_path as resolved (not including the filter fragment)
    let resolved_dir = if dir_path.ends_with('/') || dir_path == "/" {
        dir_path.to_string()
    } else {
        format!("{}/", dir_path)
    };

    LauncherToServer::ListDirectoriesResult {
        request_id,
        entries,
        error: None,
        resolved_path: Some(resolved_dir),
    }
}

async fn handle_message(
    msg: ServerToLauncher,
    ws_sender: &mut ws_bridge::WsSender<LauncherToServer>,
    process_manager: &mut ProcessManager,
    scheduler: &mut Scheduler,
) {
    match msg {
        ServerToLauncher::LaunchSession {
            request_id,
            auth_token,
            working_directory,
            session_name,
            claude_args,
            agent_type,
            resume_session_id,
            resume,
            create_worktree,
            worktree_branch,
            ..
        } => {
            // Check if this is a scheduler-owned launch.
            let pending_launch = scheduler.get_pending_launch_info(&request_id);
            let (resume_session_id, scheduled_task_id, scheduler_owned) =
                if let Some(info) = pending_launch.as_ref() {
                    let scheduled_task_id = match info.kind {
                        PendingLaunchKind::ScheduledTask { task_id } => Some(task_id),
                        PendingLaunchKind::Continuation { .. } => None,
                    };
                    (info.last_session_id, scheduled_task_id, true)
                } else {
                    (resume_session_id, None, false)
                };

            info!(
                "Launch request: dir={}, name={:?}, agent={}, scheduler_owned={}",
                working_directory, session_name, agent_type, scheduler_owned
            );

            let result = process_manager
                .spawn(SpawnParams {
                    auth_token,
                    working_directory: working_directory.clone(),
                    session_name: session_name.clone(),
                    claude_args: claude_args.clone(),
                    agent_type,
                    scheduled_task_id,
                    resume_session_id,
                    resume,
                    // Scheduler-owned relaunches never create a worktree; the
                    // backend leaves these fields at their defaults for them.
                    create_worktree,
                    worktree_branch,
                })
                .await;

            let response = match result {
                Ok(session_id) => {
                    let launch_kind = if scheduler_owned {
                        scheduler.on_session_spawned(request_id, session_id)
                    } else {
                        None
                    };
                    if let Some(PendingLaunchKind::Continuation {
                        continuation_id,
                        session_id,
                        prompt,
                    }) = launch_kind
                    {
                        let inject = LauncherToServer::InjectInput {
                            session_id,
                            content: prompt,
                        };
                        if ws_sender.send(inject).await.is_err() {
                            warn!("Failed to send relaunched continuation InjectInput");
                        } else {
                            let fired = LauncherToServer::ContinuationFired {
                                continuation_id,
                                session_id,
                            };
                            if ws_sender.send(fired).await.is_err() {
                                warn!("Failed to send relaunched ContinuationFired");
                            }
                        }
                    }
                    LauncherToServer::LaunchSessionResult {
                        request_id,
                        success: true,
                        session_id: Some(session_id),
                        pid: None,
                        error: None,
                    }
                }
                Err(e) => {
                    error!("Failed to spawn: {}", e);
                    if scheduler_owned {
                        scheduler.clear_pending_launch(&request_id);
                    }
                    LauncherToServer::LaunchSessionResult {
                        request_id,
                        success: false,
                        session_id: None,
                        pid: None,
                        error: Some(e.to_string()),
                    }
                }
            };

            if ws_sender.send(response).await.is_err() {
                warn!("Failed to send launch session result");
            }
        }
        ServerToLauncher::StopSession { session_id, .. } => {
            info!("Stop request for session {}", session_id);
            process_manager.stop(&session_id).await;
        }
        ServerToLauncher::PauseSession { session_id } => {
            info!("Pause request for session {}", session_id);
            process_manager.stop(&session_id).await;
        }
        ServerToLauncher::ContinuationSync { continuations } => {
            scheduler.update_continuations(continuations);
        }
        ServerToLauncher::ListDirectories { request_id, path } => {
            let response = list_directory(&path, request_id);
            if ws_sender.send(response).await.is_err() {
                warn!("Failed to send list directories result");
            }
        }
        ServerToLauncher::ProbeAgents { request_id } => {
            // Synchronous probe in a blocking task so two `--version` spawns
            // don't hold up the message loop.
            let agents = tokio::task::spawn_blocking(probe_agents_for_response)
                .await
                .unwrap_or_default();
            let response = shared::LauncherToServer::ProbeAgentsResult { request_id, agents };
            if ws_sender.send(response).await.is_err() {
                warn!("Failed to send probe agents result");
            }
        }
        ServerToLauncher::ScheduleSync { tasks } => {
            info!("Received ScheduleSync with {} task(s)", tasks.len());
            scheduler.update_tasks(tasks);
        }
        ServerToLauncher::ServerShutdown { reason, .. } => {
            info!("Server shutting down: {}", reason);
        }
        ServerToLauncher::UpdateAndRestart => {
            info!("Received UpdateAndRestart request from dashboard");
            tokio::spawn(async move {
                match portal_update::check_for_update(crate::BINARY_PREFIX, false).await {
                    Ok(portal_update::UpdateResult::UpToDate) => {
                        info!("Already up to date; restarting service anyway");
                    }
                    Ok(portal_update::UpdateResult::Updated) => {
                        info!("Update applied; restarting service");
                    }
                    Ok(portal_update::UpdateResult::UpdateAvailable { version, .. }) => {
                        info!(
                            "Update available ({}) but not applied; restarting anyway",
                            version
                        );
                    }
                    Err(e) => {
                        error!("Update check failed: {}; restarting anyway", e);
                    }
                }
                if crate::service::is_installed() {
                    if let Err(e) = crate::service::sync() {
                        error!("Service unit sync failed: {}", e);
                    }
                    if let Err(e) = crate::service::restart() {
                        error!("Service restart failed: {}", e);
                    }
                } else {
                    info!("Service not installed; exiting so a supervisor can respawn");
                    std::process::exit(0);
                }
            });
        }
        ServerToLauncher::TokenRefresh { auth_token } => {
            match crate::config::save_auth_token(&auth_token) {
                Ok(()) => {
                    info!("Persisted refreshed launcher auth token");
                    if ws_sender
                        .send(LauncherToServer::TokenRefreshAck)
                        .await
                        .is_err()
                    {
                        warn!("Failed to acknowledge refreshed launcher auth token");
                    }
                }
                Err(e) => {
                    warn!("Failed to persist refreshed launcher auth token: {}", e);
                }
            }
        }
        other => {
            debug!("Unhandled message from server: {:?}", other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_fatal_prefers_machine_readable_reason() {
        // A backend-supplied reason wins even over contradictory text.
        assert_eq!(
            classify_fatal(
                Some(LauncherRejectReason::DuplicateLauncher),
                "Authentication failed"
            ),
            LauncherRejectReason::DuplicateLauncher
        );
    }

    #[test]
    fn classify_fatal_string_fallback_covers_old_backends() {
        // Pre-reject_reason backends: sniff the message text.
        assert_eq!(
            classify_fatal(None, "Authentication failed"),
            LauncherRejectReason::AuthFailed
        );
        assert_eq!(
            classify_fatal(None, "No auth token provided"),
            LauncherRejectReason::AuthFailed
        );
        assert_eq!(
            classify_fatal(None, "You already have 10 launchers connected (max 10)."),
            LauncherRejectReason::TooManyLaunchers
        );
        assert_eq!(
            classify_fatal(
                None,
                "A launcher named 'x' is already connected from this host."
            ),
            LauncherRejectReason::DuplicateLauncher
        );
    }

    #[test]
    fn every_park_reason_has_an_instruction() {
        for reason in [
            LauncherRejectReason::AuthFailed,
            LauncherRejectReason::TooManyLaunchers,
            LauncherRejectReason::DuplicateLauncher,
        ] {
            assert!(!parked_instruction(reason).is_empty());
        }
        assert!(parked_instruction(LauncherRejectReason::AuthFailed).contains("agent-portal login"));
    }

    fn home_test_dir(name: &str) -> std::path::PathBuf {
        path_policy::home_dir()
            .unwrap()
            .join(format!(".agent-portal-{}", name))
    }

    fn extract_result(
        msg: LauncherToServer,
    ) -> (Vec<shared::DirectoryEntry>, Option<String>, Option<String>) {
        match msg {
            LauncherToServer::ListDirectoriesResult {
                entries,
                error,
                resolved_path,
                ..
            } => (entries, error, resolved_path),
            other => panic!("Expected ListDirectoriesResult, got {:?}", other),
        }
    }

    #[test]
    fn list_directory_returns_sorted_entries() {
        let tmp = home_test_dir("launcher-test-sorted");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::create_dir(tmp.join("beta_dir")).unwrap();
        std::fs::write(tmp.join("alpha.txt"), "").unwrap();
        std::fs::create_dir(tmp.join("alpha_dir")).unwrap();
        std::fs::write(tmp.join("beta.txt"), "").unwrap();

        let path = format!("{}/", tmp.display());
        let result = list_directory(&path, Uuid::nil());
        let (entries, error, _) = extract_result(result);

        assert!(error.is_none());
        // Directories come first, then files, each group sorted alphabetically
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["alpha_dir", "beta_dir", "alpha.txt", "beta.txt"]
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn list_directory_filters_hidden_files() {
        let tmp = home_test_dir("launcher-test-hidden");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(".hidden"), "").unwrap();
        std::fs::write(tmp.join("visible"), "").unwrap();

        let path = format!("{}/", tmp.display());
        let result = list_directory(&path, Uuid::nil());
        let (entries, _, _) = extract_result(result);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "visible");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn list_directory_prefix_filter() {
        let tmp = home_test_dir("launcher-test-prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("foo.txt"), "").unwrap();
        std::fs::write(tmp.join("bar.txt"), "").unwrap();
        std::fs::write(tmp.join("foobar.txt"), "").unwrap();

        // No trailing slash — last component "fo" becomes the prefix filter
        let path = format!("{}/fo", tmp.display());
        let result = list_directory(&path, Uuid::nil());
        let (entries, error, _) = extract_result(result);

        assert!(error.is_none());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["foo.txt", "foobar.txt"]);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn list_directory_nonexistent_returns_error() {
        let result = list_directory("/nonexistent_launcher_test_path_12345/subdir/", Uuid::nil());
        let (entries, error, _) = extract_result(result);

        assert!(entries.is_empty());
        assert!(error.is_some());
    }

    #[test]
    fn list_directory_rejects_existing_paths_outside_home() {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        let home = path_policy::home_dir().unwrap().canonicalize().unwrap();

        if temp.starts_with(home) {
            return;
        }

        let result = list_directory(&format!("{}/", temp.display()), Uuid::nil());
        let (entries, error, resolved) = extract_result(result);

        assert!(entries.is_empty());
        assert!(error.unwrap().contains("home directory"));
        assert!(resolved.is_none());
    }

    #[test]
    fn list_directory_resolved_path_has_trailing_slash() {
        let tmp = path_policy::home_dir().unwrap();
        let path = tmp.to_string_lossy().to_string();
        // Even without trailing slash, if it's a valid dir the resolved_path should end with /
        let result = list_directory(&format!("{}/", path), Uuid::nil());
        let (_, _, resolved) = extract_result(result);

        assert!(resolved.unwrap().ends_with('/'));
    }
}
