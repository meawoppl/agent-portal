//! Background task for Codex sessions using the app-server JSON-RPC protocol.
//!
//! Spawns a persistent `codex app-server --listen stdio://` process and
//! manages multi-turn conversations via thread/turn lifecycle. Converts
//! JSON-RPC notifications into exec-format JSONL events for the frontend.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::Instant;

use codex_codes::{
    AppServerBuilder, ApplyPatchApprovalResponse, AsyncClient as CodexAsyncClient,
    CommandExecutionRequestApprovalResponse, ExecCommandApprovalResponse, ServerMessage,
    ServerRequest, ThreadResumeParams, ThreadStartParams, TurnInterruptParams, TurnStartParams,
    UserInput,
};
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use session_lib::{PermissionDecision, TurnOutcome, TurnTracker};
use tokio::sync::{mpsc, oneshot};

use crate::events::{
    to_raw_output, CodexUsageEvent, ThreadStartedEvent, TurnFailedEvent, UserEchoEvent,
};
use crate::handler::handle_codex_server_message;
use crate::helpers::{format_request_id, parse_request_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexApprovalResponseKind {
    AcceptDecline,
    ExecApprovedDenied,
    ApplyPatchApprovedDenied,
}

/// Map a neutral [`PermissionDecision`] to Codex's approval response. Codex's
/// v2 command/file-change requests use `accept` / `decline`; older v1
/// exec/apply-patch requests use `approved` / `denied`.
fn codex_approval_result(
    decision: &PermissionDecision,
    kind: CodexApprovalResponseKind,
) -> serde_json::Value {
    let result = match kind {
        CodexApprovalResponseKind::AcceptDecline if decision.allow => {
            serde_json::to_value(CommandExecutionRequestApprovalResponse::accept())
        }
        CodexApprovalResponseKind::AcceptDecline => {
            serde_json::to_value(CommandExecutionRequestApprovalResponse::decline())
        }
        CodexApprovalResponseKind::ExecApprovedDenied if decision.allow => {
            serde_json::to_value(ExecCommandApprovalResponse::approved())
        }
        CodexApprovalResponseKind::ExecApprovedDenied => {
            serde_json::to_value(ExecCommandApprovalResponse::denied())
        }
        CodexApprovalResponseKind::ApplyPatchApprovedDenied if decision.allow => {
            serde_json::to_value(ApplyPatchApprovalResponse::approved())
        }
        CodexApprovalResponseKind::ApplyPatchApprovedDenied => {
            serde_json::to_value(ApplyPatchApprovalResponse::denied())
        }
    };
    result.unwrap_or(serde_json::Value::Null)
}

type DeliveryAck = oneshot::Sender<Result<(), String>>;
/// Queued user prompt: (agent-facing prompt text, delivery ack, optional typed
/// display event). The display event — when present — is an inter-agent
/// `PortalContent::AgentMessage` envelope the synthetic echo emits verbatim so
/// it renders as the provenance card instead of a raw user bubble (#inter-agent).
type QueuedPrompt = (String, Option<DeliveryAck>, Option<Box<serde_json::Value>>);

/// Background task for Codex sessions.
pub(crate) async fn codex_io_task(
    config: SessionConfig,
    mut command_rx: mpsc::UnboundedReceiver<IoCommand>,
    event_tx: mpsc::UnboundedSender<IoEvent>,
) {
    let session_id = config.session_id;
    let codex_path = config.claude_path.as_deref().unwrap_or(Path::new("codex"));

    // Parse the user-supplied extra_args into (a) global `-c key=value`
    // codex config overrides and (b) plain trailing args, because the two
    // need to land in different positions on the spawned command line:
    //
    //   codex  -c k=v  app-server --listen stdio://  <trailing args>
    //          ^^^^^^^                               ^^^^^^^^^^^^^^^
    //          global  subcommand + builder-managed  per-subcommand
    //
    // (codex-codes 0.129.2 added `config_override` / `extra_args` on
    // AppServerBuilder; see SDK #135.)
    let mut overrides: Vec<(String, String)> = Vec::new();
    let mut trailing: Vec<String> = Vec::new();
    let mut iter = config.extra_args.iter();
    while let Some(tok) = iter.next() {
        if tok == "-c" || tok == "--config" {
            if let Some(pair) = iter.next() {
                if let Some((k, v)) = pair.split_once('=') {
                    overrides.push((k.to_string(), v.to_string()));
                    continue;
                } else {
                    // `-c key` with no `=value` — pass through unchanged
                    // so codex's own error surfaces if it's malformed.
                    trailing.push(tok.clone());
                    trailing.push(pair.clone());
                    continue;
                }
            }
        }
        trailing.push(tok.clone());
    }

    let mut builder = AppServerBuilder::new()
        .command(codex_path)
        .working_directory(&config.working_directory);
    for (k, v) in &overrides {
        builder = builder.config_override(k, v);
    }
    if !trailing.is_empty() {
        builder = builder.extra_args(trailing.iter().cloned());
    }
    let configured_model_fallback = configured_codex_model(&overrides, &trailing);

    tracing::info!(
        "Starting Codex app-server: {} app-server --listen stdio:// \
         ({} config override(s), {} extra arg(s))",
        codex_path.display(),
        overrides.len(),
        trailing.len(),
    );

    let mut client = match CodexAsyncClient::start_with(builder).await {
        Ok(c) => c,
        Err(e) => {
            let _ = event_tx.send(IoEvent::Error(SessionError::CommunicationError(format!(
                "Failed to start Codex app-server: {}",
                e
            ))));
            return;
        }
    };

    // Re-attach to a prior codex thread when this is a resume launch
    // (`config.resume == true`) and the proxy persisted a thread id from
    // the previous incarnation.
    let mut resumed_thread: Option<(String, Option<String>)> = None;
    if config.resume {
        if let Some(prior) = config.codex_thread_id.as_ref() {
            let resume_params = ThreadResumeParams {
                thread_id: prior.clone(),
                // Leave cwd as None on resume — the app-server stored the
                // thread's working directory at first launch and we don't
                // want to override it from the launcher's POV.
                ..ThreadResumeParams::default()
            };
            match client.thread_resume(&resume_params).await {
                Ok(resp) => {
                    tracing::info!("Codex thread resumed: {}", prior);
                    let model = started_thread_model(resp.model, &configured_model_fallback);
                    resumed_thread = Some((prior.clone(), model));
                }
                Err(e) => {
                    // Fall through to `thread_start`. Common failure
                    // modes: the codex binary upgraded across a thread
                    // storage breaking change, or the thread was archived
                    // out of band. Don't lose the session — just strip
                    // server-side context. The transcript is rehydrated
                    // by the backend's message-replay path independently.
                    tracing::warn!(
                        "Codex thread/resume of {} failed: {} — falling back to fresh thread_start",
                        prior,
                        e
                    );
                }
            }
        } else {
            tracing::info!(
                "Codex resume requested but no prior thread_id is known — starting fresh thread"
            );
        }
    }

    let (thread_id, thread_model) = match resumed_thread {
        Some(thread) => thread,
        None => {
            let thread_params = ThreadStartParams::default();
            match client.thread_start(&thread_params).await {
                Ok(resp) => {
                    let model = started_thread_model(resp.model, &configured_model_fallback);
                    if model.is_none() {
                        tracing::warn!(
                            "Codex thread {} started without a detectable model; turn metrics will warn until model metadata is available",
                            resp.thread.id
                        );
                    }
                    (resp.thread.id.clone(), model)
                }
                Err(e) => {
                    let _ = event_tx.send(IoEvent::Error(SessionError::CommunicationError(
                        format!("Failed to start Codex thread: {}", e),
                    )));
                    return;
                }
            }
        }
    };

    tracing::info!("Codex thread ready: {}", thread_id);

    // Surface the thread id to the proxy so it can persist it for the
    // next resume of this session. Emitted once per spawn after either
    // thread_start or thread/resume returns.
    let _ = event_tx.send(IoEvent::CodexThreadId(thread_id.clone()));

    let _ = event_tx.send(IoEvent::RawOutput(to_raw_output(&ThreadStartedEvent::new(
        &thread_id,
    ))));

    let mut turn_active = false;
    let mut queued_prompts: VecDeque<QueuedPrompt> = VecDeque::new();
    // Track the turn currently being driven so we can name it on
    // `turn/interrupt` requests (codex-codes 0.129.3 made both
    // `thread_id` and `turn_id` required on `TurnInterruptParams`).
    // Set when a `turn/started` notification arrives; cleared when
    // `turn/completed` arrives.
    let mut current_turn_id: Option<String> = None;
    let mut latest_token_usage: Option<(String, CodexUsageEvent)> = None;
    // Per-turn metrics tracker. `start`ed inside `start_codex_turn`,
    // updated as `ItemStarted` / `ItemCompleted` notifications come in,
    // finalized on `TurnCompleted`.
    let mut turn_tracker = TurnTracker::new(session_id);
    // Latest `ThreadTokenUsageUpdated` capture for the current turn so we
    // can plug it into the finalized `TurnMetrics`.
    let mut current_turn_usage: Option<codex_codes::TokenUsageBreakdown> = None;
    let mut current_turn_model: Option<String> = None;
    let mut subagent_token_tracker = CodexSubagentTokenTracker::new(thread_id.clone());
    let mut pending_approval_response_kinds: HashMap<String, CodexApprovalResponseKind> =
        HashMap::new();

    loop {
        if turn_active {
            // Turn is active: drain server messages AND accept approval responses.
            tokio::select! {
                result = client.next_message() => {
                    match result {
                        Ok(Some(msg)) => {
                            if let Some((request_id, kind)) = approval_response_kind(&msg) {
                                pending_approval_response_kinds.insert(request_id, kind);
                            }

                            // Peek for turn lifecycle so we can name the
                            // turn on later `turn/interrupt` requests, and
                            // feed the per-turn metrics tracker. We can't
                            // observe these via the typed handler below
                            // because it consumes the message.
                            if let codex_codes::ServerMessage::Notification(notif) = &msg {
                                if let codex_codes::Notification::TurnStarted(p) = notif {
                                    current_turn_id = Some(p.turn.id.clone());
                                    current_turn_model = thread_model.clone();
                                    subagent_token_tracker.start_parent_turn();
                                }
                                if let codex_codes::Notification::ThreadStarted(p) = notif {
                                    subagent_token_tracker.observe_thread_started(
                                        &p.thread.id,
                                        p.thread.parent_thread_id.as_deref(),
                                    );
                                }
                                if let codex_codes::Notification::ModelRerouted(p) = notif {
                                    let matches_current_turn = current_turn_id
                                        .as_deref()
                                        .is_some_and(|turn_id| turn_id == p.turn_id);
                                    if p.thread_id == thread_id && matches_current_turn {
                                        current_turn_model =
                                            non_empty_string(p.to_model.clone()).or_else(|| {
                                                current_turn_model.clone().or_else(|| {
                                                    thread_model.clone()
                                                })
                                            });
                                    }
                                }
                                if let codex_codes::Notification::ThreadTokenUsageUpdated(p) =
                                    notif
                                {
                                    let is_current_main_turn = p.thread_id == thread_id
                                        && current_turn_id
                                            .as_deref()
                                            .is_some_and(|turn_id| turn_id == p.turn_id);
                                    if is_current_main_turn {
                                        latest_token_usage = Some((
                                            p.turn_id.clone(),
                                            CodexUsageEvent {
                                                last: p.token_usage.last.clone(),
                                                total: p.token_usage.total.clone(),
                                                model_context_window: p
                                                    .token_usage
                                                    .model_context_window,
                                            },
                                        ));
                                        // Latch the per-turn breakdown so the
                                        // finalized `TurnMetrics` can carry
                                        // the main-thread token counts.
                                        current_turn_usage = Some(p.token_usage.last.clone());
                                    } else {
                                        subagent_token_tracker
                                            .observe_token_usage(
                                                &p.thread_id,
                                                &p.turn_id,
                                                &p.token_usage.last,
                                            );
                                    }
                                }
                                // Content / tool frames for the metrics tracker.
                                // Pragmatic "first content frame" definition for
                                // codex: the first `ItemStarted` of an
                                // `agentMessage` item. Tool-style items
                                // (CommandExecution / FileChange / McpToolCall /
                                // DynamicToolCall / WebSearch) bump the tool
                                // count instead.
                                if let codex_codes::Notification::ItemStarted(p) = notif {
                                    use codex_codes::ThreadItem;
                                    match &p.item {
                                        ThreadItem::AgentMessage { .. }
                                        | ThreadItem::Reasoning { .. }
                                        | ThreadItem::Plan { .. } => {
                                            if turn_tracker.is_running() {
                                                turn_tracker
                                                    .record_content_frame(Instant::now());
                                            }
                                        }
                                        ThreadItem::CommandExecution { .. }
                                        | ThreadItem::FileChange { .. }
                                        | ThreadItem::McpToolCall { .. }
                                        | ThreadItem::DynamicToolCall { .. } => {
                                            turn_tracker.record_tool_call();
                                        }
                                        _ => {}
                                    }
                                }
                                if let Some(turn_id) = notif.turn_id() {
                                    current_turn_id = Some(turn_id.to_string());
                                }
                                if matches!(notif, codex_codes::Notification::TurnCompleted(_)) {
                                    current_turn_id = None;
                                }
                            }

                            // Finalize the per-turn metrics before handing
                            // `msg` off to `handle_codex_server_message`
                            // (which consumes it). `TurnCompleted` carries
                            // `turn.status` (Completed / Interrupted /
                            // Failed) which becomes our `stop_reason`.
                            if let codex_codes::ServerMessage::Notification(
                                codex_codes::Notification::TurnCompleted(p),
                            ) = &msg
                            {
                                let status = match p.turn.status {
                                    codex_codes::TurnStatus::Completed => "completed",
                                    codex_codes::TurnStatus::Interrupted => "interrupted",
                                    codex_codes::TurnStatus::Failed => "failed",
                                    codex_codes::TurnStatus::InProgress => "in_progress",
                                };
                                let is_error = !matches!(
                                    p.turn.status,
                                    codex_codes::TurnStatus::Completed
                                );
                                let usage = current_turn_usage.take();
                                let model = current_turn_model
                                    .clone()
                                    .or_else(|| thread_model.clone())
                                    .or_else(|| configured_model_fallback.clone());
                                let outcome = TurnOutcome {
                                    agent_type: shared::AgentType::Codex
                                        .as_str()
                                        .to_string(),
                                    model,
                                    service_tier: None,
                                    input_tokens: usage
                                        .as_ref()
                                        .map(|u| u.input_tokens)
                                        .unwrap_or(0),
                                    output_tokens: usage
                                        .as_ref()
                                        .map(|u| u.output_tokens)
                                        .unwrap_or(0),
                                    cache_creation_tokens: usage
                                        .as_ref()
                                        .and_then(|u| u.cache_write_input_tokens)
                                        .unwrap_or(0),
                                    cache_read_tokens: usage
                                        .as_ref()
                                        .map(|u| u.cached_input_tokens)
                                        .unwrap_or(0),
                                    thinking_tokens: usage
                                        .as_ref()
                                        .map(|u| u.reasoning_output_tokens)
                                        .unwrap_or(0),
                                    subagent_tokens: subagent_token_tracker
                                        .take_current_turn_tokens(),
                                    stop_reason: Some(status.to_string()),
                                    is_error,
                                    total_cost_usd: None,
                                };
                                if let Some(metrics) = turn_tracker.finalize(
                                    Instant::now(),
                                    chrono::Utc::now(),
                                    outcome,
                                ) {
                                    if metrics.has_known_model() {
                                        let _ = event_tx
                                            .send(IoEvent::TurnMetricsReady(Box::new(metrics)));
                                    } else {
                                        tracing::warn!(
                                            "Codex turn {} completed without model metadata for session {}; dropping turn metrics",
                                            p.turn.id,
                                            session_id
                                        );
                                    }
                                }
                                current_turn_model = None;
                                subagent_token_tracker.end_parent_turn();
                            }
                            let latest_usage_for_msg =
                                if let codex_codes::ServerMessage::Notification(
                                    codex_codes::Notification::TurnCompleted(p),
                                ) = &msg
                                {
                                    latest_token_usage
                                        .as_ref()
                                        .filter(|(turn_id, _)| turn_id == &p.turn.id)
                                        .map(|(_, usage)| usage)
                                } else {
                                    None
                                };
                            let (ok, turn_ended) =
                                handle_codex_server_message(
                                    msg,
                                    &event_tx,
                                    latest_usage_for_msg,
                                );
                            if turn_ended {
                                turn_active = false;
                                if let Some((prompt, delivered, display_event)) =
                                    queued_prompts.pop_front()
                                {
                                    turn_tracker
                                        .start(Instant::now(), chrono::Utc::now());
                                    turn_active = start_codex_turn(
                                        &mut client,
                                        &thread_id,
                                        prompt,
                                        display_event,
                                        delivered,
                                        &event_tx,
                                    )
                                    .await;
                                }
                            }
                            if !ok {
                                break;
                            }
                        }
                        Ok(None) => {
                            let _ = event_tx.send(IoEvent::Exited { code: 0 });
                            break;
                        }
                        Err(codex_codes::Error::Deserialization(parse_err)) => {
                            // Newer codex CLI versions emit frames whose
                            // typed param struct fails our bundled
                            // codex-codes schema (e.g. #703 — missing
                            // `callId` in 0.130 approval requests). The
                            // frame is lost.
                            //
                            // If the lost frame was a server→client
                            // request (`item/{commandExecution,fileChange}/requestApproval`),
                            // codex is now blocked on a reply we cannot
                            // send, because we never saw the request id.
                            // Result: the turn never completes,
                            // `turn_active` stays true, and subsequent user
                            // prompts wait behind the blocked turn.
                            //
                            // We can't recover the lost reply. Interrupt
                            // the turn so codex unblocks and emits
                            // `turn/completed` (Interrupted), which flips
                            // `turn_active` back to false through the
                            // normal path. Surface a `turn.failed` event
                            // + a portal message carrying the raw frame
                            // for upstream bug reports.
                            //
                            // codex-codes 0.129.3 (SDK #134) replaced the
                            // bare `Error::Json(serde_json::Error)` path
                            // with a structured `Error::Deserialization(ParseError)`
                            // that carries `raw_line`, `raw_json`, and
                            // `method` — so we can render the offending
                            // frame directly, no tracing-layer snooping.
                            let codex_codes::ParseError {
                                raw_line,
                                raw_json,
                                error_message,
                                method,
                            } = &parse_err;
                            tracing::warn!(
                                "Codex frame failed typed decode: {} (method={:?})",
                                error_message,
                                method
                            );
                            let event = TurnFailedEvent::new(format!(
                                "Codex emitted a frame this client could not parse ({}). \
                                 Interrupting the turn to recover \u{2014} please retry.",
                                error_message
                            ));
                            let _ = event_tx.send(IoEvent::RawOutput(to_raw_output(&event)));

                            let rendered = raw_json
                                .as_ref()
                                .and_then(|v| serde_json::to_string_pretty(v).ok())
                                .unwrap_or_else(|| raw_line.clone());
                            let portal_text = format!(
                                "**Codex frame failed to decode** \u{2014} `{}`{}\n\n\
                                 Paste the block below if reporting upstream \
                                 ([`meawoppl/rust-code-agent-sdks`](https://github.com/meawoppl/rust-code-agent-sdks/issues)):\n\n\
                                 ```json\n{}\n```",
                                error_message,
                                method.as_deref().map(|m| format!(" (`{}`)", m)).unwrap_or_default(),
                                rendered
                            );
                            let _ = event_tx.send(IoEvent::RawOutput(
                                shared::PortalMessage::text(portal_text).to_json(),
                            ));

                            let interrupt_params = TurnInterruptParams {
                                thread_id: thread_id.clone(),
                                turn_id: current_turn_id.clone().unwrap_or_default(),
                            };
                            if let Err(e) = client.turn_interrupt(&interrupt_params).await {
                                tracing::error!(
                                    "turn_interrupt after decode failure failed: {} \
                                     \u{2014} forcing turn_active=false to unwedge",
                                    e
                                );
                                turn_active = false;
                            }
                            continue;
                        }
                        Err(e) => {
                            let _ = event_tx.send(IoEvent::Error(
                                SessionError::CommunicationError(e.to_string()),
                            ));
                            break;
                        }
                    }
                }
                Some(cmd) = command_rx.recv() => {
                    match cmd {
                        IoCommand::Permission {
                            request_id,
                            decision,
                        } => {
                            let rid = parse_request_id(&request_id);
                            let kind = pending_approval_response_kinds
                                .remove(&request_id)
                                .unwrap_or(CodexApprovalResponseKind::AcceptDecline);
                            let result = codex_approval_result(&decision, kind);
                            if let Err(e) = client.respond(rid, &result).await {
                                tracing::error!("Failed to send Codex approval: {}", e);
                            }
                        }
                        IoCommand::UserInput {
                            text,
                            delivered,
                            display_event,
                        } => {
                            if !text.is_empty() {
                                tracing::info!(
                                    "Queued Codex input received during active turn ({} pending)",
                                    queued_prompts.len() + 1
                                );
                                queued_prompts.push_back((text, delivered, display_event));
                            } else if let Some(delivered) = delivered {
                                let _ = delivered.send(Ok(()));
                            }
                        }
                        IoCommand::Interrupt => {
                            // Cancel the in-flight turn. `turn/interrupt` needs
                            // both thread_id and turn_id; skip if we haven't seen
                            // a `turn/started` yet (nothing to name/cancel).
                            if let Some(turn_id) = current_turn_id.clone() {
                                let params = TurnInterruptParams {
                                    thread_id: thread_id.clone(),
                                    turn_id,
                                };
                                if let Err(e) = client.turn_interrupt(&params).await {
                                    tracing::error!("Failed to interrupt Codex turn: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        } else {
            // No active turn: wait for user input.
            match command_rx.recv().await {
                Some(IoCommand::UserInput {
                    text,
                    delivered,
                    display_event,
                }) => {
                    if text.is_empty() {
                        if let Some(delivered) = delivered {
                            let _ = delivered.send(Ok(()));
                        }
                        continue;
                    }
                    turn_tracker.start(Instant::now(), chrono::Utc::now());
                    turn_active = start_codex_turn(
                        &mut client,
                        &thread_id,
                        text,
                        display_event,
                        delivered,
                        &event_tx,
                    )
                    .await;
                }
                Some(IoCommand::Permission { .. }) => {
                    tracing::warn!("Codex approval response with no active turn");
                }
                Some(IoCommand::Interrupt) => {
                    // No active turn — nothing to interrupt.
                }
                None => {
                    let _ = event_tx.send(IoEvent::Exited { code: 0 });
                    break;
                }
            }
        }
    }
}

async fn start_codex_turn(
    client: &mut CodexAsyncClient,
    thread_id: &str,
    prompt: String,
    display_event: Option<Box<serde_json::Value>>,
    delivered: Option<DeliveryAck>,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) -> bool {
    // Codex's app-server protocol doesn't echo user input, so the frontend's
    // optimistic-send pending entry would never clear. Synthesize a transcript
    // entry here. Skip <system-reminder> wrappers (portal reminder injection)
    // which shouldn't appear in transcript. The agent always receives `prompt`
    // (the agent-facing text); only the *display* differs.
    if !prompt.starts_with("<system-reminder>") {
        match display_event {
            // Inter-agent message: emit the typed PortalContent::AgentMessage
            // event verbatim so it renders as the provenance card (matching the
            // claude echo-replacement path), not a raw "You" bubble.
            Some(event) => {
                let _ = event_tx.send(IoEvent::RawOutput(*event));
                return start_codex_turn_request(client, thread_id, prompt, delivered, event_tx)
                    .await;
            }
            // Plain user input: synthesize a user echo in a shape that matches
            // ClaudeOutput::User parse + the frontend's content-based pending-match.
            None => {
                let echo = UserEchoEvent::new(prompt);
                let _ = event_tx.send(IoEvent::RawOutput(to_raw_output(&echo)));
                return start_codex_turn_request(
                    client,
                    thread_id,
                    echo.content,
                    delivered,
                    event_tx,
                )
                .await;
            }
        }
    }

    start_codex_turn_request(client, thread_id, prompt, delivered, event_tx).await
}

async fn start_codex_turn_request(
    client: &mut CodexAsyncClient,
    thread_id: &str,
    prompt: String,
    delivered: Option<DeliveryAck>,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) -> bool {
    tracing::info!("Starting Codex turn with {} chars", prompt.len());

    let turn_params = turn_start_params(thread_id, prompt);
    match client.turn_start(&turn_params).await {
        Ok(_) => {
            if let Some(delivered) = delivered {
                let _ = delivered.send(Ok(()));
            }
            true
        }
        Err(e) => {
            let message = e.to_string();
            let event = TurnFailedEvent::new(e.to_string());
            let _ = event_tx.send(IoEvent::RawOutput(to_raw_output(&event)));
            if let Some(delivered) = delivered {
                let _ = delivered.send(Err(message));
            }
            false
        }
    }
}

fn turn_start_params(thread_id: &str, prompt: String) -> TurnStartParams {
    TurnStartParams {
        input: vec![UserInput::Text {
            text: prompt,
            text_elements: None,
        }],
        thread_id: thread_id.to_string(),
        ..TurnStartParams::default()
    }
}

fn approval_response_kind(msg: &ServerMessage) -> Option<(String, CodexApprovalResponseKind)> {
    let ServerMessage::Request { id, request } = msg else {
        return None;
    };
    let kind = match request {
        ServerRequest::CmdExecApproval(_) | ServerRequest::FileChangeApproval(_) => {
            CodexApprovalResponseKind::AcceptDecline
        }
        ServerRequest::ExecCommandApproval(_) => CodexApprovalResponseKind::ExecApprovedDenied,
        ServerRequest::ApplyPatchApproval(_) => CodexApprovalResponseKind::ApplyPatchApprovedDenied,
        _ => return None,
    };
    Some((format_request_id(id), kind))
}

fn non_empty_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("unknown") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Resolve the model a freshly resumed/started thread reports, falling back to
/// the configured model when the server didn't surface one. Shared by the
/// `thread_resume` and `thread_start` arms so the fallback policy stays in one
/// place. (The per-turn current→thread→configured resolution is a distinct
/// precedence and intentionally not folded in here.)
fn started_thread_model(
    resp_model: String,
    configured_fallback: &Option<String>,
) -> Option<String> {
    non_empty_string(resp_model).or_else(|| configured_fallback.clone())
}

#[derive(Debug, Clone)]
struct CodexSubagentTokenTracker {
    parent_thread_id: String,
    subagent_thread_ids: HashSet<String>,
    parent_turn_active: bool,
    current_turn_tokens_by_child_turn: HashMap<(String, String), i64>,
}

impl CodexSubagentTokenTracker {
    fn new(parent_thread_id: String) -> Self {
        Self {
            parent_thread_id,
            subagent_thread_ids: HashSet::new(),
            parent_turn_active: false,
            current_turn_tokens_by_child_turn: HashMap::new(),
        }
    }

    fn start_parent_turn(&mut self) {
        self.parent_turn_active = true;
        self.current_turn_tokens_by_child_turn.clear();
    }

    fn end_parent_turn(&mut self) {
        self.parent_turn_active = false;
    }

    fn observe_thread_started(&mut self, thread_id: &str, parent_thread_id: Option<&str>) {
        if parent_thread_id == Some(self.parent_thread_id.as_str()) {
            self.subagent_thread_ids.insert(thread_id.to_string());
        }
    }

    fn observe_token_usage(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        usage: &codex_codes::TokenUsageBreakdown,
    ) {
        if !self.parent_turn_active || !self.subagent_thread_ids.contains(thread_id) {
            return;
        }
        self.current_turn_tokens_by_child_turn.insert(
            (thread_id.to_string(), turn_id.to_string()),
            token_breakdown_total(usage),
        );
    }

    fn take_current_turn_tokens(&mut self) -> i64 {
        let total = self.current_turn_tokens_by_child_turn.values().sum();
        self.current_turn_tokens_by_child_turn.clear();
        total
    }
}

fn token_breakdown_total(usage: &codex_codes::TokenUsageBreakdown) -> i64 {
    if usage.total_tokens > 0 {
        usage.total_tokens
    } else {
        usage.input_tokens
            + usage.cached_input_tokens
            + usage.cache_write_input_tokens.unwrap_or(0)
            + usage.output_tokens
            + usage.reasoning_output_tokens
    }
}

fn configured_codex_model(overrides: &[(String, String)], trailing: &[String]) -> Option<String> {
    overrides
        .iter()
        .find_map(|(key, value)| {
            key.eq_ignore_ascii_case("model")
                .then(|| non_empty_string(value.clone()))
                .flatten()
        })
        .or_else(|| trailing_codex_model(trailing))
}

fn trailing_codex_model(args: &[String]) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--model" || arg == "-m" {
            if let Some(value) = iter.next() {
                if let Some(model) = non_empty_string(value.clone()) {
                    return Some(model);
                }
            }
            continue;
        }

        if let Some(value) = arg.strip_prefix("--model=") {
            if let Some(model) = non_empty_string(value.to_string()) {
                return Some(model);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    /// Neutral `PermissionDecision` → Codex approval response. Ported from the
    /// deleted `respond_permission` codex branch — approval is a coarse
    /// accept/decline, so only `allow` matters.
    #[test]
    fn codex_approval_result_maps_allow_to_accept_decline() {
        let accept = codex_approval_result(
            &PermissionDecision {
                allow: true,
                ..Default::default()
            },
            CodexApprovalResponseKind::AcceptDecline,
        );
        assert_eq!(accept["decision"], serde_json::json!("accept"));

        let decline = codex_approval_result(
            &PermissionDecision {
                allow: false,
                reason: Some("nope".to_string()),
                ..Default::default()
            },
            CodexApprovalResponseKind::AcceptDecline,
        );
        assert_eq!(decline["decision"], serde_json::json!("decline"));
    }

    #[test]
    fn codex_approval_result_maps_exec_and_patch_to_approved_denied() {
        let approve = PermissionDecision {
            allow: true,
            ..Default::default()
        };
        let deny = PermissionDecision {
            allow: false,
            ..Default::default()
        };

        let exec_approved =
            codex_approval_result(&approve, CodexApprovalResponseKind::ExecApprovedDenied);
        assert_eq!(exec_approved["decision"], serde_json::json!("approved"));
        let exec_denied =
            codex_approval_result(&deny, CodexApprovalResponseKind::ExecApprovedDenied);
        assert_eq!(exec_denied["decision"], serde_json::json!("denied"));

        let patch_approved = codex_approval_result(
            &approve,
            CodexApprovalResponseKind::ApplyPatchApprovedDenied,
        );
        assert_eq!(patch_approved["decision"], serde_json::json!("approved"));
        let patch_denied =
            codex_approval_result(&deny, CodexApprovalResponseKind::ApplyPatchApprovedDenied);
        assert_eq!(patch_denied["decision"], serde_json::json!("denied"));
    }

    #[test]
    fn approval_response_kind_distinguishes_codex_protocol_families() {
        let exec_req: codex_codes::ExecCommandApprovalParams =
            serde_json::from_value(serde_json::json!({
                "callId": "c",
                "conversationId": "cv",
                "command": ["ls"],
                "cwd": "/tmp"
            }))
            .unwrap();
        let exec = ServerMessage::Request {
            id: codex_codes::RequestId::Integer(7),
            request: ServerRequest::ExecCommandApproval(exec_req),
        };
        assert_eq!(
            approval_response_kind(&exec),
            Some((
                "7".to_string(),
                CodexApprovalResponseKind::ExecApprovedDenied
            ))
        );

        let cmd_req: codex_codes::CommandExecutionRequestApprovalParams =
            serde_json::from_value(serde_json::json!({
                "itemId": "i",
                "command": "ls",
                "cwd": "/tmp",
                "startedAtMs": 0,
                "threadId": "t",
                "turnId": "tu"
            }))
            .unwrap();
        let cmd = ServerMessage::Request {
            id: codex_codes::RequestId::Integer(8),
            request: ServerRequest::CmdExecApproval(cmd_req),
        };
        assert_eq!(
            approval_response_kind(&cmd),
            Some(("8".to_string(), CodexApprovalResponseKind::AcceptDecline))
        );
    }

    #[test]
    fn configured_codex_model_prefers_config_override() {
        let overrides = vec![("model".to_string(), "gpt-5.4".to_string())];
        let trailing = strings(&["--model", "gpt-5.3"]);

        assert_eq!(
            configured_codex_model(&overrides, &trailing).as_deref(),
            Some("gpt-5.4")
        );
    }

    #[test]
    fn configured_codex_model_reads_long_flag() {
        let overrides = Vec::new();
        let trailing = strings(&["--model", "gpt-5.3-codex"]);

        assert_eq!(
            configured_codex_model(&overrides, &trailing).as_deref(),
            Some("gpt-5.3-codex")
        );
    }

    #[test]
    fn configured_codex_model_reads_equals_flag() {
        let overrides = Vec::new();
        let trailing = strings(&["--model=gpt-5.3-codex-spark"]);

        assert_eq!(
            configured_codex_model(&overrides, &trailing).as_deref(),
            Some("gpt-5.3-codex-spark")
        );
    }

    #[test]
    fn configured_codex_model_ignores_blank_values() {
        let overrides = vec![("model".to_string(), " ".to_string())];
        let trailing = strings(&["--model", ""]);

        assert!(configured_codex_model(&overrides, &trailing).is_none());
    }

    #[test]
    fn configured_codex_model_ignores_unknown_values() {
        let overrides = vec![("model".to_string(), "unknown".to_string())];
        let trailing = strings(&[]);

        assert!(configured_codex_model(&overrides, &trailing).is_none());
    }

    fn usage(
        total: i64,
        input: i64,
        cached: i64,
        output: i64,
        reasoning: i64,
    ) -> codex_codes::TokenUsageBreakdown {
        codex_codes::TokenUsageBreakdown {
            input_tokens: input,
            cached_input_tokens: cached,
            cache_write_input_tokens: Some(0),
            output_tokens: output,
            reasoning_output_tokens: reasoning,
            total_tokens: total,
        }
    }

    #[test]
    fn subagent_tracker_counts_child_thread_usage_during_parent_turn() {
        let mut tracker = CodexSubagentTokenTracker::new("parent-thread".to_string());
        tracker.observe_thread_started("child-thread", Some("parent-thread"));

        tracker.start_parent_turn();
        tracker.observe_token_usage("child-thread", "child-turn-1", &usage(37, 0, 0, 0, 0));
        tracker.observe_token_usage("child-thread", "child-turn-2", &usage(11, 0, 0, 0, 0));

        assert_eq!(tracker.take_current_turn_tokens(), 48);
        tracker.end_parent_turn();
    }

    #[test]
    fn subagent_tracker_uses_last_usage_per_child_turn() {
        let mut tracker = CodexSubagentTokenTracker::new("parent-thread".to_string());
        tracker.observe_thread_started("child-thread", Some("parent-thread"));

        tracker.start_parent_turn();
        tracker.observe_token_usage("child-thread", "child-turn", &usage(37, 0, 0, 0, 0));
        tracker.observe_token_usage("child-thread", "child-turn", &usage(41, 0, 0, 0, 0));

        assert_eq!(tracker.take_current_turn_tokens(), 41);
    }

    #[test]
    fn subagent_tracker_ignores_parent_sibling_and_inactive_usage() {
        let mut tracker = CodexSubagentTokenTracker::new("parent-thread".to_string());
        tracker.observe_thread_started("child-thread", Some("parent-thread"));
        tracker.observe_thread_started("sibling-thread", Some("other-parent"));

        tracker.observe_token_usage("child-thread", "child-turn", &usage(100, 0, 0, 0, 0));
        tracker.start_parent_turn();
        tracker.observe_token_usage("parent-thread", "parent-turn", &usage(100, 0, 0, 0, 0));
        tracker.observe_token_usage("sibling-thread", "sibling-turn", &usage(100, 0, 0, 0, 0));
        tracker.observe_token_usage("unknown-thread", "unknown-turn", &usage(100, 0, 0, 0, 0));

        assert_eq!(tracker.take_current_turn_tokens(), 0);
    }

    #[test]
    fn subagent_tracker_resets_on_new_parent_turn() {
        let mut tracker = CodexSubagentTokenTracker::new("parent-thread".to_string());
        tracker.observe_thread_started("child-thread", Some("parent-thread"));

        tracker.start_parent_turn();
        tracker.observe_token_usage("child-thread", "child-turn", &usage(7, 0, 0, 0, 0));
        assert_eq!(tracker.take_current_turn_tokens(), 7);
        tracker.end_parent_turn();

        tracker.start_parent_turn();
        assert_eq!(tracker.take_current_turn_tokens(), 0);
    }

    #[test]
    fn token_breakdown_total_falls_back_when_total_tokens_missing() {
        assert_eq!(token_breakdown_total(&usage(0, 10, 3, 5, 2)), 20);
    }
}
