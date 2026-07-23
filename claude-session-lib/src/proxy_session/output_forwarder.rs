//! Output forwarder task: forwards Claude outputs to WebSocket with sequencing.
//!
//! Also handles git-metadata refresh triggering.
//!
//! NOTE (media display): the proxy used to detect a Claude `Read` of an image
//! file and synthesize an inline portal image message from the tool result.
//! That implicit, Claude-only path was replaced by the explicit, agent-agnostic
//! `agent-portal show <file>` CLI (which uploads directly to the backend and
//! works for images *and* video, for both Claude and Codex). The Read-trigger
//! logic — and the chunked `/ws/session/upload` uploader it fed — were removed
//! here; media now flows entirely through `POST /api/agent/sessions/{id}/media`.

use claude_codes::io::{ContentBlock, ControlRequestPayload, ToolUseBlock};
use claude_codes::ClaudeOutput;
use shared::{AgentType, ProxyToServer};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};
use uuid::Uuid;

use session_lib::output_buffer::PendingOutputBuffer;

use super::git_metadata::{
    check_and_send_branch_update, claude_output_has_git_signal, GitMetadataState, GitRefreshTrigger,
};
use super::{format_duration, truncate, SharedWsWrite};

/// Spawn the output forwarder task
///
/// Forwards Claude outputs to WebSocket with sequence numbers for reliable delivery.
pub fn spawn_output_forwarder(
    mut output_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    ws_write: SharedWsWrite,
    session_id: Uuid,
    working_directory: String,
    git_metadata: GitMetadataState,
    output_buffer: std::sync::Arc<tokio::sync::Mutex<PendingOutputBuffer>>,
    agent_type: AgentType,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut git_refresh = GitRefreshTrigger::default();

        while let Some(value) = output_rx.recv().await {
            // `Session` is now agent-neutral and forwards raw wire JSON. Re-parse
            // it to `ClaudeOutput` here, ONCE, at the Claude proxy edge; all
            // the typed side-effects (logging, git-signal) hang off this
            // `Option`. Malformed / non-Claude frames skip the typed work and
            // are forwarded verbatim — never dropped or rewritten (#1165 item 2,
            // slice 1).
            let parsed = super::parse_visible_claude_output(&value);

            // Branch/PR update from the PREVIOUS message's git command (deferred
            // so the command has finished). Runs each frame regardless of parse.
            if git_refresh.should_check_before_message() {
                check_and_send_branch_update(
                    &ws_write,
                    session_id,
                    &working_directory,
                    &git_metadata,
                )
                .await;
            }

            if let Some(ref output) = parsed {
                log_claude_output(output);

                // Is THIS message a git-related bash command (for next iteration)?
                if claude_output_has_git_signal(output) {
                    git_refresh.mark_git_signal();
                }
            }

            let content = value;

            // Add to buffer and get sequence number
            let seq = {
                let mut buf = output_buffer.lock().await;
                buf.push(content.clone())
            };

            // Send as sequenced output
            let msg = ProxyToServer::SequencedOutput {
                seq,
                content,
                agent_type,
            };

            {
                let mut ws = ws_write.lock().await;
                if ws.send(msg).await.is_err() {
                    error!("Failed to send to backend");
                    break;
                }
            }
        }
        debug!("Output forwarder ended - channel closed");
    })
}

