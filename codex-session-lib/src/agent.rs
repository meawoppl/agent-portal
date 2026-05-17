//! `CodexAgent`: the [`session_lib::Agent`] implementation for the `codex`
//! CLI. Construct `Session<CodexAgent>` to get a Codex-backed session.

use session_lib::agent::Agent;
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use tokio::sync::mpsc;

use crate::io_task::codex_io_task;

/// Zero-sized type that selects the Codex backend for `Session`.
pub struct CodexAgent;

impl Agent for CodexAgent {
    fn spawn_io_task(
        config: SessionConfig,
        command_rx: mpsc::UnboundedReceiver<IoCommand>,
        event_tx: mpsc::UnboundedSender<IoEvent>,
    ) -> Result<tokio::task::JoinHandle<()>, SessionError> {
        let handle = tokio::spawn(async move {
            codex_io_task(config, command_rx, event_tx).await;
        });
        Ok(handle)
    }
}
