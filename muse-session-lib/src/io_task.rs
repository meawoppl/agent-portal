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

use muse_codes::{ExecRun, MuseExecBuilder, MusePayload, MuseRecord, Provider, RunUsage};
use session_lib::adapter::AgentOutputClassifier;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use session_lib::{TurnOutcome, TurnTracker};
use std::time::Instant;
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

    // Per-turn performance metrics (#1798), same machinery as claude/codex:
    // `start`ed on each user input, first-token latched on the first streamed
    // output, finalized on `run.terminal.*`. The preceding `run.usage`
    // snapshot supplies token totals; cost and context occupancy remain
    // unknown because the wire does not report them.
    let mut turn_tracker = TurnTracker::new(config.session_id);

    while let Some(command) = command_rx.recv().await {
        match command {
            IoCommand::UserInput {
                text, delivered, ..
            } => {
                let mut delivered = delivered;
                turn_tracker.start(Instant::now(), chrono::Utc::now());
                let outcome = run_turn(
                    &config,
                    &text,
                    &mut classifier,
                    &mut turn_tracker,
                    &event_tx,
                    &mut delivered,
                )
                .await;
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
            IoCommand::Shutdown { completed } => {
                let _ = completed.send(());
                break;
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
    turn_tracker: &mut TurnTracker,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
    delivered: &mut Option<oneshot::Sender<Result<(), String>>>,
) -> Result<(), muse_codes::Error> {
    let builder = build_exec_builder(config, text);

    let mut run = ExecRun::spawn(&builder).await?;
    let _ = event_tx.send(IoEvent::AgentStarted { pid: run.pid() });

    // Turn-scoped telemetry inputs, filled from the journal as it streams.
    let mut model: Option<String> = None;
    let mut usage: Option<RunUsage> = None;

    while let Some(record) = run.next_record().await? {
        let mut terminal = false;
        match record.typed_payload() {
            Ok(MusePayload::ModelConfigured(configured)) => {
                model = Some(configured.model_id.clone());
            }
            Ok(MusePayload::RunOutputDelta(_)) => {
                // First streamed model output latches TTFT; later deltas feed
                // the inter-token-gap stat.
                turn_tracker.record_content_frame(Instant::now());
            }
            Ok(MusePayload::ToolResult(_)) => {
                turn_tracker.record_tool_call();
            }
            Ok(MusePayload::RunUsage(snapshot)) => {
                // A run-level snapshot, not a delta: retain only the latest
                // frame so a replay cannot count the same tokens twice.
                usage = Some(snapshot);
            }
            Ok(MusePayload::RunTerminal(t)) => {
                terminal = true;
                finalize_turn_metrics(turn_tracker, model.take(), usage.as_ref(), &t, event_tx);
            }
            _ => {}
        }
        acknowledge_delivery_if_accepted(&record, delivered);
        emit(classifier, &record, event_tx);
        if terminal {
            break;
        }
    }
    Ok(())
}

/// Project a finished muse run into the agent-neutral [`TurnOutcome`] and emit
/// the resulting metrics (#1798).
///
/// `run.usage` totals span all model calls (and children when the wire says
/// `children_included`), so sum each per-model row once. Cached and reasoning
/// tokens remain separate diagnostic counters; no separate child breakdown,
/// cost or context snapshot is available. Older CLIs without a usage frame
/// retain zero token counters. Model-less echo runs emit no metrics.
fn finalize_turn_metrics(
    turn_tracker: &mut TurnTracker,
    model: Option<String>,
    usage: Option<&RunUsage>,
    terminal: &muse_codes::RunTerminal,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) {
    // Clamp wire u64 counters to the signed metrics range instead of wrapping.
    let total = |field: fn(&muse_codes::RunUsageModel) -> u64| -> i64 {
        usage
            .into_iter()
            .flat_map(|snapshot| &snapshot.models)
            .fold(0i64, |sum, row| {
                sum.saturating_add(i64::try_from(field(row)).unwrap_or(i64::MAX))
            })
    };
    let outcome = TurnOutcome {
        agent_type: shared::AgentType::Muse,
        model,
        service_tier: None,
        input_tokens: total(|row| row.input_tokens),
        output_tokens: total(|row| row.output_tokens),
        cache_creation_tokens: 0,
        cache_read_tokens: total(|row| row.cached_tokens),
        thinking_tokens: total(|row| row.reasoning_tokens),
        subagent_tokens: 0,
        context_snapshot_tokens: None,
        stop_reason: Some(terminal.terminal.clone()),
        is_error: terminal.terminal != "completed",
        total_cost_usd: None,
        model_context_window: None,
    };
    if let Some(metrics) = turn_tracker.finalize(Instant::now(), chrono::Utc::now(), outcome) {
        if metrics.has_known_model() {
            let _ = event_tx.send(IoEvent::TurnMetricsReady(Box::new(metrics)));
        } else {
            tracing::debug!("Muse run ended without run.model.configured; dropping turn metrics");
        }
    }
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
    let mut builder = MuseExecBuilder::new(text)
        .binary(
            config
                .runtime_command
                .as_ref()
                .or(config.claude_path.as_ref())
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| "muse".into()),
        )
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
        // Muse 1.4.13 enables async questions even under --yolo. The
        // spawn-per-turn exec driver has no channel to return answers.
        .disable_request_user_input(true)
        .extra_args(config.extra_args.clone());
    for (key, value) in config.portal_environment() {
        builder = builder.env(key, value);
    }
    builder
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

    fn run_terminal(terminal: &str) -> muse_codes::RunTerminal {
        serde_json::from_value(serde_json::json!({
            "kind": format!("run.terminal.{terminal}"),
            "command_id": "cmd-1",
            "run_stream": {"kind": "run", "id": "r-1"},
            "terminal": terminal,
            "reason": null,
        }))
        .expect("RunTerminal fixture")
    }

    /// Older CLIs without a usage frame still report timing and terminal state.
    #[test]
    fn muse_run_projects_into_turn_metrics() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tracker = TurnTracker::new(uuid::Uuid::nil());
        tracker.start(Instant::now(), chrono::Utc::now());
        tracker.record_content_frame(Instant::now());
        tracker.record_tool_call();
        tracker.record_tool_call();

        finalize_turn_metrics(
            &mut tracker,
            Some("muse-spark-1.3".to_string()),
            None,
            &run_terminal("completed"),
            &tx,
        );

        let IoEvent::TurnMetricsReady(metrics) = rx.try_recv().expect("metrics emitted") else {
            panic!("expected TurnMetricsReady");
        };
        assert_eq!(metrics.agent_type, shared::AgentType::Muse);
        assert_eq!(metrics.model.as_deref(), Some("muse-spark-1.3"));
        assert_eq!(metrics.stop_reason.as_deref(), Some("completed"));
        assert!(!metrics.is_error);
        assert_eq!(metrics.tool_call_count, 2);
        assert!(metrics.ttft_ms.is_some(), "content frame latched TTFT");
        // No usage snapshot on this older-CLI path.
        assert_eq!(metrics.input_tokens, 0);
        assert_eq!(metrics.total_cost_usd, None);
        assert_eq!(metrics.model_context_window, None);
        assert_eq!(metrics.context_snapshot_tokens, None);
    }

    /// A failed terminal marks the turn as an error, and a run that never
    /// configured a model (the echo provider) emits nothing.
    #[test]
    fn failed_terminal_and_modelless_runs() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tracker = TurnTracker::new(uuid::Uuid::nil());

        tracker.start(Instant::now(), chrono::Utc::now());
        finalize_turn_metrics(
            &mut tracker,
            Some("muse-spark-1.3".to_string()),
            None,
            &run_terminal("failed"),
            &tx,
        );
        let IoEvent::TurnMetricsReady(metrics) = rx.try_recv().expect("metrics emitted") else {
            panic!("expected TurnMetricsReady");
        };
        assert!(metrics.is_error);
        assert_eq!(metrics.stop_reason.as_deref(), Some("failed"));

        tracker.start(Instant::now(), chrono::Utc::now());
        finalize_turn_metrics(&mut tracker, None, None, &run_terminal("completed"), &tx);
        assert!(rx.try_recv().is_err(), "model-less run emits no metrics");
    }

    #[test]
    fn usage_snapshot_sums_models_without_double_counting_cached_or_child_tokens() {
        let usage: RunUsage = serde_json::from_value(serde_json::json!({
            "kind": "run_usage", "command_id": "cmd-1",
            "run_stream": {"kind": "run", "id": "r-1"},
            "calls": 3, "completed_calls": 2, "failed_calls": 1,
            "cancelled_calls": 0, "children_included": true, "allowance": null,
            "models": [
                {"model": "muse-spark-1.3-contributor", "calls": 2,
                 "input_tokens": 100, "cached_tokens": 40,
                 "output_tokens": 20, "reasoning_tokens": 5},
                {"model": "muse-wonder-1.0", "calls": 1,
                 "input_tokens": 50, "cached_tokens": 10,
                 "output_tokens": 8, "reasoning_tokens": 2}
            ]
        }))
        .expect("typed usage");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut tracker = TurnTracker::new(uuid::Uuid::nil());
        tracker.start(Instant::now(), chrono::Utc::now());
        finalize_turn_metrics(
            &mut tracker,
            Some("muse-spark-1.3-contributor".into()),
            Some(&usage),
            &run_terminal("completed"),
            &tx,
        );
        let IoEvent::TurnMetricsReady(metrics) = rx.try_recv().expect("metrics") else {
            panic!("expected metrics");
        };
        assert_eq!(metrics.input_tokens, 150);
        assert_eq!(metrics.cache_read_tokens, 50);
        assert_eq!(metrics.output_tokens, 28);
        assert_eq!(metrics.thinking_tokens, 7);
        // Child usage is already included in the wire totals, with no separate breakdown.
        assert_eq!(metrics.subagent_tokens, 0);
        assert_eq!(metrics.cache_creation_tokens, 0);
        assert_eq!(metrics.total_cost_usd, None);
        assert_eq!(metrics.context_snapshot_tokens, None);
        assert_eq!(metrics.model_context_window, None);

        // A later turn without a usage frame must not inherit these totals.
        tracker.start(Instant::now(), chrono::Utc::now());
        finalize_turn_metrics(
            &mut tracker,
            Some("muse-spark-1.3-contributor".into()),
            None,
            &run_terminal("failed"),
            &tx,
        );
        let IoEvent::TurnMetricsReady(metrics) = rx.try_recv().expect("metrics") else {
            panic!("expected metrics");
        };
        assert_eq!(metrics.input_tokens, 0);
        assert_eq!(metrics.output_tokens, 0);
        assert!(metrics.is_error);
    }

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

#[cfg(all(test, unix))]
mod runtime_command_tests {
    use super::*;
    #[test]
    fn runtime_wrapper_overrides_binary_without_changing_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let wrapper = directory.path().join("agent wrapper with spaces");
        std::os::unix::fs::symlink("/bin/echo", &wrapper).unwrap();
        let config = SessionConfig {
            runtime_command: Some(wrapper.clone()),
            claude_path: Some("/must-not-launch-this-original".into()),
            extra_args: vec!["--model".into(), "example".into()],
            ..Default::default()
        };
        let command = build_exec_builder(&config, "prompt with spaces")
            .build_command()
            .unwrap();
        assert_eq!(command.as_std().get_program(), wrapper.as_os_str());
        assert!(command
            .as_std()
            .get_args()
            .any(|arg| arg == "prompt with spaces"));
        assert!(command.as_std().get_args().any(|arg| arg == "example"));
        assert!(command
            .as_std()
            .get_args()
            .any(|arg| arg == "--disable-request-user-input"));
    }
}
