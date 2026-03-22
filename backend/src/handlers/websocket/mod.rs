mod auth;
pub mod launcher_socket;
mod message_handlers;
mod permissions;
mod proxy_socket;
mod registration;
mod session_manager;
mod web_client_socket;

pub use session_manager::{
    LauncherConnection, ProxySender, SessionId, SessionManager, WebClientSender,
};

use axum::{
    extract::{ws::WebSocketUpgrade, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use tower_cookies::Cookies;
use tracing::{info, warn};

use crate::AppState;

pub async fn handle_session_websocket(
    ws: WebSocketUpgrade,
    State(app_state): State<Arc<AppState>>,
) -> Response {
    ws.on_upgrade(|socket| proxy_socket::handle_session_socket(socket, app_state))
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
    cookies: Cookies,
) -> Response {
    let user_id = match crate::auth::extract_user_id(&app_state, &cookies).ok() {
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
