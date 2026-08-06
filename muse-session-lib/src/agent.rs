//! `MuseAgent`: the [`session_lib::Agent`] implementation for the `muse`
//! CLI. Construct `Session<MuseAgent>` to get a Muse-backed session.

use session_lib::agent::Agent;
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use tokio::sync::mpsc;

use crate::io_task::muse_io_task;

/// Zero-sized type that selects the Muse backend for `Session`.
pub struct MuseAgent;

impl Agent for MuseAgent {
    fn spawn_io_task(
        config: SessionConfig,
        command_rx: mpsc::UnboundedReceiver<IoCommand>,
        event_tx: mpsc::UnboundedSender<IoEvent>,
    ) -> Result<tokio::task::JoinHandle<()>, SessionError> {
        let handle = tokio::spawn(async move {
            muse_io_task(config, command_rx, event_tx).await;
        });
        Ok(handle)
    }
}
