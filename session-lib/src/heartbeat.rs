use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Lock the heartbeat timestamp, recovering the guarded value when a previous
/// holder panicked. Poisoning is sticky: without recovery every later
/// `received`/`is_expired` call would panic and the proxy would never notice
/// a dead connection (or never stop seeing one as dead).
fn lock_timestamp(mutex: &Mutex<Instant>) -> std::sync::MutexGuard<'_, Instant> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
pub const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(15);

/// Tracks heartbeat round-trip timing for dead connection detection.
///
/// The proxy sends a Heartbeat every `HEARTBEAT_INTERVAL`. The backend echoes
/// it back. If no echo is received within `HEARTBEAT_TIMEOUT`, the connection
/// is considered dead and the proxy forces a reconnect.
#[derive(Clone)]
pub struct HeartbeatTracker {
    last_received: Arc<Mutex<Instant>>,
}

impl Default for HeartbeatTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl HeartbeatTracker {
    pub fn new() -> Self {
        Self {
            last_received: Arc::new(Mutex::new(Instant::now())),
        }
    }

    /// Called when a heartbeat echo is received from the backend.
    pub fn received(&self) {
        *lock_timestamp(&self.last_received) = Instant::now();
    }

    /// Returns true if no heartbeat echo has been received within the timeout.
    pub fn is_expired(&self) -> bool {
        lock_timestamp(&self.last_received).elapsed() > HEARTBEAT_TIMEOUT
    }

    /// Seconds since last heartbeat echo, for logging.
    pub fn elapsed_secs(&self) -> u64 {
        lock_timestamp(&self.last_received).elapsed().as_secs()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_lock_still_tracks_heartbeats() {
        let tracker = HeartbeatTracker::new();
        let poisoned = tracker.clone();
        let _ = std::panic::catch_unwind(move || {
            let _guard = poisoned.last_received.lock().unwrap();
            panic!("simulated holder panic");
        });
        assert!(tracker.last_received.is_poisoned());

        // Recovery, not a wedge: all three methods keep working.
        tracker.received();
        assert!(!tracker.is_expired());
        assert!(tracker.elapsed_secs() < HEARTBEAT_TIMEOUT.as_secs());
    }
}
