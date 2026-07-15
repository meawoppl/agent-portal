use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};
use uuid::Uuid;

use claude_session_lib::proxy_session::{get_git_branch, CodexThreadIdSink};
use claude_session_lib::{
    claude_transcript_status, run_connection_loop, ClaudeAgent, LoopResult, PortalInput,
    ProxySessionConfig, TranscriptStatus,
};
use codex_session_lib::CodexAgent;
use session_lib::{Session, SessionConfig};
use shared::SessionExitReason;

use crate::path_policy;

/// A `NormalExit` that lands faster than this after spawn is treated as a crash
/// loop (`SessionExitReason::CrashedEarly`) rather than a healthy completion, so
/// the backend can back off relaunching it. Matches the launcher's warn log.
const CRASH_LOOP_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(10);

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

/// Reverse of the persisted map: given a codex thread id (which a codex agent
/// exposes as `$CODEX_THREAD_ID`), find the portal session id it belongs to.
/// Lets `agent-portal message` attribute codex senders with no injection.
pub(crate) fn session_id_for_codex_thread(thread_id: &str) -> Option<Uuid> {
    load_codex_threads()
        .into_iter()
        .find(|(_, tid)| tid == thread_id)
        .map(|(session_id, _)| session_id)
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
    pub reason: SessionExitReason,
}

/// What a `run_session_task` run produced: a process exit code (for logs) plus
/// a typed reason the backend uses to decide whether to throttle relaunch.
struct TaskOutcome {
    exit_code: Option<i32>,
    reason: SessionExitReason,
}

struct ManagedTask {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
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

        // Dedup guard: never spawn a second task for a session that's already
        // running. A reconcile/launch race (the backend's running/pending sets
        // are eventually consistent) could otherwise overwrite the `ManagedTask`
        // in the map — orphaning the old task (its cancel token lost, so it can
        // never be stopped) and double-spawning the agent process. That's the
        // "two claude procs for one session id" symptom. Treat it as a no-op.
        if self.tasks.contains_key(&session_id) {
            warn!(
                "Session {} already running; ignoring duplicate launch request",
                session_id
            );
            return Ok(session_id);
        }

        let name = params
            .session_name
            .clone()
            .unwrap_or_else(claude_session_lib::default_session_name);

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
            let outcome = run_session_task(proxy_config, cancel_clone).await;
            let _ = exit_tx.send(SessionExited {
                session_id,
                exit_code: outcome.exit_code,
                reason: outcome.reason,
            });
        });

        info!(
            "Spawned session task: session_id={}, session_name={}, dir={}",
            session_id, name, working_directory
        );

        self.tasks
            .insert(session_id, ManagedTask { handle, cancel });

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

