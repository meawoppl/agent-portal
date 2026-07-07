//! Backend side of the port-forward tunnel (docs/PORT_FORWARDING.md).
//!
//! [`SessionManager::open_tunnel`] opens a stream over a session's WebSocket
//! (`TunnelOpen` → `TunnelOpened`/`TunnelRefused`) and returns one end of an
//! in-process duplex pipe; a relay task copies bytes between that pipe and
//! the tunnel frames. The reverse-proxy handler hands the other end to a
//! hyper HTTP/1.1 client — hyper never knows it isn't a TCP socket.
//!
//! Flow control mirrors the proxy side (`session_lib::tunnel`): 256 KiB
//! credit per direction, ≤16 KiB `TunnelData` frames, window re-granted as
//! bytes drain into the pipe. The outgoing path is the session's unbounded
//! `ProxySender`, so per-stream credit is what bounds queued tunnel bytes:
//! at most `MAX_STREAMS × INITIAL_WINDOW` (16 MiB) per session in the
//! pathological case, in practice one window per active stream.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use dashmap::DashMap;
use shared::{
    ServerToProxy, TunnelCloseFields, TunnelDataFields, TunnelOpenFields, TunnelWindowFields,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::debug;
use uuid::Uuid;

use super::{ProxySender, SessionManager};

/// Max decoded bytes per `TunnelData` frame (spec).
pub const MAX_CHUNK: usize = 16 * 1024;
/// Initial per-stream, per-direction flow-control window (spec).
pub const INITIAL_WINDOW: u32 = 256 * 1024;
/// How long to wait for the proxy's `TunnelOpened`/`TunnelRefused`.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);
/// Duplex pipe capacity between the relay and hyper.
const PIPE_CAPACITY: usize = 64 * 1024;

/// Frames routed from the proxy socket into a stream's relay task.
pub enum TunnelIn {
    Opened,
    Refused(String),
    Data(Vec<u8>),
    Window(u32),
    Close,
}

/// One live backend stream: the relay inbox plus receive-credit enforcement
/// (mirrors the proxy side — the inbox is unbounded, so the credit book, not
/// the channel, bounds buffered downlink bytes to the 256 KiB window even
/// against a buggy peer).
pub(super) struct BackendStreamEntry {
    inbox: mpsc::UnboundedSender<TunnelIn>,
    recv_credit: Arc<std::sync::atomic::AtomicI64>,
}

/// Registry of live backend tunnel streams, keyed by stream id.
pub(super) type TunnelStreamMap = Arc<DashMap<Uuid, BackendStreamEntry>>;

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("session proxy is not connected")]
    NotConnected,
    #[error("proxy refused the stream: {0}")]
    Refused(String),
    #[error("timed out waiting for the proxy to open the stream")]
    OpenTimeout,
    #[error("stream closed while opening")]
    ClosedEarly,
}

impl SessionManager {
    /// Route an incoming tunnel frame from a proxy socket to its stream's
    /// relay task. Unknown stream ids are post-close races — dropped quietly.
    pub fn tunnel_in(&self, stream_id: Uuid, msg: TunnelIn) {
        if let Some(entry) = self.tunnel_streams.get(&stream_id) {
            let _ = entry.value().inbox.send(msg);
        } else {
            debug!("Tunnel frame for unknown stream {} dropped", stream_id);
        }
    }

    /// Route a `TunnelData` frame: decode outside any map lock, reject
    /// oversized frames, and enforce the granted receive window before the
    /// bytes may enter the unbounded relay inbox.
    pub fn tunnel_data_in(&self, fields: &shared::TunnelDataFields) {
        let entry = self
            .tunnel_streams
            .get(&fields.stream_id)
            .map(|e| (e.value().inbox.clone(), e.value().recv_credit.clone()));
        let Some((inbox, recv_credit)) = entry else {
            debug!("TunnelData for unknown stream {} dropped", fields.stream_id);
            return;
        };
        match base64::engine::general_purpose::STANDARD.decode(&fields.data_base64) {
            Ok(bytes) if bytes.len() > MAX_CHUNK => {
                tracing::warn!(
                    "Oversized TunnelData ({} bytes) for stream {}; closing",
                    bytes.len(),
                    fields.stream_id
                );
                let _ = inbox.send(TunnelIn::Close);
            }
            Ok(bytes) => {
                let prev =
                    recv_credit.fetch_sub(bytes.len() as i64, std::sync::atomic::Ordering::AcqRel);
                if prev < bytes.len() as i64 {
                    tracing::warn!(
                        "TunnelData beyond granted window for stream {}; closing",
                        fields.stream_id
                    );
                    let _ = inbox.send(TunnelIn::Close);
                } else {
                    let _ = inbox.send(TunnelIn::Data(bytes));
                }
            }
            Err(_) => {
                tracing::warn!("Undecodable TunnelData for stream {}", fields.stream_id);
                let _ = inbox.send(TunnelIn::Close);
            }
        }
    }

