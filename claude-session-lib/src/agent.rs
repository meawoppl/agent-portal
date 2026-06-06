//! `ClaudeAgent`: the [`session_lib::Agent`] implementation for the `claude`
//! CLI. Construct `Session<ClaudeAgent>` to get a Claude-backed session.

use session_lib::agent::Agent;
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use tokio::sync::mpsc;

use crate::io_task::claude_io_task;
use crate::spawn::spawn_claude;

/// Zero-sized type that selects the Claude backend for `Session`.
pub struct ClaudeAgent;

impl Agent for ClaudeAgent {
    fn spawn_io_task(
        config: SessionConfig,
        command_rx: mpsc::UnboundedReceiver<IoCommand>,
        event_tx: mpsc::UnboundedSender<IoEvent>,
    ) -> Result<tokio::task::JoinHandle<()>, SessionError> {
        // Spawn the io task; it owns the synchronous setup (spawning the
        // claude CLI + creating the AsyncClient). Failures during setup are
        // surfaced via the event channel — see the `Err(e) =>` branch below.
        let handle = tokio::spawn(async move {
            let session_id = config.session_id;
            let client = match spawn_claude(&config).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = event_tx.send(IoEvent::Error(e));
                    let _ = event_tx.send(IoEvent::Exited { code: 1 });
                    return;
                }
            };
            claude_io_task(session_id, client, command_rx, event_tx).await;
        });
        Ok(handle)
    }
}
