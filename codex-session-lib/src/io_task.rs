//! Background task for Codex sessions using the app-server JSON-RPC protocol.
//!
//! Spawns a persistent `codex app-server --listen stdio://` process and
//! manages multi-turn conversations via thread/turn lifecycle. Converts
//! JSON-RPC notifications into exec-format JSONL events for the frontend.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

use codex_codes::{
    methods, AppServerBuilder, AsyncClient as CodexAsyncClient, ThreadResumeParams,
    ThreadResumeResponse, ThreadStartParams, TurnInterruptParams, TurnStartParams, UserInput,
};
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use session_lib::{TurnOutcome, TurnTracker};
use tokio::sync::{mpsc, oneshot};

use crate::events::{
    to_raw_output, CodexUsageEvent, ThreadStartedEvent, TurnFailedEvent, UserEchoEvent,
};
use crate::handler::handle_codex_server_message;
use crate::helpers::{extract_prompt_text, parse_request_id};

type DeliveryAck = oneshot::Sender<Result<(), String>>;
type QueuedPrompt = (String, Option<DeliveryAck>);

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
    // the previous incarnation. codex-codes 0.129.3 doesn't expose a
    // typed `thread_resume` helper on AsyncClient yet — only
    // `thread_start` / `turn_*` / `thread_archive` — so we drive the
    // JSON-RPC call directly via the low-level `request<P, R>` primitive
    // using `methods::THREAD_RESUME`. Upstream issue filed against
    // meawoppl/rust-code-agent-sdks for the missing helper.
    let mut resumed_thread: Option<(String, Option<String>)> = None;
    if config.resume {
        if let Some(prior) = config.codex_thread_id.as_ref() {
            let resume_params = ThreadResumeParams {
                thread_id: prior.clone(),
                approval_policy: None,
                approvals_reviewer: None,
                base_instructions: None,
                config: None,
                // Leave cwd as None on resume — the app-server stored the
                // thread's working directory at first launch and we don't
                // want to override it from the launcher's POV.
                cwd: None,
                developer_instructions: None,
                model: None,
                model_provider: None,
                personality: None,
                sandbox: None,
                service_tier: None,
            };
            match client
                .request::<_, ThreadResumeResponse>(methods::THREAD_RESUME, &resume_params)
                .await
            {
                Ok(resp) => {
                    tracing::info!("Codex thread resumed: {}", prior);
                    let model =
                        non_empty_string(resp.model).or_else(|| configured_model_fallback.clone());
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
            // codex-codes 0.129.3 removed the `Default` impl on
            // `ThreadStartParams` (15 Option<...> fields, all
            // skip_serializing_if Some); the SDK's idiom for an "empty"
            // params is round-tripping through `{}` JSON. See codex-codes
            // 0.129.3 src/lib.rs:18-30 example.
            let thread_params = empty_thread_start_params();
            match client.thread_start(&thread_params).await {
                Ok(resp) => {
                    let model =
                        non_empty_string(resp.model).or_else(|| configured_model_fallback.clone());
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

    loop {
        if turn_active {
            // Turn is active: drain server messages AND accept approval responses.
            tokio::select! {
                result = client.next_message() => {
                    match result {
                        Ok(Some(msg)) => {
                            // Peek for turn lifecycle so we can name the
                            // turn on later `turn/interrupt` requests, and
                            // feed the per-turn metrics tracker. We can't
                            // observe these via the typed handler below
                            // because it consumes the message.
                            if let codex_codes::ServerMessage::Notification(notif) = &msg {
                                if let codex_codes::Notification::TurnStarted(p) = notif {
                                    current_turn_id = Some(p.turn.id.clone());
                                    current_turn_model = thread_model.clone();
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
                                    latest_token_usage = Some((
                                        p.turn_id.clone(),
                                        CodexUsageEvent {
                                            last: p.token_usage.last.clone(),
                                            total: p.token_usage.total.clone(),
                                            model_context_window: p.token_usage.model_context_window,
                                        },
                                    ));
                                    // Latch the per-turn breakdown so the
                                    // finalized `TurnMetrics` can carry
                                    // the token counts.
                                    current_turn_usage = Some(p.token_usage.last.clone());
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
                                if let Ok((method, params_opt)) =
                                    notif.clone().into_envelope()
                                {
                                    match method.as_str() {
                                        "turn/started" => {
                                            if let Some(turn_id) = params_opt
                                                .as_ref()
                                                .and_then(|p| p.get("turn"))
                                                .and_then(|t| t.get("id"))
                                                .and_then(|v| v.as_str())
                                            {
                                                current_turn_id = Some(turn_id.to_string());
                                            }
                                        }
                                        "turn/completed" => {
                                            current_turn_id = None;
                                        }
                                        _ => {}
                                    }
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
                                    cache_creation_tokens: 0,
                                    cache_read_tokens: usage
                                        .as_ref()
                                        .map(|u| u.cached_input_tokens)
                                        .unwrap_or(0),
                                    thinking_tokens: usage
                                        .as_ref()
                                        .map(|u| u.reasoning_output_tokens)
                                        .unwrap_or(0),
                                    stop_reason: Some(status.to_string()),
                                    is_error,
                                    total_cost_usd: None,
                                };
                                if let Some(metrics) = turn_tracker.finalize(
                                    Instant::now(),
                                    chrono::Utc::now(),
                                    outcome,
                                ) {
                                    if metrics.model.is_some() {
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
                                if let Some((prompt, delivered)) = queued_prompts.pop_front() {
                                    turn_tracker
                                        .start(Instant::now(), chrono::Utc::now());
                                    turn_active = start_codex_turn(
                                        &mut client,
                                        &thread_id,
                                        prompt,
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
                        IoCommand::CodexApproval { request_id, result } => {
                            let rid = parse_request_id(&request_id);
                            if let Err(e) = client.respond(rid, &result).await {
                                tracing::error!("Failed to send Codex approval: {}", e);
                            }
                        }
                        IoCommand::Input { input, delivered } => {
                            let prompt = extract_prompt_text(&input);
                            if !prompt.is_empty() {
                                tracing::info!(
                                    "Queued Codex input received during active turn ({} pending)",
                                    queued_prompts.len() + 1
                                );
                                queued_prompts.push_back((prompt, delivered));
                            } else if let Some(delivered) = delivered {
                                let _ = delivered.send(Ok(()));
                            }
                        }
                        IoCommand::PermissionResponse(_) => {}
                    }
                }
            }
        } else {
            // No active turn: wait for user input.
            match command_rx.recv().await {
                Some(IoCommand::Input { input, delivered }) => {
                    let prompt = extract_prompt_text(&input);
                    if prompt.is_empty() {
                        if let Some(delivered) = delivered {
                            let _ = delivered.send(Ok(()));
                        }
                        continue;
                    }
                    turn_tracker.start(Instant::now(), chrono::Utc::now());
                    turn_active =
                        start_codex_turn(&mut client, &thread_id, prompt, delivered, &event_tx)
                            .await;
                }
                Some(IoCommand::PermissionResponse(_)) => continue,
                Some(IoCommand::CodexApproval { .. }) => {
                    tracing::warn!("Codex approval response with no active turn");
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
    delivered: Option<DeliveryAck>,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) -> bool {
    // Codex's app-server protocol doesn't echo user input, so the frontend's
    // optimistic-send pending entry would never clear. Synthesize an echo here
    // in a shape that matches both the ClaudeOutput::User parse and the
    // frontend's content-based pending-match. Skip <system-reminder> wrappers
    // (portal reminder injection) because those shouldn't appear in transcript.
    if !prompt.starts_with("<system-reminder>") {
        let echo = UserEchoEvent::new(prompt);
        let _ = event_tx.send(IoEvent::RawOutput(to_raw_output(&echo)));
        return start_codex_turn_request(client, thread_id, echo.content, delivered, event_tx)
            .await;
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

    // codex-codes 0.129.3 generated TurnStartParams from its schema:
    // `reasoning_effort` is now `effort`, a new required `text_elements` field
    // landed on `UserInput::Text`, and the struct has many more Option fields
    // with no Default impl. Keep this construction explicit so SDK schema
    // changes become compile errors.
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

fn empty_thread_start_params() -> ThreadStartParams {
    ThreadStartParams {
        approval_policy: None,
        approvals_reviewer: None,
        base_instructions: None,
        config: None,
        cwd: None,
        developer_instructions: None,
        ephemeral: None,
        model: None,
        model_provider: None,
        personality: None,
        sandbox: None,
        service_name: None,
        service_tier: None,
        session_start_source: None,
        thread_source: None,
    }
}

fn turn_start_params(thread_id: &str, prompt: String) -> TurnStartParams {
    TurnStartParams {
        approval_policy: None,
        approvals_reviewer: None,
        cwd: None,
        effort: None,
        input: vec![UserInput::Text {
            text: prompt,
            text_elements: None,
        }],
        model: None,
        output_schema: None,
        personality: None,
        sandbox_policy: None,
        service_tier: None,
        summary: None,
        thread_id: thread_id.to_string(),
    }
}

fn non_empty_string(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("unknown") {
        None
    } else {
        Some(trimmed.to_string())
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
}
