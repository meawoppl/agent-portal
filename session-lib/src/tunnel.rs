//! Proxy-side tunnel transport for port forwarding (docs/PORT_FORWARDING.md).
//!
//! A [`TunnelManager`] lives for the duration of one session-WebSocket
//! connection. It keeps the `ForwardOpen`-synced port allowlist, answers
//! probe dials with `ForwardStatus`, and runs one task per open stream
//! copying bytes between the backend (WS frames) and `127.0.0.1:{port}`.
//!
//! Backpressure has two layers, per the spec:
//! - **Stream credit**: each direction starts with a 256 KiB window; the
//!   receiver re-grants as it drains bytes into the underlying socket
//!   (`TunnelWindow`). A sender never reads more from TCP than it holds
//!   credit for.
//! - **Writer capacity**: outgoing frames go straight through the shared
//!   `WsSender` mutex (FIFO), one ≤16 KiB frame per lock. There is no queue
//!   to grow — total buffered tunnel data is bounded by streams × 16 KiB and
//!   waiting streams are served round-robin by mutex order. Session frames
//!   share the same mutex, so tunnel traffic can delay but never starve them
//!   behind unbounded queued data.
//!
//! Idle-stream reaping is a backend concern (only it knows which streams are
//! WebSocket upgrades and therefore exempt); the proxy keeps streams until
//! either side closes or the session WS drops.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use shared::{
    ForwardStatusFields, ProxyToServer, ServerToProxy, TunnelCloseFields, TunnelDataFields,
    TunnelOpenFields, TunnelRefusedFields, TunnelStreamFields, TunnelWindowFields,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex, Notify};
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Shared write half of the session WebSocket (same shape as the hosts'
/// `SharedWsWrite` aliases).
pub type TunnelWsWrite = Arc<Mutex<ws_bridge::WsSender<ProxyToServer>>>;

/// Max decoded bytes per `TunnelData` frame.
pub const MAX_CHUNK: usize = 16 * 1024;
/// Initial per-stream, per-direction flow-control window.
pub const INITIAL_WINDOW: u32 = 256 * 1024;
/// Max concurrent streams per session connection.
pub const MAX_STREAMS: usize = 64;
/// How long a probe/stream dial to loopback may take before it is refused.
const DIAL_TIMEOUT: Duration = Duration::from_secs(2);
/// Cadence of the background port-health probe. A loopback dial is
/// microseconds of work, so this can be frequent — it drives the green/red
/// liveness tint on the frontend's forward chip.
const PROBE_INTERVAL: Duration = Duration::from_secs(10);

/// Frames the manager's per-stream downlink loop consumes.
enum StreamMsg {
    Data(Vec<u8>),
    Window(u32),
    Close,
}

struct StreamHandle {
    port: u16,
    inbox: mpsc::UnboundedSender<StreamMsg>,
    /// Receive-side credit enforcement: how many downlink bytes the peer may
    /// still send before it must wait for our `TunnelWindow` grants. The
    /// reader decrements on arrival; the stream task re-increments as bytes
    /// drain into the socket. Going negative is a protocol violation and
    /// closes the stream — the inbox is unbounded, so this (not the channel)
    /// is what bounds per-stream buffered downlink data to the 256 KiB
    /// window even against a buggy or hostile peer.
    recv_credit: Arc<std::sync::atomic::AtomicI64>,
}

/// Per-connection tunnel state. Create one per established session WS,
/// dispatch forward/tunnel frames into [`TunnelManager::handle`], and call
/// [`TunnelManager::shutdown`] when the connection ends.
pub struct TunnelManager {
    ws: TunnelWsWrite,
    allowed: Mutex<HashSet<u16>>,
    streams: Mutex<HashMap<Uuid, StreamHandle>>,
    /// Last probe verdict per allowlisted port (`(listening, process name)`);
    /// the background prober reports a `ForwardStatus` only when a port's
    /// verdict *changes* (the registration-time probe seeds it), so steady
    /// state costs no frames. The process name is part of the verdict so an
    /// app swap on the same port re-reports.
    last_health: Mutex<HashMap<u16, (bool, Option<String>)>>,
    prober: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl TunnelManager {
    pub fn new(ws: TunnelWsWrite) -> Arc<Self> {
        let mgr = Arc::new(Self {
            ws,
            allowed: Mutex::new(HashSet::new()),
            streams: Mutex::new(HashMap::new()),
            last_health: Mutex::new(HashMap::new()),
            prober: std::sync::Mutex::new(None),
        });
        // Background health probe: holds only a Weak so a dropped manager
        // (connection gone) ends the loop; `shutdown` aborts it eagerly.
        let weak = Arc::downgrade(&mgr);
        let handle = tokio::spawn(async move {
            loop {
                tokio::time::sleep(PROBE_INTERVAL).await;
                let Some(mgr) = weak.upgrade() else { return };
                mgr.probe_tick().await;
            }
        });
        *mgr.prober.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        mgr
    }

    /// One pass of the background prober: dial every allowlisted port and
    /// report the ports whose verdict (listening + owning process) changed
    /// since the last pass.
    async fn probe_tick(self: &Arc<Self>) {
        let ports: Vec<u16> = self.allowed.lock().await.iter().copied().collect();
        // Drop verdicts for ports no longer forwarded.
        self.last_health
            .lock()
            .await
            .retain(|port, _| ports.contains(port));
        for port in ports {
            let (listening, error) =
                match tokio::time::timeout(DIAL_TIMEOUT, dial_loopback(port)).await {
                    Ok(Ok(_)) => (true, None),
                    Ok(Err(e)) => (false, Some(e.to_string())),
                    Err(_) => (false, Some("probe dial timed out".to_string())),
                };
            let process = if listening {
                process_on_port(port).await
            } else {
                None
            };
            // Re-check the allowlist after the dial: a `ForwardClose` that
            // raced this tick must not resurrect the port with a stale
            // status (codex review on #1257).
            if !self.allowed.lock().await.contains(&port) {
                self.last_health.lock().await.remove(&port);
                continue;
            }
            let verdict = (listening, process.clone());
            let changed = {
                let mut health = self.last_health.lock().await;
                health.insert(port, verdict.clone()) != Some(verdict)
            };
            if changed {
                info!(
                    "Forward port {} health changed: listening={} process={:?}",
                    port, listening, process
                );
                self.send(ProxyToServer::ForwardStatus(ForwardStatusFields {
                    port,
                    listening,
                    error,
                    process,
                }))
                .await;
            }
        }
    }

    /// Returns `true` if `msg` was a forward/tunnel frame (and was handled).
    /// Never blocks on I/O: dials and probes run in spawned tasks, but stream
    /// handles are registered synchronously so a pipelined `TunnelData`
    /// arriving right after `TunnelOpen` finds its inbox.
    pub async fn handle(self: &Arc<Self>, msg: &ServerToProxy) -> bool {
        match msg {
            ServerToProxy::ForwardOpen(f) => {
                self.allowed.lock().await.insert(f.port);
                info!("Forward allowlist + probe for port {}", f.port);
                let mgr = self.clone();
                let port = f.port;
                tokio::spawn(async move {
                    let (listening, error) =
                        match tokio::time::timeout(DIAL_TIMEOUT, dial_loopback(port)).await {
                            Ok(Ok(_)) => (true, None),
                            Ok(Err(e)) => (false, Some(e.to_string())),
                            Err(_) => (false, Some("probe dial timed out".to_string())),
                        };
                    let process = if listening {
                        process_on_port(port).await
                    } else {
                        None
                    };
                    // A `ForwardClose` may have raced the dial — don't emit a
                    // stale status for a port that's no longer forwarded.
                    if !mgr.allowed.lock().await.contains(&port) {
                        return;
                    }
                    // Seed the background prober so it only reports changes.
                    mgr.last_health
                        .lock()
                        .await
                        .insert(port, (listening, process.clone()));
                    mgr.send(ProxyToServer::ForwardStatus(ForwardStatusFields {
                        port,
                        listening,
                        error,
                        process,
                    }))
                    .await;
                });
                true
            }
            ServerToProxy::ForwardClose(f) => {
                self.allowed.lock().await.remove(&f.port);
                self.last_health.lock().await.remove(&f.port);
                let streams = self.streams.lock().await;
                for handle in streams.values().filter(|h| h.port == f.port) {
                    let _ = handle.inbox.send(StreamMsg::Close);
                }
                info!("Forward closed for port {}", f.port);
                true
            }
            ServerToProxy::TunnelOpen(open) => {
                self.open_stream(open).await;
                true
            }
            ServerToProxy::TunnelData(data) => {
                // Clone the handle out and drop the map lock before the
                // decode — no byte work under the streams mutex.
                let handle = {
                    let streams = self.streams.lock().await;
                    streams
                        .get(&data.stream_id)
                        .map(|h| (h.inbox.clone(), h.recv_credit.clone()))
                };
                // Unknown stream: a post-close race; drop silently.
                if let Some((inbox, recv_credit)) = handle {
                    match base64::engine::general_purpose::STANDARD.decode(&data.data_base64) {
                        Ok(bytes) if bytes.len() > MAX_CHUNK => {
                            warn!(
                                "Oversized TunnelData ({} bytes) for stream {}; closing",
                                bytes.len(),
                                data.stream_id
                            );
                            let _ = inbox.send(StreamMsg::Close);
                        }
                        Ok(bytes) => {
                            // Enforce the peer's send window: data beyond the
                            // credit we granted is a protocol violation, and
                            // the unbounded inbox must not absorb it.
                            let prev = recv_credit
                                .fetch_sub(bytes.len() as i64, std::sync::atomic::Ordering::AcqRel);
                            if prev < bytes.len() as i64 {
                                warn!(
                                    "TunnelData beyond granted window for stream {}; closing",
                                    data.stream_id
                                );
                                let _ = inbox.send(StreamMsg::Close);
                            } else {
                                let _ = inbox.send(StreamMsg::Data(bytes));
                            }
                        }
                        Err(_) => {
                            warn!("Undecodable TunnelData for stream {}", data.stream_id);
                            let _ = inbox.send(StreamMsg::Close);
                        }
                    }
                }
                true
            }
            ServerToProxy::TunnelWindow(win) => {
                let streams = self.streams.lock().await;
                if let Some(handle) = streams.get(&win.stream_id) {
                    let _ = handle.inbox.send(StreamMsg::Window(win.add_bytes));
                }
                true
            }
            ServerToProxy::TunnelClose(close) => {
                let streams = self.streams.lock().await;
                if let Some(handle) = streams.get(&close.stream_id) {
                    let _ = handle.inbox.send(StreamMsg::Close);
                }
                true
            }
            _ => false,
        }
    }

    /// Tear down every stream (session WS ended). The manager is per
    /// connection; a reconnect builds a fresh one and the backend replays
    /// `ForwardOpen`s to rebuild the allowlist.
    pub async fn shutdown(&self) {
        if let Some(prober) = self.prober.lock().unwrap_or_else(|e| e.into_inner()).take() {
            prober.abort();
        }
        let streams = self.streams.lock().await;
        for handle in streams.values() {
            let _ = handle.inbox.send(StreamMsg::Close);
        }
    }

    async fn open_stream(self: &Arc<Self>, open: &TunnelOpenFields) {
        let refuse = |error: String| {
            ProxyToServer::TunnelRefused(TunnelRefusedFields {
                stream_id: open.stream_id,
                error,
            })
        };

        if !self.allowed.lock().await.contains(&open.port) {
            self.send(refuse(format!("port {} is not forwarded", open.port)))
                .await;
            return;
        }
        // Register the inbox before the dial so ordered frames can't miss it.
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel();
        let recv_credit = Arc::new(std::sync::atomic::AtomicI64::new(INITIAL_WINDOW as i64));
        {
            let mut streams = self.streams.lock().await;
            if streams.len() >= MAX_STREAMS {
                drop(streams);
                self.send(refuse(format!("stream limit ({MAX_STREAMS}) reached")))
                    .await;
                return;
            }
            if streams.contains_key(&open.stream_id) {
                drop(streams);
                self.send(refuse("duplicate stream id".to_string())).await;
                return;
            }
            streams.insert(
                open.stream_id,
                StreamHandle {
                    port: open.port,
                    inbox: inbox_tx,
                    recv_credit: recv_credit.clone(),
                },
            );
        }

        let mgr = self.clone();
        let stream_id = open.stream_id;
        let port = open.port;
        tokio::spawn(async move {
            let tcp = match tokio::time::timeout(DIAL_TIMEOUT, dial_loopback(port)).await {
                Ok(Ok(tcp)) => tcp,
                Ok(Err(e)) => {
                    mgr.remove_stream(stream_id).await;
                    mgr.send(ProxyToServer::TunnelRefused(TunnelRefusedFields {
                        stream_id,
                        error: e.to_string(),
                    }))
                    .await;
                    return;
                }
                Err(_) => {
                    mgr.remove_stream(stream_id).await;
                    mgr.send(ProxyToServer::TunnelRefused(TunnelRefusedFields {
                        stream_id,
                        error: "dial timed out".to_string(),
                    }))
                    .await;
                    return;
                }
            };
            mgr.send(ProxyToServer::TunnelOpened(TunnelStreamFields {
                stream_id,
            }))
            .await;
            debug!("Tunnel stream {} open to port {}", stream_id, port);
            run_stream(mgr, stream_id, tcp, inbox_rx, recv_credit).await;
        });
    }

    async fn remove_stream(&self, stream_id: Uuid) {
        self.streams.lock().await.remove(&stream_id);
    }

    async fn send(&self, msg: ProxyToServer) {
        let mut ws = self.ws.lock().await;
        if let Err(e) = ws.send(msg).await {
            debug!("Tunnel WS send failed (connection closing): {}", e);
        }
    }
}

/// The proxy only ever dials loopback — hard-coded, not configurable.
async fn dial_loopback(port: u16) -> std::io::Result<TcpStream> {
    TcpStream::connect(("127.0.0.1", port)).await
}

/// Name of the process listening on `port`, best effort. `listeners` scans
/// the OS socket tables (`/proc` on Linux, libproc on macOS) — a same-user
/// lookup that can take a few ms on a busy box, hence `spawn_blocking`.
/// `None` when the owner can't be resolved (other-user process, races, or
/// unsupported platform); the caller treats that as "listening, name
/// unknown".
async fn process_on_port(port: u16) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        listeners::get_process_by_port(port, listeners::Protocol::TCP)
            .ok()
            .map(|p| p.name)
    })
    .await
    .ok()
    .flatten()
}

