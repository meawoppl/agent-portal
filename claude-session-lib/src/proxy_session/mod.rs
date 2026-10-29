//! Proxy session management and WebSocket connection handling.

mod git_metadata;
mod media_display;
mod output_forwarder;
mod session_event;
mod supervisor;
use session_lib::proxy_session::file_transfer::FileTransferBridge;
pub use supervisor::{run_connection_loop, Backoff, LoopResult, SessionState};
mod wiggum;

// Hoisted to session-lib (#1657): these serve every agent under the proxy.
// Re-exported here so internal call sites keep their `super::` paths.
use session_lib::proxy_session::heartbeat_watchdog;
pub(crate) use session_lib::proxy_session::portal_reminder;
pub use session_lib::proxy_session::{ack_portal_input, emit_input_failure, emit_input_progress};

pub use session_lib::proxy_session::ws_reader::{classify_portal_input, RoutedPortalInput};
pub use wiggum::wiggum_prompt;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use claude_codes::ClaudeOutput;
use session_lib::agent::Agent;
use session_lib::output_buffer::PendingOutputBuffer;
use session_lib::session::Session;
use shared::{ProxyToServer, ServerToProxy, SessionEndpoint};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, info, warn};
use uuid::Uuid;

pub use git_metadata::{get_git_branch, get_repo_url};
pub use media_display::MediaDisplaySink;

use git_metadata::{GitMetadataState, GitRefreshTrigger};
use output_forwarder::spawn_output_forwarder;
use session_lib::proxy_session::ws_reader::{
    spawn_ws_reader, FileDownloadEvent, FileUploadEvent, WsReaderChannels,
};
use wiggum::WiggumState;

pub use session_lib::proxy_session::{
    ConnectionResult, GracefulShutdown, NativeConnection, PermissionResponseData, PortalInput,
    PortalInputAck, SharedWsWrite, WsRead,
};

/// Sink for codex thread-id persistence. The proxy crate owns the
/// `ProxyConfig` JSON file; this callback lets the session loop hand the
/// learned thread id back without claude-session-lib having to depend on
/// proxy internals. Called at most once per spawn, from the
/// `SessionEvent::CodexThreadId` arm. `None` is a no-op (the codex
/// io-task still emits the event, the loop just doesn't persist it).
pub type CodexThreadIdSink = Arc<dyn Fn(String) + Send + Sync>;
/// Learns claude's *current* conversation id, which `/clear` rolls away from
/// the portal's session id. The launcher persists it so the next `--resume`
/// re-opens the live transcript instead of the pre-clear one.
pub type ClaudeConversationIdSink = Arc<dyn Fn(uuid::Uuid) + Send + Sync>;

/// Configuration for a proxy session
#[derive(Clone)]
pub struct ProxySessionConfig {
    /// One-time transcript disclosure when missing native history was replaced.
    pub startup_notice: Option<String>,
    /// Context changes the launcher posts while this session runs.
    pub context_updates: session_lib::ContextInbox,
    pub history_restore: Option<shared::api::HistoryRestoreLaunch>,
    pub backend_url: String,
    pub session_id: Uuid,
    pub session_name: String,
    pub auth_token: Option<String>,
    pub working_directory: String,
    pub resume: bool,
    /// Whether the next non-slash user input should carry the Portal feature
    /// reminder. Fresh contexts need it; real resumes already have it in their
    /// transcript, so re-sending it just burns context on every process restart.
    pub prime_portal_reminder_on_first_input: bool,
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
    /// Claude counterpart to `codex_thread_id_sink` (see the type docs).
    pub claude_conversation_id_sink: Option<ClaudeConversationIdSink>,
    /// Displays an image the agent read; see [`MediaDisplaySink`].
    pub media_display_sink: Option<MediaDisplaySink>,
    /// First-spawn-only source session for Claude/Codex fork semantics.
    pub fork_from_session_id: Option<Uuid>,
    /// Optional Codex native turn id; `None` forks the latest turn.
    pub fork_point_turn_id: Option<String>,
}

/// Decide whether a launch needs the Portal reminder folded onto its first
/// user input.
///
/// Most agents can answer this from `resume`: a resumed context should already
/// contain the reminder, while a fresh launch has never seen it. Codex is the
/// one wrinkle: a `resume` launch without a persisted app-server thread id
/// starts a fresh thread, so it still needs the reminder. Imported contexts
/// are primed separately by the launcher.
pub fn should_prime_portal_reminder_on_first_input(
    agent_type: shared::AgentType,
    resume: bool,
    codex_thread_id: Option<&str>,
) -> bool {
    if !resume {
        return true;
    }

    agent_type == shared::AgentType::Codex && codex_thread_id.is_none()
}

