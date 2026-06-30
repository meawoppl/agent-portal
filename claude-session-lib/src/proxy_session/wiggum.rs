//! Wiggum mode: iterative autonomous loop that re-sends prompts until "DONE".

use std::sync::Arc;
use std::time::{Duration, Instant};

use claude_codes::io::ContentBlock;
use claude_codes::ClaudeOutput;
use shared::{AgentType, ProxyToServer};
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use session_lib::agent::Agent;
use session_lib::output_buffer::PendingOutputBuffer;
use session_lib::session::Session;

use super::git_metadata::{check_and_send_branch_update_if_branch_changed, GitMetadataState};
use super::{
    ack_portal_input, emit_input_progress, format_duration, truncate, ConnectionResult, PortalInput,
    SharedWsWrite,
};

/// Maximum iterations for wiggum mode before auto-stopping
const WIGGUM_MAX_ITERATIONS: u32 = 50;

/// Build the wiggum loop prompt sent to the agent: the original user prompt
/// plus the instruction suffix whose exact wording drives DONE-loop detection.
pub fn wiggum_prompt(original: &str) -> String {
    format!(
        "{}\n\nTake action on the directions above until fully complete. If complete, respond only with DONE.",
        original
    )
}

/// Wiggum mode state
#[derive(Debug, Clone)]
pub struct WiggumState {
    /// Original user prompt (before modification)
    pub original_prompt: String,
    /// Current iteration count
    pub iteration: u32,
    /// When the current loop iteration started
    pub loop_start: Instant,
    /// Durations of the last N loop iterations (most recent last)
    pub loop_durations: Vec<Duration>,
}

/// Wiggum-activation arm of the proxy connection loop (#1165 item 3).
///
/// Extracted from the `run_main_loop` `select!` so the god-loop reads as thin
/// dispatch (parallels [`input_delivery::handle_input`](super::input_delivery)).
/// Sets [`WiggumState`] atomically with sending the first iteration's prompt,
/// emitting the [`InputDeliveryStage`](shared::InputDeliveryStage) progress
/// events (#939) along the way. Returns `Some(ConnectionResult)` to end the
/// connection (agent exited), or `None` to continue the loop.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_wiggum_activation<A: Agent>(
    ws_write: &SharedWsWrite,
    session_id: Uuid,
    working_directory: &str,
    git_metadata: &GitMetadataState,
    wiggum_state: &mut Option<WiggumState>,
    claude_session: &mut Session<A>,
    wiggum_input: PortalInput,
) -> Option<ConnectionResult> {
    info!(
        "Wiggum mode activated with prompt: {}",
        truncate(&wiggum_input.text, 60)
    );
    check_and_send_branch_update_if_branch_changed(
        ws_write,
        session_id,
        working_directory,
        git_metadata,
    )
    .await;
    emit_input_progress(
        ws_write,
        session_id,
        wiggum_input.client_msg_id,
        shared::InputDeliveryStage::ProxyReceived,
    )
    .await;
    let prompt = wiggum_prompt(&wiggum_input.text);
    *wiggum_state = Some(WiggumState {
        original_prompt: wiggum_input.text,
        iteration: 1,
        loop_start: Instant::now(),
        loop_durations: Vec::new(),
    });
    if let Err(e) = claude_session
        .send_input(serde_json::Value::String(prompt))
        .await
    {
        error!("Failed to send wiggum prompt to Claude: {}", e);
        emit_input_progress(
            ws_write,
            session_id,
            wiggum_input.client_msg_id,
            shared::InputDeliveryStage::Failed,
        )
        .await;
        return Some(ConnectionResult::ClaudeExited);
    }
    emit_input_progress(
        ws_write,
        session_id,
        wiggum_input.client_msg_id,
        shared::InputDeliveryStage::AgentAccepted,
    )
    .await;
    ack_portal_input(ws_write, wiggum_input.ack).await;
    None
}

