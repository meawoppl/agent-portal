//! Generic, agent-agnostic session lifecycle.
//!
//! `Session<A: Agent>` owns the buffer, pending-permission state, and
//! per-session command/event channels. Per-agent backends (see
//! [`crate::agent::Agent`]) plug in their I/O task via `A::spawn_io_task`.

use std::marker::PhantomData;

use chrono::Utc;
use claude_codes::io::{ControlResponse, PermissionResult, PermissionSuggestion};
use claude_codes::ClaudeInput;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::adapter::{AgentAdapter, AgentOutput};
use crate::agent::Agent;
use crate::buffer::OutputBuffer;
use crate::error::SessionError;
use crate::io::{IoCommand, IoEvent, PermissionResponse, SessionEvent};
use crate::snapshot::{PendingPermission, SessionConfig, SessionSnapshot};

/// Internal session state.
enum SessionState {
    Running,
    WaitingForPermission,
    Exited { code: i32 },
}

/// A managed agent-CLI session.
///
/// Generic over the agent backend `A`; commit at the call site
/// (`Session::<ClaudeAgent>::new(...)` or `Session::<CodexAgent>::new(...)`).
/// The per-agent backend's [`Agent::spawn_io_task`] is invoked from
/// `new` / `restore` to spawn the agent's I/O task; the task owns the
/// agent process(es) and communicates with this `Session` via mpsc
/// channels.
pub struct Session<A: Agent> {
    id: Uuid,
    config: SessionConfig,
    /// Channel to send commands (input, permission responses) to the I/O task.
    command_tx: Option<mpsc::UnboundedSender<IoCommand>>,
    buffer: OutputBuffer,
    state: SessionState,
    pending_permission: Option<PendingPermission>,
    /// Receiver for events from the I/O task.
    event_rx: Option<mpsc::UnboundedReceiver<IoEvent>>,
    /// Handle to the spawned I/O task, so `stop` (and `Drop`) can abort it and
    /// reap the agent process. The task otherwise parks in `client.receive()`
    /// and an idle agent process would outlive the session — dropping the
    /// command channel alone does not end it. Aborting drops the task's client,
    /// which (with `kill_on_drop` on the spawn) kills the child.
    io_task: Option<tokio::task::JoinHandle<()>>,
    /// OS pid of the agent process, learned from `IoEvent::AgentStarted`.
    /// `stop()` uses it to signal the agent's process group directly, since
    /// `kill_on_drop` doesn't fire when the SDK keeps the child alive in
    /// detached tasks (#927).
    agent_pid: Option<u32>,
    /// Per-agent protocol adapter, supplied by `A::adapter()`. `next_event`
    /// drives it to classify each `IoEvent::Output` unit. `None` for agents
    /// that pre-classify their output (Codex today); see [`Agent::adapter`].
    adapter: Option<Box<dyn AgentAdapter>>,
    _agent: PhantomData<A>,
}

impl<A: Agent> Drop for Session<A> {
    /// Safety net: if a `Session` is dropped without `stop()` (e.g. its owning
    /// task was aborted), abort the I/O task so the agent process doesn't leak.
    /// Drop can't await, so this is best-effort — `kill_on_drop` on the spawn
    /// does the actual reaping once the task drops its client.
    fn drop(&mut self) {
        if let Some(handle) = self.io_task.take() {
            handle.abort();
        }
        // Capture a queued AgentStarted pid (sync `try_recv`) so an
        // immediate-drop-after-spawn still reaps the agent (#927 review).
        self.capture_pending_agent_pid();
        // Best-effort SIGKILL of the agent's process group. Drop can't await,
        // so we can't do the graceful SIGTERM dance `stop()` does — but a
        // synchronous group SIGKILL still prevents the orphan (#927) that
        // `kill_on_drop` misses.
        #[cfg(unix)]
        if let Some(pid) = self.agent_pid.take() {
            signal_process_group(pid, libc::SIGKILL);
        }
    }
}

/// Signal the process group led by `pid` (the agent was spawned as a group
/// leader, so its pgid == pid). Negative pid targets the whole group, reaping
/// tools the agent spawned. Best-effort: a dead group yields `ESRCH`, ignored.
#[cfg(unix)]
fn signal_process_group(pid: u32, sig: libc::c_int) {
    // SAFETY: `kill(2)` with a group target and a fixed signal has no memory
    // safety implications; the worst case is `ESRCH` for an already-dead group.
    unsafe {
        libc::kill(-(pid as i32), sig);
    }
}