/// The local hostname, or `"unknown"` when the OS lookup fails.
/// Non-UTF-8 hostnames are converted lossily (invalid bytes become U+FFFD).
pub fn hostname_or_unknown() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Default session name when the user doesn't supply one:
/// `<hostname>-<YYYYmmdd-HHMMSS>` (local time).
pub fn default_session_name() -> String {
    format!(
        "{}-{}",
        hostname_or_unknown(),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    )
}

/// Why a [`register_session`] attempt did not yield a usable connection.
pub enum RegisterError {
    /// The backend explicitly rejected registration (`RegisterAck` with
    /// `success: false`) — a revoked/expired token or an unauthorized
    /// session. Retrying with the same credentials will never succeed, so the
    /// caller must not reconnect.
    Rejected,
    /// A transient failure (connection dropped, send failed). The caller
    /// should reconnect after backing off for the given duration.
    Transient(Duration),
}

/// State for the main message loop, reducing parameter count.
/// Contains channels and state that are specific to a single connection attempt.
/// Note: input_rx is passed separately as it persists across reconnections.
struct ConnectionState {
    /// Session id for metadata updates.
    session_id: Uuid,
    /// Receiver for permission responses from frontend
    perm_rx: mpsc::UnboundedReceiver<PermissionResponseData>,
    interrupt_rx: mpsc::UnboundedReceiver<()>,
    /// Receiver for output acknowledgments from backend
    ack_rx: mpsc::UnboundedReceiver<u64>,
    /// Sender for Claude output (raw wire JSON) to the output forwarder, which
    /// re-parses each frame to `ClaudeOutput` for its typed side-effects.
    output_tx: mpsc::UnboundedSender<serde_json::Value>,
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
    file_transfer: FileTransferBridge,
    /// Shared with `SessionState` — see the field there.
    reminder_pending: Arc<AtomicBool>,
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
    /// Displays images the agent reads; see [`MediaDisplaySink`]. Used by the
    /// non-Claude arm — Claude's detection runs in the output forwarder.
    media_display_sink: Option<MediaDisplaySink>,
}

