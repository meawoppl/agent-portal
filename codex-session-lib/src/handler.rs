//! Translate Codex app-server `ServerMessage` frames into [`IoEvent`]s.
//!
//! Dispatches on the typed `codex_codes::Notification` and
//! `codex_codes::ServerRequest` enum variants instead of stringly-typed
//! `method`-based matching (closes issue #723). Each `ServerRequest::*Approval`
//! arm now builds a typed [`shared::CodexPermissionInput`] envelope directly
//! off the SDK param struct (closes #725 / #731), so the field-name contract
//! with the frontend's `format_permission_input` is enforced at compile time
//! on both sides of the wire — the `serde_json::json!({…})` literals the
//! 2.5.45 / PR #730 refactor still emitted are gone.

use codex_codes::{Notification, ServerMessage, ServerRequest};
use session_lib::io::IoEvent;
use shared::CodexPermissionInput;
use tokio::sync::mpsc;

use crate::helpers::format_request_id;

/// Convert a Codex app-server ServerMessage into exec-format JSONL events.
/// Returns (event_sent_ok, turn_ended).
pub(crate) fn handle_codex_server_message(
    msg: ServerMessage,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
    latest_token_usage: Option<&serde_json::Value>,
) -> (bool, bool) {
    match msg {
        ServerMessage::Notification(notif) => {
            handle_notification(notif, event_tx, latest_token_usage)
        }
        ServerMessage::Request { id, request } => {
            handle_request(format_request_id(&id), request, event_tx)
        }
    }
}