/// Log detailed information about Claude output
fn log_claude_output(output: &ClaudeOutput) {
    match output {
        ClaudeOutput::System(sys) => {
            debug!("← [system] subtype={}", sys.subtype);
            if let Some(init) = sys.as_init() {
                if let Some(ref model) = init.model {
                    debug!("  model: {}", model);
                }
                if let Some(ref cwd) = init.cwd {
                    debug!("  cwd: {}", truncate(cwd, 60));
                }
                if !init.tools.is_empty() {
                    debug!("  tools: {} available", init.tools.len());
                }
            }
            if let Some(task) = sys.as_task_started() {
                debug!(
                    "  task_started: id={} type={:?} desc={}",
                    task.task_id,
                    task.task_type,
                    truncate(&task.description, 60)
                );
            }
            if let Some(task) = sys.as_task_notification() {
                debug!(
                    "  task_notification: id={} status={:?}",
                    task.task_id, task.status
                );
            }
        }
        ClaudeOutput::Assistant(asst) => {
            let msg = &asst.message;
            let stop = msg
                .stop_reason
                .as_ref()
                .map(|s| s.as_str())
                .unwrap_or("none");

            // Count content blocks by type
            let mut text_count = 0;
            let mut tool_count = 0;
            let mut thinking_count = 0;

            for block in &msg.content {
                match block {
                    ContentBlock::Text(t) => {
                        text_count += 1;
                        let preview = truncate(&t.text, 80);
                        debug!("← [assistant] text: {}", preview);
                    }
                    ContentBlock::ToolUse(tu) => {
                        tool_count += 1;
                        let input_preview = format_tool_input(tu);
                        debug!("← [assistant] tool_use: {} {}", tu.name, input_preview);
                    }
                    ContentBlock::Thinking(th) => {
                        thinking_count += 1;
                        let preview = truncate(&th.thinking, 60);
                        debug!("← [assistant] thinking: {}", preview);
                    }
                    ContentBlock::ToolResult(tr) => {
                        let status = if tr.is_error.unwrap_or(false) {
                            "error"
                        } else {
                            "ok"
                        };
                        debug!("← [assistant] tool_result: {} ({})", tr.tool_use_id, status);
                    }
                    ContentBlock::Image(_) => {
                        debug!("← [assistant] image block");
                    }
                    ContentBlock::ServerToolUse(stu) => {
                        tool_count += 1;
                        let input_preview = format_named_tool_input(&stu.name, &stu.input);
                        debug!(
                            "← [assistant] server_tool_use: {} {}",
                            stu.name, input_preview
                        );
                    }
                    ContentBlock::WebSearchToolResult(r) => {
                        debug!("← [assistant] web_search_tool_result: {}", r.tool_use_id);
                    }
                    ContentBlock::CodeExecutionToolResult(r) => {
                        debug!(
                            "← [assistant] code_execution_tool_result: {}",
                            r.tool_use_id
                        );
                    }
                    ContentBlock::McpToolUse(mtu) => {
                        tool_count += 1;
                        let input_preview = format_named_tool_input(&mtu.name, &mtu.input);
                        let server = mtu.server_name.as_deref().unwrap_or("?");
                        debug!(
                            "← [assistant] mcp_tool_use: {}::{} {}",
                            server, mtu.name, input_preview
                        );
                    }
                    ContentBlock::McpToolResult(r) => {
                        let status = if r.is_error.unwrap_or(false) {
                            "error"
                        } else {
                            "ok"
                        };
                        debug!(
                            "← [assistant] mcp_tool_result: {} ({})",
                            r.tool_use_id, status
                        );
                    }
                    ContentBlock::ContainerUpload(_) => {
                        debug!("← [assistant] container_upload block");
                    }
                    ContentBlock::Fallback(fb) => {
                        debug!(
                            "← [assistant] model fallback: {} → {}",
                            fb.from.model, fb.to.model
                        );
                    }
                    ContentBlock::Unknown(v) => {
                        let block_type =
                            v.get("type").and_then(|t| t.as_str()).unwrap_or("unknown");
                        debug!("← [assistant] unknown block: type={}", block_type);
                    }
                }
            }

            if text_count + tool_count + thinking_count > 1 {
                debug!(
                    "  stop_reason={}, blocks: {} text, {} tools, {} thinking",
                    stop, text_count, tool_count, thinking_count
                );
            } else if tool_count > 0 || stop != "none" {
                debug!("  stop_reason={}", stop);
            }
        }
        ClaudeOutput::User(user) => {
            for block in &user.message.content {
                match block {
                    ContentBlock::Text(t) => {
                        debug!("← [user] text: {}", truncate(&t.text, 80));
                    }
                    ContentBlock::ToolResult(tr) => {
                        let status = if tr.is_error.unwrap_or(false) {
                            "ERROR"
                        } else {
                            "ok"
                        };
                        let content_preview = tr
                            .content
                            .as_ref()
                            .map(|c| {
                                let s = format!("{:?}", c);
                                if s.len() > 60 {
                                    format!("{}...", truncate(&s, 60))
                                } else {
                                    s
                                }
                            })
                            .unwrap_or_default();
                        debug!("← [user] tool_result [{}]: {}", status, content_preview);
                    }
                    _ => {
                        debug!("← [user] other block");
                    }
                }
            }
        }
        ClaudeOutput::Result(res) => {
            let status = if res.is_error { "ERROR" } else { "success" };
            let duration = format_duration(res.duration_ms);
            let api_duration = format_duration(res.duration_api_ms);
            debug!(
                "← [result] {} | {} total | {} API | {} turns",
                status, duration, api_duration, res.num_turns
            );
            if res.total_cost_usd > 0.0 {
                debug!("  cost: ${:.4}", res.total_cost_usd);
            }
        }
        ClaudeOutput::ControlRequest(req) => {
            debug!("← [control_request] id={}", req.request_id);
            match &req.request {
                ControlRequestPayload::CanUseTool(tool_req) => {
                    let input_preview =
                        format_named_tool_input(&tool_req.tool_name, &tool_req.input);
                    debug!("  tool: {} {}", tool_req.tool_name, input_preview);
                }
                ControlRequestPayload::HookCallback(_) => {
                    debug!("  hook callback");
                }
                ControlRequestPayload::McpMessage(_) => {
                    debug!("  MCP message");
                }
                ControlRequestPayload::Initialize(_) => {
                    debug!("  initialize");
                }
            }
        }
        ClaudeOutput::ControlResponse(resp) => {
            debug!("← [control_response] {:?}", resp);
        }
        ClaudeOutput::Error(err) => {
            if err.is_overloaded() {
                warn!("← [error] API overloaded (529)");
            } else if err.is_rate_limited() {
                warn!("← [error] Rate limited (429)");
            } else if err.is_server_error() {
                error!("← [error] Server error (500): {}", err.error.message);
            } else {
                error!("← [error] API error: {}", err.error.message);
            }
        }
        ClaudeOutput::RateLimitEvent(evt) => {
            let info = &evt.rate_limit_info;
            debug!(
                "← [rate_limit_event] status={} type={:?} resets_at={:?} utilization={:?} overage={:?}",
                info.status,
                info.rate_limit_type,
                info.resets_at,
                info.utilization,
                info.is_using_overage
            );
        }
        // 2.1.160 wire coverage: stream_event / tool_progress / auth_status /
        // tool_use_summary / prompt_suggestion / conversation_reset plus the
        // transcript-corpus variants. This fn only produces debug logging, so
        // a variant-tag line is all they need.
        other => {
            debug!("← [{}]", other.message_type());
        }
    }
}

