//! Proxy session management and WebSocket connection handling.

mod git_metadata;
mod image_uploader;
mod output_forwarder;
mod portal_reminder;
mod wiggum;
mod ws_reader;

pub(crate) use portal_reminder::inject_portal_reminder;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use claude_codes::ClaudeOutput;
use session_lib::agent::Agent;
use session_lib::output_buffer::PendingOutputBuffer;
use session_lib::session::Session;
use session_lib::SessionEvent;
use shared::{ProxyToServer, ServerToProxy, SessionEndpoint};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use git_metadata::{
    check_and_send_branch_update, check_and_send_branch_update_if_branch_changed,
    codex_output_has_git_signal, get_git_branch, get_pr_url, get_repo_url, GitMetadataState,
    GitRefreshTrigger,
};
use output_forwarder::spawn_output_forwarder;
use wiggum::{handle_session_event_with_wiggum, WiggumState};
use ws_reader::{spawn_ws_reader, FileDownloadEvent, FileReceiveState, FileUploadEvent};

/// Type alias for the native WebSocket connection
type NativeConnection = ws_bridge::native_client::Connection<SessionEndpoint>;

/// Type alias for the shared WebSocket write half
type SharedWsWrite = Arc<tokio::sync::Mutex<ws_bridge::WsSender<ProxyToServer>>>;

/// Type alias for the WebSocket read half
type WsRead = ws_bridge::WsReceiver<ServerToProxy>;

/// Sink for codex thread-id persistence. The proxy crate owns the
/// `ProxyConfig` JSON file; this callback lets the session loop hand the
/// learned thread id back without claude-session-lib having to depend on
/// proxy internals. Called at most once per spawn, from the
/// `SessionEvent::CodexThreadId` arm. `None` is a no-op (the codex
/// io-task still emits the event, the loop just doesn't persist it).
pub type CodexThreadIdSink = Arc<dyn Fn(String) + Send + Sync>;

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
    /// If this session was started by a scheduled task
    pub scheduled_task_id: Option<Uuid>,
    /// Persist-back closure for the codex app-server thread id; see
    /// [`CodexThreadIdSink`] doc.
    pub codex_thread_id_sink: Option<CodexThreadIdSink>,
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
            max: shared::protocol::MAX_RECONNECT_BACKOFF_SECS,
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
    /// Session was terminated by the server (do not reconnect)
    SessionTerminated,
}

/// Result from the connection loop
pub enum LoopResult {
    /// Normal exit (Claude process ended)
    NormalExit,
    /// Session not found - caller should restart with fresh session
    SessionNotFound,
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

/// Portal-originated user input plus optional backend ack metadata.
pub struct PortalInput {
    pub text: String,
    pub ack: Option<PortalInputAck>,
}

pub struct PortalInputAck {
    pub session_id: Uuid,
    pub seq: i64,
}

/// Signal for graceful server shutdown with recommended reconnect delay
pub struct GracefulShutdown {
    pub reconnect_delay_ms: u64,
}

/// State that persists across WebSocket reconnections for a session.
/// This includes the input channel, output buffer, and session config.
pub struct SessionState<'a, A: Agent> {
    /// Session configuration
    pub config: &'a ProxySessionConfig,
    /// The agent-backed session (Claude or Codex).
    pub claude_session: &'a mut Session<A>,
    /// Sender for input messages (cloned per connection)
    pub input_tx: mpsc::UnboundedSender<PortalInput>,
    /// Receiver for input messages (persists across connections)
    pub input_rx: &'a mut mpsc::UnboundedReceiver<PortalInput>,
    /// Output buffer with persistence
    pub output_buffer: Arc<Mutex<PendingOutputBuffer>>,
    /// Backoff state for reconnection
    pub backoff: Backoff,
    /// Whether this is the first connection attempt
    pub first_connection: bool,
    /// When the last disconnect occurred (for reporting reconnect duration)
    pub disconnected_at: Option<Instant>,
    /// Wall-clock UTC paired with `disconnected_at` for the reconnect text
    pub disconnected_at_utc: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether the last disconnect was a graceful server shutdown
    pub last_disconnect_graceful: bool,
    /// Consecutive replay failures on the same pending messages
    pub replay_failures: u32,
}