#[cfg(all(test, unix))]
mod kill_tests {
    use super::signal_process_group;
    use std::os::unix::process::CommandExt;
    use std::process::Command;

    /// The whole point of #927: a SIGKILL to the agent's *group* reaps the
    /// process even when the immediate child handle is never dropped. Spawn a
    /// long `sleep` as its own group leader, group-kill it, and confirm it dies.
    #[test]
    fn group_kill_reaps_a_detached_process() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60").process_group(0); // own group: pgid == pid
        let mut child = cmd.spawn().expect("spawn sleep");
        let pid = child.id();

        signal_process_group(pid, libc::SIGKILL);

        // `wait` reaps promptly because the process was killed; a leaked
        // process would hang here. The exit is signal-terminated, not success.
        let status = child.wait().expect("wait on killed child");
        assert!(!status.success(), "process should have been killed");
    }
}

impl<A: Agent> Session<A> {
    /// Create a new session, spawning the per-agent I/O task.
    pub async fn new(config: SessionConfig) -> Result<Self, SessionError> {
        let buffer = OutputBuffer::new(config.session_id);

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (command_tx, command_rx) = mpsc::unbounded_channel();

        // Let the agent backend spawn its I/O task. Backends that need to do
        // synchronous setup (spawn process, handshake, etc.) before the task
        // loop starts may do so here and surface failures as `SessionError`.
        let handle = A::spawn_io_task(config.clone(), command_rx, event_tx)?;

        Ok(Self {
            id: config.session_id,
            config,
            command_tx: Some(command_tx),
            buffer,
            state: SessionState::Running,
            pending_permission: None,
            event_rx: Some(event_rx),
            io_task: Some(handle),
            agent_pid: None,
            adapter: A::adapter(),
            _agent: PhantomData,
        })
    }

    /// Restore a session from a snapshot.
    ///
    /// This restores the buffer and pending permission state, then spawns a
    /// fresh I/O task with `resume = true` so the agent CLI is re-attached
    /// to the same session id.
    pub async fn restore(snapshot: SessionSnapshot) -> Result<Self, SessionError> {
        let buffer = OutputBuffer::from_snapshot(snapshot.id, snapshot.pending_outputs);

        // Always resume when restoring.
        let mut config = snapshot.config;
        config.resume = true;

        let (command_tx, event_rx, io_task) = if snapshot.was_running {
            let (event_tx, event_rx) = mpsc::unbounded_channel();
            let (command_tx, command_rx) = mpsc::unbounded_channel();
            let handle = A::spawn_io_task(config.clone(), command_rx, event_tx)?;
            (Some(command_tx), Some(event_rx), Some(handle))
        } else {
            (None, None, None)
        };

        let state = if command_tx.is_some() {
            SessionState::Running
        } else {
            SessionState::Exited { code: 0 }
        };

        Ok(Self {
            id: snapshot.id,
            config,
            command_tx,
            buffer,
            state,
            pending_permission: snapshot.pending_permission,
            event_rx,
            io_task,
            agent_pid: None,
            adapter: A::adapter(),
            _agent: PhantomData,
        })
    }

    /// Serialize current state for persistence.
    pub fn snapshot(&self) -> SessionSnapshot {
        let was_running = matches!(
            self.state,
            SessionState::Running | SessionState::WaitingForPermission
        );
        SessionSnapshot::new(
            self.id,
            self.config.clone(),
            self.buffer.to_snapshot(),
            self.pending_permission.clone(),
            was_running,
        )
    }

