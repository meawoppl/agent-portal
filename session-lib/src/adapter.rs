//! Agent-neutral adapter boundary (refactor item #1, slice 2a — additive).
//!
//! An [`AgentAdapter`] owns all protocol parse/serialize for one agent backend
//! and exposes only **neutral decisions** to the generic `Session<A>` core.
//! Concrete protocol enums (`claude_codes::*`, `codex_codes::*`) stay *inside*
//! the adapter and never surface into `session.rs`.
//!
//! This slice introduces the trait, the neutral types, and a [`ClaudeAdapter`]
//! whose [`classify`](AgentAdapter::classify) is a pure function mirroring
//! exactly what `session.rs::next_event` currently decides for `ClaudeOutput`.
//! It is **not** yet wired into `session.rs`; that happens in a later slice.
//!
//! See `docs/design/session-adapter.md` for the full design.

use uuid::Uuid;

/// One unit of agent stdout, as opaque wire JSON. The adapter parses it.
pub type RawUnit = serde_json::Value;

/// Opaque payload the I/O task writes to the agent. The adapter produces it;
/// `Session` never inspects it.
pub type TransportPayload = serde_json::Value;

/// A neutral decision produced by the adapter from one unit of agent output.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentOutput {
    /// User-visible output to forward/persist verbatim (opaque wire JSON).
    Visible(serde_json::Value),
    /// A permission/control request awaiting a response.
    PermissionRequest {
        /// Neutral correlation id for the later [`AgentAdapter::permission_response`].
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
/// [`AgentOutput`] decisions. This is the *only* part of the adapter that fits
/// every backend — both Claude and Codex can honestly map their output here.
///
/// Generic over the input unit via [`Raw`](AgentOutputClassifier::Raw) because
/// the backends' output units differ: Claude's I/O task yields opaque stdout
/// JSON (`serde_json::Value`), while Codex's yields a typed
/// `codex_codes::ServerMessage` that is **not** `Deserialize` (the SDK parses
/// it with custom logic), so it cannot be reconstructed from a `Value`. Pinning
/// a single `Raw = Value` would lock Codex out; the associated type lets each
/// backend classify its native unit while sharing the neutral [`AgentOutput`].
///
/// It is split out from [`AgentAdapter`] (which pins `Raw = serde_json::Value`)
/// because the rest of `AgentAdapter` — `user_input`/`permission_response`
/// returning a sync `TransportPayload` for the I/O task to write — is
/// Claude-transport-shaped and does *not* fit Codex, whose input/permission
/// flow is an async `turn_start`/`respond` RPC owned by `codex_io_task`. A
/// backend that only classifies output (Codex) implements this trait alone;
/// full unification of the input side is a separate `AgentRuntime` design (see
/// `docs/design/session-adapter.md`).
///
/// `Sync` so `Session<A>`, which stores `Box<dyn AgentAdapter>`, stays `Sync`
/// for consumers that share a session across threads (the launcher holds
/// sessions in shared state). Adapters are stateless today; any future stateful
/// adapter must use interior mutability that is itself `Sync`.
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

/// Per-agent protocol parse/serialize boundary for the generic `Session<A>`,
/// for backends whose input side fits the sync stdin-payload model (Claude).
///
/// Pins `Raw = serde_json::Value` so `Session`'s `dyn AgentAdapter` can call
/// `classify` with the stdout JSON it already serializes.
pub trait AgentAdapter: AgentOutputClassifier<Raw = RawUnit> {
    /// Build the transport payload for plain user text.
    fn user_input(&self, text: &str, session_id: Uuid) -> TransportPayload;

    /// Build the transport payload responding to a prior `PermissionRequest`.
    fn permission_response(
        &self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> TransportPayload;

    /// Optional control inputs (interrupt, etc.).
    fn interrupt(&self) -> Option<TransportPayload> {
        None
    }
}

/// Claude-protocol adapter. Stateless for now; a unit struct.
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
            // Everything else (System, User, Assistant, Error, RateLimitEvent)
            // is user-visible.
            _ => vec![AgentOutput::Visible(raw)],
        }
    }
}

impl AgentAdapter for ClaudeAdapter {
    fn user_input(&self, text: &str, session_id: Uuid) -> TransportPayload {
        let input = claude_codes::ClaudeInput::user_message(text, session_id);
        serde_json::to_value(&input).unwrap_or(serde_json::Value::Null)
    }