/// Credit gate: `take` blocks while the window is empty, then consumes up to
/// `max` bytes of credit; `grant` refills (peer `TunnelWindow` or refund of
/// reserved-but-unread bytes).
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
            // Arm the waiter before checking, so a grant between the check
            // and the await can't be missed.
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
        // Saturate rather than wrap on absurd `TunnelWindow` values — the
        // window can't meaningfully exceed u32 anyway.
        let mut avail = self.avail.lock().await;
        *avail = avail.saturating_add(n);
        self.notify.notify_waiters();
    }
}

/// Copy loop for one open stream. Uplink (TCP→WS) runs as a child task gated
/// on send credit; the downlink (WS→TCP) runs here, granting window back as
/// bytes drain into the socket. Ends when either side closes; cleanup always
/// removes the handle and (best-effort) tells the backend.
async fn run_stream(
    mgr: Arc<TunnelManager>,
    stream_id: Uuid,
    tcp: TcpStream,
    mut inbox: mpsc::UnboundedReceiver<StreamMsg>,
    recv_credit: Arc<std::sync::atomic::AtomicI64>,
) {
    let (mut tcp_rd, mut tcp_wr) = tcp.into_split();
    let send_credit = Arc::new(CreditGate::new(INITIAL_WINDOW));

    let uplink_credit = send_credit.clone();
    let uplink_mgr = mgr.clone();
    let uplink = tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_CHUNK];
        loop {
            let budget = uplink_credit.take(MAX_CHUNK).await;
            let n = match tcp_rd.read(&mut buf[..budget]).await {
                Ok(0) => break None,
                Ok(n) => n,
                Err(e) => break Some(e.to_string()),
            };
            if n < budget {
                uplink_credit.grant((budget - n) as u32).await;
            }
            uplink_mgr
                .send(ProxyToServer::TunnelData(TunnelDataFields {
                    stream_id,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(&buf[..n]),
                }))
                .await;
        }
    });

    let close_reason: Option<String> = loop {
        match inbox.recv().await {
            Some(StreamMsg::Data(bytes)) => {
                if let Err(e) = tcp_wr.write_all(&bytes).await {
                    break Some(format!("local write failed: {e}"));
                }
                // Grant-on-drain: the bytes are in the socket, refill the
                // peer's window (and our receive-credit enforcement book).
                recv_credit.fetch_add(bytes.len() as i64, std::sync::atomic::Ordering::AcqRel);
                mgr.send(ProxyToServer::TunnelWindow(TunnelWindowFields {
                    stream_id,
                    add_bytes: bytes.len() as u32,
                }))
                .await;
            }
            Some(StreamMsg::Window(n)) => send_credit.grant(n).await,
            Some(StreamMsg::Close) | None => break None,
        }
    };

    // If the uplink already ended (TCP EOF/error) prefer its reason.
    let reason = if uplink.is_finished() {
        uplink.await.ok().flatten()
    } else {
        uplink.abort();
        close_reason
    };

    mgr.remove_stream(stream_id).await;
    mgr.send(ProxyToServer::TunnelClose(TunnelCloseFields {
        stream_id,
        reason,
    }))
    .await;
    debug!("Tunnel stream {} closed", stream_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The port→process resolver finds our own test binary when we bind a
    /// listener (same-user lookup, the case that matters in production).
    #[tokio::test]
    async fn process_on_port_resolves_own_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let name = process_on_port(port).await;
        assert!(
            name.as_deref().is_some_and(|n| !n.is_empty()),
            "expected to resolve our own listener, got {name:?}"
        );
    }

    /// A sender must block at zero credit and resume exactly when granted.
    #[tokio::test]
    async fn credit_gate_blocks_and_resumes() {
        let gate = Arc::new(CreditGate::new(10));
        assert_eq!(gate.take(4).await, 4);
        assert_eq!(gate.take(100).await, 6); // clamped to remaining

        // Window empty: take must not complete...
        let waiter = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.take(8).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(!waiter.is_finished(), "take completed with zero credit");

        // ...until a grant arrives.
        gate.grant(3).await;
        assert_eq!(waiter.await.unwrap(), 3);
    }

    /// A grant issued immediately before the waiter arms must not be lost.
    #[tokio::test]
    async fn credit_gate_grant_before_take_is_not_missed() {
        let gate = CreditGate::new(0);
        gate.grant(5).await;
        assert_eq!(gate.take(16).await, 5);
    }

    /// Absurd window grants saturate instead of wrapping to a tiny window.
    #[tokio::test]
    async fn credit_gate_grant_saturates() {
        let gate = CreditGate::new(u32::MAX - 1);
        gate.grant(u32::MAX).await;
        assert_eq!(gate.take(4).await, 4);
        gate.grant(u32::MAX).await; // still sane after saturation
        assert_eq!(gate.take(4).await, 4);
    }
}