/// Run a single proxy session as an in-process task. Returns the process exit
/// code (for logs) plus a typed [`SessionExitReason`] the backend uses to throttle
/// crash loops.
async fn run_session_task(
    mut config: ProxySessionConfig,
    cancel: CancellationToken,
) -> TaskOutcome {
    loop {
        // Pre-flight: if we're about to `claude --resume <id>` but the local
        // transcript is gone, claude exits near-instantly and reconcile would
        // relaunch forever. Detect it up front and rotate to a fresh session in
        // one step, instead of crash-looping until claude happens to emit
        // "No conversation found". Claude-only (codex has no transcript file);
        // `Unknown` (path-encoding uncertainty) falls through to a normal spawn.
        if config.resume
            && config.agent_type == shared::AgentType::Claude
            && claude_transcript_status(
                std::path::Path::new(&config.working_directory),
                config.session_id,
            ) == TranscriptStatus::Missing
        {
            let old_id = config.session_id;
            let new_id = Uuid::new_v4();
            warn!(
                "Session {} resume target transcript missing; rotating to fresh session {} without spawning",
                old_id, new_id
            );
            config.session_id = new_id;
            config.resume = false;
            config.replaces_session_id = Some(old_id);
            continue;
        }

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
                return TaskOutcome {
                    exit_code: Some(1),
                    reason: SessionExitReason::Error,
                };
            }
        };

        let (input_tx, mut input_rx) = tokio::sync::mpsc::unbounded_channel::<PortalInput>();

        // Track how long this attempt stays alive. A session that exits almost
        // immediately, over and over, is a crash loop — reconcile relaunches it
        // on every heartbeat (and `launch_failure_count` stays 0 because it does
        // register), so the elapsed time is the only signal that distinguishes
        // "ran for hours then ended" from "died on spawn 40 times".
        let started = std::time::Instant::now();

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
                // Surface stop errors: `stop()` now group-kills the agent
                // process (#927), so a failure here means a possible orphan.
                if let Err(e) = session.stop().await {
                    warn!("Session {} stop failed on cancel: {}", config.session_id, e);
                }
                return TaskOutcome {
                    exit_code: Some(0),
                    reason: SessionExitReason::Stopped,
                };
            }
        };

        if let Err(e) = session.stop().await {
            warn!("Session {} stop failed: {}", config.session_id, e);
        }

        let alive = started.elapsed();
        match result {
            Ok(LoopResult::NormalExit) => {
                // A near-instant clean exit is a crash loop, not a completion.
                // Tell the backend so it backs off relaunching (the wall-clock
                // gap is the only signal — it did register, so the launch
                // "succeeded"). A healthy run resets the backoff counter.
                if alive < CRASH_LOOP_THRESHOLD {
                    // A *resume* that dies this fast is almost always a wiped
                    // transcript: `claude --resume <id>` prints "No conversation
                    // found" and exits ~immediately, which surfaces here as a
                    // clean NormalExit (not SessionNotFound). The pre-flight
                    // guard only catches that when the project dir exists
                    // (TranscriptStatus::Missing); when the whole project dir is
                    // absent it returns Unknown and lets the spawn through — so
                    // reconcile would relaunch the same doomed --resume forever.
                    // Break the loop by rotating to a fresh session once. Skip
                    // the rotation only when the transcript is confirmed Present:
                    // then the early exit is some other fault, and discarding the
                    // resume would needlessly lose conversation continuity.
                    if config.resume
                        && config.agent_type == shared::AgentType::Claude
                        && claude_transcript_status(
                            std::path::Path::new(&config.working_directory),
                            config.session_id,
                        ) != TranscriptStatus::Present
                    {
                        let old_id = config.session_id;
                        let new_id = Uuid::new_v4();
                        warn!(
                            "Session {} exited after only {:.1?} on resume with no confirmed \
                             transcript — rotating to fresh session {} instead of crash-looping",
                            old_id, alive, new_id
                        );
                        config.session_id = new_id;
                        config.resume = false;
                        config.replaces_session_id = Some(old_id);
                        continue;
                    }
                    warn!(
                        "Session {} exited normally after only {:.1?} — likely crash loop, \
                         reporting CrashedEarly so reconcile backs off",
                        config.session_id, alive
                    );
                    return TaskOutcome {
                        exit_code: Some(0),
                        reason: SessionExitReason::CrashedEarly,
                    };
                }
                info!(
                    "Session {} exited normally after {:.1?}",
                    config.session_id, alive
                );
                return TaskOutcome {
                    exit_code: Some(0),
                    reason: SessionExitReason::Completed,
                };
            }
            Ok(LoopResult::RegistrationRejected) => {
                // The token this proxy was launched with is dead (revoked or
                // expired). Exit so the launcher removes us from its running
                // set; reconcile will relaunch with a freshly minted token
                // (#1045). Staying alive and reconnecting would just hammer
                // the backend forever and block that relaunch.
                warn!(
                    "Session {} registration rejected by server after {:.1?}, exiting for relaunch",
                    config.session_id, alive
                );
                return TaskOutcome {
                    exit_code: Some(1),
                    reason: SessionExitReason::RegistrationRejected,
                };
            }
            Ok(LoopResult::SessionNotFound) => {
                if !config.resume {
                    info!("Session {} not found, not resuming", config.session_id);
                    return TaskOutcome {
                        exit_code: Some(0),
                        reason: SessionExitReason::ResumeTargetMissing,
                    };
                }
                // Retry with a fresh session
                let old_id = config.session_id;
                let new_id = Uuid::new_v4();
                warn!(
                    "Session {} not found after {:.1?} (resume target missing), retrying as fresh session {}",
                    old_id, alive, new_id
                );
                config.session_id = new_id;
                config.resume = false;
                config.replaces_session_id = Some(old_id);
            }
            Err(e) => {
                error!(
                    "Session {} failed after {:.1?}: {}",
                    config.session_id, alive, e
                );
                return TaskOutcome {
                    exit_code: Some(1),
                    reason: SessionExitReason::Error,
                };
            }
        }
    }
}
