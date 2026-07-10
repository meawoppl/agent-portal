//! Server-side liveness for proxy and launcher connections (#1256).
//!
//! Disconnect detection is otherwise purely transport-driven: a connection
//! is torn down only when its socket task observes the WS stream ending. A
//! half-open TCP connection (peer gone, FIN never delivered — observed in
//! production as an inbound-alive/outbound-dead socket) never triggers
//! that, so its registry entry lingers and outbound delivery wedges until
//! someone restarts a daemon.
//!
//! Both connection types already send application-level heartbeats (proxy:
//! every 1s; launcher: every 30s). The socket tasks stamp `last_seen` on
//! every inbound frame; the sweeper spawned from `lib.rs` evicts and
//! force-closes any connection silent past its deadline. The client's
//! reconnect logic — which is reliable — does the rest.

use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::warn;
use uuid::Uuid;

use super::{SessionId, SessionManager};

/// A proxy heartbeats every 1s; a minute of silence means the transport is
/// gone (or so congested that a reconnect is the right outcome anyway).
pub const PROXY_LIVENESS_DEADLINE_SECS: u64 = 60;

/// A launcher heartbeats every 30s; three missed beats.
pub const LAUNCHER_LIVENESS_DEADLINE_SECS: u64 = 90;

/// How often the sweeper runs.
pub const LIVENESS_SWEEP_INTERVAL_SECS: u64 = 30;

