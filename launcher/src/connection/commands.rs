use crate::path_policy;
use crate::process_manager::{ProcessManager, SpawnParams};
use crate::scheduler::{PendingLaunchKind, Scheduler};
use shared::{LauncherToServer, ServerToLauncher};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Probe both supported agent CLIs and convert into the wire shape used in
/// `LauncherToServer::ProbeAgentsResult`. Runs `which::which` + `--version`
/// synchronously, so callers must run this from a `spawn_blocking` task.
fn probe_agents_for_response() -> Vec<shared::AgentInstall> {
    session_lib::probe::probe_all_agents()
        .into_iter()
        .map(|(agent_type, result)| {
            // Only read sign-in state for agents that are actually installed —
            // spawning `claude auth status` / `codex login status` for a binary
            // that isn't there just burns a failed exec to reach the same
            // `Unknown` the field defaults to.
            let login = if result.installed {
                match agent_type {
                    shared::AgentType::Claude => claude_session_lib::auth::probe_login(),
                    shared::AgentType::Codex => codex_session_lib::auth::probe_login(),
                    // Muse persists no account identity (no whoami at 0.1.0),
                    // so the cell is presence-only.
                    shared::AgentType::Muse => session_lib::probe::probe_muse_login(),
                    shared::AgentType::Antigravity => session_lib::probe::probe_antigravity_login(),
                }
            } else {
                shared::AgentLoginStatus::Unknown
            };
            shared::AgentInstall {
                agent_type,
                installed: result.installed,
                resolved_path: result
                    .resolved_path
                    .map(|p| p.to_string_lossy().to_string()),
                version: result.version,
                sandbox_ok: result.sandbox_ok,
                login,
            }
        })
        .collect()
}

