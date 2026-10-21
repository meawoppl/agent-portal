use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use antigravity_session_lib::AntigravityAgent;
use claude_session_lib::proxy_session::{
    ClaudeConversationIdSink, CodexThreadIdSink, MediaDisplaySink,
};
use claude_session_lib::{
    claude_transcript_id, claude_transcript_status, run_connection_loop,
    should_prime_portal_reminder_on_first_input, ClaudeAgent, LoopResult, PortalInput,
    ProxySessionConfig, TranscriptStatus,
};
use codex_session_lib::CodexAgent;
use muse_session_lib::MuseAgent;
use session_lib::git_metadata::get_git_branch;
use session_lib::{Session, SessionConfig};
use shared::SessionExitReason;

use crate::config;
use crate::path_policy;

/// A `NormalExit` that lands faster than this after spawn is treated as a crash
/// loop (`SessionExitReason::CrashedEarly`) rather than a healthy completion, so
/// the backend can back off relaunching it. Matches the launcher's warn log.
const CRASH_LOOP_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(10);

/// Path to the launcher's sidecar codex-thread map: `session_id -> thread_id`.
/// Lives next to `launcher.json` in the config dir so it ships with the same
/// install/uninstall surface and survives across restarts.
fn codex_threads_path() -> PathBuf {
    session_lib::paths::config_dir().join("codex_threads.json")
}

/// Load a launcher sidecar map (`session_id -> value`), tolerating a missing
/// or corrupt file with an empty map. These files are derived caches, so
/// losing one degrades to the pre-persisted behavior rather than failing.
fn load_sidecar_map<T>(path: &std::path::Path) -> HashMap<Uuid, T>
where
    T: serde::de::DeserializeOwned,
{
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Persist a sidecar map as pretty JSON, creating the parent directory first.
fn save_sidecar_map<T>(path: &std::path::Path, map: &HashMap<Uuid, T>) -> std::io::Result<()>
where
    T: serde::Serialize,
{
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(map)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(path, body)
}

fn load_codex_threads() -> HashMap<Uuid, String> {
    load_sidecar_map(&codex_threads_path())
}

// All Codex map updates share a lock, including discoveries emitted by live
// sessions. Atomic replacement keeps readers and restarts from seeing half JSON.
static CODEX_MAP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn update_codex_thread_map(
    session_id: Uuid,
    thread_id: &str,
    importing: bool,
) -> anyhow::Result<()> {
    let _guard = CODEX_MAP_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("Codex session map lock poisoned"))?;
    update_codex_thread_map_at(&codex_threads_path(), session_id, thread_id, importing)
}

fn update_codex_thread_map_at(
    path: &std::path::Path,
    session_id: Uuid,
    thread_id: &str,
    importing: bool,
) -> anyhow::Result<()> {
    let mut map: HashMap<Uuid, String> = match std::fs::read_to_string(path) {
        Ok(body) => serde_json::from_str(&body)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
        Err(e) => return Err(e.into()),
    };
    if importing {
        if map
            .iter()
            .any(|(id, thread)| thread == thread_id && *id != session_id)
        {
            anyhow::bail!("This Codex conversation is already managed by another Portal session");
        }
        if map
            .get(&session_id)
            .is_some_and(|thread| thread != thread_id)
        {
            anyhow::bail!("Portal session is already bound to another Codex conversation");
        }
    }
    if importing {
        // Separate additive metadata keeps codex_threads.json compatible with
        // older launchers. Imported contexts need the Portal reminder on their
        // first user input after resume; native history has never received it.
        let imports_path = path.with_file_name("codex_imports.json");
        let mut imports: HashMap<Uuid, String> = match std::fs::read_to_string(&imports_path) {
            Ok(body) => serde_json::from_str(&body)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e.into()),
        };
        imports.insert(session_id, thread_id.to_owned());
        save_codex_map_atomically(&imports_path, &imports)?;
    }
    map.insert(session_id, thread_id.to_owned());
    save_codex_map_atomically(path, &map)
}

fn save_codex_map_atomically(
    path: &std::path::Path,
    map: &HashMap<Uuid, String>,
) -> anyhow::Result<()> {
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    save_sidecar_map(&temp, map)?;
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(e.into());
    }
    Ok(())
}

pub(crate) fn bind_imported_codex_thread(session_id: Uuid, thread_id: &str) -> anyhow::Result<()> {
    update_codex_thread_map(session_id, thread_id, true)
}

/// Sidecar map of sessions restored from the history archive:
/// `restored session id -> archived source session id`. Marks sessions whose
/// whole point is to continue an existing native conversation, so a resume
/// must fail rather than quietly start a fresh one.
fn restored_sessions_path() -> PathBuf {
    session_lib::paths::config_dir().join("restored_sessions.json")
}

