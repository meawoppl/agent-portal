//! The [`Agent`] trait: per-agent dispatch for `Session<A>`.
//!
//! Each agent crate (claude-session-lib, codex-session-lib) defines a
//! zero-sized struct that implements this trait. The trait method spawns
//! the per-session I/O task; the task owns whatever process(es) it needs,
//! reads commands from `command_rx`, and forwards events back via
//! `event_tx`. The task is expected to terminate cleanly when
//! `command_rx` is dropped or the underlying process exits.

use tokio::sync::mpsc;

use crate::error::SessionError;
use crate::io::{IoCommand, IoEvent};
use crate::snapshot::SessionConfig;

/// Per-agent backend for [`crate::Session`].
pub trait Agent: Send + Sync + 'static {
    /// Spawn the per-session I/O task and return its `JoinHandle`.
    ///
    /// The task is responsible for:
    /// - Starting whatever process(es) the agent needs.
    /// - Reading [`IoCommand`]s off `command_rx` and acting on them.
    /// - Classifying the agent's output and emitting neutral
    ///   [`IoEvent::Classified`] decisions (plus lifecycle/raw events).
    /// - Terminating cleanly when the process exits or `command_rx` closes.
    fn spawn_io_task(
        config: SessionConfig,
        command_rx: mpsc::UnboundedReceiver<IoCommand>,
        event_tx: mpsc::UnboundedSender<IoEvent>,
    ) -> Result<tokio::task::JoinHandle<()>, SessionError>;
}