/// Format tool input for logging
fn format_tool_input(tool: &ToolUseBlock) -> String {
    format_named_tool_input(&tool.name, &tool.input)
}

fn format_named_tool_input(name: &str, input: &serde_json::Value) -> String {
    use claude_codes::tool_inputs::ToolInput;

    match ToolInput::from_named_input(name, input.clone()) {
        ToolInput::Bash(b) => format!("$ {}", truncate(&b.command, 70)),
        ToolInput::Read(r) => truncate(&r.file_path, 70).to_string(),
        ToolInput::Edit(e) => truncate(&e.file_path, 70).to_string(),
        ToolInput::Write(w) => truncate(&w.file_path, 70).to_string(),
        ToolInput::Glob(g) => format!(
            "'{}' in {}",
            truncate(&g.pattern, 40),
            truncate(g.path.as_deref().unwrap_or("."), 30)
        ),
        ToolInput::Grep(g) => format!(
            "'{}' in {}",
            truncate(&g.pattern, 40),
            truncate(g.path.as_deref().unwrap_or("."), 30)
        ),
        ToolInput::Task(t) => truncate(&t.description, 60).to_string(),
        ToolInput::WebFetch(w) => truncate(&w.url, 60).to_string(),
        ToolInput::WebSearch(w) => truncate(&w.query, 60).to_string(),
        ToolInput::Unknown(_) => format_unknown_tool_input(input),
        _ => String::new(),
    }
}

fn format_unknown_tool_input(input: &serde_json::Value) -> String {
    if let Some(obj) = input.as_object() {
        obj.iter()
            .find_map(|(k, v)| v.as_str().map(|s| format!("{}={}", k, truncate(s, 50))))
            .unwrap_or_default()
    } else {
        String::new()
    }
}
