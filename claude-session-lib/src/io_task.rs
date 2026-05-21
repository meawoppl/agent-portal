//! Background task for Claude sessions.
//!
//! Owns the [`ClaudeAsyncClient`] exclusively, draining stdout into
//! [`IoEvent`]s and writing [`IoCommand::Input`] / `PermissionResponse`
//! back to claude's stdin. Also implements the upstream-429 rate-limit
//! turn-retry state machine (see the `RATE_LIMIT_TEXT_PREFIX` comment
//! below).

use std::time::Duration;

use claude_codes::io::ContentBlock;
use claude_codes::{AsyncClient as ClaudeAsyncClient, ClaudeInput, ClaudeOutput};
use rand::Rng;
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use shared::PortalMessage;
use tokio::sync::mpsc;

/// Background task that owns the Claude process and handles all I/O.
///
/// This task:
/// - Continuously reads stdout to prevent OS pipe buffer overflow.
/// - Processes commands from the command channel to send input to Claude.
///
/// By owning the client exclusively, we avoid deadlocks that would occur
/// if we tried to share it between tasks with a mutex.
pub(crate) async fn claude_io_task(
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

    loop {
        tokio::select! {
            // Handle incoming commands (input to send to Claude).
            Some(cmd) = command_rx.recv() => {
                let result = match cmd {
                    IoCommand::Input(input) => {
                        // Each fresh user input gets its own retry budget.
                        rate_limit_attempts = 0;
                        current_turn_was_rate_limited = false;
                        let r = client.send(&input).await;
                        last_input = Some(input);
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
                        // auto-retry.
                        match &output {
                            ClaudeOutput::Assistant(asst) => {
                                let first_text = asst.message.content.iter().find_map(|b| {
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
                                }
                            }
                            ClaudeOutput::Result(r) if !r.is_error => {
                                // Successful turn — clear consecutive-failure state.
                                rate_limit_attempts = 0;
                                current_turn_was_rate_limited = false;
                            }
                            _ => {}
                        }

                        let is_rate_limit_terminator = matches!(
                            &output,
                            ClaudeOutput::Result(r) if r.is_error
                        ) && current_turn_was_rate_limited;

                        if event_tx.send(IoEvent::Output(Box::new(output))).is_err() {
                            // Receiver dropped, session ended.
                            break;
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
                                if let Err(e) = client.send(&input).await {
                                    let _ = event_tx.send(IoEvent::Error(
                                        SessionError::Agent(e.to_string()),
                                    ));
                                }
                            }
                            Some(cmd) = command_rx.recv() => {
                                match cmd {
                                    IoCommand::Input(new_input) => {
                                        // User typed something while we were waiting
                                        // — honor that, abandon the retry, and reset
                                        // the budget for the new prompt.
                                        rate_limit_attempts = 0;
                                        let r = client.send(&new_input).await;
                                        last_input = Some(new_input);
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
