//! Proxy-side tunnel transport for port forwarding (docs/PORT_FORWARDING.md).
//!
//! A [`TunnelManager`] lives for the duration of one session-WebSocket
//! connection. It keeps the `ForwardOpen`-synced port allowlist, answers
//! probe dials with `ForwardStatus`, and runs one task per open stream
//! copying bytes between the backend (WS frames) and `127.0.0.1:{port}`.
//!
//! # Two transports
//!
//! Stream bytes travel on whichever socket a stream was opened on (#1506):
//!
//! - **Binary data plane** (preferred) — a second WebSocket carrying raw
//!   payloads. Attach it with [`run_data_plane`]; stream frames then bypass the
//!   control socket entirely, so forward traffic no longer competes with agent
//!   stdio and heartbeats, and payloads skip base64.
//! - **Control socket** (fallback) — the original JSON `Tunnel*` frames with
//!   base64 payloads. Used whenever no data plane is attached, which keeps older
//!   backends and dropped data sockets working unchanged.
//!
//! The forward **allowlist** and health reports always stay on the control
//! socket: they are low-volume policy, and they already have a replay path
//! there. That split is what makes [`await_allowlist`] necessary — see its docs
//! for the cross-socket ordering race it absorbs.
//!
//! Backpressure has two layers, per the spec:
//! - **Stream credit**: each direction starts with an [`INITIAL_WINDOW`] credit;
//!   the receiver re-grants as it drains bytes into the underlying socket
//!   (`TunnelWindow`). A sender never reads more from TCP than it holds
//!   credit for.
//! - **Writer capacity**: outgoing frames go straight through a shared
//!   `WsSender` mutex (FIFO), one ≤[`MAX_CHUNK`] frame per lock. There is no
//!   queue to grow — buffered tunnel data is bounded by streams × `MAX_CHUNK` and
//!   waiting streams are served round-robin by mutex order. On the control
//!   socket that mutex is shared with session frames, so tunnel traffic can
//!   delay them (the reason the data plane exists); on the data plane it is
//!   private, so it cannot.
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
    TunnelFrame, TunnelOpenFields, TunnelRefuseReason, TunnelRefusedFields, TunnelStreamFields,
    TunnelWindowFields,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Shared write half of the session WebSocket (same shape as the hosts'
/// `SharedWsWrite` aliases).
pub type TunnelWsWrite = Arc<Mutex<ws_bridge::WsSender<ProxyToServer>>>;

/// Shared write half of the dedicated binary data plane (#1506).
pub type TunnelDataWrite = Arc<Mutex<ws_bridge::WsSender<TunnelFrame>>>;

/// Which socket a stream's outbound frames travel on.
///
/// Decided once, from where the stream's `TunnelOpen` arrived, and then carried
/// by value through `open_stream_with_egress` → `run_stream` so each stream task
/// owns its own copy. Deliberately *not* looked up per frame from manager state:
/// if the data plane attached midway through a stream, a per-frame lookup would
/// split that stream's bytes across two sockets, and the two sockets have no
/// ordering relationship — the payload could arrive reordered. Passing it by
/// value makes "one stream, one ordered channel" structural.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamEgress {
    /// Legacy: JSON frames with base64 payloads over the control socket.
    Control,
    /// Dedicated data plane: binary frames, raw payloads.
    Binary,
}

