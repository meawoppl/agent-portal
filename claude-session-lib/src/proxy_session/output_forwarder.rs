//! Output forwarder task: forwards Claude outputs to WebSocket with sequencing.
//!
//! Also handles metadata refresh triggering and image extraction.

use std::collections::HashMap;

use base64::Engine;
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
use super::image_uploader::upload_image;
use super::{format_duration, truncate, PendingInputDisplayEvents, SharedWsWrite};

/// Images at or above this raw-byte size get sent via the chunked upload
/// stream (`/ws/session/upload`) instead of inlined as base64 on the main
/// session socket. Below it, the small-image fast path keeps a single
/// round trip.
const CHUNK_UPLOAD_THRESHOLD_BYTES: usize = 1024 * 1024;

/// Spawn the output forwarder task
///
/// Forwards Claude outputs to WebSocket with sequence numbers for reliable delivery.
#[allow(clippy::too_many_arguments)]
pub fn spawn_output_forwarder(
    mut output_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    ws_write: SharedWsWrite,
    session_id: Uuid,
    backend_url: String,
    auth_token: Option<String>,
    working_directory: String,
    git_metadata: GitMetadataState,
    output_buffer: std::sync::Arc<tokio::sync::Mutex<PendingOutputBuffer>>,
    max_image_mb: u32,
    agent_type: AgentType,
    pending_input_display_events: PendingInputDisplayEvents,
) -> tokio::task::JoinHandle<()> {
    let max_bytes = max_image_mb as usize * 1024 * 1024;
    tokio::spawn(async move {
        let mut git_refresh = GitRefreshTrigger::default();
        // Track Read tool calls on image files: tool_use_id → file_path
        let mut image_read_map: HashMap<String, String> = HashMap::new();

        while let Some(value) = output_rx.recv().await {
            // `Session` is now agent-neutral and forwards raw wire JSON. Re-parse
            // it to `ClaudeOutput` here, ONCE, at the Claude proxy edge; all the
            // typed side-effects (logging, git-signal, image extraction, echo
            // replacement) hang off this `Option`. Malformed / non-Claude frames
            // skip the typed work and are forwarded verbatim — never dropped or
            // rewritten (#1165 item 2, slice 1).
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

            let (content, image_items) = if let Some(ref output) = parsed {
                log_claude_output(output);

                // Is THIS message a git-related bash command (for next iteration)?
                if claude_output_has_git_signal(output) {
                    git_refresh.mark_git_signal();
                }

                // Track Read tool calls on image files from assistant messages.
                track_image_reads(output, &mut image_read_map);

                // Image work items from user-message tool results (raw bytes for
                // large images, base64 for small ones).
                let image_items = extract_image_work_items(output, &mut image_read_map, max_bytes);

                // Echo-replacement swaps the rendered content for typed portal
                // inputs (`agent-portal message`); otherwise persist + forward
                // the EXACT raw value (invariant: exact raw Value is what gets
                // persisted and sent).
                let content =
                    replacement_input_display_event(output, &pending_input_display_events)
                        .await
                        .unwrap_or_else(|| value.clone());

                (content, image_items)
            } else {
                // Malformed / non-Claude frame: forward verbatim, no typed work.
                (value.clone(), Vec::new())
            };

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

            // Resolve each image item into a portal message — uploading
            // large images out-of-band first so the on-wire payload stays
            // small — then send them after the main output.
            let mut send_failed = false;
            for item in image_items {
                let portal_msg = match item {
                    ImageWorkItem::Inline(msg) => msg,
                    ImageWorkItem::TooLarge(msg) => msg,
                    ImageWorkItem::Upload {
                        file_path,
                        file_size,
                        media_type,
                        bytes,
                    } => {
                        let result = match auth_token.as_deref() {
                            Some(token) => {
                                upload_image(
                                    &backend_url,
                                    session_id,
                                    token,
                                    &media_type,
                                    Some(&file_path),
                                    &bytes,
                                )
                                .await
                            }
                            None => Err(anyhow::anyhow!("no auth token available for upload")),
                        };
                        match result {
                            Ok(url) => shared::PortalMessage::with_content(vec![
                                shared::PortalContent::Image {
                                    media_type,
                                    data: url,
                                    file_path: Some(file_path),
                                    file_size: Some(file_size),
                                    source_type: Some("url".to_string()),
                                },
                            ]),
                            Err(e) => {
                                warn!("Chunked image upload failed: {}", e);
                                shared::PortalMessage::text(format!("Image upload failed: {}", e))
                            }
                        }
                    }
                };

                let portal_content = portal_msg.to_json();
                let portal_seq = {
                    let mut buf = output_buffer.lock().await;
                    buf.push(portal_content.clone())
                };
                let portal_ws_msg = ProxyToServer::SequencedOutput {
                    seq: portal_seq,
                    content: portal_content,
                    agent_type,
                };
                let mut ws = ws_write.lock().await;
                if ws.send(portal_ws_msg).await.is_err() {
                    error!("Failed to send image portal message");
                    send_failed = true;
                    break;
                }
            }
            if send_failed {
                break;
            }
        }
        debug!("Output forwarder ended - channel closed");
    })
}

