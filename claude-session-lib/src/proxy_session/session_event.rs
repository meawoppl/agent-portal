//! Session-event arm of the proxy connection loop (#1165 item 3).
//!
//! Claude-specific tail of event dispatch. Agent-neutral side channels are
//! handled first by [`session_lib::proxy_session::session_event`]; this module
//! retains raw-output behavior and Wiggum. Dispatches one
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

use super::git_metadata::{
    codex_output_has_git_signal, muse_output_has_git_signal, spawn_branch_update,
};
use super::media_display::codex_output_image_read;
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
    // for its image/git/wiggum side-effects.
    if state.agent_type != shared::AgentType::Claude {
        if let Some(SessionEvent::RawOutput(ref value)) = event {
            // Fire-and-forget: this arm runs inline in the connection's
            // `select!`, so awaiting a refresh here stalls output forwarding,
            // input delivery, acks and heartbeats behind up to three `gh`
            // network calls. The update is emitted as its own `SessionUpdate`
            // when it completes, so nothing is lost by not waiting.
            if state.git_refresh.should_check_before_message() {
                spawn_branch_update(
                    &state.ws_write,
                    state.session_id,
                    &state.working_directory,
                    &state.git_metadata,
                );
            }
            // This arm carries codex *and* muse, whose wire shapes have
            // nothing in common — so the detector is chosen by agent rather
            // than tried in sequence. Running the codex predicate over muse
            // records is what left muse refreshing only on the
            // every-100-messages fallback (#1653).
            let has_git_signal = match state.agent_type {
                shared::AgentType::Muse => muse_output_has_git_signal(value),
                shared::AgentType::Codex => codex_output_has_git_signal(value),
                shared::AgentType::Claude | shared::AgentType::Antigravity => false,
            };
            if has_git_signal {
                state.git_refresh.mark_git_signal();
            }

            // Codex counterpart to the Claude Read-triggered display. Muse is
            // excluded deliberately: its journal has no command-execution record
            // this could key on, so a detector here would be dead code.
            if state.agent_type == shared::AgentType::Codex {
                if let Some(sink) = state.media_display_sink.as_ref() {
                    if let Some(path) = codex_output_image_read(value) {
                        sink(path);
                    }
                }
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

    let event = match session_lib::proxy_session::session_event::dispatch(
        event,
        &state.ws_write,
        state.session_id,
        state.connection_start,
        state.codex_thread_id_sink.as_deref(),
    )
    .await
    {
        session_lib::proxy_session::session_event::DispatchResult::Handled => return None,
        session_lib::proxy_session::session_event::DispatchResult::Disconnect(result) => {
            return Some(result);
        }
        session_lib::proxy_session::session_event::DispatchResult::Unhandled(event) => event,
    };

    handle_session_event_with_wiggum(
        event,
        state.session_id,
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
