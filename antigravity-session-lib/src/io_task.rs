use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use antigravity_codes::protocol::{HarnessSideTools, OutputEventEvent, TrajectoryStateUpdateState};
use antigravity_codes::{Client, HarnessOptions, ModelBuilder, Step, StepKind};
use serde::Serialize;
use session_lib::adapter::AgentOutput;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use session_lib::{TurnOutcome, TurnTracker};
use tokio::sync::mpsc;

const DEFAULT_MODEL: &str = "gemini-flash-latest";
const SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Serialize)]
struct ErrorEnvelope {
    #[serde(rename = "type")]
    frame_type: &'static str,
    message: String,
}

pub async fn antigravity_io_task(
    config: SessionConfig,
    mut command_rx: mpsc::UnboundedReceiver<IoCommand>,
    event_tx: mpsc::UnboundedSender<IoEvent>,
) {
    let options = harness_options(&config);
    let client = match options.as_ref() {
        Ok(options) => match launch_client(options.clone(), config.session_id).await {
            Ok(client) => Some(client),
            Err(message) => {
                emit_error(&event_tx, message);
                None
            }
        },
        Err(message) => {
            emit_error(&event_tx, message.clone());
            None
        }
    };
    // antigravity-codes intentionally owns the harness process and does not
    // expose its pid yet. Session stop still works: aborting this task drops
    // Client -> Harness, whose Drop kills the child.
    let _ = event_tx.send(IoEvent::AgentStarted { pid: None });

    let model = configured_model(&config.extra_args).to_string();
    let mut tracker = TurnTracker::new(config.session_id);
    let mut client = client;
    let mut pending = VecDeque::new();

    loop {
        let command = if let Some(command) = pending.pop_front() {
            command
        } else if let Some(command) = command_rx.recv().await {
            command
        } else {
            break;
        };
        match command {
            IoCommand::UserInput {
                text,
                mut delivered,
                display_event,
                ..
            } => {
                let options = match options.as_ref() {
                    Ok(options) => options,
                    Err(message) => {
                        fail_undelivered_turn(
                            &event_tx,
                            &mut delivered,
                            format!("Antigravity is not configured: {message}"),
                        );
                        continue;
                    }
                };
                let mut active_client = match client.take() {
                    Some(client) => client,
                    None => match launch_client(options.clone(), config.session_id).await {
                        Ok(client) => client,
                        Err(message) => {
                            fail_undelivered_turn(&event_tx, &mut delivered, message);
                            continue;
                        }
                    },
                };
                let previous_usage = active_client.usage().cloned().unwrap_or_default();
                tracker.start(Instant::now(), chrono::Utc::now());
                let mut turn = match active_client.send(text).await {
                    Ok(turn) => {
                        if let Some(tx) = delivered.take() {
                            let _ = tx.send(Ok(()));
                        }
                        turn
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if let Some(tx) = delivered.take() {
                            let _ = tx.send(Err(message.clone()));
                        }
                        emit_failed_turn(
                            &event_tx,
                            format!("Antigravity rejected input: {message}"),
                        );
                        let usage = active_client.usage().cloned().unwrap_or_default();
                        emit_metrics(
                            &event_tx,
                            &mut tracker,
                            MetricsReport {
                                model: &model,
                                before: &previous_usage,
                                after: &usage,
                                interrupted: false,
                                failure: Some(&message),
                            },
                        );
                        shutdown_client(active_client, &event_tx).await;
                        continue;
                    }
                };
                // The harness's native user Message is suppressed below because
                // Portal already owns the optimistic user row. A typed
                // inter-agent input has no equivalent native echo, though, so
                // preserve its provenance card verbatim once delivery succeeds.
                if let Some(event) = display_event {
                    let _ = event_tx.send(IoEvent::RawOutput(*event));
                }

                let mut interrupted = false;
                let mut failed = None;
                loop {
                    tokio::select! {
                        next = turn.next_step() => match next {
                            Ok(Some(step)) => emit_step(&event_tx, &mut tracker, step),
                            Ok(None) => break,
                            Err(error) => {
                                failed = Some(error.to_string());
                                break;
                            }
                        },
                        incoming = command_rx.recv() => match incoming {
                            Some(IoCommand::Interrupt) => {
                                interrupted = true;
                                break;
                            }
                            Some(IoCommand::Permission { request_id, .. }) => {
                                tracing::debug!(%request_id, "Antigravity handles confirmations in-harness; ignoring portal permission response");
                            }
                            Some(input @ IoCommand::UserInput { .. }) => pending.push_back(input),
                            // The generic session owner is stopping. Convert
                            // that into a normal cancellation so this accepted
                            // turn reaches a terminal state before its
                            // persistence acknowledgement is requested.
                            None => {
                                interrupted = true;
                                break;
                            },
                        }
                    }
                }
                drop(turn);

                if interrupted {
                    if let Err(error) = active_client.cancel().await {
                        emit_error(
                            &event_tx,
                            format!("failed to interrupt Antigravity: {error}"),
                        );
                    } else {
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(10),
                            drain_cancel(&mut active_client),
                        )
                        .await
                        {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => emit_error(
                                &event_tx,
                                format!("Antigravity interrupt did not settle cleanly: {error}"),
                            ),
                            Err(_) => emit_error(
                                &event_tx,
                                "Antigravity interrupt timed out while waiting for idle"
                                    .to_string(),
                            ),
                        }
                    }
                }
                if let Some(message) = failed.as_ref() {
                    emit_error(&event_tx, format!("Antigravity turn failed: {message}"));
                }

                let status = if interrupted {
                    shared::antigravity::AntigravityTurnStatus::Cancelled
                } else if failed.is_some() {
                    shared::antigravity::AntigravityTurnStatus::Failed
                } else {
                    shared::antigravity::AntigravityTurnStatus::Completed
                };
                emit_terminal(&event_tx, status);

                let usage = active_client.usage().cloned().unwrap_or_default();
                emit_metrics(
                    &event_tx,
                    &mut tracker,
                    MetricsReport {
                        model: &model,
                        before: &previous_usage,
                        after: &usage,
                        interrupted,
                        failure: failed.as_deref(),
                    },
                );
                // `Session::stop` aborts agent tasks rather than asking them to
                // shut down. Antigravity only guarantees persisted cascade
                // state after its session-end acknowledgement, so close after
                // every completed turn and resume the same cascade next turn.
                // This trades a small launch cost for durable, restart-safe
                // history without weakening the generic Session contract.
                shutdown_client(active_client, &event_tx).await;
            }
            IoCommand::Interrupt => {
                tracing::debug!("Antigravity interrupt received while idle");
            }
            IoCommand::Permission { request_id, .. } => {
                tracing::debug!(%request_id, "Antigravity permission response received while idle");
            }
        }
    }

    if let Some(client) = client {
        shutdown_client(client, &event_tx).await;
    }
}