/// One unit of image work surfaced from a user-message tool result.
enum ImageWorkItem {
    /// Small image: data is already base64-encoded and ready to inline.
    Inline(shared::PortalMessage),
    /// Large image: raw bytes need to go through the chunked upload stream.
    Upload {
        file_path: String,
        file_size: u64,
        media_type: String,
        bytes: Vec<u8>,
    },
    /// Image exceeded the hard cap; this is a textual rejection notice.
    TooLarge(shared::PortalMessage),
}

async fn replacement_input_display_event(
    output: &ClaudeOutput,
    pending_input_display_events: &PendingInputDisplayEvents,
) -> Option<serde_json::Value> {
    let echoed_text = user_text_echo(output)?;
    let mut pending = pending_input_display_events.lock().await;
    let pos = pending
        .iter()
        .position(|event| event.echoed_text == echoed_text)?;
    pending.remove(pos).map(|event| event.content)
}

fn user_text_echo(output: &ClaudeOutput) -> Option<String> {
    let ClaudeOutput::User(user) = output else {
        return None;
    };
    let mut text_blocks = user.message.content.iter().filter_map(|block| match block {
        ContentBlock::Text(text) => Some(text.text.clone()),
        _ => None,
    });
    let text = text_blocks.next()?;
    if text_blocks.next().is_some() {
        return None;
    }
    Some(text)
}

/// Return the MIME type for a supported image extension, or None.
fn image_mime_type(path: &str) -> Option<&'static str> {
    let lower = path.to_lowercase();
    if lower.ends_with(".svg") {
        Some("image/svg+xml")
    } else if lower.ends_with(".png") {
        Some("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        Some("image/jpeg")
    } else if lower.ends_with(".gif") {
        Some("image/gif")
    } else if lower.ends_with(".webp") {
        Some("image/webp")
    } else {
        None
    }
}

/// Track Read tool calls on image files from assistant messages.
/// Stores tool_use_id → file_path for later correlation with tool results.
fn track_image_reads(output: &ClaudeOutput, image_read_map: &mut HashMap<String, String>) {
    let blocks = match output {
        ClaudeOutput::Assistant(asst) => &asst.message.content,
        _ => return,
    };

    for block in blocks {
        if let ContentBlock::ToolUse(tu) = block {
            if let Some(claude_codes::tool_inputs::ToolInput::Read(read_input)) = tu.typed_input() {
                if image_mime_type(&read_input.file_path).is_some() {
                    debug!(
                        "Tracking image Read: tool_use_id={} path={}",
                        tu.id, read_input.file_path
                    );
                    image_read_map.insert(tu.id.clone(), read_input.file_path.clone());
                }
            }
        }
    }
}

