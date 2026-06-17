//! WebSocket reader task: receives messages from backend and dispatches to channels.

use shared::{SendMode, ServerToProxy};
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

use super::{truncate, GracefulShutdown, PermissionResponseData, WsRead};

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
    Chunk { upload_id: String, data: String },
}

pub enum FileDownloadEvent {
    Request(shared::FileDownloadRequestFields),
}

/// Tracks state for a file being received in chunks
pub(crate) struct FileReceiveState {
    pub(crate) filename: String,
    pub(crate) total_chunks: u32,
    pub(crate) total_size: u64,
    pub(crate) received_chunks: u32,
    pub(crate) received_bytes: u64,
    pub(crate) file_handle: Option<tokio::fs::File>,
    pub(crate) start_time: std::time::Instant,
    pub(crate) last_log_percent: u32,
}

/// A portal input classified by send mode: a plain user input or a
/// wiggum-mode activation carrying the original prompt.
pub enum RoutedPortalInput {
    Input(super::PortalInput),
    Wiggum(super::PortalInput),
}

/// Convert a `ClaudeInput`/`SequencedInput` payload into a routed portal
/// input, applying the shared wiggum-vs-input dispatch.
pub fn classify_portal_input(
    content: serde_json::Value,
    send_mode: Option<SendMode>,
    ack: Option<super::PortalInputAck>,
) -> RoutedPortalInput {
    let text = match content {
        serde_json::Value::String(s) => s,
        other => other.to_string(),
    };
    let input = super::PortalInput { text, ack };
    if send_mode == Some(SendMode::Wiggum) {
        RoutedPortalInput::Wiggum(input)
    } else {
        RoutedPortalInput::Input(input)
    }
}

/// Route a portal input payload to the wiggum or input channel.
fn route_portal_input(
    content: serde_json::Value,
    send_mode: Option<SendMode>,
    ack: Option<super::PortalInputAck>,
    input_tx: &mpsc::UnboundedSender<super::PortalInput>,
    wiggum_tx: &mpsc::UnboundedSender<super::PortalInput>,
) -> WsMessageResult {
    let label = if ack.is_some() { "seq_input" } else { "input" };
    let seq_part = ack
        .as_ref()
        .map(|a| format!(" seq={}", a.seq))
        .unwrap_or_default();
    match classify_portal_input(content, send_mode, ack) {
        RoutedPortalInput::Wiggum(input) => {
            debug!(
                "→ [{}/wiggum]{} {}",
                label,
                seq_part,
                truncate(&input.text, 80)
            );
            // Only send to wiggum_tx — the select loop sets state and sends prompt atomically
            if wiggum_tx.send(input).is_err() {
                error!("Failed to send wiggum activation");
                return WsMessageResult::Disconnect;
            }
        }
        RoutedPortalInput::Input(input) => {
            debug!("→ [{}]{} {}", label, seq_part, truncate(&input.text, 80));
            if input_tx.send(input).is_err() {
                error!("Failed to send input to channel");
                return WsMessageResult::Disconnect;
            }
        }
    }
    WsMessageResult::Continue
}

/// Result from handling a WebSocket text message
pub(super) enum WsMessageResult {
    /// Continue processing messages
    Continue,
    /// Disconnect (error or other reason)
    Disconnect,
    /// Server requested graceful shutdown with specified delay in ms
    GracefulShutdown(u64),
    /// Session was terminated by the server (do not reconnect)
    SessionTerminated,
}

/// Outbound channels the WebSocket reader dispatches received messages into.
///
/// Bundles every `mpsc`/`oneshot` sender used by [`spawn_ws_reader`] so the
/// spawn signature stays small. Field names mirror the call-site locals.
pub(super) struct WsReaderChannels {
    pub input_tx: mpsc::UnboundedSender<super::PortalInput>,
    pub perm_tx: mpsc::UnboundedSender<PermissionResponseData>,
    pub ack_tx: mpsc::UnboundedSender<u64>,
    pub disconnect_tx: tokio::sync::oneshot::Sender<()>,
    pub wiggum_tx: mpsc::UnboundedSender<super::PortalInput>,
    pub graceful_shutdown_tx: mpsc::UnboundedSender<GracefulShutdown>,
    pub session_terminated_tx: tokio::sync::oneshot::Sender<()>,
    pub file_upload_tx: mpsc::UnboundedSender<FileUploadEvent>,
    pub file_download_tx: mpsc::UnboundedSender<FileDownloadEvent>,
}

/// Spawn the WebSocket reader task
pub(super) fn spawn_ws_reader(
    mut ws_read: WsRead,
    channels: WsReaderChannels,
    heartbeat: session_lib::heartbeat::HeartbeatTracker,
) -> tokio::task::JoinHandle<()> {
    let WsReaderChannels {
        input_tx,
        perm_tx,
        ack_tx,
        disconnect_tx,
        wiggum_tx,
        graceful_shutdown_tx,
        session_terminated_tx,
        file_upload_tx,
        file_download_tx,
    } = channels;
    tokio::spawn(async move {
        while let Some(result) = ws_read.recv().await {
            match result {
                Ok(msg) => {
                    match handle_ws_message(
                        msg,
                        &input_tx,
                        &perm_tx,
                        &ack_tx,
                        &wiggum_tx,
                        &heartbeat,
                        &file_upload_tx,
                        &file_download_tx,
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
                        WsMessageResult::SessionTerminated => {
                            let _ = session_terminated_tx.send(());
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
    input_tx: &mpsc::UnboundedSender<super::PortalInput>,
    perm_tx: &mpsc::UnboundedSender<PermissionResponseData>,
    ack_tx: &mpsc::UnboundedSender<u64>,
    wiggum_tx: &mpsc::UnboundedSender<super::PortalInput>,
    heartbeat: &session_lib::heartbeat::HeartbeatTracker,
    file_upload_tx: &mpsc::UnboundedSender<FileUploadEvent>,
    file_download_tx: &mpsc::UnboundedSender<FileDownloadEvent>,
) -> WsMessageResult {
    if !matches!(
        proxy_msg,
        ServerToProxy::Heartbeat | ServerToProxy::FileUploadChunk(..)
    ) {
        debug!("ws recv: {:?}", proxy_msg);
    }

    match proxy_msg {
        ServerToProxy::ClaudeInput { content, send_mode } => {
            return route_portal_input(content, send_mode, None, input_tx, wiggum_tx);
        }
        ServerToProxy::SequencedInput {
            session_id,
            seq,
            content,
            send_mode,
        } => {
            return route_portal_input(
                content,
                send_mode,
                Some(super::PortalInputAck { session_id, seq }),
                input_tx,
                wiggum_tx,
            );
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
        ServerToProxy::SessionTerminated { reason } => {
            info!("Session terminated by server: {}", reason);
            return WsMessageResult::SessionTerminated;
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
            chunk_index: _,
            data,
        }) => {
            if file_upload_tx
                .send(FileUploadEvent::Chunk { upload_id, data })
                .is_err()
            {
                error!("Failed to send file upload chunk to main loop");
                return WsMessageResult::Disconnect;
            }
        }
        ServerToProxy::FileDownloadRequest(fields) => {
            if file_download_tx
                .send(FileDownloadEvent::Request(fields))
                .is_err()
            {
                error!("Failed to send file download request to main loop");
                return WsMessageResult::Disconnect;
            }
        }
        _ => {
            debug!("ws msg: {:?}", proxy_msg);
        }
    }

    WsMessageResult::Continue
}
