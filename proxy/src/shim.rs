//! Shim mode: transparent proxy between a parent process (e.g., VS Code) and Claude.
//!
//! In shim mode, the proxy acts as a stdin/stdout bridge. All claude output is
//! forwarded to stdout (for the parent process) while also being sent to the
//! portal backend via WebSocket. Input from both stdin and the portal web UI
//! reaches claude's stdin. This enables VS Code extension sessions to appear
//! in the portal dashboard.
//!
//! All diagnostic output goes to stderr only — stdout is reserved for claude I/O.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::session::{
    connect_ws, get_git_branch, register_with_backend, Backoff, PermissionResponseData,
    ProxySessionConfig, SharedWsWrite, WsEvent,
};
use anyhow::Result;
use claude_codes::io::{
    ControlRequestPayload, ControlResponse, ControlResponseMessage, PermissionResult,
};
use claude_codes::{ClaudeInput, ClaudeOutput};
use claude_session_lib::output_buffer::PendingOutputBuffer;
use futures_util::SinkExt;
use shared::ProxyToServer;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

/// Permission tracking for deduplication between VS Code and portal responses.
#[derive(Debug)]
enum PermissionState {
    /// Waiting for a response from either source.
    Pending,
    /// Already answered — ignore duplicate responses.
    Answered,
}

/// Run the shim: spawn claude, bridge stdin/stdout, connect to portal.
///
/// This function calls `std::process::exit` with claude's exit code when claude exits,
/// so it effectively never returns normally.
pub async fn run_shim(config: ProxySessionConfig) -> Result<()> {
    info!("Starting shim mode");

    // Spawn claude binary with the same flags as claude-session-lib
    let mut child = spawn_claude(&config)?;

    let claude_stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture claude stdin"))?;
    let claude_stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture claude stdout"))?;
    let claude_stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture claude stderr"))?;

    // Shared handle to claude's stdin (both VS Code input and portal input write here)
    let claude_stdin = Arc::new(Mutex::new(claude_stdin));

    // Permission dedup state
    let permissions: Arc<Mutex<HashMap<String, PermissionState>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Pipe claude's stderr to our stderr
    let stderr_task = tokio::spawn(async move {
        let mut reader = BufReader::new(claude_stderr).lines();
        while let Ok(Some(line)) = reader.next_line().await {
            eprintln!("{}", line);
        }
    });

    // Output buffer for reliable delivery to portal
    let output_buffer = Arc::new(Mutex::new(
        PendingOutputBuffer::new(config.session_id)
            .unwrap_or_else(|_| PendingOutputBuffer::new(config.session_id).unwrap()),
    ));

    // Channel for portal-sent message texts (for user echo dedup in VS Code).
    // The sender is used in the WS connection loop; the receiver lives in the
    // stdout reader task where it's drained synchronously via try_recv() — no
    // async Mutex in the hot path.
    let (portal_text_tx, portal_text_rx) = mpsc::unbounded_channel::<String>();

    // On resume, don't filter user echoes until first stdin input arrives.
    // During resume replay, Claude re-emits all past user messages and VS Code needs them.
    let filtering_active = Arc::new(AtomicBool::new(!config.resume));

    // Run the connection loop (reconnects automatically)
    run_shim_loop(
        &config,
        claude_stdout,
        claude_stdin,
        permissions,
        output_buffer,
        portal_text_tx,
        portal_text_rx,
        filtering_active,
    )
    .await;

    // Wait for claude to finish and exit with its exit code
    let code = match child.wait().await {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            error!("Failed to wait for claude: {}", e);
            1
        }
    };

    stderr_task.abort();
    info!("Claude exited with code {}", code);
    std::process::exit(code);
}

