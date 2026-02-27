//! Output forwarder task: forwards Claude outputs to WebSocket with sequencing.
//!
//! Also handles git branch detection, PR URL lookup, and image extraction.

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use claude_codes::io::{ContentBlock, ControlRequestPayload, ToolUseBlock};
use claude_codes::ClaudeOutput;
use shared::ProxyToServer;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::output_buffer::PendingOutputBuffer;

use super::{format_duration, truncate, SharedWsWrite};

/// Spawn the output forwarder task
///
/// Forwards Claude outputs to WebSocket with sequence numbers for reliable delivery.
#[allow(clippy::too_many_arguments)]
pub fn spawn_output_forwarder(
    mut output_rx: mpsc::UnboundedReceiver<ClaudeOutput>,
    ws_write: SharedWsWrite,
    session_id: Uuid,
    working_directory: String,
    current_branch: Arc<Mutex<Option<String>>>,
    current_pr_url: Arc<Mutex<Option<String>>>,
    current_repo_url: Arc<Mutex<Option<String>>>,
    output_buffer: Arc<Mutex<PendingOutputBuffer>>,
    max_image_mb: Option<u32>,
) -> tokio::task::JoinHandle<()> {
    let max_bytes = max_image_mb.unwrap_or(DEFAULT_MAX_IMAGE_MB) as usize * 1024 * 1024;
    tokio::spawn(async move {
        let mut message_count: u64 = 0;
        let mut pending_git_check = false;
        // Track Read tool calls on image files: tool_use_id → file_path
        let mut image_read_map: HashMap<String, String> = HashMap::new();

        while let Some(output) = output_rx.recv().await {
            message_count += 1;

            // Log detailed info about the message
            log_claude_output(&output);

            // Check for branch/PR update from PREVIOUS git command (deferred so
            // the command has finished executing before we query git/gh state)
            let should_check_branch = pending_git_check || message_count.is_multiple_of(100);
            if should_check_branch {
                pending_git_check = false;
                check_and_send_branch_update(
                    &ws_write,
                    session_id,
                    &working_directory,
                    &current_branch,
                    &current_pr_url,
                    &current_repo_url,
                )
                .await;
            }

            // Check if THIS message is a git-related bash command (for next iteration)
            if is_git_bash_command(&output) {
                pending_git_check = true;
            }

            // Track Read tool calls on image files from assistant messages
            track_image_reads(&output, &mut image_read_map);

            // Check for image tool results in user messages and send portal messages
            let portal_messages =
                extract_image_portal_messages(&output, &mut image_read_map, max_bytes);

            // Serialize and buffer with sequence number
            let content = serde_json::to_value(&output)
                .unwrap_or(serde_json::Value::String(format!("{:?}", output)));

            // Add to buffer and get sequence number
            let seq = {
                let mut buf = output_buffer.lock().await;
                buf.push(content.clone())
            };

            // Send as sequenced output
            let msg = ProxyToServer::SequencedOutput { seq, content };

            {
                let mut ws = ws_write.lock().await;
                if ws.send(msg).await.is_err() {
                    error!("Failed to send to backend");
                    break;
                }
            }

            // Send any image portal messages after the main output
            for portal_msg in portal_messages {
                let portal_content = portal_msg.to_json();
                let portal_seq = {
                    let mut buf = output_buffer.lock().await;
                    buf.push(portal_content.clone())
                };
                let portal_ws_msg = ProxyToServer::SequencedOutput {
                    seq: portal_seq,
                    content: portal_content,
                };
                let mut ws = ws_write.lock().await;
                if ws.send(portal_ws_msg).await.is_err() {
                    error!("Failed to send image portal message");
                    break;
                }
            }
        }
        debug!("Output forwarder ended - channel closed");
    })
}

/// Get the current git branch name, if in a git repository
pub(super) fn get_git_branch(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    // If we're in detached HEAD state, get the short commit hash instead
    if branch == "HEAD" {
        std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(cwd)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| format!("detached:{}", s.trim()))
    } else {
        Some(branch)
    }
}