    /// Get the session ID.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Get the session config.
    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Poll for the next event.
    ///
    /// Returns `None` if the session has exited and no more events are
    /// available. Use this in a loop with other async operations via
    /// `tokio::select!`. Events are delivered from a dedicated I/O task that
    /// continuously reads the agent's stdout, so this method will not block
    /// other select! branches from being processed, and stdout will not
    /// overflow.
    pub async fn next_event(&mut self) -> Option<SessionEvent> {
        // Loop to skip internal messages (ControlResponse, etc).
        loop {
            let event_rx = self.event_rx.as_mut()?;

            match event_rx.recv().await {
                Some(IoEvent::AgentStarted { pid }) => {
                    // Record the agent pid for `stop()`'s group-kill (#927);
                    // not a consumer-facing event, so keep looping.
                    self.agent_pid = pid;
                }
                Some(IoEvent::Output(boxed_output)) => {
                    let output = *boxed_output;

                    // Buffer the raw output before classifying (preserve ordering).
                    let output_value = serde_json::to_value(&output).unwrap_or_default();
                    self.buffer.push(output_value.clone());

                    // Delegate protocol classification to the agent's adapter
                    // (`A::adapter()`). Claude is 1:1 (one raw unit → one
                    // decision) but handle the Vec generally so the loop matches
                    // the trait contract. Collect first so the `&mut adapter`
                    // borrow ends before the loop mutates other `self` fields.
                    let decisions = match self.adapter.as_mut() {
                        Some(adapter) => adapter.classify(output_value),
                        // Agents without an adapter (Codex today) never emit
                        // `IoEvent::Output` — they pre-classify upstream. Forward
                        // any stray unit verbatim so output is never dropped.
                        None => vec![AgentOutput::Visible(output_value)],
                    };
                    let mut visible = false;
                    for decision in decisions {
                        match decision {
                            AgentOutput::NotFound => {
                                self.state = SessionState::Exited { code: 1 };
                                self.command_tx = None;
                                self.event_rx = None;
                                return Some(SessionEvent::SessionNotFound);
                            }
                            AgentOutput::PermissionRequest {
                                request_id,
                                tool_name,
                                input,
                                suggestions,
                            } => {
                                // Recover the typed suggestions from the adapter's
                                // opaque JSON round-trip (it serialized them).
                                let permission_suggestions: Vec<PermissionSuggestion> = suggestions
                                    .into_iter()
                                    .filter_map(|s| serde_json::from_value(s).ok())
                                    .collect();
                                self.pending_permission = Some(PendingPermission {
                                    request_id: request_id.clone(),
                                    tool_name: tool_name.clone(),
                                    input: input.clone(),
                                    requested_at: Utc::now(),
                                });
                                self.state = SessionState::WaitingForPermission;
                                return Some(SessionEvent::PermissionRequest {
                                    request_id,
                                    tool_name,
                                    input,
                                    permission_suggestions,
                                });
                            }
                            // Internal ack / nothing for the consumer — skip it and
                            // let the outer loop pull the next event.
                            AgentOutput::Noop => {}
                            // User-visible output: forward the ORIGINAL typed value.
                            AgentOutput::Visible(_) => visible = true,
                        }
                    }

                    if visible {
                        return Some(SessionEvent::Output(Box::new(output)));
                    }
                    continue;
                }
                Some(IoEvent::RawOutput(value)) => {
                    self.buffer.push(value.clone());
                    return Some(SessionEvent::RawOutput(value));
                }
                Some(IoEvent::CodexPermissionRequest { request_id, input }) => {
                    // Derive the stringly-typed `tool_name` from the typed
                    // variant so the cross-agent `SessionEvent::PermissionRequest`
                    // and the persisted `PendingPermission` keep the same key
                    // the claude path provides verbatim from `ToolUseRequest`.
                    let tool_name = input.tool_name().to_string();
                    // Re-serialize the typed envelope to JSON for the
                    // cross-agent boundary (the claude path emits a raw
                    // `serde_json::Value` from `ToolUseRequest::input` and
                    // the wire stays `Value` for both). Infallible for a
                    // serde-derived enum, but fall back to Null defensively.
                    let input_value =
                        serde_json::to_value(&input).unwrap_or(serde_json::Value::Null);
                    self.pending_permission = Some(PendingPermission {
                        request_id: request_id.clone(),
                        tool_name: tool_name.clone(),
                        input: input_value.clone(),
                        requested_at: Utc::now(),
                    });
                    self.state = SessionState::WaitingForPermission;
                    return Some(SessionEvent::PermissionRequest {
                        request_id,
                        tool_name,
                        input: input_value,
                        permission_suggestions: vec![],
                    });
                }
                Some(IoEvent::TurnMetricsReady(metrics)) => {
                    return Some(SessionEvent::TurnMetricsReady(metrics));
                }
                Some(IoEvent::CodexThreadId(thread_id)) => {
                    return Some(SessionEvent::CodexThreadId(thread_id));
                }
                Some(IoEvent::SessionLimitReached {
                    session_id,
                    reset_at,
                    source_message,
                    prompt,
                }) => {
                    return Some(SessionEvent::SessionLimitReached {
                        session_id,
                        reset_at,
                        source_message,
                        prompt,
                    });
                }
                Some(IoEvent::Exited { code }) => {
                    self.state = SessionState::Exited { code };
                    self.command_tx = None;
                    self.event_rx = None;
                    return Some(SessionEvent::Exited { code });
                }
                Some(IoEvent::Error(e)) => {
                    return Some(SessionEvent::Error(e));
                }
                None => {
                    self.state = SessionState::Exited { code: 0 };
                    self.command_tx = None;
                    self.event_rx = None;
                    return None;
                }
            }
        }
    }

