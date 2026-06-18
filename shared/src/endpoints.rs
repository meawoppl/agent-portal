use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub use ws_bridge::WsEndpoint;

use crate::{
    AgentInstall, AgentType, DirectoryEntry, PermissionSuggestion, SendMode, SessionCost,
    SessionStatus, TurnMetrics,
};

// =============================================================================
// Shared field structs — used by both proxy and client endpoints
// =============================================================================

/// Fields for session registration (shared by proxy and web client).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterFields {
    pub session_id: Uuid,
    pub session_name: String,
    pub auth_token: Option<String>,
    pub working_directory: String,
    #[serde(default)]
    pub resuming: bool,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub replay_after: Option<String>,
    #[serde(default)]
    pub client_version: Option<String>,
    #[serde(default)]
    pub replaces_session_id: Option<Uuid>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub launcher_id: Option<Uuid>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default)]
    pub scheduled_task_id: Option<Uuid>,
    #[serde(default)]
    pub claude_args: Vec<String>,
}

/// Core scheduled-task fields shared by `ScheduledTaskConfig`,
/// `CreateScheduledTaskRequest`, and `ScheduledTaskInfo` via `#[serde(flatten)]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduledTaskFields {
    pub name: String,
    pub cron_expression: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    pub working_directory: String,
    pub prompt: String,
    #[serde(default)]
    pub claude_args: Vec<String>,
    #[serde(default)]
    pub agent_type: AgentType,
    #[serde(default = "default_max_runtime_minutes")]
    pub max_runtime_minutes: i32,
}

fn default_timezone() -> String {
    "UTC".to_string()
}

fn default_max_runtime_minutes() -> i32 {
    30
}

