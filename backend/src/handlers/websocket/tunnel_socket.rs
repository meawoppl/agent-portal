//! Backend accept path for the dedicated port-forward data plane (#1506).
//!
//! This socket carries **only** binary tunnel frames. Keeping it separate from
//! `/ws/session` is the whole point: forward bytes used to share the control
//! socket with agent stdio and heartbeats, so a busy preview could delay
//! heartbeats past the liveness deadline, get the connection evicted, and take
//! the agent session down with it.
//!
//! Consequences of that separation, both deliberate:
//!
//! - **This socket is never authoritative for session liveness.** Inbound
//!   frames stamp a diagnostic timestamp only; the control socket's heartbeat
//!   remains the sole liveness signal. A saturated data plane must not be able
//!   to *prove* a session alive any more than it can kill it.
//! - **Losing it is not a session failure.** On teardown the registry entry is
//!   dropped and `open_tunnel` silently reverts to the control-socket JSON
//!   path. Nothing about the agent session changes.
//!
//! Auth is the `Hello` ticket (see [`tunnel_ticket`](super::tunnel_ticket)),
//! which also binds the socket to one control-connection generation.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::WebSocket;
use shared::{TunnelDataEndpoint, TunnelFrame};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::session_manager::DATA_PLANE_CHANNEL_CAPACITY;
use super::{conn_channel, tunnel_ticket};
use crate::AppState;

/// How long a freshly connected data socket may take to send its `Hello`.
/// Short: the proxy sends it as the very first frame, immediately after
/// connecting. An unauthenticated socket that lingers is dropped rather than
/// held open.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn handle_tunnel_data_socket(socket: WebSocket, app_state: Arc<AppState>) {
    let conn = ws_bridge::server::into_connection::<TunnelDataEndpoint>(socket);
    let (mut ws_sender, mut ws_receiver) = conn.split();

    // First frame must be the ticket. Nothing is registered — and no frames are
    // routed — until it verifies.
    let hello = match tokio::time::timeout(HELLO_TIMEOUT, ws_receiver.recv()).await {
        Ok(Some(Ok(TunnelFrame::Hello { ticket }))) => ticket,
        Ok(Some(Ok(other))) => {
            warn!(
                "Data-plane socket sent {:?} before Hello; closing",
                std::mem::discriminant(&other)
            );
            return;
        }
        Ok(Some(Err(e))) => {
            debug!("Data-plane socket failed to decode its first frame: {}", e);
            return;
        }
        Ok(None) => {
            debug!("Data-plane socket closed before Hello");
            return;
        }
        Err(_) => {
            debug!("Data-plane socket did not send Hello within the timeout");
            return;
        }
    };

    let Some(ticket) = tunnel_ticket::verify(&app_state.jwt_secret, &hello) else {
        warn!("Rejecting data-plane socket: invalid or expired ticket");
        return;
    };

    let session_manager = &app_state.session_manager;
    let (tx, mut rx) = conn_channel::<TunnelFrame>(DATA_PLANE_CHANNEL_CAPACITY);
    let cancel = CancellationToken::new();

    // Generation-guarded: refuses a ticket replayed from a connection that has
    // since been superseded, and a socket that finished connecting only after
    // its control connection was replaced.
    if !session_manager.register_data_plane(
        ticket.session_key.clone(),
        ticket.gen,
        tx,
        cancel.clone(),
    ) {
        warn!(
            "Rejecting data-plane socket for session {} (gen={}): control connection is gone or superseded",
            ticket.session_key, ticket.gen
        );
        return;
    }

    // Outbound: drain the registry channel onto the socket.
    let send_cancel = cancel.clone();
    let send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = send_cancel.cancelled() => break,
                frame = rx.recv() => match frame {
                    Some(frame) => {
                        if ws_sender.send(frame).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                },
            }
        }
        let _ = ws_sender.close().await;
    });

    info!(
        "Data plane connected for session {} (gen={})",
        ticket.session_key, ticket.gen
    );

    // Inbound: route tunnel frames into their streams' relays.
    loop {
        let frame = tokio::select! {
            _ = cancel.cancelled() => break,
            frame = ws_receiver.recv() => frame,
        };
        match frame {
            Some(Ok(frame)) => {
                // Diagnostics only — see the module note on liveness.
                session_manager.touch_data_plane(&ticket.session_key);
                if !route_inbound(&app_state, frame) {
                    break;
                }
            }
            Some(Err(e)) => {
                debug!(
                    "Data-plane decode error for session {}: {}",
                    ticket.session_key, e
                );
                break;
            }
            None => break,
        }
    }

    // Teardown is generation-guarded so a stale socket cannot evict its
    // successor. Dropping the entry reverts `open_tunnel` to the control-socket
    // path; the session itself is unaffected.
    session_manager.unregister_data_plane(&ticket.session_key, ticket.gen);
    cancel.cancel();
    let _ = send_task.await;
    info!(
        "Data plane disconnected for session {} (gen={})",
        ticket.session_key, ticket.gen
    );
}

/// Feed one inbound frame to its stream. Returns `false` to close the socket.
fn route_inbound(app_state: &AppState, frame: TunnelFrame) -> bool {
    use super::TunnelIn;
    let manager = &app_state.session_manager;
    match frame {
        TunnelFrame::Opened { stream_id } => manager.tunnel_in(stream_id, TunnelIn::Opened),
        TunnelFrame::Refused { stream_id, reason } => {
            manager.tunnel_in(stream_id, TunnelIn::Refused(reason))
        }
        TunnelFrame::Data { stream_id, bytes } => manager.tunnel_bytes_in(stream_id, bytes),
        TunnelFrame::Window {
            stream_id,
            add_bytes,
        } => manager.tunnel_in(stream_id, TunnelIn::Window(add_bytes)),
        TunnelFrame::Close { stream_id, .. } => manager.tunnel_in(stream_id, TunnelIn::Close),
        // Server→proxy only; a proxy sending these is confused.
        TunnelFrame::Open { .. } => {
            warn!("Data plane received a server-only Open frame; closing");
            return false;
        }
        // A second Hello is protocol misuse: the socket is already bound.
        TunnelFrame::Hello { .. } => {
            warn!("Data plane sent a duplicate Hello; closing");
            return false;
        }
    }
    true
}