/// Spawn the claude binary with piped stdin/stdout/stderr.
fn spawn_claude(config: &ProxySessionConfig) -> Result<tokio::process::Child> {
    let mut cmd = Command::new("claude");
    cmd.arg("--print")
        .arg("--verbose")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--permission-prompt-tool")
        .arg("stdio")
        .arg("--replay-user-messages");

    if config.resume {
        cmd.arg("--resume").arg(config.session_id.to_string());
    } else {
        cmd.arg("--session-id").arg(config.session_id.to_string());
    }

    for arg in &config.claude_args {
        cmd.arg(arg);
    }

    cmd.current_dir(&config.working_directory);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    info!(
        "Spawning claude in shim mode (session={}, resume={})",
        config.session_id, config.resume
    );

    cmd.spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn claude: {}", e))
}

/// Main shim loop with WebSocket reconnection.
///
/// Reads claude's stdout and forwards to both our stdout and the portal backend.
/// Reads our stdin and portal WebSocket input, forwards both to claude's stdin.
#[allow(clippy::too_many_arguments)]
async fn run_shim_loop(
    config: &ProxySessionConfig,
    claude_stdout: tokio::process::ChildStdout,
    claude_stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    permissions: Arc<Mutex<HashMap<String, PermissionState>>>,
    output_buffer: Arc<Mutex<PendingOutputBuffer>>,
    portal_text_tx: mpsc::UnboundedSender<String>,
    portal_text_rx: mpsc::UnboundedReceiver<String>,
    filtering_active: Arc<AtomicBool>,
) {
    let mut backoff = Backoff::new();
    let mut first_connection = true;
    let mut claude_stdout_reader = BufReader::new(claude_stdout).lines();

    // Channel for sequenced outputs to send to portal (seq assigned at buffer push time)
    let (output_line_tx, mut output_line_rx) =
        mpsc::unbounded_channel::<(u64, serde_json::Value)>();

    // Channel for permission requests extracted from claude stdout
    let (perm_request_tx, mut perm_request_rx) = mpsc::unbounded_channel::<ProxyToServer>();

    // Our stdout handle (for forwarding to VS Code)
    let our_stdout = Arc::new(Mutex::new(tokio::io::stdout()));

    // Our stdin (from VS Code)
    let own_stdin = tokio::io::stdin();
    let mut own_stdin_reader = BufReader::new(own_stdin).lines();

    // Read claude stdout and forward to our stdout + output channel
    // This runs independently of the WebSocket connection
    let output_buffer_for_reader = output_buffer.clone();
    let our_stdout_for_reader = our_stdout.clone();
    let permissions_for_reader = permissions.clone();
    let filtering_for_reader = filtering_active.clone();

    // Claude stdout reader task: reads lines, forwards to stdout and queues for portal.
    //
    // User echo dedup: when --replay-user-messages is active, Claude echoes every user
    // message back on stdout. VS Code already displays what the user typed, so these
    // echoes create duplicates. We filter them out UNLESS the message came from the
    // portal (which VS Code doesn't know about and needs to see).
    //
    // Portal-sent texts arrive via channel (try_recv, non-blocking) to avoid any
    // async Mutex in this hot path.
    let stdout_reader_task = tokio::spawn(async move {
        let mut portal_text_rx = portal_text_rx;
        let mut portal_texts: Vec<String> = Vec::new();

        while let Ok(Some(line)) = claude_stdout_reader.next_line().await {
            // Drain any new portal-sent texts (non-blocking, no Mutex)
            while let Ok(text) = portal_text_rx.try_recv() {
                portal_texts.push(text);
            }

            let parsed = serde_json::from_str::<serde_json::Value>(&line).ok();

            // Decide if this line should go to VS Code stdout.
            // All non-user messages always go through. For user echoes, check
            // if it came from the portal (tracked text match) or from VS Code (filter it).
            let forward_to_vscode = match &parsed {
                Some(value) if value.get("type").and_then(|t| t.as_str()) == Some("user") => {
                    if !filtering_for_reader.load(Ordering::Relaxed) {
                        // Resume replay phase — forward all user echoes
                        true
                    } else {
                        // Check if this echo is from a portal-sent message
                        match extract_user_text(value) {
                            Some(ref text) => {
                                if let Some(pos) = portal_texts.iter().position(|t| t == text) {
                                    portal_texts.remove(pos);
                                    true // Portal message — VS Code needs to see it
                                } else {
                                    false // Local echo — VS Code already has it
                                }
                            }
                            None => true, // Can't extract text — forward to be safe
                        }
                    }
                }
                _ => true, // Non-user or non-JSON: always forward
            };

            // Forward to VS Code stdout (when appropriate)
            if forward_to_vscode {
                let mut stdout = our_stdout_for_reader.lock().await;
                if let Err(e) = stdout.write_all(line.as_bytes()).await {
                    error!("Failed to write to stdout: {}", e);
                    break;
                }
                if let Err(e) = stdout.write_all(b"\n").await {
                    error!("Failed to write newline to stdout: {}", e);
                    break;
                }
                let _ = stdout.flush().await;
            }

            // Parse for portal forwarding (independent of VS Code decision)
            if let Some(value) = parsed {
                let msg_type = value.get("type").and_then(|t| t.as_str());

                // Skip protocol noise that the portal doesn't need
                if matches!(msg_type, Some("stream_event") | Some("control_response")) {
                    continue;
                }

                // Send control_request (can_use_tool) as a typed PermissionRequest
                // so the portal shows an interactive approval dialog
                if msg_type == Some("control_request") {
                    if let Ok(ClaudeOutput::ControlRequest(req)) =
                        serde_json::from_value::<ClaudeOutput>(value.clone())
                    {
                        let mut perms = permissions_for_reader.lock().await;
                        perms.insert(req.request_id.clone(), PermissionState::Pending);
                        debug!("Tracking permission request: {}", req.request_id);

                        if let ControlRequestPayload::CanUseTool(tool_req) = req.request {
                            let _ = perm_request_tx.send(ProxyToServer::PermissionRequest {
                                request_id: req.request_id,
                                tool_name: tool_req.tool_name,
                                input: tool_req.input,
                                permission_suggestions: tool_req.permission_suggestions,
                            });
                            continue;
                        }
                    }
                    // Non-can_use_tool control requests (hooks, mcp, init) — skip
                    continue;
                }

                // Buffer regular output for portal delivery (seq assigned here)
                let seq = {
                    let mut buf = output_buffer_for_reader.lock().await;
                    buf.push(value.clone())
                };
                let _ = output_line_tx.send((seq, value));
            }
        }
        info!("Claude stdout ended");
    });

    // Stdin reader task: reads VS Code input, forwards to claude + tracks permissions
    let claude_stdin_for_reader = claude_stdin.clone();
    let permissions_for_stdin = permissions.clone();
    let filtering_for_stdin = filtering_active.clone();
    let (stdin_line_tx, mut stdin_line_rx) = mpsc::unbounded_channel::<String>();

    let stdin_reader_task = tokio::spawn(async move {
        while let Ok(Some(line)) = own_stdin_reader.next_line().await {
            // Activate user echo filtering after first stdin input.
            // On resume, this marks the end of the replay phase.
            filtering_for_stdin.store(true, Ordering::Relaxed);
            // Check if this is a permission response from VS Code (for dedup tracking)
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                if value.get("type").and_then(|t| t.as_str()) == Some("control_response") {
                    if let Some(request_id) = value.get("request_id").and_then(|r| r.as_str()) {
                        let mut perms = permissions_for_stdin.lock().await;
                        if let Some(state) = perms.get_mut(request_id) {
                            if matches!(state, PermissionState::Pending) {
                                *state = PermissionState::Answered;
                                debug!("Permission {} answered by VS Code (stdin)", request_id);
                            } else {
                                // Already answered by portal — still forward to claude
                                // (claude handles duplicate gracefully)
                                debug!(
                                    "Permission {} already answered, forwarding anyway",
                                    request_id
                                );
                            }
                        }
                    }
                }
            }

            // Always forward stdin to claude (transparency)
            let mut stdin = claude_stdin_for_reader.lock().await;
            if let Err(e) = stdin.write_all(line.as_bytes()).await {
                error!("Failed to write to claude stdin: {}", e);
                break;
            }
            if let Err(e) = stdin.write_all(b"\n").await {
                error!("Failed to write newline to claude stdin: {}", e);
                break;
            }
            let _ = stdin.flush().await;

            // Also notify the WS loop (for input forwarding to portal if needed)
            let _ = stdin_line_tx.send(line);
        }
        info!("Own stdin ended (parent process disconnected)");
    });

    // WebSocket connection loop with reconnection
    loop {
        // Connect to portal backend
        let conn = match connect_ws(&config.backend_url).await {
            Ok(conn) => {
                if !first_connection {
                    info!("Reconnected to portal backend");
                }
                backoff.reset();
                conn
            }
            Err(_) => {
                if first_connection {
                    info!("Portal backend unreachable, continuing without portal");
                } else {
                    warn!(
                        "Failed to reconnect, retrying in {}s",
                        backoff.current_secs()
                    );
                }
                tokio::time::sleep(backoff.sleep_duration()).await;
                backoff.advance();
                first_connection = false;
                continue;
            }
        };

        // Register session
        let mut conn = conn;
        let config_with_branch = ProxySessionConfig {
            git_branch: get_git_branch(&config.working_directory),
            ..config.clone()
        };

        if let Err((_, err)) = register_with_backend(&mut conn, &config_with_branch).await {
            warn!(
                "Registration failed: {}, retrying in {}s",
                err.as_deref().unwrap_or("unknown"),
                backoff.current_secs()
            );
            tokio::time::sleep(backoff.sleep_duration()).await;
            backoff.advance();
            first_connection = false;
            continue;
        }

        first_connection = false;

        // Replay pending messages
        {
            let buf = output_buffer.lock().await;
            let pending = buf.pending_count();
            if pending > 0 {
                info!("Replaying {} pending messages", pending);
                for p in buf.get_pending() {
                    let msg = ProxyToServer::SequencedOutput {
                        seq: p.seq,
                        content: p.content.clone(),
                    };
                    if let Err(e) = conn.send(&msg).await {
                        error!("Failed to replay: {}", e);
                        break;
                    }
                }
            }
        }

        // Run message loop for this connection
        let connection_start = Instant::now();
        let result = run_shim_connection(
            config,
            conn,
            &mut output_line_rx,
            &mut perm_request_rx,
            &mut stdin_line_rx,
            claude_stdin.clone(),
            permissions.clone(),
            output_buffer.clone(),
            portal_text_tx.clone(),
        )
        .await;

        // Persist buffer on disconnect
        if let Err(e) = output_buffer.lock().await.persist() {
            warn!("Failed to persist buffer: {}", e);
        }

        match result {
            ShimConnectionResult::ClaudeExited => {
                info!("Claude exited, shutting down shim");
                stdout_reader_task.abort();
                stdin_reader_task.abort();
                return;
            }
            ShimConnectionResult::Disconnected => {
                backoff.reset_if_stable(connection_start.elapsed());
                let pending = output_buffer.lock().await.pending_count();
                warn!(
                    "Portal disconnected, {} pending messages, reconnecting in {}s",
                    pending,
                    backoff.current_secs()
                );
                tokio::time::sleep(backoff.sleep_duration()).await;
                backoff.advance();
            }
            ShimConnectionResult::ServerShutdown(delay) => {
                backoff.reset();
                info!(
                    "Server shutting down, reconnecting in {}ms",
                    delay.as_millis()
                );
                tokio::time::sleep(delay).await;
            }
            ShimConnectionResult::SessionTerminated => {
                info!("Session terminated by server, not reconnecting");
                stdout_reader_task.abort();
                stdin_reader_task.abort();
                return;
            }
        }
    }
}

