//! Translate Codex app-server `ServerMessage` frames into [`IoEvent`]s.
//!
//! The `ServerMessage` → neutral-output mapping now lives in
//! [`CodexClassifier`](crate::classifier::CodexClassifier) (the single source
//! of Codex output classification, #1165 item 2). This module is the thin
//! adapter that drives the classifier from `codex_io_task` and translates its
//! neutral [`AgentOutput`]s back into the [`IoEvent`]s the I/O task forwards
//! (`Visible` → `RawOutput`, `PermissionRequest` → `CodexPermissionRequest`).
//!
//! It also owns the one carve-out the classifier deliberately skips:
//! `TurnCompleted`. That frame carries token usage and drives per-turn metrics
//! finalization — turn/token orchestration owned by `codex_io_task` — so the
//! `turn.completed` event (with usage) is shaped here, and `turn_ended` is
//! returned to the I/O task's turn-lifecycle loop.

use codex_codes::{Notification, ServerMessage};
use session_lib::io::IoEvent;
use session_lib::{AgentOutput, AgentOutputClassifier};
use shared::CodexPermissionInput;
use tokio::sync::mpsc;

use crate::classifier::CodexClassifier;
use crate::events::{to_raw_output, CodexUsageEvent, TurnCompletedEvent};

/// Convert a Codex app-server ServerMessage into exec-format JSONL events.
/// Returns (event_sent_ok, turn_ended).
pub(crate) fn handle_codex_server_message(
    msg: ServerMessage,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
    latest_token_usage: Option<&CodexUsageEvent>,
) -> (bool, bool) {
    // Turn completion stays here: it carries token usage + drives per-turn
    // metrics, which is turn/token orchestration owned by the I/O task.
    // `CodexClassifier` deliberately returns `Noop` for it.
    if let ServerMessage::Notification(Notification::TurnCompleted(p)) = &msg {
        let event = TurnCompletedEvent::new(
            p.turn.id.clone(),
            turn_status_label(&p.turn.status).to_string(),
            p.turn.duration_ms,
            latest_token_usage.cloned(),
        );
        let ok = event_tx
            .send(IoEvent::RawOutput(to_raw_output(&event)))
            .is_ok();
        return (ok, true);
    }

    // Everything else: the classifier is the single mapping source. Translate
    // its neutral decisions back into the I/O task's transport events.
    let mut classifier = CodexClassifier;
    let mut ok = true;
    for output in classifier.classify(msg) {
        let event = match output {
            AgentOutput::Visible(value) => IoEvent::RawOutput(value),
            AgentOutput::PermissionRequest {
                request_id, input, ..
            } => {
                // Recover the typed Codex permission envelope the classifier
                // serialized (`CodexPermissionInput` round-trips through its
                // wire form). The I/O task / `Session` consume the typed event.
                match serde_json::from_value::<CodexPermissionInput>(input) {
                    Ok(input) => IoEvent::CodexPermissionRequest { request_id, input },
                    Err(e) => {
                        tracing::error!("Codex permission input round-trip failed: {}", e);
                        continue;
                    }
                }
            }
            // Codex never produces `NotFound`; `Noop` is an internal skip.
            AgentOutput::Noop | AgentOutput::NotFound => continue,
        };
        if event_tx.send(event).is_err() {
            ok = false;
        }
    }
    (ok, false)
}

fn turn_status_label(status: &codex_codes::TurnStatus) -> &'static str {
    match status {
        codex_codes::TurnStatus::Completed => "completed",
        codex_codes::TurnStatus::Interrupted => "interrupted",
        codex_codes::TurnStatus::Failed => "failed",
        codex_codes::TurnStatus::InProgress => "in_progress",
    }
}

#[cfg(test)]
mod tests {
    //! Verify that the typed dispatch produces the same JSON shape the frontend
    //! expects (preserves `IoEvent::CodexPermissionRequest::input` field names).
    //! Construct typed param structs from JSON via `serde_json::from_value` so
    //! the tests don't have to chase per-field defaults.
    use super::*;
    use codex_codes::{Notification, RequestId, ServerMessage, ServerRequest};
    use serde_json::json;

