use serde::{Deserialize, Serialize};
use uuid::Uuid;
use ws_bridge::WsEndpoint;

use super::types::{
    FileUploadChunkFields, FileUploadStartFields, PermissionResponseFields, RegisterFields,
};
use crate::{AgentType, PermissionSuggestion, SendMode, SessionCost, SessionStatus, TurnMetrics};

/// Stages a user input passes through end to end, named by **transport fact**
/// (not implied model semantics) so the UI never claims "the model has it"
/// before it does. The frontend may relabel for display (e.g. `ProxyReceived`
/// → "At local proxy", `AgentAccepted` → "In agent stream"). Carried on
/// [`ServerToClient::InputProgress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputDeliveryStage {
    /// Backend accepted the WebSocket input frame.
    ServerReceived,
    /// Proxy/launcher received the sequenced input from the backend.
    ProxyReceived,
    /// Delivered into the agent's stream (stdin write for Claude / turn start
    /// for Codex). The optimistic "pending" row clears at this stage.
    AgentAccepted,
    /// Delivery failed; `InputProgress.message` carries the reason.
    Failed,
}

/// Who produced a message. A single sum type — exactly one of human / agent /
/// portal — rather than independent `sender` + `origin` optionals that could
/// (illegally) both be set or disagree. `None` on [`PortalMeta`] means "this
/// session's own agent output" (the common assistant-message case), which
/// needs no attribution chip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageSource {
    /// A human user via the web client. Drives the per-message sender label on
    /// multi-member sessions.
    Human { account_id: Uuid, name: String },
    /// Another agent session (inter-agent message) — drives the
    /// "Message from Codex (…)" card.
    Agent {
        session_id: Uuid,
        agent_type: String,
    },
    /// The portal itself: continuation prompts, reminders, system notices.
    Portal,
}

/// Portal-side presentation/provenance metadata for a rendered message.
///
/// Carried as a typed **sibling** of the raw agent `content`, never merged into
/// it. This is the structured replacement for the historical `_origin` /
/// `_sender` / `_created_at` / `_delivery_*` keys that were flattened into the
/// agent JSON blob on every delivery path and read back by string key — a
/// convention with no compile-time enforcement that silently dropped
/// attribution when any one injection site drifted (see
/// `docs/PORTAL_META_SIDECAR.md`). The `_`-key injection has been fully removed;
/// every field is optional + `serde(default)` for wire-compat across versions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalMeta {
    /// Server-assigned persisted-row timestamp (ISO-8601 µs); the frontend's
    /// reconnect-replay watermark. `Option` because optimistic (pre-persist)
    /// rows and non-DB broadcast paths have no server timestamp yet — a
    /// client-clock sentinel would reintroduce the #784 skew bug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// Who produced the message. `Option` (= this session's own agent output)
    /// rather than per-source flags — the sum type makes "human XOR agent XOR
    /// portal" structural. Folds the old `sender` + `origin` into one field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<MessageSource>,
    /// Optimistic-send delivery tracking. `Option` because only browser-sent,
    /// tracked inputs have it; once present the id + stage are guaranteed (see
    /// [`DeliveryMeta`]). Frontend-owned — the backend leaves this `None` (it
    /// does not persist per-message delivery state).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryMeta>,
}

/// Frontend-owned optimistic-send delivery state for a tracked input. Grouped
/// so the invariant "a tracked send always has a `client_msg_id`" is
/// structural rather than four independently-`Option` fields that could drift
/// into nonsensical combinations (a stage with no id, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryMeta {
    /// Browser-assigned id correlating this row with its delivery stages.
    pub client_msg_id: Uuid,
    /// Latest stage reached. `None` ⇒ submitted locally, awaiting the first
    /// server ack (`ServerReceived`). Non-`None` once stages start arriving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<InputDeliveryStage>,
    /// Failure detail (only when `stage == Some(Failed)`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl DeliveryMeta {
    /// Whether the optimistic row should still render as "pending" — derived
    /// from the stage rather than stored, so it can never disagree with it.
    pub fn pending(&self) -> bool {
        !matches!(
            self.stage,
            Some(InputDeliveryStage::AgentAccepted) | Some(InputDeliveryStage::Failed)
        )
    }
}

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

    /// User sends input to the agent.
    #[serde(rename = "ClaudeInput")]
    AgentInput {
        content: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        send_mode: Option<SendMode>,
        /// Browser-assigned correlation id for delivery tracking. The backend
        /// echoes it on every `ServerToClient::InputProgress` so the frontend
        /// can advance the matching optimistic row through its delivery stages.
        /// `None` from older clients (and the legacy content-reconciliation
        /// path still covers those).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_msg_id: Option<Uuid>,
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
    /// Output from Claude Code.
    #[serde(rename = "ClaudeOutput")]
    AgentOutput {
        /// Raw agent JSON, untouched — all portal metadata rides in `meta`.
        content: serde_json::Value,
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
        /// Typed portal sidecar: attribution (`source`), server `created_at`
        /// (the reconnect-replay watermark, closes #784), and frontend-owned
        /// delivery state. Replaces the former flat `sender_*` / `created_at` /
        /// `origin` fields and the `_`-key injection (see
        /// `docs/PORTAL_META_SIDECAR.md`). `Option`/`default` keeps the rare
        /// no-DB broadcast paths and old-wire frames parseable.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<PortalMeta>,
    },

    /// Batch of historical messages for replay.
    ///
    /// Each entry in `messages` is the **raw** stored content JSON (no portal
    /// metadata mixed in). All attribution/timestamp rides in the index-aligned
    /// `message_meta`. `last_created_at` is the timestamp of the latest message
    /// in this batch (or `None` for an empty batch); the frontend uses it
    /// directly as its reconnect-replay watermark. Both `message_meta` and
    /// `last_created_at` default to absent for wire-compat with older backends.
    HistoryBatch {
        messages: Vec<serde_json::Value>,
        /// Typed portal sidecar, **index-aligned** with `messages` (entry `i`
        /// is the meta for `messages[i]`; `None` when a message has no portal
        /// metadata). A parallel vector rather than a `{content, meta}` wrapper
        /// so `messages` stays a plain list of raw content; the frontend zips
        /// the two. `#[serde(default)]` ⇒ empty/absent on older backends.
        /// (Collapsing this into a typed `HistoryEntry` is tracked in #1139.)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        message_meta: Vec<Option<PortalMeta>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_created_at: Option<String>,
    },

    /// Delivery-progress update for a previously-sent `AgentInput`, keyed by
    /// its browser-assigned `client_msg_id`. The frontend advances the matching
    /// optimistic row through its stages and keeps it "pending"-styled until
    /// `AgentAccepted` (the message is in the agent's stream). See
    /// [`InputDeliveryStage`].
    InputProgress {
        client_msg_id: Uuid,
        stage: InputDeliveryStage,
        /// Populated for `Failed` (the delivery error), absent otherwise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
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
