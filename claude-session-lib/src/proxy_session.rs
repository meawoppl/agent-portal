//! Proxy session management and WebSocket connection handling.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::session::{Session as ClaudeSession, SessionEvent};
use anyhow::Result;
use base64::Engine;
use claude_codes::io::{ContentBlock, ControlRequestPayload, ToolUseBlock};
use claude_codes::ClaudeOutput;
use shared::{ProxyToServer, SendMode, ServerToProxy, SessionEndpoint};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;

use crate::output_buffer::PendingOutputBuffer;

/// Type alias for the native WebSocket connection
type NativeConnection = ws_bridge::native_client::Connection<SessionEndpoint>;

/// Type alias for the shared WebSocket write half
type SharedWsWrite = Arc<tokio::sync::Mutex<ws_bridge::WsSender<ProxyToServer>>>;

/// Type alias for the WebSocket read half
type WsRead = ws_bridge::WsReceiver<ServerToProxy>;

/// Configuration for a proxy session
#[derive(Clone)]
pub struct ProxySessionConfig {
    pub backend_url: String,
    pub session_id: Uuid,
    pub session_name: String,
    pub auth_token: Option<String>,
    pub working_directory: String,
    pub resume: bool,
    pub git_branch: Option<String>,
    /// Extra arguments to pass through to the claude CLI
    pub claude_args: Vec<String>,
    /// If this session replaces a previous one (after SessionNotFound), the old session ID
    pub replaces_session_id: Option<Uuid>,
    /// Launcher ID if this session was started by a launcher
    pub launcher_id: Option<Uuid>,
    /// Which agent CLI to use
    pub agent_type: shared::AgentType,
}

/// Exponential backoff helper
pub struct Backoff {
    current: u64,
    initial: u64,
    max: u64,
    multiplier: u64,
    stable_threshold: u64,
}

impl Backoff {
    pub fn new() -> Self {
        Self {
            current: 1,
            initial: 1,
            max: 30,
            multiplier: 2,
            stable_threshold: 30,
        }
    }

    /// Get the current backoff duration
    pub fn current_secs(&self) -> u64 {
        self.current
    }

    /// Advance to the next backoff interval
    pub fn advance(&mut self) {
        self.current = (self.current * self.multiplier).min(self.max);
    }

    /// Reset backoff if connection was stable
    pub fn reset_if_stable(&mut self, connection_duration: Duration) {
        if connection_duration.as_secs() >= self.stable_threshold {
            info!(
                "Connection was stable for {}s, resetting backoff",
                connection_duration.as_secs()
            );
            self.current = self.initial;
        }
    }

    /// Reset backoff to initial value unconditionally
    pub fn reset(&mut self) {
        self.current = self.initial;
    }

    /// Get a sleep duration
    pub fn sleep_duration(&self) -> Duration {
        Duration::from_secs(self.current)
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Result from a single WebSocket connection attempt
pub enum ConnectionResult {
    /// Claude process exited normally
    ClaudeExited,
    /// WebSocket disconnected, includes how long the connection was up
    Disconnected(Duration),
    /// Session not found error - need to restart with fresh session
    SessionNotFound,
    /// Server is shutting down gracefully, includes suggested reconnect delay
    ServerShutdown(Duration),
}

/// Result from the connection loop
pub enum LoopResult {
    /// Normal exit (Claude process ended)
    NormalExit,
    /// Session not found - caller should restart with fresh session
    SessionNotFound,
}

/// State that persists across WebSocket reconnections for a session.
/// This includes the input channel, output buffer, and session config.
pub struct SessionState<'a> {
    /// Session configuration
    pub config: &'a ProxySessionConfig,
    /// Claude session from claude-session-lib
    pub claude_session: &'a mut ClaudeSession,
    /// Sender for input messages (cloned per connection)
    pub input_tx: mpsc::UnboundedSender<String>,
    /// Receiver for input messages (persists across connections)
    pub input_rx: &'a mut mpsc::UnboundedReceiver<String>,
    /// Output buffer with persistence
    pub output_buffer: Arc<Mutex<PendingOutputBuffer>>,
    /// Backoff state for reconnection
    pub backoff: Backoff,
    /// Whether this is the first connection attempt
    pub first_connection: bool,
    /// When the last disconnect occurred (for reporting reconnect duration)
    pub disconnected_at: Option<Instant>,
    /// Whether the last disconnect was a graceful server shutdown
    pub last_disconnect_graceful: bool,
}

impl<'a> SessionState<'a> {
    /// Create a new session state
    pub fn new(
        config: &'a ProxySessionConfig,
        claude_session: &'a mut ClaudeSession,
        input_tx: mpsc::UnboundedSender<String>,
        input_rx: &'a mut mpsc::UnboundedReceiver<String>,
    ) -> Result<Self> {
        let output_buffer = match PendingOutputBuffer::new(config.session_id) {
            Ok(buf) => buf,
            Err(e) => {
                warn!(
                    "Failed to create output buffer, continuing without persistence: {}",
                    e
                );
                PendingOutputBuffer::new(config.session_id)?
            }
        };
        let output_buffer = Arc::new(Mutex::new(output_buffer));

        Ok(Self {
            config,
            claude_session,
            input_tx,
            input_rx,
            output_buffer,
            backoff: Backoff::new(),
            first_connection: true,
            disconnected_at: None,
            last_disconnect_graceful: false,
        })
    }

    /// Log pending messages from previous session
    pub async fn log_pending_messages(&self) {
        let buf = self.output_buffer.lock().await;
        let pending = buf.pending_count();
        if pending > 0 {
            info!(
                "Loaded {} pending messages from previous session, will replay on connect",
                pending
            );
        }
    }

    /// Persist the output buffer
    pub async fn persist_buffer(&self) {
        if let Err(e) = self.output_buffer.lock().await.persist() {
            warn!("Failed to persist output buffer: {}", e);
        }
    }

