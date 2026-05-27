//! Generic, agent-agnostic session lifecycle.
//!
//! `Session<A: Agent>` owns the buffer, pending-permission state, and
//! per-session command/event channels. Per-agent backends (see
//! [`crate::agent::Agent`]) plug in their I/O task via `A::spawn_io_task`.

use std::marker::PhantomData;

use chrono::Utc;
use claude_codes::io::{ControlResponse, PermissionResult};
use claude_codes::{ClaudeInput, ClaudeOutput};
use tokio::sync::mpsc;
use uuid::Uuid;

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
    _agent: PhantomData<A>,
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
        let _handle = A::spawn_io_task(config.clone(), command_rx, event_tx)?;

        Ok(Self {
            id: config.session_id,
            config,
            command_tx: Some(command_tx),
            buffer,
            state: SessionState::Running,
            pending_permission: None,
            event_rx: Some(event_rx),
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

        let (command_tx, event_rx) = if snapshot.was_running {
            let (event_tx, event_rx) = mpsc::unbounded_channel();
            let (command_tx, command_rx) = mpsc::unbounded_channel();
            let _handle = A::spawn_io_task(config.clone(), command_rx, event_tx)?;
            (Some(command_tx), Some(event_rx))
        } else {
            (None, None)
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
                Some(IoEvent::Output(boxed_output)) => {
                    let output = *boxed_output;

                    // Buffer the output.
                    let output_value = serde_json::to_value(&output).unwrap_or_default();
                    self.buffer.push(output_value);

                    // Check for "No conversation found" error (session not found locally).
                    if let ClaudeOutput::Result(ref res) = output {
                        if res.is_error
                            && res
                                .errors
                                .iter()
                                .any(|e| e.contains("No conversation found"))
                        {
                            self.state = SessionState::Exited { code: 1 };
                            self.command_tx = None;
                            self.event_rx = None;
                            return Some(SessionEvent::SessionNotFound);
                        }
                    }

                    // Permission requests are emitted as `PermissionRequest`,
                    // not `Output`, so the consumer doesn't see them twice.
                    if let ClaudeOutput::ControlRequest(ref req) = output {
                        if let claude_codes::io::ControlRequestPayload::CanUseTool(ref tool_req) =
                            req.request
                        {
                            let request_id = req.request_id.clone();
                            self.pending_permission = Some(PendingPermission {
                                request_id: request_id.clone(),
                                tool_name: tool_req.tool_name.clone(),
                                input: tool_req.input.clone(),
                                requested_at: Utc::now(),
                            });
                            self.state = SessionState::WaitingForPermission;

                            return Some(SessionEvent::PermissionRequest {
                                request_id,
                                tool_name: tool_req.tool_name.clone(),
                                input: tool_req.input.clone(),
                                permission_suggestions: tool_req.permission_suggestions.clone(),
                            });
                        }
                    }

                    // Skip ControlResponse (acks from Claude, not useful to callers).
                    if matches!(output, ClaudeOutput::ControlResponse(_)) {
                        continue;
                    }

                    return Some(SessionEvent::Output(Box::new(output)));
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
        if let SessionState::Exited { code } = self.state {
            return Err(SessionError::AlreadyExited(code));
        }

        if let Some(ref command_tx) = self.command_tx {
            let text = match &content {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let input = ClaudeInput::user_message(text, self.id);
            command_tx
                .send(IoCommand::Input(input))
                .map_err(|_| SessionError::CommunicationError("I/O task closed".to_string()))?;
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

    /// Gracefully stop the session.
    pub async fn stop(&mut self) -> Result<(), SessionError> {
        // Dropping the command_tx will cause the I/O task to exit,
        // which in turn will drop the client and terminate the process.
        self.command_tx = None;
        self.event_rx = None;
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
