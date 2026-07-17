use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ws_bridge::WsEndpoint;

use super::types::{ContinuationConfig, ScheduledTaskConfig};
use crate::{AgentInstall, AgentType, DirectoryEntry};

pub struct LauncherEndpoint;

impl WsEndpoint for LauncherEndpoint {
    const PATH: &'static str = "/ws/launcher";
    type ServerMsg = ServerToLauncher;
    type ClientMsg = LauncherToServer;
}

/// Why a proxy session task ended, as classified by the launcher. Lets the
/// backend distinguish a healthy run that finished from an instant crash loop
/// (which it throttles with launch backoff) without guessing from the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionExitReason {
    /// Ran for a healthy duration then exited. Resets launch backoff.
    #[default]
    Completed,
    /// Registered but exited almost immediately — a likely crash loop. The
    /// backend bumps `launch_failure_count` so reconcile backs off.
    CrashedEarly,
    /// A `--resume` whose local conversation was gone; rotated to a fresh
    /// session. Also throttled, since it tends to recur until cleaned up.
    ResumeTargetMissing,
    /// Backend rejected registration (revoked/expired token). Relaunched with a
    /// fresh token; not a failure to back off.
    RegistrationRejected,
    /// Stopped by the user / cancellation. Not a failure.
    Stopped,
    /// Errored (spawn or runtime). Neutral for backoff.
    Error,
}

/// Messages the launcher sends to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LauncherToServer {
    /// Register a launcher daemon
    LauncherRegister {
        launcher_id: Uuid,
        launcher_name: String,
        auth_token: Option<String>,
        hostname: String,
        #[serde(default)]
        version: Option<String>,
        /// Working directory where the launcher process is running
        #[serde(default)]
        working_directory: Option<String>,
        /// Additive feature flags supported by this launcher binary.
        #[serde(default)]
        capabilities: Vec<String>,
    },

    /// Result of a launch request
    LaunchSessionResult {
        request_id: Uuid,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<Uuid>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Launcher heartbeat with running process summary
    LauncherHeartbeat {
        launcher_id: Uuid,
        running_sessions: Vec<Uuid>,
        uptime_secs: u64,
    },

    /// Log output from a proxy process
    ProxyLog {
        session_id: Uuid,
        level: String,
        message: String,
        timestamp: String,
    },

    /// A proxy process exited
    SessionExited {
        session_id: Uuid,
        exit_code: Option<i32>,
        /// Why the session ended, so the backend can throttle crash loops.
        /// `#[serde(default)]` keeps older launchers (no `reason`) deserializing
        /// as `Completed` (the neutral default).
        #[serde(default)]
        reason: SessionExitReason,
    },

    /// Directory listing response
    ListDirectoriesResult {
        request_id: Uuid,
        #[serde(default)]
        entries: Vec<DirectoryEntry>,
        error: Option<String>,
        #[serde(default)]
        resolved_path: Option<String>,
    },

    /// On-demand agent install probe result. The launcher re-runs the
    /// `which` + `--version` scan and sends back the fresh capabilities.
    ProbeAgentsResult {
        request_id: Uuid,
        #[serde(default)]
        agents: Vec<AgentInstall>,
    },

    /// Request the backend to mint a token and launch a session
    RequestLaunch {
        request_id: Uuid,
        working_directory: String,
        #[serde(default)]
        session_name: Option<String>,
        #[serde(default)]
        claude_args: Vec<String>,
        #[serde(default)]
        agent_type: AgentType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduled_task_id: Option<Uuid>,
        /// Existing session this launch would resume, if this is a restart of
        /// a launcher-persisted session. The backend uses this to suppress
        /// auto-resume when the session is paused server-side.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_session_id: Option<Uuid>,
        /// One-shot limit continuation this launch should satisfy. Only set
        /// when the launcher is relaunching an exited session to deliver a
        /// scheduled continuation prompt.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continuation_id: Option<Uuid>,
    },

    /// Inject input into a session on behalf of the scheduler
    InjectInput { session_id: Uuid, content: String },

    /// A one-shot hard-limit continuation was injected into the still-running
    /// local session.
    ContinuationFired {
        continuation_id: Uuid,
        session_id: Uuid,
    },

    /// A one-shot hard-limit continuation reached its reset time, but the
    /// original local session process was no longer running.
    ContinuationDropped {
        continuation_id: Uuid,
        session_id: Uuid,
        reason: String,
    },

    /// Report that a scheduled task run has started
    ScheduledRunStarted { task_id: Uuid, session_id: Uuid },

    /// Report that a scheduled task run has completed
    ScheduledRunCompleted {
        task_id: Uuid,
        session_id: Uuid,
        exit_code: Option<i32>,
        duration_secs: u64,
    },

    /// Acknowledge that a live launcher persisted a refreshed auth token.
    ///
    /// Additive for #1237 part 2. The backend keeps the previous token valid
    /// until this ack, then shortens it to a small reconnect-race grace window.
    TokenRefreshAck,
}