/// Look up the GitHub repository URL using the `gh` CLI
pub(super) fn get_repo_url(cwd: &str) -> Option<String> {
    let output = std::process::Command::new("gh")
        .args(["repo", "view", "--json", "url", "-q", ".url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Look up the GitHub PR URL for a branch using the `gh` CLI
pub(super) fn get_pr_url(cwd: &str, branch: &str) -> Option<String> {
    if branch == "main" || branch == "master" || branch.starts_with("detached:") {
        return None;
    }
    let output = std::process::Command::new("gh")
        .args(["pr", "view", branch, "--json", "url", "-q", ".url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Check if a tool use is a Bash command containing "git"
fn is_git_bash_command(output: &ClaudeOutput) -> bool {
    if let ClaudeOutput::User(user) = output {
        for block in &user.message.content {
            if let ContentBlock::ToolResult(tr) = block {
                if let Some(ref content) = tr.content {
                    let content_str = format!("{:?}", content);
                    if content_str.contains("git ")
                        || content_str.contains("gh ")
                        || content_str.contains("branch")
                        || content_str.contains("checkout")
                        || content_str.contains("merge")
                        || content_str.contains("rebase")
                        || content_str.contains("commit")
                    {
                        return true;
                    }
                }
            }
        }
    }
    // Also check if an assistant message contains a Bash tool_use with git
    if let Some(bash) = output.as_tool_use("Bash") {
        if let Some(claude_codes::tool_inputs::ToolInput::Bash(input)) = bash.typed_input() {
            if input.command.contains("git ") || input.command.contains("gh ") {
                return true;
            }
        }
    }
    false
}

/// Check and send git branch or PR URL update if changed
async fn check_and_send_branch_update(
    ws_write: &SharedWsWrite,
    session_id: Uuid,
    working_directory: &str,
    current_branch: &Arc<Mutex<Option<String>>>,
    current_pr_url: &Arc<Mutex<Option<String>>>,
    current_repo_url: &Arc<Mutex<Option<String>>>,
) {
    let new_branch = get_git_branch(working_directory);
    let new_pr_url = new_branch
        .as_deref()
        .and_then(|b| get_pr_url(working_directory, b));
    let new_repo_url = get_repo_url(working_directory);

    let mut branch_guard = current_branch.lock().await;
    let mut pr_guard = current_pr_url.lock().await;
    let mut repo_guard = current_repo_url.lock().await;

    let branch_changed = *branch_guard != new_branch;
    let pr_changed = *pr_guard != new_pr_url;
    let repo_changed = *repo_guard != new_repo_url;

    if branch_changed || pr_changed || repo_changed {
        if branch_changed {
            debug!(
                "Git branch changed: {:?} -> {:?}",
                *branch_guard, new_branch
            );
        }
        if pr_changed {
            debug!("PR URL changed: {:?} -> {:?}", *pr_guard, new_pr_url);
        }
        *branch_guard = new_branch.clone();
        *pr_guard = new_pr_url.clone();
        *repo_guard = new_repo_url.clone();

        // Drop locks before acquiring ws lock
        drop(branch_guard);
        drop(pr_guard);
        drop(repo_guard);

        let update_msg = ProxyToServer::SessionUpdate {
            session_id,
            git_branch: new_branch,
            pr_url: new_pr_url,
            repo_url: new_repo_url,
        };

        let mut ws = ws_write.lock().await;
        if let Err(e) = ws.send(update_msg).await {
            error!("Failed to send branch update: {}", e);
        }
    }
}

/// Default max image size in MB (used when backend doesn't provide a value)
const DEFAULT_MAX_IMAGE_MB: u32 = 10;

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

/// Check user messages for tool results that correspond to tracked image reads.
/// For each match, reads the file from disk, base64-encodes it, and returns a PortalMessage.
fn extract_image_portal_messages(
    output: &ClaudeOutput,
    image_read_map: &mut HashMap<String, String>,
    max_image_bytes: usize,
) -> Vec<shared::PortalMessage> {
    let blocks = match output {
        ClaudeOutput::User(user) => &user.message.content,
        _ => return Vec::new(),
    };

    let mut portal_messages = Vec::new();

    for block in blocks {
        if let ContentBlock::ToolResult(tr) = block {
            if let Some(file_path) = image_read_map.remove(&tr.tool_use_id) {
                if tr.is_error.unwrap_or(false) {
                    continue;
                }

                let mime = image_mime_type(&file_path).unwrap_or("image/png");

                match std::fs::read(&file_path) {
                    Ok(data) => {
                        if data.len() > max_image_bytes {
                            let size_mb = data.len() as f64 / (1024.0 * 1024.0);
                            let limit_mb = max_image_bytes as f64 / (1024.0 * 1024.0);
                            portal_messages.push(shared::PortalMessage::text(format!(
                                "Image too large to display: **{:.1} MB** (limit is {:.0} MB)",
                                size_mb, limit_mb
                            )));
                        } else {
                            let file_size = data.len() as u64;
                            let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
                            debug!(
                                "Sending image portal message for {} ({} bytes)",
                                file_path,
                                data.len()
                            );
                            portal_messages.push(shared::PortalMessage::image_with_info(
                                mime.to_string(),
                                encoded,
                                Some(file_path.clone()),
                                Some(file_size),
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

    portal_messages
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
                "← [rate_limit_event] status={} type={} resets_at={} utilization={:?} overage={}",
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
