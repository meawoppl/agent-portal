//! Agent-agnostic pieces of the proxy session (#1657).
//!
//! The proxy session — transport, heartbeat, reminder injection, input
//! delivery — serves **every** agent that runs under the proxy, but it grew up
//! inside `claude-session-lib`, where nothing in the module path said which
//! parts were shared. This module is the landing zone as those pieces hoist
//! out, slice by slice; `claude-session-lib::proxy_session` re-exports each
//! moved item so its internal call sites are unchanged.

pub mod heartbeat_watchdog;
pub mod input_delivery;
pub mod permission_bridge;
pub mod portal_reminder;
pub mod session_event;
pub mod ws_reader;

use std::sync::Arc;
use std::time::Duration;

use shared::{ProxyToServer, ServerToProxy, SessionEndpoint};

/// Permission response data routed from the portal to an agent session.
#[derive(Debug)]
pub struct PermissionResponseData {
    pub request_id: String,
    pub allow: bool,
    pub input: Option<serde_json::Value>,
    pub permissions: Vec<claude_codes::io::PermissionSuggestion>,
    pub reason: Option<String>,
}

/// Portal-originated user input plus optional backend acknowledgement metadata.
pub struct PortalInput {
    pub text: String,
    pub display_event: Option<serde_json::Value>,
    pub reasoning_effort: Option<shared::ReasoningEffort>,
    pub ack: Option<PortalInputAck>,
    pub client_msg_id: Option<uuid::Uuid>,
}

pub struct PortalInputAck {
    pub session_id: uuid::Uuid,
    pub seq: i64,
}

pub struct GracefulShutdown {
    pub reconnect_delay_ms: u64,
}

/// The native WebSocket connection to the backend session endpoint.
pub type NativeConnection = ws_bridge::native_client::Connection<SessionEndpoint>;

/// The shared WebSocket write half.
pub type SharedWsWrite = Arc<tokio::sync::Mutex<ws_bridge::WsSender<ProxyToServer>>>;

/// The WebSocket read half.
pub type WsRead = ws_bridge::WsReceiver<ServerToProxy>;

/// Result from a single WebSocket connection attempt.
pub enum ConnectionResult {
    /// The agent process exited normally.
    AgentExited,
    /// WebSocket disconnected, includes how long the connection was up.
    Disconnected(Duration),
    /// Session not found error - need to restart with fresh session.
    SessionNotFound,
    /// Server is shutting down gracefully, includes suggested reconnect delay.
    ServerShutdown(Duration),
    /// Session was terminated by the server (do not reconnect).
    SessionTerminated,
    /// The backend rejected registration (revoked/expired token, unauthorized).
    /// Reconnecting with the same token can never succeed, so the proxy must
    /// stop rather than hammer the backend forever (#1045).
    RegistrationRejected,
}

/// Emit an input-delivery stage for a browser-tracked message.
pub async fn emit_input_progress(
    ws_write: &SharedWsWrite,
    session_id: uuid::Uuid,
    client_msg_id: Option<uuid::Uuid>,
    stage: shared::InputDeliveryStage,
) {
    let Some(client_msg_id) = client_msg_id else {
        return;
    };
    let msg = ProxyToServer::InputProgressAck {
        session_id,
        client_msg_id,
        stage,
    };
    let mut ws = ws_write.lock().await;
    if let Err(error) = ws.send(msg).await {
        tracing::error!("Failed to send InputProgressAck: {error}");
    }
}

/// Acknowledge a persisted portal input after the agent accepts it.
pub async fn ack_portal_input(ws_write: &SharedWsWrite, ack: Option<PortalInputAck>) {
    let Some(ack) = ack else {
        return;
    };
    let msg = ProxyToServer::InputAck {
        session_id: ack.session_id,
        ack_seq: ack.seq,
    };
    let mut ws = ws_write.lock().await;
    if let Err(error) = ws.send(msg).await {
        tracing::error!("Failed to send InputAck: {error}");
    }
}
