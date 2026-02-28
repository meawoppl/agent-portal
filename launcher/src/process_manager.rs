use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use claude_session_lib::{
    run_connection_loop, LoopResult, ProxySessionConfig, Session as ClaudeSession, SessionConfig,
};

/// Notification that a session task has finished.
pub struct SessionExited {
    pub session_id: Uuid,
    pub exit_code: Option<i32>,
}

struct ManagedTask {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
    working_directory: String,
}

pub struct ProcessManager {
    tasks: HashMap<Uuid, ManagedTask>,
    backend_url: String,
    max_sessions: usize,
    exit_tx: mpsc::UnboundedSender<SessionExited>,
    launcher_id: Option<Uuid>,
}

impl ProcessManager {
    pub fn new(
        backend_url: String,
        max_sessions: usize,
    ) -> (Self, mpsc::UnboundedReceiver<SessionExited>) {
        let (exit_tx, exit_rx) = mpsc::unbounded_channel();
        (
            Self {
                tasks: HashMap::new(),
                backend_url,
                max_sessions,
                exit_tx,
                launcher_id: None,
            },
            exit_rx,
        )
    }

    pub fn set_launcher_id(&mut self, id: Uuid) {
        self.launcher_id = Some(id);
    }

    pub fn running_session_ids(&self) -> Vec<Uuid> {
        self.tasks.keys().copied().collect()
    }

    /// Returns the set of working directories that currently have running sessions.
    pub fn running_directories(&self) -> Vec<String> {
        self.tasks
            .values()
            .map(|t| t.working_directory.clone())
            .collect()
    }

    /// Returns the working directory for a given session, if it exists.
    pub fn session_working_directory(&self, session_id: &Uuid) -> Option<String> {
        self.tasks
            .get(session_id)
            .map(|t| t.working_directory.clone())
    }

    pub async fn spawn(
        &mut self,
        auth_token: &str,
        working_directory: &str,
        session_name: Option<&str>,
        claude_args: &[String],
        agent_type: shared::AgentType,
    ) -> anyhow::Result<Uuid> {
        // Enforce the concurrency cap. Each session is a long-lived Claude CLI
        // process consuming memory, CPU, and a WebSocket connection. Without a
        // limit, a burst of launch requests could starve the host of resources
        // and degrade every running session.
        if self.tasks.len() >= self.max_sessions {
            anyhow::bail!(
                "At session limit ({}/{})",
                self.tasks.len(),
                self.max_sessions
            );
        }

        let wd = std::path::Path::new(working_directory);
        if !wd.is_dir() {
            anyhow::bail!("Working directory does not exist: {}", working_directory);
        }

        let session_id = Uuid::new_v4();
        let default_name = {
            let hostname = hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "unknown".to_string());
            let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            format!("{}-{}", hostname, timestamp)
        };
        let name = session_name.unwrap_or(&default_name).to_string();

        let git_branch = get_git_branch(working_directory);

        let proxy_config = ProxySessionConfig {
            backend_url: self.backend_url.clone(),
            session_id,
            session_name: name.clone(),
            auth_token: Some(auth_token.to_string()),
            working_directory: working_directory.to_string(),
            resume: false,
            git_branch,
            claude_args: claude_args.to_vec(),
            replaces_session_id: None,
            launcher_id: self.launcher_id,
            agent_type,
        };

        let exit_tx = self.exit_tx.clone();
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();

        let handle = tokio::spawn(async move {
            let exit_code = run_session_task(proxy_config, cancel_clone).await;
            let _ = exit_tx.send(SessionExited {
                session_id,
                exit_code,
            });
        });

        info!(
            "Spawned session task: session_id={}, session_name={}, dir={}",
            session_id, name, working_directory
        );

        self.tasks.insert(
            session_id,
            ManagedTask {
                handle,
                cancel,
                working_directory: working_directory.to_string(),
            },
        );

        Ok(session_id)
    }

    pub async fn stop(&mut self, session_id: &Uuid) -> bool {
        if let Some(mut task) = self.tasks.remove(session_id) {
            info!("Stopping session task {}", session_id);
            task.cancel.cancel();
            // Give the task a moment to shut down gracefully before aborting
            tokio::select! {
                _ = &mut task.handle => {}
                _ = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                    warn!("Session {} did not stop within 5s, force aborting", session_id);
                    task.handle.abort();
                }
            }
            true
        } else {
            warn!("No task found for session {}", session_id);
            false
        }
    }

    /// Remove a finished task from tracking. Called when we receive a SessionExited notification.
    pub fn remove_finished(&mut self, session_id: &Uuid) {
        self.tasks.remove(session_id);
    }
}

/// Run a single proxy session as an in-process task.
/// Returns an exit code: Some(0) for normal exit, Some(1) for error, None for abort.
async fn run_session_task(
    mut config: ProxySessionConfig,
    cancel: CancellationToken,
) -> Option<i32> {
    loop {
        let claude_config = SessionConfig {
            session_id: config.session_id,
            working_directory: PathBuf::from(&config.working_directory),
            session_name: config.session_name.clone(),
            resume: config.resume,
            claude_path: None,
            extra_args: config.claude_args.clone(),
            agent_type: config.agent_type,
        };

        let mut claude_session = match ClaudeSession::new(claude_config).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to create Claude session: {}", e);
                return Some(1);
            }
        };

        let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let result = tokio::select! {
            r = run_connection_loop(&config, &mut claude_session, input_tx, &mut input_rx) => r,
            _ = cancel.cancelled() => {
                info!("Session {} cancelled by stop request", config.session_id);
                let _ = claude_session.stop().await;
                return Some(0);
            }
        };

        let _ = claude_session.stop().await;

        match result {
            Ok(LoopResult::NormalExit) => {
                info!("Session {} exited normally", config.session_id);
                return Some(0);
            }
            Ok(LoopResult::SessionNotFound) => {
                if !config.resume {
                    info!("Session {} not found, not resuming", config.session_id);
                    return Some(0);
                }
                // Retry with a fresh session
                let old_id = config.session_id;
                let new_id = Uuid::new_v4();
                warn!(
                    "Session {} not found, retrying as fresh session {}",
                    old_id, new_id
                );
                config.session_id = new_id;
                config.resume = false;
                config.replaces_session_id = Some(old_id);
            }
            Err(e) => {
                error!("Session {} failed: {}", config.session_id, e);
                return Some(1);
            }
        }
    }
}

fn get_git_branch(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    if branch == "HEAD" {
        // Detached HEAD — return short commit hash
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(cwd)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| format!("detached:{}", s.trim()))
    } else {
        Some(branch)
    }
}