/// Why a launcher registration was rejected with `fatal: true`. Machine-
/// readable so the launcher can pick a recovery strategy without string-
/// matching `error` (issue #1237): auth failures park with an
/// `agent-portal login` hint and a slow self-healing retry; the rest park
/// with their own message. Additive — old backends omit it (launcher falls
/// back to a string heuristic), old launchers ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LauncherRejectReason {
    /// Missing, invalid, expired, or revoked auth token.
    AuthFailed,
    /// The user is at their launcher limit.
    TooManyLaunchers,
    /// Another launcher with the same (user, hostname) is already connected.
    DuplicateLauncher,
}

/// Messages the backend sends to the launcher.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerToLauncher {
    /// Acknowledge launcher registration
    LauncherRegisterAck {
        success: bool,
        launcher_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// If true the launcher must not treat this as a transient failure.
        /// (Launchers ≥ 2.13.10 park with a slow retry instead of exiting —
        /// exiting under systemd `Restart=on-failure` was a 5s crash loop,
        /// issue #1237.)
        #[serde(default)]
        fatal: bool,
        /// Machine-readable rejection category when `fatal` is set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reject_reason: Option<LauncherRejectReason>,
    },

    /// Request to launch a new proxy instance
    LaunchSession {
        request_id: Uuid,
        user_id: Uuid,
        auth_token: String,
        working_directory: String,
        #[serde(default)]
        session_name: Option<String>,
        #[serde(default)]
        claude_args: Vec<String>,
        #[serde(default)]
        agent_type: AgentType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scheduled_task_id: Option<Uuid>,
        /// Specific existing session to resume. When omitted, the launcher may
        /// use its local expected-session mapping for backwards compatibility.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resume_session_id: Option<Uuid>,
        /// When true, the launcher creates a git worktree from the repository
        /// that contains `working_directory` and runs the session there.
        /// Additive/opt-in: older launchers ignore it via `#[serde(default)]`.
        #[serde(default)]
        create_worktree: bool,
        /// Optional branch name for the worktree. When omitted, the launcher
        /// derives a timestamped default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree_branch: Option<String>,
    },

    /// Request to stop a session and remove the launcher's persisted
    /// expected-session metadata.
    StopSession {
        session_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_directory: Option<String>,
    },

    /// Pause a running session without deleting the launcher's persisted
    /// expected-session metadata.
    PauseSession { session_id: Uuid },

    /// Request directory listing
    ListDirectories { request_id: Uuid, path: String },

    /// Ask the launcher to (re-)probe its agent CLIs. The launcher replies
    /// with `LauncherToServer::ProbeAgentsResult` carrying the fresh state.
    ProbeAgents { request_id: Uuid },

    /// Server is shutting down
    ServerShutdown {
        reason: String,
        reconnect_delay_ms: u64,
    },

    /// Sync scheduled task definitions to the launcher
    ScheduleSync { tasks: Vec<ScheduledTaskConfig> },

    /// Sync one-shot hard-limit continuations to the launcher.
    ContinuationSync {
        continuations: Vec<ContinuationConfig>,
    },

    /// Tell the launcher to pull the latest release, install it, and restart
    /// the agent-portal service. Triggered from the dashboard Launchers panel.
    UpdateAndRestart,

    /// Tell the launcher to restart the agent-portal service *without* updating
    /// the binary — the same graceful restart as `UpdateAndRestart` minus the
    /// download/replace. Gated behind `LAUNCHER_CAPABILITY_RESTART` so the
    /// backend never sends this frame to a launcher too old to decode it.
    Restart,

    /// Ask a live launcher to persist a freshly rotated auth token.
    ///
    /// New launchers reply with `LauncherToServer::TokenRefreshAck` after the
    /// token is written to `launcher.json`. Older launchers may log a decode
    /// error and ignore this unknown frame; the old token remains valid.
    TokenRefresh { auth_token: String },
}
