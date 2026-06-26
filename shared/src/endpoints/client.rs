use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ws_bridge::WsEndpoint;

use super::types::{
    FileUploadChunkFields, FileUploadStartFields, PermissionResponseFields, RegisterFields,
};
use crate::{
    AgentType, MessageOrigin, PermissionSuggestion, SendMode, SessionCost, SessionStatus,
    TurnMetrics,
};

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
    #[serde(rename = "ClaudeInput")]
    AgentInput {
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
    #[serde(rename = "ClaudeOutput")]
    AgentOutput {
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<MessageOrigin>,
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
