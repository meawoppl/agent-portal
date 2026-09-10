mod continuations;
pub mod launcher_socket;
mod message_handlers;
mod permissions;
mod proxy_socket;
mod registration;
mod replay;
mod session_manager;
mod tunnel_socket;
mod tunnel_ticket;
mod turn_metrics;
mod uploads;
mod web_client_socket;

pub use session_manager::{
    conn_channel, ConnSender, DataPlaneConnection, DataPlaneSender, ForwardHealth,
    LauncherConnection, ProxySender, SessionId, SessionManager, TunnelError, TunnelIn,
    WebClientSender, DATA_PLANE_CHANNEL_CAPACITY, LAUNCHER_CHANNEL_CAPACITY,
    LAUNCHER_LIVENESS_DEADLINE_SECS, LIVENESS_SWEEP_INTERVAL_SECS, PROXY_CHANNEL_CAPACITY,
    PROXY_LIVENESS_DEADLINE_SECS, WEB_CLIENT_CHANNEL_CAPACITY,
};

/// Mint a port-forward data-plane ticket (#1506). Re-exported so the proxy
/// socket's `Register` handler can hand one to a capable proxy without
/// depending on the ticket module's internals.
pub(crate) fn mint_tunnel_ticket(
    jwt_secret: &str,
    session_id: uuid::Uuid,
    session_key: &str,
    gen: u64,
    sizing: shared::TunnelSizing,
) -> Option<String> {
    tunnel_ticket::mint(
        jwt_secret,
        session_id,
        session_key,
        gen,
        sizing,
        chrono::Utc::now().timestamp(),
    )
}

use axum::{
    extract::{ws::WebSocketUpgrade, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use tower_cookies::Cookies;
use tracing::{info, warn};

use crate::AppState;

/// Message/frame size ceiling for the proxy session socket.
///
/// `FileDownloadResponse` carries a whole file as ONE base64 text message
/// (`claude-session-lib read_download_file`), and tokio-tungstenite does not
/// fragment on send — so the receiver's frame limit is the effective file-size
/// ceiling. The defaults (16 MiB frame / 64 MiB message) silently closed the
/// socket for downloads over ~12 MiB on disk while `files::MAX_PULL_BYTES`
/// advertised 25 MiB; from the user's side the session appeared to crash.
/// 48 MiB covers the 25 MiB cap after base64 (~33.4 MiB) plus JSON envelope
/// with room to spare. A unit test in `handlers::files` pins the invariant so
/// the two constants cannot drift apart again.
pub const SESSION_WS_MAX_MESSAGE_BYTES: usize = 48 * 1024 * 1024;

pub async fn handle_session_websocket(
    ws: WebSocketUpgrade,
    State(app_state): State<Arc<AppState>>,
) -> Response {
    ws.max_frame_size(SESSION_WS_MAX_MESSAGE_BYTES)
        .max_message_size(SESSION_WS_MAX_MESSAGE_BYTES)
        .on_upgrade(|socket| proxy_socket::handle_session_socket(socket, app_state))
}

/// Upgrade for the dedicated binary port-forward data plane (#1506).
///
/// Deliberately unauthenticated at the HTTP layer, exactly like `/ws/session`:
/// the credential is the `Hello` ticket carried in the first frame, which also
/// binds the socket to one control-connection generation. Nothing is registered
/// and no frames are routed until that ticket verifies.
pub async fn handle_tunnel_data_websocket(
    ws: WebSocketUpgrade,
    State(app_state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(|socket| tunnel_socket::handle_tunnel_data_socket(socket, app_state))
}

pub async fn handle_launcher_websocket(
    ws: WebSocketUpgrade,
    State(app_state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(|socket| launcher_socket::handle_launcher_socket(socket, app_state))
}

pub async fn handle_web_client_websocket(
    ws: WebSocketUpgrade,
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Response {
    let user_id = match crate::auth::extract_user_id(&app_state, Some(&headers), &cookies).ok() {
        Some(id) => id,
        None => {
            warn!("Unauthenticated WebSocket connection attempt to /ws/client");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };

    info!("Authenticated WebSocket upgrade for user: {}", user_id);
    ws.on_upgrade(move |socket| {
        web_client_socket::handle_web_client_socket(socket, app_state, user_id)
    })
}