impl<'a, A: Agent> SessionState<'a, A> {
    /// Create a new session state
    pub fn new(
        config: &'a ProxySessionConfig,
        claude_session: &'a mut Session<A>,
        input_tx: mpsc::UnboundedSender<PortalInput>,
        input_rx: &'a mut mpsc::UnboundedReceiver<PortalInput>,
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
            disconnected_at_utc: None,
            last_disconnect_graceful: false,
            replay_failures: 0,
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

/// State for the main message loop, reducing parameter count.
/// Contains channels and state that are specific to a single connection attempt.
/// Note: input_rx is passed separately as it persists across reconnections.
struct ConnectionState {
    /// Session id for metadata updates.
    session_id: Uuid,
    /// Receiver for permission responses from frontend
    perm_rx: mpsc::UnboundedReceiver<PermissionResponseData>,
    /// Receiver for output acknowledgments from backend
    ack_rx: mpsc::UnboundedReceiver<u64>,
    /// Sender for Claude outputs to the output forwarder
    output_tx: mpsc::UnboundedSender<ClaudeOutput>,
    /// WebSocket write handle for sending permission requests directly
    ws_write: SharedWsWrite,
    /// Receiver to detect WebSocket disconnection
    disconnect_rx: tokio::sync::oneshot::Receiver<()>,
    /// Receiver for graceful server shutdown signal
    graceful_shutdown_rx: mpsc::UnboundedReceiver<GracefulShutdown>,
    /// Receiver for session terminated signal (do not reconnect)
    session_terminated_rx: tokio::sync::oneshot::Receiver<()>,
    /// When the connection was established
    connection_start: Instant,
    /// Buffer for pending outputs
    output_buffer: Arc<Mutex<PendingOutputBuffer>>,
    /// Receiver for wiggum mode activation
    wiggum_rx: mpsc::UnboundedReceiver<PortalInput>,
    /// Current wiggum state (if active)
    wiggum_state: Option<WiggumState>,
    /// Heartbeat tracker for dead connection detection
    heartbeat: session_lib::heartbeat::HeartbeatTracker,
    /// Receiver for file upload events from backend
    file_upload_rx: mpsc::UnboundedReceiver<FileUploadEvent>,
    /// Receiver for file download requests from backend
    file_download_rx: mpsc::UnboundedReceiver<FileDownloadEvent>,
    /// Working directory for file uploads
    working_directory: String,
    /// Active file uploads being received in chunks
    active_uploads: std::collections::HashMap<String, FileReceiveState>,
    /// Agent type for tagging per-message wire output (proxy emission side)
    agent_type: shared::AgentType,
    /// Shared git metadata state for branch / PR / repo refreshes.
    git_metadata: GitMetadataState,
    /// Refresh cadence for Codex raw-output messages.
    git_refresh: GitRefreshTrigger,
    /// Persist-back closure for the codex app-server thread id; see
    /// [`CodexThreadIdSink`] doc. Cloned out of `ProxySessionConfig` so
    /// the per-connection state doesn't need to carry a config reference.
    codex_thread_id_sink: Option<CodexThreadIdSink>,
}

/// Run the WebSocket connection loop with auto-reconnect
pub async fn run_connection_loop<A: Agent>(
    config: &ProxySessionConfig,
    claude_session: &mut Session<A>,
    input_tx: mpsc::UnboundedSender<PortalInput>,
    input_rx: &mut mpsc::UnboundedReceiver<PortalInput>,
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
                if session.disconnected_at.is_none() {
                    session.disconnected_at = Some(Instant::now());
                    session.disconnected_at_utc = Some(chrono::Utc::now());
                }
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
                if session.disconnected_at.is_none() {
                    session.disconnected_at = Some(Instant::now());
                    session.disconnected_at_utc = Some(chrono::Utc::now());
                }
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
            ConnectionResult::SessionTerminated => {
                info!("Session terminated by server, not reconnecting");
                session.persist_buffer().await;
                return Ok(LoopResult::NormalExit);
            }
        }
    }
}

