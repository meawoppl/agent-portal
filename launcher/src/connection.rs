use crate::config::ExpectedSession;
use crate::process_manager::{ProcessManager, SessionExited};
use shared::{LauncherEndpoint, LauncherToServer, ServerToLauncher};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const RESTART_DELAY: Duration = Duration::from_secs(5);
const MAX_RESTART_ATTEMPTS: u32 = 3;

pub async fn run_launcher_loop(
    backend_url: &str,
    launcher_id: Uuid,
    launcher_name: &str,
    auth_token: Option<&str>,
    mut process_manager: ProcessManager,
    mut exit_rx: mpsc::UnboundedReceiver<SessionExited>,
    expected_sessions: Vec<ExpectedSession>,
) -> anyhow::Result<()> {
    process_manager.set_launcher_id(launcher_id);
    let mut backoff = Duration::from_secs(1);

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
                    tokio::select! {
                        result = ws_receiver.recv() => {
                            match result {
                                Some(Ok(msg)) => {
                                    handle_message(
                                        msg,
                                        &mut ws_sender,
                                        &mut process_manager,
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

                            // Schedule restart if this was an expected session
                            if let Some(dir) = exited_dir {
                                if let Some(expected) = expected_sessions.iter().find(|s| s.working_directory == dir) {
                                    // restart_counts is never reset after a successful run.
                                    // This is intentional: MAX_RESTART_ATTEMPTS is a
                                    // total lifetime cap per directory, not a per-window
                                    // counter. Fail-fast rather than retry indefinitely.
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
                            };
                            if ws_sender.send(request).await.is_err() {
                                warn!("Failed to send session restart request");
                                break;
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
            info!(
                "Launch request: dir={}, name={:?}, agent={}",
                working_directory, session_name, agent_type
            );

            let result = process_manager
                .spawn(
                    &auth_token,
                    &working_directory,
                    session_name.as_deref(),
                    &claude_args,
                    agent_type,
                )
                .await;

            let response = match result {
                Ok(session_id) => LauncherToServer::LaunchSessionResult {
                    request_id,
                    success: true,
                    session_id: Some(session_id),
                    pid: None,
                    error: None,
                },
                Err(e) => {
                    error!("Failed to spawn: {}", e);
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
            process_manager.stop(&session_id).await;
        }
        ServerToLauncher::ListDirectories { request_id, path } => {
            let response = list_directory(&path, request_id);
            if ws_sender.send(response).await.is_err() {
                warn!("Failed to send list directories result");
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