enum ShimConnectionResult {
    ClaudeExited,
    Disconnected,
    ServerShutdown(Duration),
    SessionTerminated,
}

/// Run the message loop for a single WebSocket connection.
#[allow(clippy::too_many_arguments)]
async fn run_shim_connection(
    config: &ProxySessionConfig,
    conn: crate::session::WebSocketConnection,
    output_line_rx: &mut mpsc::UnboundedReceiver<(u64, serde_json::Value)>,
    perm_request_rx: &mut mpsc::UnboundedReceiver<ProxyToServer>,
    stdin_line_rx: &mut mpsc::UnboundedReceiver<String>,
    claude_stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    permissions: Arc<Mutex<HashMap<String, PermissionState>>>,
    output_buffer: Arc<Mutex<PendingOutputBuffer>>,
    portal_text_tx: mpsc::UnboundedSender<String>,
) -> ShimConnectionResult {
    let (ws_write, ws_read) = conn.split();
    let ws_write: SharedWsWrite = Arc::new(Mutex::new(ws_write));

    // Single event channel for all WebSocket reader events
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<WsEvent>();

    let reader_task = crate::session::spawn_ws_reader(ws_read, ws_write.clone(), event_tx);

    // Main select loop
    let result = loop {
        tokio::select! {
            // WebSocket events from portal (unified channel)
            event = event_rx.recv() => {
                match event {
                    Some(WsEvent::Input(text)) => {
                        debug!("Portal input: {}", &text[..text.len().min(80)]);
                        let _ = portal_text_tx.send(text.clone());
                        let mut stdin = claude_stdin.lock().await;
                        let input = ClaudeInput::user_message(&text, config.session_id);
                        if let Ok(json_line) = serde_json::to_string(&input) {
                            if let Err(e) = stdin.write_all(json_line.as_bytes()).await {
                                error!("Failed to write portal input to claude: {}", e);
                                break ShimConnectionResult::ClaudeExited;
                            }
                            let _ = stdin.write_all(b"\n").await;
                            let _ = stdin.flush().await;
                        }
                    }
                    Some(WsEvent::WiggumActivation(prompt)) => {
                        debug!("Portal wiggum input: {}", &prompt[..prompt.len().min(60)]);
                        let wiggum_prompt = format!(
                            "{}\n\nTake action on the directions above until fully complete. If complete, respond only with DONE.",
                            prompt
                        );
                        let _ = portal_text_tx.send(wiggum_prompt.clone());
                        let mut stdin = claude_stdin.lock().await;
                        let input = ClaudeInput::user_message(&wiggum_prompt, config.session_id);
                        if let Ok(json_line) = serde_json::to_string(&input) {
                            if let Err(e) = stdin.write_all(json_line.as_bytes()).await {
                                error!("Failed to write wiggum input to claude: {}", e);
                                break ShimConnectionResult::ClaudeExited;
                            }
                            let _ = stdin.write_all(b"\n").await;
                            let _ = stdin.flush().await;
                        }
                    }
                    Some(WsEvent::PermissionResponse(perm_response)) => {
                        let request_id = &perm_response.request_id;
                        let should_forward = {
                            let mut perms = permissions.lock().await;
                            match perms.get(request_id) {
                                Some(PermissionState::Pending) => {
                                    *perms.get_mut(request_id).unwrap() = PermissionState::Answered;
                                    debug!("Permission {} answered by portal", request_id);
                                    true
                                }
                                Some(PermissionState::Answered) => {
                                    debug!("Permission {} already answered, ignoring portal response", request_id);
                                    false
                                }
                                None => {
                                    warn!("Unknown permission {}, forwarding", request_id);
                                    true
                                }
                            }
                        };

                        if should_forward {
                            let ctrl_response: ControlResponseMessage = build_control_response(&perm_response).into();
                            if let Ok(json_line) = serde_json::to_string(&ctrl_response) {
                                let mut stdin = claude_stdin.lock().await;
                                if let Err(e) = stdin.write_all(json_line.as_bytes()).await {
                                    error!("Failed to write permission response to claude: {}", e);
                                    break ShimConnectionResult::ClaudeExited;
                                }
                                let _ = stdin.write_all(b"\n").await;
                                let _ = stdin.flush().await;
                            }
                        }
                    }
                    Some(WsEvent::OutputAck(ack_seq)) => {
                        let mut buf = output_buffer.lock().await;
                        buf.acknowledge(ack_seq);
                        if let Err(e) = buf.persist() {
                            warn!("Failed to persist buffer after ack: {}", e);
                        }
                    }
                    Some(WsEvent::GracefulShutdown(delay_ms)) => {
                        break ShimConnectionResult::ServerShutdown(
                            Duration::from_millis(delay_ms)
                        );
                    }
                    Some(WsEvent::SessionTerminated) => {
                        break ShimConnectionResult::SessionTerminated;
                    }
                    Some(WsEvent::Interrupt) => {
                        info!("Sending interrupt to Claude");
                        let mut stdin = claude_stdin.lock().await;
                        let input = ClaudeInput::interrupt();
                        if let Ok(json_line) = serde_json::to_string(&input) {
                            if let Err(e) = stdin.write_all(json_line.as_bytes()).await {
                                error!("Failed to write interrupt to claude: {}", e);
                                break ShimConnectionResult::ClaudeExited;
                            }
                            let _ = stdin.write_all(b"\n").await;
                            let _ = stdin.flush().await;
                        }
                    }
                    Some(WsEvent::Disconnect) | None => {
                        info!("Portal WebSocket disconnected");
                        break ShimConnectionResult::Disconnected;
                    }
                }
            }

            // Claude output ready to send to portal (seq was assigned at buffer push time)
            Some((seq, content)) = output_line_rx.recv() => {
                let msg = ProxyToServer::SequencedOutput { seq, content };
                let mut ws = ws_write.lock().await;
                if let Ok(json) = serde_json::to_string(&msg) {
                    if let Err(e) = ws.send(Message::Text(json)).await {
                        error!("Failed to send output to portal: {}", e);
                        break ShimConnectionResult::Disconnected;
                    }
                }
            }

            // Permission request from claude → send as typed PermissionRequest
            Some(perm_msg) = perm_request_rx.recv() => {
                let mut ws = ws_write.lock().await;
                if let Ok(json) = serde_json::to_string(&perm_msg) {
                    if let Err(e) = ws.send(Message::Text(json)).await {
                        error!("Failed to send permission request to portal: {}", e);
                        break ShimConnectionResult::Disconnected;
                    }
                }
            }

            // Drain stdin lines (actual forwarding happens in the stdin reader task)
            _ = stdin_line_rx.recv() => {}
        }
    };

    reader_task.abort();
    result
}

