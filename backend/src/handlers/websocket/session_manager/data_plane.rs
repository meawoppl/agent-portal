//! Registry of dedicated port-forward data-plane sockets (#1506).
//!
//! Each entry is the binary `/ws/session/data` socket belonging to *one*
//! control connection, keyed by session key and tagged with that connection's
//! generation. Every mutation is generation-guarded for the same reason
//! [`proxy_lifecycle`](super::proxy_lifecycle) is: a proxy reconnect races its
//! own teardown, and a stale socket must never evict — or be used by — the
//! connection that replaced it.
//!
//! The registry is deliberately *optional* state. `open_tunnel` consults it per
//! call and silently uses the JSON-over-control-socket path when there is no
//! live entry, so an older proxy, a proxy whose data socket dropped, or a
//! data socket that hasn't finished connecting yet all degrade to the previous
//! behavior instead of failing.

use dashmap::DashMap;
use shared::TunnelFrame;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use super::{ConnSender, SessionId, SessionManager};

/// Bounded outbound channel for a data-plane socket.
///
/// Sized well above the control socket's budget: this carries bulk stream bytes
/// (one frame per chunk per stream), which is exactly the traffic that used to
/// contend with heartbeats on the control socket. Overflow is still treated as
/// a dead connection — the tunnel's per-stream credit windows are what bound
/// in-flight bytes, so a peer that cannot drain this much is wedged.
pub const DATA_PLANE_CHANNEL_CAPACITY: usize = 2048;

pub type DataPlaneSender = ConnSender<TunnelFrame>;

/// One registered data-plane socket.
pub struct DataPlaneConnection {
    pub sender: DataPlaneSender,
    /// Generation of the *control* connection this socket serves. Frames are
    /// only routed here while this matches the live control connection.
    pub gen: u64,
    /// Tunnel sizing negotiated for this connection (#1511); streams opened over
    /// this data plane are configured to it.
    pub sizing: shared::TunnelSizing,
    /// Fired to force this socket's task to close (mirrors `ProxyConnection`).
    pub cancel: CancellationToken,
    /// Epoch seconds of the last inbound frame, for diagnostics.
    pub last_seen: AtomicU64,
}

impl SessionManager {
    /// Register a data-plane socket for `(session_key, gen)`.
    ///
    /// Returns `false` — and registers nothing — when `gen` is not the live
    /// control connection's generation. That rejects both a ticket replayed
    /// from a superseded connection and a data socket that finished connecting
    /// after its control socket had already been replaced.
    pub fn register_data_plane(
        &self,
        session_key: SessionId,
        gen: u64,
        sizing: shared::TunnelSizing,
        sender: DataPlaneSender,
        cancel: CancellationToken,
    ) -> bool {
        // NOTE: deliberately *not* `is_current_connection` — that returns true
        // for a session with no registered proxy at all (it answers "has this
        // been superseded?", which teardown wants). As an authorization guard we
        // need positive proof of a live control connection at this exact
        // generation, or a ticket replayed after the proxy disconnected would
        // attach a data plane to nothing.
        if self.current_connection_gen(&session_key) != Some(gen) {
            debug!(
                "Rejecting data-plane socket for {} (gen={}): no live control connection at that generation",
                session_key, gen
            );
            return false;
        }
        // Displace any existing entry for this session; its task observes the
        // cancel and exits. Two data sockets for one connection is not a
        // supported shape, and keeping the newer one matches how the control
        // socket handles a re-register.
        if let Some(prev) = self.data_planes.insert(
            session_key.clone(),
            DataPlaneConnection {
                sender,
                gen,
                sizing,
                cancel,
                last_seen: AtomicU64::new(super::liveness::epoch_secs()),
            },
        ) {
            prev.cancel.cancel();
        }
        info!(
            "Registered binary data plane for session {} (gen={})",
            session_key, gen
        );
        true
    }

    /// Remove a data-plane socket, but only if it is still the registered one
    /// for `gen` — so a stale socket's teardown cannot evict its successor.
    pub fn unregister_data_plane(&self, session_key: &str, gen: u64) {
        if self
            .data_planes
            .remove_if(session_key, |_, conn| conn.gen == gen)
            .is_some()
        {
            debug!(
                "Unregistered data plane for session {} (gen={})",
                session_key, gen
            );
        }
    }

