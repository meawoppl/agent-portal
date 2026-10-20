use std::time::Duration;

use session_lib::agent::Agent;
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use tokio::sync::mpsc;

use crate::io_task::antigravity_io_task;

/// Zero-sized type that selects the Antigravity backend for `Session`.
pub struct AntigravityAgent;

impl Agent for AntigravityAgent {
    fn graceful_stop_timeout() -> Option<Duration> {
        Some(Duration::from_secs(8))
    }

    fn spawn_io_task(
        config: SessionConfig,
        command_rx: mpsc::UnboundedReceiver<IoCommand>,
        event_tx: mpsc::UnboundedSender<IoEvent>,
    ) -> Result<tokio::task::JoinHandle<()>, SessionError> {
        Ok(tokio::spawn(async move {
            antigravity_io_task(config, command_rx, event_tx).await;
        }))
    }
}