/// Connect to the backend WebSocket
pub async fn connect_to_backend(
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
///
/// On success returns the backend's data-plane grant when it issued one (#1506):
/// the `(ticket, sizing)` for dialing the binary port-forward data plane, where
/// `sizing` is the negotiated [`shared::TunnelSizing`] (#1511). `None` means no
/// data plane for this connection — an older backend, or one that declined — in
/// which case tunnel bytes keep riding the control socket.
pub async fn register_session(
    conn: &mut NativeConnection,
    config: &ProxySessionConfig,
) -> Result<Option<(String, shared::TunnelSizing)>, RegisterError> {
    info!("Registering session...");

    let hostname = hostname_or_unknown();

    let register_msg = ProxyToServer::Register(shared::RegisterFields {
        protocol_version: Some(shared::protocol::PROTOCOL_VERSION),
        session_id: config.session_id,
        session_name: config.session_name.clone(),
        auth_token: config.auth_token.clone(),
        working_directory: config.working_directory.clone(),
        resuming: config.resume,
        git_branch: config.git_branch.clone(),
        replay_after: None, // Proxy doesn't need history replay
        client_version: Some(shared::VERSION.to_string()),
        replaces_session_id: config.replaces_session_id,
        hostname: Some(hostname),
        launcher_id: config.launcher_id,
        agent_type: config.agent_type,
        repo_url: get_repo_url(&config.working_directory),
        scheduled_task_id: config.scheduled_task_id,
        claude_args: config.claude_args.clone(),
        // Opt in to the dedicated binary port-forward data plane (#1506). The
        // backend mints a `tunnel_data_ticket` only for proxies that advertise
        // this; if we fail to dial the data socket, tunneling silently stays on
        // this control socket.
        capabilities: vec![shared::PROXY_CAPABILITY_TUNNEL_BINARY_V1.to_string()],
    });

    if conn.send(register_msg).await.is_err() {
        error!("Failed to send registration message");
        return Err(RegisterError::Transient(Duration::ZERO));
    }

    // Wait for RegisterAck with timeout
    let ack_timeout = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(result) = conn.recv().await {
            match result {
                Ok(ServerToProxy::RegisterAck {
                    protocol_version,
                    success,
                    session_id: _,
                    error,
                    // `max_image_mb` is still sent by the backend but no longer
                    // consumed here — the Read-triggered image path it capped
                    // was replaced by `agent-portal show` (see output_forwarder).
                    max_image_mb: _,
                    retryable,
                    tunnel_data_ticket,
                    tunnel_sizing,
                }) => {
                    if let Err(error) =
                        shared::protocol::check_protocol_version(protocol_version, "server")
                    {
                        return Some((false, Some(error), false, None, None));
                    }
                    return Some((success, error, retryable, tunnel_data_ticket, tunnel_sizing));
                }
                Ok(_) => continue,
                Err(_) => return None,
            }
        }
        None
    })
    .await;

    match ack_timeout {
        Ok(Some((true, _, _, tunnel_data_ticket, tunnel_sizing))) => {
            // Pair the ticket with its negotiated sizing. An older backend sends
            // no sizing, so default to V1 — the profile the data plane shipped
            // with (#1511).
            let grant =
                tunnel_data_ticket.map(|ticket| (ticket, tunnel_sizing.unwrap_or_default()));
            if let Some((_, sizing)) = &grant {
                info!(
                    "Session registered (data plane offered, {} KiB frames / {} KiB window)",
                    sizing.max_chunk / 1024,
                    sizing.initial_window / 1024,
                );
            } else {
                info!("Session registered (no data plane)");
            }
            Ok(grant)
        }
        Ok(Some((false, error, retryable, _, _))) => {
            let err_msg = error.as_deref().unwrap_or("Unknown error");
            if retryable {
                // Transient infrastructure failure (DB blip mid-deploy) —
                // the backend explicitly asked us to retry. Keep the agent
                // process alive and reconnect with backoff instead of
                // exiting for relaunch. #1264.
                warn!("Registration deferred by server (retryable): {}", err_msg);
                Err(RegisterError::Transient(Duration::ZERO))
            } else {
                error!("Registration rejected by server: {}", err_msg);
                Err(RegisterError::Rejected)
            }
        }
        Ok(None) => {
            // The socket closed before any RegisterAck arrived — a transient
            // connection problem, not an explicit rejection. Reconnect.
            error!("Connection closed during registration");
            Err(RegisterError::Transient(Duration::ZERO))
        }
        Err(_) => {
            // No ack within the window. Every deployed backend acks
            // registration, so silence means the connection is wedged (or
            // the backend is mid-restart) — not an old server. Reconnect
            // with backoff rather than assuming success and running with a
            // half-open socket. #1264.
            warn!("No RegisterAck received within timeout; reconnecting");
            Err(RegisterError::Transient(Duration::ZERO))
        }
    }
}