    /// Get pending message count
    pub async fn pending_count(&self) -> usize {
        self.output_buffer.lock().await.pending_count()
    }
}

/// Run the WebSocket connection loop with auto-reconnect
pub async fn run_connection_loop(
    config: &ProxySessionConfig,
    claude_session: &mut ClaudeSession,
    input_tx: mpsc::UnboundedSender<String>,
    input_rx: &mut mpsc::UnboundedReceiver<String>,
) -> Result<LoopResult> {
    let mut session = SessionState::new(config, claude_session, input_tx, input_rx)?;
    session.log_pending_messages().await;

    loop {
        if session.first_connection {
            info!("Proxy ready");
        }

        let result = run_single_connection(&mut session).await;
        session.first_connection = false;

        match result {
            ConnectionResult::ClaudeExited => {
                info!("Claude process exited, shutting down");
                session.persist_buffer().await;
                return Ok(LoopResult::NormalExit);
            }
            ConnectionResult::SessionNotFound => {
                warn!("Session not found, need to restart with fresh session");
                session.persist_buffer().await;
                return Ok(LoopResult::SessionNotFound);
            }
            ConnectionResult::Disconnected(duration) => {
                session.disconnected_at.get_or_insert(Instant::now());
                session.last_disconnect_graceful = false;
                session.backoff.reset_if_stable(duration);
                session.persist_buffer().await;

                let pending = session.pending_count().await;
                warn!(
                    "WebSocket disconnected, {} pending messages, reconnecting in {}s",
                    pending,
                    session.backoff.current_secs()
                );

                tokio::time::sleep(session.backoff.sleep_duration()).await;
                session.backoff.advance();
            }
            ConnectionResult::ServerShutdown(delay) => {
                session.disconnected_at.get_or_insert(Instant::now());
                session.last_disconnect_graceful = true;
                // Graceful shutdown - reset backoff and use server's suggested delay
                session.backoff.reset();
                session.persist_buffer().await;

                let pending = session.pending_count().await;
                let delay_secs = delay.as_secs().max(1);
                info!(
                    "Server shutting down, {} pending messages, reconnecting in {}s",
                    pending, delay_secs
                );

                tokio::time::sleep(delay).await;
            }
        }
    }
}

/// Run a single WebSocket connection until it disconnects or Claude exits
async fn run_single_connection(session: &mut SessionState<'_>) -> ConnectionResult {
    // Connect to WebSocket
    let mut conn =
        match connect_to_backend(&session.config.backend_url, session.first_connection).await {
            Ok(conn) => conn,
            Err(duration) => return ConnectionResult::Disconnected(duration),
        };

    // Re-detect git branch on reconnect (it may have changed)
    let current_branch = get_git_branch(&session.config.working_directory);
    let config_with_branch = ProxySessionConfig {
        git_branch: current_branch,
        ..session.config.clone()
    };

    // Register with backend and wait for acknowledgment
    if let Err(duration) = register_session(&mut conn, &config_with_branch).await {
        return ConnectionResult::Disconnected(duration);
    }

    // Look up PR URL for the current branch and send as SessionUpdate
    if let Some(ref branch) = config_with_branch.git_branch {
        let pr_url = get_pr_url(&session.config.working_directory, branch);
        if pr_url.is_some() {
            let update_msg = ProxyToServer::SessionUpdate {
                session_id: config_with_branch.session_id,
                git_branch: config_with_branch.git_branch.clone(),
                pr_url,
            };
            if conn.send(update_msg).await.is_err() {
                error!("Failed to send initial PR URL update");
            }
        }
    }

    // Replay pending messages after successful registration
    {
        let buf = session.output_buffer.lock().await;
        let pending_count = buf.pending_count();
        if pending_count > 0 {
            debug!(
                "Replaying {} pending messages after reconnect",
                pending_count
            );
            for pending in buf.get_pending() {
                let msg = ProxyToServer::SequencedOutput {
                    seq: pending.seq,
                    content: pending.content.clone(),
                };
                if conn.send(msg).await.is_err() {
                    error!("Failed to replay pending message seq={}", pending.seq);
                    return ConnectionResult::Disconnected(Duration::ZERO);
                }
            }
            debug!("Finished replaying pending messages");
        }
    }

    // Send a portal message so the frontend shows connection status
    {
        let text = if session.first_connection {
            "Proxy connected".to_string()
        } else {
            let duration_str = session
                .disconnected_at
                .map(|t| {
                    let secs = t.elapsed().as_secs();
                    if secs < 60 {
                        format!("{}s", secs)
                    } else {
                        format!("{}m {}s", secs / 60, secs % 60)
                    }
                })
                .unwrap_or_default();
            let reason = if session.last_disconnect_graceful {
                "server restart"
            } else {
                "unexpected disconnect"
            };
            if duration_str.is_empty() {
                format!("Proxy reconnected ({})", reason)
            } else {
                format!("Proxy reconnected after {} ({})", duration_str, reason)
            }
        };
        let portal_content = shared::PortalMessage::text(text).to_json();
        let seq = {
            let mut buf = session.output_buffer.lock().await;
            buf.push(portal_content.clone())
        };
        let msg = ProxyToServer::SequencedOutput {
            seq,
            content: portal_content,
        };
        if conn.send(msg).await.is_err() {
            error!("Failed to send connection portal message");
            return ConnectionResult::Disconnected(Duration::ZERO);
        }
    }

    if !session.first_connection {
        info!("Connection restored");
        session.disconnected_at = None;
    }

    // Run the message loop - split connection for concurrent read/write
    run_message_loop(session, &config_with_branch, conn).await
}

/// Connect to the backend WebSocket
async fn connect_to_backend(
    backend_url: &str,
    first_connection: bool,
) -> Result<NativeConnection, Duration> {
    if first_connection {
        info!("Connecting to backend...");
    } else {
        info!("Reconnecting to backend...");
    }

    match ws_bridge::native_client::connect::<SessionEndpoint>(backend_url).await {
        Ok(conn) => {
            info!("Connected to backend");
            Ok(conn)
        }
        Err(e) => {
            error!("Failed to connect to backend: {}", e);
            Err(Duration::ZERO)
        }
    }
}

/// Register session with the backend and wait for acknowledgment
async fn register_session(
    conn: &mut NativeConnection,
    config: &ProxySessionConfig,
) -> Result<(), Duration> {
    info!("Registering session...");

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());

