//! [`CodexClassifier`]: the [`AgentOutputClassifier`] for the Codex
//! app-server protocol.
//!
//! Maps one `codex_codes::ServerMessage` into neutral [`AgentOutput`]
//! decisions, mirroring the output shaping `handler.rs` currently performs
//! inline in `codex_io_task` — user-visible item/error/notice/passthrough
//! frames become [`AgentOutput::Visible`], approval requests become
//! [`AgentOutput::PermissionRequest`], and pure-lifecycle frames are
//! [`AgentOutput::Noop`].
//!
//! Scope (issue #1165 item 2, "classification boundary"): this implements
//! ONLY [`AgentOutputClassifier`]. Codex's input/permission flow is an async
//! `turn_start`/`respond` RPC owned by `codex_io_task`, which does not fit the
//! sync `AgentAdapter::user_input`/`permission_response` -> `TransportPayload`
//! contract — so `CodexClassifier` deliberately does not implement
//! `AgentAdapter`. Full input-side unification is a separate `AgentRuntime`
//! design (see `docs/design/session-adapter.md`).
//!
//! `TurnCompleted` is intentionally `Noop` here: it carries turn status +
//! token usage and drives per-turn metrics finalization, which is turn/token
//! orchestration that stays in `codex_io_task` (it owns the metrics tracker,
//! subagent-token tracker, and the `latest_token_usage` feed). The classifier
//! is concerned only with user-visible output mapping.
//!
//! This is the additive (unwired) first cut, introduced alongside the still-
//! live `handle_codex_server_message` path exactly as `ClaudeAdapter` was
//! introduced before `session.rs` was wired to it. The follow-up slice routes
//! `codex_io_task` through this classifier and deletes the duplicate mapping
//! in `handler.rs`.

use codex_codes::{Notification, ServerMessage, ServerRequest};
use session_lib::{AgentOutput, AgentOutputClassifier};
use shared::CodexPermissionInput;

use crate::events::{to_raw_output, ErrorEvent, ItemEvent, PassthroughEvent};
use crate::helpers::format_request_id;

/// Stateless classifier for Codex `ServerMessage`s. A unit struct today;
/// `&mut self` on `classify` leaves room for a future request-id ↔ tool map.
///
/// Its [`Raw`](AgentOutputClassifier::Raw) is the typed `ServerMessage` rather
/// than `serde_json::Value`. The I/O task holds the typed message and hands it
/// straight here — no JSON round-trip.
#[derive(Debug, Clone, Copy, Default)]
pub struct CodexClassifier;

impl AgentOutputClassifier for CodexClassifier {
    type Raw = ServerMessage;

    fn classify(&mut self, msg: ServerMessage) -> Vec<AgentOutput> {
        match msg {
            ServerMessage::Notification(notif) => classify_notification(notif),
            ServerMessage::Request { id, request } => {
                classify_request(format_request_id(&id), request)
            }
        }
    }
}

