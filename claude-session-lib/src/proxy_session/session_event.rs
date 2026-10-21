//! Session-event arm of the proxy connection loop (#1165 item 3).
//!
//! Claude-specific tail of event dispatch. Agent-neutral side channels are
//! handled first by [`session_lib::proxy_session::session_event`]; this module
//! retains raw-output behavior and Wiggum. Dispatches one
//! [`SessionEvent`] from `claude_session.next_event()`:
//!
//! - Non-Claude `RawOutput`: the simple codex path (git refresh, buffer, send,
//!   compaction re-arms the reminder) — Claude `RawOutput` falls through to the
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
use super::{is_codex_compaction_event, ConnectionResult, ConnectionState};

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
    // Portal notices (including terminal OOM attribution) must reach the
    // sequenced replay buffer inline. The Claude output worker is aborted on
    // Exited; queuing a notice there can erase it before it is even sequenced.
    let portal_notice = matches!(&event, Some(SessionEvent::RawOutput(value))
        if serde_json::from_value::<shared::PortalMessage>(value.clone())
            .is_ok_and(|message| message.message_type == shared::PortalMessage::MESSAGE_TYPE));
    if state.agent_type != shared::AgentType::Claude || portal_notice {
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
                shared::AgentType::Antigravity => false,
                _ => codex_output_has_git_signal(value),
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
            let mut ws = state.ws_write.lock().await;
            if ws.send(msg).await.is_err() {
                error!("Failed to send raw output");
                return Some(ConnectionResult::Disconnected(
                    state.connection_start.elapsed(),
                ));
            }
            // Compaction drops the reminder from the agent's context. Re-arm
            // it so the next real input carries it (see `input_delivery`).
            if is_codex_compaction {
                state
                    .reminder_pending
                    .store(true, std::sync::atomic::Ordering::SeqCst);
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
        &state.reminder_pending,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use session_lib::{
        agent::Agent,
        error::SessionError,
        io::{IoCommand, IoEvent},
        snapshot::SessionConfig,
    };
    use std::sync::{atomic::AtomicBool, Arc};
    use std::time::Instant;
    use tokio::sync::{mpsc, oneshot, Mutex};

    struct NoProcess;
    impl Agent for NoProcess {
        fn spawn_io_task(
            _: SessionConfig,
            _: mpsc::UnboundedReceiver<IoCommand>,
            _: mpsc::UnboundedSender<IoEvent>,
        ) -> Result<tokio::task::JoinHandle<()>, SessionError> {
            Ok(tokio::spawn(async {}))
        }
    }

    /// The output worker is deliberately never polled. A notice immediately
    /// followed by Exited must still be on the wire and in the replay buffer.
    #[tokio::test]
    async fn claude_terminal_notice_bypasses_abortable_output_queue() {
        use futures_util::StreamExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(socket).await.unwrap();
            let frame = socket.next().await.unwrap().unwrap();
            frame.into_text().unwrap().to_string()
        });
        let connection = ws_bridge::native_client::connect::<shared::SessionEndpoint>(&format!(
            "ws://{address}"
        ))
        .await
        .unwrap();
        let (write, _read) = connection.split();
        let ws_write = Arc::new(Mutex::new(write));
        let id = uuid::Uuid::new_v4();
        let (output_tx, mut output_rx) = mpsc::unbounded_channel();
        let mut state = ConnectionState {
            session_id: id,
            perm_rx: mpsc::unbounded_channel().1,
            interrupt_rx: mpsc::unbounded_channel().1,
            ack_rx: mpsc::unbounded_channel().1,
            output_tx,
            ws_write: ws_write.clone(),
            disconnect_rx: oneshot::channel().1,
            graceful_shutdown_rx: mpsc::unbounded_channel().1,
            session_terminated_rx: oneshot::channel().1,
            connection_start: Instant::now(),
            output_buffer: Arc::new(Mutex::new(
                session_lib::output_buffer::PendingOutputBuffer::new(id).unwrap(),
            )),
            wiggum_rx: mpsc::unbounded_channel().1,
            wiggum_state: None,
            heartbeat: session_lib::heartbeat::HeartbeatTracker::new(),
            file_upload_rx: mpsc::unbounded_channel().1,
            file_download_rx: mpsc::unbounded_channel().1,
            working_directory: ".".into(),
            file_transfer: super::super::FileTransferBridge::new(".".into(), ws_write),
            reminder_pending: Arc::new(AtomicBool::new(false)),
            agent_type: shared::AgentType::Claude,
            git_metadata: session_lib::git_metadata::GitMetadataState::new(None),
            git_refresh: Default::default(),
            codex_thread_id_sink: None,
            media_display_sink: None,
        };
        let mut session = Session::<NoProcess>::new(SessionConfig::default())
            .await
            .unwrap();
        let notice = shared::PortalMessage::text(
            "Out of memory: the kernel killed 1 process in this session's cgroup.".into(),
        )
        .to_json();
        assert!(handle_next_event(
            &mut state,
            &mut session,
            Some(SessionEvent::RawOutput(notice.clone()))
        )
        .await
        .is_none());
        assert!(
            output_rx.try_recv().is_err(),
            "notice must not wait for the worker"
        );
        assert_eq!(
            state
                .output_buffer
                .lock()
                .await
                .get_pending()
                .next()
                .unwrap()
                .content,
            notice
        );
        assert!(handle_next_event(
            &mut state,
            &mut session,
            Some(SessionEvent::Exited { code: 137 })
        )
        .await
        .is_some());
        let wire = tokio::time::timeout(std::time::Duration::from_secs(2), server)
            .await
            .unwrap()
            .unwrap();
        let message: ProxyToServer = serde_json::from_str(&wire).unwrap();
        assert!(
            matches!(message, ProxyToServer::SequencedOutput { content, .. } if content == notice)
        );
    }
}
