//! Agent-neutral protocol boundary (refactor #1165 item 2).
//!
//! The generic `Session<A>` core never touches `claude_codes` / `codex_codes`
//! protocol types. The one trait here, [`AgentOutputClassifier`], parses a unit
//! of agent output into neutral [`AgentOutput`] decisions. It runs **inside each
//! per-agent I/O task** (Claude's `claude_io_task` via [`ClaudeAdapter`]; Codex's
//! via `CodexClassifier`), which then emits `IoEvent::Classified(AgentOutput)`;
//! `Session` only maps those neutral decisions to `SessionEvent`s.
//!
//! The **input** side (turning neutral [`crate::io::IoCommand`]s into the
//! agent's wire form) is NOT a trait — there is no agent-neutral input
//! abstraction. Each I/O task serializes input directly against its typed
//! client (Claude: `ClaudeInput` / `ControlResponse` to stdin; Codex:
//! `turn_start` / `respond` RPC). Phase-A slice 2 removed the old
//! Claude-transport-shaped `AgentAdapter` input trait, which couldn't fit
//! Codex's async RPC model. A future `AgentRuntime` may unify it.
//!
//! See `docs/design/session-adapter.md` and `docs/design/agent-runtime.md`.

/// One unit of agent stdout, as opaque wire JSON. The classifier parses it.
pub type RawUnit = serde_json::Value;

/// A neutral decision produced by the classifier from one unit of agent output.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentOutput {
    /// User-visible output to forward/persist verbatim (opaque wire JSON).
    Visible(serde_json::Value),
    /// A permission/control request awaiting a response.
    PermissionRequest {
        /// Neutral correlation id, echoed back in [`crate::io::IoCommand::Permission`].
        request_id: String,
        /// Stringly-typed tool name (the same key the claude path carries
        /// verbatim from `ToolUseRequest`).
        tool_name: String,
        /// Tool input parameters.
        input: serde_json::Value,
        /// Opaque suggested permissions (claude `PermissionSuggestion`s
        /// serialized to JSON for slice 1). See design doc open questions.
        suggestions: Vec<serde_json::Value>,
    },
    /// Fatal "conversation not found" → maps to `SessionEvent::SessionNotFound`.
    NotFound,
    /// An ephemeral tool-progress heartbeat (Claude's `tool_progress` frame).
    ///
    /// A distinct outcome from `Visible` on purpose: heartbeats are pure
    /// live-status and must NOT be buffered or persisted (a long-running tool
    /// emits one every ~30s — a 20-minute Bash call would otherwise write ~40
    /// junk rows into `messages` and render ~40 "Unrecognized Message" cards).
    /// `Session` maps this to `SessionEvent::ToolProgress` without touching the
    /// replay buffer; the proxy forwards it on a typed side-channel that the
    /// backend fans out to web clients (never to the DB). Codex has no analogue
    /// today, so only `ClaudeAdapter` produces it.
    ToolProgress {
        /// The heartbeat's own id (`<tool_use_id>-heartbeat-N`).
        tool_use_id: String,
        /// The running tool's id when Claude provides it (see the wire docs on
        /// `shared::ProxyToServer::ToolProgress`).
        parent_tool_use_id: Option<String>,
        tool_name: String,
        elapsed_time_seconds: f64,
    },
    /// Internal ack / nothing for the consumer — `Session` skips it.
    Noop,
}

/// Neutral permission decision — the agent-agnostic form of
/// [`crate::io::PermissionResponse`], minus the concrete `claude_codes` types.
#[derive(Debug, Clone, Default)]
pub struct PermissionDecision {
    /// Whether to allow the tool use.
    pub allow: bool,
    /// Optional modified input (for edit suggestions).
    pub modified_input: Option<serde_json::Value>,
    /// Permissions to remember for future similar operations. Opaque for
    /// slice 1 (claude `Permission`s serialized to JSON).
    pub remember: Vec<serde_json::Value>,
    /// Reason for denial (used when `allow` is false).
    pub reason: Option<String>,
}

/// Output-classification boundary: parse one unit of agent output into neutral
/// [`AgentOutput`] decisions — the part of the protocol surface every backend
/// can honestly implement (both Claude and Codex map their output here).
///
/// Generic over the input unit via [`Raw`](AgentOutputClassifier::Raw) because
/// the backends' output units differ: Claude's I/O task yields opaque stdout
/// JSON (`serde_json::Value`), while Codex's yields a typed
/// `codex_codes::ServerMessage` that is **not** `Deserialize` (the SDK parses
/// it with custom logic), so it cannot be reconstructed from a `Value`. Pinning
/// a single `Raw = Value` would lock Codex out; the associated type lets each
/// backend classify its native unit while sharing the neutral [`AgentOutput`].
///
/// The input side (turning neutral commands into wire form) is deliberately
/// NOT part of this trait: it lives in each I/O task against its typed client,
/// because the agents' input models diverge (Claude writes to stdin
/// synchronously; Codex issues async `turn_start`/`respond` RPCs). See the
/// module docs.
///
/// `Send + Sync + 'static` so a boxed classifier can be held across threads if
/// needed and `Session<A>` stays `Sync` for consumers that share a session (the
/// launcher holds sessions in shared state). Classifiers are stateless today;
/// any future stateful classifier must use interior mutability that is itself
/// `Sync`.
pub trait AgentOutputClassifier: Send + Sync + 'static {
    /// The native output unit this classifier parses (Claude: `serde_json::Value`
    /// stdout JSON; Codex: `codex_codes::ServerMessage`).
    type Raw;

    /// Parse + classify one unit of agent output into 0..n neutral decisions.
    ///
    /// `&mut self` so future stateful classifiers (e.g. codex, which threads a
    /// request-id ↔ tool map) can update internal state. Claude is stateless
    /// and 1:1 (one unit → exactly one `AgentOutput`).
    fn classify(&mut self, raw: Self::Raw) -> Vec<AgentOutput>;
}