/// Extract the text content from a user message echo JSON.
///
/// Expected format: `{"type":"user","message":{"role":"user","content":[{"type":"text","text":"..."}]}}`
/// Returns None if the structure doesn't match (safe fallback: caller forwards to VS Code).
fn extract_user_text(value: &serde_json::Value) -> Option<String> {
    let content = value.get("message")?.get("content")?.as_array()?;
    let mut texts = Vec::new();
    for block in content {
        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                texts.push(text);
            }
        }
    }
    if texts.is_empty() {
        None
    } else {
        Some(texts.join(""))
    }
}

/// Build a ControlResponse from a portal PermissionResponse.
/// Mirrors the logic in session.rs run_main_loop's permission handling.
fn build_control_response(perm: &PermissionResponseData) -> ControlResponse {
    use claude_codes::io::Permission;

    let input_value = perm
        .input
        .clone()
        .unwrap_or(serde_json::Value::Object(Default::default()));

    if perm.allow {
        let permissions: Vec<Permission> = perm
            .permissions
            .iter()
            .map(Permission::from_suggestion)
            .collect();

        if permissions.is_empty() {
            ControlResponse::from_result(&perm.request_id, PermissionResult::allow(input_value))
        } else {
            ControlResponse::from_result(
                &perm.request_id,
                PermissionResult::allow_with_typed_permissions(input_value, permissions),
            )
        }
    } else {
        let reason = perm
            .reason
            .clone()
            .unwrap_or_else(|| "User denied".to_string());
        ControlResponse::from_result(&perm.request_id, PermissionResult::deny(reason))
    }
}
