//! Cross-agent I/O command + event types used by the `Session<A>` lifecycle.
//!
//! These enums are deliberately the union of every agent backend's needs —
//! per-agent I/O tasks just ignore the variants they don't handle. See
//! `agent.rs` for the dispatch trait.

use claude_codes::io::{ControlResponse, PermissionSuggestion};
use claude_codes::{ClaudeInput, ClaudeOutput};
use shared::{CodexPermissionInput, TurnMetrics};
use tokio::sync::oneshot;

use crate::error::SessionError;

/// Commands sent from `Session<A>` to the per-agent I/O task.
///
/// Per-agent tasks ignore variants they don't handle:
/// - Claude io task ignores `CodexApproval`.
/// - Codex io task ignores `PermissionResponse`.
pub enum IoCommand {
    /// User input to forward to the agent.
    Input {
        input: ClaudeInput,
        delivered: Option<oneshot::Sender<Result<(), String>>>,
    },
    /// Permission response for Claude's `can_use_tool` control flow.
    PermissionResponse(ControlResponse),
    /// Approval response for Codex's app-server JSON-RPC approval flow.
    CodexApproval {
        request_id: String,
        result: serde_json::Value,
    },
}

/// Events emitted from the per-agent I/O task back up to `Session<A>`.
pub enum IoEvent {
    /// Typed output from a claude-protocol session.
    Output(Box<ClaudeOutput>),
    /// Raw JSON output from a non-claude agent (e.g. Codex JSONL).
    ///
    /// Bypasses claude-specific processing (permission extraction,
    /// `ControlResponse` filtering) and is forwarded directly to the
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
    Error(SessionError),
    Exited {
        code: i32,
    },
}

/// Events emitted by a `Session<A>` to its consumer.
#[derive(Debug)]
pub enum SessionEvent {
    /// Claude produced output (excluding permission requests, which have
    /// their own event).
    Output(Box<ClaudeOutput>),

    /// Raw JSON output from a non-Claude agent (e.g. Codex JSONL).
    ///
    /// This bypasses Claude-specific processing (permission extraction,
    /// ControlResponse filtering) and is forwarded directly to the
    /// backend/frontend.
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