/// Max decoded bytes per `TunnelData` frame.
pub const MAX_CHUNK: usize = 16 * 1024;
/// Initial per-stream, per-direction flow-control window.
///
/// Kept small so the worst-case buffered bytes
/// (`MAX_STREAMS × INITIAL_WINDOW` per direction) stay bounded even with a high
/// stream cap; loopback/WS latency is low enough that this saturates a single
/// stream easily.
///
/// **Both ends must agree** (`backend::…::tunnel_client::INITIAL_WINDOW`): the
/// sender's credit gate and the receiver's credit book are seeded from this same
/// value, so a mismatch makes the sender exceed the receiver's book and every
/// stream closes as "beyond granted window". Raising it therefore needs a
/// negotiated protocol bump rather than an edit here.
pub const INITIAL_WINDOW: u32 = 64 * 1024;
/// Max concurrent streams per session connection.
///
/// Purely proxy-local: the backend has no cap of its own, it just receives a
/// [`TunnelRefuseReason::StreamLimit`] refusal. That makes this the one sizing
/// knob that needs no cross-version agreement, so it is safe to raise on its own
/// (unlike `MAX_CHUNK`/`INITIAL_WINDOW`, which both ends must match — see their
/// docs).
///
/// Every request becomes its own stream because upstream connections are not yet
/// pooled (#1468), so a resource-heavy page or a polling app can burst through
/// hundreds at once; past the cap requests fail with `at-capacity`. 512 doubles
/// the previous headroom while keeping the worst case bounded at
/// `MAX_STREAMS × INITIAL_WINDOW` = 32 MiB per direction — and that is the
/// pathological all-streams-saturated figure, not steady state. Pooling is the
/// real fix for the cap; this buys room until it lands.
pub const MAX_STREAMS: usize = 512;
/// How long a single dial to loopback may take before it is treated as a
/// timeout (service hung, not down).
const DIAL_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a *browser-stream* dial keeps retrying a *refused* loopback before
/// giving up. A refusal usually just means the local service is momentarily
/// down — mid-restart, or finishing a build — so [`connect_loopback`] backs
/// off and retries within this budget instead of instantly reporting the port
/// dead. A dial that *times out* is never retried (repeating a multi-second
/// hang would only stall the browser).
const STREAM_DIAL_RETRY_BUDGET: Duration = Duration::from_secs(8);
/// Retry budget for the background health probe and the registration probe.
/// Short: a small grace absorbs a momentary refusal (so the chip doesn't flap
/// red on a blip) without the probe lingering for seconds on a genuinely dead
/// port. Real traffic outcomes correct the chip faster than a probe anyway.
const PROBE_DIAL_BUDGET: Duration = Duration::from_millis(1500);
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
    /// is what bounds per-stream buffered downlink data to [`INITIAL_WINDOW`]
    /// even against a buggy or hostile peer.
    recv_credit: Arc<std::sync::atomic::AtomicI64>,
}

