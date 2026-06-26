use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ws_bridge::WsEndpoint;

use super::types::{
    FileDownloadRequestFields, FileDownloadResponseFields, FileUploadChunkFields,
    FileUploadStartFields, PermissionResponseFields, RegisterFields,
    SessionLimitContinuationFields,
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
}