fn is_restored_session(session_id: Uuid) -> bool {
    load_sidecar_map::<Uuid>(&restored_sessions_path()).contains_key(&session_id)
}

/// Bind an archived session's native conversation to `session_id`.
///
/// The archived session no longer exists in Portal, but its Claude transcript
/// or Codex thread usually survives on this host (deleting a session never
/// touches native state). Point the new session at it: Claude via the
/// conversation sidecar (`--resume` follows it), Codex by moving the thread
/// binding. Idempotent, so a retried restore succeeds.
pub(crate) fn bind_restored_session(
    agent_type: shared::AgentType,
    source_session_id: Uuid,
    session_id: Uuid,
    working_directory: &Path,
) -> anyhow::Result<()> {
    match agent_type {
        shared::AgentType::Claude => {
            // A repeat restore finds the binding already made.
            let conversation = load_claude_conversation_id(session_id)
                .unwrap_or_else(|| resolved_transcript_id(source_session_id));
            if claude_transcript_status(working_directory, conversation)
                == TranscriptStatus::Missing
            {
                anyhow::bail!("This session's Claude conversation is no longer on this computer");
            }
            let mut map = load_claude_conversations();
            map.insert(session_id, conversation);
            save_sidecar_map(&claude_conversations_path(), &map)?;
        }
        shared::AgentType::Codex => {
            let _guard = CODEX_MAP_LOCK
                .lock()
                .map_err(|_| anyhow::anyhow!("Codex session map lock poisoned"))?;
            let path = codex_threads_path();
            let mut map: HashMap<Uuid, String> = load_sidecar_map(&path);
            let thread = match map.get(&session_id) {
                Some(thread) => thread.clone(),
                None => map.remove(&source_session_id).ok_or_else(|| {
                    anyhow::anyhow!("This session's Codex thread is no longer on this computer")
                })?,
            };
            map.insert(session_id, thread);
            save_codex_map_atomically(&path, &map)?;
        }
        shared::AgentType::Muse | shared::AgentType::Antigravity => {
            anyhow::bail!("Only Claude and Codex sessions can be restored")
        }
    }
    let mut restored: HashMap<Uuid, Uuid> = load_sidecar_map(&restored_sessions_path());
    restored.insert(session_id, source_session_id);
    save_sidecar_map(&restored_sessions_path(), &restored)?;
    Ok(())
}

pub(crate) fn load_codex_thread_id(session_id: Uuid) -> Option<String> {
    load_codex_threads().get(&session_id).cloned()
}

/// Sidecar map of claude's *current* conversation per portal session, the
/// claude counterpart to `codex_threads.json`.
///
/// `/clear` rolls claude onto a new conversation while the portal session id
/// stays put, so from that point `--resume <portal id>` re-opens the pre-clear
/// transcript. Persisting the divergence lets a relaunch resume what the user
/// can actually see. Deliberately a separate file rather than a field in
/// `launcher.json`: it is derived cache, rewritten from the output stream, and
/// losing it degrades to the old behavior rather than corrupting config.
fn claude_conversations_path() -> PathBuf {
    session_lib::paths::config_dir().join("claude_conversations.json")
}

fn load_claude_conversations() -> HashMap<Uuid, Uuid> {
    load_sidecar_map(&claude_conversations_path())
}

fn load_claude_conversation_id(session_id: Uuid) -> Option<Uuid> {
    load_claude_conversations().get(&session_id).copied()
}

/// The id naming claude's transcript on disk for `session_id` right now, from
/// the launcher's persisted view of the divergence.
///
/// Thin wrapper that feeds the sidecar into the shared rule the argv builder
/// uses, so a gate and the spawn it guards can never check different files —
/// see [`claude_session_lib::claude_transcript_id`] for why that matters.
pub(crate) fn resolved_transcript_id(session_id: Uuid) -> Uuid {
    claude_transcript_id(session_id, load_claude_conversation_id(session_id))
}

/// Upload images the agent reads so they render inline in the transcript.
///
/// The detection half lives in `claude_session_lib::proxy_session::media_display`;
/// the upload lands here because only the launcher holds the session credentials.
/// Fire-and-forget by construction: the sink is called from the connection's
/// output path, so it must not block, and a failed display is never worth
/// interrupting a session over — it is logged and dropped. Deliberately quiet
/// about the common benign failures (file already moved, oversize, unsupported
/// container), which are `debug!` rather than `warn!`.
fn make_media_display_sink(session_id: Uuid) -> MediaDisplaySink {
    std::sync::Arc::new(move |path: PathBuf| {
        tokio::spawn(async move {
            match crate::media::upload_media(&session_id.to_string(), &path).await {
                Ok((_, filename, _, _)) => {
                    info!("Displayed {} read by session {}", filename, session_id)
                }
                Err(e) => debug!(
                    "Not displaying {} for session {}: {}",
                    path.display(),
                    session_id,
                    e
                ),
            }
        });
    })
}

