//! Translate Codex app-server `ServerMessage` frames into [`IoEvent`]s.
//!
//! This module keeps the existing stringly-typed `match method.as_str()`
//! dispatch verbatim from the pre-split `Session::handle_codex_server_message`.
//! Issue #723 will refactor it to typed enum matching against
//! `codex_codes::ServerRequest` variants — intentionally out of scope here.

use session_lib::io::IoEvent;
use tokio::sync::mpsc;

use crate::helpers::format_request_id;

/// Convert a Codex app-server ServerMessage into exec-format JSONL events.
/// Returns (event_sent_ok, turn_ended).
pub(crate) fn handle_codex_server_message(
    msg: codex_codes::ServerMessage,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) -> (bool, bool) {
    match msg {
        codex_codes::ServerMessage::Notification(notif) => {
            let (method, params) = match notif.into_envelope() {
                Ok((m, p)) => (m, p.unwrap_or(serde_json::Value::Null)),
                Err(e) => {
                    tracing::warn!("Codex notification envelope error: {}", e);
                    return (true, false);
                }
            };

            match method.as_str() {
                "thread/started" | "turn/started" | "thread/status/changed" => {
                    // Already handled or not needed for frontend
                    (true, false)
                }
                "turn/completed" => {
                    // Extract usage from the Turn object if available
                    let usage = params
                        .get("turn")
                        .and_then(|t| t.get("usage"))
                        .or_else(|| params.get("usage"))
                        .cloned()
                        .unwrap_or(serde_json::json!(null));
                    let event = serde_json::json!({
                        "type": "turn.completed",
                        "usage": usage
                    });
                    let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
                    (ok, true)
                }
                "item/started" => {
                    if let Some(item) = params.get("item") {
                        let event = serde_json::json!({
                            "type": "item.started",
                            "item": item
                        });
                        let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
                        (ok, false)
                    } else {
                        (true, false)
                    }
                }
                "item/completed" => {
                    if let Some(item) = params.get("item") {
                        let event = serde_json::json!({
                            "type": "item.completed",
                            "item": item
                        });
                        let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
                        (ok, false)
                    } else {
                        (true, false)
                    }
                }
                "thread/tokenUsage/updated" => {
                    // Skip — usage is included in turn.completed
                    (true, false)
                }
                "error" => {
                    let message = params
                        .get("error")
                        .and_then(|v| v.as_str())
                        .or_else(|| params.get("message").and_then(|v| v.as_str()))
                        .unwrap_or("Unknown error");
                    let event = serde_json::json!({
                        "type": "error",
                        "message": message
                    });
                    let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
                    (ok, false)
                }
                // Codex 0.130+ notifications — high-visibility ones surface as portal text
                // so the user sees them; streaming-delta ones get a structured event the
                // frontend can opt into rendering; status / lifecycle ones stay silent.
                "deprecationNotice" => {
                    let message = params
                        .get("message")
                        .and_then(|v| v.as_str())
                        .or_else(|| params.get("notice").and_then(|v| v.as_str()))
                        .unwrap_or("(no message)");
                    let event = shared::PortalMessage::text(format!(
                        "**Codex deprecation notice**: {}",
                        message
                    ))
                    .to_json();
                    let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
                    (ok, false)
                }
                "guardianWarning" => {
                    let message = params
                        .get("message")
                        .and_then(|v| v.as_str())
                        .or_else(|| params.get("warning").and_then(|v| v.as_str()))
                        .unwrap_or("(no message)");
                    let event = shared::PortalMessage::text(format!(
                        "**Codex guardian warning**: {}",
                        message
                    ))
                    .to_json();
                    let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
                    (ok, false)
                }
                // Streaming/plan/diff notifications — emit a typed event so the frontend
                // can render them as a delta block. Frontend currently falls through to
                // raw display for unknown types; that's tolerable until purpose-built
                // renderers land.
                "item/plan/delta"
                | "turn/plan/updated"
                | "turn/diff/updated"
                | "item/reasoning/summaryPartAdded"
                | "item/reasoning/textDelta"
                | "item/fileChange/patchUpdated" => {
                    let event = serde_json::json!({
                        "type": method,
                        "params": params,
                    });
                    let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
                    (ok, false)
                }
                // Pure-status notifications — not user-facing.
                "mcpServer/oauthLogin/completed"
                | "account/login/completed"
                | "account/rateLimits/updated"
                | "mcpServer/startupStatus/updated"
                | "remoteControl/status/changed" => {
                    tracing::debug!("Codex status notification: {}", method);
                    (true, false)
                }
                _ => {
                    // Skip delta notifications — item/completed provides the full item
                    tracing::debug!("Codex notification: {}", method);
                    (true, false)
                }
            }
        }
        codex_codes::ServerMessage::Request { id, request } => {
            let method = request.method().to_string();
            // Codex 0.129 added several new server→client request variants
            // (tool-input prompts, MCP elicitations, permission requests,
            // tool-call requests, auth-token refresh, attestation, patch
            // approval, exec approval). Until we add purpose-built UI for
            // each, serialize the typed param struct back to a Value and
            // let the downstream string-dispatch route it; unmodeled
            // methods fall through to the warn! arm and surface as a raw
            // codex frame, which is the user-visible safety net.
            let params: serde_json::Value = match &request {
                codex_codes::ServerRequest::CmdExecApproval(p) => {
                    serde_json::to_value(p).unwrap_or_default()
                }
                codex_codes::ServerRequest::FileChangeApproval(p) => {
                    serde_json::to_value(p).unwrap_or_default()
                }
                codex_codes::ServerRequest::ToolRequestUserInput(p) => {
                    serde_json::to_value(p).unwrap_or_default()
                }
                codex_codes::ServerRequest::McpServerElicitationRequest(p) => {
                    serde_json::to_value(p).unwrap_or_default()
                }
                codex_codes::ServerRequest::PermissionsRequestApproval(p) => {
                    serde_json::to_value(p).unwrap_or_default()
                }
                codex_codes::ServerRequest::ItemToolCall(p) => {
                    serde_json::to_value(p).unwrap_or_default()
                }
                codex_codes::ServerRequest::ChatgptAuthTokensRefresh(p) => {
                    serde_json::to_value(p).unwrap_or_default()
                }
                codex_codes::ServerRequest::AttestationGenerate(p) => {
                    serde_json::to_value(p).unwrap_or_default()
                }
                codex_codes::ServerRequest::ApplyPatchApproval(p) => {
                    serde_json::to_value(p).unwrap_or_default()
                }
                codex_codes::ServerRequest::ExecCommandApproval(p) => {
                    serde_json::to_value(p).unwrap_or_default()
                }
                codex_codes::ServerRequest::Unknown { params, .. } => {
                    params.clone().unwrap_or_default()
                }
            };
            let request_id_str = format_request_id(&id);

            match method.as_str() {
                "item/commandExecution/requestApproval" => {
                    let command = params
                        .get("command")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(unknown)")
                        .to_string();
                    let input = serde_json::json!({
                        "command": command,
                        "cwd": params.get("cwd").and_then(|v| v.as_str()).unwrap_or("")
                    });
                    let ok = event_tx
                        .send(IoEvent::CodexPermissionRequest {
                            request_id: request_id_str,
                            tool_name: "Bash".to_string(),
                            input,
                        })
                        .is_ok();
                    (ok, false)
                }
                "item/fileChange/requestApproval" => {
                    let input = serde_json::json!({
                        "changes": params.get("changes").cloned().unwrap_or_default()
                    });
                    let ok = event_tx
                        .send(IoEvent::CodexPermissionRequest {
                            request_id: request_id_str,
                            tool_name: "FileChange".to_string(),
                            input,
                        })
                        .is_ok();
                    (ok, false)
                }
                // Codex 0.130+ alternative exec-approval shape — `command` is now an argv vec
                // and the params carry `parsedCmd`, `approvalId`, etc.
                "execCommandApproval" => {
                    let command = params
                        .get("command")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str())
                                .collect::<Vec<_>>()
                                .join(" ")
                        })
                        .unwrap_or_else(|| "(unknown)".to_string());
                    let input = serde_json::json!({
                        "command": command,
                        "cwd": params.get("cwd").and_then(|v| v.as_str()).unwrap_or(""),
                        "parsedCmd": params.get("parsedCmd").cloned().unwrap_or_default(),
                    });
                    let ok = event_tx
                        .send(IoEvent::CodexPermissionRequest {
                            request_id: request_id_str,
                            tool_name: "ExecCommand".to_string(),
                            input,
                        })
                        .is_ok();
                    (ok, false)
                }
                // Codex 0.130+ apply-patch flow — `fileChanges` is a BTreeMap keyed by path
                "applyPatchApproval" => {
                    let input = serde_json::json!({
                        "fileChanges": params.get("fileChanges").cloned().unwrap_or_default(),
                        "grantRoot": params.get("grantRoot").cloned().unwrap_or(serde_json::Value::Null),
                        "reason": params.get("reason").cloned().unwrap_or(serde_json::Value::Null),
                    });
                    let ok = event_tx
                        .send(IoEvent::CodexPermissionRequest {
                            request_id: request_id_str,
                            tool_name: "ApplyPatch".to_string(),
                            input,
                        })
                        .is_ok();
                    (ok, false)
                }
                "item/permissions/requestApproval" => {
                    let input = serde_json::json!({
                        "cwd": params.get("cwd").cloned().unwrap_or(serde_json::Value::Null),
                        "permissions": params.get("permissions").cloned().unwrap_or_default(),
                        "reason": params.get("reason").cloned().unwrap_or(serde_json::Value::Null),
                    });
                    let ok = event_tx
                        .send(IoEvent::CodexPermissionRequest {
                            request_id: request_id_str,
                            tool_name: "Permissions".to_string(),
                            input,
                        })
                        .is_ok();
                    (ok, false)
                }
                "item/tool/requestUserInput" => {
                    let input = serde_json::json!({
                        "questions": params.get("questions").cloned().unwrap_or_default(),
                    });
                    let ok = event_tx
                        .send(IoEvent::CodexPermissionRequest {
                            request_id: request_id_str,
                            tool_name: "AskUserQuestion".to_string(),
                            input,
                        })
                        .is_ok();
                    (ok, false)
                }
                "mcpServer/elicitation/request" => {
                    let server = params
                        .get("serverName")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(unknown)")
                        .to_string();
                    let input = serde_json::json!({
                        "serverName": server,
                    });
                    let ok = event_tx
                        .send(IoEvent::CodexPermissionRequest {
                            request_id: request_id_str,
                            tool_name: "McpElicitation".to_string(),
                            input,
                        })
                        .is_ok();
                    (ok, false)
                }
                // Internal / system requests (codex 0.130+) — surface a portal message so
                // the user sees what was requested, but don't block on user approval. We
                // can't auto-respond meaningfully (we have no auth token, no attestation
                // signer); codex will retry or move on.
                "item/tool/call" => {
                    let tool = params
                        .get("tool")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(unknown)");
                    let msg = format!("**Codex tool call**: `{}`", tool);
                    let event = shared::PortalMessage::text(msg).to_json();
                    let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
                    (ok, false)
                }
                "account/chatgptAuthTokens/refresh" => {
                    let event = shared::PortalMessage::text(
                        "**Codex requested ChatGPT auth token refresh** (not handled — the agent may pause)."
                            .to_string(),
                    )
                    .to_json();
                    let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
                    (ok, false)
                }
                "attestation/generate" => {
                    let event = shared::PortalMessage::text(
                        "**Codex requested attestation generation** (not handled).".to_string(),
                    )
                    .to_json();
                    let ok = event_tx.send(IoEvent::RawOutput(event)).is_ok();
                    (ok, false)
                }
                _ => {
                    tracing::warn!("Unknown Codex request: {}", method);
                    (true, false)
                }
            }
        }
    }
}
