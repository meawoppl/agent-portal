use crate::config::{self, ExpectedSession};
use crate::process_manager::{ProcessManager, SessionExited, SpawnParams};
use crate::scheduler::Scheduler;
use shared::{LauncherEndpoint, LauncherToServer, ServerToLauncher};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(shared::protocol::MAX_RECONNECT_BACKOFF_SECS);
const RESTART_DELAY: Duration = Duration::from_secs(5);
const MAX_RESTART_ATTEMPTS: u32 = 3;

pub async fn run_launcher_loop(
    backend_url: &str,
    launcher_id: Uuid,
    launcher_name: &str,
    auth_token: Option<&str>,
    mut process_manager: ProcessManager,
    mut exit_rx: mpsc::UnboundedReceiver<SessionExited>,
    mut expected_sessions: Vec<ExpectedSession>,
) -> anyhow::Result<()> {
    process_manager.set_launcher_id(launcher_id);
    let mut backoff = Duration::from_secs(1);
    let mut scheduler = Scheduler::new();

    loop {
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
                    auth_token: auth_token.map(|s| s.to_string()),
                    hostname: hostname::get()
                        .map(|h| h.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    version: Some(env!("CARGO_PKG_VERSION").to_string()),
                    working_directory: std::env::current_dir()
                        .ok()
                        .map(|p| p.to_string_lossy().to_string()),
                };
                if ws_sender.send(register).await.is_err() {
                    warn!("Failed to send registration");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    continue;
                }

                // Wait for RegisterAck
                let ack_ok = loop {
                    match ws_receiver.recv().await {
                        Some(Ok(ServerToLauncher::LauncherRegisterAck {
                            success,
                            error,
                            fatal,
                            ..
                        })) => {
                            if success {
                                info!("Registration successful");
                                break Some(true);
                            } else {
                                let msg = error.unwrap_or_default();
                                if fatal {
                                    error!("Registration rejected (fatal): {}", msg);
                                    break None; // signal: exit, do not retry
                                } else {
                                    error!("Registration failed: {}", msg);
                                    break Some(false);
                                }
                            }
                        }
                        Some(Ok(_)) => continue,
                        Some(Err(e)) => {
                            warn!("Decode error during registration: {}", e);
                            continue;
                        }
                        None => break Some(false),
                    }
                };

                match ack_ok {
                    None => {
                        // Fatal rejection — exit immediately, do not retry
                        return Err(anyhow::anyhow!("Fatal registration error, exiting"));
                    }
                    Some(false) => {
                        warn!("Registration failed, will retry");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(MAX_BACKOFF);
                        continue;
                    }
                    Some(true) => {} // fall through to main loop
                }

                // Reconcile expected sessions: launch any that aren't running
                let mut restart_counts: HashMap<String, u32> = HashMap::new();
                if !expected_sessions.is_empty() {
                    let running_dirs = process_manager.running_directories();
                    for expected in &expected_sessions {
                        if running_dirs.contains(&expected.working_directory) {
                            info!(
                                "Expected session already running: {}",
                                expected.working_directory
                            );
                            continue;
                        }
                        info!("Launching expected session: {}", expected.working_directory);
                        let request = LauncherToServer::RequestLaunch {
                            request_id: Uuid::new_v4(),
                            working_directory: expected.working_directory.clone(),
                            session_name: expected.session_name.clone(),
                            claude_args: expected.claude_args.clone(),
                            agent_type: expected.agent_type,
                            scheduled_task_id: None,
                        };
                        if ws_sender.send(request).await.is_err() {
                            warn!("Failed to send expected session launch request");
                            break;
                        }
                    }
                }

                // Channel for delayed restart requests
                let (restart_tx, mut restart_rx) = mpsc::unbounded_channel::<ExpectedSession>();

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
                    let sched_sleep = tokio::time::sleep(sched_dur);
                    let prompt_sleep = tokio::time::sleep(prompt_dur);
                    tokio::pin!(sched_sleep);
                    tokio::pin!(prompt_sleep);

                    tokio::select! {
                        result = ws_receiver.recv() => {
                            match result {
                                Some(Ok(msg)) => {
                                    handle_message(
                                        msg,
                                        &mut ws_sender,
                                        &mut process_manager,
                                        &mut expected_sessions,
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
                            let exited_dir = process_manager.session_working_directory(&exited.session_id);
                            info!(
                                "Session {} exited with code {:?}",
                                exited.session_id, exited.exit_code
                            );
                            process_manager.remove_finished(&exited.session_id);
                            let msg = LauncherToServer::SessionExited {
                                session_id: exited.session_id,
                                exit_code: exited.exit_code,
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

                            if let Some(dir) = exited_dir {
                                let is_clean_exit = exited.exit_code == Some(0);
                                if is_clean_exit {
                                    // Clean exit: remove from expected sessions
                                    expected_sessions.retain(|s| s.working_directory != dir);
                                    if let Err(e) = config::remove_session(&dir) {
                                        warn!("Failed to remove session from config: {}", e);
                                    }
                                    info!("Session exited cleanly, removed from expected: {}", dir);
                                } else if let Some(expected) = expected_sessions.iter().find(|s| s.working_directory == dir) {
                                    // Non-clean exit: try to restart
                                    let count = restart_counts.entry(dir.clone()).or_insert(0);
                                    *count += 1;
                                    if *count <= MAX_RESTART_ATTEMPTS {
                                        info!(
                                            "Expected session exited, scheduling restart ({}/{}): {}",
                                            count, MAX_RESTART_ATTEMPTS, dir
                                        );
                                        let tx = restart_tx.clone();
                                        let session = expected.clone();
                                        tokio::spawn(async move {
                                            tokio::time::sleep(RESTART_DELAY).await;
                                            let _ = tx.send(session);
                                        });
                                    } else {
                                        warn!(
                                            "Expected session exceeded max restarts ({}): {}",
                                            MAX_RESTART_ATTEMPTS, dir
                                        );
                                        expected_sessions.retain(|s| s.working_directory != dir);
                                        if let Err(e) = config::remove_session(&dir) {
                                            warn!("Failed to remove session from config: {}", e);
                                        }
                                    }
                                }
                            }
                        }

                        Some(session) = restart_rx.recv() => {
                            info!("Restarting expected session: {}", session.working_directory);
                            let request = LauncherToServer::RequestLaunch {
                                request_id: Uuid::new_v4(),
                                working_directory: session.working_directory,
                                session_name: session.session_name,
                                claude_args: session.claude_args,
                                agent_type: session.agent_type,
                                scheduled_task_id: None,
                            };
                            if ws_sender.send(request).await.is_err() {
                                warn!("Failed to send session restart request");
                                break;
                            }
                        }

                        // Scheduler: fire due tasks
                        _ = &mut sched_sleep => {
                            for task_to_fire in scheduler.fire_due_tasks() {
                                info!(
                                    "Firing scheduled task '{}' ({})",
                                    task_to_fire.config.name, task_to_fire.config.id
                                );
                                let msg = LauncherToServer::RequestLaunch {
                                    request_id: task_to_fire.request_id,
                                    working_directory: task_to_fire.config.working_directory.clone(),
                                    session_name: Some(task_to_fire.config.name.clone()),
                                    claude_args: task_to_fire.config.claude_args.clone(),
                                    agent_type: task_to_fire.config.agent_type,
                                    scheduled_task_id: Some(task_to_fire.config.id),
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

fn list_directory(path: &str, request_id: Uuid) -> LauncherToServer {
    // Resolve ~ to home directory (trailing slash ensures the dir itself is listed,
    // not treated as a filter prefix against its parent)
    let resolved = if path == "~" || path == "~/" {
        dirs::home_dir()
            .map(|p| format!("{}/", p.to_string_lossy()))
            .unwrap_or_else(|| "/".to_string())
    } else if let Some(rest) = path.strip_prefix("~/") {
        dirs::home_dir()
            .map(|p| format!("{}/{}", p.to_string_lossy(), rest))
            .unwrap_or_else(|| format!("/{}", rest))
    } else {
        path.to_string()
    };

    // Split into (dir_to_list, filter_prefix)
    // If the path ends with '/', list the directory with no filter
    // Otherwise, treat the last component as a prefix filter
    let (dir_path, filter) = if resolved.ends_with('/') || resolved == "/" {
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
    // Uses synchronous std::fs::read_dir (blocking I/O). This is acceptable because
    // list_directory is only called for small local directories (UI path completion).
    let read_dir = match std::fs::read_dir(dir) {
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
    expected_sessions: &mut Vec<ExpectedSession>,
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
            ..
        } => {
            // Check if this is a scheduled launch
            let (resume_session_id, scheduled_task_id, is_scheduled) = if let Some((
                resume_id,
                task_id,
            )) =
                scheduler.get_pending_launch_info(&request_id)
            {
                (resume_id, Some(task_id), true)
            } else {
                (None, None, false)
            };

            info!(
                "Launch request: dir={}, name={:?}, agent={}, scheduled={}",
                working_directory, session_name, agent_type, is_scheduled
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
                })
                .await;

            let response = match result {
                Ok(session_id) => {
                    if is_scheduled {
                        scheduler.on_session_spawned(request_id, session_id);
                    } else {
                        // Persist so this session survives launcher restarts
                        if !expected_sessions
                            .iter()
                            .any(|s| s.working_directory == working_directory)
                        {
                            let expected = ExpectedSession {
                                working_directory: working_directory.clone(),
                                session_name: session_name.clone(),
                                agent_type,
                                claude_args: claude_args.clone(),
                            };
                            if let Err(e) = config::add_session(&expected) {
                                warn!("Failed to persist session to config: {}", e);
                            }
                            expected_sessions.push(expected);
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
                    if is_scheduled {
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
        ServerToLauncher::StopSession { session_id } => {
            info!("Stop request for session {}", session_id);
            let working_dir = process_manager.session_working_directory(&session_id);
            process_manager.stop(&session_id).await;
            if let Some(dir) = working_dir {
                expected_sessions.retain(|s| s.working_directory != dir);
                if let Err(e) = config::remove_session(&dir) {
                    warn!("Failed to remove session from config: {}", e);
                }
            }
        }
        ServerToLauncher::ListDirectories { request_id, path } => {
            let response = list_directory(&path, request_id);
            if ws_sender.send(response).await.is_err() {
                warn!("Failed to send list directories result");
            }
        }
        ServerToLauncher::ScheduleSync { tasks } => {
            info!("Received ScheduleSync with {} task(s)", tasks.len());
            scheduler.update_tasks(tasks);
        }
        ServerToLauncher::TokenRenewed { token } => {
            info!("Received renewed auth token from server");
            if let Err(e) = config::save_auth_token(&token) {
                error!("Failed to save renewed token: {}", e);
            } else {
                info!("Renewed token saved to config");
            }
        }
        ServerToLauncher::ServerShutdown { reason, .. } => {
            info!("Server shutting down: {}", reason);
        }
        other => {
            debug!("Unhandled message from server: {:?}", other);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let tmp = std::env::temp_dir().join("launcher_test_sorted");
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
        let tmp = std::env::temp_dir().join("launcher_test_hidden");
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
        let tmp = std::env::temp_dir().join("launcher_test_prefix");
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
    fn list_directory_resolved_path_has_trailing_slash() {
        let tmp = std::env::temp_dir();
        let path = tmp.to_string_lossy().to_string();
        // Even without trailing slash, if it's a valid dir the resolved_path should end with /
        let result = list_directory(&format!("{}/", path), Uuid::nil());
        let (_, _, resolved) = extract_result(result);

        assert!(resolved.unwrap().ends_with('/'));
    }
}