fn handle_notification(
    notif: Notification,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
    latest_token_usage: Option<&serde_json::Value>,
) -> (bool, bool) {
    match notif {
        // Lifecycle — already handled elsewhere or intentionally silent.
        Notification::ThreadStarted(_)
        | Notification::ThreadStatusChanged(_)
        | Notification::TurnStarted(_)
        | Notification::ThreadTokenUsageUpdated(_) => (true, false),

        Notification::TurnCompleted(p) => {
            let status = serde_json::to_value(&p.turn.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string));
            let event = serde_json::json!({
                "type": "turn.completed",
                "turn_id": p.turn.id,
                "status": status,
                "duration_ms": p.turn.duration_ms,
                "usage": latest_token_usage.cloned(),
            });
            let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
            (ok, true)
        }

        Notification::ItemStarted(p) => {
            let item = serde_json::to_value(&p.item).unwrap_or(serde_json::Value::Null);
            let event = serde_json::json!({
                "type": "item.started",
                "item": item,
            });
            let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
            (ok, false)
        }

        Notification::ItemCompleted(p) => {
            let item = serde_json::to_value(&p.item).unwrap_or(serde_json::Value::Null);
            let event = serde_json::json!({
                "type": "item.completed",
                "item": item,
            });
            let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
            (ok, false)
        }

        Notification::Error(p) => {
            // Existing handler emitted `{"type": "error", "message": <string>}`.
            // `ErrorNotification.error: TurnError` has its own `message` field.
            let message = if p.error.message.is_empty() {
                "Unknown error".to_string()
            } else {
                p.error.message.clone()
            };
            let event = serde_json::json!({
                "type": "error",
                "message": message,
            });
            let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
            (ok, false)
        }

        Notification::DeprecationNotice(p) => {
            // Typed struct exposes `summary` (and optional `details`); the
            // pre-refactor handler looked for `message`/`notice`, which never
            // existed on the wire — so the user always saw "(no message)".
            // With the typed field we get the real text.
            let summary = if p.summary.is_empty() {
                "(no message)".to_string()
            } else {
                p.summary.clone()
            };
            let event =
                shared::PortalMessage::text(format!("**Codex deprecation notice**: {}", summary))
                    .to_json();
            let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
            (ok, false)
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
            let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
            (ok, false)
        }

        // Streaming/plan/diff notifications — emit a typed event so the frontend
        // can render them as a delta block. Frontend currently falls through to
        // raw display for unknown types; that's tolerable until purpose-built
        // renderers land.
        Notification::PlanDelta(p) => emit_passthrough(event_tx, "item/plan/delta", &p),
        Notification::TurnPlanUpdated(p) => emit_passthrough(event_tx, "turn/plan/updated", &p),
        Notification::TurnDiffUpdated(p) => emit_passthrough(event_tx, "turn/diff/updated", &p),
        Notification::ReasoningSummaryPartAdded(p) => {
            emit_passthrough(event_tx, "item/reasoning/summaryPartAdded", &p)
        }
        Notification::ReasoningTextDelta(p) => {
            emit_passthrough(event_tx, "item/reasoning/textDelta", &p)
        }
        Notification::FileChangePatchUpdated(p) => {
            emit_passthrough(event_tx, "item/fileChange/patchUpdated", &p)
        }
        Notification::ContextCompacted(p) => emit_passthrough(event_tx, "thread/compacted", &p),

        // Pure-status notifications — not user-facing. Logged via the typed
        // `Notification::method()` accessor so the debug message stays accurate
        // when the SDK adds variants.
        other @ (Notification::McpServerOauthLoginCompleted(_)
        | Notification::AccountLoginCompleted(_)
        | Notification::AccountRateLimitsUpdated(_)
        | Notification::McpServerStartupStatusUpdated(_)
        | Notification::RemoteControlStatusChanged(_)) => {
            tracing::debug!("Codex status notification: {}", other.method());
            (true, false)
        }

        // Everything else: keep at debug. Includes delta notifications, realtime
        // audio frames, hook/process events, etc. — adding purpose-built rendering
        // for each is out of scope here.
        other @ (Notification::AgentMessageDelta(_)
        | Notification::CmdOutputDelta(_)
        | Notification::FileChangeOutputDelta(_)
        | Notification::ReasoningDelta(_)
        | Notification::Warning(_)
        | Notification::ThreadArchived(_)
        | Notification::ThreadClosed(_)
        | Notification::ThreadUnarchived(_)
        | Notification::ThreadGoalCleared(_)
        | Notification::ThreadNameUpdated(_)
        | Notification::SkillsChanged(_)
        | Notification::FsChanged(_)
        | Notification::ConfigWarning(_)
        | Notification::AccountUpdated(_)
        | Notification::AppListUpdated(_)
        | Notification::CommandExecOutputDelta(_)
        | Notification::ExternalAgentConfigImportCompleted(_)
        | Notification::FuzzyFileSearchSessionCompleted(_)
        | Notification::FuzzyFileSearchSessionUpdated(_)
        | Notification::HookCompleted(_)
        | Notification::HookStarted(_)
        | Notification::ItemGuardianApprovalReviewCompleted(_)
        | Notification::ItemGuardianApprovalReviewStarted(_)
        | Notification::TerminalInteraction(_)
        | Notification::McpToolCallProgress(_)
        | Notification::ModelRerouted(_)
        | Notification::ModelVerification(_)
        | Notification::ProcessExited(_)
        | Notification::ProcessOutputDelta(_)
        | Notification::ServerRequestResolved(_)
        | Notification::ThreadGoalUpdated(_)
        | Notification::ThreadRealtimeClosed(_)
        | Notification::ThreadRealtimeError(_)
        | Notification::ThreadRealtimeItemAdded(_)
        | Notification::ThreadRealtimeOutputAudioDelta(_)
        | Notification::ThreadRealtimeSdp(_)
        | Notification::ThreadRealtimeStarted(_)
        | Notification::ThreadRealtimeTranscriptDelta(_)
        | Notification::ThreadRealtimeTranscriptDone(_)
        | Notification::WindowsWorldWritableWarning(_)
        | Notification::WindowsSandboxSetupCompleted(_)) => {
            tracing::debug!("Codex notification: {}", other.method());
            (true, false)
        }

        Notification::Unknown { method, .. } => {
            tracing::debug!("Codex notification: {}", method);
            (true, false)
        }
    }
}

/// Helper: emit `{"type": method, "params": <serialized p>}` as a passthrough
/// RawOutput event for streaming/delta notifications that don't yet have a
/// purpose-built renderer.
fn emit_passthrough<P: serde::Serialize>(
    event_tx: &mpsc::UnboundedSender<IoEvent>,
    method: &str,
    p: &P,
) -> (bool, bool) {
    let params = serde_json::to_value(p).unwrap_or(serde_json::Value::Null);
    let event = serde_json::json!({
        "type": method,
        "params": params,
    });
    let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
    (ok, false)
}

