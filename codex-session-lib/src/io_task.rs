//! Background task for Codex sessions using the app-server JSON-RPC protocol.
//!
//! Spawns a persistent `codex app-server --listen stdio://` process and
//! manages multi-turn conversations via thread/turn lifecycle. Converts
//! JSON-RPC notifications into exec-format JSONL events for the frontend.

use std::collections::VecDeque;
use std::path::Path;

use codex_codes::{
    AppServerBuilder, AsyncClient as CodexAsyncClient, ThreadStartParams, TurnInterruptParams,
    TurnStartParams, UserInput,
};
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::snapshot::SessionConfig;
use tokio::sync::mpsc;

use crate::handler::handle_codex_server_message;
use crate::helpers::{extract_prompt_text, parse_request_id};

/// Background task for Codex sessions.
pub(crate) async fn codex_io_task(
    config: SessionConfig,
    mut command_rx: mpsc::UnboundedReceiver<IoCommand>,
    event_tx: mpsc::UnboundedSender<IoEvent>,
) {
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

    // Start a thread (conversation session). codex-codes 0.129.3 removed
    // the `Default` impl on `ThreadStartParams` (15 Option<...> fields,
    // all skip_serializing_if Some); the SDK's idiom for an "empty"
    // params is round-tripping through `{}` JSON. See codex-codes
    // 0.129.3 src/lib.rs:18-30 example.
    let thread_params: ThreadStartParams = serde_json::from_value(serde_json::json!({}))
        .expect("empty JSON object is a valid ThreadStartParams");
    let thread_id = match client.thread_start(&thread_params).await {
        Ok(resp) => resp.thread.id.clone(),
        Err(e) => {
            let _ = event_tx.send(IoEvent::Error(SessionError::CommunicationError(format!(
                "Failed to start Codex thread: {}",
                e
            ))));
            return;
        }
    };

    tracing::info!("Codex thread started: {}", thread_id);

    let _ = event_tx.send(IoEvent::RawOutput(serde_json::json!({
        "type": "thread.started",
        "thread_id": &thread_id
    })));

    let mut turn_active = false;
    let mut queued_prompts: VecDeque<String> = VecDeque::new();
    // Track the turn currently being driven so we can name it on
    // `turn/interrupt` requests (codex-codes 0.129.3 made both
    // `thread_id` and `turn_id` required on `TurnInterruptParams`).
    // Set when a `turn/started` notification arrives; cleared when
    // `turn/completed` arrives.
    let mut current_turn_id: Option<String> = None;

    loop {
        if turn_active {
            // Turn is active: drain server messages AND accept approval responses.
            tokio::select! {
                result = client.next_message() => {
                    match result {
                        Ok(Some(msg)) => {
                            // Peek for turn lifecycle so we can name the
                            // turn on later `turn/interrupt` requests. We
                            // can't observe these via the typed handler
                            // below because it consumes the message.
                            if let codex_codes::ServerMessage::Notification(notif) = &msg {
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
                            let (ok, turn_ended) =
                                handle_codex_server_message(msg, &event_tx);
                            if turn_ended {
                                turn_active = false;
                                if let Some(prompt) = queued_prompts.pop_front() {
                                    turn_active =
                                        start_codex_turn(&mut client, &thread_id, prompt, &event_tx)
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
                            let _ = event_tx.send(IoEvent::RawOutput(serde_json::json!({
                                "type": "turn.failed",
                                "error": {
                                    "message": format!(
                                        "Codex emitted a frame this client could not parse ({}). \
                                         Interrupting the turn to recover — please retry.",
                                        error_message
                                    )
                                }
                            })));

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
                            let _ = event_tx.send(IoEvent::RawOutput(serde_json::json!({
                                "type": "portal",
                                "content": [{
                                    "type": "text",
                                    "text": portal_text
                                }]
                            })));

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
                        IoCommand::Input(input) => {
                            let prompt = extract_prompt_text(&input);
                            if !prompt.is_empty() {
                                tracing::info!(
                                    "Queued Codex input received during active turn ({} pending)",
                                    queued_prompts.len() + 1
                                );
                                queued_prompts.push_back(prompt);
                            }
                        }
                        IoCommand::PermissionResponse(_) => {}
                    }
                }
            }
        } else {
            // No active turn: wait for user input.
            match command_rx.recv().await {
                Some(IoCommand::Input(input)) => {
                    let prompt = extract_prompt_text(&input);
                    if prompt.is_empty() {
                        continue;
                    }
                    turn_active =
                        start_codex_turn(&mut client, &thread_id, prompt, &event_tx).await;
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
    event_tx: &mpsc::UnboundedSender<IoEvent>,
) -> bool {
    // Codex's app-server protocol doesn't echo user input, so the frontend's
    // optimistic-send pending entry would never clear. Synthesize an echo here
    // in a shape that matches both the ClaudeOutput::User parse and the
    // frontend's content-based pending-match. Skip <system-reminder> wrappers
    // (portal reminder injection) because those shouldn't appear in transcript.
    if !prompt.starts_with("<system-reminder>") {
        let echo = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": prompt}]
            },
            "content": prompt,
        });
        let _ = event_tx.send(IoEvent::RawOutput(echo));
    }

    tracing::info!("Starting Codex turn with {} chars", prompt.len());

    // codex-codes 0.129.3 generated TurnStartParams from its schema:
    // `reasoning_effort` is now `effort`, a new required `text_elements` field
    // landed on `UserInput::Text`, and the struct has many more Option fields
    // with no Default impl. We construct the explicit fields and `..` from an
    // empty JSON baseline to inherit Nones for the rest.
    let turn_params_base: TurnStartParams =
        serde_json::from_value(serde_json::json!({"threadId": thread_id, "input": []}))
            .expect("threadId + empty input is a valid TurnStartParams base");
    let turn_params = TurnStartParams {
        thread_id: thread_id.to_string(),
        input: vec![UserInput::Text {
            text: prompt,
            text_elements: None,
        }],
        ..turn_params_base
    };
    match client.turn_start(&turn_params).await {
        Ok(_) => true,
        Err(e) => {
            let _ = event_tx.send(IoEvent::RawOutput(serde_json::json!({
                "type": "turn.failed",
                "error": { "message": e.to_string() }
            })));
            false
        }
    }
}