fn classify_notification(notif: Notification) -> Vec<AgentOutput> {
    match notif {
        // Pure lifecycle — no user-facing output (handler returns no event).
        Notification::ThreadStarted(_)
        | Notification::ThreadStatusChanged(_)
        | Notification::TurnStarted(_)
        | Notification::ThreadTokenUsageUpdated(_) => vec![AgentOutput::Noop],

        // Turn completion is turn/token orchestration owned by `codex_io_task`
        // (status + usage + metrics finalize). The classifier does not shape it.
        Notification::TurnCompleted(_) => vec![AgentOutput::Noop],

        Notification::ItemStarted(p) => {
            let item = serde_json::to_value(&p.item).unwrap_or(serde_json::Value::Null);
            vec![AgentOutput::Visible(to_raw_output(&ItemEvent::started(
                item,
            )))]
        }

        Notification::ItemCompleted(p) => {
            let item = serde_json::to_value(&p.item).unwrap_or(serde_json::Value::Null);
            vec![AgentOutput::Visible(to_raw_output(&ItemEvent::completed(
                item,
            )))]
        }

        Notification::Error(p) => {
            let message = if p.error.message.is_empty() {
                "Unknown error".to_string()
            } else {
                p.error.message.clone()
            };
            vec![AgentOutput::Visible(to_raw_output(&ErrorEvent::new(
                message,
            )))]
        }

        Notification::DeprecationNotice(p) => {
            let summary = if p.summary.is_empty() {
                "(no message)".to_string()
            } else {
                p.summary.clone()
            };
            let event =
                shared::PortalMessage::text(format!("**Codex deprecation notice**: {}", summary))
                    .to_json();
            vec![AgentOutput::Visible(event)]
        }

        Notification::GuardianWarning(p) => {
            let message = if p.message.is_empty() {
                "(no message)".to_string()
            } else {
                p.message.clone()
            };
            let event =
                shared::PortalMessage::text(format!("**Codex guardian warning**: {}", message))
                    .to_json();
            vec![AgentOutput::Visible(event)]
        }

        // Streaming/plan/diff notifications — emit a typed passthrough block.
        Notification::PlanDelta(p) => passthrough("item/plan/delta", &p),
        Notification::TurnPlanUpdated(p) => passthrough("turn/plan/updated", &p),
        Notification::TurnDiffUpdated(p) => passthrough("turn/diff/updated", &p),
        Notification::ReasoningSummaryPartAdded(p) => {
            passthrough("item/reasoning/summaryPartAdded", &p)
        }
        Notification::ReasoningTextDelta(p) => passthrough("item/reasoning/textDelta", &p),
        Notification::FileChangePatchUpdated(p) => passthrough("item/fileChange/patchUpdated", &p),
        Notification::ContextCompacted(p) => passthrough("thread/compacted", &p),

        // Everything else (status notifications, deltas, realtime, hooks,
        // unknown) is not user-facing — the handler logs at debug and emits
        // nothing.
        _ => vec![AgentOutput::Noop],
    }
}

fn classify_request(request_id: String, request: ServerRequest) -> Vec<AgentOutput> {
    match request {
        ServerRequest::CmdExecApproval(p) => permission(
            request_id,
            CodexPermissionInput::Bash {
                command: p.command.unwrap_or_else(|| "(unknown)".to_string()),
                cwd: p.cwd.map(|c| c.0).unwrap_or_default(),
                parsed_cmd: None,
            },
        ),

        ServerRequest::FileChangeApproval(p) => permission(
            request_id,
            CodexPermissionInput::FileChange {
                item_id: p.item_id,
                paths: vec![],
                reason: p.reason,
                grant_root: p.grant_root,
            },
        ),

        ServerRequest::ExecCommandApproval(p) => {
            let command = if p.command.is_empty() {
                "(unknown)".to_string()
            } else {
                p.command.join(" ")
            };
            let parsed_cmd = (!p.parsed_cmd.is_empty()).then_some(p.parsed_cmd);
            permission(
                request_id,
                CodexPermissionInput::ExecCommand {
                    command,
                    cwd: p.cwd,
                    parsed_cmd,
                },
            )
        }

        ServerRequest::ApplyPatchApproval(p) => permission(
            request_id,
            CodexPermissionInput::ApplyPatch {
                file_changes: p.file_changes,
                grant_root: p.grant_root,
                reason: p.reason,
            },
        ),

        ServerRequest::PermissionsRequestApproval(p) => permission(
            request_id,
            CodexPermissionInput::Permissions {
                cwd: Some(p.cwd.0),
                permissions: Some(p.permissions),
                reason: p.reason,
            },
        ),

        ServerRequest::ToolRequestUserInput(p) => permission(
            request_id,
            CodexPermissionInput::AskUserQuestion {
                questions: p.questions,
            },
        ),

        ServerRequest::McpServerElicitationRequest(_p) => permission(
            request_id,
            // Preserve the existing "(unknown)" default — the typed params
            // expose no server identifier (matches handler.rs).
            CodexPermissionInput::McpElicitation {
                server_name: "(unknown)".to_string(),
            },
        ),

        // Internal / system requests — surface a portal message so the user
        // sees what was requested; we can't meaningfully auto-respond.
        ServerRequest::ItemToolCall(p) => {
            let tool = if p.tool.is_empty() {
                "(unknown)"
            } else {
                &p.tool
            };
            let event =
                shared::PortalMessage::text(format!("**Codex tool call**: `{}`", tool)).to_json();
            vec![AgentOutput::Visible(event)]
        }
        ServerRequest::ChatgptAuthTokensRefresh(_p) => {
            let event = shared::PortalMessage::text(
                "**Codex requested ChatGPT auth token refresh** (not handled — the agent may pause)."
                    .to_string(),
            )
            .to_json();
            vec![AgentOutput::Visible(event)]
        }
        ServerRequest::AttestationGenerate(_p) => {
            let event = shared::PortalMessage::text(
                "**Codex requested attestation generation** (not handled).".to_string(),
            )
            .to_json();
            vec![AgentOutput::Visible(event)]
        }

        ServerRequest::Unknown { method, .. } => {
            tracing::warn!("Unknown Codex request: {}", method);
            vec![AgentOutput::Noop]
        }
    }
}