/// Per-connection tunnel state. Create one per established session WS,
/// dispatch forward/tunnel frames into [`TunnelManager::handle`], and call
/// [`TunnelManager::shutdown`] when the connection ends.
pub struct TunnelManager {
    ws: TunnelWsWrite,
    /// Write half of the dedicated binary data plane, once connected (#1506).
    ///
    /// `None` means every stream uses the control socket, which is exactly the
    /// pre-existing behavior — so an unavailable or dropped data plane degrades
    /// instead of failing. Only *stream* frames move here; the forward allowlist
    /// and health reports stay on the control socket.
    data: Mutex<Option<TunnelDataWrite>>,
    allowed: Mutex<HashSet<u16>>,
    streams: Mutex<HashMap<Uuid, StreamHandle>>,
    /// Last reported `listening` verdict per allowlisted port; the background
    /// prober reports a `ForwardStatus` only when this flips (the
    /// registration-time probe seeds it), so steady state costs no frames.
    /// Process name is display-only and rides the frame but does not trigger a
    /// report on its own — that name flickering (best-effort resolution) was a
    /// source of spurious churn.
    last_health: Mutex<HashMap<u16, bool>>,
    prober: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl TunnelManager {
    pub fn new(ws: TunnelWsWrite) -> Arc<Self> {
        let mgr = Arc::new(Self {
            ws,
            data: Mutex::new(None),
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
            let (listening, error) = match connect_loopback(port, PROBE_DIAL_BUDGET).await {
                Ok(_) => (true, None),
                Err(e) => (false, Some(e.to_string())),
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
            let changed = {
                let mut health = self.last_health.lock().await;
                health.insert(port, listening) != Some(listening)
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
                    let (listening, error) = match connect_loopback(port, PROBE_DIAL_BUDGET).await {
                        Ok(_) => (true, None),
                        Err(e) => (false, Some(e.to_string())),
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
                    mgr.last_health.lock().await.insert(port, listening);
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
        self.open_stream_with_egress(open, StreamEgress::Control)
            .await;
    }

    async fn open_stream_with_egress(
        self: &Arc<Self>,
        open: &TunnelOpenFields,
        egress: StreamEgress,
    ) {
        if !self.allowed.lock().await.contains(&open.port) {
            // On the data plane, absorb the allowlist-sync race before refusing.
            let synced = egress == StreamEgress::Binary
                && await_allowlist(&self.allowed, open.port, ALLOWLIST_SYNC_GRACE).await;
            if !synced {
                self.send_stream_refused(egress, open.stream_id, TunnelRefuseReason::NotForwarded)
                    .await;
                return;
            }
            debug!(
                "Port {} appeared in the allowlist after the stream open (cross-socket sync race)",
                open.port
            );
        }
        // Register the inbox before the dial so ordered frames can't miss it.
        let (inbox_tx, inbox_rx) = mpsc::unbounded_channel();
        let recv_credit = Arc::new(std::sync::atomic::AtomicI64::new(INITIAL_WINDOW as i64));
        {
            let mut streams = self.streams.lock().await;
            if streams.len() >= MAX_STREAMS {
                drop(streams);
                warn!(
                    "Tunnel stream limit ({}) reached; refusing stream {}",
                    MAX_STREAMS, open.stream_id
                );
                self.send_stream_refused(egress, open.stream_id, TunnelRefuseReason::StreamLimit)
                    .await;
                return;
            }
            if streams.contains_key(&open.stream_id) {
                drop(streams);
                self.send_stream_refused(egress, open.stream_id, TunnelRefuseReason::Protocol)
                    .await;
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
            let tcp = match connect_loopback(port, STREAM_DIAL_RETRY_BUDGET).await {
                Ok(tcp) => tcp,
                Err(e) => {
                    mgr.remove_stream(stream_id).await;
                    warn!("Tunnel dial to port {} refused: {}", port, e);
                    mgr.send_stream_refused(egress, stream_id, TunnelRefuseReason::NoListener)
                        .await;
                    return;
                }
            };
            mgr.send_stream_opened(egress, stream_id).await;
            debug!("Tunnel stream {} open to port {}", stream_id, port);
            run_stream(mgr, stream_id, egress, tcp, inbox_rx, recv_credit).await;
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

    /// Adopt a connected data plane. Streams opened from here on ride it;
    /// streams already running keep the transport they opened with.
    pub async fn attach_data_plane(&self, data: TunnelDataWrite) {
        *self.data.lock().await = Some(data);
        info!("Port-forward data plane attached (binary transport active)");
    }

    /// Forget the data plane (its socket ended). New streams fall back to the
    /// control socket. Streams still pinned to `Binary` will fail their next
    /// send and close, which is correct — their transport is gone.
    pub async fn detach_data_plane(&self) {
        if self.data.lock().await.take().is_some() {
            info!("Port-forward data plane detached (falling back to control socket)");
        }
    }

    /// Whether a data plane is currently attached.
    pub async fn has_data_plane(&self) -> bool {
        self.data.lock().await.is_some()
    }

    /// Send one binary frame on the data plane. Returns `false` if there is no
    /// data plane or the send failed.
    async fn send_binary(&self, frame: TunnelFrame) -> bool {
        let mut guard = self.data.lock().await;
        let Some(data) = guard.as_mut() else {
            return false;
        };
        let mut ws = data.lock().await;
        match ws.send(frame).await {
            Ok(()) => true,
            Err(e) => {
                debug!("Data-plane send failed (connection closing): {}", e);
                false
            }
        }
    }

    /// Emit a stream payload chunk on the stream's pinned transport.
    async fn send_stream_data(&self, egress: StreamEgress, stream_id: Uuid, bytes: &[u8]) {
        match egress {
            StreamEgress::Binary => {
                self.send_binary(TunnelFrame::Data {
                    stream_id,
                    bytes: bytes.to_vec(),
                })
                .await;
            }
            StreamEgress::Control => {
                self.send(ProxyToServer::TunnelData(TunnelDataFields {
                    stream_id,
                    data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
                }))
                .await;
            }
        }
    }

    /// Grant the backend more send credit on the stream's pinned transport.
    async fn send_stream_window(&self, egress: StreamEgress, stream_id: Uuid, add_bytes: u32) {
        match egress {
            StreamEgress::Binary => {
                self.send_binary(TunnelFrame::Window {
                    stream_id,
                    add_bytes,
                })
                .await;
            }
            StreamEgress::Control => {
                self.send(ProxyToServer::TunnelWindow(TunnelWindowFields {
                    stream_id,
                    add_bytes,
                }))
                .await;
            }
        }
    }

    async fn send_stream_opened(&self, egress: StreamEgress, stream_id: Uuid) {
        match egress {
            StreamEgress::Binary => {
                self.send_binary(TunnelFrame::Opened { stream_id }).await;
            }
            StreamEgress::Control => {
                self.send(ProxyToServer::TunnelOpened(TunnelStreamFields {
                    stream_id,
                }))
                .await;
            }
        }
    }

    async fn send_stream_refused(
        &self,
        egress: StreamEgress,
        stream_id: Uuid,
        reason: TunnelRefuseReason,
    ) {
        match egress {
            StreamEgress::Binary => {
                self.send_binary(TunnelFrame::Refused { stream_id, reason })
                    .await;
            }
            StreamEgress::Control => {
                self.send(ProxyToServer::TunnelRefused(TunnelRefusedFields {
                    stream_id,
                    reason,
                }))
                .await;
            }
        }
    }

    async fn send_stream_close(
        &self,
        egress: StreamEgress,
        stream_id: Uuid,
        reason: Option<String>,
    ) {
        match egress {
            StreamEgress::Binary => {
                self.send_binary(TunnelFrame::Close { stream_id, reason })
                    .await;
            }
            StreamEgress::Control => {
                self.send(ProxyToServer::TunnelClose(TunnelCloseFields {
                    stream_id,
                    reason,
                }))
                .await;
            }
        }
    }

    /// Handle one inbound frame from the data plane.
    ///
    /// The mirror of [`Self::handle`] for the binary transport. Only
    /// stream-scoped frames arrive here; the allowlist still syncs over the
    /// control socket, so `ForwardOpen`/`ForwardClose` are not part of this
    /// dispatch. Returns `false` if the frame was protocol misuse and the data
    /// socket should be closed.
    pub async fn handle_data_frame(self: &Arc<Self>, frame: TunnelFrame) -> bool {
        match frame {
            TunnelFrame::Open { stream_id, port } => {
                self.open_stream_with_egress(
                    &TunnelOpenFields { stream_id, port },
                    StreamEgress::Binary,
                )
                .await;
            }
            TunnelFrame::Data { stream_id, bytes } => {
                let handle = {
                    let streams = self.streams.lock().await;
                    streams
                        .get(&stream_id)
                        .map(|h| (h.inbox.clone(), h.recv_credit.clone()))
                };
                // Unknown stream: a post-close race; drop silently.
                if let Some((inbox, recv_credit)) = handle {
                    if bytes.len() > MAX_CHUNK {
                        warn!(
                            "Oversized data-plane payload ({} bytes) for stream {}; closing",
                            bytes.len(),
                            stream_id
                        );
                        let _ = inbox.send(StreamMsg::Close);
                    } else {
                        // Same credit enforcement as the control path: the
                        // unbounded inbox must not absorb beyond the window.
                        let prev = recv_credit
                            .fetch_sub(bytes.len() as i64, std::sync::atomic::Ordering::AcqRel);
                        if prev < bytes.len() as i64 {
                            warn!(
                                "Data-plane payload beyond granted window for stream {}; closing",
                                stream_id
                            );
                            let _ = inbox.send(StreamMsg::Close);
                        } else {
                            let _ = inbox.send(StreamMsg::Data(bytes));
                        }
                    }
                }
            }
            TunnelFrame::Window {
                stream_id,
                add_bytes,
            } => {
                let streams = self.streams.lock().await;
                if let Some(handle) = streams.get(&stream_id) {
                    let _ = handle.inbox.send(StreamMsg::Window(add_bytes));
                }
            }
            TunnelFrame::Close { stream_id, .. } => {
                let streams = self.streams.lock().await;
                if let Some(handle) = streams.get(&stream_id) {
                    let _ = handle.inbox.send(StreamMsg::Close);
                }
            }
            // Proxy→server only; the backend sending these is confused.
            TunnelFrame::Opened { .. }
            | TunnelFrame::Refused { .. }
            | TunnelFrame::Hello { .. } => {
                warn!("Data plane received a client-only frame from the backend; closing");
                return false;
            }
        }
        true
    }
}

/// Connect the dedicated binary data plane and pump it until it ends (#1506).
///
/// Dials [`TunnelDataEndpoint`], sends the `Hello` ticket, attaches the write
/// half to `mgr`, then forwards inbound frames into
/// [`TunnelManager::handle_data_frame`]. Detaches on exit so subsequent streams
/// fall back to the control socket.
///
/// Every failure here is non-fatal by design: the data plane is an optimization,
/// so a dial error, a rejected ticket, or a mid-session drop leaves the session
/// untouched and tunneling on the control socket. Spawn it and forget it — do
/// not gate session startup on it.
pub async fn run_data_plane(mgr: Arc<TunnelManager>, backend_url: String, ticket: String) {
    let conn =
        match ws_bridge::native_client::connect::<shared::TunnelDataEndpoint>(&backend_url).await {
            Ok(conn) => conn,
            Err(e) => {
                warn!(
                    "Port-forward data plane unavailable ({}); tunneling over the control socket",
                    e
                );
                return;
            }
        };
    let (mut write, mut read) = conn.split();

    // The ticket must be the first frame; the backend routes nothing until it
    // verifies.
    if let Err(e) = write.send(TunnelFrame::Hello { ticket }).await {
        warn!("Failed to send data-plane Hello: {}", e);
        return;
    }

    mgr.attach_data_plane(Arc::new(Mutex::new(write))).await;

    while let Some(result) = read.recv().await {
        match result {
            Ok(frame) => {
                if !mgr.handle_data_frame(frame).await {
                    break;
                }
            }
            Err(e) => {
                debug!("Data-plane read ended: {}", e);
                break;
            }
        }
    }

    mgr.detach_data_plane().await;
}

/// Grace period for the forward-allowlist sync race (see [`await_allowlist`]).
const ALLOWLIST_SYNC_GRACE: Duration = Duration::from_millis(750);

/// Wait up to `grace` for `port` to appear in the forward allowlist.
///
/// Exists only because of the cross-socket race the data plane introduced: the
/// allowlist syncs via `ForwardOpen` on the **control** socket while stream opens
/// arrive on the **data** socket, and the two sockets have no ordering
/// relationship. A `TunnelOpen` can therefore beat its port's `ForwardOpen` and
/// be refused as `NotForwarded` even though the forward is perfectly valid. (On
/// the control socket the two frames were ordered, so this could not happen.)
///
/// A revoked port simply is not in the allowlist and never will be, so the wait
/// costs a bounded delay only on the genuinely-not-forwarded path — much cheaper
/// than a spurious error page on a live forward.
///
/// Takes the allowlist directly rather than `&self` so the timing behavior is
/// testable without a live WebSocket.
async fn await_allowlist(allowed: &Mutex<HashSet<u16>>, port: u16, grace: Duration) -> bool {
    const POLL: Duration = Duration::from_millis(25);
    let deadline = Instant::now() + grace;
    loop {
        if allowed.lock().await.contains(&port) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Dial loopback (`127.0.0.1:{port}` — hard-coded, not configurable), the one
/// dial used by *both* real browser streams and the health probe so the two
/// can never disagree on what "up" means. Each attempt is bounded by
/// `DIAL_TIMEOUT`; a *refused* dial backs off and retries until `budget`
/// elapses (the local service is likely mid-restart), while a dial that *times
/// out* (hung, not down) fails immediately without retry. A zero `budget`
/// means a single attempt.
async fn connect_loopback(port: u16, budget: Duration) -> std::io::Result<TcpStream> {
    let deadline = Instant::now() + budget;
    let mut backoff = Duration::from_millis(100);
    loop {
        match tokio::time::timeout(DIAL_TIMEOUT, TcpStream::connect(("127.0.0.1", port))).await {
            Ok(Ok(tcp)) => return Ok(tcp),
            Ok(Err(e)) => {
                if Instant::now() + backoff >= deadline {
                    return Err(e);
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_millis(500));
            }
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "dial timed out",
                ));
            }
        }
    }
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
    egress: StreamEgress,
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
                .send_stream_data(egress, stream_id, &buf[..n])
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
                mgr.send_stream_window(egress, stream_id, bytes.len() as u32)
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
    mgr.send_stream_close(egress, stream_id, reason).await;
    debug!("Tunnel stream {} closed", stream_id);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist(ports: &[u16]) -> Mutex<HashSet<u16>> {
        Mutex::new(ports.iter().copied().collect())
    }

    /// An already-synced port returns immediately, so the common path pays
    /// nothing for the race handling.
    #[tokio::test]
    async fn await_allowlist_is_immediate_when_already_synced() {
        let allowed = allowlist(&[8080]);
        let started = Instant::now();
        assert!(await_allowlist(&allowed, 8080, ALLOWLIST_SYNC_GRACE).await);
        assert!(started.elapsed() < Duration::from_millis(20));
    }

    /// The race this exists for: the port lands (a `ForwardOpen` arrives on the
    /// control socket) only after the stream open was already being handled.
    #[tokio::test]
    async fn await_allowlist_returns_once_the_port_syncs() {
        let allowed = Arc::new(allowlist(&[]));
        let waiter = {
            let allowed = allowed.clone();
            tokio::spawn(async move { await_allowlist(&allowed, 4321, ALLOWLIST_SYNC_GRACE).await })
        };
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(!waiter.is_finished(), "should still be waiting");

        allowed.lock().await.insert(4321);
        assert!(waiter.await.unwrap(), "should observe the synced port");
    }

    /// A genuinely un-forwarded port is still refused — the grace is bounded,
    /// not an indefinite wait.
    #[tokio::test]
    async fn await_allowlist_gives_up_on_a_port_that_never_syncs() {
        let allowed = allowlist(&[]);
        let grace = Duration::from_millis(100);
        let started = Instant::now();
        assert!(!await_allowlist(&allowed, 9999, grace).await);
        assert!(started.elapsed() >= grace, "must honor the full grace");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "must stay bounded, took {:?}",
            started.elapsed()
        );
    }

    /// A browser-stream dial connects immediately when the service is up.
    #[tokio::test]
    async fn stream_dial_connects_when_service_is_up() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let stream = connect_loopback(port, STREAM_DIAL_RETRY_BUDGET).await;
        assert!(stream.is_ok(), "expected immediate connect, got {stream:?}");
        let _ = accept.await;
    }

    /// The dial retries across a brief outage: the listener comes up only after
    /// the first attempts would have been refused, and the dial still connects
    /// within the retry budget. This is the "backend mid-restart" case that
    /// otherwise surfaces as "nothing listening on the forwarded port".
    #[tokio::test]
    async fn stream_dial_retries_until_listener_is_up() {
        // Grab a free port, then release it so the first dials are refused.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        // Bring the listener up only after a delay that outlasts the first
        // couple of retry backoffs.
        let listener_task = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
                .await
                .unwrap();
            let _ = listener.accept().await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        });

        let stream = connect_loopback(port, STREAM_DIAL_RETRY_BUDGET).await;
        assert!(
            stream.is_ok(),
            "expected the retry to connect once the listener came up, got {stream:?}"
        );
        let _ = listener_task.await;
    }
}