fn fail_undelivered_turn(
    event_tx: &mpsc::UnboundedSender<IoEvent>,
    delivered: &mut Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    message: String,
) {
    if let Some(tx) = delivered.take() {
        let _ = tx.send(Err(message.clone()));
    }
    emit_failed_turn(event_tx, message);
}

fn emit_failed_turn(event_tx: &mpsc::UnboundedSender<IoEvent>, message: String) {
    emit_error(event_tx, message);
    emit_terminal(event_tx, shared::antigravity::AntigravityTurnStatus::Failed);
}

fn emit_terminal(
    event_tx: &mpsc::UnboundedSender<IoEvent>,
    status: shared::antigravity::AntigravityTurnStatus,
) {
    let terminal = shared::antigravity::AntigravityTurnCompletedEnvelope::new(status);
    if let Ok(value) = serde_json::to_value(terminal) {
        let _ = event_tx.send(IoEvent::Classified(AgentOutput::Visible(value)));
    }
}

async fn launch_client(options: HarnessOptions, session_id: uuid::Uuid) -> Result<Client, String> {
    let client = Client::launch(options)
        .await
        .map_err(|error| format!("failed to launch Antigravity: {error}"))?;
    let expected = session_id.to_string();
    match client.cascade_id() {
        Some(actual) if actual == expected => Ok(client),
        actual => {
            let actual = actual.unwrap_or("<missing>").to_string();
            let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, client.shutdown()).await;
            Err(format!(
                "Antigravity returned cascade id {actual:?}, expected {expected:?}; refusing to risk resuming a different conversation"
            ))
        }
    }
}

async fn shutdown_client(client: Client, event_tx: &mpsc::UnboundedSender<IoEvent>) {
    match tokio::time::timeout(SHUTDOWN_TIMEOUT, client.shutdown()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => emit_error(
            event_tx,
            format!("Antigravity failed to persist the completed turn: {error}"),
        ),
        Err(_) => emit_error(
            event_tx,
            "Antigravity timed out while persisting the completed turn".to_string(),
        ),
    }
}

