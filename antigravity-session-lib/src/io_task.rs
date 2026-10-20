use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use antigravity_codes::protocol::{
    CallHookResponse, EmptyResult, InputEvent, OutputEventEvent, PolicyConfig,
    PolicyConfigWorkspaceContainment, PolicyDecisionResponse, PolicyEvaluationOutcome, StepUpdate,
    ToolConfirmation, ToolResponse, TrajectoryStateUpdateState, UsageMetadata,
    UserQuestionsResponse,
};
use antigravity_codes::steps::{Step, StepAssembler, StepKind};
use antigravity_codes::{HarnessOptions, ModelBuilder, RawClient};
use serde::Serialize;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use session_lib::turn_tracker::{TurnOutcome, TurnTracker};
use shared::LocalFrame;
use tokio::sync::{mpsc, oneshot};

use crate::DEFAULT_MODEL;

type Delivery = Option<oneshot::Sender<Result<(), String>>>;

struct PendingInput {
    text: String,
    delivered: Delivery,
    display_event: Option<Box<serde_json::Value>>,
}

enum TurnResult {
    Complete { queued: VecDeque<PendingInput> },
    Shutdown { completed: oneshot::Sender<()> },
}

pub async fn antigravity_io_task(
    config: SessionConfig,
    mut command_rx: mpsc::UnboundedReceiver<IoCommand>,
    event_tx: mpsc::UnboundedSender<IoEvent>,
) {
    let _ = event_tx.send(IoEvent::AgentStarted { pid: None });
    let mut queued = VecDeque::new();
    let mut turn_tracker = TurnTracker::new(config.session_id);

    loop {
        let pending = match queued.pop_front() {
            Some(pending) => pending,
            None => match command_rx.recv().await {
                Some(IoCommand::UserInput {
                    text,
                    delivered,
                    display_event,
                    ..
                }) => PendingInput {
                    text,
                    delivered,
                    display_event,
                },
                Some(IoCommand::Permission { request_id, .. }) => {
                    tracing::debug!(
                        request_id = %request_id,
                        "antigravity: ignoring portal permission response; localharness uses its own request frames"
                    );
                    continue;
                }
                Some(IoCommand::Interrupt) => {
                    tracing::debug!("antigravity: interrupt outside an active turn");
                    continue;
                }
                Some(IoCommand::Shutdown { completed }) => {
                    let _ = completed.send(());
                    break;
                }
                None => break,
            },
        };

        if pending.text.is_empty() {
            if let Some(delivered) = pending.delivered {
                let _ = delivered.send(Ok(()));
            }
            continue;
        }

        turn_tracker.start(Instant::now(), chrono::Utc::now());
        match run_turn(
            &config,
            pending,
            &mut command_rx,
            &mut turn_tracker,
            &event_tx,
        )
        .await
        {
            TurnResult::Complete {
                queued: mut newly_queued,
            } => {
                append_after_existing(&mut queued, &mut newly_queued);
            }
            TurnResult::Shutdown { completed } => {
                let _ = completed.send(());
                break;
            }
        }
    }
}

