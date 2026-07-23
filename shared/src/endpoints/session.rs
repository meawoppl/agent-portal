use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ws_bridge::WsEndpoint;

use super::types::{
    FileDownloadRequestFields, FileDownloadResponseFields, FileUploadChunkFields,
    FileUploadResultFields, FileUploadStartFields, ForwardPortFields, ForwardStatusFields,
    PermissionResponseFields, RegisterFields, SessionLimitContinuationFields, TunnelCloseFields,
    TunnelDataFields, TunnelOpenFields, TunnelRefusedFields, TunnelStreamFields,
    TunnelWindowFields,
};
use crate::{AgentType, PermissionSuggestion, SendMode, SessionStatus, TurnMetrics};

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
    #[serde(rename = "ClaudeOutput")]
    AgentOutput {
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

    /// Delivery-progress signal for a client-correlated input (#939). Emitted
    /// by the proxy at `ProxyReceived` (on `SequencedInput` receipt) and
    /// `AgentAccepted` (once the input is in the agent's stream). The backend
    /// relays it to web clients as `ServerToClient::InputProgress`. Distinct
    /// from `InputAck`, which drives seq dedup + `pending_inputs` deletion.
    InputProgressAck {
        session_id: Uuid,
        client_msg_id: Uuid,
        stage: crate::InputDeliveryStage,
    },

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

    /// Ephemeral tool-progress heartbeat from Claude for a long-running tool
    /// call (`ClaudeOutput::ToolProgress`, emitted every ~30s while the tool
    /// runs). Deliberately a typed side-channel — NOT `SequencedOutput`: it is
    /// never buffered, never persisted (a 20-minute Bash call would otherwise
    /// write ~40 noise rows into `messages` and blow retention), and never
    /// replayed. The backend fans it out to the session's web clients as
    /// `ServerToClient::ToolProgress` to drive a live "running — Xs" status;
    /// if no client is listening it simply evaporates. See the classifier
    /// (`session_lib::adapter::ClaudeAdapter`) and the frontend active-tool
    /// strip for the two ends of this path.
    ToolProgress {
        session_id: Uuid,
        /// The heartbeat's own id (`<tool_use_id>-heartbeat-N`).
        tool_use_id: String,
        /// The running tool's id, when Claude sets it. In production this is
        /// the base tool id (the heartbeat id minus the `-heartbeat-N`
        /// suffix); `None` for the top-level agent in some wire shapes. The
        /// frontend derives its correlation key from this plus `tool_use_id`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_tool_use_id: Option<String>,
        tool_name: String,
        elapsed_time_seconds: f64,
    },

    /// Claude reported a hard session limit with a reset time. The backend
    /// persists a one-shot continuation candidate and asks the user whether
    /// to schedule it.
    SessionLimitReached(SessionLimitContinuationFields),

    /// Response to a backend-requested local file download.
    FileDownloadResponse(FileDownloadResponseFields),

    /// Terminal outcome of a chunked file upload: the file is fully written
    /// and renamed into place, or has definitively failed (#939 phase 4).
    FileUploadResult(FileUploadResultFields),

    /// Reply to `ForwardOpen`: allowlist updated + probe-dial result
    /// (docs/PORT_FORWARDING.md).
    ForwardStatus(ForwardStatusFields),

    /// Tunnel stream successfully dialed on the proxy host.
    TunnelOpened(TunnelStreamFields),

    /// Tunnel stream could not be dialed (connection refused, port not in the
    /// allowlist, stream cap reached, …).
    TunnelRefused(TunnelRefusedFields),

    /// Stream bytes, proxy-host service → browser direction.
    TunnelData(TunnelDataFields),

    /// Flow-control credit grant for a stream's backend→proxy direction.
    TunnelWindow(TunnelWindowFields),

    /// Stream teardown (either side may initiate; no half-close).
    TunnelClose(TunnelCloseFields),
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
        /// Deprecated: no current proxy consumes this. It capped the
        /// Read-triggered inline-image path, which was replaced by the
        /// explicit `agent-portal show` CLI (that path enforces
        /// `PORTAL_MAX_IMAGE_MB` backend-side on the upload route instead).
        ///
        /// The field is retained purely for wire compatibility: older proxies
        /// deserialize `RegisterAck` without `#[serde(default)]` on this field,
        /// so dropping it would make their registration ack fail to decode and
        /// wedge them in a reconnect loop. The backend keeps populating it from
        /// the still-live `max_image_mb` config (which the user-upload paths
        /// continue to enforce).
        max_image_mb: u32,
        /// On failure: whether the proxy should reconnect with backoff
        /// (transient infrastructure error) instead of treating the
        /// rejection as fatal. Defaults to `false` so acks from older
        /// backends keep today's fatal semantics. #1264.
        #[serde(default)]
        retryable: bool,
    },

    /// Keepalive heartbeat
    Heartbeat,

    /// User input (unsequenced fallback)
    #[serde(rename = "ClaudeInput")]
    AgentInput {
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
        /// Browser-assigned correlation id for delivery tracking (#939); the
        /// proxy echoes it on `InputProgressAck`. `None` for legacy/replayed
        /// inputs (those still rely on content reconciliation in the UI).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_msg_id: Option<Uuid>,
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

    /// Add a port to the proxy's forward allowlist (docs/PORT_FORWARDING.md).
    /// The proxy replies with `ForwardStatus`. Replayed for every active
    /// forward after each successful registration.
    ForwardOpen(ForwardPortFields),

    /// Remove a port from the proxy's forward allowlist and drop its live
    /// streams.
    ForwardClose(ForwardPortFields),

    /// Open a tunnel stream to `127.0.0.1:{port}` on the proxy host. The
    /// proxy replies `TunnelOpened` or `TunnelRefused`.
    TunnelOpen(TunnelOpenFields),

    /// Stream bytes, browser → proxy-host service direction.
    TunnelData(TunnelDataFields),

    /// Flow-control credit grant for a stream's proxy→backend direction.
    TunnelWindow(TunnelWindowFields),

    /// Stream teardown (either side may initiate; no half-close).
    TunnelClose(TunnelCloseFields),
}