fn harness_options(config: &SessionConfig) -> Result<HarnessOptions, String> {
    let api_key = std::env::var("GEMINI_API_KEY")
        .map_err(|_| "Antigravity requires GEMINI_API_KEY on the launcher host".to_string())?;
    let model = configured_model(&config.extra_args);
    let mut options = HarnessOptions::new()
        .workspace(&config.working_directory)
        .model(ModelBuilder::gemini(model, api_key))
        // The tentative integration is deliberately read-only. The SDK's
        // default confirmation handler refuses writes; widening this before
        // Portal can surface native confirmations would be an approval bypass.
        .harness_side_tools(HarnessSideTools::read_only())
        .cascade_id(config.session_id.to_string())
        .env("PORTAL_SESSION_ID", config.session_id.to_string());

    if let Some(path) = discover_harness() {
        options = options.binary(path);
    }
    if let Some(project) = directories::ProjectDirs::from("org", "CosmicFrontier", "agent-portal") {
        options = options.storage_directory(project.data_dir().join("antigravity"));
    }
    Ok(options)
}

fn configured_model(args: &[String]) -> &str {
    args.windows(2)
        .find_map(|pair| (pair[0] == "--model").then_some(pair[1].as_str()))
        .unwrap_or(DEFAULT_MODEL)
}