/// Build a neutral `PermissionRequest` from a typed Codex permission input,
/// matching what `Session`'s neutral `PermissionRequest` handling expects:
/// `tool_name` from the typed variant, `input` re-serialized to JSON, no
/// suggestions (codex carries none).
fn permission(request_id: String, input: CodexPermissionInput) -> Vec<AgentOutput> {
    let tool_name = input.tool_name().to_string();
    let input_value = serde_json::to_value(&input).unwrap_or(serde_json::Value::Null);
    vec![AgentOutput::PermissionRequest {
        request_id,
        tool_name,
        input: input_value,
        suggestions: vec![],
    }]
}

/// `{"type": method, "params": <serialized p>}` passthrough for streaming/delta
/// notifications without a purpose-built renderer.
fn passthrough<P: serde::Serialize>(method: &str, p: &P) -> Vec<AgentOutput> {
    let params = serde_json::to_value(p).unwrap_or(serde_json::Value::Null);
    vec![AgentOutput::Visible(to_raw_output(&PassthroughEvent::new(
        method, params,
    )))]
}

#[cfg(test)]
mod tests {
    //! Ported from `handler.rs`'s suite, asserting the same JSON shapes — but
    //! against `AgentOutput` instead of `IoEvent`. Construct typed param structs
    //! from JSON via `serde_json::from_value`, then classify the typed
    //! `ServerMessage` directly (the form the I/O task hands to `classify`).
    use super::*;
    use codex_codes::{Notification, RequestId, ServerMessage, ServerRequest};
    use serde_json::json;

    fn classify(msg: ServerMessage) -> Vec<AgentOutput> {
        CodexClassifier.classify(msg)
    }

