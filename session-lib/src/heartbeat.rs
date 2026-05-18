use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
        *self.last_received.lock().unwrap() = Instant::now();
    }

    /// Returns true if no heartbeat echo has been received within the timeout.
    pub fn is_expired(&self) -> bool {
        self.last_received.lock().unwrap().elapsed() > HEARTBEAT_TIMEOUT
    }

    /// Seconds since last heartbeat echo, for logging.
    pub fn elapsed_secs(&self) -> u64 {
        self.last_received.lock().unwrap().elapsed().as_secs()
    }
}
