// Re-export library types used by main.rs and shim.rs
pub use claude_session_lib::proxy_session::*;

// ---------------------------------------------------------------------------
// Raw tokio-tungstenite helpers for shim mode.
//
// The shim manages its own WebSocket connection independently of the library's
// ws_bridge-based connection loop. These types and functions provide low-level
// WS primitives that the shim needs.
// ---------------------------------------------------------------------------

use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use shared::{ProxyToServer, SendMode, ServerToProxy};
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, warn};

/// Raw tokio-tungstenite WebSocket stream type.
pub type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Shared write half for concurrent WebSocket sends (raw).
pub type SharedWsWrite = Arc<tokio::sync::Mutex<SplitSink<WsStream, Message>>>;

/// WebSocket connection wrapper (raw tokio-tungstenite).
pub struct WebSocketConnection {
    write: SplitSink<WsStream, Message>,
    read: SplitStream<WsStream>,
}

impl WebSocketConnection {
    pub fn new(stream: WsStream) -> Self {
        let (write, read) = stream.split();
        Self { write, read }
    }

    /// Send a serializable message as JSON text.
    pub async fn send<T: serde::Serialize>(&mut self, msg: &T) -> Result<(), String> {
        let json = serde_json::to_string(msg).map_err(|e| e.to_string())?;
        self.write
            .send(Message::Text(json))
            .await
            .map_err(|e| e.to_string())
    }

    /// Receive the next raw WebSocket message.
    pub async fn recv(&mut self) -> Option<Result<Message, tokio_tungstenite::tungstenite::Error>> {
        self.read.next().await
    }

    /// Split into write and read halves for concurrent use.
    pub fn split(self) -> (SplitSink<WsStream, Message>, SplitStream<WsStream>) {
        (self.write, self.read)
    }
}

/// Connect to the backend WebSocket (raw, no TUI output).
pub async fn connect_ws(backend_url: &str) -> Result<WebSocketConnection, Duration> {
    let ws_url = format!("{}/ws/session", backend_url);

    match connect_async(&ws_url).await {
        Ok((stream, _)) => {
            info!("Connected to backend");
            Ok(WebSocketConnection::new(stream))
        }
        Err(e) => {
            error!("Failed to connect to backend: {}", e);
            Err(Duration::ZERO)
        }
    }
}

/// Register session with the backend and wait for acknowledgment (raw WS).
///
/// Returns `Ok(())` on success, `Err((Duration, Option<String>))` on failure.
pub async fn register_with_backend(
    conn: &mut WebSocketConnection,
    config: &ProxySessionConfig,
) -> Result<(), (Duration, Option<String>)> {
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
        replay_after: None,
        client_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        replaces_session_id: config.replaces_session_id,
        hostname: Some(hostname),
        launcher_id: config.launcher_id,
        agent_type: config.agent_type,
    });

    if let Err(e) = conn.send(&register_msg).await {
        error!("Failed to send registration message: {}", e);
        return Err((Duration::ZERO, Some(e)));
    }

    // Wait for RegisterAck with timeout
    let ack_timeout = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(msg) = conn.recv().await {
            match msg {
                Ok(Message::Text(text)) => {
                    if let Ok(ServerToProxy::RegisterAck {
                        success,
                        session_id: _,
                        error,
                    }) = serde_json::from_str::<ServerToProxy>(&text)
                    {
                        return Some((success, error));
                    }
                }
                Ok(Message::Close(_)) => return None,
                Err(_) => return None,
                _ => continue,
            }
        }
        None
    })
    .await;

    match ack_timeout {
        Ok(Some((true, _))) => {
            info!("Session registered successfully");
            Ok(())
        }
        Ok(Some((false, error))) => {
            let err_msg = error.clone().unwrap_or_else(|| "Unknown error".to_string());
            error!("Registration failed: {}", err_msg);
            Err((Duration::ZERO, error))
        }
        Ok(None) => {
            error!("Connection closed during registration");
            Err((Duration::ZERO, None))
        }
        Err(_) => {
            info!(
                "No RegisterAck received (timeout), assuming success for backwards compatibility"
            );
            Ok(())
        }
    }
}