/// Run the main message forwarding loop
async fn run_message_loop<A: Agent>(
    session: &mut SessionState<'_, A>,
    config: &ProxySessionConfig,
    conn: NativeConnection,
    tunnel_data_grant: Option<(String, shared::TunnelSizing)>,
) -> ConnectionResult {
    let connection_start = Instant::now();
    let session_id = config.session_id;

    // Split connection for concurrent read/write
    let (ws_write, ws_read) = conn.split();

    // Channel for Claude outputs
    let (output_tx, output_rx) = mpsc::unbounded_channel::<serde_json::Value>();

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

    // Interrupt signal: backend `ServerToProxy::Interrupt` → main loop, which
    // calls `Session::interrupt()` to cancel the agent's in-flight turn.
    let (interrupt_tx, interrupt_rx) = mpsc::unbounded_channel::<()>();

    // Wrap ws_write for sharing
    let ws_write = std::sync::Arc::new(tokio::sync::Mutex::new(ws_write));

    // Per-connection port-forward tunnel state (docs/PORT_FORWARDING.md).
    // The backend replays `ForwardOpen`s after registration, so a fresh
    // manager per connection starts with the correct allowlist.
    let tunnel = session_lib::tunnel::TunnelManager::new(ws_write.clone());

    // Bring up the binary data plane, if the backend issued a ticket (#1506).
    // Spawned so forward bytes never gate session startup, but tied to THIS
    // connection's lifetime: the handle is aborted in cleanup below. Before
    // #1859 it was fire-and-forget, and the task only returns when its own
    // socket ends — so every reconnect-with-grant parked another task on a
    // healthy idle socket, leaking one fd per reconnect until the process hit
    // RLIMIT_NOFILE and every connect failed with a bogus "nodename nor
    // servname provided" resolver error.
    let data_plane_task = tunnel_data_grant.map(|(ticket, sizing)| {
        tokio::spawn(session_lib::tunnel::run_data_plane(
            tunnel.clone(),
            config.backend_url.clone(),
            ticket,
            sizing,
        ))
    });

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
        config.working_directory.clone(),
        git_metadata.clone(),
        session.output_buffer.clone(),
        config.agent_type,
        config.claude_conversation_id_sink.clone(),
        config.media_display_sink.clone(),
    );

    // Spawn WebSocket reader task
    let reader_task = spawn_ws_reader(
        ws_read,
        WsReaderChannels {
            input_tx: session.input_tx.clone(),
            perm_tx,
            ack_tx,
            disconnect_tx,
            wiggum_tx,
            graceful_shutdown_tx,
            session_terminated_tx,
            file_upload_tx,
            file_download_tx,
            interrupt_tx,
        },
        heartbeat.clone(),
        tunnel.clone(),
    );

    // Create connection state (per-connection channels and timing)
    let mut conn_state = ConnectionState {
        session_id,
        perm_rx,
        interrupt_rx,
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
        file_transfer: FileTransferBridge::new(config.working_directory.clone(), ws_write.clone()),
        reminder_pending: session.reminder_pending.clone(),
        agent_type: config.agent_type,
        git_metadata,
        git_refresh: GitRefreshTrigger::default(),
        codex_thread_id_sink: config.codex_thread_id_sink.clone(),
        media_display_sink: config.media_display_sink.clone(),
    };

    // Do not send the session-start reminder as a standalone input here. The
    // first user turn claims `reminder_pending` and folds the reminder into
    // that real input; sending it before the user types gives the agent an
    // invisible prompt to answer.

    // Main loop
    let result = run_main_loop(session.claude_session, session.input_rx, &mut conn_state).await;

    // Remove uncommitted uploads before discarding connection state.
    conn_state.file_transfer.shutdown().await;

    // Clean up
    output_task.abort();
    reader_task.abort();
    // The old data plane must die with its connection — the next register
    // issues a fresh ticket and dials a fresh socket (#1859).
    if let Some(task) = data_plane_task {
        task.abort();
    }
    tunnel.shutdown().await;

    result
}