    /// Open a tunnel stream to `127.0.0.1:{port}` on the proxy serving
    /// `session_key`. Returns the local end of the byte pipe on success.
    pub async fn open_tunnel(
        &self,
        session_key: &str,
        port: u16,
    ) -> Result<tokio::io::DuplexStream, TunnelError> {
        let proxy_tx: ProxySender = match self.sessions.get(session_key) {
            Some(entry) => entry.value().clone(),
            None => return Err(TunnelError::NotConnected),
        };

        let stream_id = Uuid::new_v4();
        let (relay_tx, mut relay_rx) = mpsc::unbounded_channel::<TunnelIn>();
        let recv_credit = Arc::new(std::sync::atomic::AtomicI64::new(INITIAL_WINDOW as i64));
        self.tunnel_streams.insert(
            stream_id,
            BackendStreamEntry {
                inbox: relay_tx,
                recv_credit: recv_credit.clone(),
            },
        );

        if proxy_tx
            .send(ServerToProxy::TunnelOpen(TunnelOpenFields {
                stream_id,
                port,
            }))
            .is_err()
        {
            self.tunnel_streams.remove(&stream_id);
            return Err(TunnelError::NotConnected);
        }

        // First frame must be the open verdict.
        let verdict = tokio::time::timeout(OPEN_TIMEOUT, relay_rx.recv()).await;
        match verdict {
            Ok(Some(TunnelIn::Opened)) => {}
            Ok(Some(TunnelIn::Refused(error))) => {
                self.tunnel_streams.remove(&stream_id);
                return Err(TunnelError::Refused(error));
            }
            Ok(_) => {
                self.tunnel_streams.remove(&stream_id);
                return Err(TunnelError::ClosedEarly);
            }
            Err(_) => {
                self.tunnel_streams.remove(&stream_id);
                // Best effort: tell the proxy to forget the half-open stream.
                let _ = proxy_tx.send(ServerToProxy::TunnelClose(TunnelCloseFields {
                    stream_id,
                    reason: Some("open timed out".to_string()),
                }));
                return Err(TunnelError::OpenTimeout);
            }
        }

        let (client_io, relay_io) = tokio::io::duplex(PIPE_CAPACITY);
        let streams = self.tunnel_streams.clone();
        tokio::spawn(run_relay(
            stream_id,
            proxy_tx,
            relay_io,
            relay_rx,
            streams,
            recv_credit,
        ));
        Ok(client_io)
    }
}

/// Credit gate: `take` blocks while the window is empty, then consumes up to
/// `max` bytes; `grant` refills. Mirrors `session_lib::tunnel::CreditGate`
/// (session-lib is not a backend dependency, so the ~30 lines are local).
struct CreditGate {
    avail: Mutex<u32>,
    notify: Notify,
}

impl CreditGate {
    fn new(initial: u32) -> Self {
        Self {
            avail: Mutex::new(initial),
            notify: Notify::new(),
        }
    }

    async fn take(&self, max: usize) -> usize {
        loop {
            // Arm the waiter before checking so a concurrent grant can't be
            // missed between the check and the await.
            let notified = self.notify.notified();
            tokio::pin!(notified);
            {
                let mut avail = self.avail.lock().await;
                if *avail > 0 {
                    let n = (*avail as usize).min(max);
                    *avail -= n as u32;
                    return n;
                }
            }
            notified.as_mut().await;
        }
    }

    async fn grant(&self, n: u32) {
        *self.avail.lock().await += n;
        self.notify.notify_waiters();
    }
}

/// Copy loop between the duplex pipe (hyper side) and tunnel frames.
async fn run_relay(
    stream_id: Uuid,
    proxy_tx: ProxySender,
    relay_io: tokio::io::DuplexStream,
    mut inbox: mpsc::UnboundedReceiver<TunnelIn>,
    streams: TunnelStreamMap,
    recv_credit: Arc<std::sync::atomic::AtomicI64>,
) {
    let (mut pipe_rd, mut pipe_wr) = tokio::io::split(relay_io);
    let send_credit = Arc::new(CreditGate::new(INITIAL_WINDOW));

    // Uplink: bytes hyper writes (requests) → TunnelData frames, credit-gated.
    let uplink_credit = send_credit.clone();
    let uplink_tx = proxy_tx.clone();
    let uplink = tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_CHUNK];
        loop {
            let budget = uplink_credit.take(MAX_CHUNK).await;
            let n = match pipe_rd.read(&mut buf[..budget]).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if n < budget {
                uplink_credit.grant((budget - n) as u32).await;
            }
            let frame = ServerToProxy::TunnelData(TunnelDataFields {
                stream_id,
                data_base64: base64::engine::general_purpose::STANDARD.encode(&buf[..n]),
            });
            if uplink_tx.send(frame).is_err() {
                break;
            }
        }
    });

    // Downlink: TunnelData frames (responses) → the pipe, granting window
    // back as bytes drain.
    loop {
        match inbox.recv().await {
            Some(TunnelIn::Data(bytes)) => {
                if pipe_wr.write_all(&bytes).await.is_err() {
                    break; // hyper side dropped (response consumer gone)
                }
                // Grant-on-drain: refill the peer's window and our
                // receive-credit enforcement book together.
                recv_credit.fetch_add(bytes.len() as i64, std::sync::atomic::Ordering::AcqRel);
                let _ = proxy_tx.send(ServerToProxy::TunnelWindow(TunnelWindowFields {
                    stream_id,
                    add_bytes: bytes.len() as u32,
                }));
            }
            Some(TunnelIn::Window(n)) => send_credit.grant(n).await,
            Some(TunnelIn::Opened) => {} // late duplicate; ignore
            Some(TunnelIn::Refused(_)) | Some(TunnelIn::Close) | None => break,
        }
    }

    uplink.abort();
    streams.remove(&stream_id);
    let _ = proxy_tx.send(ServerToProxy::TunnelClose(TunnelCloseFields {
        stream_id,
        reason: None,
    }));
    debug!("Backend tunnel stream {} closed", stream_id);
}