/// Run a single WebSocket connection until it disconnects or Claude exits
async fn run_single_connection<A: Agent>(session: &mut SessionState<'_, A>) -> ConnectionResult {
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
    let max_image_mb = match register_session(&mut conn, &config_with_branch).await {
        Ok(mb) => mb,
        Err(duration) => return ConnectionResult::Disconnected(duration),
    };

    // Look up PR URL and repo URL for the current branch and send as SessionUpdate
    let repo_url = get_repo_url(&session.config.working_directory);
    let pr_url = config_with_branch
        .git_branch
        .as_deref()
        .and_then(|b| get_pr_url(&session.config.working_directory, b));
    let update_msg = ProxyToServer::SessionUpdate {
        session_id: config_with_branch.session_id,
        git_branch: config_with_branch.git_branch.clone(),
        pr_url,
        repo_url,
    };
    if conn.send(update_msg).await.is_err() {
        error!("Failed to send initial session update");
    }

    // Replay pending messages after successful registration.
    // If replay fails repeatedly (e.g., backend reset and rejects stale seqs),
    // drop the pending messages after MAX_REPLAY_FAILURES to avoid an infinite loop.
    const MAX_REPLAY_FAILURES: u32 = 5;
    {
        let mut buf = session.output_buffer.lock().await;
        let pending_count = buf.pending_count();
        if pending_count > 0 {
            if session.replay_failures >= MAX_REPLAY_FAILURES {
                error!(
                    "Dropping {} pending messages after {} consecutive replay failures",
                    pending_count, session.replay_failures
                );
                buf.clear();
                session.replay_failures = 0;
            } else {
                debug!(
                    "Replaying {} pending messages after reconnect (attempt {})",
                    pending_count,
                    session.replay_failures + 1
                );
                let mut replay_failed = false;
                for pending in buf.get_pending() {
                    let msg = ProxyToServer::SequencedOutput {
                        seq: pending.seq,
                        content: pending.content.clone(),
                        agent_type: config_with_branch.agent_type,
                    };
                    if conn.send(msg).await.is_err() {
                        error!("Failed to replay pending message seq={}", pending.seq);
                        session.replay_failures += 1;
                        replay_failed = true;
                        break;
                    }
                }
                if replay_failed {
                    return ConnectionResult::Disconnected(Duration::ZERO);
                }
                debug!("Finished replaying pending messages");
                session.replay_failures = 0;
            }
        }
    }

    // Send a portal message with session details
    {
        let hostname = hostname::get()
            .ok()
            .and_then(|h| h.into_string().ok())
            .unwrap_or_else(|| "unknown".to_string());

        let status_line = if session.first_connection {
            "**Session started**".to_string()
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
            let now_utc = chrono::Utc::now();
            let header = if duration_str.is_empty() {
                format!("**Proxy reconnected** ({})", reason)
            } else {
                format!("**Proxy reconnected** after {} ({})", duration_str, reason)
            };
            match session.disconnected_at_utc {
                Some(disc_utc) => format!(
                    "{}\n  disconnected at {} (UTC)\n  reconnected  at {} (UTC)",
                    header,
                    disc_utc.format("%Y-%m-%dT%H:%M:%SZ"),
                    now_utc.format("%Y-%m-%dT%H:%M:%SZ"),
                ),
                None => format!(
                    "{}\n  reconnected at {} (UTC)",
                    header,
                    now_utc.format("%Y-%m-%dT%H:%M:%SZ"),
                ),
            }
        };

        let short_id = &session.config.session_id.to_string()[..8];
        let text = format!(
            "{} — `{}` on `{}` in `{}` ({} `{}…`)",
            status_line,
            session.config.session_name,
            hostname,
            session.config.working_directory,
            config_with_branch.agent_type,
            short_id,
        );

        let portal_content = shared::PortalMessage::text(text).to_json();
        let seq = {
            let mut buf = session.output_buffer.lock().await;
            buf.push(portal_content.clone())
        };
        let msg = ProxyToServer::SequencedOutput {
            seq,
            content: portal_content,
            agent_type: config_with_branch.agent_type,
        };
        if conn.send(msg).await.is_err() {
            error!("Failed to send connection portal message");
            return ConnectionResult::Disconnected(Duration::ZERO);
        }
    }

    if !session.first_connection {
        info!("Connection restored");
        session.disconnected_at = None;
        session.disconnected_at_utc = None;
    }

    // Run the message loop - split connection for concurrent read/write
    run_message_loop(session, &config_with_branch, conn, max_image_mb).await
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

/// Register session with the backend and wait for acknowledgment.
/// On success, returns the backend-provided max_image_mb.
async fn register_session(
    conn: &mut NativeConnection,
    config: &ProxySessionConfig,
) -> Result<u32, Duration> {
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
        repo_url: get_repo_url(&config.working_directory),
        scheduled_task_id: config.scheduled_task_id,
        claude_args: config.claude_args.clone(),
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
                    max_image_mb,
                }) => {
                    return Some((success, error, max_image_mb));
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        None
    })
    .await;

    match ack_timeout {
        Ok(Some((true, _, max_image_mb))) => {
            info!("Session registered (max_image_mb: {})", max_image_mb);
            Ok(max_image_mb)
        }
        Ok(Some((false, error, _))) => {
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
            Ok(10)
        }
    }
}