    /// Drop the data-plane socket belonging to a control connection that is
    /// going away, closing its transport. Called from the control socket's
    /// teardown: the data plane exists only to serve that connection, so
    /// leaving it registered would let a later `open_tunnel` route bytes into
    /// a socket whose session is gone.
    pub fn close_data_plane_for_connection(&self, session_key: &str, gen: u64) {
        if let Some((_, conn)) = self
            .data_planes
            .remove_if(session_key, |_, conn| conn.gen == gen)
        {
            conn.cancel.cancel();
            debug!(
                "Closed data plane for ended connection {} (gen={})",
                session_key, gen
            );
        }
    }

    /// Sender and negotiated sizing for the live data plane of
    /// `(session_key, gen)`, if one is registered. `None` means "use the
    /// control-socket JSON path". The sizing is returned alongside the sender so
    /// `open_tunnel` configures the stream from the same lookup that chose the
    /// transport, with no second map access to race.
    pub fn data_plane_egress(
        &self,
        session_key: &str,
        gen: u64,
    ) -> Option<(DataPlaneSender, shared::TunnelSizing)> {
        let conn = self.data_planes.get(session_key)?;
        (conn.gen == gen).then(|| (conn.sender.clone(), conn.sizing))
    }

    /// Stamp liveness for a data-plane socket (diagnostics only — the control
    /// socket's heartbeat remains the authority on session liveness, which is
    /// the entire point of separating the planes).
    pub fn touch_data_plane(&self, session_key: &str) {
        if let Some(conn) = self.data_planes.get(session_key) {
            conn.last_seen.store(
                super::liveness::epoch_secs(),
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }

    /// Whether a live data plane is registered for `(session_key, gen)`.
    pub fn has_data_plane(&self, session_key: &str, gen: u64) -> bool {
        self.data_planes
            .get(session_key)
            .is_some_and(|c| c.gen == gen)
    }
}

/// Shared map type for the registry.
pub(super) type DataPlaneMap = Arc<DashMap<SessionId, DataPlaneConnection>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::websocket::{conn_channel, SessionManager};

    fn register_control(mgr: &SessionManager, key: &str) -> u64 {
        let (tx, rx) = conn_channel::<shared::ServerToProxy>(8);
        // Keep the receiver alive for the test's duration.
        std::mem::forget(rx);
        mgr.register_session(
            SessionId::new(key.to_string()),
            tx,
            CancellationToken::new(),
        )
    }

    fn data_sender() -> (DataPlaneSender, tokio::sync::mpsc::Receiver<TunnelFrame>) {
        conn_channel::<TunnelFrame>(8)
    }

    #[test]
    fn registers_only_for_the_live_control_generation() {
        let mgr = SessionManager::new();
        let gen = register_control(&mgr, "s1");

        let (tx, _rx) = data_sender();
        assert!(mgr.register_data_plane(
            "s1".into(),
            gen,
            shared::TunnelSizing::V1,
            tx,
            CancellationToken::new()
        ));
        assert!(mgr.has_data_plane("s1", gen));

        // A stale generation is refused outright.
        let (tx, _rx) = data_sender();
        assert!(!mgr.register_data_plane(
            "s1".into(),
            gen - 1,
            shared::TunnelSizing::V1,
            tx,
            CancellationToken::new()
        ));
    }

    #[test]
    fn rejects_data_plane_for_unknown_session() {
        let mgr = SessionManager::new();
        let (tx, _rx) = data_sender();
        assert!(!mgr.register_data_plane(
            "nope".into(),
            1,
            shared::TunnelSizing::V1,
            tx,
            CancellationToken::new()
        ));
        assert!(!mgr.has_data_plane("nope", 1));
    }

    /// A ticket minted on a superseded connection must not attach a data plane
    /// to the connection that replaced it.
    #[test]
    fn a_replayed_ticket_from_a_previous_connection_is_refused() {
        let mgr = SessionManager::new();
        let old_gen = register_control(&mgr, "s1");
        let new_gen = register_control(&mgr, "s1");
        assert_ne!(old_gen, new_gen);

        let (tx, _rx) = data_sender();
        assert!(!mgr.register_data_plane(
            "s1".into(),
            old_gen,
            shared::TunnelSizing::V1,
            tx,
            CancellationToken::new()
        ));

        let (tx, _rx) = data_sender();
        assert!(mgr.register_data_plane(
            "s1".into(),
            new_gen,
            shared::TunnelSizing::V1,
            tx,
            CancellationToken::new()
        ));
        assert!(mgr.has_data_plane("s1", new_gen));
        assert!(!mgr.has_data_plane("s1", old_gen));
    }

    #[test]
    fn stale_unregister_cannot_evict_the_successor() {
        let mgr = SessionManager::new();
        let gen = register_control(&mgr, "s1");
        let (tx, _rx) = data_sender();
        mgr.register_data_plane(
            "s1".into(),
            gen,
            shared::TunnelSizing::V1,
            tx,
            CancellationToken::new(),
        );

        // A late teardown from an older generation is a no-op.
        mgr.unregister_data_plane("s1", gen - 1);
        assert!(mgr.has_data_plane("s1", gen));

        mgr.unregister_data_plane("s1", gen);
        assert!(!mgr.has_data_plane("s1", gen));
    }

    #[test]
    fn re_registering_displaces_and_cancels_the_previous_socket() {
        let mgr = SessionManager::new();
        let gen = register_control(&mgr, "s1");

        let first_cancel = CancellationToken::new();
        let (tx, _rx) = data_sender();
        mgr.register_data_plane(
            "s1".into(),
            gen,
            shared::TunnelSizing::V1,
            tx,
            first_cancel.clone(),
        );

        let (tx, _rx) = data_sender();
        mgr.register_data_plane(
            "s1".into(),
            gen,
            shared::TunnelSizing::V1,
            tx,
            CancellationToken::new(),
        );

        assert!(
            first_cancel.is_cancelled(),
            "the displaced socket must be told to close"
        );
        assert!(mgr.has_data_plane("s1", gen));
    }

    #[test]
    fn sender_lookup_is_generation_scoped() {
        let mgr = SessionManager::new();
        let gen = register_control(&mgr, "s1");
        let (tx, _rx) = data_sender();
        mgr.register_data_plane(
            "s1".into(),
            gen,
            shared::TunnelSizing::V1,
            tx,
            CancellationToken::new(),
        );

        assert!(mgr.data_plane_egress("s1", gen).is_some());
        // Falling back to the control path is the correct answer for any other
        // generation, and for a session with no data plane at all.
        assert!(mgr.data_plane_egress("s1", gen + 1).is_none());
        assert!(mgr.data_plane_egress("other", gen).is_none());
    }

    #[test]
    fn closing_a_connection_drops_and_cancels_its_data_plane() {
        let mgr = SessionManager::new();
        let gen = register_control(&mgr, "s1");
        let cancel = CancellationToken::new();
        let (tx, _rx) = data_sender();
        mgr.register_data_plane(
            "s1".into(),
            gen,
            shared::TunnelSizing::V1,
            tx,
            cancel.clone(),
        );

        mgr.close_data_plane_for_connection("s1", gen);

        assert!(!mgr.has_data_plane("s1", gen));
        assert!(cancel.is_cancelled());
    }

    /// A control connection ending must not take down the data plane of a
    /// *newer* connection for the same session.
    #[test]
    fn closing_a_stale_connection_spares_the_current_data_plane() {
        let mgr = SessionManager::new();
        let old_gen = register_control(&mgr, "s1");
        let new_gen = register_control(&mgr, "s1");

        let cancel = CancellationToken::new();
        let (tx, _rx) = data_sender();
        mgr.register_data_plane(
            "s1".into(),
            new_gen,
            shared::TunnelSizing::V1,
            tx,
            cancel.clone(),
        );

        mgr.close_data_plane_for_connection("s1", old_gen);

        assert!(mgr.has_data_plane("s1", new_gen));
        assert!(!cancel.is_cancelled());
    }
}