/// Claude-protocol output classifier. Stateless; a unit struct.
///
/// The input side (building `ClaudeInput` / `ControlResponse` from neutral
/// commands) lives in `claude_io_task`, which owns the typed client — there is
/// no agent-neutral input trait (phase A slice 2 removed the old
/// `AgentAdapter`; see `docs/design/agent-runtime.md`).
#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeAdapter;

impl AgentOutputClassifier for ClaudeAdapter {
    type Raw = RawUnit;

    fn classify(&mut self, raw: RawUnit) -> Vec<AgentOutput> {
        use claude_codes::io::ControlRequestPayload;
        use claude_codes::ClaudeOutput;

        // Parse failure → forward verbatim, never panic. Mirrors the proxy/io
        // path which keeps unparseable JSON visible rather than dropping it.
        let output: ClaudeOutput = match serde_json::from_value(raw.clone()) {
            Ok(output) => output,
            Err(_) => return vec![AgentOutput::Visible(raw)],
        };

        match output {
            // "No conversation found" → session not found locally. Any other
            // Result is ordinary user-visible output.
            ClaudeOutput::Result(ref res) => {
                if res.is_error
                    && res
                        .errors
                        .iter()
                        .any(|e| e.contains("No conversation found"))
                {
                    vec![AgentOutput::NotFound]
                } else {
                    vec![AgentOutput::Visible(raw)]
                }
            }
            // Tool permission request → neutral PermissionRequest. Any other
            // ControlRequest variant (hooks, mcp, initialize) is forwarded
            // verbatim, matching `next_event`'s fall-through.
            ClaudeOutput::ControlRequest(ref req) => {
                if let ControlRequestPayload::CanUseTool(ref tool_req) = req.request {
                    let suggestions = tool_req
                        .permission_suggestions
                        .iter()
                        .map(|s| serde_json::to_value(s).unwrap_or(serde_json::Value::Null))
                        .collect();
                    vec![AgentOutput::PermissionRequest {
                        request_id: req.request_id.clone(),
                        tool_name: tool_req.tool_name.clone(),
                        input: tool_req.input.clone(),
                        suggestions,
                    }]
                } else {
                    vec![AgentOutput::Visible(raw)]
                }
            }
            // Control responses are CLI acks — internal, the consumer skips.
            ClaudeOutput::ControlResponse(_) => vec![AgentOutput::Noop],
            // Tool-progress heartbeat → ephemeral live status, never persisted.
            // Handled explicitly (not via the `Visible` fall-through) so a
            // long-running tool's ~30s heartbeats don't flood `messages` /
            // render as "Unrecognized Message" cards. See `AgentOutput::ToolProgress`.
            ClaudeOutput::ToolProgress(tp) => vec![AgentOutput::ToolProgress {
                tool_use_id: tp.tool_use_id,
                parent_tool_use_id: tp.parent_tool_use_id,
                tool_name: tp.tool_name,
                elapsed_time_seconds: tp.elapsed_time_seconds,
            }],
            // Everything else (System, User, Assistant, Error, RateLimitEvent)
            // is user-visible.
            _ => vec![AgentOutput::Visible(raw)],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- classify golden tests ---------------------------------------------

    #[test]
    fn classify_assistant_message_is_visible() {
        let raw = json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{"type": "text", "text": "hi"}]
            }
        });
        let mut adapter = ClaudeAdapter;
        assert_eq!(
            adapter.classify(raw.clone()),
            vec![AgentOutput::Visible(raw)]
        );
    }

    #[test]
    fn classify_user_message_is_visible() {
        let raw = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": "hello"}]
            }
        });
        let mut adapter = ClaudeAdapter;
        assert_eq!(
            adapter.classify(raw.clone()),
            vec![AgentOutput::Visible(raw)]
        );
    }

    #[test]
    fn classify_ordinary_result_is_visible() {
        let raw = json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "duration_ms": 100,
            "duration_api_ms": 200,
            "num_turns": 1,
            "result": "Done",
            "session_id": "abc",
            "total_cost_usd": 0.01
        });
        let mut adapter = ClaudeAdapter;
        assert_eq!(
            adapter.classify(raw.clone()),
            vec![AgentOutput::Visible(raw)]
        );
    }

    #[test]
    fn classify_no_conversation_found_result_is_not_found() {
        // Exactly the shape `next_event` keys on: is_error + an errors entry
        // containing "No conversation found".
        let raw = json!({
            "type": "result",
            "subtype": "error_during_execution",
            "is_error": true,
            "duration_ms": 0,
            "duration_api_ms": 0,
            "num_turns": 0,
            "session_id": "27934753-425a-4182-892c-6b1c15050c3f",
            "total_cost_usd": 0,
            "errors": ["No conversation found with session ID: d56965c9-c855-4042-a8f5-f12bbb14d6f6"]
        });
        let mut adapter = ClaudeAdapter;
        assert_eq!(adapter.classify(raw), vec![AgentOutput::NotFound]);
    }

    #[test]
    fn classify_error_result_without_no_conversation_is_visible() {
        // is_error but a *different* error string → still user-visible,
        // mirroring `next_event`'s `.any(... "No conversation found")` guard.
        let raw = json!({
            "type": "result",
            "subtype": "error_during_execution",
            "is_error": true,
            "duration_ms": 0,
            "duration_api_ms": 0,
            "num_turns": 0,
            "session_id": "abc",
            "total_cost_usd": 0,
            "errors": ["Some other failure"]
        });
        let mut adapter = ClaudeAdapter;
        assert_eq!(
            adapter.classify(raw.clone()),
            vec![AgentOutput::Visible(raw)]
        );
    }

    #[test]
    fn classify_can_use_tool_is_permission_request() {
        let raw = json!({
            "type": "control_request",
            "request_id": "perm-abc123",
            "request": {
                "subtype": "can_use_tool",
                "tool_name": "Write",
                "input": {"file_path": "/tmp/hello.py", "content": "print('hi')"},
                "permission_suggestions": [
                    {"type": "setMode", "mode": "acceptEdits", "destination": "session"}
                ]
            }
        });
        let mut adapter = ClaudeAdapter;
        let out = adapter.classify(raw);
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentOutput::PermissionRequest {
                request_id,
                tool_name,
                input,
                suggestions,
            } => {
                assert_eq!(request_id, "perm-abc123");
                assert_eq!(tool_name, "Write");
                assert_eq!(input["file_path"], json!("/tmp/hello.py"));
                assert_eq!(input["content"], json!("print('hi')"));
                assert_eq!(suggestions.len(), 1);
                assert_eq!(suggestions[0]["type"], json!("setMode"));
                assert_eq!(suggestions[0]["mode"], json!("acceptEdits"));
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn classify_non_can_use_tool_control_request_is_visible() {
        // An initialize control request is not a CanUseTool → forwarded verbatim.
        let raw = json!({
            "type": "control_request",
            "request_id": "init-1",
            "request": {"subtype": "initialize"}
        });
        let mut adapter = ClaudeAdapter;
        assert_eq!(
            adapter.classify(raw.clone()),
            vec![AgentOutput::Visible(raw)]
        );
    }

    #[test]
    fn classify_control_response_is_noop() {
        let raw = json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": "req-1"}
        });
        let mut adapter = ClaudeAdapter;
        assert_eq!(adapter.classify(raw), vec![AgentOutput::Noop]);
    }

    #[test]
    fn classify_tool_progress_is_ephemeral_not_visible() {
        // The production heartbeat shape: a `-heartbeat-N` tool_use_id with the
        // running tool's base id in `parent_tool_use_id`, arriving every ~30s.
        let raw = json!({
            "type": "tool_progress",
            "tool_name": "Bash",
            "elapsed_time_seconds": 30.0,
            "parent_tool_use_id": "toolu_01abc",
            "tool_use_id": "toolu_01abc-heartbeat-0",
            "session_id": "01890000-0000-7000-8000-000000000001",
            "uuid": "01890000-0000-7000-8000-000000000002"
        });
        let mut adapter = ClaudeAdapter;
        let out = adapter.classify(raw);
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentOutput::ToolProgress {
                tool_use_id,
                parent_tool_use_id,
                tool_name,
                elapsed_time_seconds,
            } => {
                assert_eq!(tool_use_id, "toolu_01abc-heartbeat-0");
                assert_eq!(parent_tool_use_id.as_deref(), Some("toolu_01abc"));
                assert_eq!(tool_name, "Bash");
                assert_eq!(*elapsed_time_seconds, 30.0);
            }
            other => panic!("expected ToolProgress, got {other:?}"),
        }
        // The critical invariant: a heartbeat is NOT a Visible/persisted frame.
        assert!(
            !matches!(&out[0], AgentOutput::Visible(_)),
            "tool_progress must never classify as Visible — it would be persisted"
        );
    }

    #[test]
    fn classify_garbage_is_visible_no_panic() {
        let raw = json!({"totally": "unrecognized", "shape": [1, 2, 3]});
        let mut adapter = ClaudeAdapter;
        assert_eq!(
            adapter.classify(raw.clone()),
            vec![AgentOutput::Visible(raw)]
        );

        // A non-object value also must not panic.
        let scalar = json!(42);
        assert_eq!(
            adapter.classify(scalar.clone()),
            vec![AgentOutput::Visible(scalar)]
        );
    }
}