/// Receive from a oneshot, returning `Some(T)` on success or `None` if the sender was dropped.
/// This prevents `tokio::select!` from treating a dropped sender as a valid signal.
async fn recv_option(rx: &mut tokio::sync::oneshot::Receiver<()>) -> Option<()> {
    rx.await.ok()
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
    let mut heartbeat_interval = tokio::time::interval(session_lib::heartbeat::HEARTBEAT_INTERVAL);

    loop {
        tokio::select! {
            _ = heartbeat_interval.tick() => {
                if let Some(result) = heartbeat_watchdog::tick(
                    &state.heartbeat,
                    &state.ws_write,
                    state.connection_start,
                )
                .await
                {
                    return result;
                }
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
                if let Some(result) = session_lib::proxy_session::input_delivery::handle_input(
                    &state.ws_write,
                    state.session_id,
                    &state.reminder_pending,
                    claude_session,
                    input,
                    || git_metadata::check_and_send_branch_update_if_branch_changed(
                        &state.ws_write,
                        state.session_id,
                        &state.working_directory,
                        &state.git_metadata,
                    ),
                    |text| crate::io_task::claude_user_echo_value(
                        text.to_string(),
                        state.session_id,
                    ),
                )
                .await
                {
                    return result;
                }
            }

            // Wiggum mode activation — set state and send prompt atomically
            Some(wiggum_input) = state.wiggum_rx.recv() => {
                if let Some(result) = wiggum::handle_wiggum_activation(
                    &state.ws_write,
                    state.session_id,
                    &state.working_directory,
                    &state.git_metadata,
                    &mut state.wiggum_state,
                    &state.reminder_pending,
                    &state.output_buffer,
                    state.agent_type,
                    claude_session,
                    wiggum_input,
                )
                .await
                {
                    return result;
                }
            }

            Some(upload_event) = state.file_upload_rx.recv() => {
                state.file_transfer.handle_upload(upload_event).await;
            }

            Some(download_event) = state.file_download_rx.recv() => {
                state.file_transfer.handle_download(download_event).await;
            }

            Some(perm_response) = state.perm_rx.recv() => {
                session_lib::proxy_session::permission_bridge::handle_permission_response(
                    claude_session,
                    perm_response,
                )
                .await;
            }

            Some(()) = state.interrupt_rx.recv() => {
                // Forward a backend interrupt to the agent's I/O task, which
                // cancels the in-flight turn (Claude: wrapped `control_request`;
                // Codex: `turn/interrupt`). Harmless when nothing is running.
                if let Err(e) = claude_session.interrupt().await {
                    warn!("Failed to forward interrupt to agent: {}", e);
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
                if let Some(result) =
                    session_event::handle_next_event(state, claude_session, event).await
                {
                    return result;
                }
            }
        }
    }
}

/// Re-parse a neutral `Visible` output value back to the typed `ClaudeOutput`
/// at the Claude proxy edge. `Session` is now agent-neutral (it forwards raw
/// `Value`s); the Claude-specific consumers — the `output_forwarder`'s
/// image/git handling and `wiggum`'s DONE detection — re-parse here.
/// Returns `None` for malformed/non-Claude frames, which callers forward
/// verbatim rather than drop (#1165 item 2, slice 1).
pub(super) fn parse_visible_claude_output(value: &serde_json::Value) -> Option<ClaudeOutput> {
    serde_json::from_value(value.clone()).ok()
}

fn is_codex_compaction_event(value: &serde_json::Value) -> bool {
    value
        .get("type")
        .and_then(|t| t.as_str())
        .is_some_and(|t| t == "thread/compacted")
}

// String helpers shared with the frontend (see `shared::fmt`). Note the
// minute format is `"{}m {}s"` (with a space), matching the frontend
// transcript — the old local copy here used `"{}m{}s"`.
pub(crate) use shared::fmt::{format_duration, truncate_str as truncate};

#[cfg(test)]
mod tests {
    #[test]
    fn portal_reminder_primes_fresh_contexts_only() {
        assert!(should_prime_portal_reminder_on_first_input(
            shared::AgentType::Claude,
            false,
            None,
        ));
        assert!(should_prime_portal_reminder_on_first_input(
            shared::AgentType::Codex,
            false,
            None,
        ));
        assert!(should_prime_portal_reminder_on_first_input(
            shared::AgentType::Muse,
            false,
            None,
        ));

        assert!(!should_prime_portal_reminder_on_first_input(
            shared::AgentType::Claude,
            true,
            None,
        ));
        assert!(!should_prime_portal_reminder_on_first_input(
            shared::AgentType::Muse,
            true,
            None,
        ));
    }

    #[test]
    fn codex_resume_without_a_thread_is_fresh_context_for_reminder() {
        assert!(should_prime_portal_reminder_on_first_input(
            shared::AgentType::Codex,
            true,
            None,
        ));
        assert!(!should_prime_portal_reminder_on_first_input(
            shared::AgentType::Codex,
            true,
            Some("thread-123"),
        ));
    }

    use super::*;

    /// The Claude proxy-edge re-parse gate (#1165 item 2, slice 1). Valid Claude
    /// frames parse to `Some`, so the typed side-effects (wiggum DONE,
    /// image/git handling) run; malformed / non-Claude frames yield
    /// `None`, so the caller forwards the original raw value verbatim — never
    /// dropping or rewriting it, and never panicking.
    #[test]
    fn parse_visible_claude_output_gates_typed_side_effects() {
        use serde_json::json;

        // Valid Claude Result → Some (wiggum DONE detection can fire).
        let result = json!({
            "type": "result", "subtype": "success", "is_error": false,
            "duration_ms": 1, "duration_api_ms": 1, "num_turns": 1,
            "result": "DONE", "session_id": "s", "total_cost_usd": 0.0
        });
        assert!(matches!(
            parse_visible_claude_output(&result),
            Some(ClaudeOutput::Result(_))
        ));

        // Malformed / non-Claude frames → None (forward raw, no panic).
        let garbage = json!({"totally": "unknown", "shape": [1, 2, 3]});
        assert!(parse_visible_claude_output(&garbage).is_none());
        assert!(parse_visible_claude_output(&json!(42)).is_none());
    }
}

#[cfg(test)]
mod registration_compatibility_tests;