/// Handle a session event from session-lib, with wiggum loop support
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_session_event_with_wiggum<A: Agent>(
    event: Option<session_lib::SessionEvent>,
    output_tx: &mpsc::UnboundedSender<serde_json::Value>,
    ws_write: &SharedWsWrite,
    connection_start: Instant,
    wiggum_state: &mut Option<WiggumState>,
    output_buffer: &Arc<Mutex<PendingOutputBuffer>>,
    claude_session: &mut Session<A>,
    agent_type: AgentType,
) -> Option<ConnectionResult> {
    use session_lib::SessionEvent;

    match event {
        Some(SessionEvent::RawOutput(ref value)) => {
            // Claude visible output now arrives as neutral raw JSON. Re-parse to
            // `ClaudeOutput` for the typed side-effects (wiggum DONE, compaction
            // reminder, system-reminder echo drop); malformed/non-Claude frames
            // skip those and are forwarded verbatim, never dropped.
            let Some(output) = super::parse_visible_claude_output(value) else {
                if output_tx.send(value.clone()).is_err() {
                    error!("Failed to forward Claude output");
                    return Some(ConnectionResult::Disconnected(connection_start.elapsed()));
                }
                return None;
            };

            // Check for wiggum completion before forwarding
            let should_continue_wiggum = if let ClaudeOutput::Result(ref result) = &output {
                if let Some(ref state) = wiggum_state {
                    // Check if Claude responded with "DONE"
                    let is_done = check_wiggum_done(result);
                    if is_done {
                        info!("Wiggum mode complete after {} iterations", state.iteration);
                        false
                    } else {
                        true // Continue the loop
                    }
                } else {
                    false
                }
            } else {
                false
            };

            // If this is a compaction-boundary system message, the agent's
            // context has just been reset to a summary. Re-inject the portal
            // features reminder so the agent has it in the fresh context.
            // We check before forwarding so the reminder lands logically
            // after the compaction-completed notice in the user's transcript.
            let is_compaction_boundary = match &output {
                ClaudeOutput::System(sys) => shared::is_compaction_boundary(sys),
                _ => false,
            };

            // The portal-features reminder is injected as user input wrapped in
            // <system-reminder> tags. Claude echoes that back as a User message
            // on its stdout — if we forward it, the wrapper text leaks into the
            // user's transcript as if they had typed it. Drop those echoes.
            if is_system_reminder_echo(&output) {
                debug!("Dropping system-reminder user echo from transcript");
                // Skip forwarding and skip the rest of the handler.
                return None;
            }

            // Forward the output
            if output_tx.send(value.clone()).is_err() {
                error!("Failed to forward Claude output");
                return Some(ConnectionResult::Disconnected(connection_start.elapsed()));
            }

            if is_compaction_boundary {
                super::inject_portal_reminder(claude_session).await;
            }

            // Handle wiggum loop continuation
            if should_continue_wiggum {
                if let Some(ref mut state) = wiggum_state {
                    // Record the duration of the loop that just finished
                    let loop_duration = state.loop_start.elapsed();
                    state.loop_durations.push(loop_duration);
                    // Keep only the last 10
                    if state.loop_durations.len() > 10 {
                        state.loop_durations.remove(0);
                    }

                    state.iteration += 1;

                    // Check max iterations safety limit
                    if state.iteration > WIGGUM_MAX_ITERATIONS {
                        warn!(
                            "Wiggum reached max iterations ({}), stopping",
                            WIGGUM_MAX_ITERATIONS
                        );
                        *wiggum_state = None;
                    } else {
                        info!("Wiggum iteration {} - resending prompt", state.iteration);

                        // Send a portal message with loop status
                        let portal_text = format_wiggum_status(state);
                        let portal_content = shared::PortalMessage::text(portal_text).to_json();
                        let seq = {
                            let mut buf = output_buffer.lock().await;
                            buf.push(portal_content.clone())
                        };
                        let msg = ProxyToServer::SequencedOutput {
                            seq,
                            content: portal_content,
                            agent_type,
                        };
                        let mut ws = ws_write.lock().await;
                        if ws.send(msg).await.is_err() {
                            error!("Failed to send wiggum portal message");
                            return Some(ConnectionResult::Disconnected(
                                connection_start.elapsed(),
                            ));
                        }

                        // Reset loop_start for the new iteration
                        state.loop_start = Instant::now();

                        // Resend the prompt
                        let prompt = wiggum_prompt(&state.original_prompt);
                        if let Err(e) = claude_session
                            .send_input(serde_json::Value::String(prompt))
                            .await
                        {
                            error!("Failed to resend wiggum prompt: {}", e);
                            *wiggum_state = None;
                            return Some(ConnectionResult::ClaudeExited);
                        }
                    }
                }
            } else if matches!(&output, ClaudeOutput::Result(_)) && wiggum_state.is_some() {
                // Send final completion portal message
                if let Some(ref mut state) = wiggum_state {
                    let loop_duration = state.loop_start.elapsed();
                    state.loop_durations.push(loop_duration);
                    if state.loop_durations.len() > 10 {
                        state.loop_durations.remove(0);
                    }

                    let total: Duration = state.loop_durations.iter().sum();
                    let portal_text = format!(
                        "**Wiggum complete** after **{}** iteration{} (total: {})",
                        state.iteration,
                        if state.iteration == 1 { "" } else { "s" },
                        format_duration(total.as_millis() as u64),
                    );
                    let portal_content = shared::PortalMessage::text(portal_text).to_json();
                    let seq = {
                        let mut buf = output_buffer.lock().await;
                        buf.push(portal_content.clone())
                    };
                    let msg = ProxyToServer::SequencedOutput {
                        seq,
                        content: portal_content,
                        agent_type,
                    };
                    let mut ws = ws_write.lock().await;
                    if ws.send(msg).await.is_err() {
                        error!("Failed to send wiggum completion portal message");
                    }
                }
                // Clear wiggum state when done
                *wiggum_state = None;
            }

            if matches!(&output, ClaudeOutput::Result(_)) && wiggum_state.is_none() {
                debug!("--- ready for input ---");
            }
            None
        }
        Some(SessionEvent::PermissionRequest {
            request_id,
            tool_name,
            input,
            permission_suggestions,
        }) => {
            // `Session` hands these as opaque wire JSON (it's protocol-neutral);
            // re-parse into the typed `ProxyToServer` wire form at the proxy
            // edge. Unparseable entries are dropped rather than failing the
            // request (claude emits well-formed suggestions; codex emits none).
            let permission_suggestions: Vec<claude_codes::io::PermissionSuggestion> =
                permission_suggestions
                    .into_iter()
                    .filter_map(|s| serde_json::from_value(s).ok())
                    .collect();
            // Send permission request directly to WebSocket
            let msg = ProxyToServer::PermissionRequest {
                request_id,
                tool_name,
                input,
                permission_suggestions,
            };
            let mut ws = ws_write.lock().await;
            if let Err(e) = ws.send(msg).await {
                error!("Failed to send permission request to backend: {}", e);
                return Some(ConnectionResult::Disconnected(connection_start.elapsed()));
            }
            None
        }
        Some(SessionEvent::SessionNotFound) => {
            warn!("Session not found (from library event)");
            Some(ConnectionResult::SessionNotFound)
        }
        Some(SessionEvent::Exited { code }) => {
            info!("Claude session exited with code {}", code);
            Some(ConnectionResult::ClaudeExited)
        }
        Some(SessionEvent::TurnMetricsReady(_)) => {
            // Handled in run_main_loop before calling this function
            unreachable!(
                "TurnMetricsReady should be handled before calling handle_session_event_with_wiggum"
            );
        }
        Some(SessionEvent::CodexThreadId(_)) => {
            // Handled in run_main_loop before calling this function
            unreachable!(
                "CodexThreadId should be handled before calling handle_session_event_with_wiggum"
            );
        }
        Some(SessionEvent::SessionLimitReached { .. }) => {
            // Handled in run_main_loop before calling this function
            unreachable!(
                "SessionLimitReached should be handled before calling handle_session_event_with_wiggum"
            );
        }
        Some(SessionEvent::Error(e)) => {
            let err_msg = e.to_string();
            error!("Session error: {}", err_msg);
            if err_msg.contains("Connection closed") || err_msg.contains("Claude stderr") {
                // Claude exited immediately — print a user-visible hint
                eprintln!();
                eprintln!("Claude CLI exited unexpectedly.");
                if let Some(stderr_start) = err_msg.find("Claude stderr: ") {
                    let stderr_text = &err_msg[stderr_start + 15..];
                    eprintln!("stderr: {}", stderr_text);
                } else {
                    eprintln!("No output from Claude. Is `claude` installed and on your PATH?");
                    eprintln!("Try running: claude --version");
                }
                eprintln!();
            }
            Some(ConnectionResult::ClaudeExited)
        }
        None => {
            // Session has ended
            info!("Claude session ended");
            Some(ConnectionResult::ClaudeExited)
        }
    }
}