/// Check user messages for tool results that correspond to tracked image
/// reads. Reads each matching file from disk and classifies it for
/// downstream delivery: inline base64 for small images, chunked upload
/// for large ones, or a textual rejection if it exceeds the backend cap.
fn extract_image_work_items(
    output: &ClaudeOutput,
    image_read_map: &mut HashMap<String, String>,
    max_image_bytes: usize,
) -> Vec<ImageWorkItem> {
    let blocks = match output {
        ClaudeOutput::User(user) => &user.message.content,
        _ => return Vec::new(),
    };

    let mut items = Vec::new();

    for block in blocks {
        if let ContentBlock::ToolResult(tr) = block {
            if let Some(file_path) = image_read_map.remove(&tr.tool_use_id) {
                if tr.is_error.unwrap_or(false) {
                    continue;
                }

                let mime = image_mime_type(&file_path).unwrap_or("image/png");

                match std::fs::read(&file_path) {
                    Ok(data) => {
                        let file_size = data.len() as u64;
                        if data.len() > max_image_bytes {
                            let size_mb = data.len() as f64 / (1024.0 * 1024.0);
                            let limit_mb = max_image_bytes as f64 / (1024.0 * 1024.0);
                            warn!(
                                "Image {} too large ({:.1} MB > {:.0} MB cap), skipping",
                                file_path, size_mb, limit_mb
                            );
                            items.push(ImageWorkItem::TooLarge(shared::PortalMessage::text(
                                format!(
                                    "Image too large to display: **{:.1} MB** (limit is {:.0} MB)",
                                    size_mb, limit_mb
                                ),
                            )));
                        } else if data.len() >= CHUNK_UPLOAD_THRESHOLD_BYTES {
                            debug!(
                                "Queueing chunked upload for {} ({} bytes)",
                                file_path,
                                data.len()
                            );
                            items.push(ImageWorkItem::Upload {
                                file_path: file_path.clone(),
                                file_size,
                                media_type: mime.to_string(),
                                bytes: data,
                            });
                        } else {
                            let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                            debug!(
                                "Inlining image portal message for {} ({} bytes)",
                                file_path,
                                data.len()
                            );
                            items.push(ImageWorkItem::Inline(
                                shared::PortalMessage::image_with_info(
                                    mime.to_string(),
                                    encoded,
                                    Some(file_path.clone()),
                                    Some(file_size),
                                ),
                            ));
                        }
                    }
                    Err(e) => {
                        warn!("Failed to read image file {}: {}", file_path, e);
                    }
                }
            }
        }
    }

    items
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
                        let input_preview = format_tool_input_json(&stu.input);
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
                        let input_preview = format_tool_input_json(&mtu.input);
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
                    let input_preview = format_tool_input_json(&tool_req.input);
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
                "← [rate_limit_event] status={} type={:?} resets_at={:?} utilization={:?} overage={}",
                info.status,
                info.rate_limit_type,
                info.resets_at,
                info.utilization,
                info.is_using_overage
            );
        }
    }
}

/// Format tool input for logging
fn format_tool_input(tool: &ToolUseBlock) -> String {
    format_tool_input_json(&tool.input)
}

fn format_tool_input_json(input: &serde_json::Value) -> String {
    use claude_codes::tool_inputs::ToolInput;

    // Try to parse as typed input first
    if let Ok(typed) = serde_json::from_value::<ToolInput>(input.clone()) {
        return match typed {
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
            _ => String::new(),
        };
    }

    // Fallback to manual JSON extraction for unknown tools
    if let Some(obj) = input.as_object() {
        obj.iter()
            .find_map(|(k, v)| v.as_str().map(|s| format!("{}={}", k, truncate(s, 50))))
            .unwrap_or_default()
    } else {
        String::new()
    }
}
