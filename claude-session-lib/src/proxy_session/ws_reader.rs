//! WebSocket reader task: receives messages from backend and dispatches to channels.

use shared::{ProxyToServer, SendMode, ServerToProxy};
use tokio::sync::mpsc;
use tracing::{debug, error, info, trace, warn};

use super::{truncate, GracefulShutdown, PermissionResponseData, SharedWsWrite, WsRead};

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

/// Result from handling a WebSocket text message
pub(super) enum WsMessageResult {
    /// Continue processing messages
    Continue,
    /// Disconnect (error or other reason)
    Disconnect,
    /// Server requested graceful shutdown with specified delay in ms
    GracefulShutdown(u64),
}

/// Spawn the WebSocket reader task
#[allow(clippy::too_many_arguments)] // TODO: refactor to event enum (issue #271)
pub(super) fn spawn_ws_reader(
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
        _ => {
            debug!("ws msg: {:?}", proxy_msg);
        }
    }

    WsMessageResult::Continue
}