    fn drain(rx: &mut mpsc::UnboundedReceiver<IoEvent>) -> Vec<IoEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
        }
        out
    }

    fn handle_with_usage(
        msg: ServerMessage,
        latest_token_usage: Option<&CodexUsageEvent>,
    ) -> (Vec<IoEvent>, bool, bool) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (sent, ended) = handle_codex_server_message(msg, &tx, latest_token_usage);
        drop(tx);
        (drain(&mut rx), sent, ended)
    }

    fn handle(msg: ServerMessage) -> (Vec<IoEvent>, bool, bool) {
        handle_with_usage(msg, None)
    }

    #[test]
    fn context_compacted_emits_renderable_event() {
        let notif: codex_codes::ContextCompactedNotification = serde_json::from_value(json!({
            "threadId": "thread-1",
            "turnId": "turn-1"
        }))
        .unwrap();
        let msg = ServerMessage::Notification(Notification::ContextCompacted(notif));
        let (events, sent, ended) = handle(msg);

        assert!(sent);
        assert!(!ended);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::RawOutput(value) => {
                assert_eq!(value["type"], "thread/compacted");
                assert_eq!(value["params"]["threadId"], "thread-1");
                assert_eq!(value["params"]["turnId"], "turn-1");
            }
            _ => panic!("expected RawOutput"),
        }
    }

    #[test]
    fn turn_completed_emits_duration_status_and_latest_usage() {
        let notif: codex_codes::TurnCompletedNotification = serde_json::from_value(json!({
            "threadId": "thread-1",
            "turn": {
                "id": "turn-1",
                "status": "completed",
                "durationMs": 4200,
                "items": []
            }
        }))
        .unwrap();
        let usage = CodexUsageEvent {
            last: codex_codes::TokenUsageBreakdown {
                input_tokens: 100,
                cached_input_tokens: 25,
                output_tokens: 40,
                reasoning_output_tokens: 7,
                total_tokens: 147,
            },
            total: codex_codes::TokenUsageBreakdown {
                input_tokens: 300,
                cached_input_tokens: 75,
                output_tokens: 90,
                reasoning_output_tokens: 17,
                total_tokens: 407,
            },
            model_context_window: Some(200000),
        };
        let msg = ServerMessage::Notification(Notification::TurnCompleted(notif));
        let (events, sent, ended) = handle_with_usage(msg, Some(&usage));

        assert!(sent);
        assert!(ended);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::RawOutput(value) => {
                assert_eq!(value["type"], "turn.completed");
                assert_eq!(value["turn_id"], "turn-1");
                assert_eq!(value["status"], "completed");
                assert_eq!(value["duration_ms"], 4200);
                assert_eq!(value["usage"]["last"]["inputTokens"], 100);
                assert_eq!(value["usage"]["model_context_window"], 200000);
            }
            _ => panic!("expected RawOutput"),
        }
    }

    /// `FileChangeApproval` should now expose the typed `itemId`/`reason`/`grantRoot`
    /// fields rather than a stale `changes: null` (the issue #723 / PR #721 bug).
    #[test]
    fn file_change_approval_emits_typed_fields() {
        let req: codex_codes::FileChangeRequestApprovalParams = serde_json::from_value(json!({
            "itemId": "item-1",
            "reason": "writes /etc/passwd",
            "threadId": "t1",
            "turnId": "tu1",
            "startedAtMs": 0
        }))
        .unwrap();
        let msg = ServerMessage::Request {
            id: RequestId::Integer(7),
            request: ServerRequest::FileChangeApproval(req),
        };
        let (events, _, ended) = handle(msg);
        assert!(!ended);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::CodexPermissionRequest { request_id, input } => {
                assert_eq!(request_id, "7");
                assert_eq!(input.tool_name(), "FileChange");
                match input {
                    CodexPermissionInput::FileChange {
                        item_id,
                        reason,
                        grant_root,
                    } => {
                        assert_eq!(item_id, "item-1");
                        assert_eq!(reason.as_deref(), Some("writes /etc/passwd"));
                        // grantRoot omitted in source JSON → None
                        assert!(grant_root.is_none());
                    }
                    _ => panic!("expected FileChange variant"),
                }
            }
            _ => panic!("expected CodexPermissionRequest"),
        }
    }

    /// `ExecCommandApproval` joins the argv `Vec<String>` into a single string
    /// for the frontend, and preserves `cwd` + `parsedCmd`.
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
        let msg = ServerMessage::Request {
            id: RequestId::String("abc".to_string()),
            request: ServerRequest::ExecCommandApproval(req),
        };
        let (events, _, _) = handle(msg);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::CodexPermissionRequest { request_id, input } => {
                assert_eq!(request_id, "abc");
                assert_eq!(input.tool_name(), "ExecCommand");
                match input {
                    CodexPermissionInput::ExecCommand {
                        command,
                        cwd,
                        parsed_cmd,
                    } => {
                        assert_eq!(command, "ls -la /tmp");
                        assert_eq!(cwd, "/home/user");
                        // empty parsed_cmd in source → None (skipped on serialize)
                        assert!(parsed_cmd.is_none());
                    }
                    _ => panic!("expected ExecCommand variant"),
                }
            }
            _ => panic!("expected CodexPermissionRequest"),
        }
    }

    /// `CmdExecApproval` unwraps `Option<String>` command / `Option<AbsolutePathBuf>` cwd.
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
        let msg = ServerMessage::Request {
            id: RequestId::Integer(1),
            request: ServerRequest::CmdExecApproval(req),
        };
        let (events, _, _) = handle(msg);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::CodexPermissionRequest { input, .. } => {
                assert_eq!(input.tool_name(), "Bash");
                match input {
                    CodexPermissionInput::Bash { command, cwd, .. } => {
                        assert_eq!(command, "echo hello");
                        assert_eq!(cwd, "/work");
                    }
                    _ => panic!("expected Bash variant"),
                }
            }
            _ => panic!("expected CodexPermissionRequest"),
        }
    }

    /// `ApplyPatchApproval` should preserve the `fileChanges` map keyed by path
    /// (the frontend's `format_permission_input` reads these keys).
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
        let msg = ServerMessage::Request {
            id: RequestId::Integer(2),
            request: ServerRequest::ApplyPatchApproval(req),
        };
        let (events, _, _) = handle(msg);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::CodexPermissionRequest { input, .. } => {
                assert_eq!(input.tool_name(), "ApplyPatch");
                match input {
                    CodexPermissionInput::ApplyPatch {
                        file_changes,
                        reason,
                        ..
                    } => {
                        let keys: Vec<&str> = file_changes
                            .as_object()
                            .map(|m| m.keys().map(String::as_str).collect())
                            .unwrap_or_default();
                        assert!(keys.contains(&"/tmp/a.rs"));
                        assert!(keys.contains(&"/tmp/b.rs"));
                        assert_eq!(reason.as_deref(), Some("tidy up"));
                    }
                    _ => panic!("expected ApplyPatch variant"),
                }
            }
            _ => panic!("expected CodexPermissionRequest"),
        }
    }

    /// `TurnCompleted` signals turn end and emits a `turn.completed` raw event.
    #[test]
    fn turn_completed_ends_turn() {
        let notif: codex_codes::TurnCompletedNotification = serde_json::from_value(json!({
            "threadId": "t",
            "turn": {
                "id": "tu1",
                "items": [],
                "status": "completed"
            }
        }))
        .unwrap();
        let msg = ServerMessage::Notification(Notification::TurnCompleted(notif));
        let (events, _, ended) = handle(msg);
        assert!(ended, "TurnCompleted must signal turn_ended=true");
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::RawOutput(v) => {
                assert_eq!(
                    v.get("type").and_then(|t| t.as_str()),
                    Some("turn.completed")
                );
            }
            _ => panic!("expected RawOutput"),
        }
    }

    /// `Error` notification extracts `error.message` from the typed `TurnError`.
    #[test]
    fn error_notification_uses_typed_message() {
        let notif: codex_codes::ErrorNotification = serde_json::from_value(json!({
            "error": { "message": "model unavailable" },
            "threadId": "t",
            "turnId": "tu",
            "willRetry": false
        }))
        .unwrap();
        let msg = ServerMessage::Notification(Notification::Error(notif));
        let (events, _, _) = handle(msg);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::RawOutput(v) => {
                assert_eq!(v.get("type").and_then(|t| t.as_str()), Some("error"));
                assert_eq!(
                    v.get("message").and_then(|m| m.as_str()),
                    Some("model unavailable")
                );
            }
            _ => panic!("expected RawOutput"),
        }
    }

    /// `ItemStarted` re-serializes the typed `ThreadItem` back into the raw event.
    #[test]
    fn item_started_emits_item() {
        let notif: codex_codes::ItemStartedNotification = serde_json::from_value(json!({
            "item": { "type": "userMessage", "id": "i1", "content": [] },
            "startedAtMs": 0,
            "threadId": "t",
            "turnId": "tu"
        }))
        .unwrap();
        let msg = ServerMessage::Notification(Notification::ItemStarted(notif));
        let (events, _, _) = handle(msg);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::RawOutput(v) => {
                assert_eq!(v.get("type").and_then(|t| t.as_str()), Some("item.started"));
                assert!(v.get("item").is_some());
            }
            _ => panic!("expected RawOutput"),
        }
    }

    /// Pre-refactor handler hard-defaulted `serverName` to `"(unknown)"` for
    /// MCP elicitation requests because the field never existed on the wire.
    /// Preserve that frontend-facing string so `format_permission_input`'s
    /// "MCP server `(unknown)` is asking …" rendering is unchanged.
    #[test]
    fn mcp_elicitation_preserves_unknown_default() {
        let req: codex_codes::McpServerElicitationRequestParams = serde_json::from_value(json!({
            "mode": "url",
            "elicitationId": "e1",
            "message": "please auth",
            "url": "https://example.com/auth"
        }))
        .unwrap();
        let msg = ServerMessage::Request {
            id: RequestId::Integer(3),
            request: ServerRequest::McpServerElicitationRequest(req),
        };
        let (events, _, _) = handle(msg);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::CodexPermissionRequest { input, .. } => {
                assert_eq!(input.tool_name(), "McpElicitation");
                match input {
                    CodexPermissionInput::McpElicitation { server_name } => {
                        assert_eq!(server_name, "(unknown)");
                    }
                    _ => panic!("expected McpElicitation variant"),
                }
            }
            _ => panic!("expected CodexPermissionRequest"),
        }
    }

    /// The wire envelope serialized from a `CodexPermissionInput` must include
    /// the `tool` discriminant so the frontend's typed round-trip works (#731).
    /// Closes the proxy → frontend type contract end-to-end.
    #[test]
    fn permission_input_serializes_with_tool_discriminant() {
        let req: codex_codes::FileChangeRequestApprovalParams = serde_json::from_value(json!({
            "itemId": "item-x",
            "threadId": "t",
            "turnId": "tu",
            "startedAtMs": 0
        }))
        .unwrap();
        let msg = ServerMessage::Request {
            id: RequestId::Integer(9),
            request: ServerRequest::FileChangeApproval(req),
        };
        let (events, _, _) = handle(msg);
        assert_eq!(events.len(), 1);
        match &events[0] {
            IoEvent::CodexPermissionRequest { input, .. } => {
                let wire = serde_json::to_value(input).unwrap();
                assert_eq!(wire["tool"], "fileChange");
                assert_eq!(wire["itemId"], "item-x");
                // Round-trip via wire → typed parse must succeed (this is the
                // frontend's consumption path).
                let parsed: CodexPermissionInput = serde_json::from_value(wire).unwrap();
                assert_eq!(parsed.tool_name(), "FileChange");
            }
            _ => panic!("expected CodexPermissionRequest"),
        }
    }
}