pub(super) fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl SessionManager {
    /// Record inbound activity from a proxy connection.
    pub fn touch_session(&self, session_key: &SessionId) {
        if let Some(conn) = self.sessions.get(session_key) {
            conn.last_seen.store(epoch_secs(), Ordering::Relaxed);
        }
    }

    /// Record inbound activity from a launcher connection.
    pub fn touch_launcher(&self, launcher_id: &Uuid) {
        if let Some(conn) = self.launchers.get(launcher_id) {
            conn.last_seen.store(epoch_secs(), Ordering::Relaxed);
        }
    }

    /// Evict and force-close every connection that has been silent past its
    /// deadline. Returns `(proxies_evicted, launchers_evicted)`.
    pub fn sweep_stale_connections(
        &self,
        proxy_deadline_secs: u64,
        launcher_deadline_secs: u64,
    ) -> (usize, usize) {
        let now = epoch_secs();

        // Collect victims first: evicting while iterating a DashMap can
        // deadlock on the shard being iterated.
        let stale_sessions: Vec<(SessionId, u64)> = self
            .sessions
            .iter()
            .filter(|e| {
                now.saturating_sub(e.value().last_seen.load(Ordering::Relaxed))
                    > proxy_deadline_secs
            })
            .map(|e| (e.key().clone(), e.value().gen))
            .collect();

        let mut proxies_evicted = 0;
        for (key, gen) in &stale_sessions {
            if let Some(conn) = self.sessions.get(key) {
                if conn.gen != *gen {
                    continue; // superseded since we looked
                }
                let cancel = conn.cancel.clone();
                drop(conn);
                if self.evict_dead_connection(key, *gen, &cancel) {
                    warn!(
                        "Liveness sweep: proxy connection for session {} silent > {}s; evicted",
                        key, proxy_deadline_secs
                    );
                    proxies_evicted += 1;
                }
            }
        }

        let stale_launchers: Vec<(Uuid, u64)> = self
            .launchers
            .iter()
            .filter(|e| {
                now.saturating_sub(e.value().last_seen.load(Ordering::Relaxed))
                    > launcher_deadline_secs
            })
            .map(|e| (*e.key(), e.value().gen))
            .collect();

        let mut launchers_evicted = 0;
        for (id, gen) in &stale_launchers {
            if let Some(conn) = self.launchers.get(id) {
                if conn.gen != *gen {
                    continue;
                }
                let cancel = conn.cancel.clone();
                drop(conn);
                if self.unregister_launcher(id, Some(*gen)) {
                    warn!(
                        "Liveness sweep: launcher {} silent > {}s; evicted",
                        id, launcher_deadline_secs
                    );
                    launchers_evicted += 1;
                }
                cancel.cancel();
            }
        }

        // Real evictions, not stale candidates — a candidate can be spared
        // by the gen re-check when it reconnected mid-sweep, and incident
        // forensics should never overcount.
        (proxies_evicted, launchers_evicted)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::make_heartbeat;
    use super::super::LauncherConnection;
    use super::*;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    fn launcher_conn(
        user_id: Uuid,
        hostname: &str,
        cancel: CancellationToken,
    ) -> LauncherConnection {
        let (sender, rx) = mpsc::unbounded_channel();
        // Keep the channel open so eviction is attributable to liveness,
        // not to a dead channel.
        std::mem::forget(rx);
        LauncherConnection {
            sender,
            launcher_name: format!("launcher-{hostname}"),
            hostname: hostname.to_string(),
            user_id,
            running_sessions: Vec::new(),
            working_directory: None,
            version: "test".to_string(),
            cancel,
            gen: 0,
            last_seen: std::sync::atomic::AtomicU64::new(0),
        }
    }

    #[test]
    fn sweep_evicts_silent_connections_and_spares_fresh() {
        let mgr = SessionManager::new();

        // Fresh proxy connection.
        let (tx_fresh, _rx_fresh) = mpsc::unbounded_channel();
        mgr.register_session("fresh".into(), tx_fresh, CancellationToken::new());

        // Silent proxy connection: backdate its last_seen.
        let (tx_stale, mut rx_stale) = mpsc::unbounded_channel();
        let stale_cancel = CancellationToken::new();
        mgr.register_session("stale".into(), tx_stale, stale_cancel.clone());
        mgr.sessions
            .get("stale")
            .unwrap()
            .last_seen
            .store(epoch_secs() - 3600, Ordering::Relaxed);

        // Silent launcher.
        let launcher_id = Uuid::new_v4();
        let launcher_cancel = CancellationToken::new();
        mgr.try_register_launcher(
            launcher_id,
            launcher_conn(Uuid::new_v4(), "host1", launcher_cancel.clone()),
        )
        .expect("register launcher");
        mgr.launchers
            .get(&launcher_id)
            .unwrap()
            .last_seen
            .store(epoch_secs() - 3600, Ordering::Relaxed);

        let (proxies, launchers) = mgr.sweep_stale_connections(
            PROXY_LIVENESS_DEADLINE_SECS,
            LAUNCHER_LIVENESS_DEADLINE_SECS,
        );

        assert_eq!((proxies, launchers), (1, 1));
        assert!(mgr.sessions.contains_key("fresh"));
        assert!(!mgr.sessions.contains_key("stale"));
        assert!(stale_cancel.is_cancelled());
        assert!(!mgr.launchers.contains_key(&launcher_id));
        assert!(launcher_cancel.is_cancelled());

        // The evicted proxy's channel was still open — no message was sent
        // to it during eviction.
        assert!(rx_stale.try_recv().is_err());
    }

    #[test]
    fn touch_resets_the_clock() {
        let mgr = SessionManager::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        mgr.register_session("s1".into(), tx, CancellationToken::new());
        mgr.sessions
            .get("s1")
            .unwrap()
            .last_seen
            .store(epoch_secs() - 3600, Ordering::Relaxed);

        // An inbound frame arrives: the connection is alive after all.
        mgr.touch_session(&"s1".into());

        let (proxies, _) = mgr.sweep_stale_connections(
            PROXY_LIVENESS_DEADLINE_SECS,
            LAUNCHER_LIVENESS_DEADLINE_SECS,
        );
        assert_eq!(proxies, 0);
        assert!(mgr.sessions.contains_key("s1"));
        assert!(mgr.send_to_session(&"s1".into(), make_heartbeat()));
    }
}
