//! Cross-agent I/O command + event types used by the `Session<A>` lifecycle.
//!
//! These enums are deliberately the union of every agent backend's needs —
//! per-agent I/O tasks just ignore the variants they don't handle. See
//! `agent.rs` for the dispatch trait.

use claude_codes::io::PermissionSuggestion;
use shared::{CodexPermissionInput, TurnMetrics};
use tokio::sync::oneshot;

use crate::adapter::{AgentOutput, PermissionDecision};
use crate::error::SessionError;

/// Neutral commands sent from `Session<A>` to the per-agent I/O task.
///
/// These carry no concrete-protocol types — each I/O task translates them into
/// its own wire form (Claude: `ClaudeInput` / `ControlResponse` to stdin;
/// Codex: `turn_start` / `respond` JSON-RPC). `Session` stays agent-neutral
/// (#1165 item 2, phase A slice 2).
pub enum IoCommand {
    /// Plain user input to forward to the agent.
    UserInput {
        /// The user-facing text. The I/O task wraps it in its protocol's
        /// user-message form.
        text: String,
        delivered: Option<oneshot::Sender<Result<(), String>>>,
        /// Optional typed portal event to display in place of the agent's echo
        /// of this input (e.g. an inter-agent `PortalContent::AgentMessage`).
        /// Claude relies on the proxy's echo-replacement and ignores this;
        /// Codex — which doesn't echo — emits this verbatim as its synthetic
        /// echo so inter-agent messages render as the typed card with
        /// provenance instead of a raw `[message from …]` / JSON user bubble.
        /// Boxed to keep this variant from dominating `IoCommand`'s size.
        display_event: Option<Box<serde_json::Value>>,
    },
    /// Response to a pending permission request. The I/O task serializes the
    /// neutral decision into its protocol form (Claude `ControlResponse`,
    /// Codex `accept`/`decline` approval).
    Permission {
        request_id: String,
        decision: PermissionDecision,
    },
}

/// Events emitted from the per-agent I/O task back up to `Session<A>`.
pub enum IoEvent {
    /// The agent OS process has been spawned; carries its pid (when the
    /// platform exposes one). `Session` records it so `stop()` can signal the
    /// agent's process group directly rather than relying on `kill_on_drop`,
    /// which the SDK's detached-task ownership of the child defeats (#927).
    AgentStarted {
        pid: Option<u32>,
    },
    /// A neutral, already-classified output decision from the agent's I/O task.
    ///
    /// The per-agent I/O task runs its `AgentOutputClassifier` (Claude:
    /// `ClaudeAdapter`; Codex: `CodexClassifier`) and emits the resulting
    /// neutral [`AgentOutput`]s here, so `Session` never sees a concrete
    /// protocol type. `Session::next_event` maps each to a `SessionEvent`
    /// (`Visible` → `RawOutput`, `PermissionRequest` → `PermissionRequest`,
    /// `NotFound` → `SessionNotFound`, `Noop` → skip).
    Classified(AgentOutput),
    /// Raw JSON output forwarded directly (e.g. Codex JSONL frames the codex
    /// I/O task emits outside the classifier, and portal-reminder messages).
    /// Bypasses classification and is forwarded verbatim to the
    /// backend/frontend.
    RawOutput(serde_json::Value),
    /// Permission request from Codex's app-server approval flow.
    ///
    /// Claude permission requests come up the typed `Output` channel
    /// (`ClaudeOutput::ControlRequest`) and `Session::next_event` handles
    /// the extraction.
    ///
    /// The `tool_name` discriminant lives on the `CodexPermissionInput`
    /// variant — call `input.tool_name()` if a stringly-typed key is
    /// needed (e.g. for the `SessionEvent::PermissionRequest` envelope
    /// which still carries one for the cross-agent claude path).
    CodexPermissionRequest {
        request_id: String,
        input: CodexPermissionInput,
    },
    /// A completed-turn performance metrics payload, ready for the proxy to
    /// ship to the backend as `ProxyToServer::TurnMetricsReport`. Emitted
    /// once per finalize by the agent-specific I/O task.
    TurnMetricsReady(Box<TurnMetrics>),
    /// Claude reported a hard session limit with a reset time. The proxy
    /// persists a one-shot continuation candidate via the backend.
    SessionLimitReached {
        session_id: uuid::Uuid,
        reset_at: String,
        source_message: String,
        prompt: String,
    },
    /// The codex io-task learned (or re-confirmed) the app-server thread
    /// id for this session — emitted exactly once per spawn after
    /// `thread_start` / `thread/resume` returns. The proxy persists the
    /// value into `ProxyConfig.directory_sessions[wd].codex_thread_id` so
    /// the next launch of the same session can pass it back through
    /// `SessionConfig.codex_thread_id` to drive `thread/resume`.
    CodexThreadId(String),
    Error(SessionError),
    Exited {
        code: i32,
    },
}