fn handle_request(
    request_id: String,
    request: ServerRequest,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) -> (bool, bool) {
    match request {
        ServerRequest::CmdExecApproval(p) => {
            // Older `item/commandExecution/requestApproval` form carries a
            // single-string `command` (vs the 0.130+ argv-vector form on
            // `execCommandApproval`); no `parsedCmd` here.
            let input = CodexPermissionInput::Bash {
                command: p.command.unwrap_or_else(|| "(unknown)".to_string()),
                cwd: p.cwd.map(|c| c.0).unwrap_or_default(),
                parsed_cmd: None,
            };
            send_permission(event_tx, request_id, input)
        }

        ServerRequest::FileChangeApproval(p) => {
            // codex-codes 0.129.3 dropped the inline `changes` field — the
            // actual diff is streamed earlier under the matching `itemId`.
            // Carry `itemId` / `reason` / `grantRoot` so the frontend can
            // render a pointer + reason instead of the 2.5.38 `{"changes":
            // null}` regression (band-aided in PR #721).
            let input = CodexPermissionInput::FileChange {
                item_id: p.item_id,
                reason: p.reason,
                grant_root: p.grant_root,
            };
            send_permission(event_tx, request_id, input)
        }

        ServerRequest::ExecCommandApproval(p) => {
            // Codex 0.130+ argv-vector form — join for display, keep the
            // typed parsed_cmd along for future renderers.
            let command = if p.command.is_empty() {
                "(unknown)".to_string()
            } else {
                p.command.join(" ")
            };
            let parsed_cmd = if p.parsed_cmd.is_empty() {
                None
            } else {
                Some(serde_json::to_value(&p.parsed_cmd).unwrap_or(serde_json::Value::Null))
            };
            let input = CodexPermissionInput::ExecCommand {
                command,
                cwd: p.cwd,
                parsed_cmd,
            };
            send_permission(event_tx, request_id, input)
        }

        ServerRequest::ApplyPatchApproval(p) => {
            let file_changes =
                serde_json::to_value(&p.file_changes).unwrap_or(serde_json::Value::Null);
            let input = CodexPermissionInput::ApplyPatch {
                file_changes,
                grant_root: p.grant_root,
                reason: p.reason,
            };
            send_permission(event_tx, request_id, input)
        }

        ServerRequest::PermissionsRequestApproval(p) => {
            let permissions = serde_json::to_value(&p.permissions).ok();
            let input = CodexPermissionInput::Permissions {
                cwd: Some(p.cwd.0),
                permissions,
                reason: p.reason,
            };
            send_permission(event_tx, request_id, input)
        }

        ServerRequest::ToolRequestUserInput(p) => {
            let questions = serde_json::to_value(&p.questions).unwrap_or(serde_json::Value::Null);
            let input = CodexPermissionInput::AskUserQuestion { questions };
            send_permission(event_tx, request_id, input)
        }

        ServerRequest::McpServerElicitationRequest(_p) => {
            // Pre-refactor handler hard-defaulted `serverName` to `"(unknown)"`
            // because the field never existed on the wire. The typed
            // `McpServerElicitationRequestParams` enum (Form/Url variants)
            // doesn't expose a server name either; preserve the existing
            // default so `format_permission_input`'s "MCP server `(unknown)`
            // is asking …" rendering is unchanged. TODO(SDK): if upstream
            // adds a server identifier to the typed params, surface it here
            // and widen `CodexPermissionInput::McpElicitation` to match.
            let input = CodexPermissionInput::McpElicitation {
                server_name: "(unknown)".to_string(),
            };
            send_permission(event_tx, request_id, input)
        }

        // Internal / system requests (codex 0.130+) — surface a portal message so
        // the user sees what was requested, but don't block on user approval. We
        // can't auto-respond meaningfully (we have no auth token, no attestation
        // signer); codex will retry or move on.
        ServerRequest::ItemToolCall(p) => {
            let tool = if p.tool.is_empty() {
                "(unknown)"
            } else {
                &p.tool
            };
            let msg = format!("**Codex tool call**: `{}`", tool);
            let event = shared::PortalMessage::text(msg).to_json();
            let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
            (ok, false)
        }
        ServerRequest::ChatgptAuthTokensRefresh(_p) => {
            let event = shared::PortalMessage::text(
                "**Codex requested ChatGPT auth token refresh** (not handled — the agent may pause)."
                    .to_string(),
            )
            .to_json();
            let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
            (ok, false)
        }
        ServerRequest::AttestationGenerate(_p) => {
            let event = shared::PortalMessage::text(
                "**Codex requested attestation generation** (not handled).".to_string(),
            )
            .to_json();
            let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
            (ok, false)
        }

        ServerRequest::Unknown { method, .. } => {
            tracing::warn!("Unknown Codex request: {}", method);
            (true, false)
        }
    }
}

fn send_permission(
    event_tx: &mpsc::UnboundedSender<IoEvent>,
    request_id: String,
    input: CodexPermissionInput,
) -> (bool, bool) {
    let ok = event_tx
        .send(IoEvent::CodexPermissionRequest { request_id, input })
        .is_ok();
    (ok, false)
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
        latest_token_usage: Option<&serde_json::Value>,
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
        let usage = json!({
            "last": {
                "inputTokens": 100,
                "cachedInputTokens": 25,
                "outputTokens": 40,
                "reasoningOutputTokens": 7,
                "totalTokens": 147
            },
            "total": {
                "inputTokens": 300,
                "cachedInputTokens": 75,
                "outputTokens": 90,
                "reasoningOutputTokens": 17,
                "totalTokens": 407
            },
            "model_context_window": 200000
        });
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
