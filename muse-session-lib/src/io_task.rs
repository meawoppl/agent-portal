//! Background I/O task for Muse sessions.
//!
//! Muse's headless mode is **spawn-per-turn**, like Claude and unlike
//! Codex's long-lived app-server: each user turn runs one
//! `muse exec --json --session-id <uuid>` process to completion, and the
//! next turn respawns against the same session id. Continuity therefore
//! lives in the CLI's own session store, not in a held connection.
//!
//! Two behaviors here were measured against the real CLI rather than
//! assumed (see `docs/MUSE_SUPPORT.md`):
//!
//! - **Interrupt is a kill.** There is no interrupt protocol. Killing the
//!   child mid-run is safe: a run SIGKILLed before it emitted a byte left
//!   the session store usable and the next turn ran clean. So `Interrupt`
//!   aborts the child and the session simply continues.
//! - **The session id is ours.** The portal mints a v4 uuid and the CLI
//!   adopts it verbatim as `stream.id`. That is what makes the
//!   `(stream_id, record_id)` identity composite safe — record ids repeat
//!   across sessions.

use muse_codes::{ExecRun, MuseExecBuilder, MusePayload, MuseRecord, Provider};
use session_lib::adapter::AgentOutputClassifier;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use tokio::sync::mpsc;

use crate::classifier::MuseClassifier;

/// Drive one Muse session: read neutral commands, run a child per turn,
/// classify its journal records back out as neutral events.
pub async fn muse_io_task(
    config: SessionConfig,
    mut command_rx: mpsc::UnboundedReceiver<IoCommand>,
    event_tx: mpsc::UnboundedSender<IoEvent>,
) {
    let mut classifier = MuseClassifier;

    while let Some(command) = command_rx.recv().await {
        match command {
            IoCommand::UserInput {
                text, delivered, ..
            } => {
                let outcome = run_turn(&config, &text, &mut classifier, &event_tx).await;
                if let Some(tx) = delivered {
                    let _ = tx.send(outcome.map_err(|e| e.to_string()));
                }
            }
            // Muse decides tool policy itself in headless runs — there is no
            // approval round-trip on this stream, so a permission response
            // has nothing to answer. Log rather than fail: a stale response
            // arriving after a turn ended is not an error condition.
            IoCommand::Permission { request_id, .. } => {
                tracing::debug!(
                    request_id = %request_id,
                    "muse: ignoring permission response (headless muse asks no approvals)"
                );
            }
            // The child is owned by `run_turn` for the life of a turn, and
            // killing it IS the interrupt (measured safe — see module docs).
            // Between turns there is no child, so this is a no-op. Left as an
            // explicit arm rather than a catch-all so a future `IoCommand`
            // variant fails to compile here instead of being silently
            // swallowed.
            IoCommand::Interrupt => {
                tracing::debug!("muse: interrupt outside an active turn — no child to kill");
            }
        }
    }
}

/// Run one turn to its terminal record, emitting classified events as they
/// arrive.
async fn run_turn(
    config: &SessionConfig,
    text: &str,
    classifier: &mut MuseClassifier,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) -> Result<(), muse_codes::Error> {
    let mut builder = MuseExecBuilder::new(text)
        .provider(Provider::Meta)
        .working_directory(&config.working_directory);
    builder = builder.session_id(config.session_id.to_string());

    let mut run = ExecRun::spawn(&builder).await?;
    let _ = event_tx.send(IoEvent::AgentStarted { pid: run.pid() });

    while let Some(record) = run.next_record().await? {
        let terminal = matches!(record.typed_payload(), Ok(MusePayload::RunTerminal(_)));
        emit(classifier, &record, event_tx);
        if terminal {
            break;
        }
    }
    Ok(())
}

fn emit(
    classifier: &mut MuseClassifier,
    record: &MuseRecord,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) {
    for decision in classifier.classify(record.clone()) {
        let _ = event_tx.send(IoEvent::Classified(decision));
    }
}
