//! Bounded WebSocket dial to the backend.
//!
//! `ws_bridge::native_client::connect` has no deadline of its own. When the TCP
//! connection opens but the TLS or upgrade handshake is never answered — a
//! load balancer holding a half-open socket after a network blip — the await
//! never returns. Heartbeat watchdogs only start once a connection exists, so
//! nothing else notices, and the caller's retry loop never runs again. Every
//! production dial goes through [`connect`] so a stalled handshake fails like
//! any other connect error and the caller's backoff retries it.

use std::time::Duration;

use ws_bridge::native_client::Connection;
use ws_bridge::WsEndpoint;

/// Upper bound on the TCP + TLS + WebSocket upgrade handshake.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug)]
pub enum ConnectError {
    Failed(ws_bridge::native_client::ConnectError),
    TimedOut(Duration),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Failed(e) => e.fmt(f),
            ConnectError::TimedOut(after) => {
                write!(f, "WebSocket handshake timed out after {after:?}")
            }
        }
    }
}

impl std::error::Error for ConnectError {}

/// Dial `base_url` for endpoint `E`, giving up after [`CONNECT_TIMEOUT`].
pub async fn connect<E: WsEndpoint>(base_url: &str) -> Result<Connection<E>, ConnectError> {
    connect_within::<E>(base_url, CONNECT_TIMEOUT).await
}

async fn connect_within<E: WsEndpoint>(
    base_url: &str,
    limit: Duration,
) -> Result<Connection<E>, ConnectError> {
    match tokio::time::timeout(limit, ws_bridge::native_client::connect::<E>(base_url)).await {
        Ok(result) => result.map_err(ConnectError::Failed),
        Err(_) => Err(ConnectError::TimedOut(limit)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handshake_that_never_answers_times_out() {
        // Accept the TCP connection but never speak, like a half-open socket.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });

        let limit = Duration::from_millis(200);
        let result =
            connect_within::<shared::SessionEndpoint>(&format!("ws://{address}"), limit).await;

        assert!(matches!(result, Err(ConnectError::TimedOut(after)) if after == limit));
        server.abort();
    }
}