/// Check if Claude's result indicates wiggum completion (responded with "DONE")
/// Is this Claude output Claude's echo of a `<system-reminder>`-wrapped user
/// input? Those wrapped messages are how we inject the portal-features
/// reminder (and similar out-of-band context); they shouldn't leak into the
/// user's transcript as if the user had typed them.
fn is_system_reminder_echo(output: &ClaudeOutput) -> bool {
    let ClaudeOutput::User(user) = output else {
        return false;
    };
    user.message.content.iter().any(|block| {
        matches!(block, ContentBlock::Text(t) if t.text.trim_start().starts_with("<system-reminder>"))
    })
}

fn check_wiggum_done(result: &claude_codes::io::ResultMessage) -> bool {
    // Check if it was an error (don't continue on errors)
    if result.is_error {
        warn!("Wiggum stopping due to error");
        return true;
    }

    // The result message has a `result` field which contains Claude's final
    // text response. Wiggum only stops when the agent follows the prompt and
    // responds with DONE as the whole answer. Phrases like "not done" or
    // "done with step 1" are progress updates and must keep looping.
    if let Some(ref result_text) = result.result {
        let trimmed = result_text.trim();
        if is_standalone_done(trimmed) {
            info!("Wiggum complete: Claude responded with DONE");
            return true;
        }
    }

    false // Continue the loop
}