async fn run_turn(
    config: &SessionConfig,
    pending: PendingInput,
    command_rx: &mut mpsc::UnboundedReceiver<IoCommand>,
    turn_tracker: &mut TurnTracker,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) -> TurnResult {
    let mut queued = VecDeque::new();
    let model = selected_model(config);
    let options = match build_harness_options(config, &model) {
        Ok(options) => options,
        Err(message) => {
            fail_delivery(pending.delivered, &message);
            emit_error(event_tx, message);
            return TurnResult::Complete { queued };
        }
    };

    let mut client = match RawClient::launch(options).await {
        Ok(client) => client,
        Err(e) => {
            let message = format!("Antigravity launch failed: {e}");
            fail_delivery(pending.delivered, &message);
            emit_error(event_tx, message);
            return TurnResult::Complete { queued };
        }
    };

    let cascade_id = client
        .cascade_id()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| config.session_id.to_string());
    let mut assembler = StepAssembler::new(Some(cascade_id.clone()));
    warm_assembler(&mut assembler, &client);

    if let Err(e) = client.send(&InputEvent::user(&pending.text)).await {
        let message = format!("Antigravity input failed: {e}");
        fail_delivery(pending.delivered, &message);
        emit_error(event_tx, message);
        let _ = client.shutdown().await;
        return TurnResult::Complete { queued };
    }

    emit_user_input_display(&pending.text, pending.display_event, event_tx);
    if let Some(delivered) = pending.delivered {
        let _ = delivered.send(Ok(()));
    }

    let mut latest_usage = client.initialize_response().cumulative_usage.clone();
    let mut stop_reason = None;
    let mut is_error = false;

    loop {
        tokio::select! {
            command = command_rx.recv() => {
                match command {
                    Some(IoCommand::UserInput {
                        text,
                        delivered,
                        display_event,
                        ..
                    }) => {
                        queued.push_back(PendingInput { text, delivered, display_event });
                    }
                    Some(IoCommand::Permission { request_id, .. }) => {
                        tracing::debug!(
                            request_id = %request_id,
                            "antigravity: ignoring portal permission response while turn is active"
                        );
                    }
                    Some(IoCommand::Interrupt) => {
                        let _ = client.send(&InputEvent::halt()).await;
                    }
                    Some(IoCommand::Shutdown { completed }) => {
                        let _ = client.send(&InputEvent::halt()).await;
                        let _ = client.shutdown().await;
                        return TurnResult::Shutdown { completed };
                    }
                    None => {
                        let _ = client.shutdown().await;
                        return TurnResult::Complete { queued };
                    }
                }
            }
            event = client.next_event() => {
                match event {
                    Ok(Some(event)) => {
                        match event.into_event() {
                            Some(OutputEventEvent::StepUpdate(update)) => {
                                if let Some(step) = handle_step_update(
                                    update,
                                    &mut assembler,
                                    &mut client,
                                    turn_tracker,
                                    event_tx,
                                ).await {
                                    if matches!(step.kind, StepKind::Error) || step.error_message.is_some() {
                                        is_error = true;
                                        stop_reason.get_or_insert_with(|| "step_error".to_string());
                                    }
                                }
                            }
                            Some(OutputEventEvent::TrajectoryStateUpdate(update)) => {
                                let is_main = update
                                    .trajectory_id
                                    .as_deref()
                                    .is_some_and(|id| assembler.is_main(id));
                                emit_antigravity_event(
                                    event_tx,
                                    value_or_null(AntigravityTrajectoryEvent {
                                        frame_type: "antigravity_event",
                                        event: "trajectory_state_update",
                                        trajectory_id: update.trajectory_id.clone(),
                                        state: update.state.as_ref().map(|s| s.as_str().to_string()),
                                        is_main,
                                        error: update.error.clone(),
                                        error_code: update.error_code.clone(),
                                        stop_reason: update.stop_reason.as_ref().map(|s| s.as_str().to_string()),
                                    }),
                                );
                                if is_main {
                                    match update.state {
                                        Some(TrajectoryStateUpdateState::FullyIdle) => {
                                            stop_reason = update.stop_reason.map(|s| s.as_str().to_string());
                                            break;
                                        }
                                        Some(TrajectoryStateUpdateState::Cancelled) => {
                                            stop_reason = Some("cancelled".to_string());
                                            is_error = true;
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            Some(OutputEventEvent::UsageUpdate(update)) => {
                                if let Some(total) = update.total {
                                    latest_usage = Some(total);
                                }
                            }
                            Some(OutputEventEvent::PolicyDecisionRequest(request)) => {
                                let response = PolicyDecisionResponse {
                                    request_id: request.request_id,
                                    outcome: Some(PolicyEvaluationOutcome::Deny),
                                    deny_reason: Some(
                                        "Agent Portal Antigravity preview is read-only".to_string(),
                                    ),
                                };
                                let _ = client.send(&InputEvent::policy_response(response)).await;
                            }
                            Some(OutputEventEvent::ToolCall(call)) => {
                                let id = call.id.unwrap_or_default();
                                let response = ToolResponse::error(
                                    id,
                                    "Agent Portal Antigravity preview does not execute client-side tools",
                                );
                                let _ = client.send(&InputEvent::tool_response(response)).await;
                            }
                            Some(OutputEventEvent::CallHookRequest(request)) => {
                                let response = CallHookResponse {
                                    request_id: request.request_id,
                                    empty_result: Some(EmptyResult::default()),
                                    ..Default::default()
                                };
                                let _ = client.send(&InputEvent::hook_response(response)).await;
                            }
                            Some(OutputEventEvent::SessionEndResponse(_)) => {
                                break;
                            }
                            Some(OutputEventEvent::InitializeConversationResponse(_)) | None => {}
                        }
                    }
                    Ok(None) => {
                        stop_reason.get_or_insert_with(|| "socket_closed".to_string());
                        is_error = true;
                        break;
                    }
                    Err(e) => {
                        let message = format!("Antigravity stream failed: {e}");
                        emit_error(event_tx, message);
                        stop_reason.get_or_insert_with(|| "stream_error".to_string());
                        is_error = true;
                        break;
                    }
                }
            }
        }
    }

    let _ = client.shutdown().await;
    finalize_turn_metrics(
        turn_tracker,
        &model,
        latest_usage.as_ref(),
        stop_reason.clone(),
        is_error,
        event_tx,
    );
    emit_antigravity_event(
        event_tx,
        value_or_null(AntigravityTurnCompleted {
            frame_type: "antigravity_turn_completed",
            model,
            stop_reason,
            is_error,
        }),
    );

    TurnResult::Complete { queued }
}

fn append_after_existing(
    existing: &mut VecDeque<PendingInput>,
    incoming: &mut VecDeque<PendingInput>,
) {
    existing.append(incoming);
}

async fn handle_step_update(
    update: StepUpdate,
    assembler: &mut StepAssembler,
    client: &mut RawClient,
    turn_tracker: &mut TurnTracker,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) -> Option<Step> {
    let step = assembler.ingest(update);
    if step.text().is_some() && step.user_facing_text().is_some() {
        turn_tracker.record_content_frame(Instant::now());
    }
    if !matches!(
        step.kind,
        StepKind::Message | StepKind::Finish | StepKind::Compaction | StepKind::Error
    ) {
        turn_tracker.record_tool_call();
    }
    if step.kind == StepKind::QuestionsRequest {
        let response = UserQuestionsResponse {
            cancelled: Some(true),
            trajectory_id: Some(step.trajectory_id.clone()),
            step_index: Some(step.step_index),
            ..Default::default()
        };
        let _ = client
            .send(&InputEvent {
                question_response: Some(response),
                ..Default::default()
            })
            .await;
    }
    if step.kind == StepKind::ToolConfirmationRequest {
        let confirmation = ToolConfirmation {
            trajectory_id: Some(step.trajectory_id.clone()),
            step_index: Some(step.step_index),
            accepted: Some(false),
        };
        let _ = client
            .send(&InputEvent {
                tool_confirmation: Some(confirmation),
                ..Default::default()
            })
            .await;
    }
    let kind = format!("{:?}", step.kind);
    let state = step.state.as_str().to_string();
    let source = step.source.as_str().to_string();
    let target = step.target.as_str().to_string();
    let is_main = assembler.is_main(&step.trajectory_id);
    let is_final = step.is_final();
    emit_antigravity_event(
        event_tx,
        value_or_null(AntigravityStepEvent {
            frame_type: "antigravity_event",
            event: "step_update",
            trajectory_id: step.trajectory_id.clone(),
            step_index: step.step_index,
            step_kind: kind,
            state,
            source,
            target,
            is_main,
            is_final,
            text: step.text.clone(),
            text_delta: step.text_delta.clone(),
            thinking: step.thinking.clone(),
            error_message: step.error_message.clone(),
            update: step.update.clone(),
        }),
    );
    Some(step)
}

fn build_harness_options(config: &SessionConfig, model: &str) -> Result<HarnessOptions, String> {
    let storage_dir = storage_dir(config.session_id);
    std::fs::create_dir_all(&storage_dir).map_err(|e| {
        format!(
            "Failed to create Antigravity storage directory {}: {e}",
            storage_dir.display()
        )
    })?;

    let mut options = HarnessOptions::new()
        .storage_directory(&storage_dir)
        .app_data_dir(storage_dir.join("app-data"))
        .workspace(&config.working_directory)
        .cascade_id(config.session_id.to_string())
        .policy(PolicyConfig {
            workspace_containment: Some(PolicyConfigWorkspaceContainment::Enabled),
            ..Default::default()
        });

    options = match credentials_model(model) {
        Some(model_builder) => options.model(model_builder),
        None => {
            return Err(
                "Antigravity needs GEMINI_API_KEY or ANTIGRAVITY_VERTEX_PROJECT/ANTIGRAVITY_VERTEX_LOCATION"
                    .to_string(),
            )
        }
    };

    Ok(options)
}

fn credentials_model(model: &str) -> Option<ModelBuilder> {
    if let Ok(key) = std::env::var("GEMINI_API_KEY") {
        if shared::strings::is_non_blank(&key) {
            return Some(ModelBuilder::gemini(model, key));
        }
    }

    let project = std::env::var("ANTIGRAVITY_VERTEX_PROJECT").ok()?;
    let location = std::env::var("ANTIGRAVITY_VERTEX_LOCATION").ok()?;
    if shared::strings::is_non_blank(&project) && shared::strings::is_non_blank(&location) {
        Some(ModelBuilder::vertex(model, project, location))
    } else {
        None
    }
}

fn selected_model(config: &SessionConfig) -> String {
    if let Some(model) = model_arg(&config.extra_args) {
        return model;
    }
    std::env::var("ANTIGRAVITY_MODEL")
        .ok()
        .filter(|m| shared::strings::is_non_blank(m))
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

fn model_arg(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        if arg == "--model" {
            return it
                .next()
                .filter(|value| shared::strings::is_non_blank(value))
                .cloned();
        }
        if let Some(value) = arg.strip_prefix("--model=") {
            if shared::strings::is_non_blank(value) {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn storage_dir(session_id: uuid::Uuid) -> PathBuf {
    session_lib::paths::config_dir()
        .join("antigravity")
        .join("sessions")
        .join(session_id.to_string())
}

fn warm_assembler(assembler: &mut StepAssembler, client: &RawClient) {
    for update in client.initialize_response().history.iter().cloned() {
        assembler.ingest(update);
    }
}

fn emit_user_input_display(
    prompt: &str,
    display_event: Option<Box<serde_json::Value>>,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) {
    if display_event.is_none() && prompt.starts_with("<system-reminder>") {
        return;
    }
    let value = match display_event {
        Some(event) => *event,
        None => serde_json::from_str(&LocalFrame::user(prompt).to_json()).unwrap_or_else(|_| {
            value_or_null(LocalUserFallback {
                frame_type: "user",
                content: prompt,
            })
        }),
    };
    let _ = event_tx.send(IoEvent::RawOutput(value));
}

fn fail_delivery(delivered: Delivery, message: &str) {
    if let Some(delivered) = delivered {
        let _ = delivered.send(Err(message.to_string()));
    }
}

fn emit_error(event_tx: &mpsc::UnboundedSender<IoEvent>, message: String) {
    let value =
        serde_json::from_str(&LocalFrame::error(message.clone()).to_json()).unwrap_or_else(|_| {
            value_or_null(LocalErrorFallback {
                frame_type: "error",
                message,
            })
        });
    let _ = event_tx.send(IoEvent::RawOutput(value));
}

fn emit_antigravity_event(event_tx: &mpsc::UnboundedSender<IoEvent>, value: serde_json::Value) {
    let _ = event_tx.send(IoEvent::RawOutput(value));
}

fn value_or_null<T: Serialize>(payload: T) -> serde_json::Value {
    serde_json::to_value(payload).unwrap_or(serde_json::Value::Null)
}

#[derive(Serialize)]
struct AntigravityTrajectoryEvent {
    #[serde(rename = "type")]
    frame_type: &'static str,
    event: &'static str,
    trajectory_id: Option<String>,
    state: Option<String>,
    is_main: bool,
    error: Option<String>,
    error_code: Option<String>,
    stop_reason: Option<String>,
}

#[derive(Serialize)]
struct AntigravityTurnCompleted {
    #[serde(rename = "type")]
    frame_type: &'static str,
    model: String,
    stop_reason: Option<String>,
    is_error: bool,
}

#[derive(Serialize)]
struct AntigravityStepEvent {
    #[serde(rename = "type")]
    frame_type: &'static str,
    event: &'static str,
    trajectory_id: String,
    step_index: u32,
    step_kind: String,
    state: String,
    source: String,
    target: String,
    is_main: bool,
    is_final: bool,
    text: String,
    text_delta: String,
    thinking: String,
    error_message: Option<String>,
    update: StepUpdate,
}

#[derive(Serialize)]
struct LocalUserFallback<'a> {
    #[serde(rename = "type")]
    frame_type: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct LocalErrorFallback {
    #[serde(rename = "type")]
    frame_type: &'static str,
    message: String,
}

fn finalize_turn_metrics(
    turn_tracker: &mut TurnTracker,
    model: &str,
    usage: Option<&UsageMetadata>,
    stop_reason: Option<String>,
    is_error: bool,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) {
    let usage = usage.cloned().unwrap_or_default();
    let input_tokens = i64_from_u64(usage.prompt_token_count);
    let cache_read_tokens = i64_from_u64(usage.cached_content_token_count);
    let output_tokens = i64_from_u64(usage.candidates_token_count);
    let thinking_tokens = i64_from_u64(usage.thoughts_token_count);
    let outcome = TurnOutcome {
        agent_type: shared::AgentType::Antigravity,
        model: Some(model.to_string()),
        service_tier: usage.service_tier,
        input_tokens,
        output_tokens,
        cache_creation_tokens: 0,
        cache_read_tokens,
        thinking_tokens,
        subagent_tokens: 0,
        context_snapshot_tokens: Some(input_tokens),
        stop_reason,
        is_error,
        total_cost_usd: None,
        model_context_window: None,
    };
    if let Some(metrics) = turn_tracker.finalize(Instant::now(), chrono::Utc::now(), outcome) {
        if metrics.has_known_model() {
            let _ = event_tx.send(IoEvent::TurnMetricsReady(Box::new(metrics)));
        }
    }
}

fn i64_from_u64(value: Option<u64>) -> i64 {
    value.unwrap_or_default().min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use antigravity_codes::protocol::{InitializeConversationResponse, StepUpdateState};

    #[test]
    fn model_arg_accepts_space_and_equals_forms() {
        assert_eq!(
            model_arg(&["--model".into(), "gemini-2.5-flash".into()]).as_deref(),
            Some("gemini-2.5-flash")
        );
        assert_eq!(
            model_arg(&["--model=gemini-2.5-pro".into()]).as_deref(),
            Some("gemini-2.5-pro")
        );
    }

    #[test]
    fn storage_dir_is_session_scoped() {
        let id = uuid::Uuid::new_v4();
        let path = storage_dir(id);
        assert!(path.ends_with(id.to_string()));
        assert!(path.to_string_lossy().contains("antigravity"));
    }

    #[test]
    fn history_is_warmed_without_creating_visible_events() {
        let history = vec![StepUpdate {
            trajectory_id: Some("main".into()),
            step_index: Some(0),
            state: Some(StepUpdateState::Done),
            text: Some("already persisted".into()),
            ..Default::default()
        }];
        let mut assembler = StepAssembler::new(Some("main".into()));
        for update in history {
            assembler.ingest(update);
        }

        let step = assembler.ingest(StepUpdate {
            trajectory_id: Some("main".into()),
            step_index: Some(1),
            state: Some(StepUpdateState::Active),
            text_delta: Some("new".into()),
            ..Default::default()
        });
        assert_eq!(step.text, "new");
        assert_eq!(assembler.main_trajectory(), Some("main"));
    }

    #[test]
    fn mid_turn_inputs_do_not_jump_existing_queue() {
        let mut existing = VecDeque::from([pending_text("second")]);
        let mut incoming = VecDeque::from([pending_text("third")]);

        append_after_existing(&mut existing, &mut incoming);

        let labels: Vec<_> = existing.into_iter().map(|pending| pending.text).collect();
        assert_eq!(labels, ["second", "third"]);
        assert!(incoming.is_empty());
    }

    #[test]
    fn usage_maps_to_metrics_without_double_counting_cached_input() {
        let mut tracker = TurnTracker::new(uuid::Uuid::new_v4());
        tracker.start(Instant::now(), chrono::Utc::now());
        let (tx, mut rx) = mpsc::unbounded_channel();
        finalize_turn_metrics(
            &mut tracker,
            DEFAULT_MODEL,
            Some(&UsageMetadata {
                prompt_token_count: Some(100),
                cached_content_token_count: Some(30),
                candidates_token_count: Some(20),
                thoughts_token_count: Some(7),
                service_tier: Some("test-tier".into()),
                ..Default::default()
            }),
            Some("done".into()),
            false,
            &tx,
        );
        let Some(IoEvent::TurnMetricsReady(metrics)) = rx.try_recv().ok() else {
            panic!("metrics should be emitted");
        };
        assert_eq!(metrics.agent_type, shared::AgentType::Antigravity);
        assert_eq!(metrics.input_tokens, 100);
        assert_eq!(metrics.cache_read_tokens, 30);
        assert_eq!(metrics.output_tokens, 20);
        assert_eq!(metrics.thinking_tokens, 7);
        assert_eq!(metrics.context_snapshot_tokens, Some(100));
    }

    #[test]
    fn read_only_policy_is_part_of_harness_options() {
        let dir = tempfile::tempdir().unwrap();
        let config = SessionConfig {
            session_id: uuid::Uuid::new_v4(),
            working_directory: dir.path().to_path_buf(),
            extra_args: vec!["--model".into(), DEFAULT_MODEL.into()],
            ..Default::default()
        };
        std::env::set_var("GEMINI_API_KEY", "test-key");
        let options = build_harness_options(&config, DEFAULT_MODEL).unwrap();
        std::env::remove_var("GEMINI_API_KEY");
        let harness_config = options.config();
        assert_eq!(harness_config.workspaces.len(), 1);
        assert_eq!(
            harness_config
                .policy_config
                .as_ref()
                .and_then(|p| p.workspace_containment.clone()),
            Some(PolicyConfigWorkspaceContainment::Enabled)
        );
        assert!(harness_config.harness_side_tools.is_some());
    }

    #[test]
    fn initialize_response_history_shape_stays_expected() {
        let init = InitializeConversationResponse {
            cascade_id: Some("main".into()),
            history: vec![StepUpdate::default()],
            ..Default::default()
        };
        assert_eq!(init.cascade_id.as_deref(), Some("main"));
        assert_eq!(init.history.len(), 1);
    }

    fn pending_text(text: &str) -> PendingInput {
        PendingInput {
            text: text.to_string(),
            delivered: None,
            display_event: None,
        }
    }
}
