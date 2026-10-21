//! Generic, agent-agnostic session lifecycle.
//!
//! `Session<A: Agent>` owns the buffer, pending-permission state, and
//! per-session command/event channels. Per-agent backends (see
//! [`crate::agent::Agent`]) plug in their I/O task via `A::spawn_io_task`.

use std::marker::PhantomData;

use chrono::Utc;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

use crate::adapter::{AgentOutput, PermissionDecision};
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
    memory_events: Option<crate::memory_events::MemoryEvents>,
    // Preserve terminal events while delivering a final kernel OOM notice first.
    pending_event: Option<Option<IoEvent>>,
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

/// Signal the process group led by `pid` (the process must have been spawned
/// as a group leader, so its pgid == pid). Negative pid targets the whole
/// group, reaping its children too. Best-effort: a dead group yields `ESRCH`,
/// ignored. Also used by the launcher to stop plugin surfaces.
#[cfg(unix)]
pub fn signal_process_group(pid: u32, sig: libc::c_int) {
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
        let memory_events = config
            .memory_events_path
            .clone()
            .map(crate::memory_events::MemoryEvents::new);

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
            memory_events,
            pending_event: None,
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

        let memory_events = config
            .memory_events_path
            .clone()
            .map(crate::memory_events::MemoryEvents::new);
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
            memory_events,
            pending_event: None,
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

    /// Claim recovered reference text for a real input, preserving slash commands.
    pub fn take_recovery_context(&mut self) -> Option<String> {
        self.config.recovery_context.take()
    }

    /// Where the launcher posts this session's plugin context changes.
    pub fn context_inbox(&self) -> &crate::ContextInbox {
        &self.config.context_updates
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
            if let Some(value) = self.memory_events.as_mut().and_then(|m| m.poll(false)) {
                self.buffer.push(value.clone());
                return Some(SessionEvent::RawOutput(value));
            }
            let event = if let Some(pending) = self.pending_event.take() {
                pending
            } else {
                let event_rx = self.event_rx.as_mut()?;
                tokio::select! {
                    event = event_rx.recv() => event,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)), if self.memory_events.is_some() => continue,
                }
            };
            if matches!(
                &event,
                None | Some(IoEvent::Exited { .. } | IoEvent::Error(_))
                    | Some(IoEvent::Classified(AgentOutput::NotFound))
            ) {
                if let Some(value) = self.memory_events.as_mut().and_then(|m| m.poll(true)) {
                    self.pending_event = Some(event);
                    self.buffer.push(value.clone());
                    return Some(SessionEvent::RawOutput(value));
                }
            }
            match event {
                Some(IoEvent::AgentStarted { pid }) => {
                    // Record the agent pid for `stop()`'s group-kill (#927);
                    // not a consumer-facing event, so keep looping.
                    self.agent_pid = pid;
                }
                Some(IoEvent::Classified(decision)) => {
                    // The per-agent I/O task has already run its
                    // `AgentOutputClassifier`; `Session` only maps the neutral
                    // decision to a `SessionEvent`. Both backends converge here.
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
                            // `suggestions` is already opaque wire JSON; forward
                            // it as-is. The proxy edge re-parses into its typed
                            // wire form, keeping `Session` protocol-neutral.
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
                                permission_suggestions: suggestions,
                            });
                        }
                        AgentOutput::PermissionCancelled { request_id } => {
                            // A late cancellation for an older request must not
                            // clear a newer prompt or change its waiting state.
                            if self
                                .pending_permission
                                .as_ref()
                                .is_some_and(|pending| pending.request_id == request_id)
                            {
                                self.pending_permission = None;
                                self.state = SessionState::Running;
                            }
                            return Some(SessionEvent::PermissionCancelled { request_id });
                        }
                        // User-visible output: buffer the raw value and forward it
                        // verbatim — the same path the codex `RawOutput` arm below
                        // takes. Both agents now converge on this neutral channel.
                        AgentOutput::Visible(value) => {
                            self.buffer.push(value.clone());
                            return Some(SessionEvent::RawOutput(value));
                        }
                        // Ephemeral tool-progress heartbeat: forward it as a
                        // live-status side-channel event WITHOUT buffering, so
                        // it is never persisted or replayed. See
                        // `AgentOutput::ToolProgress`.
                        // Live status: forwarded, never buffered — the same
                        // contract as ToolProgress below, for backends whose
                        // ephemerals are not tool heartbeats.
                        AgentOutput::Ephemeral(value) => {
                            return Some(SessionEvent::Ephemeral(value));
                        }
                        AgentOutput::ToolProgress {
                            tool_use_id,
                            parent_tool_use_id,
                            tool_name,
                            elapsed_time_seconds,
                            subagent_type,
                            subagent_retry,
                        } => {
                            return Some(SessionEvent::ToolProgress {
                                tool_use_id,
                                parent_tool_use_id,
                                tool_name,
                                elapsed_time_seconds,
                                subagent_type,
                                subagent_retry,
                            });
                        }
                        // Internal ack / nothing for the consumer — skip it and
                        // let the outer loop pull the next event.
                        AgentOutput::Noop => {}
                    }
                }
                Some(IoEvent::RawOutput(value)) => {
                    self.buffer.push(value.clone());
                    return Some(SessionEvent::RawOutput(value));
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
    /// [`IoCommand::UserInput::display_event`]). The proxy passes the inter-agent
    /// `PortalContent::AgentMessage` envelope here so the agent I/O task can
    /// synthesize the typed card into the replay buffer instead of relying on a
    /// CLI echo.
    pub async fn send_input_with_display(
        &mut self,
        content: serde_json::Value,
        display_event: Option<serde_json::Value>,
    ) -> Result<(), SessionError> {
        self.send_input_with_display_and_effort(content, display_event, None)
            .await
    }

    /// Like [`send_input_with_display`](Self::send_input_with_display), but
    /// carries an optional per-turn reasoning effort override for agents that
    /// expose one.
    pub async fn send_input_with_display_and_effort(
        &mut self,
        content: serde_json::Value,
        display_event: Option<serde_json::Value>,
        reasoning_effort: Option<shared::ReasoningEffort>,
    ) -> Result<(), SessionError> {
        let delivered_rx =
            self.enqueue_input_with_display(content, display_event, reasoning_effort)?;
        delivered_rx
            .await
            .map_err(|_| {
                SessionError::CommunicationError(
                    "I/O task closed before accepting input".to_string(),
                )
            })?
            .map_err(SessionError::Agent)
    }

    /// Queue user input to the agent and return a receiver that resolves when
    /// the agent has accepted it into its stream.
    ///
    /// This lets proxy connection loops keep processing output, reconnects, and
    /// heartbeats while Codex queues input behind an active turn. Callers that
    /// need the old blocking behavior can await the returned receiver; see
    /// [`send_input_with_display`](Self::send_input_with_display).
    pub fn enqueue_input_with_display(
        &mut self,
        content: serde_json::Value,
        display_event: Option<serde_json::Value>,
        reasoning_effort: Option<shared::ReasoningEffort>,
    ) -> Result<oneshot::Receiver<Result<(), String>>, SessionError> {
        if let SessionState::Exited { code } = self.state {
            return Err(SessionError::AlreadyExited(code));
        }

        let command_tx = self
            .command_tx
            .as_ref()
            .ok_or_else(|| SessionError::CommunicationError("I/O task closed".to_string()))?;
        let text = match &content {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let (delivered_tx, delivered_rx) = oneshot::channel();
        command_tx
            .send(IoCommand::UserInput {
                text,
                reasoning_effort,
                delivered: Some(delivered_tx),
                display_event: display_event.map(Box::new),
            })
            .map_err(|_| SessionError::CommunicationError("I/O task closed".to_string()))?;

        Ok(delivered_rx)
    }

    /// Respond to a pending permission request.
    ///
    /// Builds a neutral [`PermissionDecision`] and hands it to the I/O task via
    /// [`IoCommand::Permission`]; the per-agent task serializes it into its own
    /// protocol form (Claude `ControlResponse`, Codex `accept`/`decline`).
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
            // Map the consumer-facing response to the neutral decision. The
            // `remember` permissions are serialized to opaque JSON here so the
            // neutral form carries no `claude_codes` types.
            let decision = PermissionDecision {
                allow: response.allow,
                modified_input: response.input,
                remember: response
                    .permissions
                    .iter()
                    .map(|p| serde_json::to_value(p).unwrap_or(serde_json::Value::Null))
                    .collect(),
                reason: response.reason,
            };
            command_tx
                .send(IoCommand::Permission {
                    request_id: request_id.to_string(),
                    decision,
                })
                .map_err(|_| SessionError::CommunicationError("I/O task closed".to_string()))?;
        }

        self.pending_permission = None;
        self.state = SessionState::Running;

        Ok(())
    }

    /// Interrupt the agent's in-flight turn, if any. Sends
    /// [`IoCommand::Interrupt`]; the per-agent I/O task serializes it into its
    /// protocol's cancel form. A no-op when the I/O task has stopped or nothing
    /// is running (the agent ignores an interrupt with no active turn).
    pub async fn interrupt(&mut self) -> Result<(), SessionError> {
        if let Some(ref command_tx) = self.command_tx {
            command_tx
                .send(IoCommand::Interrupt)
                .map_err(|_| SessionError::CommunicationError("I/O task closed".to_string()))?;
        }
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

        if let (Some(timeout), Some(command_tx)) =
            (A::graceful_stop_timeout(), self.command_tx.as_ref())
        {
            let (completed_tx, completed_rx) = oneshot::channel();
            if command_tx
                .send(IoCommand::Shutdown {
                    completed: completed_tx,
                })
                .is_ok()
            {
                let _ = tokio::time::timeout(timeout, completed_rx).await;
            }
        }

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
    use std::time::Duration;

    // `STARTED` flips once the mock io-task is actually running (so we don't
    // abort it before it begins); `GUARD_DROPPED` flips when it's dropped, which
    // proves `stop` aborted it.
    static STARTED: AtomicBool = AtomicBool::new(false);
    static GUARD_DROPPED: AtomicBool = AtomicBool::new(false);
    static COMMAND_RECEIVED: AtomicBool = AtomicBool::new(false);

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

    /// An agent that accepts the command from the session channel but never
    /// resolves the per-input delivery signal, matching a Codex prompt queued
    /// behind an active turn.
    struct HangingDeliveryAgent;
    impl Agent for HangingDeliveryAgent {
        fn spawn_io_task(
            _config: SessionConfig,
            mut command_rx: mpsc::UnboundedReceiver<IoCommand>,
            _event_tx: mpsc::UnboundedSender<IoEvent>,
        ) -> Result<tokio::task::JoinHandle<()>, SessionError> {
            Ok(tokio::spawn(async move {
                loop {
                    match command_rx.recv().await {
                        Some(IoCommand::UserInput { delivered, .. }) => {
                            let _delivered = delivered;
                            COMMAND_RECEIVED.store(true, Ordering::SeqCst);
                            std::future::pending::<()>().await;
                        }
                        Some(IoCommand::Shutdown { completed }) => {
                            let _ = completed.send(());
                            break;
                        }
                        Some(IoCommand::Permission { .. } | IoCommand::Interrupt) => {}
                        None => break,
                    }
                }
            }))
        }
    }

    #[tokio::test]
    async fn permission_cancel_matches_id_and_never_replies_or_buffers() {
        struct PermissionAgent;
        impl Agent for PermissionAgent {
            fn spawn_io_task(
                _config: SessionConfig,
                mut commands: mpsc::UnboundedReceiver<IoCommand>,
                events: mpsc::UnboundedSender<IoEvent>,
            ) -> Result<tokio::task::JoinHandle<()>, SessionError> {
                Ok(tokio::spawn(async move {
                    events
                        .send(IoEvent::Classified(AgentOutput::PermissionRequest {
                            request_id: "current".into(),
                            tool_name: "Bash".into(),
                            input: serde_json::Value::Null,
                            suggestions: vec![],
                        }))
                        .unwrap();
                    for id in ["older", "current", "current"] {
                        events
                            .send(IoEvent::Classified(AgentOutput::PermissionCancelled {
                                request_id: id.into(),
                            }))
                            .unwrap();
                    }
                    match commands.recv().await {
                        Some(IoCommand::Shutdown { completed }) => {
                            let _ = completed.send(());
                        }
                        _ => panic!("Cancellation must not send a response"),
                    }
                }))
            }
        }
        let mut session = Session::<PermissionAgent>::new(SessionConfig::default())
            .await
            .unwrap();
        assert!(matches!(
            session.next_event().await,
            Some(SessionEvent::PermissionRequest { .. })
        ));
        assert!(
            matches!(session.next_event().await, Some(SessionEvent::PermissionCancelled { request_id }) if request_id == "older")
        );
        assert_eq!(session.pending_permission().unwrap().request_id, "current");
        assert!(matches!(session.state, SessionState::WaitingForPermission));
        for _ in 0..2 {
            assert!(
                matches!(session.next_event().await, Some(SessionEvent::PermissionCancelled { request_id }) if request_id == "current")
            );
            assert!(!session.has_pending_permission());
            assert!(matches!(session.state, SessionState::Running));
        }
        assert!(matches!(
            session
                .respond_permission("current", PermissionResponse::default())
                .await,
            Err(SessionError::InvalidPermissionResponse(_))
        ));
        assert!(session.buffer.is_empty());
        session.stop().await.unwrap();
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

    #[tokio::test]
    async fn recovered_context_is_claimed_once_without_sending_an_input() {
        // No command is sent by constructing a recovered session or claiming
        // its reference text. Only handle_input can enqueue a user's work.
        struct NoInputAgent;
        impl Agent for NoInputAgent {
            fn spawn_io_task(
                _config: SessionConfig,
                mut commands: mpsc::UnboundedReceiver<IoCommand>,
                _events: mpsc::UnboundedSender<IoEvent>,
            ) -> Result<tokio::task::JoinHandle<()>, SessionError> {
                Ok(tokio::spawn(async move {
                    if let Some(command) = commands.recv().await {
                        match command {
                            IoCommand::Shutdown { completed } => {
                                let _ = completed.send(());
                            }
                            _ => panic!("Recovery must not enqueue historical work"),
                        }
                    }
                }))
            }
        }
        let mut session = Session::<NoInputAgent>::new(SessionConfig {
            recovery_context: Some("quoted past request".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(
            session.take_recovery_context().as_deref(),
            Some("quoted past request")
        );
        assert_eq!(session.take_recovery_context(), None);

        // The launcher's handle and the session's are the same inbox.
        let launcher_side = session.config().context_updates.clone();
        assert!(!session.context_inbox().is_pending());
        launcher_side.post(1, "current".into(), "update".into());
        assert_eq!(session.context_inbox().pending().unwrap().text, "update");
        session.stop().await.unwrap();
    }

    #[tokio::test]
    async fn enqueue_input_returns_before_delivery_ack() {
        COMMAND_RECEIVED.store(false, Ordering::SeqCst);
        let mut session = Session::<HangingDeliveryAgent>::new(SessionConfig::default())
            .await
            .unwrap();

        let delivered = session
            .enqueue_input_with_display(serde_json::Value::String("queued".to_string()), None, None)
            .unwrap();

        while !COMMAND_RECEIVED.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }

        assert!(
            tokio::time::timeout(Duration::from_millis(25), delivered)
                .await
                .is_err(),
            "enqueue should return without waiting for the agent delivery ack"
        );

        session.stop().await.unwrap();
    }
}

#[cfg(test)]
mod memory_notice_tests {
    use super::*;

    struct QuietAgent;
    impl Agent for QuietAgent {
        fn spawn_io_task(
            config: SessionConfig,
            mut commands: mpsc::UnboundedReceiver<IoCommand>,
            events: mpsc::UnboundedSender<IoEvent>,
        ) -> Result<tokio::task::JoinHandle<()>, SessionError> {
            Ok(tokio::spawn(async move {
                // One command drives a terminal OOM; silence otherwise tests
                // that the session's own timer delivers nonterminal notices.
                if commands.recv().await.is_some() {
                    std::fs::write(config.memory_events_path.unwrap(), "oom_kill 1\n").unwrap();
                    let _ = events.send(IoEvent::Exited { code: 137 });
                }
            }))
        }
    }

    #[tokio::test]
    async fn oom_notice_arrives_while_idle_and_is_buffered() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("memory.events");
        std::fs::write(&path, "oom_kill 0\n").unwrap();
        let mut session = Session::<QuietAgent>::new(SessionConfig {
            memory_events_path: Some(path.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
        let writer = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            std::fs::write(path, "oom_kill 1\n").unwrap();
        });
        let event = tokio::time::timeout(std::time::Duration::from_secs(3), session.next_event())
            .await
            .unwrap();
        assert!(
            matches!(event, Some(SessionEvent::RawOutput(ref v)) if v.to_string().contains("Out of memory"))
        );
        assert_eq!(session.snapshot().pending_outputs.len(), 1);
        writer.await.unwrap();
        session.stop().await.unwrap();
    }

    #[tokio::test]
    async fn final_notice_precedes_exit_and_runtime_paths_do_not_persist() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("memory.events");
        std::fs::write(&path, "oom_kill 0\n").unwrap();
        let mut session = Session::<QuietAgent>::new(SessionConfig {
            memory_events_path: Some(path),
            runtime_command: Some("ephemeral-wrapper".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        let bytes = session.snapshot().to_bytes().unwrap();
        let restored = SessionSnapshot::from_bytes(&bytes).unwrap();
        assert!(restored.config.memory_events_path.is_none());
        assert!(restored.config.runtime_command.is_none());
        session
            .command_tx
            .as_ref()
            .unwrap()
            .send(IoCommand::Interrupt)
            .unwrap();
        let event = session.next_event().await;
        assert!(
            matches!(event, Some(SessionEvent::RawOutput(ref v)) if v.to_string().contains("Out of memory"))
        );
        assert!(matches!(
            session.next_event().await,
            Some(SessionEvent::Exited { code: 137 })
        ));
        assert!(session.next_event().await.is_none());
    }
}