/// Configuration for a scheduled task, sent from backend to launcher via ScheduleSync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTaskConfig {
    pub id: Uuid,
    #[serde(flatten)]
    pub fields: ScheduledTaskFields,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuationConfig {
    pub id: Uuid,
    pub session_id: Uuid,
    pub reset_at: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLimitContinuationFields {
    pub session_id: Uuid,
    pub reset_at: String,
    pub source_message: String,
    pub prompt: String,
}

/// Fields for a permission response (shared by server-to-proxy and client-to-server).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponseFields {
    pub request_id: String,
    pub allow: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<PermissionSuggestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Fields for starting a file upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadStartFields {
    pub upload_id: String,
    pub filename: String,
    pub content_type: String,
    pub total_chunks: u32,
    #[serde(default)]
    pub total_size: u64,
}

/// Fields for a single file upload chunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadChunkFields {
    pub upload_id: String,
    pub chunk_index: u32,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDownloadRequestFields {
    pub request_id: Uuid,
    pub path: String,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDownloadResponseFields {
    pub request_id: Uuid,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// =============================================================================
// Session endpoint: proxy <-> backend (/ws/session)
// =============================================================================

pub struct SessionEndpoint;

impl WsEndpoint for SessionEndpoint {
    const PATH: &'static str = "/ws/session";
    type ServerMsg = ServerToProxy;
    type ClientMsg = ProxyToServer;
}

/// Messages the proxy sends to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProxyToServer {
    /// Register a new session or connect to an existing one
    Register(RegisterFields),

    /// Raw output from Claude Code (unsequenced fallback)
    ClaudeOutput {
        content: serde_json::Value,
        /// Which agent's wire format the `content` JSON came from.
        ///
        /// Tagged at the proxy emission boundary so the backend can attribute
        /// each message correctly without needing to remember the
        /// registration-time agent for the whole connection. Critical for any
        /// future multi-agent / sub-agent session shape, where a single
        /// `/ws/session` connection might ferry messages from more than one
        /// agent type.
        ///
        /// `#[serde(default)]` is a backwards-compat hack for pre-2.5.42
        /// proxies that don't send this field — they get
        /// `AgentType::default()` (currently `Claude`) and any in-flight
        /// codex messages from such a proxy will be presumed misattributed
        /// until the proxy upgrades. New code MUST always set this field.
        #[serde(default)]
        agent_type: AgentType,
    },

    /// Sequenced output from Claude Code
    SequencedOutput {
        seq: u64,
        content: serde_json::Value,
        /// Which agent's wire format the `content` JSON came from.
        ///
        /// Tagged at the proxy emission boundary so the backend can attribute
        /// each message correctly without needing to remember the
        /// registration-time agent for the whole connection. Critical for any
        /// future multi-agent / sub-agent session shape, where a single
        /// `/ws/session` connection might ferry messages from more than one
        /// agent type.
        ///
        /// `#[serde(default)]` is a backwards-compat hack for pre-2.5.42
        /// proxies that don't send this field — they get
        /// `AgentType::default()` (currently `Claude`) and any in-flight
        /// codex messages from such a proxy will be presumed misattributed
        /// until the proxy upgrades. New code MUST always set this field.
        #[serde(default)]
        agent_type: AgentType,
    },

    /// Keepalive heartbeat
    Heartbeat,

    /// Permission request from Claude (tool wants to execute)
    PermissionRequest {
        request_id: String,
        tool_name: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        permission_suggestions: Vec<PermissionSuggestion>,
    },

    /// Update session metadata (e.g., git branch changed)
    SessionUpdate {
        session_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        git_branch: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pr_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        repo_url: Option<String>,
        /// All open PRs in the repo, sorted by number.
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        open_prs: Vec<crate::PrRef>,
    },

    /// Acknowledge receipt of input messages
    InputAck { session_id: Uuid, ack_seq: i64 },

    /// Session status update
    SessionStatus { status: SessionStatus },

    /// Per-turn performance metrics report (PR 1 of N).
    ///
    /// Emitted once per completed turn by the proxy. The backend persists
    /// the row in `turn_metrics` and broadcasts it to web clients as
    /// `ServerToClient::TurnMetrics`. See `shared::TurnMetrics` for field
    /// semantics.
    ///
    /// Boxed because `TurnMetrics` is materially larger than the rest of
    /// the enum's variants; keeps the discriminant small for the
    /// hot-path frames that fly orders of magnitude more often.
    TurnMetricsReport(Box<TurnMetrics>),

    /// Claude reported a hard session limit with a reset time. The backend
    /// persists a one-shot continuation candidate and asks the user whether
    /// to schedule it.
    SessionLimitReached(SessionLimitContinuationFields),

    /// Response to a backend-requested local file download.
    FileDownloadResponse(FileDownloadResponseFields),
}

/// Messages the backend sends to the proxy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerToProxy {
    /// Acknowledge session registration
    RegisterAck {
        success: bool,
        session_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        max_image_mb: u32,
    },

    /// Keepalive heartbeat
    Heartbeat,

    /// User input (unsequenced fallback)
    ClaudeInput {
        content: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        send_mode: Option<SendMode>,
    },

    /// Sequenced user input
    SequencedInput {
        session_id: Uuid,
        seq: i64,
        content: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        send_mode: Option<SendMode>,
    },

    /// User's permission decision
    PermissionResponse(PermissionResponseFields),

    /// Acknowledge receipt of output messages
    OutputAck { session_id: Uuid, ack_seq: u64 },

    /// Start a chunked file upload to the proxy's working directory
    FileUploadStart(FileUploadStartFields),

    /// A single chunk of a file upload (base64-encoded, ~1KB decoded)
    FileUploadChunk(FileUploadChunkFields),

    /// Server is shutting down (proxy should reconnect after delay)
    ServerShutdown {
        reason: String,
        reconnect_delay_ms: u64,
    },

    /// Session has been terminated (proxy should NOT reconnect)
    SessionTerminated { reason: String },

    /// Interrupt the current Claude response
    Interrupt,

    /// Request that the proxy read a file relative to the session working directory.
    FileDownloadRequest(FileDownloadRequestFields),
}

// =============================================================================
// Client endpoint: frontend <-> backend (/ws/client)
// =============================================================================

pub struct ClientEndpoint;

impl WsEndpoint for ClientEndpoint {
    const PATH: &'static str = "/ws/client";
    type ServerMsg = ServerToClient;
    type ClientMsg = ClientToServer;
}

/// Messages the frontend sends to the backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ClientToServer {
    /// Register to receive updates for a session
    Register(RegisterFields),

    /// User sends input to Claude
    ClaudeInput {
        content: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        send_mode: Option<SendMode>,
    },

    /// User's permission decision
    PermissionResponse(PermissionResponseFields),

    /// Start a chunked file upload
    FileUploadStart(FileUploadStartFields),

    /// A single chunk of a file upload
    FileUploadChunk(FileUploadChunkFields),

    /// Interrupt the current Claude response
    Interrupt,

    /// Schedule a one-shot continuation that was detected by the proxy after
    /// Claude reported a hard session limit.
    ScheduleLimitContinuation { continuation_id: Uuid },
}