    /// Send user input to the agent.
    ///
    /// The content can be a JSON string value for plain text, or a more
    /// complex JSON structure if needed.
    pub async fn send_input(&mut self, content: serde_json::Value) -> Result<(), SessionError> {
        self.send_input_with_display(content, None).await
    }

    /// Like [`send_input`](Self::send_input), but carries an optional typed
    /// portal `display_event` to render in place of the agent's echo (see
    /// [`IoCommand::Input::display_event`]). The proxy passes the inter-agent
    /// `PortalContent::AgentMessage` envelope here so the Codex path (which
    /// can't rely on echo-replacement) still renders the typed card.
    pub async fn send_input_with_display(
        &mut self,
        content: serde_json::Value,
        display_event: Option<serde_json::Value>,
    ) -> Result<(), SessionError> {
        if let SessionState::Exited { code } = self.state {
            return Err(SessionError::AlreadyExited(code));
        }

        if let Some(ref command_tx) = self.command_tx {
            let text = match &content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let input = ClaudeInput::user_message(text, self.id);
            let (delivered_tx, delivered_rx) = oneshot::channel();
            command_tx
                .send(IoCommand::Input {
                    input,
                    delivered: Some(delivered_tx),
                    display_event: display_event.map(Box::new),
                })
                .map_err(|_| SessionError::CommunicationError("I/O task closed".to_string()))?;
            delivered_rx
                .await
                .map_err(|_| {
                    SessionError::CommunicationError(
                        "I/O task closed before accepting input".to_string(),
                    )
                })?
                .map_err(SessionError::Agent)?;
        }

        Ok(())
    }

    /// Respond to a pending permission request.
    ///
    /// Codex sessions route this through `IoCommand::CodexApproval`; Claude
    /// sessions route it through `IoCommand::PermissionResponse`. The per-
    /// agent I/O task ignores whichever variant doesn't apply.
    pub async fn respond_permission(
        &mut self,
        request_id: &str,
        response: PermissionResponse,
    ) -> Result<(), SessionError> {
        // Verify this is the pending request.
        match &self.pending_permission {
            Some(perm) if perm.request_id == request_id => {}
            _ => {
                return Err(SessionError::InvalidPermissionResponse(
                    request_id.to_string(),
                ));
            }
        }

        if let Some(ref command_tx) = self.command_tx {
            if self.config.agent_type == shared::AgentType::Codex {
                // Codex approval flow: map allow/deny to accept/decline.
                // Typed wrapper so the wire shape lives in source rather than
                // in a `json!` literal.
                #[derive(serde::Serialize)]
                struct CodexApprovalResult<'a> {
                    decision: &'a str,
                }
                let decision = if response.allow { "accept" } else { "decline" };
                let result = serde_json::to_value(CodexApprovalResult { decision })
                    .unwrap_or(serde_json::Value::Null);
                command_tx
                    .send(IoCommand::CodexApproval {
                        request_id: request_id.to_string(),
                        result,
                    })
                    .map_err(|_| SessionError::CommunicationError("I/O task closed".to_string()))?;
            } else {
                // Claude permission flow.
                let input_value = response
                    .input
                    .unwrap_or(serde_json::Value::Object(Default::default()));

                let ctrl_response = if response.allow {
                    if response.permissions.is_empty() {
                        ControlResponse::from_result(
                            request_id,
                            PermissionResult::allow(input_value),
                        )
                    } else {
                        ControlResponse::from_result(
                            request_id,
                            PermissionResult::allow_with_typed_permissions(
                                input_value,
                                response.permissions,
                            ),
                        )
                    }
                } else {
                    let reason = response.reason.unwrap_or_else(|| "User denied".to_string());
                    ControlResponse::from_result(request_id, PermissionResult::deny(reason))
                };

                command_tx
                    .send(IoCommand::PermissionResponse(ctrl_response))
                    .map_err(|_| SessionError::CommunicationError("I/O task closed".to_string()))?;
            }
        }

        self.pending_permission = None;
        self.state = SessionState::Running;