fn is_standalone_done(text: &str) -> bool {
    let text = text.trim();
    if text.len() < "DONE".len() {
        return false;
    }

    let (head, tail) = text.split_at("DONE".len());
    if !head.eq_ignore_ascii_case("DONE") {
        return false;
    }

    tail.chars()
        .all(|c| matches!(c, '.' | '!' | '?' | ':' | ';') || c.is_whitespace())
}

/// Build the portal message text for a wiggum loop iteration
fn format_wiggum_status(state: &WiggumState) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "**Wiggum** loop **{}** / {}",
        state.iteration, WIGGUM_MAX_ITERATIONS,
    ));

    if !state.loop_durations.is_empty() {
        lines.push(String::new());
        lines.push("| Loop | Duration |".to_string());
        lines.push("|-----:|---------:|".to_string());

        let start_iter = state.iteration as usize - state.loop_durations.len();
        for (i, d) in state.loop_durations.iter().enumerate() {
            lines.push(format!(
                "| {} | {} |",
                start_iter + i,
                format_duration(d.as_millis() as u64)
            ));
        }

        let total: Duration = state.loop_durations.iter().sum();
        let avg = total / state.loop_durations.len() as u32;
        lines.push(format!(
            "\nAvg: **{}** | Total: **{}**",
            format_duration(avg.as_millis() as u64),
            format_duration(total.as_millis() as u64),
        ));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_codes::io::{ResultMessage, ResultSubtype};

    fn result_message(result: &str) -> ResultMessage {
        ResultMessage {
            subtype: ResultSubtype::Success,
            is_error: false,
            duration_ms: 0,
            duration_api_ms: 0,
            ttft_ms: None,
            ttft_stream_ms: None,
            time_to_request_ms: None,
            num_turns: 1,
            result: Some(result.to_string()),
            session_id: "test-session".to_string(),
            total_cost_usd: 0.0,
            usage: None,
            permission_denials: Vec::new(),
            errors: Vec::new(),
            uuid: None,
            api_error_status: None,
            stop_reason: None,
            terminal_reason: None,
            fast_mode_state: None,
            model_usage: None,
        }
    }

    fn error_result() -> ResultMessage {
        ResultMessage {
            is_error: true,
            result: Some("failed".to_string()),
            ..result_message("failed")
        }
    }

    #[test]
    fn wiggum_done_accepts_standalone_done_only() {
        for text in [
            "DONE", "done", " DONE ", "DONE.", "done.", "DONE!", "DONE?", "DONE:",
        ] {
            assert!(
                check_wiggum_done(&result_message(text)),
                "{text:?} should complete wiggum"
            );
        }
    }

    #[test]
    fn wiggum_done_rejects_progress_phrases_containing_done() {
        for text in [
            "not done",
            "Not done yet.",
            "done with step 1",
            "DONE with setup",
            "almost DONE",
            "DONE - continuing",
            "Done, next I will test",
        ] {
            assert!(
                !check_wiggum_done(&result_message(text)),
                "{text:?} should keep wiggum running"
            );
        }
    }

    #[test]
    fn wiggum_done_still_stops_on_error_result() {
        assert!(check_wiggum_done(&error_result()));
    }
}