/// Run the main message forwarding loop
async fn run_message_loop<A: Agent>(
    session: &mut SessionState<'_, A>,
    config: &ProxySessionConfig,
    conn: NativeConnection,
    max_image_mb: u32,
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
    let (wiggum_tx, wiggum_rx) = mpsc::unbounded_channel::<PortalInput>();

    // Channel for graceful server shutdown signals
    let (graceful_shutdown_tx, graceful_shutdown_rx) =
        mpsc::unbounded_channel::<GracefulShutdown>();

    // Channel for file upload events from backend
    let (file_upload_tx, file_upload_rx) = mpsc::unbounded_channel::<FileUploadEvent>();

    // Channel for file download requests from backend
    let (file_download_tx, file_download_rx) = mpsc::unbounded_channel::<FileDownloadEvent>();

    // Channel for session terminated signal (do not reconnect)
    let (session_terminated_tx, session_terminated_rx) = tokio::sync::oneshot::channel::<()>();

    // Wrap ws_write for sharing
    let ws_write = std::sync::Arc::new(tokio::sync::Mutex::new(ws_write));

    // Heartbeat tracker for dead connection detection
    let heartbeat = session_lib::heartbeat::HeartbeatTracker::new();

    // Channel to signal WebSocket disconnection
    let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel::<()>();

    // Shared state for tracking git branch, PR URL, and repo URL updates
    let git_metadata = GitMetadataState::new(config.git_branch.clone());

    // Spawn output forwarder task with buffer
    let output_task = spawn_output_forwarder(
        output_rx,
        ws_write.clone(),
        session_id,
        config.backend_url.clone(),
        config.auth_token.clone(),
        config.working_directory.clone(),
        git_metadata.clone(),
        session.output_buffer.clone(),
        max_image_mb,
        config.agent_type,
    );

    // Spawn WebSocket reader task
    let reader_task = spawn_ws_reader(
        ws_read,
        session.input_tx.clone(),
        perm_tx,
        ack_tx,
        disconnect_tx,
        wiggum_tx,
        graceful_shutdown_tx,
        session_terminated_tx,
        heartbeat.clone(),
        file_upload_tx,
        file_download_tx,
    );

    // Create connection state (per-connection channels and timing)
    let mut conn_state = ConnectionState {
        session_id,
        perm_rx,
        ack_rx,
        output_tx,
        ws_write: ws_write.clone(),
        disconnect_rx,
        graceful_shutdown_rx,
        session_terminated_rx,
        connection_start,
        output_buffer: session.output_buffer.clone(),
        wiggum_rx,
        wiggum_state: None,
        heartbeat,
        file_upload_rx,
        file_download_rx,
        working_directory: config.working_directory.clone(),
        active_uploads: std::collections::HashMap::new(),
        agent_type: config.agent_type,
        git_metadata,
        git_refresh: GitRefreshTrigger::default(),
        codex_thread_id_sink: config.codex_thread_id_sink.clone(),
    };

    // On the very first connection of this session, inject the portal
    // features reminder. It primes the agent with a `<system-reminder>` on
    // its stdin and emits a collapsed portal message for the user. On
    // reconnects we skip — the agent's context is unchanged so the agent
    // already has it; subsequent re-injections happen at compaction
    // boundaries from inside `output_forwarder`.
    if session.first_connection {
        inject_portal_reminder(session.claude_session).await;
    }

    // Main loop
    let result = run_main_loop(session.claude_session, session.input_rx, &mut conn_state).await;

    // Clean up
    output_task.abort();
    reader_task.abort();

    result
}