fn make_claude_conversation_id_sink(session_id: Uuid) -> ClaudeConversationIdSink {
    std::sync::Arc::new(move |conversation_id: Uuid| {
        let mut map = load_claude_conversations();
        if map.get(&session_id) == Some(&conversation_id) {
            return;
        }
        map.insert(session_id, conversation_id);
        match save_sidecar_map(&claude_conversations_path(), &map) {
            Ok(()) => info!(
                "Claude conversation for session {} recorded as {}",
                session_id, conversation_id
            ),
            Err(e) => warn!(
                "Failed to persist claude conversation id for session {}: {}",
                session_id, e
            ),
        }
    })
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
        if let Err(e) = update_codex_thread_map(session_id, &thread_id, false) {
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
    /// The session id the task is *currently* running under. `run_session_task`
    /// may rotate to a fresh id mid-flight (missing resume transcript, "No
    /// conversation found"); the map key stays the id the spawn was asked for,
    /// so the heartbeat, stop and dedup paths consult this instead.
    live_id: LiveSessionId,
}

/// Shared handle to a task's current session id, updated on rotation.
///
/// Without this the launcher kept reporting the *original* id in its
/// heartbeat after a rotation, the backend saw the rotated session as
/// desired-but-not-running, relaunched it with `--resume`, the launcher
/// rotated again, and every heartbeat minted another empty session until
/// the launcher hit its session cap.
#[derive(Clone, Debug)]
pub(crate) struct LiveSessionId(std::sync::Arc<std::sync::Mutex<Uuid>>);

impl LiveSessionId {
    fn new(id: Uuid) -> Self {
        Self(std::sync::Arc::new(std::sync::Mutex::new(id)))
    }

    pub(crate) fn get(&self) -> Uuid {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn set(&self, id: Uuid) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = id;
    }
}

pub struct SpawnParams {
    pub history_restore: Option<shared::api::HistoryRestoreLaunch>,
    pub auth_token: String,
    pub working_directory: String,
    pub session_name: Option<String>,
    pub claude_args: Vec<String>,
    pub agent_type: shared::AgentType,
    pub scheduled_task_id: Option<Uuid>,
    pub resume_session_id: Option<Uuid>,
    /// Whether `resume_session_id` is an existing session to resume
    /// (`Some(true)`) or a freshly-assigned id for a brand-new session to
    /// create (`Some(false)`). `None` (older backend) falls back to the
    /// historical "any id means resume" behavior. See the field of the same
    /// name on `ServerToLauncher::LaunchSession` (#1405).
    pub resume: Option<bool>,
    /// When true, create a git worktree from the repository containing
    /// `working_directory` and run the session inside the new worktree.
    pub create_worktree: bool,
    /// Optional branch name for the worktree. When omitted a timestamped
    /// default (`session-<YYYYMMDD-HHMMSS>`) is derived.
    pub worktree_branch: Option<String>,
    pub scratch_worktree: bool,
    pub fork_from_session_id: Option<Uuid>,
    pub fork_point_turn_id: Option<String>,
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

    /// Ids the backend should consider running: each task's *live* id, so a
    /// session that rotated mid-flight is reported under the id it now holds.
    pub fn running_session_ids(&self) -> Vec<Uuid> {
        self.tasks.values().map(|t| t.live_id.get()).collect()
    }

    /// Map a session id (original or rotated) to the key of the task running it.
    fn task_key(&self, session_id: &Uuid) -> Option<Uuid> {
        if self.tasks.contains_key(session_id) {
            return Some(*session_id);
        }
        self.tasks
            .iter()
            .find(|(_, t)| t.live_id.get() == *session_id)
            .map(|(k, _)| *k)
    }

    pub async fn spawn(&mut self, mut params: SpawnParams) -> anyhow::Result<Uuid> {
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

        // Resolve (and validate) the base directory the request targets. It
        // must exist and live under the launcher user's home directory.
        let base_dir = path_policy::ensure_existing_dir_under_home(&params.working_directory)?;

        if let Some(source_id) = params.fork_from_session_id {
            match params.agent_type {
                shared::AgentType::Claude => {
                    // Validate the transcript the fork will actually branch from
                    // — the source's *conversation* — not its portal id, which
                    // names the pre-clear transcript once the source has been
                    // `/clear`ed. See `resolved_transcript_id`.
                    if claude_transcript_status(&base_dir, resolved_transcript_id(source_id))
                        == TranscriptStatus::Missing
                    {
                        anyhow::bail!(
                            "Cannot fork: source Claude transcript is missing on this launcher"
                        );
                    }
                }
                shared::AgentType::Codex => {
                    if load_codex_thread_id(source_id).is_none() {
                        anyhow::bail!(
                            "Cannot fork: source Codex session has no persisted thread (it may have no turns yet)"
                        );
                    }
                }
                shared::AgentType::Muse => anyhow::bail!("Muse sessions cannot be forked"),
                shared::AgentType::Antigravity => {
                    anyhow::bail!("Antigravity sessions cannot be forked")
                }
            }
        }

        // When the request opts into a worktree, create it from the repo that
        // contains `base_dir` and run the session there instead. Otherwise the
        // session runs directly in `base_dir`.
        let mut scratch_cleanup = None;
        let working_directory = if params.scratch_worktree {
            let branch = params.worktree_branch.as_deref().ok_or_else(|| {
                anyhow::anyhow!("Scratch worktree requires a deterministic branch")
            })?;
            let scratch = crate::worktree::create_scratch_worktree(&base_dir, branch)?;
            let path = scratch.path.to_string_lossy().to_string();
            scratch_cleanup = Some(scratch);
            path
        } else if params.create_worktree {
            let worktree =
                crate::worktree::create_worktree(&base_dir, params.worktree_branch.as_deref())?;
            // The derived worktree must also be under home (create_worktree
            // places it inside the repo, which already passed the check, but we
            // re-validate defensively before running anything there).
            path_policy::ensure_canonical_path_under_home(&worktree)?;
            worktree.to_string_lossy().to_string()
        } else {
            base_dir.to_string_lossy().to_string()
        };

        // A supplied id is resumed only when the backend says it names an
        // existing session. A brand-new dialog launch supplies its freshly
        // minted id with `resume: Some(false)` so we create it under that id
        // (`--session-id`) rather than `--resume`-ing a nonexistent transcript
        // and rotating to a different id — the rotation changed the id the
        // dashboard had already focused, dropping composer focus on every new
        // session (#1405). `None` (older backend) keeps the historical
        // "any id means resume" behavior. No id at all is always a fresh
        // session with a random id.
        let (session_id, mut resume) = match params.resume_session_id {
            Some(id) => (id, params.resume.unwrap_or(true)),
            None => (Uuid::new_v4(), false),
        };

        // A restored session exists only to continue its native conversation.
        // Refuse to launch it fresh if that conversation has since vanished,
        // instead of the usual quiet fallback to a new one.
        if resume && is_restored_session(session_id) {
            let present = match params.agent_type {
                shared::AgentType::Claude => {
                    claude_transcript_status(
                        Path::new(&working_directory),
                        resolved_transcript_id(session_id),
                    ) != TranscriptStatus::Missing
                }
                shared::AgentType::Codex => load_codex_thread_id(session_id).is_some(),
                shared::AgentType::Muse | shared::AgentType::Antigravity => false,
            };
            if !present {
                anyhow::bail!(
                    "Cannot resume restored session: its native conversation is no longer on this computer"
                );
            }
        }

        // Dedup guard: never spawn a second task for a session that's already
        // running. A reconcile/launch race (the backend's running/pending sets
        // are eventually consistent) could otherwise overwrite the `ManagedTask`
        // in the map — orphaning the old task (its cancel token lost, so it can
        // never be stopped) and double-spawning the agent process. That's the
        // "two claude procs for one session id" symptom. Treat it as a no-op.
        if self.task_key(&session_id).is_some() {
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

        if let Some(restore) = &params.history_restore {
            if restore.mode == shared::api::HistoryRestoreMode::Native {
                let has_native = match params.agent_type {
                    shared::AgentType::Codex => load_codex_thread_id(session_id).is_some(),
                    shared::AgentType::Claude => {
                        load_claude_conversation_id(session_id).is_some()
                            || claude_transcript_status(&base_dir, session_id)
                                == TranscriptStatus::Present
                    }
                    _ => false,
                };
                if has_native {
                    resume = true;
                } else {
                    resume = false;
                    params.fork_from_session_id = Some(restore.source_session_id);
                }
            }
        }
        let proxy_config = ProxySessionConfig {
            history_restore: params.history_restore,
            backend_url: self.backend_url.clone(),
            session_id,
            session_name: name.clone(),
            auth_token: Some(params.auth_token),
            working_directory: working_directory.clone(),
            resume,
            prime_portal_reminder_on_first_input: !resume,
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
            // Claude counterpart: learn the conversation id `/clear` rolls
            // to, so the next resume opens the live transcript (not the
            // pre-clear one).
            claude_conversation_id_sink: Some(make_claude_conversation_id_sink(session_id)),
            media_display_sink: Some(make_media_display_sink(session_id)),
            fork_from_session_id: params.fork_from_session_id,
            fork_point_turn_id: params.fork_point_turn_id,
        };

        let exit_tx = self.exit_tx.clone();
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let live_id = LiveSessionId::new(session_id);
        let task_live_id = live_id.clone();

        let handle = tokio::spawn(async move {
            let outcome = run_session_task(proxy_config, cancel_clone, &task_live_id).await;
            if let Some(scratch) = scratch_cleanup {
                crate::worktree::cleanup_scratch_worktree(scratch);
            }
            // Report the id the task ended up running under, so the backend's
            // backoff accounting lands on the live row, not a replaced one.
            let _ = exit_tx.send(SessionExited {
                session_id: task_live_id.get(),
                exit_code: outcome.exit_code,
                reason: outcome.reason,
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
                live_id,
            },
        );

        Ok(session_id)
    }

    pub async fn stop(&mut self, session_id: &Uuid) -> bool {
        let task = self
            .task_key(session_id)
            .and_then(|key| self.tasks.remove(&key));
        if let Some(mut task) = task {
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
        if let Some(key) = self.task_key(session_id) {
            self.tasks.remove(&key);
        }
    }
}

/// Heterogeneous wrapper around `Session<A>` so we can store Claude- and
/// Codex-backed sessions side-by-side. The launcher is the one place
/// agent-portal needs to dispatch across agent types in the same process.
enum AnySession {
    Claude(Session<ClaudeAgent>),
    Codex(Session<CodexAgent>),
    Muse(Session<MuseAgent>),
    Antigravity(Session<AntigravityAgent>),
}

impl AnySession {
    async fn new(config: SessionConfig) -> Result<Self, session_lib::SessionError> {
        match config.agent_type {
            shared::AgentType::Claude => {
                Ok(Self::Claude(Session::<ClaudeAgent>::new(config).await?))
            }
            shared::AgentType::Codex => Ok(Self::Codex(Session::<CodexAgent>::new(config).await?)),
            shared::AgentType::Muse => Ok(Self::Muse(Session::<MuseAgent>::new(config).await?)),
            shared::AgentType::Antigravity => Ok(Self::Antigravity(
                Session::<AntigravityAgent>::new(config).await?,
            )),
        }
    }

    async fn stop(&mut self) -> Result<(), session_lib::SessionError> {
        match self {
            Self::Claude(s) => s.stop().await,
            Self::Codex(s) => s.stop().await,
            Self::Muse(s) => s.stop().await,
            Self::Antigravity(s) => s.stop().await,
        }
    }
}

/// Run a single proxy session as an in-process task. Returns the process exit
/// code (for logs) plus a typed [`SessionExitReason`] the backend uses to throttle
/// crash loops.
async fn run_session_task(
    mut config: ProxySessionConfig,
    cancel: CancellationToken,
    live_id: &LiveSessionId,
) -> TaskOutcome {
    // A native-state probe can start a temporary app-server. Keep it inside
    // the per-session task so slow probes never stall launcher heartbeats or
    // other sessions, and a pause can cancel a pending probe immediately.
    if let Some(restore) = config
        .history_restore
        .as_ref()
        .filter(|r| r.mode == shared::api::HistoryRestoreMode::Native && !config.resume)
    {
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => return TaskOutcome {
                exit_code: Some(0),
                reason: SessionExitReason::Stopped,
            },
            result = tokio::time::timeout(
                std::time::Duration::from_secs(90),
                crate::history_restore::check(config.agent_type, &config.working_directory, restore),
            ) => result.map_err(|_| anyhow::anyhow!("Native restore check timed out")).and_then(|r| r),
        };
        if let Err(error) = result {
            error!("Native restore preflight failed: {error:#}");
            return TaskOutcome {
                exit_code: Some(1),
                reason: SessionExitReason::ResumeTargetMissing,
            };
        }
    }
    loop {
        // Pre-flight: if we're about to `claude --resume <id>` but the local
        // transcript is gone, claude exits near-instantly and reconcile would
        // relaunch forever. Detect it up front and rotate to a fresh session in
        // one step, instead of crash-looping until claude happens to emit
        // "No conversation found". Claude-only (codex has no transcript file);
        // `Unknown` (path-encoding uncertainty) falls through to a normal spawn.
        // Asks about the id the spawn below will actually `--resume` (see
        // `resolved_transcript_id`) — gating on the portal id would rotate away a
        // live post-`/clear` conversation whose pre-clear file had been cleaned up.
        if config.resume
            && config.agent_type == shared::AgentType::Claude
            && claude_transcript_status(
                std::path::Path::new(&config.working_directory),
                resolved_transcript_id(config.session_id),
            ) == TranscriptStatus::Missing
        {
            if config
                .history_restore
                .as_ref()
                .is_some_and(|r| r.mode == shared::api::HistoryRestoreMode::Native)
            {
                error!(
                    "Native restored conversation is missing; refusing to start an empty session"
                );
                return TaskOutcome {
                    exit_code: Some(1),
                    reason: SessionExitReason::ResumeTargetMissing,
                };
            }
            let old_id = config.session_id;
            if resolved_transcript_id(old_id) == old_id {
                // Nothing to resume *and* no other transcript claims this id,
                // so create the conversation under the id we already have
                // (`--session-id`), exactly like a brand-new dialog launch.
                // Keeping the id means the backend row, the dashboard focus
                // and the launcher's running set all stay in agreement; the
                // previous rotate-to-a-fresh-id here minted a new session row
                // on every relaunch of a never-messaged session.
                warn!(
                    "Session {} resume target transcript missing; starting it fresh under the same id",
                    old_id
                );
                config.resume = false;
                continue;
            }
            // The id maps to a post-`/clear` conversation whose file is gone;
            // `--session-id old_id` could collide with the pre-clear transcript,
            // so rotate to a fresh id as before.
            let new_id = Uuid::new_v4();
            warn!(
                "Session {} resume target transcript missing; rotating to fresh session {} without spawning",
                old_id, new_id
            );
            config.session_id = new_id;
            live_id.set(new_id);
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
        // A fork carries the source conversation, which already holds the
        // reminder, so its first input must not deliver a second copy.
        config.prime_portal_reminder_on_first_input = config.fork_from_session_id.is_none()
            && should_prime_portal_reminder_on_first_input(
                config.agent_type,
                config.resume,
                codex_thread_id.as_deref(),
            );
        let mut codex_require_resume = config.resume
            && config
                .history_restore
                .as_ref()
                .is_some_and(|r| r.mode == shared::api::HistoryRestoreMode::Native);
        if config.agent_type == shared::AgentType::Codex && config.resume {
            let imports: HashMap<Uuid, String> =
                load_sidecar_map(&codex_threads_path().with_file_name("codex_imports.json"));
            let imported = imports.get(&config.session_id);
            config.prime_portal_reminder_on_first_input |=
                imported == codex_thread_id.as_ref() && codex_thread_id.is_some();
            codex_require_resume |= imported.is_some() || is_restored_session(config.session_id);
        }

        // The portal's established dangerous-permissions wire format is the
        // agent argument list. Consume Muse's token here and carry it as typed
        // config so the backend uses MuseExecBuilder::yolo rather than a raw
        // passthrough argument.
        let muse_yolo = config.agent_type == shared::AgentType::Muse
            && config.claude_args.iter().any(|arg| arg == "--yolo");
        let extra_args = if config.agent_type == shared::AgentType::Muse {
            config
                .claude_args
                .iter()
                .filter(|arg| arg.as_str() != "--yolo")
                .cloned()
                .collect()
        } else {
            config.claude_args.clone()
        };
        let plugin_skill_context = config::plugin_skill_context(
            config.agent_type,
            config.session_id,
            std::path::Path::new(&config.working_directory),
        );
        let plugin_skill_cleanup_dir = plugin_skill_context.cleanup_dir.clone();
        let plugin_skill = plugin_skill_launch_policy(
            config.agent_type,
            extra_args,
            plugin_skill_context.claude_plugin_dirs,
            plugin_skill_context.reminder,
        );
        let extra_args = plugin_skill.extra_args;
        let plugin_skill_reminder = plugin_skill.reminder;

        // A wrapper joins the cgroup before exec, so no agent tool can race a
        // post-spawn PID move. Antigravity connects to an existing IDE process.
        let limits = config::load_config().session_memory.unwrap_or_default();
        let executable = match config.agent_type {
            shared::AgentType::Claude => Some("claude"),
            shared::AgentType::Codex => Some("codex"),
            shared::AgentType::Muse => Some("muse"),
            shared::AgentType::Antigravity => {
                info!(session_id = %config.session_id, "Antigravity uses an external IDE; no per-session cgroup containment");
                None
            }
        };
        let session_group = if let Some(binary) = executable {
            match crate::session_cgroups::SessionGroup::prepare(
                config.session_id,
                Path::new(binary),
                limits.high_bytes,
                limits.max_bytes,
            ) {
                Ok(group) => group,
                Err(error) if limits.high_bytes.is_some() || limits.max_bytes.is_some() => {
                    error!(session_id = %config.session_id, "Cannot apply configured session memory limits: {error:#}");
                    cleanup_plugin_skill_root(plugin_skill_cleanup_dir.as_deref());
                    return TaskOutcome {
                        exit_code: Some(1),
                        reason: SessionExitReason::Error,
                    };
                }
                Err(error) => {
                    warn!(session_id = %config.session_id, "Session memory containment unavailable; using ordinary process groups: {error:#}");
                    None
                }
            }
        } else {
            None
        };
        if session_group.is_none() && (limits.high_bytes.is_some() || limits.max_bytes.is_some()) {
            warn!(session_id = %config.session_id, "Memory delegation unsupported for this run; configured session limits are not enforced");
        }
        let session_config = SessionConfig {
            runtime_command: session_group.as_ref().map(|group| group.command()),
            memory_events_path: session_group.as_ref().map(|group| group.events()),
            recovery_context: config
                .history_restore
                .as_ref()
                .and_then(|r| r.context.clone()),
            session_id: config.session_id,
            working_directory: PathBuf::from(&config.working_directory),
            session_name: config.session_name.clone(),
            resume: config.resume,
            claude_path: None,
            extra_args,
            plugin_skill_reminder,
            muse_yolo,
            agent_type: config.agent_type,
            codex_thread_id,
            codex_require_resume,
            fork_from_session_id: config.fork_from_session_id,
            codex_fork_from_thread_id: config.fork_from_session_id.and_then(load_codex_thread_id),
            claude_conversation_id: load_claude_conversation_id(config.session_id),
            claude_fork_from_conversation_id: config
                .fork_from_session_id
                .and_then(load_claude_conversation_id),
            codex_fork_last_turn_id: config.fork_point_turn_id.clone(),
        };

        // Fork metadata seeds one spawn attempt only. Rotation/retry paths may
        // set resume=false again and must never silently re-fork (#1537).
        config.fork_from_session_id = None;
        config.fork_point_turn_id = None;

        let mut session = match AnySession::new(session_config).await {
            Ok(s) => s,
            Err(e) => {
                error!("Failed to create {} session: {}", config.agent_type, e);
                cleanup_plugin_skill_root(plugin_skill_cleanup_dir.as_deref());
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
                    AnySession::Muse(s) => {
                        run_connection_loop(&config, s, input_tx, &mut input_rx).await
                    }
                    AnySession::Antigravity(s) => {
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
                cleanup_plugin_skill_root(plugin_skill_cleanup_dir.as_deref());
                return TaskOutcome {
                    exit_code: Some(0),
                    reason: SessionExitReason::Stopped,
                };
            }
        };

        if let Err(e) = session.stop().await {
            warn!("Session {} stop failed: {}", config.session_id, e);
        }
        cleanup_plugin_skill_root(plugin_skill_cleanup_dir.as_deref());

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
                    // Same id discipline as the pre-flight gate above: ask about
                    // the conversation we actually resumed, or a stale pre-clear
                    // file reading `Present` suppresses the rotation and we
                    // crash-loop on exactly the case this exists to break.
                    if config.history_restore.is_none()
                        && config.resume
                        && config.agent_type == shared::AgentType::Claude
                        && claude_transcript_status(
                            std::path::Path::new(&config.working_directory),
                            resolved_transcript_id(config.session_id),
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
                        live_id.set(new_id);
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
            Ok(LoopResult::SessionNotFound) if config.history_restore.is_some() => {
                error!("Restored native conversation was not found; preserving the restored session ID");
                return TaskOutcome {
                    exit_code: Some(1),
                    reason: SessionExitReason::ResumeTargetMissing,
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
                live_id.set(new_id);
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

#[cfg(test)]
mod live_id_tests {
    use super::*;

    fn manager_with_task(key: Uuid) -> (ProcessManager, LiveSessionId) {
        let (mut pm, _rx) = ProcessManager::new("http://127.0.0.1:1".into(), 4);
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        let handle = tokio::spawn(async move { c.cancelled().await });
        let live_id = LiveSessionId::new(key);
        pm.tasks.insert(
            key,
            ManagedTask {
                handle,
                cancel,
                live_id: live_id.clone(),
            },
        );
        (pm, live_id)
    }

    #[tokio::test]
    async fn heartbeat_stop_and_dedup_follow_a_rotated_id() {
        let original = Uuid::new_v4();
        let rotated = Uuid::new_v4();
        let (mut pm, live_id) = manager_with_task(original);

        assert_eq!(pm.running_session_ids(), vec![original]);
        live_id.set(rotated);

        // The heartbeat reports the id the task now runs under, so the backend
        // never sees the rotated session as "desired but not running".
        assert_eq!(pm.running_session_ids(), vec![rotated]);
        assert_eq!(pm.task_key(&rotated), Some(original));
        assert_eq!(pm.task_key(&original), Some(original));
        assert_eq!(pm.task_key(&Uuid::new_v4()), None);

        // Stopping by the rotated id finds and cancels the task.
        assert!(pm.stop(&rotated).await);
        assert!(pm.running_session_ids().is_empty());
    }

    #[tokio::test]
    async fn remove_finished_accepts_the_live_id() {
        let original = Uuid::new_v4();
        let rotated = Uuid::new_v4();
        let (mut pm, live_id) = manager_with_task(original);
        live_id.set(rotated);
        pm.remove_finished(&rotated);
        assert!(pm.tasks.is_empty());
    }
}

#[cfg(test)]
mod sidecar_tests {
    use super::{load_sidecar_map, save_sidecar_map};
    use std::collections::HashMap;
    use uuid::Uuid;

    #[test]
    fn sidecar_map_round_trips_and_tolerates_missing_or_corrupt_files() {
        let dir = tempfile::tempdir().unwrap();
        // Nested path proves the save creates missing parent directories.
        let path = dir.path().join("sub").join("sidecar.json");

        // A missing file reads as empty rather than failing.
        let empty: HashMap<Uuid, String> = load_sidecar_map(&path);
        assert!(empty.is_empty());

        let id = Uuid::new_v4();
        let mut map = HashMap::new();
        map.insert(id, "thread-1".to_string());
        save_sidecar_map(&path, &map).unwrap();
        let loaded: HashMap<Uuid, String> = load_sidecar_map(&path);
        assert_eq!(loaded.get(&id).map(String::as_str), Some("thread-1"));

        // A corrupt file degrades to empty rather than failing the caller.
        std::fs::write(&path, "{not json").unwrap();
        let degraded: HashMap<Uuid, String> = load_sidecar_map(&path);
        assert!(degraded.is_empty());
    }
}

fn cleanup_plugin_skill_root(path: Option<&Path>) {
    if let Some(path) = path {
        let _ = std::fs::remove_dir_all(path);
    }
}

struct PluginSkillLaunchPolicy {
    extra_args: Vec<String>,
    reminder: Option<String>,
}

fn plugin_skill_launch_policy(
    agent_type: shared::AgentType,
    mut extra_args: Vec<String>,
    skill_roots: Vec<PathBuf>,
    reminder: Option<String>,
) -> PluginSkillLaunchPolicy {
    match agent_type {
        shared::AgentType::Claude => {
            for root in skill_roots {
                extra_args.push("--plugin-dir".to_string());
                extra_args.push(root.display().to_string());
            }
            PluginSkillLaunchPolicy {
                extra_args,
                reminder: None,
            }
        }
        shared::AgentType::Codex | shared::AgentType::Muse | shared::AgentType::Antigravity => {
            PluginSkillLaunchPolicy {
                extra_args,
                reminder,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill_roots() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/plugins/kicad-pcb"),
            PathBuf::from("/plugins/other"),
        ]
    }

    #[test]
    fn claude_gets_plugin_dirs_for_enabled_plugin_skills() {
        let policy = plugin_skill_launch_policy(
            shared::AgentType::Claude,
            vec!["--some-arg".to_string()],
            skill_roots(),
            Some("plugin skills reminder".to_string()),
        );

        assert_eq!(
            policy.extra_args,
            vec![
                "--some-arg",
                "--plugin-dir",
                "/plugins/kicad-pcb",
                "--plugin-dir",
                "/plugins/other"
            ]
        );
        assert!(policy.reminder.is_none());
    }

    #[test]
    fn non_claude_agents_get_skill_reminders_not_claude_plugin_dirs() {
        for agent_type in [
            shared::AgentType::Codex,
            shared::AgentType::Muse,
            shared::AgentType::Antigravity,
        ] {
            let policy = plugin_skill_launch_policy(
                agent_type,
                vec!["--some-arg".to_string()],
                skill_roots(),
                Some("plugin skills reminder".to_string()),
            );

            assert_eq!(policy.extra_args, vec!["--some-arg"]);
            assert_eq!(policy.reminder.as_deref(), Some("plugin skills reminder"));
        }
    }
}

#[cfg(test)]
mod import_binding_tests {
    use super::*;

    #[test]
    fn binding_is_idempotent_rejects_aliases_and_preserves_other_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.json");
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        update_codex_thread_map_at(&path, first, "thread-a", true).unwrap();
        update_codex_thread_map_at(&path, first, "thread-a", true).unwrap();
        assert!(update_codex_thread_map_at(&path, second, "thread-a", true).is_err());
        assert!(update_codex_thread_map_at(&path, first, "thread-b", true).is_err());
        update_codex_thread_map_at(&path, second, "thread-b", false).unwrap();
        let imports: HashMap<Uuid, String> = serde_json::from_str(
            &std::fs::read_to_string(path.with_file_name("codex_imports.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(imports.get(&first).map(String::as_str), Some("thread-a"));
        assert!(!imports.contains_key(&second));
        let map: HashMap<Uuid, String> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(map.get(&first).unwrap(), "thread-a");
        assert_eq!(map.get(&second).unwrap(), "thread-b");
    }

    #[test]
    fn corrupt_map_is_reported_without_overwriting_recoverable_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("threads.json");
        std::fs::write(&path, "broken JSON").unwrap();
        assert!(update_codex_thread_map_at(&path, Uuid::new_v4(), "thread", true).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "broken JSON");
    }
}
