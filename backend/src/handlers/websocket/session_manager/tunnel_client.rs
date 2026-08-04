//! Backend side of the port-forward tunnel (docs/PORT_FORWARDING.md).
//!
//! [`SessionManager::open_tunnel`] opens a stream over a session's WebSocket
//! (`TunnelOpen` → `TunnelOpened`/`TunnelRefused`) and returns one end of an
//! in-process duplex pipe; a relay task copies bytes between that pipe and
//! the tunnel frames. The reverse-proxy handler hands the other end to a
//! hyper HTTP/1.1 client — hyper never knows it isn't a TCP socket.
//!
//! Flow control mirrors the proxy side (`session_lib::tunnel`): a per-stream
//! credit per direction and a max frame size, window re-granted as bytes drain
//! into the pipe. Both come from the connection's negotiated
//! [`shared::TunnelSizing`] (#1511) rather than a fixed constant — the sender
//! and receiver of a stream must agree, so the values are chosen once per
//! connection and carried on the stream. The outgoing path is the session's
//! unbounded `ProxySender`, so per-stream credit is what bounds queued tunnel
//! bytes: at most `MAX_STREAMS × window` per session in the pathological case,
//! in practice one window per active stream.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use dashmap::DashMap;
use shared::{
    ServerToProxy, TunnelCloseFields, TunnelDataFields, TunnelFrame, TunnelOpenFields,
    TunnelRefuseReason, TunnelWindowFields,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::debug;
use uuid::Uuid;

use super::{ProxySender, SessionManager};

/// How long to wait for the proxy's `TunnelOpened`/`TunnelRefused` before
/// giving up and reporting `agent-unreachable`.
///
/// **Invariant (#1504):** this must exceed the proxy's worst-case time to
/// *produce* a verdict, plus network RTT, or a truthful `no-listener` refusal
/// arrives after the deadline — the stream is already reaped, so the verdict is
/// dropped and the user sees the misleading `agent-unreachable` instead. The
/// proxy retries a refused loopback dial for `STREAM_DIAL_RETRY_BUDGET` (8 s)
/// with a `DIAL_TIMEOUT` (2 s) final attempt, so a mid-restart origin can emit
/// its refusal as late as ~10 s. 15 s leaves ~3 s of RTT/queueing headroom;
/// keep `STREAM_DIAL_RETRY_BUDGET + DIAL_TIMEOUT + RTT < OPEN_TIMEOUT`.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// Duplex pipe capacity between the relay and hyper.
const PIPE_CAPACITY: usize = 64 * 1024;

/// Whether a proxy connection last heard from at `last_seen` (epoch secs) has
/// been silent long enough — relative to `now` — that `open_tunnel` should
/// treat it as gone rather than wait `OPEN_TIMEOUT` on a reply that can't come.
/// Mirrors the liveness sweeper's deadline exactly, so a live, recently-
/// heartbeating proxy is never mis-flagged. Pure for unit testing.
fn connection_silent_too_long(last_seen: u64, now: u64) -> bool {
    now.saturating_sub(last_seen) > super::liveness::PROXY_LIVENESS_DEADLINE_SECS
}

/// Frames routed from the proxy socket into a stream's relay task.
pub enum TunnelIn {
    Opened,
    Refused(TunnelRefuseReason),
    Data(Vec<u8>),
    Window(u32),
    Close,
}

/// One live backend stream: the relay inbox plus receive-credit enforcement
/// (mirrors the proxy side — the inbox is unbounded, so the credit book, not
/// the channel, bounds buffered downlink bytes to the negotiated window even
/// against a buggy peer).
pub(super) struct BackendStreamEntry {
    inbox: mpsc::UnboundedSender<TunnelIn>,
    recv_credit: Arc<std::sync::atomic::AtomicI64>,
    /// Negotiated max frame size for this stream (#1511). Stored per stream so
    /// the receive-side oversize check uses the value this connection agreed on,
    /// not a global constant, keeping it correct if profiles ever differ.
    max_chunk: usize,
    /// The connection this stream was opened on. Its relay task holds a clone
    /// of that connection's `ProxySender`, so when the connection tears down
    /// we must close the stream (dropping the clone) — otherwise the session
    /// socket's `send_task` never sees its channel close and hangs. Keyed on
    /// `gen` too so a reconnect can't reap streams opened on the new
    /// connection.
    session_key: String,
    gen: u64,
}

/// Registry of live backend tunnel streams, keyed by stream id.
pub(super) type TunnelStreamMap = Arc<DashMap<Uuid, BackendStreamEntry>>;

/// Where a stream's outbound frames go: the dedicated binary data plane when
/// one is registered for this connection, otherwise the legacy JSON path over
/// the session control socket (#1506).
///
/// Selected once per stream at open time and carried by the relay, so the copy
/// loops stay transport-agnostic. Choosing per stream (rather than globally) is
/// what makes the rollout safe: a proxy without the capability, or one whose
/// data socket has dropped, transparently keeps the old path.
#[derive(Clone)]
pub(super) enum TunnelEgress {
    /// JSON frames over the shared control socket — base64-encoded payloads.
    Control(ProxySender),
    /// Binary frames over the dedicated data plane — raw payloads.
    Binary(super::DataPlaneSender),
}

impl TunnelEgress {
    /// Human label for logs/metrics.
    fn kind(&self) -> &'static str {
        match self {
            Self::Control(_) => "control",
            Self::Binary(_) => "binary",
        }
    }

    fn open(&self, stream_id: Uuid, port: u16) -> bool {
        match self {
            Self::Control(tx) => tx
                .send(ServerToProxy::TunnelOpen(TunnelOpenFields {
                    stream_id,
                    port,
                }))
                .is_ok(),
            Self::Binary(tx) => tx.send(TunnelFrame::Open { stream_id, port }).is_ok(),
        }
    }

    fn data(&self, stream_id: Uuid, bytes: &[u8]) -> bool {
        match self {
            Self::Control(tx) => tx
                .send(ServerToProxy::TunnelData(TunnelDataFields {
                    stream_id,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                }))
                .is_ok(),
            // The point of the binary plane: the payload goes on the wire as-is.
            Self::Binary(tx) => tx
                .send(TunnelFrame::Data {
                    stream_id,
                    bytes: bytes.to_vec(),
                })
                .is_ok(),
        }
    }

    fn window(&self, stream_id: Uuid, add_bytes: u32) -> bool {
        match self {
            Self::Control(tx) => tx
                .send(ServerToProxy::TunnelWindow(TunnelWindowFields {
                    stream_id,
                    add_bytes,
                }))
                .is_ok(),
            Self::Binary(tx) => tx
                .send(TunnelFrame::Window {
                    stream_id,
                    add_bytes,
                })
                .is_ok(),
        }
    }

    fn close(&self, stream_id: Uuid, reason: Option<String>) -> bool {
        match self {
            Self::Control(tx) => tx
                .send(ServerToProxy::TunnelClose(TunnelCloseFields {
                    stream_id,
                    reason,
                }))
                .is_ok(),
            Self::Binary(tx) => tx.send(TunnelFrame::Close { stream_id, reason }).is_ok(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("session proxy is not connected")]
    NotConnected,
    #[error("proxy refused the stream: {0}")]
    Refused(TunnelRefuseReason),
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
            // A verdict (`Opened`/`Refused`) for an unknown stream almost always
            // means it arrived *after* `OPEN_TIMEOUT` reaped the stream — so the
            // browser was already told `agent-unreachable` even though the proxy
            // did answer. Log that at INFO so the misclassification is visible
            // (#1504) rather than silently dropped; it also disambiguates a lost
            // frame (no late verdict ever arrives) from a slow one. Data/Window/
            // Close for an unknown stream are ordinary post-close races — quiet.
            match msg {
                TunnelIn::Opened => tracing::info!(
                    "Late tunnel Opened for reaped stream {stream_id} \
                     (arrived after OPEN_TIMEOUT; was reported agent-unreachable)"
                ),
                TunnelIn::Refused(reason) => tracing::info!(
                    "Late tunnel Refused({reason:?}) for reaped stream {stream_id} \
                     (arrived after OPEN_TIMEOUT; was reported agent-unreachable — \
                     the truthful verdict was this refusal)"
                ),
                _ => debug!("Tunnel frame for unknown stream {} dropped", stream_id),
            }
        }
    }

    /// Route a JSON-plane `TunnelData` frame: decode outside any map lock, then
    /// hand the bytes to the shared ingest path.
    ///
    /// This is the legacy control-socket transport, kept for proxies that don't
    /// advertise [`PROXY_CAPABILITY_TUNNEL_BINARY_V1`](shared::PROXY_CAPABILITY_TUNNEL_BINARY_V1).
    /// The binary data plane calls [`Self::tunnel_bytes_in`] directly — same
    /// size and credit enforcement, no base64 step.
    pub fn tunnel_data_in(&self, fields: &shared::TunnelDataFields) {
        match base64::engine::general_purpose::STANDARD.decode(&fields.data_base64) {
            Ok(bytes) => self.tunnel_bytes_in(fields.stream_id, bytes),
            Err(_) => {
                tracing::warn!("Undecodable TunnelData for stream {}", fields.stream_id);
                if let Some(entry) = self.tunnel_streams.get(&fields.stream_id) {
                    let _ = entry.value().inbox.send(TunnelIn::Close);
                }
            }
        }
    }

    /// Shared ingest for stream payload bytes, whichever transport carried them:
    /// reject oversized frames and enforce the granted receive window before the
    /// bytes may enter the unbounded relay inbox.
    ///
    /// The credit book — not the channel — is what bounds buffered downlink
    /// bytes, so this check is load-bearing against a buggy or hostile peer.
    pub fn tunnel_bytes_in(&self, stream_id: Uuid, bytes: Vec<u8>) {
        let entry = self.tunnel_streams.get(&stream_id).map(|e| {
            (
                e.value().inbox.clone(),
                e.value().recv_credit.clone(),
                e.value().max_chunk,
            )
        });
        let Some((inbox, recv_credit, max_chunk)) = entry else {
            debug!("Tunnel payload for unknown stream {} dropped", stream_id);
            return;
        };
        if bytes.len() > max_chunk {
            tracing::warn!(
                "Oversized tunnel payload ({} bytes) for stream {}; closing",
                bytes.len(),
                stream_id
            );
            let _ = inbox.send(TunnelIn::Close);
            return;
        }
        let prev = recv_credit.fetch_sub(bytes.len() as i64, std::sync::atomic::Ordering::AcqRel);
        if prev < bytes.len() as i64 {
            tracing::warn!(
                "Tunnel payload beyond granted window for stream {}; closing",
                stream_id
            );
            let _ = inbox.send(TunnelIn::Close);
        } else {
            let _ = inbox.send(TunnelIn::Data(bytes));
        }
    }

    /// Close every tunnel stream opened on the `(session_key, gen)`
    /// connection: send `Close` to each relay so it drops its `ProxySender`
    /// clone and exits, then drop the registry entry. Called when a session
    /// socket tears down, so the socket's `send_task` can see its channel
    /// close instead of hanging on a stalled relay's held sender.
    pub fn close_tunnels_for_connection(&self, session_key: &str, gen: u64) {
        let doomed: Vec<Uuid> = self
            .tunnel_streams
            .iter()
            .filter(|e| e.value().session_key == session_key && e.value().gen == gen)
            .map(|e| *e.key())
            .collect();
        for stream_id in doomed {
            if let Some((_, entry)) = self.tunnel_streams.remove(&stream_id) {
                let _ = entry.inbox.send(TunnelIn::Close);
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
        // Capture the sender and its connection generation together so the
        // stream is reaped with the exact connection it rode on. Single
        // lookup — sender and gen live in the same entry, so they can't
        // disagree.
        let (proxy_tx, gen): (ProxySender, u64) = match self.sessions.get(session_key) {
            Some(conn) => {
                // Fast-fail a registered-but-silent connection. A proxy
                // heartbeats every 1s, so silence past the liveness deadline
                // means the transport is gone and the sweeper simply hasn't
                // evicted it yet (it runs every `LIVENESS_SWEEP_INTERVAL_SECS`).
                // Without this the open below would wait the full `OPEN_TIMEOUT`
                // for a reply that can never come — a 10s hang surfacing as the
                // ambiguous `agent-unreachable`. Reusing the sweeper's own
                // deadline means we never mis-flag a live, recently-
                // heartbeating proxy.
                let last_seen = conn.last_seen.load(std::sync::atomic::Ordering::Relaxed);
                if connection_silent_too_long(last_seen, super::liveness::epoch_secs()) {
                    debug!(
                        "open_tunnel: session {} silent since {} — treating as offline",
                        session_key, last_seen
                    );
                    return Err(TunnelError::NotConnected);
                }
                (conn.sender.clone(), conn.gen)
            }
            None => return Err(TunnelError::NotConnected),
        };

        // Prefer the dedicated binary data plane when this exact connection has
        // one registered; otherwise fall back to JSON over the control socket.
        // Resolved per stream, so a data socket that appears or drops mid-session
        // is picked up (or degraded away from) without any coordination. The
        // data plane carries its negotiated sizing; the control fallback is
        // always V1, which is exactly what a pre-#1511 proxy on that path uses.
        let (egress, sizing) = match self.data_plane_egress(session_key, gen) {
            Some((tx, sizing)) => (TunnelEgress::Binary(tx), sizing),
            None => (
                TunnelEgress::Control(proxy_tx.clone()),
                shared::TunnelSizing::V1,
            ),
        };

        let stream_id = Uuid::new_v4();
        let (relay_tx, mut relay_rx) = mpsc::unbounded_channel::<TunnelIn>();
        let recv_credit = Arc::new(std::sync::atomic::AtomicI64::new(
            sizing.initial_window as i64,
        ));
        self.tunnel_streams.insert(
            stream_id,
            BackendStreamEntry {
                inbox: relay_tx,
                recv_credit: recv_credit.clone(),
                max_chunk: sizing.max_chunk as usize,
                session_key: session_key.to_string(),
                gen,
            },
        );

        if !egress.open(stream_id, port) {
            self.tunnel_streams.remove(&stream_id);
            return Err(TunnelError::NotConnected);
        }

        // First frame must be the open verdict.
        let verdict = tokio::time::timeout(OPEN_TIMEOUT, relay_rx.recv()).await;
        match verdict {
            Ok(Some(TunnelIn::Opened)) => {}
            Ok(Some(TunnelIn::Refused(reason))) => {
                self.tunnel_streams.remove(&stream_id);
                return Err(TunnelError::Refused(reason));
            }
            Ok(_) => {
                self.tunnel_streams.remove(&stream_id);
                return Err(TunnelError::ClosedEarly);
            }
            Err(_) => {
                self.tunnel_streams.remove(&stream_id);
                // Best effort: tell the proxy to forget the half-open stream.
                egress.close(stream_id, Some("open timed out".to_string()));
                return Err(TunnelError::OpenTimeout);
            }
        }

        // INFO, not DEBUG: a deployment diagnosing a forward incident needs to
        // see that opens are arriving and succeeding without turning on debug
        // logging fleet-wide (#1504).
        tracing::info!(
            "Tunnel stream {} open over the {} plane",
            stream_id,
            egress.kind()
        );
        let (client_io, relay_io) = tokio::io::duplex(PIPE_CAPACITY);
        let streams = self.tunnel_streams.clone();
        tokio::spawn(run_relay(
            stream_id,
            egress,
            sizing,
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
        // Saturate rather than wrap on absurd `TunnelWindow` values (parity
        // with the proxy-side gate).
        let mut avail = self.avail.lock().await;
        *avail = avail.saturating_add(n);
        self.notify.notify_waiters();
    }
}

/// Copy loop between the duplex pipe (hyper side) and tunnel frames.
async fn run_relay(
    stream_id: Uuid,
    egress: TunnelEgress,
    sizing: shared::TunnelSizing,
    relay_io: tokio::io::DuplexStream,
    mut inbox: mpsc::UnboundedReceiver<TunnelIn>,
    streams: TunnelStreamMap,
    recv_credit: Arc<std::sync::atomic::AtomicI64>,
) {
    let (mut pipe_rd, mut pipe_wr) = tokio::io::split(relay_io);
    let send_credit = Arc::new(CreditGate::new(sizing.initial_window));
    let max_chunk = sizing.max_chunk as usize;

    // Uplink: bytes hyper writes (requests) → payload frames, credit-gated.
    let uplink_credit = send_credit.clone();
    let uplink_egress = egress.clone();
    let uplink = tokio::spawn(async move {
        let mut buf = vec![0u8; max_chunk];
        loop {
            let budget = uplink_credit.take(max_chunk).await;
            let n = match pipe_rd.read(&mut buf[..budget]).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if n < budget {
                uplink_credit.grant((budget - n) as u32).await;
            }
            if !uplink_egress.data(stream_id, &buf[..n]) {
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
                egress.window(stream_id, bytes.len() as u32);
            }
            Some(TunnelIn::Window(n)) => send_credit.grant(n).await,
            Some(TunnelIn::Opened) => {} // late duplicate; ignore
            Some(TunnelIn::Refused(_)) | Some(TunnelIn::Close) | None => break,
        }
    }

    uplink.abort();
    streams.remove(&stream_id);
    egress.close(stream_id, None);
    debug!("Backend tunnel stream {} closed", stream_id);
}

#[cfg(test)]
mod tests {
    use super::super::SessionId;
    use super::*;
    use crate::handlers::websocket::SessionManager;

    fn insert_stream(
        mgr: &SessionManager,
        key: &str,
        gen: u64,
    ) -> (Uuid, mpsc::UnboundedReceiver<TunnelIn>) {
        // Tunnel inboxes are NOT connection senders — they stay unbounded
        // (flow control is the tunnel window, not the channel).
        let (tx, rx) = mpsc::unbounded_channel();
        let id = Uuid::new_v4();
        mgr.tunnel_streams.insert(
            id,
            BackendStreamEntry {
                inbox: tx,
                recv_credit: Arc::new(std::sync::atomic::AtomicI64::new(
                    shared::TunnelSizing::V1.initial_window as i64,
                )),
                max_chunk: shared::TunnelSizing::V1.max_chunk as usize,
                session_key: key.to_string(),
                gen,
            },
        );
        (id, rx)
    }

    #[test]
    fn close_tunnels_scoped_to_connection_gen() {
        let mgr = SessionManager::new();
        // Two streams on the same session but different connection gens, plus
        // one on a different session.
        let (old_id, mut old_rx) = insert_stream(&mgr, "s1", 1);
        let (new_id, mut new_rx) = insert_stream(&mgr, "s1", 2);
        let (other_id, mut other_rx) = insert_stream(&mgr, "s2", 1);

        // Closing gen 1 of s1 must only reap the old stream.
        mgr.close_tunnels_for_connection("s1", 1);

        assert!(!mgr.tunnel_streams.contains_key(&old_id));
        assert!(mgr.tunnel_streams.contains_key(&new_id));
        assert!(mgr.tunnel_streams.contains_key(&other_id));
        assert!(matches!(old_rx.try_recv(), Ok(TunnelIn::Close)));
        // The newer connection's stream and the other session are untouched.
        assert!(new_rx.try_recv().is_err());
        assert!(other_rx.try_recv().is_err());
    }

    #[test]
    fn connection_silent_too_long_matches_the_liveness_deadline() {
        let deadline = super::super::liveness::PROXY_LIVENESS_DEADLINE_SECS;
        let now = 1_000_000u64;
        // Just heard / within the deadline → live.
        assert!(!connection_silent_too_long(now, now));
        assert!(!connection_silent_too_long(now - deadline, now));
        // One second past the deadline → stale.
        assert!(connection_silent_too_long(now - deadline - 1, now));
        // A future/garbage last_seen saturates to zero elapsed → never stale.
        assert!(!connection_silent_too_long(now + 100, now));
    }

    #[tokio::test]
    async fn open_tunnel_reports_offline_for_a_silent_registered_session() {
        use crate::handlers::websocket::conn_channel;
        use shared::ServerToProxy;
        use std::sync::atomic::Ordering;

        let mgr = SessionManager::new();
        let (tx, _rx) = conn_channel::<ServerToProxy>(64);
        mgr.register_session(
            SessionId::new("k"),
            tx,
            tokio_util::sync::CancellationToken::new(),
        );

        // Backdate the connection well past the liveness deadline — a
        // registered-but-dead transport the sweeper hasn't evicted yet.
        if let Some(conn) = mgr.sessions.get("k") {
            let stale = super::super::liveness::epoch_secs()
                .saturating_sub(super::super::liveness::PROXY_LIVENESS_DEADLINE_SECS + 5);
            conn.last_seen.store(stale, Ordering::Relaxed);
        }

        // Fast-fails as NotConnected (→ agent-offline) instead of hanging the
        // full OPEN_TIMEOUT waiting for a reply that can never arrive.
        assert!(matches!(
            mgr.open_tunnel("k", 8080).await,
            Err(TunnelError::NotConnected)
        ));
    }
}
