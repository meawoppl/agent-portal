use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use claude_session_lib::proxy_session::{get_git_branch, CodexThreadIdSink};
use claude_session_lib::{
    run_connection_loop, ClaudeAgent, LoopResult, PortalInput, ProxySessionConfig,
};
use codex_session_lib::CodexAgent;
use session_lib::{Session, SessionConfig};

use crate::path_policy;

/// Path to the launcher's sidecar codex-thread map: `session_id -> thread_id`.
/// Lives next to `launcher.json` in `ProjectDirs` so it ships with the same
/// install/uninstall surface and survives across restarts.
fn codex_threads_path() -> PathBuf {
    directories::ProjectDirs::from("com", "anthropic", "agent-portal")
        .map(|p| p.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/tmp/agent-portal"))
        .join("codex_threads.json")
}

fn load_codex_threads() -> HashMap<Uuid, String> {
    std::fs::read_to_string(codex_threads_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_codex_threads(map: &HashMap<Uuid, String>) -> std::io::Result<()> {
    let path = codex_threads_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(map)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, body)
}

fn load_codex_thread_id(session_id: Uuid) -> Option<String> {
    load_codex_threads().get(&session_id).cloned()
}

fn make_codex_thread_id_sink(session_id: Uuid) -> CodexThreadIdSink {
    std::sync::Arc::new(move |thread_id: String| {
        let mut map = load_codex_threads();
        map.insert(session_id, thread_id);
        if let Err(e) = save_codex_threads(&map) {
            warn!(
                "Failed to persist codex thread id for session {}: {}",
                session_id, e
            );
        }
    })
}

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

pub struct SpawnParams {
    pub auth_token: String,
    pub working_directory: String,
    pub session_name: Option<String>,
    pub claude_args: Vec<String>,
    pub agent_type: shared::AgentType,
    pub scheduled_task_id: Option<Uuid>,
    pub resume_session_id: Option<Uuid>,
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

    /// Returns the working directory for a given session, if it exists.
    pub fn session_working_directory(&self, session_id: &Uuid) -> Option<String> {
        self.tasks
            .get(session_id)
            .map(|t| t.working_directory.clone())
    }

    pub async fn spawn(&mut self, params: SpawnParams) -> anyhow::Result<Uuid> {
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

        let working_directory =
            path_policy::ensure_existing_dir_under_home(&params.working_directory)?
                .to_string_lossy()
                .to_string();

        let (session_id, resume) = match params.resume_session_id {
            Some(id) => (id, true),
            None => (Uuid::new_v4(), false),
        };
        let default_name = {
            let hostname = hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "unknown".to_string());
            let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            format!("{}-{}", hostname, timestamp)
        };
        let name = params
            .session_name
            .as_deref()
            .unwrap_or(&default_name)
            .to_string();

        let git_branch = get_git_branch(&working_directory);

        let proxy_config = ProxySessionConfig {
            backend_url: self.backend_url.clone(),
            session_id,
            session_name: name.clone(),
            auth_token: Some(params.auth_token),
            working_directory: working_directory.clone(),
            resume,
            git_branch,
            claude_args: params.claude_args,
            replaces_session_id: None,
            launcher_id: self.launcher_id,
            agent_type: params.agent_type,
            scheduled_task_id: params.scheduled_task_id,
            // Persist the codex app-server thread id under the launcher's
            // session_id key so the next resume of this session can call
            // thread/resume. No-op for claude sessions (the codex io-task
            // is the only emitter).
            codex_thread_id_sink: Some(make_codex_thread_id_sink(session_id)),
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
                working_directory,
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

/// Heterogeneous wrapper around `Session<A>` so we can store Claude- and
/// Codex-backed sessions side-by-side. The launcher is the one place
/// agent-portal needs to dispatch across agent types in the same process.
enum AnySession {
    Claude(Session<ClaudeAgent>),
    Codex(Session<CodexAgent>),
}

impl AnySession {
    async fn new(config: SessionConfig) -> Result<Self, session_lib::SessionError> {
        match config.agent_type {
            shared::AgentType::Claude => {
                Ok(Self::Claude(Session::<ClaudeAgent>::new(config).await?))
            }
            shared::AgentType::Codex => Ok(Self::Codex(Session::<CodexAgent>::new(config).await?)),
        }
    }

    async fn stop(&mut self) -> Result<(), session_lib::SessionError> {
        match self {
            Self::Claude(s) => s.stop().await,
            Self::Codex(s) => s.stop().await,
        }
    }
}

/// Run a single proxy session as an in-process task.
/// Returns an exit code: Some(0) for normal exit, Some(1) for error, None for abort.
async fn run_session_task(
    mut config: ProxySessionConfig,
    cancel: CancellationToken,
) -> Option<i32> {
    loop {
        // On resume, look up the codex app-server thread id we persisted
        // on the previous launch. Missing entry / claude sessions yield
        // `None` and the codex io-task falls back to `thread_start`.
        let codex_thread_id = if config.resume {
            load_codex_thread_id(config.session_id)
        } else {
            None
        };

        let session_config = SessionConfig {
            session_id: config.session_id,
            working_directory: PathBuf::from(&config.working_directory),
            session_name: config.session_name.clone(),
            resume: config.resume,
            claude_path: None,
            extra_args: config.claude_args.clone(),
            agent_type: config.agent_type,
            codex_thread_id,
        };

        let mut session = match AnySession::new(session_config).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to create {} session: {}", config.agent_type, e);
                return Some(1);
            }
        };

        let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<PortalInput>();

        // run_connection_loop is generic over A: Agent, so we dispatch
        // here on the AnySession variant. Everything else
        // (cancellation, retry-on-SessionNotFound) is agent-agnostic.
        let result = tokio::select! {
            r = async {
                match &mut session {
                    AnySession::Claude(s) => {
                        run_connection_loop(&config, s, input_tx, &mut input_rx).await
                    }
                    AnySession::Codex(s) => {
                        run_connection_loop(&config, s, input_tx, &mut input_rx).await
                    }
                }
            } => r,
            _ = cancel.cancelled() => {
                info!("Session {} cancelled by stop request", config.session_id);
                let _ = session.stop().await;
                return Some(0);
            }
        };

        let _ = session.stop().await;

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