/// Messages the backend sends to the frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ServerToClient {
    /// Output from Claude Code
    ClaudeOutput {
        content: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        sender_user_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        sender_name: Option<String>,
        /// Which agent's wire format the `content` JSON came from.
        ///
        /// Mirrors the per-message `agent_type` tag on the proxy → backend
        /// `ProxyToServer::ClaudeOutput` / `SequencedOutput` so the live
        /// broadcast can be tagged the same way as the historical-read path
        /// (`MessageWithSender` → `Message.agent_type`). Without it the
        /// frontend can only learn an in-flight message's agent_type by
        /// reloading history. `#[serde(default)]` keeps older clients
        /// (and any hand-rolled JSON) parseable; new code MUST set it.
        #[serde(default)]
        agent_type: AgentType,
        /// Server-assigned `created_at` of the persisted `messages` row, in
        /// the ISO-8601 microsecond format the backend's `replay_history`
        /// parser accepts (`%Y-%m-%dT%H:%M:%S%.f`). Sourced from the DB row
        /// (via `RETURNING created_at` on the insert) so the frontend can
        /// remember the **server's** watermark for reconnect replay rather
        /// than `Date.now()` (closes #784 — silent data-loss on reconnect
        /// when client/server clocks were skewed). `Option<_>` + `#[serde(
        /// default, skip_serializing_if = "Option::is_none")]` for the rare
        /// broadcast paths that don't go through a DB insert (e.g. error
        /// envelopes injected from in-memory state) and for wire compat
        /// with older backends.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        created_at: Option<String>,
    },

    /// Batch of historical messages for replay.
    ///
    /// Each entry in `messages` is the raw stored content JSON with `_sender`
    /// (for user-role messages) and `_created_at` (the server-assigned row
    /// timestamp) injected by the backend's `replay_history`. The separate
    /// `last_created_at` is the timestamp of the latest message in this batch
    /// (or `None` for an empty batch); the frontend uses it directly as its
    /// reconnect-replay watermark without having to re-parse `_created_at`
    /// out of the last entry. Both default to absent on older backends.
    HistoryBatch {
        messages: Vec<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_created_at: Option<String>,
    },

    /// Permission request from Claude (tool wants to execute)
    PermissionRequest {
        request_id: String,
        tool_name: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        permission_suggestions: Vec<PermissionSuggestion>,
    },

    /// Error message
    Error { message: String },

    /// Session metadata changed
    SessionUpdate {
        session_id: Uuid,
        #[serde(skip_serializing_if = "Option::is_none")]
        git_branch: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        pr_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        repo_url: Option<String>,
        /// All open PRs in the repo, sorted by number.
        #[serde(skip_serializing_if = "Vec::is_empty", default)]
        open_prs: Vec<crate::PrRef>,
    },

    /// User spend data update
    UserSpendUpdate {
        total_spend_usd: f64,
        session_costs: Vec<SessionCost>,
    },

    /// Server is shutting down
    ServerShutdown {
        reason: String,
        reconnect_delay_ms: u64,
    },

    /// Session status changed
    SessionStatus { status: SessionStatus },

    /// Status update for a one-shot hard-limit continuation action card.
    ContinuationStatus {
        continuation_id: Uuid,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },

    /// Launch session result (forwarded from launcher)
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

    /// A proxy process exited (forwarded from launcher)
    SessionExited {
        session_id: Uuid,
        exit_code: Option<i32>,
    },

    /// Per-turn performance metrics row saved by the backend (PR 1 of N).
    ///
    /// Broadcast to all web clients on the matching session immediately
    /// after the row is inserted, so live dashboards can update without a
    /// page refresh. Frontend rendering ships in a follow-up PR; current
    /// frontends silently ignore the frame.
    ///
    /// Boxed to keep the rest of the enum compact — see
    /// `ProxyToServer::TurnMetricsReport` for the same rationale.
    TurnMetrics(Box<TurnMetrics>),
}

