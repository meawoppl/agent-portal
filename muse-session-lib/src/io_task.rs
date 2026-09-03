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
use tokio::sync::{mpsc, oneshot};

use crate::classifier::MuseClassifier;

/// Drive one Muse session: read neutral commands, run a child per turn,
/// classify its journal records back out as neutral events.
pub async fn muse_io_task(
    config: SessionConfig,
    mut command_rx: mpsc::UnboundedReceiver<IoCommand>,
    event_tx: mpsc::UnboundedSender<IoEvent>,
) {
    // One classifier for the whole session, not per turn: the reminder
    // screen learns task ids from `proposed` records, and a task's records
    // can only span one turn — but keeping it session-scoped is harmless
    // and avoids re-learning state mid-stream on a respawn boundary.
    let mut classifier = MuseClassifier::default();

    while let Some(command) = command_rx.recv().await {
        match command {
            IoCommand::UserInput {
                text, delivered, ..
            } => {
                let mut delivered = delivered;
                let outcome =
                    run_turn(&config, &text, &mut classifier, &event_tx, &mut delivered).await;
                // Compatibility fallback for a Muse version that completes a
                // run without emitting the typed acceptance record. Current
                // Muse releases resolve this earlier at `turn.input.user`.
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
    delivered: &mut Option<oneshot::Sender<Result<(), String>>>,
) -> Result<(), muse_codes::Error> {
    let builder = build_exec_builder(config, text);

    let mut run = ExecRun::spawn(&builder).await?;
    let _ = event_tx.send(IoEvent::AgentStarted { pid: run.pid() });

    while let Some(record) = run.next_record().await? {
        let terminal = matches!(record.typed_payload(), Ok(MusePayload::RunTerminal(_)));
        acknowledge_delivery_if_accepted(&record, delivered);
        emit(classifier, &record, event_tx);
        if terminal {
            break;
        }
    }
    Ok(())
}

/// Resolve Portal's delivery signal when Muse journals the user's input into
/// the run. Process spawn and command acceptance happen earlier, but
/// `turn.input.user` is the first record proving the prompt is in the agent's
/// stream — the same boundary represented by `AgentAccepted` for other agents.
fn acknowledge_delivery_if_accepted(
    record: &MuseRecord,
    delivered: &mut Option<oneshot::Sender<Result<(), String>>>,
) -> bool {
    if !matches!(record.typed_payload(), Ok(MusePayload::TurnInputUser(_))) {
        return false;
    }
    if let Some(tx) = delivered.take() {
        let _ = tx.send(Ok(()));
    }
    true
}

/// Build the `muse exec` invocation for one turn. Extracted from [`run_turn`] so
/// the argv is testable without spawning: it is the single place session config
/// maps onto the muse CLI.
///
/// `config.extra_args` is forwarded verbatim (the launch dialog's model picker
/// emits `--model <id>`, plus anything typed in the extra-args box). The
/// passthrough sits after the typed flags and before the positional prompt, so
/// raw tokens can never override structural args. Mirrors how claude
/// (`args.extend`) and codex (`.extra_args`) pass the same field — muse was
/// previously the lone agent that dropped it.
fn build_exec_builder(config: &SessionConfig, text: &str) -> MuseExecBuilder {
    MuseExecBuilder::new(text)
        .provider(Provider::Meta)
        .working_directory(&config.working_directory)
        .session_id(config.session_id.to_string())
        // Exported so shell tools muse spawns inherit it — `agent-portal
        // message send` resolves the sender from this var (its first arm in
        // `sender_session_id`; claude/codex have agent-specific fallbacks
        // there, muse has none). Without it, inter-agent sends from muse fall
        // back to the backend's untyped "[portal message from <user>]" string
        // and the recipient renders a raw user message instead of the typed
        // agent-message card. Muse is spawn-per-turn against one stable portal
        // session id, so the var cannot go stale the way claude's /clear does.
        .env("PORTAL_SESSION_ID", config.session_id.to_string())
        .yolo(config.muse_yolo)
        .extra_args(config.extra_args.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn corpus_record(payload_type: &str) -> MuseRecord {
        include_str!("../tests/corpus_echo_turn.jsonl")
            .lines()
            .filter_map(|line| serde_json::from_str::<MuseRecord>(line).ok())
            .find(|record| record.payload_type == payload_type)
            .unwrap_or_else(|| panic!("missing {payload_type} in Muse corpus"))
    }

    #[test]
    fn turn_input_user_acknowledges_delivery_before_turn_terminal() {
        let (tx, mut rx) = oneshot::channel();
        let mut delivered = Some(tx);

        assert!(!acknowledge_delivery_if_accepted(
            &corpus_record("runtime.command.accepted"),
            &mut delivered,
        ));
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        assert!(acknowledge_delivery_if_accepted(
            &corpus_record("turn.input.user"),
            &mut delivered,
        ));
        assert!(delivered.is_none());
        assert_eq!(rx.try_recv(), Ok(Ok(())));
    }

    /// Live argv check: the launcher's `extra_args` must reach the spawned
    /// `muse exec` argv (this is the exact seam muse previously dropped). It
    /// resolves the real `muse` binary via `which`, so it's `#[ignore]`d —
    /// run it on a host with muse installed via
    /// `cargo test -p muse-session-lib -- --ignored`. muse-codes' own
    /// `extra_args_sit_between_typed_flags_and_the_prompt` pins the argv
    /// position in CI; wirecheck live-verifies the flag surface against the CLI.
    #[test]
    #[ignore = "requires the muse binary on PATH; run with --ignored"]
    fn extra_args_reach_the_muse_argv() {
        let config = SessionConfig {
            working_directory: PathBuf::from("/tmp"),
            extra_args: vec!["--model".to_string(), "test-model".to_string()],
            muse_yolo: true,
            ..Default::default()
        };
        let cmd = build_exec_builder(&config, "hello")
            .build_command()
            .expect("muse binary should resolve on PATH");
        let argv: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();

        assert!(argv.contains(&"exec".to_string()), "argv: {argv:?}");
        for token in ["--model", "test-model", "--yolo"] {
            assert!(
                argv.iter().any(|a| a == token),
                "expected {token:?} in argv: {argv:?}"
            );
        }
        // Passthrough sits before the positional prompt, which stays last.
        assert_eq!(
            argv.last().map(String::as_str),
            Some("hello"),
            "argv: {argv:?}"
        );
        let session_pos = argv.iter().position(|a| a == "--session-id").unwrap();
        let model_pos = argv.iter().position(|a| a == "--model").unwrap();
        assert!(
            session_pos < model_pos,
            "typed flags precede the raw passthrough; argv: {argv:?}"
        );

        // Sender attribution: shell tools muse spawns must inherit the portal
        // session id so `agent-portal message send` attributes the sender.
        let envs: Vec<(String, String)> = cmd
            .as_std()
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert!(
            envs.iter()
                .any(|(k, v)| k == "PORTAL_SESSION_ID" && *v == config.session_id.to_string()),
            "PORTAL_SESSION_ID must be exported to the muse child; envs: {envs:?}"
        );
    }
}