    fn permission_response(
        &self,
        request_id: &str,
        decision: PermissionDecision,
    ) -> TransportPayload {
        use claude_codes::io::{ControlResponse, PermissionResult};

        // Mirrors `session.rs::respond_permission` (claude path):
        // - input defaults to an empty object when not modified,
        // - non-empty remembered permissions use the typed-permissions allow,
        // - deny carries the reason, defaulting to "User denied".
        let ctrl_response = if decision.allow {
            let input_value = decision
                .modified_input
                .unwrap_or(serde_json::Value::Object(Default::default()));
            if decision.remember.is_empty() {
                ControlResponse::from_result(request_id, PermissionResult::allow(input_value))
            } else {
                ControlResponse::from_result(
                    request_id,
                    PermissionResult::allow_with_permissions(input_value, decision.remember),
                )
            }
        } else {
            let reason = decision.reason.unwrap_or_else(|| "User denied".to_string());
            ControlResponse::from_result(request_id, PermissionResult::deny(reason))
        };

        serde_json::to_value(&ctrl_response).unwrap_or(serde_json::Value::Null)
    }

    fn interrupt(&self) -> Option<TransportPayload> {
        Some(
            serde_json::to_value(claude_codes::ClaudeInput::interrupt())
                .unwrap_or(serde_json::Value::Null),
        )
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

    // --- input / permission serialization tests ----------------------------

    #[test]
    fn user_input_builds_claude_user_message() {
        let adapter = ClaudeAdapter;
        let session_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let payload = adapter.user_input("Hello, Claude!", session_id);
        assert_eq!(payload["type"], json!("user"));
        assert_eq!(payload["message"]["role"], json!("user"));
        assert_eq!(
            payload["message"]["content"][0]["text"],
            json!("Hello, Claude!")
        );
        assert_eq!(
            payload["session_id"],
            json!("550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[test]
    fn permission_response_allow_uses_empty_input_by_default() {
        let adapter = ClaudeAdapter;
        let payload = adapter.permission_response(
            "req-1",
            PermissionDecision {
                allow: true,
                ..Default::default()
            },
        );
        let inner = &payload["response"];
        assert_eq!(inner["subtype"], json!("success"));
        assert_eq!(inner["request_id"], json!("req-1"));
        assert_eq!(inner["response"]["behavior"], json!("allow"));
        // input defaults to an empty object (mirrors respond_permission).
        assert_eq!(inner["response"]["updatedInput"], json!({}));
        assert!(inner["response"].get("updatedPermissions").is_none());
    }

    #[test]
    fn permission_response_allow_with_modified_input_and_remember() {
        let adapter = ClaudeAdapter;
        let perm =
            serde_json::to_value(claude_codes::Permission::allow_tool("Bash", "npm test")).unwrap();
        let payload = adapter.permission_response(
            "req-2",
            PermissionDecision {
                allow: true,
                modified_input: Some(json!({"command": "npm test"})),
                remember: vec![perm],
                reason: None,
            },
        );
        let result = &payload["response"]["response"];
        assert_eq!(result["behavior"], json!("allow"));
        assert_eq!(result["updatedInput"], json!({"command": "npm test"}));
        assert_eq!(result["updatedPermissions"][0]["type"], json!("addRules"));
        assert_eq!(
            result["updatedPermissions"][0]["rules"][0]["toolName"],
            json!("Bash")
        );
    }

    #[test]
    fn permission_response_deny_uses_reason_with_default() {
        let adapter = ClaudeAdapter;

        // Explicit reason.
        let payload = adapter.permission_response(
            "req-3",
            PermissionDecision {
                allow: false,
                reason: Some("nope".to_string()),
                ..Default::default()
            },
        );
        let result = &payload["response"]["response"];
        assert_eq!(result["behavior"], json!("deny"));
        assert_eq!(result["message"], json!("nope"));

        // Default reason mirrors respond_permission's "User denied".
        let payload = adapter.permission_response(
            "req-4",
            PermissionDecision {
                allow: false,
                ..Default::default()
            },
        );
        assert_eq!(
            payload["response"]["response"]["message"],
            json!("User denied")
        );
    }

    #[test]
    fn interrupt_builds_interrupt_payload() {
        let adapter = ClaudeAdapter;
        let payload = adapter.interrupt().expect("claude supports interrupt");
        assert_eq!(payload["subtype"], json!("interrupt"));
    }
}