fn discover_harness() -> Option<PathBuf> {
    if let Ok(path) = antigravity_codes::process::find_harness() {
        return Some(path);
    }
    if let Some(path) =
        session_lib::probe::antigravity_managed_harness_path().filter(|path| path.is_file())
    {
        return Some(path);
    }
    // `pip install google-antigravity` keeps the binary inside the wheel.
    // Ask Python for that deterministic package-relative path so the Install
    // button produces a launcher that works without a manual symlink.
    let output = std::process::Command::new("python3")
        .args([
            "-c",
            "import pathlib, google.antigravity as a; print(pathlib.Path(a.__file__).parent / 'bin' / 'localharness')",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    path.is_file().then_some(path)
}

fn emit_step(event_tx: &mpsc::UnboundedSender<IoEvent>, tracker: &mut TurnTracker, step: Step) {
    // The harness reports the user's own prompt as a Message step addressed to
    // the agent. Portal already owns the optimistic user row; persisting this
    // echo would render the prompt twice.
    if step.kind == StepKind::Message && step.user_facing_text().is_none() {
        return;
    }
    if !step.text_delta.is_empty() {
        tracker.record_content_frame(Instant::now());
    }
    // Active and final Step values are snapshots of the same logical step.
    // Count only the terminal snapshot so a streamed tool does not inflate the
    // turn's tool-call metric.
    let is_final = step.is_final();
    if is_final
        && !matches!(
            step.kind,
            StepKind::Message | StepKind::Finish | StepKind::Compaction
        )
    {
        tracker.record_tool_call();
    }

    let envelope = shared::antigravity::AntigravityStepEnvelope {
        frame_type: shared::antigravity::STEP_FRAME_TYPE.to_string(),
        trajectory_id: step.trajectory_id,
        step_index: step.step_index,
        kind: portal_step_kind(step.kind),
        state: step.state,
        source: step.source,
        target: step.target,
        text: step.text,
        thinking: step.thinking,
        error_message: step.error_message,
        update: step.update,
    };
    let Ok(payload) = serde_json::to_value(envelope) else {
        return;
    };
    let classified = if is_final {
        AgentOutput::Visible(payload)
    } else {
        AgentOutput::Ephemeral(payload)
    };
    let _ = event_tx.send(IoEvent::Classified(classified));
}

fn portal_step_kind(kind: StepKind) -> shared::antigravity::AntigravityStepKind {
    use shared::antigravity::AntigravityStepKind as PortalKind;
    match kind {
        StepKind::Message => PortalKind::Message,
        StepKind::ListDirectory => PortalKind::ListDirectory,
        StepKind::FindFile => PortalKind::FindFile,
        StepKind::SearchDirectory => PortalKind::SearchDirectory,
        StepKind::ViewFile => PortalKind::ViewFile,
        StepKind::CreateFile => PortalKind::CreateFile,
        StepKind::EditFile => PortalKind::EditFile,
        StepKind::RunCommand => PortalKind::RunCommand,
        StepKind::Compaction => PortalKind::Compaction,
        StepKind::InvokeSubagent => PortalKind::InvokeSubagent,
        StepKind::GenerateImage => PortalKind::GenerateImage,
        StepKind::SearchWeb => PortalKind::SearchWeb,
        StepKind::ReadUrlContent => PortalKind::ReadUrlContent,
        StepKind::McpTool => PortalKind::McpTool,
        StepKind::CustomTool => PortalKind::CustomTool,
        StepKind::Finish => PortalKind::Finish,
        StepKind::Error => PortalKind::Error,
        StepKind::ToolConfirmationRequest => PortalKind::ToolConfirmationRequest,
        StepKind::QuestionsRequest => PortalKind::QuestionsRequest,
        _ => PortalKind::Other,
    }
}

async fn drain_cancel(client: &mut Client) -> antigravity_codes::Result<()> {
    let main = client.cascade_id().map(str::to_string);
    loop {
        let Some(event) = client.raw().next_event().await? else {
            return Ok(());
        };
        if let Some(OutputEventEvent::TrajectoryStateUpdate(update)) = event.into_event() {
            let is_main = main
                .as_deref()
                .is_none_or(|id| update.trajectory_id.as_deref() == Some(id));
            if is_main
                && matches!(
                    update.state,
                    Some(
                        TrajectoryStateUpdateState::Cancelled
                            | TrajectoryStateUpdateState::FullyIdle
                    )
                )
            {
                return Ok(());
            }
        }
    }
}

struct MetricsReport<'a> {
    model: &'a str,
    before: &'a antigravity_codes::protocol::UsageMetadata,
    after: &'a antigravity_codes::protocol::UsageMetadata,
    interrupted: bool,
    failure: Option<&'a str>,
}

fn emit_metrics(
    event_tx: &mpsc::UnboundedSender<IoEvent>,
    tracker: &mut TurnTracker,
    report: MetricsReport<'_>,
) {
    let outcome = TurnOutcome {
        agent_type: shared::AgentType::Antigravity,
        model: Some(report.model.to_string()),
        service_tier: report.after.service_tier.clone(),
        input_tokens: usage_delta(
            report.after.prompt_token_count,
            report.before.prompt_token_count,
        ),
        output_tokens: usage_delta(
            report.after.candidates_token_count,
            report.before.candidates_token_count,
        ),
        cache_creation_tokens: 0,
        cache_read_tokens: usage_delta(
            report.after.cached_content_token_count,
            report.before.cached_content_token_count,
        ),
        thinking_tokens: usage_delta(
            report.after.thoughts_token_count,
            report.before.thoughts_token_count,
        ),
        subagent_tokens: 0,
        context_snapshot_tokens: None,
        stop_reason: Some(if report.interrupted {
            "cancelled".to_string()
        } else if report.failure.is_some() {
            "error".to_string()
        } else {
            "completed".to_string()
        }),
        is_error: report.failure.is_some(),
        total_cost_usd: None,
        model_context_window: None,
    };
    if let Some(metrics) = tracker.finalize(Instant::now(), chrono::Utc::now(), outcome) {
        let _ = event_tx.send(IoEvent::TurnMetricsReady(Box::new(metrics)));
    }
}

fn usage_delta(after: Option<u64>, before: Option<u64>) -> i64 {
    i64::try_from(
        after
            .unwrap_or_default()
            .saturating_sub(before.unwrap_or_default()),
    )
    .unwrap_or(i64::MAX)
}

fn emit_error(event_tx: &mpsc::UnboundedSender<IoEvent>, message: String) {
    let envelope = ErrorEnvelope {
        frame_type: "error",
        message,
    };
    if let Ok(value) = serde_json::to_value(envelope) {
        let _ = event_tx.send(IoEvent::Classified(AgentOutput::Visible(value)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_flag_or_safe_default() {
        assert_eq!(configured_model(&[]), DEFAULT_MODEL);
        assert_eq!(
            configured_model(&["--model".into(), "gemini-2.5-pro".into()]),
            "gemini-2.5-pro"
        );
    }

    #[test]
    fn cumulative_usage_is_reported_as_a_per_turn_delta() {
        assert_eq!(usage_delta(Some(110), Some(100)), 10);
        assert_eq!(usage_delta(Some(90), Some(100)), 0);
    }

    #[test]
    fn every_failed_turn_emits_an_explicit_terminal() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        emit_failed_turn(&event_tx, "launch failed".to_string());

        assert!(matches!(
            event_rx.try_recv(),
            Ok(IoEvent::Classified(AgentOutput::Visible(_)))
        ));
        let Ok(IoEvent::Classified(AgentOutput::Visible(value))) = event_rx.try_recv() else {
            panic!("failed turn must end with a visible terminal");
        };
        let terminal: shared::antigravity::AntigravityTurnCompletedEnvelope =
            serde_json::from_value(value).unwrap();
        assert_eq!(
            terminal.status,
            shared::antigravity::AntigravityTurnStatus::Failed
        );
    }
}
