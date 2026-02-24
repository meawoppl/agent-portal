//! Wiggum mode: iterative autonomous loop that re-sends prompts until "DONE".

use std::sync::Arc;
use std::time::{Duration, Instant};

use claude_codes::ClaudeOutput;
use shared::ProxyToServer;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, error, info, warn};

use crate::output_buffer::PendingOutputBuffer;
use crate::session::Session as ClaudeSession;

use super::{format_duration, ConnectionResult, SharedWsWrite};

/// Maximum iterations for wiggum mode before auto-stopping
const WIGGUM_MAX_ITERATIONS: u32 = 50;

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

/// Handle a session event from claude-session-lib, with wiggum loop support
pub(super) async fn handle_session_event_with_wiggum(
    event: Option<crate::session::SessionEvent>,
    output_tx: &mpsc::UnboundedSender<ClaudeOutput>,
    ws_write: &SharedWsWrite,
    connection_start: Instant,
    wiggum_state: &mut Option<WiggumState>,
    output_buffer: &Arc<Mutex<PendingOutputBuffer>>,
    claude_session: &mut ClaudeSession,
) -> Option<ConnectionResult> {
    use crate::session::SessionEvent;

    match event {
        Some(SessionEvent::Output(ref output)) => {
            // Check for wiggum completion before forwarding
            let should_continue_wiggum = if let ClaudeOutput::Result(ref result) = output {
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

            // Forward the output
            if output_tx.send(output.clone()).is_err() {
                error!("Failed to forward Claude output");
                return Some(ConnectionResult::Disconnected(connection_start.elapsed()));
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
                        let wiggum_prompt = format!(
                            "{}\n\nTake action on the directions above until fully complete. If complete, respond only with DONE.",
                            state.original_prompt
                        );
                        if let Err(e) = claude_session
                            .send_input(serde_json::Value::String(wiggum_prompt))
                            .await
                        {
                            error!("Failed to resend wiggum prompt: {}", e);
                            *wiggum_state = None;
                            return Some(ConnectionResult::ClaudeExited);
                        }
                    }
                }
            } else if matches!(output, ClaudeOutput::Result(_)) && wiggum_state.is_some() {
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
                    };
                    let mut ws = ws_write.lock().await;
                    if ws.send(msg).await.is_err() {
                        error!("Failed to send wiggum completion portal message");
                    }
                }
                // Clear wiggum state when done
                *wiggum_state = None;
            }

            if matches!(output, ClaudeOutput::Result(_)) && wiggum_state.is_none() {
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
        Some(SessionEvent::RawOutput(_)) => {
            // Handled in run_main_loop before calling this function
            unreachable!(
                "RawOutput should be handled before calling handle_session_event_with_wiggum"
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
fn check_wiggum_done(result: &claude_codes::io::ResultMessage) -> bool {
    // Check if it was an error (don't continue on errors)
    if result.is_error {
        warn!("Wiggum stopping due to error");
        return true;
    }

    // The result message has a `result` field which contains Claude's final text response
    if let Some(ref result_text) = result.result {
        let text_upper: String = result_text.to_uppercase();
        // Check if the result is exactly "DONE" or contains it prominently
        // Being strict: must be "DONE" alone or "DONE" with minimal surrounding text
        let trimmed = text_upper.trim();
        if trimmed == "DONE" || trimmed.starts_with("DONE.") || trimmed.starts_with("DONE!") {
            info!("Wiggum complete: Claude responded with DONE");
            return true;
        }
        // Also check if DONE appears as the main content
        if trimmed.len() < 50 && trimmed.contains("DONE") {
            info!("Wiggum complete: Claude responded with short message containing DONE");
            return true;
        }
    }

    false // Continue the loop
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