        Ok(())
    }

    /// Drain any queued events to capture a not-yet-consumed
    /// `IoEvent::AgentStarted` pid. `next_event` normally records it, but an
    /// immediate stop/cancel right after spawn can run before `next_event`
    /// ever drains the channel — without this, the group-kill in `stop()` /
    /// `Drop` would be skipped and the agent orphaned anyway (#927 review).
    fn capture_pending_agent_pid(&mut self) {
        if let Some(rx) = self.event_rx.as_mut() {
            while let Ok(event) = rx.try_recv() {
                if let IoEvent::AgentStarted { pid } = event {
                    self.agent_pid = pid;
                }
            }
        }
    }

    /// Gracefully stop the session and reap the agent process.
    pub async fn stop(&mut self) -> Result<(), SessionError> {
        // Capture a queued pid before dropping the receiver below.
        self.capture_pending_agent_pid();
        // Drop the channels first so the I/O task stops accepting input.
        self.command_tx = None;
        self.event_rx = None;
        // Then abort and await the I/O task. Dropping the command channel alone
        // is not enough — the task parks in `client.receive()` and would keep an
        // idle agent process alive indefinitely. Aborting drops the task's
        // client; with `kill_on_drop` on the spawn that kills the child. Awaiting
        // (the abort yields a cancelled `JoinError`) makes stop synchronous with
        // teardown so callers don't race a still-dying process.
        if let Some(handle) = self.io_task.take() {
            handle.abort();
            let _ = handle.await;
        }
        // Explicitly terminate the agent's process group. `kill_on_drop` is
        // unreliable here — the SDK's `AsyncClient` keeps the child alive in
        // detached tasks, so aborting the I/O task doesn't drop (and thus
        // doesn't kill) it, orphaning the agent process (#927). SIGTERM first
        // for a clean shutdown, then SIGKILL after a short grace period.
        #[cfg(unix)]
        if let Some(pid) = self.agent_pid.take() {
            signal_process_group(pid, libc::SIGTERM);
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            signal_process_group(pid, libc::SIGKILL);
        }
        self.state = SessionState::Exited { code: 0 };
        Ok(())
    }

    /// Check if session is still running.
    pub fn is_running(&self) -> bool {
        matches!(
            self.state,
            SessionState::Running | SessionState::WaitingForPermission
        )
    }

    /// Check if session has a pending permission request.
    pub fn has_pending_permission(&self) -> bool {
        self.pending_permission.is_some()
    }

    /// Get the pending permission request, if any.
    pub fn pending_permission(&self) -> Option<&PendingPermission> {
        self.pending_permission.as_ref()
    }

    /// Acknowledge outputs up to the given sequence number.
    pub fn ack_outputs(&mut self, seq: u64) {
        self.buffer.ack(seq);
    }

    /// Get pending output count.
    pub fn pending_output_count(&self) -> usize {
        self.buffer.pending_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    // `STARTED` flips once the mock io-task is actually running (so we don't
    // abort it before it begins); `GUARD_DROPPED` flips when it's dropped, which
    // proves `stop` aborted it.
    static STARTED: AtomicBool = AtomicBool::new(false);
    static GUARD_DROPPED: AtomicBool = AtomicBool::new(false);

    struct DropFlag;
    impl Drop for DropFlag {
        fn drop(&mut self) {
            GUARD_DROPPED.store(true, Ordering::SeqCst);
        }
    }

    /// An agent whose io-task parks forever (like a real one waiting on
    /// `client.receive()`), holding a guard so we can observe it being dropped.
    struct ParkAgent;
    impl Agent for ParkAgent {
        fn spawn_io_task(
            _config: SessionConfig,
            _command_rx: mpsc::UnboundedReceiver<IoCommand>,
            _event_tx: mpsc::UnboundedSender<IoEvent>,
        ) -> Result<tokio::task::JoinHandle<()>, SessionError> {
            Ok(tokio::spawn(async {
                let _guard = DropFlag;
                STARTED.store(true, Ordering::SeqCst);
                std::future::pending::<()>().await;
            }))
        }
    }

    #[tokio::test]
    async fn stop_aborts_and_reaps_io_task() {
        STARTED.store(false, Ordering::SeqCst);
        GUARD_DROPPED.store(false, Ordering::SeqCst);
        let mut session = Session::<ParkAgent>::new(SessionConfig::default())
            .await
            .unwrap();

        // Let the spawned task actually start (and create its guard) before we
        // stop it, so the test exercises aborting a *running* parked task.
        while !STARTED.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        assert!(!GUARD_DROPPED.load(Ordering::SeqCst));

        // stop() must abort the parked task and await it, so by the time it
        // returns the task has run its drop. Dropping the command channel alone
        // would not end a task parked in `client.receive()`.
        session.stop().await.unwrap();
        assert!(
            GUARD_DROPPED.load(Ordering::SeqCst),
            "stop() should abort the io task and reap it"
        );
    }
}
