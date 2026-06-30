//! Heartbeat-watchdog arm of the proxy connection loop (#1165 item 3).
//!
//! Extracted from the `run_main_loop` `select!`. On each interval tick: force a
//! reconnect if no heartbeat response has arrived within the window, otherwise
//! send a `Heartbeat` — with the lock+send bounded by a timeout so a wedged
//! `ws_write` on a half-dead socket can't starve the `select!` and strand the
//! session "shown but not connected" (#926).

use session_lib::heartbeat::HeartbeatTracker;
use shared::ProxyToServer;
use tracing::warn;

use super::{ConnectionResult, SharedWsWrite};

/// Run one heartbeat tick. Returns `Some(ConnectionResult::Disconnected)` to
/// force a reconnect (expired, send failed, or send wedged), or `None` to
/// continue the loop.
pub(super) async fn tick(
    heartbeat: &HeartbeatTracker,
    ws_write: &SharedWsWrite,
    connection_start: std::time::Instant,
) -> Option<ConnectionResult> {
    if heartbeat.is_expired() {
        warn!(
            "No heartbeat response in {}s, forcing reconnect",
            heartbeat.elapsed_secs()
        );
        return Some(ConnectionResult::Disconnected(connection_start.elapsed()));
    }
    // Bound the heartbeat lock+send. On a half-dead socket (e.g. after a backend
    // restart) the send can block forever; that starves this `select!` so the
    // disconnect / graceful-shutdown arms never fire and the data plane never
    // reconnects — the session is "shown but not connected" until the launcher
    // is restarted (#926). Timing out the lock acquisition *and* the send makes
    // this a true watchdog: even if another ws_write send is wedged holding the
    // lock, this fires and returns `Disconnected`, letting the reconnect/backoff
    // in `run_connection_loop` recover.
    let send = async {
        let mut ws = ws_write.lock().await;
        ws.send(ProxyToServer::Heartbeat).await
    };
    match tokio::time::timeout(session_lib::heartbeat::HEARTBEAT_TIMEOUT, send).await {
        Ok(Ok(())) => None,
        Ok(Err(e)) => {
            warn!("Heartbeat send failed ({e}), forcing reconnect");
            Some(ConnectionResult::Disconnected(connection_start.elapsed()))
        }
        Err(_) => {
            warn!(
                "Heartbeat send blocked >{}s (dead socket), forcing reconnect",
                session_lib::heartbeat::HEARTBEAT_TIMEOUT.as_secs()
            );
            Some(ConnectionResult::Disconnected(connection_start.elapsed()))
        }
    }
}