/// Receive from a oneshot, returning `Some(T)` on success or `None` if the sender was dropped.
/// This prevents `tokio::select!` from treating a dropped sender as a valid signal.
async fn recv_option(rx: &mut tokio::sync::oneshot::Receiver<()>) -> Option<()> {
    rx.await.ok()
}

async fn ack_portal_input(ws_write: &SharedWsWrite, ack: Option<PortalInputAck>) {
    let Some(ack) = ack else {
        return;
    };
    let msg = ProxyToServer::InputAck {
        session_id: ack.session_id,
        ack_seq: ack.seq,
    };
    let mut ws = ws_write.lock().await;
    if let Err(e) = ws.send(msg).await {
        error!("Failed to send InputAck: {}", e);
    }
}

/// Run the main select loop
///
/// The Claude session internally uses a dedicated drain task to continuously
/// read stdout, so there's no risk of buffer starvation in this select! loop.
/// See: https://github.com/meawoppl/agent-portal/issues/278
async fn run_main_loop<A: Agent>(
    claude_session: &mut Session<A>,
    input_rx: &mut mpsc::UnboundedReceiver<PortalInput>,
    state: &mut ConnectionState,
) -> ConnectionResult {
    use crate::Permission;
    use session_lib::io::PermissionResponse as LibPermissionResponse;

    let mut heartbeat_interval = tokio::time::interval(session_lib::heartbeat::HEARTBEAT_INTERVAL);

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

            Some(()) = recv_option(&mut state.session_terminated_rx) => {
                info!("Session terminated by server");
                return ConnectionResult::SessionTerminated;
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

            Some(input) = input_rx.recv() => {
                check_and_send_branch_update_if_branch_changed(
                    &state.ws_write,
                    state.session_id,
                    &state.working_directory,
                    &state.git_metadata,
                )
                .await;

                debug!("sending to claude process: {}", truncate(&input.text, 100));

                if let Err(e) = claude_session
                    .send_input(serde_json::Value::String(input.text))
                    .await
                {
                    error!("Failed to send to Claude: {}", e);
                    return ConnectionResult::ClaudeExited;
                }
                ack_portal_input(&state.ws_write, input.ack).await;
            }

            // Wiggum mode activation — set state and send prompt atomically
            Some(wiggum_input) = state.wiggum_rx.recv() => {
                info!(
                    "Wiggum mode activated with prompt: {}",
                    truncate(&wiggum_input.text, 60)
                );
                check_and_send_branch_update_if_branch_changed(
                    &state.ws_write,
                    state.session_id,
                    &state.working_directory,
                    &state.git_metadata,
                )
                .await;
                let wiggum_prompt = format!(
                    "{}\n\nTake action on the directions above until fully complete. If complete, respond only with DONE.",
                    wiggum_input.text
                );
                state.wiggum_state = Some(WiggumState {
                    original_prompt: wiggum_input.text,
                    iteration: 1,
                    loop_start: Instant::now(),
                    loop_durations: Vec::new(),
                });
                if let Err(e) = claude_session.send_input(serde_json::Value::String(wiggum_prompt)).await {
                    error!("Failed to send wiggum prompt to Claude: {}", e);
                    return ConnectionResult::ClaudeExited;
                }
                ack_portal_input(&state.ws_write, wiggum_input.ack).await;
            }

            Some(upload_event) = state.file_upload_rx.recv() => {
                handle_file_upload(upload_event, state).await;
            }

            Some(download_event) = state.file_download_rx.recv() => {
                handle_file_download(download_event, state).await;
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
                    if state.git_refresh.should_check_before_message() {
                        check_and_send_branch_update(
                            &state.ws_write,
                            state.session_id,
                            &state.working_directory,
                            &state.git_metadata,
                        )
                        .await;
                    }
                    if codex_output_has_git_signal(value) {
                        state.git_refresh.mark_git_signal();
                    }

                    let is_codex_compaction = is_codex_compaction_event(value);
                    let seq = {
                        let mut buf = state.output_buffer.lock().await;
                        buf.push(value.clone())
                    };
                    let msg = ProxyToServer::SequencedOutput {
                        seq,
                        content: value.clone(),
                        agent_type: state.agent_type,
                    };
                    let mut ws = state.ws_write.lock().await;
                    if ws.send(msg).await.is_err() {
                        error!("Failed to send raw output");
                        return ConnectionResult::Disconnected(state.connection_start.elapsed());
                    }
                    if is_codex_compaction {
                        inject_portal_reminder(claude_session).await;
                    }
                    continue;
                }

                // Per-turn metrics: forward as a typed `TurnMetricsReport`
                // envelope. Doesn't go through the output buffer because
                // it's not part of the message-replay path — a missed
                // metrics row is acceptable (next turn replaces it on the
                // dashboard) but we still want low-latency delivery.
                if let Some(SessionEvent::TurnMetricsReady(metrics)) = event {
                    let msg = ProxyToServer::TurnMetricsReport(metrics);
                    let mut ws = state.ws_write.lock().await;
                    if ws.send(msg).await.is_err() {
                        error!("Failed to send turn metrics report");
                        return ConnectionResult::Disconnected(state.connection_start.elapsed());
                    }
                    continue;
                }

                // Codex app-server thread id: hand it to the persistence
                // sink (the proxy's ProxyConfig writer) so the next resume
                // of this session can call `thread/resume` with it.
                // Emitted once per spawn by codex-session-lib. No-op for
                // claude sessions (claude doesn't emit it).
                if let Some(SessionEvent::CodexThreadId(thread_id)) = event {
                    if let Some(sink) = state.codex_thread_id_sink.as_ref() {
                        sink(thread_id);
                    }
                    continue;
                }

                if let Some(SessionEvent::SessionLimitReached {
                    session_id,
                    reset_at,
                    source_message,
                    prompt,
                }) = event
                {
                    let msg =
                        ProxyToServer::SessionLimitReached(shared::SessionLimitContinuationFields {
                            session_id,
                            reset_at,
                            source_message,
                            prompt,
                        });
                    let mut ws = state.ws_write.lock().await;
                    if ws.send(msg).await.is_err() {
                        error!("Failed to send session-limit continuation candidate");
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
                    state.agent_type,
                ).await {
                    Some(result) => return result,
                    None => continue,
                }
            }
        }
    }
}

