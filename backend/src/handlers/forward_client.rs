//! Pooled HTTP client for forwarded origins.
//!
//! Each pool authority names one `(session, connection generation, port)`.
//! Generation is part of the key so a proxy reconnect can never inherit a TCP
//! stream opened by the connection it replaced.

use std::error::Error as StdError;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use axum::body::Body;
use dashmap::DashMap;
use hyper::body::Incoming;
use hyper::Uri;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use tower_service::Service;
use uuid::Uuid;

use super::websocket::{SessionManager, TunnelError};

#[derive(Clone)]
pub(crate) struct ForwardHttpClient {
    client: Client<TunnelConnector, Body>,
    targets: std::sync::Arc<DashMap<String, TunnelTarget>>,
}

#[derive(Clone)]
struct TunnelConnector {
    session_manager: SessionManager,
    targets: std::sync::Arc<DashMap<String, TunnelTarget>>,
}

#[derive(Clone)]
struct TunnelTarget {
    session_key: String,
    generation: u64,
    port: u16,
}

#[derive(Debug)]
pub(crate) struct ForwardResponse {
    pub(crate) response: hyper::Response<Incoming>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ForwardClientError {
    #[error("could not construct the internal forward URI: {0}")]
    InvalidUri(#[from] hyper::http::uri::InvalidUri),
    #[error("forward HTTP request failed: {0}")]
    Client(#[from] hyper_util::client::legacy::Error),
}

impl ForwardHttpClient {
    pub(crate) fn new(session_manager: SessionManager) -> Self {
        let targets = std::sync::Arc::new(DashMap::new());
        let connector = TunnelConnector {
            session_manager,
            targets: targets.clone(),
        };
        // A page load can fan out into hundreds of concurrent HTTP/1.1
        // connections after the browser's HTTP/2 connection terminates here.
        // Reuse them, but retain only a small warm set: otherwise a completed
        // burst would occupy the proxy's stream cap until every upstream
        // keep-alive timed out.
        let client = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .pool_timer(TokioTimer::new())
            .build(connector);
        Self { client, targets }
    }

    /// Send one request through the pool for this exact proxy connection.
    ///
    /// The synthetic authority is stable for the lifetime of a connection,
    /// enabling HTTP/1.1 keep-alive reuse. It changes after reconnect, making
    /// stale pooled connections structurally unreachable.
    pub(crate) async fn request(
        &self,
        session_id: Uuid,
        session_key: &str,
        generation: u64,
        port: u16,
        mut request: hyper::Request<Body>,
    ) -> Result<ForwardResponse, ForwardClientError> {
        let authority = format!("s{session_id}-g{generation}-p{port}.tunnel.invalid");
        self.targets.insert(
            authority.clone(),
            TunnelTarget {
                session_key: session_key.to_string(),
                generation,
                port,
            },
        );

        let path = request
            .uri()
            .path_and_query()
            .map_or("/", hyper::http::uri::PathAndQuery::as_str);
        let absolute = format!("http://{authority}{path}").parse::<Uri>()?;
        *request.uri_mut() = absolute;
        self.client
            .request(request)
            .await
            .map(|response| ForwardResponse { response })
            .map_err(Into::into)
    }
}

impl Service<Uri> for TunnelConnector {
    type Response = TunnelIo;
    type Error = TunnelError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let target = uri
            .authority()
            .and_then(|authority| self.targets.get(authority.as_str()).map(|v| v.clone()));
        let manager = self.session_manager.clone();
        Box::pin(async move {
            let target = target.ok_or(TunnelError::NotConnected)?;
            manager
                .open_tunnel_for_generation(&target.session_key, target.generation, target.port)
                .await
                .map(|stream| TunnelIo(TokioIo::new(stream)))
        })
    }
}

pub(crate) struct TunnelIo(TokioIo<tokio::io::DuplexStream>);

impl hyper::rt::Read for TunnelIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for TunnelIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.0.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.0).poll_write_vectored(cx, bufs)
    }
}

