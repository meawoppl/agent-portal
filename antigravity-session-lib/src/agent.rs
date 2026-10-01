use session_lib::agent::Agent;
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use tokio::sync::mpsc;

use crate::io_task::antigravity_io_task;

/// Zero-sized selector for the Antigravity session backend.
pub struct AntigravityAgent;

impl Agent for AntigravityAgent {
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