/// Get the current git branch name, if in a git repository.
pub fn get_git_branch(cwd: &str) -> Option<String> {
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

/// Result from handling a raw WebSocket text message.
pub enum WsMessageResult {
    Continue,
    Disconnect,
    GracefulShutdown(u64),
}

/// Spawn a WebSocket reader task (raw tokio-tungstenite).
///
/// Reads raw WS text messages, parses them as `ServerToProxy`, and dispatches
/// to typed channels for the shim's select loop.
#[allow(clippy::too_many_arguments)]
pub fn spawn_ws_reader(
    mut ws_read: SplitStream<WsStream>,
    input_tx: mpsc::UnboundedSender<String>,
    perm_tx: mpsc::UnboundedSender<PermissionResponseData>,
    ack_tx: mpsc::UnboundedSender<u64>,
    ws_write: SharedWsWrite,
    disconnect_tx: tokio::sync::oneshot::Sender<()>,
    wiggum_tx: mpsc::UnboundedSender<String>,
    graceful_shutdown_tx: mpsc::UnboundedSender<GracefulShutdown>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(msg) = ws_read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    match handle_ws_text_message(
                        &text, &input_tx, &perm_tx, &ack_tx, &ws_write, &wiggum_tx,
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
                Ok(Message::Close(_)) => {
                    info!("WebSocket closed by server");
                    break;
                }
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    break;
                }
                _ => {}
            }
        }
        debug!("WebSocket reader ended");
        let _ = disconnect_tx.send(());
    })
}

/// Handle a raw text message from the WebSocket.
async fn handle_ws_text_message(
    text: &str,
    input_tx: &mpsc::UnboundedSender<String>,
    perm_tx: &mpsc::UnboundedSender<PermissionResponseData>,
    ack_tx: &mpsc::UnboundedSender<u64>,
    ws_write: &SharedWsWrite,
    wiggum_tx: &mpsc::UnboundedSender<String>,
) -> WsMessageResult {
    let server_msg = match serde_json::from_str::<ServerToProxy>(text) {
        Ok(msg) => msg,
        Err(_) => return WsMessageResult::Continue,
    };

    match server_msg {
        ServerToProxy::ClaudeInput { content, send_mode } => {
            let user_text = match &content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };

            if send_mode == Some(SendMode::Wiggum) {
                if wiggum_tx.send(user_text).is_err() {
                    error!("Failed to send wiggum activation");
                    return WsMessageResult::Disconnect;
                }
            } else if input_tx.send(user_text).is_err() {
                error!("Failed to send input to channel");
                return WsMessageResult::Disconnect;
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
                if wiggum_tx.send(user_text).is_err() {
                    error!("Failed to send wiggum activation");
                    return WsMessageResult::Disconnect;
                }
            } else if input_tx.send(user_text).is_err() {
                error!("Failed to send input to channel");
                return WsMessageResult::Disconnect;
            }

            // Send InputAck back to backend
            let ack = ProxyToServer::InputAck {
                session_id,
                ack_seq: seq,
            };
            let mut ws = ws_write.lock().await;
            if let Ok(json) = serde_json::to_string(&ack) {
                if let Err(e) = ws.send(Message::Text(json)).await {
                    error!("Failed to send InputAck: {}", e);
                }
            }
        }
        ServerToProxy::PermissionResponse(shared::PermissionResponseFields {
            request_id,
            allow,
            input,
            permissions,
            reason,
        }) => {
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
            if ack_tx.send(ack_seq).is_err() {
                error!("Failed to send output ack to channel");
                return WsMessageResult::Disconnect;
            }
        }
        ServerToProxy::Heartbeat => {
            let mut ws = ws_write.lock().await;
            if let Ok(json) = serde_json::to_string(&ProxyToServer::Heartbeat) {
                let _ = ws.send(Message::Text(json)).await;
            }
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
        _ => {}
    }

    WsMessageResult::Continue
}
