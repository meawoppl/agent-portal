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
    /// Bytes are written here and renamed to `filename` only on completion,
    /// so a consumer (the agent) can never read a truncated file (#939).
    pub(crate) temp_path: std::path::PathBuf,
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
    client_msg_id: Option<uuid::Uuid>,
) -> RoutedPortalInput {
    let (text, display_event) = match content {
        serde_json::Value::String(s) => (s, None),
        other => match serde_json::from_value::<shared::PortalMessage>(other.clone())
            .ok()
            .and_then(|msg| msg.agent_facing_text())
        {
            Some(text) => (text, Some(other)),
            None => (other.to_string(), None),
        },
    };
    let input = super::PortalInput {
        text,
        display_event,
        ack,
        client_msg_id,
    };
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
    client_msg_id: Option<uuid::Uuid>,
    input_tx: &mpsc::UnboundedSender<super::PortalInput>,
    wiggum_tx: &mpsc::UnboundedSender<super::PortalInput>,
) -> WsMessageResult {
    let label = if ack.is_some() { "seq_input" } else { "input" };
    let seq_part = ack
        .as_ref()
        .map(|a| format!(" seq={}", a.seq))
        .unwrap_or_default();
    match classify_portal_input(content, send_mode, ack, client_msg_id) {
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
    pub interrupt_tx: mpsc::UnboundedSender<()>,
}

/// Spawn the WebSocket reader task
pub(super) fn spawn_ws_reader(
    mut ws_read: WsRead,
    channels: WsReaderChannels,
    heartbeat: session_lib::heartbeat::HeartbeatTracker,
    tunnel: std::sync::Arc<session_lib::tunnel::TunnelManager>,
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
        interrupt_tx,
    } = channels;
    tokio::spawn(async move {
        while let Some(result) = ws_read.recv().await {
            match result {
                Ok(msg) => {
                    // Forward/tunnel frames are consumed by the tunnel
                    // manager; `handle` never blocks on I/O (dials run in
                    // spawned tasks), so the reader stays responsive.
                    if tunnel.handle(&msg).await {
                        continue;
                    }
                    match handle_ws_message(
                        msg,
                        &input_tx,
                        &perm_tx,
                        &ack_tx,
                        &wiggum_tx,
                        &heartbeat,
                        &file_upload_tx,
                        &file_download_tx,
                        &interrupt_tx,
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
    interrupt_tx: &mpsc::UnboundedSender<()>,
) -> WsMessageResult {
    if !matches!(
        proxy_msg,
        ServerToProxy::Heartbeat | ServerToProxy::FileUploadChunk(..)
    ) {
        debug!("ws recv: {:?}", proxy_msg);
    }

    match proxy_msg {
        ServerToProxy::AgentInput { content, send_mode } => {
            return route_portal_input(content, send_mode, None, None, input_tx, wiggum_tx);
        }
        ServerToProxy::SequencedInput {
            session_id,
            seq,
            content,
            send_mode,
            client_msg_id,
        } => {
            return route_portal_input(
                content,
                send_mode,
                Some(super::PortalInputAck { session_id, seq }),
                client_msg_id,
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
        ServerToProxy::Interrupt => {
            // Cancel the agent's in-flight turn. Routed to the main loop, which
            // calls `Session::interrupt()`; the per-agent I/O task writes the
            // protocol's cancel form. Previously unhandled — it fell through to
            // the catch-all below and was silently dropped, so interrupts never
            // reached the agent on the launcher path (only the VS Code shim
            // handled them). A closed channel means the loop is gone anyway.
            debug!("→ [interrupt]");
            let _ = interrupt_tx.send(());
        }
        _ => {
            debug!("ws msg: {:?}", proxy_msg);
        }
    }

    WsMessageResult::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_agent_message_portal_input_keeps_display_event() {
        let content = shared::PortalMessage::agent_message(
            "codex".to_string(),
            "12345678-0000-0000-0000-000000000000".to_string(),
            "hello".to_string(),
        )
        .to_json();

        let RoutedPortalInput::Input(input) =
            classify_portal_input(content.clone(), None, None, None)
        else {
            panic!("expected normal input");
        };

        assert!(input
            .text
            .starts_with("[message from codex 12345678-0000-0000-0000-000000000000]\nhello"));
        assert!(input
            .text
            .contains("agent-portal message send 12345678 \"your reply\""));
        assert_eq!(input.display_event, Some(content));
    }

    #[test]
    fn classify_plain_string_input_has_no_display_event() {
        let RoutedPortalInput::Input(input) = classify_portal_input(
            serde_json::Value::String("hello".to_string()),
            None,
            None,
            None,
        ) else {
            panic!("expected normal input");
        };

        assert_eq!(input.text, "hello");
        assert!(input.display_event.is_none());
    }

    #[test]
    fn classify_preserves_ack_and_client_msg_id_for_normal_input() {
        // The delivery-progress pipeline (#939) depends on client_msg_id and the
        // seq ack surviving classification — dropping either silently breaks the
        // optimistic-row advance / pending-input replay.
        let session_id = uuid::Uuid::nil();
        let client_msg_id = uuid::Uuid::from_u128(7);
        let ack = super::super::PortalInputAck {
            session_id,
            seq: 42,
        };

        let RoutedPortalInput::Input(input) = classify_portal_input(
            serde_json::Value::String("hi".to_string()),
            None,
            Some(ack),
            Some(client_msg_id),
        ) else {
            panic!("expected normal input");
        };

        assert_eq!(input.client_msg_id, Some(client_msg_id));
        let ack = input.ack.expect("ack preserved");
        assert_eq!(ack.seq, 42);
        assert_eq!(ack.session_id, session_id);
    }

    #[test]
    fn classify_wiggum_preserves_ack_and_client_msg_id() {
        let client_msg_id = uuid::Uuid::from_u128(9);
        let ack = super::super::PortalInputAck {
            session_id: uuid::Uuid::nil(),
            seq: 5,
        };

        let RoutedPortalInput::Wiggum(input) = classify_portal_input(
            serde_json::Value::String("activate".to_string()),
            Some(SendMode::Wiggum),
            Some(ack),
            Some(client_msg_id),
        ) else {
            panic!("expected wiggum activation");
        };

        assert_eq!(input.client_msg_id, Some(client_msg_id));
        assert_eq!(input.ack.expect("ack preserved").seq, 5);
    }
}
