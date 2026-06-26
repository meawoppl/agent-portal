//! Background task for Claude sessions.
//!
//! Owns the [`ClaudeAsyncClient`] exclusively, draining stdout into
//! [`IoEvent`]s and writing [`IoCommand::Input`] / `PermissionResponse`
//! back to claude's stdin. Also implements the upstream-429 rate-limit
//! turn-retry state machine (see the `RATE_LIMIT_TEXT_PREFIX` comment
//! below).

use std::time::{Duration, Instant};

use chrono::{NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use claude_codes::io::ContentBlock;
use claude_codes::{AsyncClient as ClaudeAsyncClient, ClaudeInput, ClaudeOutput};
use rand::Rng;
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::{TurnOutcome, TurnTracker};
use shared::PortalMessage;
use tokio::sync::mpsc;
use uuid::Uuid;

const SESSION_LIMIT_TEXT: &str = "You've hit your session limit";
const SESSION_LIMIT_CONTINUE_PROMPT: &str =
    "Continue from where you stopped before the Claude session limit was reached.";

/// Background task that owns the Claude process and handles all I/O.
///
/// This task:
/// - Continuously reads stdout to prevent OS pipe buffer overflow.
/// - Processes commands from the command channel to send input to Claude.
///
/// By owning the client exclusively, we avoid deadlocks that would occur
/// if we tried to share it between tasks with a mutex.
pub(crate) async fn claude_io_task(
    session_id: Uuid,
    mut client: ClaudeAsyncClient,
    mut command_rx: mpsc::UnboundedReceiver<IoCommand>,
    event_tx: mpsc::UnboundedSender<IoEvent>,
) {
    // Detector for the upstream-429 turn shape. When Anthropic's API rate-
    // limits a request, `claude --print` doesn't fail — it streams an
    // assistant message whose first text block starts with this prefix, then
    // emits a Result with is_error=true. We retry that turn for the user
    // with full-jitter exponential backoff.
    const RATE_LIMIT_TEXT_PREFIX: &str = "API Error: Server is temporarily limiting requests";
    const MAX_RATE_LIMIT_RETRIES: u32 = 30;
    const RATE_LIMIT_BACKOFF_CAP_SECS: u64 = 60;

    // Take stderr so we can read it if Claude exits unexpectedly.
    let mut stderr_reader = client.take_stderr();

    // The most recent user input we sent — kept so we can re-issue it on
    // a rate-limited turn without bothering the user.
    let mut last_input: Option<ClaudeInput> = None;
    // Consecutive rate-limited turns. Resets on success, on a new user
    // input, and after a max-out give-up.
    let mut rate_limit_attempts: u32 = 0;
    // Set while an assistant frame in the current turn matched the
    // rate-limit text prefix. Latched until the turn's Result frame so we
    // can classify the whole turn on its terminator.
    let mut current_turn_was_rate_limited = false;
    let mut pending_session_limit: Option<(String, String)> = None;

    // Per-turn performance metrics tracker. `start`ed on each user input,
    // `record_content_frame`d on assistant/text frames, finalized on
    // `ClaudeOutput::Result`. See `session_lib::turn_tracker`.
    let mut turn_tracker = TurnTracker::new(session_id);
    // Model + service tier observed for the current turn. Picked up from
    // `ClaudeOutput::System(Init)` (issued at session start) and the
    // assistant message's `model` field (per-message); the tracker doesn't
    // know either on its own.
    let mut current_model: Option<String> = None;
    let mut current_service_tier: Option<String> = None;

    loop {
        tokio::select! {
            // Handle incoming commands (input to send to Claude).
            Some(cmd) = command_rx.recv() => {
                let result = match cmd {
                    // `display_event` is for agents that don't echo (Codex);
                    // claude echoes its input and the proxy swaps the typed
                    // event in via output_forwarder, so it's ignored here.
                    IoCommand::Input {
                        input,
                        delivered,
                        display_event: _,
                    } => {
                        // Each fresh user input gets its own retry budget.
                        rate_limit_attempts = 0;
                        current_turn_was_rate_limited = false;
                        pending_session_limit = None;
                        // Begin per-turn metrics capture. Wall-clock UTC is
                        // chrono::Utc::now(); the monotonic instant is the
                        // anchor for TTFT / total / gap durations.
                        turn_tracker.start(Instant::now(), chrono::Utc::now());
                        let r = client.send(&input).await;
                        last_input = Some(input);
                        if let Some(delivered) = delivered {
                            let _ = delivered.send(r.as_ref().map(|_| ()).map_err(|e| e.to_string()));
                        }
                        r
                    }
                    IoCommand::PermissionResponse(response) => {
                        client.send_control_response(response).await
                    }
                    IoCommand::CodexApproval { .. } => continue,
                };
                if let Err(e) = result {
                    let _ = event_tx.send(IoEvent::Error(SessionError::Agent(e.to_string())));
                }
            }

            // Read output from Claude.
            result = client.receive() => {
                match result {
                    Ok(output) => {
                        // Classify the frame before forwarding so we can
                        // decide whether the turn's terminator triggers an
                        // auto-retry, and feed the per-turn metrics tracker.
                        match &output {
                            ClaudeOutput::System(sys) => {
                                // Init frames carry the model name — latch
                                // it so we can stamp the turn metrics.
                                if let Some(init) = sys.as_init() {
                                    if init.model.is_some() {
                                        current_model = init.model.clone();
                                    }
                                }
                            }
                            ClaudeOutput::Assistant(asst) => {
                                // Any assistant frame counts as a content
                                // frame for TTFT / inter-token-gap purposes.
                                // The Result frame carries the *terminator*
                                // and is excluded from "content".
                                if turn_tracker.is_running() {
                                    turn_tracker.record_content_frame(Instant::now());
                                }
                                // Refresh per-turn model + service tier from
                                // the most recent assistant frame.
                                if !asst.message.model.is_empty() {
                                    current_model = Some(asst.message.model.clone());
                                }
                                if let Some(usage) = &asst.message.usage {
                                    if let Some(tier) = &usage.service_tier {
                                        current_service_tier = Some(tier.clone());
                                    }
                                }
                                // Count tool-use blocks for the turn.
                                for block in &asst.message.content {
                                    if matches!(
                                        block,
                                        ContentBlock::ToolUse(_)
                                            | ContentBlock::ServerToolUse(_)
                                            | ContentBlock::McpToolUse(_)
                                    ) {
                                        turn_tracker.record_tool_call();
                                    }
                                }
                                let first_text =
                                    asst.message.content.iter().find_map(|b| {
                                        if let ContentBlock::Text(t) = b {
                                            Some(t.text.as_str())
                                        } else {
                                            None
                                        }
                                    });
                                if let Some(text) = first_text {
                                    if text.starts_with(RATE_LIMIT_TEXT_PREFIX) {
                                        current_turn_was_rate_limited = true;
                                    }
                                    if let Some(reset_at) = parse_session_limit_reset(text) {
                                        pending_session_limit =
                                            Some((reset_at, text.to_string()));
                                    }
                                }
                            }
                            ClaudeOutput::Result(r) if !r.is_error => {
                                // Successful turn — clear consecutive-failure state.
                                rate_limit_attempts = 0;
                                current_turn_was_rate_limited = false;
                                pending_session_limit = None;
                            }
                            _ => {}
                        }

                        // Finalize the per-turn metrics on the terminator.
                        // Done before the auto-retry branch so the metrics
                        // row reflects this turn, not the eventual retried
                        // turn (which will get its own row when `start` runs
                        // again).
                        if let ClaudeOutput::Result(ref r) = output {
                            let usage = r.usage.as_ref();
                            let model = current_model.clone();
                            let outcome = TurnOutcome {
                                agent_type: shared::AgentType::Claude.as_str().to_string(),
                                model,
                                service_tier: current_service_tier.clone().or_else(|| {
                                    usage
                                        .map(|u| u.service_tier.clone())
                                        .filter(|s| !s.is_empty())
                                }),
                                input_tokens: usage
                                    .map(|u| u.input_tokens as i64)
                                    .unwrap_or(0),
                                output_tokens: usage
                                    .map(|u| u.output_tokens as i64)
                                    .unwrap_or(0),
                                cache_creation_tokens: usage
                                    .map(|u| u.cache_creation_input_tokens as i64)
                                    .unwrap_or(0),
                                cache_read_tokens: usage
                                    .map(|u| u.cache_read_input_tokens as i64)
                                    .unwrap_or(0),
                                thinking_tokens: 0,
                                // The Claude binary tracks subagent (`Task` /
                                // sidechain) tokens and surfaces them as a
                                // distinct `<subagent_tokens>` line in its
                                // result `<usage>` envelope, but the
                                // stream-json `usage` shape the proxy receives
                                // exposes no subagent field (claude-codes
                                // `UsageInfo` carries only input/output/cache/
                                // service_tier). So we can't attribute
                                // subagent tokens on the Claude path yet.
                                // TODO(SDK #169): once claude-codes exposes
                                // the result `<usage>` subagent rollup,
                                // populate this instead of reporting 0.
                                subagent_tokens: 0,
                                stop_reason: r.stop_reason.clone(),
                                is_error: r.is_error,
                                total_cost_usd: Some(r.total_cost_usd),
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
                                        "Claude result completed without model metadata for session {}; dropping turn metrics",
                                        session_id
                                    );
                                }
                            }
                        }

                        let is_rate_limit_terminator = matches!(
                            &output,
                            ClaudeOutput::Result(r) if r.is_error
                        ) && current_turn_was_rate_limited;
                        let is_session_limit_terminator = matches!(
                            &output,
                            ClaudeOutput::Result(r) if r.is_error
                        ) && pending_session_limit.is_some();

                        if event_tx.send(IoEvent::Output(Box::new(output))).is_err() {
                            // Receiver dropped, session ended.
                            break;
                        }

                        if is_session_limit_terminator {
                            if let Some((reset_at, source_message)) = pending_session_limit.take() {
                                let _ = event_tx.send(IoEvent::SessionLimitReached {
                                    session_id,
                                    reset_at,
                                    source_message,
                                    prompt: SESSION_LIMIT_CONTINUE_PROMPT.to_string(),
                                });
                            }
                        }

                        if !is_rate_limit_terminator {
                            continue;
                        }

                        current_turn_was_rate_limited = false;
                        let Some(input) = last_input.clone() else {
                            continue;
                        };

                        if rate_limit_attempts >= MAX_RATE_LIMIT_RETRIES {
                            let _ = event_tx.send(IoEvent::RawOutput(
                                PortalMessage::text(format!(
                                    "Rate-limited by upstream API {} times in a row \u{2014} not retrying. Send your message again to try once more.",
                                    rate_limit_attempts
                                ))
                                .to_json(),
                            ));
                            rate_limit_attempts = 0;
                            continue;
                        }

                        rate_limit_attempts += 1;
                        // Full-jitter exponential backoff:
                        //   delay ∈ [0, min(cap, 2^attempt)] seconds.
                        // attempt=1 -> [0,2]; 2 -> [0,4]; 3 -> [0,8]; 4 -> [0,16];
                        // the cap saturates by attempt 5 but we max-out before that.
                        let exp_cap = 2u64
                            .saturating_pow(rate_limit_attempts)
                            .min(RATE_LIMIT_BACKOFF_CAP_SECS);
                        let delay_secs = {
                            let mut rng = rand::thread_rng();
                            rng.gen_range(0.0..=exp_cap as f64)
                        };
                        let _ = event_tx.send(IoEvent::RawOutput(
                            PortalMessage::text(format!(
                                "Rate-limited by upstream API. Retrying in {:.1}s (attempt {}/{}).",
                                delay_secs, rate_limit_attempts, MAX_RATE_LIMIT_RETRIES
                            ))
                            .to_json(),
                        ));

                        // Wait for the backoff window, but let a new user
                        // input cancel the retry and run normally.
                        let sleep = tokio::time::sleep(Duration::from_secs_f64(delay_secs));
                        tokio::pin!(sleep);
                        tokio::select! {
                            _ = &mut sleep => {
                                // Auto-retry of the same user input — fresh
                                // turn from a metrics standpoint, but tag
                                // the new turn with a stream-restart count
                                // so the dashboard can spot retried turns.
                                turn_tracker.start(Instant::now(), chrono::Utc::now());
                                turn_tracker.record_stream_restart();
                                if let Err(e) = client.send(&input).await {
                                    let _ = event_tx.send(IoEvent::Error(
                                        SessionError::Agent(e.to_string()),
                                    ));
                                }
                            }
                            Some(cmd) = command_rx.recv() => {
                                match cmd {
                                    IoCommand::Input {
                                        input: new_input,
                                        delivered,
                                        display_event: _,
                                    } => {
                                        // User typed something while we were waiting
                                        // — honor that, abandon the retry, and reset
                                        // the budget for the new prompt.
                                        rate_limit_attempts = 0;
                                        pending_session_limit = None;
                                        turn_tracker.start(Instant::now(), chrono::Utc::now());
                                        let r = client.send(&new_input).await;
                                        last_input = Some(new_input);
                                        if let Some(delivered) = delivered {
                                            let _ = delivered
                                                .send(r.as_ref().map(|_| ()).map_err(|e| e.to_string()));
                                        }
                                        if let Err(e) = r {
                                            let _ = event_tx.send(IoEvent::Error(
                                                SessionError::Agent(e.to_string()),
                                            ));
                                        }
                                    }
                                    IoCommand::PermissionResponse(response) => {
                                        if let Err(e) =
                                            client.send_control_response(response).await
                                        {
                                            let _ = event_tx.send(IoEvent::Error(
                                                SessionError::Agent(e.to_string()),
                                            ));
                                        }
                                    }
                                    IoCommand::CodexApproval { .. } => {}
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Deserialization errors are non-fatal: the CLI sent a message
                        // we don't understand yet. Wrap in a portal message so the
                        // frontend renders it cleanly, and keep the session alive.
                        if let claude_codes::Error::Deserialization(ref parse_err) = e {
                            tracing::warn!(
                                "Unparsable message from CLI (session continues): {}",
                                parse_err.error_message
                            );

                            let raw_display = parse_err
                                .raw_json
                                .as_ref()
                                .and_then(|v| serde_json::to_string_pretty(v).ok())
                                .unwrap_or_else(|| parse_err.error_message.clone());

                            let portal_json = PortalMessage::text(format!(
                                "Received an unrecognized message from the agent CLI. \
                                The session will continue normally.\n\n\
                                {}\n\n\
                                If you believe this is a bug, please report it at:\n\
                                https://github.com/meawoppl/rust-code-agent-sdks/issues",
                                raw_display
                            ))
                            .to_json();
                            let _ = event_tx.send(IoEvent::RawOutput(portal_json));
                            continue;
                        }

                        let err_str = e.to_string();
                        if err_str.contains("exit") || err_str.contains("terminated") {
                            let _ = event_tx.send(IoEvent::Exited { code: 1 });
                            break;
                        }
                        // Connection closed is fatal.
                        if matches!(e, claude_codes::Error::ConnectionClosed) {
                            let _ = event_tx.send(IoEvent::Exited { code: 1 });
                            break;
                        }
                        // Try to read stderr for more context.
                        let stderr_output = read_stderr(&mut stderr_reader).await;
                        let enriched_error = if let Some(stderr) = stderr_output {
                            SessionError::CommunicationError(format!(
                                "{}\nClaude stderr: {}",
                                e, stderr
                            ))
                        } else {
                            SessionError::Agent(e.to_string())
                        };
                        if event_tx.send(IoEvent::Error(enriched_error)).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn parse_session_limit_reset(text: &str) -> Option<String> {
    if !text.contains(SESSION_LIMIT_TEXT) {
        return None;
    }

    let after_resets = text.split("resets ").nth(1)?;
    let open = after_resets.find('(')?;
    let close = after_resets[open + 1..].find(')')? + open + 1;
    let time_text = after_resets[..open].trim();
    let tz_text = after_resets[open + 1..close].trim();

    let tz: Tz = tz_text.parse().ok()?;
    let reset_time = parse_limit_time(time_text)?;
    let now_utc = Utc::now();
    let now_local = now_utc.with_timezone(&tz);
    let mut date = now_local.date_naive();

    let local_dt = match tz.from_local_datetime(&date.and_time(reset_time)) {
        chrono::LocalResult::Single(dt) => dt,
        chrono::LocalResult::Ambiguous(earlier, _) => earlier,
        chrono::LocalResult::None => {
            date = date.succ_opt()?;
            match tz.from_local_datetime(&date.and_time(reset_time)) {
                chrono::LocalResult::Single(dt) => dt,
                chrono::LocalResult::Ambiguous(earlier, _) => earlier,
                chrono::LocalResult::None => return None,
            }
        }
    };

    let reset_utc = if local_dt.with_timezone(&Utc) <= now_utc {
        let next_date = date.succ_opt()?;
        match tz.from_local_datetime(&next_date.and_time(reset_time)) {
            chrono::LocalResult::Single(dt) => dt,
            chrono::LocalResult::Ambiguous(earlier, _) => earlier,
            chrono::LocalResult::None => return None,
        }
    } else {
        local_dt
    };

    Some(reset_utc.with_timezone(&Utc).to_rfc3339())
}

fn parse_limit_time(input: &str) -> Option<NaiveTime> {
    let lower = input.trim().to_ascii_lowercase();
    for fmt in ["%-I:%M%P", "%-I:%M %P", "%-I%P", "%-I %P"] {
        if let Ok(time) = NaiveTime::parse_from_str(&lower, fmt) {
            return Some(time);
        }
    }
    None
}

/// Read available stderr output from the Claude process.
async fn read_stderr(
    stderr_reader: &mut Option<tokio::io::BufReader<tokio::process::ChildStderr>>,
) -> Option<String> {
    use tokio::io::AsyncReadExt;

    let reader = stderr_reader.as_mut()?;
    let mut buf = Vec::with_capacity(4096);

    // Use a short timeout — stderr may have data already buffered.
    match tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_to_end(&mut buf),
    )
    .await
    {
        Ok(Ok(_)) if !buf.is_empty() => {
            let text = String::from_utf8_lossy(&buf).trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        _ => None,
    }
}