fn is_codex_compaction_event(value: &serde_json::Value) -> bool {
    value
        .get("type")
        .and_then(|t| t.as_str())
        .is_some_and(|t| t == "thread/compacted")
}

/// Handle a file upload event (start or chunk)
async fn handle_file_upload(upload_event: FileUploadEvent, state: &mut ConnectionState) {
    match upload_event {
        FileUploadEvent::Start {
            upload_id,
            filename,
            total_chunks,
            total_size,
        } => {
            // Sanitize filename
            let safe_name: String = filename
                .rsplit('/')
                .next()
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
                    state.active_uploads.insert(
                        upload_id,
                        FileReceiveState {
                            filename: safe_name,
                            total_chunks,
                            total_size,
                            received_chunks: 0,
                            received_bytes: 0,
                            file_handle: Some(fh),
                            start_time: Instant::now(),
                            last_log_percent: 0,
                        },
                    );
                }
                Err(e) => {
                    error!("Failed to create file {}: {}", file_path.display(), e);
                }
            }
        }
        FileUploadEvent::Chunk { upload_id, data } => {
            use base64::Engine;
            use tokio::io::AsyncWriteExt;

            let upload_id_short = &upload_id[..8.min(upload_id.len())];
            let Some(recv_state) = state.active_uploads.get_mut(&upload_id) else {
                warn!("[upload {}] Chunk for unknown upload", upload_id_short);
                return;
            };

            let decoded = match base64::engine::general_purpose::STANDARD.decode(&data) {
                Ok(b) => b,
                Err(e) => {
                    error!("[upload {}] Base64 decode error: {}", upload_id_short, e);
                    return;
                }
            };

            if let Some(ref mut fh) = recv_state.file_handle {
                if let Err(e) = fh.write_all(&decoded).await {
                    error!("[upload {}] Write error: {}", upload_id_short, e);
                    return;
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

async fn handle_file_download(download_event: FileDownloadEvent, state: &mut ConnectionState) {
    let FileDownloadEvent::Request(request) = download_event;
    let response = read_download_file(&state.working_directory, &request).await;
    let mut ws = state.ws_write.lock().await;
    if let Err(e) = ws.send(ProxyToServer::FileDownloadResponse(response)).await {
        error!("Failed to send file download response: {}", e);
    }
}

async fn read_download_file(
    working_directory: &str,
    request: &shared::FileDownloadRequestFields,
) -> shared::FileDownloadResponseFields {
    use base64::Engine;
    use std::path::{Component, Path};

    let fail = |error: &str| shared::FileDownloadResponseFields {
        request_id: request.request_id,
        success: false,
        filename: None,
        media_type: None,
        size: None,
        data_base64: None,
        error: Some(error.to_string()),
    };

    let requested = Path::new(&request.path);
    if requested.is_absolute()
        || requested.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return fail("invalid relative path");
    }

    let root = match tokio::fs::canonicalize(working_directory).await {
        Ok(path) => path,
        Err(_) => return fail("working directory is unavailable"),
    };
    let target = root.join(requested);
    let canonical = match tokio::fs::canonicalize(&target).await {
        Ok(path) => path,
        Err(_) => return fail("file not found"),
    };
    if !canonical.starts_with(&root) {
        return fail("path escapes working directory");
    }

    let metadata = match tokio::fs::metadata(&canonical).await {
        Ok(metadata) => metadata,
        Err(_) => return fail("file not found"),
    };
    if !metadata.is_file() {
        return fail("path is not a file");
    }
    if metadata.len() > request.max_bytes {
        return fail("file exceeds size limit");
    }

    let bytes = match tokio::fs::read(&canonical).await {
        Ok(bytes) => bytes,
        Err(_) => return fail("failed to read file"),
    };
    if bytes.len() as u64 > request.max_bytes {
        return fail("file exceeds size limit");
    }

    let filename = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download")
        .to_string();

    shared::FileDownloadResponseFields {
        request_id: request.request_id,
        success: true,
        filename: Some(filename),
        media_type: Some("application/octet-stream".to_string()),
        size: Some(bytes.len() as u64),
        data_base64: Some(base64::engine::general_purpose::STANDARD.encode(bytes)),
        error: None,
    }
}

/// Truncate a string to max length
pub(crate) fn truncate(s: &str, max_len: usize) -> &str {
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
pub(crate) fn format_duration(ms: u64) -> String {
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
