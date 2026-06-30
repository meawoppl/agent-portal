//! Session-event arm of the proxy connection loop (#1165 item 3).
//!
//! Extracted from the `run_main_loop` `select!` so the god-loop reads as thin
//! dispatch (parallels [`input_delivery::handle_input`](super::input_delivery)
//! and [`wiggum::handle_wiggum_activation`](super::wiggum)). Dispatches one
//! [`SessionEvent`] from `claude_session.next_event()`:
//!
//! - Non-Claude `RawOutput`: the simple codex path (git refresh, buffer, send,
//!   compaction reminder) — Claude `RawOutput` falls through to the
//!   wiggum/output-forwarder path that re-parses to `ClaudeOutput`.
//! - `TurnMetricsReady`, `CodexThreadId`, `SessionLimitReached`: typed
//!   side-channel forwards.
//! - Everything else: delegated to
//!   [`wiggum::handle_session_event_with_wiggum`](super::wiggum).
//!
//! Returns `Some(ConnectionResult)` to end the connection, or `None` to
//! continue the loop (the inline `continue`s become `None`).

use session_lib::agent::Agent;
use session_lib::session::Session;
use session_lib::SessionEvent;
use shared::ProxyToServer;
use tracing::error;

use super::git_metadata::{check_and_send_branch_update, codex_output_has_git_signal};
use super::wiggum::handle_session_event_with_wiggum;
use super::{inject_portal_reminder, is_codex_compaction_event, ConnectionResult, ConnectionState};

pub(super) async fn handle_next_event<A: Agent>(
    state: &mut ConnectionState,
    claude_session: &mut Session<A>,
    event: Option<SessionEvent>,
) -> Option<ConnectionResult> {
    // Both backends now deliver visible output as the neutral
    // `SessionEvent::RawOutput(value)`. Route by agent at the proxy edge: Codex
    // uses this simple inline path; Claude falls through to the
    // wiggum/output-forwarder path, which re-parses the value to `ClaudeOutput`
    // for its image/git/echo/wiggum side-effects.
    if state.agent_type != shared::AgentType::Claude {
        if let Some(SessionEvent::RawOutput(ref value)) = event {
            if state.git_refresh.should_check_before_message() {
                check_and_send_branch_update(
                    &state.ws_write,
                    state.session_id,
                    &state.working_directory,
                    &state.git_metadata,
                )
                .await;
            }
            if codex_output_has_git_signal(value) {
                state.git_refresh.mark_git_signal();
            }

            let is_codex_compaction = is_codex_compaction_event(value);
            let seq = {
                let mut buf = state.output_buffer.lock().await;
                buf.push(value.clone())
            };
            let msg = ProxyToServer::SequencedOutput {
                seq,
                content: value.clone(),
                agent_type: state.agent_type,
            };
            // NB: the `ws` write guard is intentionally held across the
            // `inject_portal_reminder` await below, matching the pre-extraction
            // inline behavior (the guard's scope was the whole arm). The
            // reminder injects input to the agent and never touches `ws_write`,
            // so this is a plain serialization point, not a deadlock.
            let mut ws = state.ws_write.lock().await;
            if ws.send(msg).await.is_err() {
                error!("Failed to send raw output");
                return Some(ConnectionResult::Disconnected(
                    state.connection_start.elapsed(),
                ));
            }
            if is_codex_compaction {
                inject_portal_reminder(claude_session).await;
            }
            return None;
        }
    }

    // Per-turn metrics: forward as a typed `TurnMetricsReport` envelope. Doesn't
    // go through the output buffer because it's not part of the message-replay
    // path — a missed metrics row is acceptable (next turn replaces it on the
    // dashboard) but we still want low-latency delivery.
    if let Some(SessionEvent::TurnMetricsReady(metrics)) = event {
        let msg = ProxyToServer::TurnMetricsReport(metrics);
        let mut ws = state.ws_write.lock().await;
        if ws.send(msg).await.is_err() {
            error!("Failed to send turn metrics report");
            return Some(ConnectionResult::Disconnected(
                state.connection_start.elapsed(),
            ));
        }
        return None;
    }

    // Codex app-server thread id: hand it to the persistence sink (the proxy's
    // ProxyConfig writer) so the next resume of this session can call
    // `thread/resume` with it. Emitted once per spawn by codex-session-lib.
    // No-op for claude sessions (claude doesn't emit it).
    if let Some(SessionEvent::CodexThreadId(thread_id)) = event {
        if let Some(sink) = state.codex_thread_id_sink.as_ref() {
            sink(thread_id);
        }
        return None;
    }

    if let Some(SessionEvent::SessionLimitReached {
        session_id,
        reset_at,
        source_message,
        prompt,
    }) = event
    {
        let msg = ProxyToServer::SessionLimitReached(shared::SessionLimitContinuationFields {
            session_id,
            reset_at,
            source_message,
            prompt,
        });
        let mut ws = state.ws_write.lock().await;
        if ws.send(msg).await.is_err() {
            error!("Failed to send session-limit continuation candidate");
            return Some(ConnectionResult::Disconnected(
                state.connection_start.elapsed(),
            ));
        }
        return None;
    }

    handle_session_event_with_wiggum(
        event,
        &state.output_tx,
        &state.ws_write,
        state.connection_start,
        &mut state.wiggum_state,
        &state.output_buffer,
        claude_session,
        state.agent_type,
    )
    .await
}
