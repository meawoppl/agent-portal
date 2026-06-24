// Re-export library types used by main.rs and shim.rs
pub use claude_session_lib::proxy_session::*;

// ---------------------------------------------------------------------------
// Shim-mode WebSocket reader.
//
// The shim manages its own connection loop independently of the library's
// `run_connection_loop`, but shares the same ws_bridge typed connection
// (`connect_to_backend` / `register_session`). This reader dispatches
// `ServerToProxy` messages onto a single event channel for the shim's
// select loop.
// ---------------------------------------------------------------------------

use shared::{ProxyToServer, ServerToProxy};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Events produced by the WebSocket reader for the shim's select loop.
pub enum WsEvent {
    /// Text input from the portal web UI
    Input(PortalInput),
    /// Wiggum mode activation with the original prompt
    WiggumActivation(PortalInput),
    /// Permission response from the portal
    PermissionResponse(PermissionResponseData),
    /// Output acknowledgment from the backend
    OutputAck(u64),
    /// WebSocket disconnected (connection closed or error)
    Disconnect,
    /// Server requested graceful shutdown with reconnect delay
    GracefulShutdown(u64),
    /// Session was terminated by the server (do not reconnect)
    SessionTerminated,
    /// Interrupt the current Claude response
    Interrupt,
    /// Backend requested a local file download.
    FileDownloadRequest(shared::FileDownloadRequestFields),
}

/// Spawn a WebSocket reader task for shim mode.
///
/// Reads typed `ServerToProxy` messages and dispatches events through a
/// single channel for the shim's select loop. The `ws_write` handle is used
/// internally for Heartbeat replies.
pub fn spawn_ws_reader(
    mut ws_read: WsRead,
    ws_write: SharedWsWrite,
    event_tx: mpsc::UnboundedSender<WsEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(result) = ws_read.recv().await {
            match result {
                Ok(msg) => {
                    if !handle_server_message(msg, &event_tx, &ws_write).await {
                        break;
                    }
                }
                Err(ws_bridge::RecvError::Decode(e)) => {
                    // Unknown or malformed message — skip it, matching the
                    // tolerance of the previous raw-text reader.
                    warn!("Ignoring undecodable WebSocket message: {}", e);
                }
                Err(e) => {
                    error!("WebSocket error: {}", e);
                    break;
                }
            }
        }
        debug!("WebSocket reader ended");
        let _ = event_tx.send(WsEvent::Disconnect);
    })
}

/// Handle a typed message from the backend.
/// Returns `true` to continue reading, `false` to stop.
async fn handle_server_message(
    server_msg: ServerToProxy,
    event_tx: &mpsc::UnboundedSender<WsEvent>,
    ws_write: &SharedWsWrite,
) -> bool {
    match server_msg {
        ServerToProxy::AgentInput { content, send_mode } => {
            event_tx.send(input_event(content, send_mode, None)).is_ok()
        }
        ServerToProxy::SequencedInput {
            session_id,
            seq,
            content,
            send_mode,
        } => {
            let ack = Some(PortalInputAck { session_id, seq });
            event_tx.send(input_event(content, send_mode, ack)).is_ok()
        }
        ServerToProxy::PermissionResponse(shared::PermissionResponseFields {
            request_id,
            allow,
            input,
            permissions,
            reason,
        }) => event_tx
            .send(WsEvent::PermissionResponse(PermissionResponseData {
                request_id,
                allow,
                input,
                permissions,
                reason,
            }))
            .is_ok(),
        ServerToProxy::OutputAck {
            session_id: _,
            ack_seq,
        } => event_tx.send(WsEvent::OutputAck(ack_seq)).is_ok(),
        ServerToProxy::Heartbeat => {
            let mut ws = ws_write.lock().await;
            let _ = ws.send(ProxyToServer::Heartbeat).await;
            true
        }
        ServerToProxy::ServerShutdown {
            reason,
            reconnect_delay_ms,
        } => {
            warn!(
                "Server shutting down: {} (reconnecting in {}ms)",
                reason, reconnect_delay_ms
            );
            let _ = event_tx.send(WsEvent::GracefulShutdown(reconnect_delay_ms));
            false // stop reading
        }
        ServerToProxy::SessionTerminated { reason } => {
            info!("Session terminated by server: {}", reason);
            let _ = event_tx.send(WsEvent::SessionTerminated);
            false // stop reading
        }
        ServerToProxy::Interrupt => {
            info!("Interrupt received from server");
            event_tx.send(WsEvent::Interrupt).is_ok()
        }
        ServerToProxy::FileDownloadRequest(fields) => {
            event_tx.send(WsEvent::FileDownloadRequest(fields)).is_ok()
        }
        _ => true,
    }
}

/// Map a portal input payload to the matching shim event.
fn input_event(
    content: serde_json::Value,
    send_mode: Option<shared::SendMode>,
    ack: Option<PortalInputAck>,
) -> WsEvent {
    match classify_portal_input(content, send_mode, ack) {
        RoutedPortalInput::Wiggum(input) => WsEvent::WiggumActivation(input),
        RoutedPortalInput::Input(input) => WsEvent::Input(input),
    }
}