impl Connection for TunnelIo {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

/// Recover the typed tunnel refusal hidden inside the legacy client's error
/// chain. This preserves the existing `at-capacity` / `no-listener` taxonomy
/// even though connection establishment now happens inside the pool.
pub(crate) fn tunnel_error<'a>(error: &'a (dyn StdError + 'static)) -> Option<&'a TunnelError> {
    let mut source = Some(error);
    while let Some(err) = source {
        if let Some(tunnel) = err.downcast_ref::<TunnelError>() {
            return Some(tunnel);
        }
        source = err.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use base64::Engine;
    use shared::{ServerToProxy, TunnelRefuseReason};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn request() -> hyper::Request<Body> {
        hyper::Request::builder()
            .uri("/")
            .body(Body::empty())
            .expect("request")
    }

    #[tokio::test]
    async fn sequential_requests_reuse_one_tunnel_stream() {
        let manager = SessionManager::new();
        let (tx, mut rx) = super::super::websocket::conn_channel::<ServerToProxy>(64);
        let generation = manager.register_session(
            super::super::websocket::SessionId::new("session-key"),
            tx,
            tokio_util::sync::CancellationToken::new(),
        );
        let client = ForwardHttpClient::new(manager.clone());
        let proxy_manager = manager.clone();
        let proxy = tokio::spawn(async move {
            let mut stream_id = None;
            let mut opens = 0;
            let mut requests = 0;
            let mut request_bytes = Vec::new();
            while requests < 2 {
                match rx.recv().await.expect("backend tunnel frame") {
                    ServerToProxy::TunnelOpen(open) => {
                        opens += 1;
                        stream_id = Some(open.stream_id);
                        proxy_manager
                            .tunnel_in(open.stream_id, super::super::websocket::TunnelIn::Opened);
                    }
                    ServerToProxy::TunnelData(data) => {
                        assert_eq!(Some(data.stream_id), stream_id);
                        request_bytes.extend(
                            base64::engine::general_purpose::STANDARD
                                .decode(data.data_base64)
                                .expect("request data"),
                        );
                        if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                            requests += 1;
                            request_bytes.clear();
                            proxy_manager.tunnel_bytes_in(
                                data.stream_id,
                                b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec(),
                            );
                        }
                    }
                    ServerToProxy::TunnelWindow(_) => {}
                    other => panic!("unexpected tunnel frame: {other:?}"),
                }
            }
            opens
        });

        for _ in 0..2 {
            let response = client
                .request(Uuid::nil(), "session-key", generation, 8080, request())
                .await
                .expect("forward request")
                .response;
            assert_eq!(response.status(), hyper::StatusCode::OK);
            assert_eq!(
                to_bytes(Body::new(response.into_body()), 16)
                    .await
                    .expect("body"),
                "ok"
            );
        }

        assert_eq!(proxy.await.expect("proxy task"), 1);
    }

    #[tokio::test]
    async fn connector_refusal_remains_typed_through_client_error() {
        let manager = SessionManager::new();
        let (tx, mut rx) = super::super::websocket::conn_channel::<ServerToProxy>(64);
        let generation = manager.register_session(
            super::super::websocket::SessionId::new("session-key"),
            tx,
            tokio_util::sync::CancellationToken::new(),
        );
        let client = ForwardHttpClient::new(manager.clone());
        let proxy_manager = manager.clone();
        tokio::spawn(async move {
            let ServerToProxy::TunnelOpen(open) = rx.recv().await.expect("open") else {
                panic!("expected tunnel open");
            };
            proxy_manager.tunnel_in(
                open.stream_id,
                super::super::websocket::TunnelIn::Refused(TunnelRefuseReason::StreamLimit),
            );
        });

        let error = client
            .request(Uuid::nil(), "session-key", generation, 8080, request())
            .await
            .expect_err("request should be refused");
        assert!(matches!(
            tunnel_error(&error),
            Some(TunnelError::Refused(TunnelRefuseReason::StreamLimit))
        ));
    }

    #[tokio::test]
    async fn protocol_upgrade_leaves_pool_and_keeps_bidirectional_stream() {
        let manager = SessionManager::new();
        let (tx, mut rx) = super::super::websocket::conn_channel::<ServerToProxy>(64);
        let generation = manager.register_session(
            super::super::websocket::SessionId::new("session-key"),
            tx,
            tokio_util::sync::CancellationToken::new(),
        );
        let client = ForwardHttpClient::new(manager.clone());
        let proxy_manager = manager.clone();
        let proxy = tokio::spawn(async move {
            let mut stream_id = None;
            let mut request_bytes = Vec::new();
            loop {
                match rx.recv().await.expect("backend tunnel frame") {
                    ServerToProxy::TunnelOpen(open) => {
                        stream_id = Some(open.stream_id);
                        proxy_manager
                            .tunnel_in(open.stream_id, super::super::websocket::TunnelIn::Opened);
                    }
                    ServerToProxy::TunnelData(data) => {
                        assert_eq!(Some(data.stream_id), stream_id);
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(data.data_base64)
                            .expect("request data");
                        if bytes == b"ping" {
                            proxy_manager.tunnel_bytes_in(data.stream_id, b"pong".to_vec());
                            return;
                        }
                        request_bytes.extend(bytes);
                        if request_bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                            proxy_manager.tunnel_bytes_in(
                                data.stream_id,
                                b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: test\r\n\r\n"
                                    .to_vec(),
                            );
                        }
                    }
                    ServerToProxy::TunnelWindow(_) => {}
                    other => panic!("unexpected tunnel frame: {other:?}"),
                }
            }
        });

        let upgrade_request = hyper::Request::builder()
            .uri("/")
            .header(hyper::header::CONNECTION, "upgrade")
            .header(hyper::header::UPGRADE, "test")
            .body(Body::empty())
            .expect("upgrade request");
        let mut response = client
            .request(
                Uuid::nil(),
                "session-key",
                generation,
                8080,
                upgrade_request,
            )
            .await
            .expect("forward request")
            .response;
        assert_eq!(response.status(), hyper::StatusCode::SWITCHING_PROTOCOLS);

        let upgraded = hyper::upgrade::on(&mut response)
            .await
            .expect("upgraded tunnel");
        let mut upgraded = TokioIo::new(upgraded);
        upgraded.write_all(b"ping").await.expect("write upgraded");
        let mut reply = [0; 4];
        upgraded
            .read_exact(&mut reply)
            .await
            .expect("read upgraded");
        assert_eq!(&reply, b"pong");
        proxy.await.expect("proxy task");
    }
}