// =============================================================================
// Launcher endpoint: launcher <-> backend (/ws/launcher)
// =============================================================================

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
        /// If true the launcher must not retry — it should exit immediately
        #[serde(default)]
        fatal: bool,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_endpoint_path() {
        assert_eq!(SessionEndpoint::PATH, "/ws/session");
    }

    #[test]
    fn session_exited_reason_defaults_for_old_launchers() {
        // An older launcher omits `reason`; it must deserialize as the neutral
        // default rather than failing, so a mixed-version fleet keeps working.
        let json = r#"{"type":"SessionExited","session_id":"00000000-0000-0000-0000-000000000000","exit_code":0}"#;
        let msg: LauncherToServer = serde_json::from_str(json).unwrap();
        match msg {
            LauncherToServer::SessionExited { reason, .. } => {
                assert_eq!(reason, SessionExitReason::Completed);
            }
            _ => panic!("expected SessionExited"),
        }
    }

    #[test]
    fn session_exit_reason_roundtrips() {
        let json = serde_json::to_string(&SessionExitReason::CrashedEarly).unwrap();
        assert_eq!(json, "\"crashed_early\"");
        let back: SessionExitReason = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SessionExitReason::CrashedEarly);
    }

    #[test]
    fn client_endpoint_path() {
        assert_eq!(ClientEndpoint::PATH, "/ws/client");
    }

    #[test]
    fn launcher_endpoint_path() {
        assert_eq!(LauncherEndpoint::PATH, "/ws/launcher");
    }

    #[test]
    fn proxy_to_server_register_roundtrip() {
        let msg = ProxyToServer::Register(RegisterFields {
            session_id: Uuid::nil(),
            session_name: "test".into(),
            auth_token: None,
            working_directory: "/tmp".into(),
            resuming: false,
            git_branch: None,
            replay_after: None,
            client_version: None,
            replaces_session_id: None,
            hostname: None,
            launcher_id: None,
            agent_type: AgentType::Claude,
            repo_url: None,
            scheduled_task_id: None,
            claude_args: Vec::new(),
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"Register""#));
        let parsed: ProxyToServer = serde_json::from_str(&json).unwrap();
        match parsed {
            ProxyToServer::Register(reg) => {
                assert_eq!(reg.session_name, "test");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn server_to_proxy_sequenced_input_roundtrip() {
        let msg = ServerToProxy::SequencedInput {
            session_id: Uuid::nil(),
            seq: 5,
            content: serde_json::json!({"text": "hello"}),
            send_mode: Some(SendMode::Wiggum),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"SequencedInput""#));
        let parsed: ServerToProxy = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToProxy::SequencedInput { seq, send_mode, .. } => {
                assert_eq!(seq, 5);
                assert_eq!(send_mode, Some(SendMode::Wiggum));
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn client_to_server_claude_input_roundtrip() {
        let msg = ClientToServer::ClaudeInput {
            content: serde_json::json!({"text": "hi"}),
            send_mode: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ClaudeInput""#));
        let parsed: ClientToServer = serde_json::from_str(&json).unwrap();
        match parsed {
            ClientToServer::ClaudeInput { send_mode, .. } => {
                assert!(send_mode.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn server_to_client_output_roundtrip() {
        let msg = ServerToClient::ClaudeOutput {
            content: serde_json::json!({"type": "assistant", "text": "hello"}),
            sender_user_id: None,
            sender_name: None,
            agent_type: AgentType::Codex,
            created_at: Some("2026-05-18T12:34:56.789012".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerToClient = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToClient::ClaudeOutput {
                content,
                agent_type,
                created_at,
                ..
            } => {
                assert_eq!(content["text"], "hello");
                assert_eq!(agent_type, AgentType::Codex);
                assert_eq!(created_at.as_deref(), Some("2026-05-18T12:34:56.789012"));
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Pre-#784 wire JSON for `ServerToClient::ClaudeOutput` (no `created_at`)
    /// must still parse. The frontend's reconnect watermark just stays at the
    /// last value it had (or `None` if it never received a timestamped
    /// message) — same backward-compat slack we extend for `agent_type`.
    #[test]
    fn wire_compat_pre_784_omits_created_at() {
        let json = r#"{"type":"ClaudeOutput","content":{"hello":"world"}}"#;
        let parsed: ServerToClient = serde_json::from_str(json).unwrap();
        match parsed {
            ServerToClient::ClaudeOutput { created_at, .. } => {
                assert!(created_at.is_none());
            }
            _ => panic!("Wrong variant"),
        }

        // Pre-#784 HistoryBatch without `last_created_at` must also parse.
        let json = r#"{"type":"HistoryBatch","messages":[]}"#;
        let parsed: ServerToClient = serde_json::from_str(json).unwrap();
        match parsed {
            ServerToClient::HistoryBatch {
                messages,
                last_created_at,
            } => {
                assert!(messages.is_empty());
                assert!(last_created_at.is_none());
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// New-shape `HistoryBatch` carries the latest server-assigned timestamp
    /// alongside the messages so the frontend can update the reconnect
    /// watermark without re-parsing `_created_at` out of the last entry
    /// (closes #784).
    #[test]
    fn history_batch_roundtrip_with_last_created_at() {
        let msg = ServerToClient::HistoryBatch {
            messages: vec![serde_json::json!({"_created_at": "2026-05-18T00:00:00.000000"})],
            last_created_at: Some("2026-05-18T00:00:00.000000".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let parsed: ServerToClient = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToClient::HistoryBatch {
                messages,
                last_created_at,
            } => {
                assert_eq!(messages.len(), 1);
                assert_eq!(
                    last_created_at.as_deref(),
                    Some("2026-05-18T00:00:00.000000")
                );
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Pre-2.5.42 wire JSON for `SequencedOutput` (no `agent_type`) must still
    /// parse, defaulting to `AgentType::Claude`. Same for `ClaudeOutput` and
    /// the backend → frontend `ServerToClient::ClaudeOutput` shape.
    #[test]
    fn wire_compat_pre_2_5_42_omits_agent_type() {
        let json = r#"{"type":"SequencedOutput","seq":3,"content":{"hello":"world"}}"#;
        let parsed: ProxyToServer = serde_json::from_str(json).unwrap();
        match parsed {
            ProxyToServer::SequencedOutput { agent_type, .. } => {
                assert_eq!(agent_type, AgentType::Claude);
            }
            _ => panic!("Wrong variant"),
        }

        let json = r#"{"type":"ClaudeOutput","content":{"hello":"world"}}"#;
        let parsed: ProxyToServer = serde_json::from_str(json).unwrap();
        match parsed {
            ProxyToServer::ClaudeOutput { agent_type, .. } => {
                assert_eq!(agent_type, AgentType::Claude);
            }
            _ => panic!("Wrong variant"),
        }

        let json = r#"{"type":"ClaudeOutput","content":{"hello":"world"}}"#;
        let parsed: ServerToClient = serde_json::from_str(json).unwrap();
        match parsed {
            ServerToClient::ClaudeOutput { agent_type, .. } => {
                assert_eq!(agent_type, AgentType::Claude);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn launcher_to_server_register_roundtrip() {
        let msg = LauncherToServer::LauncherRegister {
            launcher_id: Uuid::nil(),
            launcher_name: "test-launcher".into(),
            auth_token: Some("tok".into()),
            hostname: "host1".into(),
            version: Some("1.0".into()),
            working_directory: Some("/home/user/project".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"LauncherRegister""#));
        let parsed: LauncherToServer = serde_json::from_str(&json).unwrap();
        match parsed {
            LauncherToServer::LauncherRegister { launcher_name, .. } => {
                assert_eq!(launcher_name, "test-launcher");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn server_to_launcher_launch_roundtrip() {
        let msg = ServerToLauncher::LaunchSession {
            request_id: Uuid::nil(),
            user_id: Uuid::nil(),
            auth_token: "token".into(),
            working_directory: "/home".into(),
            session_name: Some("my-session".into()),
            claude_args: vec!["--verbose".into()],
            agent_type: AgentType::Claude,
            scheduled_task_id: None,
            resume_session_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"LaunchSession""#));
        let parsed: ServerToLauncher = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToLauncher::LaunchSession {
                working_directory,
                claude_args,
                ..
            } => {
                assert_eq!(working_directory, "/home");
                assert_eq!(claude_args, vec!["--verbose"]);
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Verify wire-format compatibility of per-endpoint types.
    #[test]
    fn wire_compat_register() {
        // Register JSON format
        let json = r#"{
            "type": "Register",
            "session_id": "550e8400-e29b-41d4-a716-446655440000",
            "session_name": "test",
            "auth_token": null,
            "working_directory": "/tmp"
        }"#;
        // Must parse as both ProxyToServer and ClientToServer
        let _: ProxyToServer = serde_json::from_str(json).unwrap();
        let _: ClientToServer = serde_json::from_str(json).unwrap();
    }

    #[test]
    fn launcher_request_launch_roundtrip() {
        let msg = LauncherToServer::RequestLaunch {
            request_id: Uuid::nil(),
            working_directory: "/home/user/project".into(),
            session_name: Some("my-project".into()),
            claude_args: vec!["--verbose".into()],
            agent_type: AgentType::Claude,
            scheduled_task_id: None,
            last_session_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"RequestLaunch""#));
        let parsed: LauncherToServer = serde_json::from_str(&json).unwrap();
        match parsed {
            LauncherToServer::RequestLaunch {
                working_directory,
                session_name,
                claude_args,
                ..
            } => {
                assert_eq!(working_directory, "/home/user/project");
                assert_eq!(session_name.as_deref(), Some("my-project"));
                assert_eq!(claude_args, vec!["--verbose"]);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn wire_compat_server_shutdown() {
        let json = r#"{"type":"ServerShutdown","reason":"update","reconnect_delay_ms":5000}"#;
        // Must parse in all three server->X enums
        let _: ServerToProxy = serde_json::from_str(json).unwrap();
        let _: ServerToClient = serde_json::from_str(json).unwrap();
        let _: ServerToLauncher = serde_json::from_str(json).unwrap();
    }

    #[test]
    fn wire_compat_session_terminated() {
        let json = r#"{"type":"SessionTerminated","reason":"Session stopped by user"}"#;
        let msg: ServerToProxy = serde_json::from_str(json).unwrap();
        match msg {
            ServerToProxy::SessionTerminated { reason } => {
                assert_eq!(reason, "Session stopped by user");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn schedule_sync_roundtrip() {
        let msg = ServerToLauncher::ScheduleSync {
            tasks: vec![ScheduledTaskConfig {
                id: Uuid::nil(),
                fields: ScheduledTaskFields {
                    name: "nightly audit".into(),
                    cron_expression: "0 3 * * *".into(),
                    timezone: "UTC".into(),
                    working_directory: "/home/user/project".into(),
                    prompt: "Check deps".into(),
                    claude_args: vec![],
                    agent_type: AgentType::Claude,
                    max_runtime_minutes: 30,
                },
                enabled: true,
                last_session_id: None,
            }],
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ScheduleSync""#));
        let parsed: ServerToLauncher = serde_json::from_str(&json).unwrap();
        match parsed {
            ServerToLauncher::ScheduleSync { tasks } => {
                assert_eq!(tasks.len(), 1);
                assert_eq!(tasks[0].fields.name, "nightly audit");
            }
            _ => panic!("Wrong variant"),
        }
    }

    /// Pins the ScheduledTaskConfig wire shape: flattened fields must produce
    /// the same keys/values as the pre-flatten struct, and JSON in the old
    /// field order (as emitted by older backends) must still deserialize.
    #[test]
    fn scheduled_task_config_wire_compat() {
        let config = ScheduledTaskConfig {
            id: Uuid::nil(),
            fields: ScheduledTaskFields {
                name: "nightly audit".into(),
                cron_expression: "0 3 * * *".into(),
                timezone: "UTC".into(),
                working_directory: "/home/user/project".into(),
                prompt: "Check deps".into(),
                claude_args: vec!["--verbose".into()],
                agent_type: AgentType::Claude,
                max_runtime_minutes: 30,
            },
            enabled: true,
            last_session_id: None,
        };
        let expected: serde_json::Value = serde_json::from_str(
            r#"{
                "id": "00000000-0000-0000-0000-000000000000",
                "name": "nightly audit",
                "cron_expression": "0 3 * * *",
                "timezone": "UTC",
                "working_directory": "/home/user/project",
                "prompt": "Check deps",
                "claude_args": ["--verbose"],
                "agent_type": "claude",
                "enabled": true,
                "max_runtime_minutes": 30
            }"#,
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&config).unwrap(), expected);

        // Old wire order (enabled before max_runtime_minutes) still parses.
        let old_wire = r#"{
            "id": "00000000-0000-0000-0000-000000000000",
            "name": "nightly audit",
            "cron_expression": "0 3 * * *",
            "timezone": "UTC",
            "working_directory": "/home/user/project",
            "prompt": "Check deps",
            "claude_args": ["--verbose"],
            "agent_type": "claude",
            "enabled": true,
            "max_runtime_minutes": 30,
            "last_session_id": "11111111-1111-1111-1111-111111111111"
        }"#;
        let parsed: ScheduledTaskConfig = serde_json::from_str(old_wire).unwrap();
        assert_eq!(parsed.fields.name, "nightly audit");
        assert_eq!(parsed.fields.max_runtime_minutes, 30);
        assert!(parsed.enabled);
        assert_eq!(
            parsed.last_session_id,
            Some(Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap())
        );
    }

    #[test]
    fn inject_input_roundtrip() {
        let msg = LauncherToServer::InjectInput {
            session_id: Uuid::nil(),
            content: "Check for updates".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"InjectInput""#));
        let _: LauncherToServer = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn scheduled_run_started_roundtrip() {
        let msg = LauncherToServer::ScheduledRunStarted {
            task_id: Uuid::nil(),
            session_id: Uuid::nil(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ScheduledRunStarted""#));
        let _: LauncherToServer = serde_json::from_str(&json).unwrap();
    }

    #[test]
    fn scheduled_run_completed_roundtrip() {
        let msg = LauncherToServer::ScheduledRunCompleted {
            task_id: Uuid::nil(),
            session_id: Uuid::nil(),
            exit_code: Some(0),
            duration_secs: 120,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"ScheduledRunCompleted""#));
        let _: LauncherToServer = serde_json::from_str(&json).unwrap();
    }
}