    #[test]
    fn item_started_is_visible_item_event() {
        let notif: codex_codes::ItemStartedNotification = serde_json::from_value(json!({
            "item": { "type": "userMessage", "id": "i1", "content": [] },
            "startedAtMs": 0,
            "threadId": "t",
            "turnId": "tu"
        }))
        .unwrap();
        let out = classify(ServerMessage::Notification(Notification::ItemStarted(
            notif,
        )));
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentOutput::Visible(v) => {
                assert_eq!(v["type"], "item.started");
                assert!(v.get("item").is_some());
            }
            other => panic!("expected Visible, got {other:?}"),
        }
    }

    #[test]
    fn error_notification_uses_typed_message() {
        let notif: codex_codes::ErrorNotification = serde_json::from_value(json!({
            "error": { "message": "model unavailable" },
            "threadId": "t",
            "turnId": "tu",
            "willRetry": false
        }))
        .unwrap();
        let out = classify(ServerMessage::Notification(Notification::Error(notif)));
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentOutput::Visible(v) => {
                assert_eq!(v["type"], "error");
                assert_eq!(v["message"], "model unavailable");
            }
            other => panic!("expected Visible, got {other:?}"),
        }
    }

    #[test]
    fn context_compacted_is_passthrough() {
        let notif: codex_codes::ContextCompactedNotification = serde_json::from_value(json!({
            "threadId": "thread-1",
            "turnId": "turn-1"
        }))
        .unwrap();
        let out = classify(ServerMessage::Notification(Notification::ContextCompacted(
            notif,
        )));
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentOutput::Visible(v) => {
                assert_eq!(v["type"], "thread/compacted");
                assert_eq!(v["params"]["threadId"], "thread-1");
                assert_eq!(v["params"]["turnId"], "turn-1");
            }
            other => panic!("expected Visible, got {other:?}"),
        }
    }

    #[test]
    fn turn_completed_is_noop_owned_by_io_task() {
        let notif: codex_codes::TurnCompletedNotification = serde_json::from_value(json!({
            "threadId": "t",
            "turn": { "id": "tu1", "items": [], "status": "completed" }
        }))
        .unwrap();
        let out = classify(ServerMessage::Notification(Notification::TurnCompleted(
            notif,
        )));
        assert_eq!(out, vec![AgentOutput::Noop]);
    }

    #[test]
    fn file_change_approval_is_permission_request() {
        let req: codex_codes::FileChangeRequestApprovalParams = serde_json::from_value(json!({
            "itemId": "item-1",
            "reason": "writes /etc/passwd",
            "threadId": "t1",
            "turnId": "tu1",
            "startedAtMs": 0
        }))
        .unwrap();
        let out = classify(ServerMessage::Request {
            id: RequestId::Integer(7),
            request: ServerRequest::FileChangeApproval(req),
        });
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentOutput::PermissionRequest {
                request_id,
                tool_name,
                input,
                suggestions,
            } => {
                assert_eq!(request_id, "7");
                assert_eq!(tool_name, "FileChange");
                assert_eq!(input["tool"], "fileChange");
                assert_eq!(input["itemId"], "item-1");
                assert_eq!(input["reason"], "writes /etc/passwd");
                assert!(suggestions.is_empty());
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn exec_command_approval_joins_argv() {
        let req: codex_codes::ExecCommandApprovalParams = serde_json::from_value(json!({
            "callId": "call-1",
            "conversationId": "conv-1",
            "command": ["ls", "-la", "/tmp"],
            "cwd": "/home/user",
            "parsedCmd": []
        }))
        .unwrap();
        let out = classify(ServerMessage::Request {
            id: RequestId::String("abc".to_string()),
            request: ServerRequest::ExecCommandApproval(req),
        });
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentOutput::PermissionRequest {
                request_id,
                tool_name,
                input,
                ..
            } => {
                assert_eq!(request_id, "abc");
                assert_eq!(tool_name, "ExecCommand");
                assert_eq!(input["command"], "ls -la /tmp");
                assert_eq!(input["cwd"], "/home/user");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn cmd_exec_approval_unwraps_options() {
        let req: codex_codes::CommandExecutionRequestApprovalParams =
            serde_json::from_value(json!({
                "itemId": "item-2",
                "command": "echo hello",
                "cwd": "/work",
                "startedAtMs": 0,
                "threadId": "t",
                "turnId": "tu"
            }))
            .unwrap();
        let out = classify(ServerMessage::Request {
            id: RequestId::Integer(1),
            request: ServerRequest::CmdExecApproval(req),
        });
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentOutput::PermissionRequest {
                tool_name, input, ..
            } => {
                assert_eq!(tool_name, "Bash");
                assert_eq!(input["command"], "echo hello");
                assert_eq!(input["cwd"], "/work");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn apply_patch_approval_preserves_file_changes_map() {
        let req: codex_codes::ApplyPatchApprovalParams = serde_json::from_value(json!({
            "callId": "c",
            "conversationId": "cv",
            "fileChanges": {
                "/tmp/a.rs": { "type": "add", "content": "fn a() {}" },
                "/tmp/b.rs": { "type": "delete", "content": "old contents" }
            },
            "reason": "tidy up"
        }))
        .unwrap();
        let out = classify(ServerMessage::Request {
            id: RequestId::Integer(2),
            request: ServerRequest::ApplyPatchApproval(req),
        });
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentOutput::PermissionRequest {
                tool_name, input, ..
            } => {
                assert_eq!(tool_name, "ApplyPatch");
                let keys: Vec<&str> = input["fileChanges"]
                    .as_object()
                    .map(|m| m.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                assert!(keys.contains(&"/tmp/a.rs"));
                assert!(keys.contains(&"/tmp/b.rs"));
                assert_eq!(input["reason"], "tidy up");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn mcp_elicitation_preserves_unknown_default() {
        let req: codex_codes::McpServerElicitationRequestParams = serde_json::from_value(json!({
            "mode": "url",
            "elicitationId": "e1",
            "message": "please auth",
            "url": "https://example.com/auth"
        }))
        .unwrap();
        let out = classify(ServerMessage::Request {
            id: RequestId::Integer(3),
            request: ServerRequest::McpServerElicitationRequest(req),
        });
        assert_eq!(out.len(), 1);
        match &out[0] {
            AgentOutput::PermissionRequest {
                tool_name, input, ..
            } => {
                assert_eq!(tool_name, "McpElicitation");
                assert_eq!(input["serverName"], "(unknown)");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }
}