/// Gracefully restart the launcher process. When run under a service manager
/// (systemd/launchd), sync the unit and ask the manager to restart us (the
/// manager sends SIGTERM and respawns per the unit's `Restart=`/`KeepAlive`
/// settings); when unmanaged, exit(0) so an external supervisor respawns.
/// Shared by the `UpdateAndRestart` (post-download) and `Restart` (no-download)
/// paths so the restart mechanism lives in exactly one place.
fn restart_service() {
    if crate::service::is_installed() {
        if let Err(e) = crate::service::sync() {
            error!("Service unit sync failed; leaving launcher running: {}", e);
            return;
        }
        if let Err(e) = crate::service::restart() {
            error!("Service restart failed: {}", e);
        }
    } else {
        info!("Service not installed; exiting so a supervisor can respawn");
        std::process::exit(0);
    }
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
        let is_dir = entry.file_type().is_ok_and(|ft| ft.is_dir());
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

pub(super) async fn handle_message(
    msg: ServerToLauncher,
    ws_sender: &mut ws_bridge::WsSender<LauncherToServer>,
    process_manager: &mut ProcessManager,
    scheduler: &mut Scheduler,
    login_registry: &mut crate::login_registry::LoginRegistry,
    import_reply_tx: &mpsc::Sender<LauncherToServer>,
) {
    match msg {
        ServerToLauncher::LaunchSession {
            plugin_selection,
            history_restore,
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
            scratch_worktree,
            fork_from_session_id,
            fork_point_turn_id,
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
                    plugin_selection,
                    history_restore: history_restore.map(|r| *r),
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
                    scratch_worktree,
                    fork_from_session_id,
                    fork_point_turn_id,
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
        ServerToLauncher::ListExternalSessions {
            request_id,
            agent_type,
            query,
        } => {
            let sender = import_reply_tx.clone();
            tokio::spawn(async move {
                let result = tokio::select! {
                    _ = sender.closed() => return,
                    result = tokio::time::timeout(std::time::Duration::from_secs(45), crate::imports::list(agent_type, query)) => result,
                }
                .map_err(|_| "Session discovery timed out".to_string())
                .and_then(|r| r.map_err(|e| format!("{e:#}")));
                if sender
                    .send(LauncherToServer::ExternalSessionsResult { request_id, result })
                    .await
                    .is_err()
                {
                    warn!("Failed to send external sessions result");
                }
            });
        }
        ServerToLauncher::CheckHistoryRestore {
            request_id,
            agent_type,
            working_directory,
            restore,
        } => {
            let sender = import_reply_tx.clone();
            let running = process_manager
                .running_session_ids()
                .contains(&restore.source_session_id);
            tokio::spawn(async move {
                let result = if running && restore.mode == shared::api::HistoryRestoreMode::Native {
                    Err("Original conversation is still running on this host. Stop it before restoring native state.".to_string())
                } else {
                    tokio::select! {
                        _ = sender.closed() => return,
                        result = tokio::time::timeout(
                            std::time::Duration::from_secs(90),
                            crate::history_restore::check(agent_type, &working_directory, &restore),
                        ) => result,
                    }
                    .map_err(|_| {
                        "Restore check timed out; reconnect the host and retry".to_string()
                    })
                    .and_then(|r| r.map_err(|e| format!("{e:#}")))
                };
                let _ = sender
                    .send(LauncherToServer::CheckHistoryRestoreResult { request_id, result })
                    .await;
            });
        }
        ServerToLauncher::PrepareSessionImport {
            request_id,
            agent_type,
            session_id,
            request,
        } => {
            let sender = import_reply_tx.clone();
            tokio::spawn(async move {
                let result = tokio::select! {
                    _ = sender.closed() => return,
                    result = tokio::time::timeout(std::time::Duration::from_secs(90), crate::imports::prepare(agent_type, session_id, request)) => result,
                }
                    .map_err(|_| "Session import timed out. Close the original client and retry, or import without copying history.".to_string())
                    .and_then(|r| r.map_err(|e| format!("{e:#}")));
                if sender
                    .send(LauncherToServer::PrepareSessionImportResult { request_id, result })
                    .await
                    .is_err()
                {
                    warn!("Failed to send session import result");
                }
            });
        }
        ServerToLauncher::PrepareSessionRestore {
            request_id,
            agent_type,
            source_session_id,
            session_id,
            working_directory,
        } => {
            let result = crate::path_policy::ensure_existing_dir_under_home(&working_directory)
                .and_then(|dir| {
                    crate::process_manager::bind_restored_session(
                        agent_type,
                        source_session_id,
                        session_id,
                        &dir,
                    )
                })
                .map_err(|e| format!("{e:#}"));
            match &result {
                Ok(()) => info!(
                    "Restored session {} from archived {}",
                    session_id, source_session_id
                ),
                Err(e) => warn!("Restore of {} failed: {}", source_session_id, e),
            }
            if ws_sender
                .send(LauncherToServer::PrepareSessionRestoreResult { request_id, result })
                .await
                .is_err()
            {
                warn!("Failed to send session restore result");
            }
        }
        ServerToLauncher::SessionPlugins {
            request_id,
            session_id,
            working_directory,
            selection,
        } => {
            let sender = import_reply_tx.clone();
            // A session running here picks up a selection change on its next
            // turn, instead of waiting for a restart.
            let live = process_manager.context_inbox(&session_id);
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    crate::session_plugins::request(session_id, working_directory, selection, live)
                })
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| format!("{error:#}")));
                let _ = sender
                    .send(LauncherToServer::SessionPluginsResult { request_id, result })
                    .await;
            });
        }
        ServerToLauncher::Plugin {
            request_id,
            request,
        } => {
            let sender = import_reply_tx.clone();
            tokio::spawn(async move {
                let result = crate::plugin_rpc::handle(request)
                    .await
                    .map_err(|error| format!("{error:#}"));
                let _ = sender
                    .send(LauncherToServer::PluginResult { request_id, result })
                    .await;
            });
        }
        ServerToLauncher::ListDirectories { request_id, path } => {
            let response = list_directory(&path, request_id);
            if ws_sender.send(response).await.is_err() {
                warn!("Failed to send list directories result");
            }
        }
        ServerToLauncher::StartAgentLogin {
            request_id,
            flow_id,
            agent_type,
        } => {
            let response = match login_registry.start(flow_id, agent_type).await {
                Ok((presentable, interaction)) => LauncherToServer::AgentLoginStartResult {
                    request_id,
                    flow_id,
                    presentable: Some(presentable),
                    interaction: Some(interaction),
                    error: None,
                },
                Err(error) => LauncherToServer::AgentLoginStartResult {
                    request_id,
                    flow_id,
                    presentable: None,
                    interaction: None,
                    error: Some(error),
                },
            };
            if ws_sender.send(response).await.is_err() {
                warn!("Failed to send agent login start result");
            }
        }
        ServerToLauncher::SubmitAgentLoginCode {
            request_id,
            flow_id,
            code,
        } => {
            let outcome = login_registry.submit_code(flow_id, code).await;
            if outcome.done {
                login_registry.remove(flow_id);
            }
            let response = LauncherToServer::AgentLoginOutcomeResult {
                request_id,
                outcome,
            };
            if ws_sender.send(response).await.is_err() {
                warn!("Failed to send agent login outcome");
            }
        }
        ServerToLauncher::PollAgentLogin {
            request_id,
            flow_id,
        } => {
            let outcome = login_registry.poll(flow_id);
            if outcome.done {
                login_registry.remove(flow_id);
            }
            let response = LauncherToServer::AgentLoginOutcomeResult {
                request_id,
                outcome,
            };
            if ws_sender.send(response).await.is_err() {
                warn!("Failed to send agent login poll result");
            }
        }
        ServerToLauncher::CancelAgentLogin { flow_id } => {
            login_registry.remove(flow_id);
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
        ServerToLauncher::InstallAgent {
            request_id,
            agent_type,
        } => {
            info!("Installing agent {agent_type} on request");
            // The install command (npm) can run for tens of seconds — off the
            // message loop, like the probe.
            let result = tokio::task::spawn_blocking(move || {
                session_lib::install::install_agent(agent_type)
            })
            .await;
            let (success, message) = match result {
                Ok(r) => (r.success, r.message),
                Err(e) => (false, Some(format!("install task failed to run: {e}"))),
            };
            let response = shared::LauncherToServer::InstallAgentResult {
                request_id,
                success,
                message,
            };
            if ws_sender.send(response).await.is_err() {
                warn!("Failed to send install agent result");
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
                restart_service();
            });
        }
        ServerToLauncher::Restart => {
            info!("Received Restart request from dashboard (restart requested via portal)");
            // Same graceful restart as the post-update path, minus the
            // download/replace: the service manager (systemd/launchd) respawns
            // the process, or — when unmanaged — we exit for a supervisor to.
            tokio::spawn(async move {
                restart_service();
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