    let register_msg = ProxyToServer::Register(shared::RegisterFields {
        session_id: config.session_id,
        session_name: config.session_name.clone(),
        auth_token: config.auth_token.clone(),
        working_directory: config.working_directory.clone(),
        resuming: config.resume,
        git_branch: config.git_branch.clone(),
        replay_after: None, // Proxy doesn't need history replay
        client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        replaces_session_id: config.replaces_session_id,
        hostname: Some(hostname),
        launcher_id: config.launcher_id,
        agent_type: config.agent_type,
    });

    if conn.send(register_msg).await.is_err() {
        error!("Failed to send registration message");
        return Err(Duration::ZERO);
    }

    // Wait for RegisterAck with timeout
    let ack_timeout = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(result) = conn.recv().await {
            match result {
                Ok(ServerToProxy::RegisterAck {
                    success,
                    session_id: _,
                    error,
                }) => {
                    return Some((success, error));
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        None
    })
    .await;

    match ack_timeout {
        Ok(Some((true, _))) => {
            info!("Session registered");
            Ok(())
        }
        Ok(Some((false, error))) => {
            let err_msg = error.as_deref().unwrap_or("Unknown error");
            error!("Registration failed: {}", err_msg);
            Err(Duration::ZERO)
        }
        Ok(None) => {
            error!("Connection closed during registration");
            Err(Duration::ZERO)
        }
        Err(_) => {
            // Timeout - assume success for backwards compatibility with older backends
            info!(
                "No RegisterAck received (timeout), assuming success for backwards compatibility"
            );
            Ok(())
        }
    }
}

/// Permission response data (from frontend to Claude)
#[derive(Debug)]
pub struct PermissionResponseData {
    pub request_id: String,
    pub allow: bool,
    pub input: Option<serde_json::Value>,
    pub permissions: Vec<claude_codes::io::PermissionSuggestion>,
    pub reason: Option<String>,
}

/// Maximum iterations for wiggum mode before auto-stopping
const WIGGUM_MAX_ITERATIONS: u32 = 50;

/// Wiggum mode state
#[derive(Debug, Clone)]
pub struct WiggumState {
    /// Original user prompt (before modification)
    pub original_prompt: String,
    /// Current iteration count
    pub iteration: u32,
    /// When the current loop iteration started
    pub loop_start: Instant,
    /// Durations of the last N loop iterations (most recent last)
    pub loop_durations: Vec<Duration>,
}

/// Signal for graceful server shutdown with recommended reconnect delay
pub struct GracefulShutdown {
    pub reconnect_delay_ms: u64,
}

/// Events sent through the file upload channel from the WS reader to the main loop
pub enum FileUploadEvent {
    /// A new file upload is starting
    Start {
        upload_id: String,
        filename: String,
        total_chunks: u32,
        total_size: u64,
    },
    /// A chunk of file data (base64-encoded)
    Chunk {
        upload_id: String,
        chunk_index: u32,
        data: String,
    },
}

/// Tracks state for a file being received in chunks
pub(crate) struct FileReceiveState {
    filename: String,
    total_chunks: u32,
    total_size: u64,
    received_chunks: u32,
    received_bytes: u64,
    file_handle: Option<tokio::fs::File>,
    start_time: Instant,
    last_log_percent: u32,
}

/// Result from handling a WebSocket text message
enum WsMessageResult {
    /// Continue processing messages
    Continue,
    /// Disconnect (error or other reason)
    Disconnect,
    /// Server requested graceful shutdown with specified delay in ms
    GracefulShutdown(u64),
}

/// State for the main message loop, reducing parameter count
/// Contains channels and state that are specific to a single connection attempt.
/// Note: input_rx is passed separately as it persists across reconnections.
pub struct ConnectionState {
    /// Receiver for permission responses from frontend
    pub perm_rx: mpsc::UnboundedReceiver<PermissionResponseData>,
    /// Receiver for output acknowledgments from backend
    pub ack_rx: mpsc::UnboundedReceiver<u64>,
    /// Sender for Claude outputs to the output forwarder
    pub output_tx: mpsc::UnboundedSender<ClaudeOutput>,
    /// WebSocket write handle for sending permission requests directly
    pub ws_write: SharedWsWrite,
    /// Receiver to detect WebSocket disconnection
    pub disconnect_rx: tokio::sync::oneshot::Receiver<()>,
    /// Receiver for graceful server shutdown signal
    pub graceful_shutdown_rx: mpsc::UnboundedReceiver<GracefulShutdown>,
    /// When the connection was established
    pub connection_start: Instant,
    /// Buffer for pending outputs
    pub output_buffer: Arc<Mutex<PendingOutputBuffer>>,
    /// Receiver for wiggum mode activation
    pub wiggum_rx: mpsc::UnboundedReceiver<String>,
    /// Current wiggum state (if active)
    pub wiggum_state: Option<WiggumState>,
    /// Heartbeat tracker for dead connection detection
    pub heartbeat: crate::heartbeat::HeartbeatTracker,
    /// Receiver for file upload events from backend
    pub file_upload_rx: mpsc::UnboundedReceiver<FileUploadEvent>,
    /// Working directory for file uploads
    pub working_directory: String,
    /// Active file uploads being received in chunks
    pub(crate) active_uploads: std::collections::HashMap<String, FileReceiveState>,
}

/// Run the main message forwarding loop
async fn run_message_loop(
    session: &mut SessionState<'_>,
    config: &ProxySessionConfig,
    conn: NativeConnection,
) -> ConnectionResult {
    let connection_start = Instant::now();
    let session_id = config.session_id;

    // Split connection for concurrent read/write
    let (ws_write, ws_read) = conn.split();

    // Channel for Claude outputs
    let (output_tx, output_rx) = mpsc::unbounded_channel::<ClaudeOutput>();

    // Channel for permission responses from frontend
    let (perm_tx, perm_rx) = mpsc::unbounded_channel::<PermissionResponseData>();

    // Channel for output acknowledgments from backend
    let (ack_tx, ack_rx) = mpsc::unbounded_channel::<u64>();

    // Channel for wiggum mode activation
    let (wiggum_tx, wiggum_rx) = mpsc::unbounded_channel::<String>();

    // Channel for graceful server shutdown signals
    let (graceful_shutdown_tx, graceful_shutdown_rx) =
        mpsc::unbounded_channel::<GracefulShutdown>();

    // Channel for file upload events from backend
    let (file_upload_tx, file_upload_rx) = mpsc::unbounded_channel::<FileUploadEvent>();

    // Wrap ws_write for sharing
    let ws_write = std::sync::Arc::new(tokio::sync::Mutex::new(ws_write));

    // Heartbeat tracker for dead connection detection
    let heartbeat = crate::heartbeat::HeartbeatTracker::new();

    // Channel to signal WebSocket disconnection
    let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel::<()>();

    // Shared state for tracking git branch and PR URL updates
    let current_branch = Arc::new(Mutex::new(config.git_branch.clone()));
    let current_pr_url = Arc::new(Mutex::new(None::<String>));

    // Spawn output forwarder task with buffer
    let output_task = spawn_output_forwarder(
        output_rx,
        ws_write.clone(),
        session_id,
        config.working_directory.clone(),
        current_branch,
        current_pr_url,
        session.output_buffer.clone(),
    );

    // Spawn WebSocket reader task
    let reader_task = spawn_ws_reader(
        ws_read,
        session.input_tx.clone(),
        perm_tx,
        ack_tx,
        ws_write.clone(),
        disconnect_tx,
        wiggum_tx,
        graceful_shutdown_tx,
        heartbeat.clone(),
        file_upload_tx,
    );

    // Create connection state (per-connection channels and timing)
    let mut conn_state = ConnectionState {
        perm_rx,
        ack_rx,
        output_tx,
        ws_write: ws_write.clone(),
        disconnect_rx,
        graceful_shutdown_rx,
        connection_start,
        output_buffer: session.output_buffer.clone(),
        wiggum_rx,
        wiggum_state: None,
        heartbeat,
        file_upload_rx,
        working_directory: config.working_directory.clone(),
        active_uploads: std::collections::HashMap::new(),
    };

    // Main loop
    let result = run_main_loop(session.claude_session, session.input_rx, &mut conn_state).await;

    // Clean up
    output_task.abort();
    reader_task.abort();

    result
}

/// Get the current git branch name, if in a git repository
fn get_git_branch(cwd: &str) -> Option<String> {
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

/// Check if a tool use is a Bash command containing "git"
fn is_git_bash_command(output: &ClaudeOutput) -> bool {
    if let ClaudeOutput::User(user) = output {
        for block in &user.message.content {
            if let ContentBlock::ToolResult(tr) = block {
                // Check if this is a result from a Bash tool
                // We check tool_use_id pattern or content for git indicators
                // The safest way is to track pending tool calls, but for simplicity
                // we check if the result content mentions git commands
                if let Some(ref content) = tr.content {
                    let content_str = format!("{:?}", content);
                    // Check if this looks like output from a git command
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

/// Look up the GitHub PR URL for a branch using the `gh` CLI
fn get_pr_url(cwd: &str, branch: &str) -> Option<String> {
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

/// Check and send git branch or PR URL update if changed
async fn check_and_send_branch_update(
    ws_write: &SharedWsWrite,
    session_id: Uuid,
    working_directory: &str,
    current_branch: &Arc<Mutex<Option<String>>>,
    current_pr_url: &Arc<Mutex<Option<String>>>,
) {
    let new_branch = get_git_branch(working_directory);
    let new_pr_url = new_branch
        .as_deref()
        .and_then(|b| get_pr_url(working_directory, b));

    let mut branch_guard = current_branch.lock().await;
    let mut pr_guard = current_pr_url.lock().await;

    let branch_changed = *branch_guard != new_branch;
    let pr_changed = *pr_guard != new_pr_url;

    if branch_changed || pr_changed {
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

        // Drop locks before acquiring ws lock
        drop(branch_guard);
        drop(pr_guard);

        let update_msg = ProxyToServer::SessionUpdate {
            session_id,
            git_branch: new_branch,
            pr_url: new_pr_url,
        };

        let mut ws = ws_write.lock().await;
        if let Err(e) = ws.send(update_msg).await {
            error!("Failed to send branch update: {}", e);
        }
    }
}

/// Default 2 MB limit on image file size for portal messages.
/// Override with PORTAL_MAX_IMAGE_MB environment variable.
const DEFAULT_MAX_IMAGE_MB: usize = 2;

fn max_image_bytes() -> usize {
    std::env::var("PORTAL_MAX_IMAGE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_MAX_IMAGE_MB)
        * 1024
        * 1024
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

/// Check user messages for tool results that correspond to tracked image reads.
/// For each match, reads the file from disk, base64-encodes it, and returns a PortalMessage.
fn extract_image_portal_messages(
    output: &ClaudeOutput,
    image_read_map: &mut HashMap<String, String>,
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
                        let max_bytes = max_image_bytes();
                        if data.len() > max_bytes {
                            let size_mb = data.len() as f64 / (1024.0 * 1024.0);
                            let limit_mb = max_bytes as f64 / (1024.0 * 1024.0);
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

/// Spawn the output forwarder task
///
/// Forwards Claude outputs to WebSocket with sequence numbers for reliable delivery.
fn spawn_output_forwarder(
    mut output_rx: mpsc::UnboundedReceiver<ClaudeOutput>,
    ws_write: SharedWsWrite,
    session_id: Uuid,
    working_directory: String,
    current_branch: Arc<Mutex<Option<String>>>,
    current_pr_url: Arc<Mutex<Option<String>>>,
    output_buffer: Arc<Mutex<PendingOutputBuffer>>,
) -> tokio::task::JoinHandle<()> {
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
            let portal_messages = extract_image_portal_messages(&output, &mut image_read_map);

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
        }
        ClaudeOutput::Assistant(asst) => {
            let msg = &asst.message;
            let stop = msg.stop_reason.as_deref().unwrap_or("none");

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

/// Truncate a string to max length, adding "..." if truncated
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        // Find a safe UTF-8 boundary
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Format duration in ms to human readable
fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{}ms", ms)
    } else if ms < 60000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60000;
        let secs = (ms % 60000) / 1000;
        format!("{}m{}s", mins, secs)
    }
}

/// Spawn the WebSocket reader task
#[allow(clippy::too_many_arguments)] // TODO: refactor to event enum (issue #271)
fn spawn_ws_reader(
    mut ws_read: WsRead,
    input_tx: mpsc::UnboundedSender<String>,
    perm_tx: mpsc::UnboundedSender<PermissionResponseData>,
    ack_tx: mpsc::UnboundedSender<u64>,
    ws_write: SharedWsWrite,
    disconnect_tx: tokio::sync::oneshot::Sender<()>,
    wiggum_tx: mpsc::UnboundedSender<String>,
    graceful_shutdown_tx: mpsc::UnboundedSender<GracefulShutdown>,
    heartbeat: crate::heartbeat::HeartbeatTracker,
    file_upload_tx: mpsc::UnboundedSender<FileUploadEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(result) = ws_read.recv().await {
            match result {
                Ok(msg) => {
                    match handle_ws_message(
                        msg,
                        &input_tx,
                        &perm_tx,
                        &ack_tx,
                        &ws_write,
                        &wiggum_tx,
                        &heartbeat,
                        &file_upload_tx,
                    )
                    .await
                    {
                        WsMessageResult::Continue => {}
                        WsMessageResult::Disconnect => break,
                        WsMessageResult::GracefulShutdown(delay_ms) => {
                            let _ = graceful_shutdown_tx.send(GracefulShutdown {
                                reconnect_delay_ms: delay_ms,
                            });
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    break;
                }
            }
        }
        debug!("WebSocket reader ended");
        let _ = disconnect_tx.send(());
    })
}

/// Handle a typed message from the WebSocket
#[allow(clippy::too_many_arguments)]
async fn handle_ws_message(
    proxy_msg: ServerToProxy,
    input_tx: &mpsc::UnboundedSender<String>,
    perm_tx: &mpsc::UnboundedSender<PermissionResponseData>,
    ack_tx: &mpsc::UnboundedSender<u64>,
    ws_write: &SharedWsWrite,
    wiggum_tx: &mpsc::UnboundedSender<String>,
    heartbeat: &crate::heartbeat::HeartbeatTracker,
    file_upload_tx: &mpsc::UnboundedSender<FileUploadEvent>,
) -> WsMessageResult {
    if !matches!(
        proxy_msg,
        ServerToProxy::Heartbeat | ServerToProxy::FileUploadChunk(..)
    ) {
        debug!("ws recv: {:?}", proxy_msg);
    }

    match proxy_msg {
        ServerToProxy::ClaudeInput { content, send_mode } => {
            let user_text = match &content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };

            if send_mode == Some(SendMode::Wiggum) {
                debug!("→ [input/wiggum] {}", truncate(&user_text, 80));
                // Only send to wiggum_tx — the select loop sets state and sends prompt atomically
                if wiggum_tx.send(user_text).is_err() {
                    error!("Failed to send wiggum activation");
                    return WsMessageResult::Disconnect;
                }
            } else {
                debug!("→ [input] {}", truncate(&user_text, 80));
                if input_tx.send(user_text).is_err() {
                    error!("Failed to send input to channel");
                    return WsMessageResult::Disconnect;
                }
            }
        }
        ServerToProxy::SequencedInput {
            session_id,
            seq,
            content,
            send_mode,
        } => {
            let user_text = match &content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };

            if send_mode == Some(SendMode::Wiggum) {
                debug!(
                    "→ [seq_input/wiggum] seq={} {}",
                    seq,
                    truncate(&user_text, 80)
                );
                // Only send to wiggum_tx — the select loop sets state and sends prompt atomically
                if wiggum_tx.send(user_text).is_err() {
                    error!("Failed to send wiggum activation");
                    return WsMessageResult::Disconnect;
                }
            } else {
                debug!("→ [seq_input] seq={} {}", seq, truncate(&user_text, 80));
                if input_tx.send(user_text).is_err() {
                    error!("Failed to send input to channel");
                    return WsMessageResult::Disconnect;
                }
            }

            // Send InputAck back to backend
            let ack = ProxyToServer::InputAck {
                session_id,
                ack_seq: seq,
            };
            let mut ws = ws_write.lock().await;
            if let Err(e) = ws.send(ack).await {
                error!("Failed to send InputAck: {}", e);
            }
        }
        ServerToProxy::PermissionResponse(shared::PermissionResponseFields {
            request_id,
            allow,
            input,
            permissions,
            reason,
        }) => {
            debug!(
                "→ [perm_response] {} allow={} permissions={} reason={:?}",
                request_id,
                allow,
                permissions.len(),
                reason
            );
            if perm_tx
                .send(PermissionResponseData {
                    request_id,
                    allow,
                    input,
                    permissions,
                    reason,
                })
                .is_err()
            {
                error!("Failed to send permission response to channel");
                return WsMessageResult::Disconnect;
            }
        }
        ServerToProxy::OutputAck {
            session_id: _,
            ack_seq,
        } => {
            debug!("→ [output_ack] seq={}", ack_seq);
            if ack_tx.send(ack_seq).is_err() {
                error!("Failed to send output ack to channel");
                return WsMessageResult::Disconnect;
            }
        }
        ServerToProxy::Heartbeat => {
            trace!("heartbeat");
            heartbeat.received();
        }
        ServerToProxy::ServerShutdown {
            reason,
            reconnect_delay_ms,
        } => {
            warn!(
                "Server shutting down: {} (reconnecting in {}ms)",
                reason, reconnect_delay_ms
            );
            return WsMessageResult::GracefulShutdown(reconnect_delay_ms);
        }
        ServerToProxy::FileUploadStart(shared::FileUploadStartFields {
            upload_id,
            filename,
            content_type: _,
            total_chunks,
            total_size,
        }) => {
            info!(
                "[upload {}] Starting: {} ({} bytes, {} chunks)",
                &upload_id[..8.min(upload_id.len())],
                filename,
                total_size,
                total_chunks
            );
            if file_upload_tx
                .send(FileUploadEvent::Start {
                    upload_id,
                    filename,
                    total_chunks,
                    total_size,
                })
                .is_err()
            {
                error!("Failed to send file upload start to main loop");
                return WsMessageResult::Disconnect;
            }
        }
        ServerToProxy::FileUploadChunk(shared::FileUploadChunkFields {
            upload_id,
            chunk_index,
            data,
        }) => {
            if file_upload_tx
                .send(FileUploadEvent::Chunk {
                    upload_id,
                    chunk_index,
                    data,
                })
                .is_err()
            {
                error!("Failed to send file upload chunk to main loop");
                return WsMessageResult::Disconnect;
            }
        }
        _ => {
            debug!("ws msg: {:?}", proxy_msg);
        }
    }

    WsMessageResult::Continue
}

/// Run the main select loop
///
/// The Claude session internally uses a dedicated drain task to continuously
/// read stdout, so there's no risk of buffer starvation in this select! loop.
/// See: https://github.com/meawoppl/claude-code-portal/issues/278
async fn run_main_loop(
    claude_session: &mut ClaudeSession,
    input_rx: &mut mpsc::UnboundedReceiver<String>,
    state: &mut ConnectionState,
) -> ConnectionResult {
    use crate::session::PermissionResponse as LibPermissionResponse;
    use crate::Permission;

    let mut heartbeat_interval = tokio::time::interval(crate::heartbeat::HEARTBEAT_INTERVAL);

    loop {
        tokio::select! {
            _ = heartbeat_interval.tick() => {
                if state.heartbeat.is_expired() {
                    warn!(
                        "No heartbeat response in {}s, forcing reconnect",
                        state.heartbeat.elapsed_secs()
                    );
                    return ConnectionResult::Disconnected(state.connection_start.elapsed());
                }
                let mut ws = state.ws_write.lock().await;
                let _ = ws.send(ProxyToServer::Heartbeat).await;
            }

            _ = &mut state.disconnect_rx => {
                // Check if a graceful shutdown was queued before the disconnect
                if let Ok(shutdown) = state.graceful_shutdown_rx.try_recv() {
                    info!("Server graceful shutdown, will reconnect in {}ms", shutdown.reconnect_delay_ms);
                    return ConnectionResult::ServerShutdown(Duration::from_millis(shutdown.reconnect_delay_ms));
                }
                info!("WebSocket disconnected");
                return ConnectionResult::Disconnected(state.connection_start.elapsed());
            }

            Some(shutdown) = state.graceful_shutdown_rx.recv() => {
                info!("Server graceful shutdown, will reconnect in {}ms", shutdown.reconnect_delay_ms);
                return ConnectionResult::ServerShutdown(Duration::from_millis(shutdown.reconnect_delay_ms));
            }

            Some(text) = input_rx.recv() => {
                debug!("sending to claude process: {}", truncate(&text, 100));

                if let Err(e) = claude_session.send_input(serde_json::Value::String(text)).await {
                    error!("Failed to send to Claude: {}", e);
                    return ConnectionResult::ClaudeExited;
                }
            }

            // Wiggum mode activation — set state and send prompt atomically
            Some(original_prompt) = state.wiggum_rx.recv() => {
                info!("Wiggum mode activated with prompt: {}", truncate(&original_prompt, 60));
                let wiggum_prompt = format!(
                    "{}\n\nTake action on the directions above until fully complete. If complete, respond only with DONE.",
                    original_prompt
                );
                state.wiggum_state = Some(WiggumState {
                    original_prompt,
                    iteration: 1,
                    loop_start: Instant::now(),
                    loop_durations: Vec::new(),
                });
                if let Err(e) = claude_session.send_input(serde_json::Value::String(wiggum_prompt)).await {
                    error!("Failed to send wiggum prompt to Claude: {}", e);
                    return ConnectionResult::ClaudeExited;
                }
            }

            Some(upload_event) = state.file_upload_rx.recv() => {
                match upload_event {
                    FileUploadEvent::Start { upload_id, filename, total_chunks, total_size } => {
                        // Sanitize filename
                        let safe_name: String = filename
                            .rsplit('/').next()
                            .or_else(|| filename.rsplit('\\').next())
                            .unwrap_or(&filename)
                            .chars()
                            .filter(|c| *c != '/' && *c != '\\' && *c != '\0')
                            .collect();
                        let safe_name = if safe_name.is_empty() || safe_name == "." || safe_name == ".." {
                            "uploaded_file".to_string()
                        } else {
                            safe_name
                        };

                        let file_path = std::path::Path::new(&state.working_directory).join(&safe_name);
                        match tokio::fs::File::create(&file_path).await {
                            Ok(fh) => {
                                state.active_uploads.insert(upload_id, FileReceiveState {
                                    filename: safe_name,
                                    total_chunks,
                                    total_size,
                                    received_chunks: 0,
                                    received_bytes: 0,
                                    file_handle: Some(fh),
                                    start_time: Instant::now(),
                                    last_log_percent: 0,
                                });
                            }
                            Err(e) => {
                                error!("Failed to create file {}: {}", file_path.display(), e);
                            }
                        }
                    }
                    FileUploadEvent::Chunk { upload_id, chunk_index: _, data } => {
                        use tokio::io::AsyncWriteExt;

                        let upload_id_short = &upload_id[..8.min(upload_id.len())];
                        let Some(recv_state) = state.active_uploads.get_mut(&upload_id) else {
                            warn!("[upload {}] Chunk for unknown upload", upload_id_short);
                            continue;
                        };

                        let decoded = match base64::engine::general_purpose::STANDARD.decode(&data) {
                            Ok(b) => b,
                            Err(e) => {
                                error!("[upload {}] Base64 decode error: {}", upload_id_short, e);
                                continue;
                            }
                        };

                        if let Some(ref mut fh) = recv_state.file_handle {
                            if let Err(e) = fh.write_all(&decoded).await {
                                error!("[upload {}] Write error: {}", upload_id_short, e);
                                continue;
                            }
                        }

                        recv_state.received_chunks += 1;
                        recv_state.received_bytes += decoded.len() as u64;

                        // Log every 10% milestone
                        let percent = if recv_state.total_size > 0 {
                            ((recv_state.received_bytes as f64 / recv_state.total_size as f64) * 100.0) as u32
                        } else {
                            100
                        };
                        let log_threshold = (percent / 10) * 10;
                        if log_threshold > recv_state.last_log_percent {
                            let elapsed = recv_state.start_time.elapsed().as_secs_f64();
                            let rate_kb = if elapsed > 0.0 {
                                recv_state.received_bytes as f64 / elapsed / 1024.0
                            } else {
                                0.0
                            };
                            info!(
                                "[upload {}] {} - {}% ({}/{} bytes) - {:.1} KB/s",
                                upload_id_short,
                                recv_state.filename,
                                log_threshold.min(100),
                                recv_state.received_bytes,
                                recv_state.total_size,
                                rate_kb
                            );
                            recv_state.last_log_percent = log_threshold;
                        }

                        // Check if complete
                        if recv_state.received_chunks >= recv_state.total_chunks {
                            use tokio::io::AsyncWriteExt;

                            let elapsed = recv_state.start_time.elapsed().as_secs_f64();
                            let rate_kb = if elapsed > 0.0 {
                                recv_state.received_bytes as f64 / elapsed / 1024.0
                            } else {
                                0.0
                            };

                            // Flush and close file
                            if let Some(mut fh) = recv_state.file_handle.take() {
                                let _ = fh.flush().await;
                            }

                            let filename = recv_state.filename.clone();
                            info!(
                                "[upload {}] Complete: {} ({} bytes in {:.1}s, avg {:.1} KB/s)",
                                upload_id_short, filename, recv_state.received_bytes, elapsed, rate_kb
                            );
                            state.active_uploads.remove(&upload_id);
                        }
                    }
                }
            }

            Some(perm_response) = state.perm_rx.recv() => {
                debug!("sending permission response to claude: {:?}", perm_response);

                // Build the library's PermissionResponse
                let lib_response = if perm_response.allow {
                    let input = perm_response.input.unwrap_or(serde_json::Value::Object(Default::default()));
                    let permissions: Vec<Permission> = perm_response
                        .permissions
                        .iter()
                        .map(Permission::from_suggestion)
                        .collect();

                    if permissions.is_empty() {
                        LibPermissionResponse::allow_with_input(input)
                    } else {
                        LibPermissionResponse::allow_with_input_and_remember(input, permissions)
                    }
                } else {
                    let reason = perm_response.reason.unwrap_or_else(|| "User denied".to_string());
                    LibPermissionResponse::deny_with_reason(reason)
                };

                if let Err(e) = claude_session.respond_permission(&perm_response.request_id, lib_response).await {
                    warn!("Permission response failed (stale request?): {}", e);
                }
            }

            Some(ack_seq) = state.ack_rx.recv() => {
                // Acknowledge receipt of messages from backend
                let mut buf = state.output_buffer.lock().await;
                buf.acknowledge(ack_seq);
                // Persist periodically (on every ack for now, could be batched)
                if let Err(e) = buf.persist() {
                    warn!("Failed to persist buffer after ack: {}", e);
                }
            }

            event = claude_session.next_event() => {
                // Handle raw output directly (Codex JSONL) — bypasses the
                // ClaudeOutput-typed output forwarder
                if let Some(SessionEvent::RawOutput(ref value)) = event {
                    let seq = {
                        let mut buf = state.output_buffer.lock().await;
                        buf.push(value.clone())
                    };
                    let msg = ProxyToServer::SequencedOutput {
                        seq,
                        content: value.clone(),
                    };
                    let mut ws = state.ws_write.lock().await;
                    if ws.send(msg).await.is_err() {
                        error!("Failed to send raw output");
                        return ConnectionResult::Disconnected(state.connection_start.elapsed());
                    }
                    continue;
                }

                match handle_session_event_with_wiggum(
                    event,
                    &state.output_tx,
                    &state.ws_write,
                    state.connection_start,
                    &mut state.wiggum_state,
                    &state.output_buffer,
                    claude_session,
                ).await {
                    Some(result) => return result,
                    None => continue,
                }
            }
        }
    }
}

/// Handle a session event from claude-session-lib, with wiggum loop support
async fn handle_session_event_with_wiggum(
    event: Option<SessionEvent>,
    output_tx: &mpsc::UnboundedSender<ClaudeOutput>,
    ws_write: &SharedWsWrite,
    connection_start: Instant,
    wiggum_state: &mut Option<WiggumState>,
    output_buffer: &Arc<Mutex<PendingOutputBuffer>>,
    claude_session: &mut ClaudeSession,
) -> Option<ConnectionResult> {
    match event {
        Some(SessionEvent::Output(ref output)) => {
            // Check for wiggum completion before forwarding
            let should_continue_wiggum = if let ClaudeOutput::Result(ref result) = output {
                if let Some(ref state) = wiggum_state {
                    // Check if Claude responded with "DONE"
                    let is_done = check_wiggum_done(result);
                    if is_done {
                        info!("Wiggum mode complete after {} iterations", state.iteration);
                        false
                    } else {
                        true // Continue the loop
                    }
                } else {
                    false
                }
            } else {
                false
            };

            // Forward the output
            if output_tx.send(output.clone()).is_err() {
                error!("Failed to forward Claude output");
                return Some(ConnectionResult::Disconnected(connection_start.elapsed()));
            }

            // Handle wiggum loop continuation
            if should_continue_wiggum {
                if let Some(ref mut state) = wiggum_state {
                    // Record the duration of the loop that just finished
                    let loop_duration = state.loop_start.elapsed();
                    state.loop_durations.push(loop_duration);
                    // Keep only the last 10
                    if state.loop_durations.len() > 10 {
                        state.loop_durations.remove(0);
                    }

                    state.iteration += 1;

                    // Check max iterations safety limit
                    if state.iteration > WIGGUM_MAX_ITERATIONS {
                        warn!(
                            "Wiggum reached max iterations ({}), stopping",
                            WIGGUM_MAX_ITERATIONS
                        );
                        *wiggum_state = None;
                    } else {
                        info!("Wiggum iteration {} - resending prompt", state.iteration);

                        // Send a portal message with loop status
                        let portal_text = format_wiggum_status(state);
                        let portal_content = shared::PortalMessage::text(portal_text).to_json();
                        let seq = {
                            let mut buf = output_buffer.lock().await;
                            buf.push(portal_content.clone())
                        };
                        let msg = ProxyToServer::SequencedOutput {
                            seq,
                            content: portal_content,
                        };
                        let mut ws = ws_write.lock().await;
                        if ws.send(msg).await.is_err() {
                            error!("Failed to send wiggum portal message");
                            return Some(ConnectionResult::Disconnected(
                                connection_start.elapsed(),
                            ));
                        }

                        // Reset loop_start for the new iteration
                        state.loop_start = Instant::now();

                        // Resend the prompt
                        let wiggum_prompt = format!(
                            "{}\n\nTake action on the directions above until fully complete. If complete, respond only with DONE.",
                            state.original_prompt
                        );
                        if let Err(e) = claude_session
                            .send_input(serde_json::Value::String(wiggum_prompt))
                            .await
                        {
                            error!("Failed to resend wiggum prompt: {}", e);
                            *wiggum_state = None;
                            return Some(ConnectionResult::ClaudeExited);
                        }
                    }
                }
            } else if matches!(output, ClaudeOutput::Result(_)) && wiggum_state.is_some() {
                // Send final completion portal message
                if let Some(ref mut state) = wiggum_state {
                    let loop_duration = state.loop_start.elapsed();
                    state.loop_durations.push(loop_duration);
                    if state.loop_durations.len() > 10 {
                        state.loop_durations.remove(0);
                    }

                    let total: Duration = state.loop_durations.iter().sum();
                    let portal_text = format!(
                        "**Wiggum complete** after **{}** iteration{} (total: {})",
                        state.iteration,
                        if state.iteration == 1 { "" } else { "s" },
                        format_duration(total.as_millis() as u64),
                    );
                    let portal_content = shared::PortalMessage::text(portal_text).to_json();
                    let seq = {
                        let mut buf = output_buffer.lock().await;
                        buf.push(portal_content.clone())
                    };
                    let msg = ProxyToServer::SequencedOutput {
                        seq,
                        content: portal_content,
                    };
                    let mut ws = ws_write.lock().await;
                    if ws.send(msg).await.is_err() {
                        error!("Failed to send wiggum completion portal message");
                    }
                }
                // Clear wiggum state when done
                *wiggum_state = None;
            }

            if matches!(output, ClaudeOutput::Result(_)) && wiggum_state.is_none() {
                debug!("--- ready for input ---");
            }
            None
        }
        Some(SessionEvent::PermissionRequest {
            request_id,
            tool_name,
            input,
            permission_suggestions,
        }) => {
            // Send permission request directly to WebSocket
            let msg = ProxyToServer::PermissionRequest {
                request_id,
                tool_name,
                input,
                permission_suggestions,
            };
            let mut ws = ws_write.lock().await;
            if let Err(e) = ws.send(msg).await {
                error!("Failed to send permission request to backend: {}", e);
                return Some(ConnectionResult::Disconnected(connection_start.elapsed()));
            }
            None
        }
        Some(SessionEvent::SessionNotFound) => {
            warn!("Session not found (from library event)");
            Some(ConnectionResult::SessionNotFound)
        }
        Some(SessionEvent::Exited { code }) => {
            info!("Claude session exited with code {}", code);
            Some(ConnectionResult::ClaudeExited)
        }
        Some(SessionEvent::RawOutput(_)) => {
            // Handled in run_main_loop before calling this function
            unreachable!(
                "RawOutput should be handled before calling handle_session_event_with_wiggum"
            );
        }
        Some(SessionEvent::Error(e)) => {
            let err_msg = e.to_string();
            error!("Session error: {}", err_msg);
            if err_msg.contains("Connection closed") || err_msg.contains("Claude stderr") {
                // Claude exited immediately — print a user-visible hint
                eprintln!();
                eprintln!("Claude CLI exited unexpectedly.");
                if let Some(stderr_start) = err_msg.find("Claude stderr: ") {
                    let stderr_text = &err_msg[stderr_start + 15..];
                    eprintln!("stderr: {}", stderr_text);
                } else {
                    eprintln!("No output from Claude. Is `claude` installed and on your PATH?");
                    eprintln!("Try running: claude --version");
                }
                eprintln!();
            }
            Some(ConnectionResult::ClaudeExited)
        }
        None => {
            // Session has ended
            info!("Claude session ended");
            Some(ConnectionResult::ClaudeExited)
        }
    }
}

/// Check if Claude's result indicates wiggum completion (responded with "DONE")
fn check_wiggum_done(result: &claude_codes::io::ResultMessage) -> bool {
    // Check if it was an error (don't continue on errors)
    if result.is_error {
        warn!("Wiggum stopping due to error");
        return true;
    }

    // The result message has a `result` field which contains Claude's final text response
    if let Some(ref result_text) = result.result {
        let text_upper: String = result_text.to_uppercase();
        // Check if the result is exactly "DONE" or contains it prominently
        // Being strict: must be "DONE" alone or "DONE" with minimal surrounding text
        let trimmed = text_upper.trim();
        if trimmed == "DONE" || trimmed.starts_with("DONE.") || trimmed.starts_with("DONE!") {
            info!("Wiggum complete: Claude responded with DONE");
            return true;
        }
        // Also check if DONE appears as the main content
        if trimmed.len() < 50 && trimmed.contains("DONE") {
            info!("Wiggum complete: Claude responded with short message containing DONE");
            return true;
        }
    }

    false // Continue the loop
}

/// Build the portal message text for a wiggum loop iteration
fn format_wiggum_status(state: &WiggumState) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "**Wiggum** loop **{}** / {}",
        state.iteration, WIGGUM_MAX_ITERATIONS,
    ));

    if !state.loop_durations.is_empty() {
        lines.push(String::new());
        lines.push("| Loop | Duration |".to_string());
        lines.push("|-----:|---------:|".to_string());

        let start_iter = state.iteration as usize - state.loop_durations.len();
        for (i, d) in state.loop_durations.iter().enumerate() {
            lines.push(format!(
                "| {} | {} |",
                start_iter + i,
                format_duration(d.as_millis() as u64)
            ));
        }

        let total: Duration = state.loop_durations.iter().sum();
        let avg = total / state.loop_durations.len() as u32;
        lines.push(format!(
            "\nAvg: **{}** | Total: **{}**",
            format_duration(avg.as_millis() as u64),
            format_duration(total.as_millis() as u64),
        ));
    }

    lines.join("\n")
}
