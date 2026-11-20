pub(crate) mod browser_redaction;
mod claude_cost;
use tracing::Instrument;
mod continuations;
pub mod launcher_socket;
mod member_status;
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
    conn_channel, ConnReceiver, ConnSender, DataPlaneConnection, DataPlaneSender, ForwardHealth,
    LauncherConnection, ProxySender, SessionId, SessionManager, TunnelError, TunnelIn,
    WebClientSender, DATA_PLANE_CHANNEL_CAPACITY, LAUNCHER_CHANNEL_CAPACITY,
    LAUNCHER_LIVENESS_DEADLINE_SECS, LIVENESS_SWEEP_INTERVAL_SECS, PROGRESS_BAR_TTL,
    PROXY_CHANNEL_CAPACITY, PROXY_LIVENESS_DEADLINE_SECS, WEB_CLIENT_CHANNEL_CAPACITY,
};
pub(crate) use session_manager::{
    CompactionGate, CompactionPhase, EnqueueInput, LauncherRpcError, LauncherRpcKind,
};

/// Run a socket task's synchronous database work without stalling the runtime.
///
/// The socket loops handle each frame inline, and that handling checks out a
/// pooled connection (`pool.get()` waits up to 30 s when the pool is
/// exhausted) and can wait on row locks. On a worker thread, such a wait
/// stalls every task scheduled there; with one worker per vCPU, two waiting
/// frames freeze heartbeats, dashboards and health checks at once.
/// `block_in_place` hands the worker's other tasks to another thread for the
/// duration, while keeping this connection's frames in order and its borrowed
/// state in place. A current-thread runtime (tests) has no other worker to
/// hand them to, so the work runs inline there.
pub(crate) fn run_blocking<R>(work: impl FnOnce() -> R) -> R {
    match tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor()) {
        Ok(tokio::runtime::RuntimeFlavor::MultiThread) => tokio::task::block_in_place(work),
        _ => work(),
    }
}

#[cfg(test)]
mod run_blocking_tests {
    use std::time::{Duration, Instant};

    /// How long the second task takes to finish while the first one blocks
    /// for 600 ms, on a runtime with a single worker thread.
    fn other_task_latency(use_run_blocking: bool) -> Duration {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let started = Instant::now();
            let blocker = tokio::spawn(async move {
                let wait = || std::thread::sleep(Duration::from_millis(600));
                if use_run_blocking {
                    super::run_blocking(wait);
                } else {
                    wait();
                }
            });
            // Let the blocker occupy the only worker first.
            tokio::task::yield_now().await;
            let other = tokio::spawn(async move { started.elapsed() });
            let latency = other.await.unwrap();
            blocker.await.unwrap();
            latency
        })
    }

    #[test]
    fn other_tasks_keep_running_while_one_blocks() {
        assert!(other_task_latency(true) < Duration::from_millis(300));
    }

    /// Guards the test above: the same setup without `run_blocking` stalls,
    /// so the fast path really is the hand-off and not scheduling luck.
    #[test]
    fn inline_blocking_stalls_the_only_worker() {
        assert!(other_task_latency(false) >= Duration::from_millis(500));
    }

    #[test]
    fn current_thread_runtime_runs_inline() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        assert_eq!(runtime.block_on(async { super::run_blocking(|| 7) }), 7);
        assert_eq!(super::run_blocking(|| 8), 8, "outside any runtime");
    }
}

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
    extract::{ws::WebSocketUpgrade, Query, State},
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
    let request_span = tracing::Span::current();
    ws.max_frame_size(SESSION_WS_MAX_MESSAGE_BYTES)
        .max_message_size(SESSION_WS_MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            proxy_socket::handle_session_socket(socket, app_state).instrument(request_span)
        })
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
    let request_span = tracing::Span::current();
    ws.on_upgrade(move |socket| {
        tunnel_socket::handle_tunnel_data_socket(socket, app_state).instrument(request_span)
    })
}

pub async fn handle_launcher_websocket(
    ws: WebSocketUpgrade,
    State(app_state): State<Arc<AppState>>,
) -> Response {
    let request_span = tracing::Span::current();
    ws.on_upgrade(move |socket| {
        launcher_socket::handle_launcher_socket(socket, app_state).instrument(request_span)
    })
}

// Deliberately no Debug: a query ticket is a credential.
#[derive(serde::Deserialize)]
pub struct ClientSocketQuery {
    ticket: Option<String>,
    /// Narrows cookie authority for a browser view; never authenticates it.
    account: Option<String>,
}

pub async fn handle_web_client_websocket(
    ws: WebSocketUpgrade,
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Query(query): Query<ClientSocketQuery>,
) -> Response {
    if let Some(ticket) = query.ticket {
        if query.account.is_some() || headers.contains_key(axum::http::header::AUTHORIZATION) {
            return StatusCode::BAD_REQUEST.into_response();
        }
        return match crate::handlers::federation::tickets::consume(&app_state, &ticket, &headers) {
            Ok(auth) => {
                let user_id = auth.user_id;
                let request_span = tracing::Span::current();
                ws.on_upgrade(move |socket| {
                    web_client_socket::handle_web_client_socket(
                        socket,
                        app_state,
                        user_id,
                        Some(auth),
                    )
                    .instrument(request_span)
                })
            }
            Err(error) => error.into_response(),
        };
    }
    // Dashboard frames act with owner authority (including launch RPCs).
    // A session token must stay on /ws/session and its scoped agent APIs.
    if let Err(error) = crate::auth::require_manual_launch_credential(&app_state, &headers) {
        return error.into_response();
    }
    let user_id = match crate::auth::extract_user_id(&app_state, Some(&headers), &cookies).ok() {
        Some(id) => id,
        None => {
            warn!("Unauthenticated WebSocket connection attempt to /ws/client");
            return StatusCode::UNAUTHORIZED.into_response();
        }
    };

    // Bind every home reconnect to the account that rendered the view.
    // A cookie changed in another tab must not authorize queued input as
    // a different user, even when both users can edit the same session.
    if query
        .account
        .as_deref()
        .is_some_and(|expected| expected.parse::<uuid::Uuid>().ok() != Some(user_id))
    {
        return StatusCode::FORBIDDEN.into_response();
    }

    info!("Authenticated WebSocket upgrade for user: {}", user_id);
    let request_span = tracing::Span::current();
    ws.on_upgrade(move |socket| {
        web_client_socket::handle_web_client_socket(socket, app_state, user_id, None)
            .instrument(request_span)
    })
}

/// A stalled transport must release its queued and in-flight payloads. A send
/// timeout ends the connection: cancellation makes delivery uncertain, so the
/// same socket must never be reused after an error.
struct DeadlineSender<T: ws_bridge::WsCodec>(ws_bridge::WsSender<T>);
impl<T: ws_bridge::WsCodec> DeadlineSender<T> {
    async fn send(&mut self, message: T) -> Result<(), ws_bridge::SendError> {
        tokio::time::timeout(std::time::Duration::from_secs(15), self.0.send(message))
            .await
            .unwrap_or(Err(ws_bridge::SendError::Closed))
    }
    async fn close(&mut self) -> Result<(), ws_bridge::SendError> {
        tokio::time::timeout(std::time::Duration::from_secs(15), self.0.close())
            .await
            .unwrap_or(Err(ws_bridge::SendError::Closed))
    }
}