/// Events emitted by a `Session<A>` to its consumer.
#[derive(Debug)]
pub enum SessionEvent {
    /// User-visible agent output, as the original raw wire JSON.
    ///
    /// Both backends now arrive here: the per-agent I/O task classifies its
    /// native output and `Session` forwards each `AgentOutput::Visible` value
    /// verbatim. Agent-specific consumers (e.g. the Claude proxy
    /// `output_forwarder`, which re-parses for image/git/echo handling) live at
    /// the proxy edge, keyed on `agent_type` — `Session` stays neutral.
    RawOutput(serde_json::Value),

    /// The agent is requesting permission for a tool.
    PermissionRequest {
        request_id: String,
        tool_name: String,
        input: serde_json::Value,
        permission_suggestions: Vec<PermissionSuggestion>,
    },

    /// Session not found locally (e.g., when resuming an expired session).
    SessionNotFound,

    /// Agent process exited.
    Exited { code: i32 },

    /// Session encountered an error.
    Error(SessionError),

    /// A completed-turn performance metrics payload. The proxy forwards this
    /// to the backend as `ProxyToServer::TurnMetricsReport`. See PR-1
    /// design doc / `shared::TurnMetrics` for field semantics.
    TurnMetricsReady(Box<TurnMetrics>),

    /// Codex app-server thread id, surfaced by the codex io-task so the
    /// proxy can persist it. See `IoEvent::CodexThreadId`.
    CodexThreadId(String),

    /// Claude reported a hard session limit with a reset time.
    SessionLimitReached {
        session_id: uuid::Uuid,
        reset_at: String,
        source_message: String,
        prompt: String,
    },
}

/// Response to a permission request.
#[derive(Debug, Clone, Default)]
pub struct PermissionResponse {
    /// Whether to allow the tool use.
    pub allow: bool,
    /// Optional modified input (for edit suggestions).
    pub input: Option<serde_json::Value>,
    /// Permissions to grant for future similar operations ("remember this decision").
    pub permissions: Vec<claude_codes::Permission>,
    /// Reason for denial (if allow is false).
    pub reason: Option<String>,
}

impl PermissionResponse {
    /// Create an allow response.
    pub fn allow() -> Self {
        Self {
            allow: true,
            input: None,
            permissions: vec![],
            reason: None,
        }
    }

    /// Create an allow response with modified input.
    pub fn allow_with_input(input: serde_json::Value) -> Self {
        Self {
            allow: true,
            input: Some(input),
            permissions: vec![],
            reason: None,
        }
    }

    /// Create an allow response with permissions to remember.
    pub fn allow_and_remember(permissions: Vec<claude_codes::Permission>) -> Self {
        Self {
            allow: true,
            input: None,
            permissions,
            reason: None,
        }
    }

    /// Create an allow response with input and permissions to remember.
    pub fn allow_with_input_and_remember(
        input: serde_json::Value,
        permissions: Vec<claude_codes::Permission>,
    ) -> Self {
        Self {
            allow: true,
            input: Some(input),
            permissions,
            reason: None,
        }
    }

    /// Create a deny response.
    pub fn deny() -> Self {
        Self::default()
    }

    /// Create a deny response with a reason.
    pub fn deny_with_reason(reason: impl Into<String>) -> Self {
        Self {
            allow: false,
            input: None,
            permissions: vec![],
            reason: Some(reason.into()),
        }
    }
}
