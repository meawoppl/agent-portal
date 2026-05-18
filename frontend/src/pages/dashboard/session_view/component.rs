//! SessionView component - Main terminal view for a single session

use crate::components::message_renderer::types::{ClaudeMessage, ContentBlock};
use crate::components::message_renderer::MessageRenderer;
use crate::components::{group_messages, MessageGroupRenderer, VoiceInput};
use crate::utils;
use gloo::timers::callback::Timeout;
use gloo_net::http::Request;
use shared::api::ErrorMessage;
use shared::{ClientToServer, SendMode, SessionInfo};
use uuid::Uuid;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{ClipboardEvent, DragEvent, Element, HtmlTextAreaElement, KeyboardEvent};
use yew::prelude::*;

/// Wire `type` tag for a typed [`ClaudeMessage`] variant. Centralizes the
/// variant-to-tag mapping so call sites that still trade in `msg_type: String`
/// can derive it from the typed enum instead of poking `.get("type")`.
fn message_type_tag(m: &ClaudeMessage) -> &'static str {
    match m {
        ClaudeMessage::System(_) => "system",
        ClaudeMessage::Assistant(_) => "assistant",
        ClaudeMessage::Result(_) => "result",
        ClaudeMessage::User(_) => "user",
        ClaudeMessage::Error(_) => "error",
        ClaudeMessage::Portal(_) => "portal",
        ClaudeMessage::RateLimitEvent(_) => "rate_limit_event",
        ClaudeMessage::Unknown => "unknown",
    }
}

/// Extract the user-text payload from a typed user message for pending-send
/// echo matching. Returns the top-level `content` string when present (used by
/// the frontend's optimistic-send synthesizer and the codex shim's synthesized
/// echo) and otherwise concatenates `ContentBlock::Text` blocks from
/// `message.content` (the shape Claude's `--replay-user-messages` emits).
fn extract_user_text(m: &ClaudeMessage) -> Option<String> {
    let ClaudeMessage::User(u) = m else {
        return None;
    };
    if let Some(text) = u.content.as_ref() {
        return Some(text.clone());
    }
    let blocks = u.message.as_ref().and_then(|m| m.content.as_ref())?;
    let texts: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join(""))
    }
}

/// Compute the next `should_autoscroll` value when the scroll listener
/// reports a new at-bottom reading. Returns `None` when no transition has
/// occurred (caller should skip the re-render) and `Some(new_value)` when
/// the flag flips. The transition gate lives here, outside the component,
/// so it can be unit-tested without a Yew `Context`.
fn autoscroll_transition(current: bool, new_at_bottom: bool) -> Option<bool> {
    if current == new_at_bottom {
        None
    } else {
        Some(new_at_bottom)
    }
}

/// Check if a Claude session is awaiting user input by scanning messages
/// backwards. Skips noise types (portal, error, system, rate_limit_event)
/// and returns true if "result" is found before "user" or "assistant".
fn is_claude_awaiting(messages: impl DoubleEndedIterator<Item = impl AsRef<str>>) -> bool {
    messages
        .rev()
        .find_map(|msg| {
            serde_json::from_str::<ClaudeMessage>(msg.as_ref())
                .ok()
                .filter(|m| {
                    matches!(
                        m,
                        ClaudeMessage::Result(_)
                            | ClaudeMessage::Assistant(_)
                            | ClaudeMessage::User(_)
                    )
                })
                .map(|m| message_type_tag(&m).to_string())
        })
        .is_some_and(|t| t == "result")
}

use super::history::CommandHistory;
use super::permission_handler::{
    build_permission_response, refocus_textarea, PermissionHandler, PermissionResponseKind,
};
use super::tasks_panel::{TaskEvent, TaskStatus, TasksInbound, TasksPanel};
use super::types::{PendingPermission, WsSender, MAX_MESSAGES_PER_SESSION};
use super::websocket::{connect_websocket, send_message, WsEvent};
use crate::pages::dashboard::types::{calculate_backoff, MessageData, MessagesResponse};

/// Props for the SessionView component
#[derive(Properties, PartialEq)]
pub struct SessionViewProps {
    pub session: SessionInfo,
    pub focused: bool,
    pub on_awaiting_change: Callback<(Uuid, bool)>,
    pub on_cost_change: Callback<(Uuid, f64)>,
    pub on_connected_change: Callback<(Uuid, bool)>,
    pub on_message_sent: Callback<Uuid>,
    #[allow(clippy::type_complexity)]
    pub on_branch_change: Callback<(Uuid, Option<String>, Option<String>, Option<String>)>,
    #[prop_or_default]
    pub on_activity: Callback<(Uuid, String, f64)>,
    #[prop_or(false)]
    pub voice_enabled: bool,
    #[prop_or_default]
    pub current_user_id: Option<String>,
    #[prop_or(0)]
    pub interrupt_signal: u32,
}

/// Messages for the SessionView component
pub enum SessionViewMsg {
    SendInput,
    UpdateInput(String),
    LoadHistory(Vec<MessageData>, Option<String>),
    ReceivedOutput(String),
    WebSocketConnected(WsSender),
    WebSocketError(String),
    AttemptReconnect,
    CheckAwaiting,
    ClearCostFlash,
    BranchChanged(Option<String>, Option<String>, Option<String>),
    HistoryUp,
    HistoryDown,
    VoiceRecordingChanged(bool),
    VoiceTranscription(String),
    VoiceInterimTranscription(String),
    VoiceError(String),
    ToggleVoice,
    /// PermissionHandler is mounted and handed us its inbound-request
    /// dispatcher. We store it so live `WsEvent::Permission` frames can be
    /// forwarded without the parent owning any permission state.
    PermissionDispatcherRegistered(Callback<PendingPermission>),
    /// PermissionHandler reports a transition in its pending state. We
    /// track the flag for the `is_awaiting` computation.
    PermissionPendingChanged(bool),
    /// PermissionHandler emitted a typed answer for the user. We translate
    /// it into the wire frame here so the WS plumbing stays in this file.
    PermissionAnswered(String, PermissionResponseKind),
    /// Handle WebSocket event from connection
    WsEvent(WsEvent),
    /// Toggle send mode dropdown visibility
    ToggleSendModeDropdown,
    /// Close send mode dropdown (click outside)
    CloseSendModeDropdown,
    /// Send with wiggum mode
    SendWiggum,
    /// User selected files via "Send with attachment(s)" dropdown
    FilesSelected(Vec<web_sys::File>),
    /// File upload progress (0.0-1.0)
    FileUploadProgress(f32),
    /// File upload completed — sends follow-up message
    FileUploaded(String),
    /// File upload failed
    FileUploadError(String),
    /// User dragged files over the input area
    DragEnter,
    /// User dragged files out of the input area
    DragLeave,
    /// Dismiss the upload bar after completion
    UploadDismiss,
    /// No-op (used by keydown/paste handlers that need to return a message)
    Noop,
    /// TasksPanel is mounted and handed us its inbound-event dispatcher.
    /// We store it so live `WsEvent::Output` task events and REST replay
    /// task events can be forwarded without the parent owning any task
    /// state.
    TasksDispatcherRegistered(Callback<TasksInbound>),
    /// Send an interrupt to stop the current Claude response
    Interrupt,
    /// Scroll listener reports the current at-bottom state. The `update()`
    /// arm flips `should_autoscroll` and re-renders only when the value
    /// changes, so the closure can dispatch on every scroll event without
    /// per-event re-renders.
    AutoscrollChanged(bool),
    /// User clicked the "Jump to live" pill: resume tailing and scroll to bottom.
    JumpToLive,
}

/// SessionView - Main terminal view for a single session
pub struct SessionView {
    messages: Vec<String>,
    ws_connected: bool,
    ws_sender: Option<WsSender>,
    messages_ref: NodeRef,
    input_ref: NodeRef,
    should_autoscroll: bool,
    #[allow(dead_code)]
    scroll_listener: Option<Closure<dyn Fn()>>,
    was_focused: bool,
    total_cost: f64,
    cost_flash: bool,
    /// Dispatcher into the mounted `PermissionHandler`. Stored once at child
    /// `create` time via `PermissionDispatcherRegistered`; live permission
    /// frames off the wire are forwarded through it so this component holds
    /// zero permission UI state itself.
    permission_dispatcher: Option<Callback<PendingPermission>>,
    /// Mirror of the handler's pending state, kept in sync via
    /// `PermissionPendingChanged`. Feeds the `is_awaiting` computation.
    has_pending_permission: bool,
    /// Snapshot of the last permission request forwarded to the handler.
    /// Kept so the wire-frame translation in `PermissionAnswered` can read
    /// the original `input` / `permission_suggestions` without the child
    /// having to echo them back across the callback.
    last_permission_request: Option<PendingPermission>,
    reconnect_attempt: u32,
    #[allow(dead_code)]
    reconnect_timer: Option<Timeout>,
    command_history: CommandHistory,
    is_recording: bool,
    interim_transcription: Option<String>,
    last_message_timestamp: Option<String>,
    voice_button_ref: NodeRef,
    send_mode_dropdown_open: bool,
    file_input_ref: NodeRef,
    upload_progress: Option<f32>,
    upload_files: Vec<(String, u64)>,
    upload_departing: bool,
    #[allow(dead_code)]
    upload_dismiss_timer: Option<Timeout>,
    drag_hover: bool,
    /// Dispatcher into the mounted `TasksPanel`. Stored once at child
    /// `create` time via `TasksDispatcherRegistered`; live task events
    /// derived from `WsEvent::Output` and replay events derived from the
    /// REST `LoadHistory` path are forwarded through it so this component
    /// holds zero task UI state itself.
    tasks_dispatcher: Option<Callback<TasksInbound>>,
    /// Tracks textarea content so it can be restored after reconnection re-renders
    input_text: String,
    /// Messages sent but not yet confirmed by the server echo
    pending_sends: Vec<String>,
}

impl Component for SessionView {
    type Message = SessionViewMsg;
    type Properties = SessionViewProps;

    fn create(ctx: &Context<Self>) -> Self {
        let link = ctx.link().clone();
        let session_id = ctx.props().session.id;
        let on_awaiting_change = ctx.props().on_awaiting_change.clone();

        // Fetch existing messages via REST, then connect WebSocket
        spawn_local(async move {
            let mut last_message_time: Option<String> = None;
            let api_endpoint = utils::api_url(&format!("/api/sessions/{}/messages", session_id));

            if let Ok(response) = Request::get(&api_endpoint).send().await {
                if let Ok(data) = response.json::<MessagesResponse>().await {
                    let is_awaiting = is_claude_awaiting(data.messages.iter().map(|m| &m.content));
                    on_awaiting_change.emit((session_id, is_awaiting));

                    last_message_time = data.messages.last().map(|m| m.created_at.clone());

                    link.send_message(SessionViewMsg::LoadHistory(
                        data.messages,
                        last_message_time.clone(),
                    ));
                }
            }

            // Connect WebSocket with event callback
            let ws_link = link.clone();
            let on_event = Callback::from(move |event: WsEvent| {
                ws_link.send_message(SessionViewMsg::WsEvent(event));
            });
            connect_websocket(session_id, last_message_time, false, on_event);
        });

        Self {
            messages: vec![],
            ws_connected: false,
            ws_sender: None,
            messages_ref: NodeRef::default(),
            input_ref: NodeRef::default(),
            should_autoscroll: true,
            scroll_listener: None,
            was_focused: ctx.props().focused,
            total_cost: 0.0,
            cost_flash: false,
            permission_dispatcher: None,
            has_pending_permission: false,
            last_permission_request: None,
            reconnect_attempt: 0,
            reconnect_timer: None,
            command_history: CommandHistory::for_session(ctx.props().session.id),
            is_recording: false,
            interim_transcription: None,
            last_message_timestamp: None,
            voice_button_ref: NodeRef::default(),
            send_mode_dropdown_open: false,
            file_input_ref: NodeRef::default(),
            upload_progress: None,
            upload_files: Vec::new(),
            upload_departing: false,
            upload_dismiss_timer: None,
            drag_hover: false,
            tasks_dispatcher: None,
            input_text: String::new(),
            pending_sends: Vec::new(),
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, old_props: &Self::Properties) -> bool {
        let now_focused = ctx.props().focused;
        let became_focused = now_focused && !self.was_focused;
        self.was_focused = now_focused;

        if became_focused {
            if let Some(input) = self.input_ref.cast::<HtmlTextAreaElement>() {
                let _ = input.focus();
            }
        }

        // Detect interrupt signal change on the focused session
        if now_focused
            && ctx.props().interrupt_signal != old_props.interrupt_signal
            && ctx.props().interrupt_signal > 0
        {
            ctx.link().send_message(SessionViewMsg::Interrupt);
        }

        true
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        if first_render && ctx.props().focused {
            if let Some(input) = self.input_ref.cast::<HtmlTextAreaElement>() {
                let _ = input.focus();
            }
        }

        // Restore textarea content if it was cleared during a re-render (e.g. reconnection)
        if !self.input_text.is_empty() {
            if let Some(el) = self.input_ref.cast::<HtmlTextAreaElement>() {
                if el.value().is_empty() {
                    self.set_input_text(&self.input_text.clone());
                }
            }
        }

        if let Some(element) = self.messages_ref.cast::<Element>() {
            if first_render {
                let element_clone = element.clone();
                let link = ctx.link().clone();

                let closure = Closure::new(move || {
                    let scroll_top = element_clone.scroll_top();
                    let scroll_height = element_clone.scroll_height();
                    let client_height = element_clone.client_height();
                    let at_bottom = scroll_height - scroll_top - client_height < 50;
                    link.send_message(SessionViewMsg::AutoscrollChanged(at_bottom));
                });

                let _ = element
                    .add_event_listener_with_callback("scroll", closure.as_ref().unchecked_ref());

                self.scroll_listener = Some(closure);
            }

            if self.should_autoscroll {
                element.set_scroll_top(element.scroll_height());
            }
        }
    }

    fn update(&mut self, ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            SessionViewMsg::WsEvent(event) => self.handle_ws_event(ctx, event),
            SessionViewMsg::UpdateInput(value) => {
                // Textarea is uncontrolled — the DOM already has the new value.
                // Track in state so we can restore after reconnection re-renders.
                self.input_text = value;
                false
            }
            SessionViewMsg::SendInput => self.handle_send_input_with_mode(ctx, SendMode::Normal),
            SessionViewMsg::LoadHistory(mut messages, last_timestamp) => {
                if messages.len() > MAX_MESSAGES_PER_SESSION {
                    let excess = messages.len() - MAX_MESSAGES_PER_SESSION;
                    messages.drain(0..excess);
                }
                let session_id = ctx.props().session.id;
                self.dispatch_tasks(TasksInbound::ClearForReplay);
                for msg in &messages {
                    let mut msg_type = "unknown".to_string();
                    if let Ok(claude_msg) =
                        serde_json::from_str::<shared::ClaudeOutput>(&msg.content)
                    {
                        msg_type = claude_msg.message_type();
                        if let shared::ClaudeOutput::System(sys) = &claude_msg {
                            if let Some(status) = sys.as_status() {
                                if status.status.as_ref().map(|s| s.as_str()) == Some("compacting")
                                {
                                    msg_type = "compaction_start".to_string();
                                }
                            } else if shared::is_compaction_boundary(sys) {
                                msg_type = "compaction_end".to_string();
                            } else if sys.as_task_started().is_some() {
                                msg_type = "task_start".to_string();
                            } else if sys.as_task_notification().is_some() {
                                msg_type = "task_end".to_string();
                            }
                        }
                        for ev in derive_task_events(&claude_msg, &msg.created_at, false) {
                            self.dispatch_tasks(TasksInbound::Replay(ev));
                        }
                    } else if let Ok(parsed) = serde_json::from_str::<ClaudeMessage>(&msg.content) {
                        msg_type = message_type_tag(&parsed).to_string();
                    }
                    let ts_ms = js_sys::Date::parse(&msg.created_at);
                    if ts_ms.is_finite() {
                        ctx.props().on_activity.emit((session_id, msg_type, ts_ms));
                    }
                }
                self.messages = messages
                    .into_iter()
                    .map(|m| {
                        if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&m.content) {
                            if let Some(obj) = val.as_object_mut() {
                                // Inject _sender into user messages from API metadata
                                if m.role == "user"
                                    && (m.user_id.is_some() || m.sender_name.is_some())
                                {
                                    obj.insert(
                                        "_sender".to_string(),
                                        serde_json::json!({
                                            "user_id": m.user_id.unwrap_or_default(),
                                            "name": m.sender_name.unwrap_or_default(),
                                        }),
                                    );
                                }
                                // Inject _created_at for tooltip display
                                obj.insert(
                                    "_created_at".to_string(),
                                    serde_json::Value::String(m.created_at.clone()),
                                );
                            }
                            return val.to_string();
                        }
                        m.content
                    })
                    .collect();
                self.last_message_timestamp = last_timestamp;
                ctx.link().send_message(SessionViewMsg::CheckAwaiting);
                true
            }
            SessionViewMsg::ReceivedOutput(output) => self.handle_received_output(ctx, output),
            SessionViewMsg::ClearCostFlash => {
                self.cost_flash = false;
                true
            }
            SessionViewMsg::PermissionDispatcherRegistered(dispatcher) => {
                self.permission_dispatcher = Some(dispatcher);
                false
            }
            SessionViewMsg::PermissionPendingChanged(pending) => {
                self.has_pending_permission = pending;
                if pending {
                    let session_id = ctx.props().session.id;
                    ctx.props().on_awaiting_change.emit((session_id, true));
                } else {
                    ctx.link().send_message(SessionViewMsg::CheckAwaiting);
                }
                false
            }
            SessionViewMsg::PermissionAnswered(request_id, kind) => {
                let Some(perm) = self.last_permission_request.take() else {
                    return false;
                };
                if let Some(ref sender) = self.ws_sender {
                    let frame = build_permission_response(request_id, kind, &perm);
                    send_message(sender, ClientToServer::PermissionResponse(frame));
                }
                refocus_textarea(&self.input_ref);
                false
            }
            SessionViewMsg::WebSocketConnected(sender) => {
                self.ws_connected = true;
                self.ws_sender = Some(sender);
                self.reconnect_attempt = 0;
                self.reconnect_timer = None;
                let session_id = ctx.props().session.id;
                ctx.props().on_connected_change.emit((session_id, true));
                true
            }
            SessionViewMsg::WebSocketError(err) => self.handle_ws_error(ctx, err),
            SessionViewMsg::AttemptReconnect => {
                self.attempt_reconnect(ctx);
                false
            }
            SessionViewMsg::CheckAwaiting => {
                let is_codex = ctx.props().session.agent_type == shared::AgentType::Codex;
                let is_result_awaiting = if is_codex {
                    // For Codex: search backwards for terminal events
                    // turn.completed / turn.failed = awaiting, item.* = working
                    self.messages
                        .iter()
                        .rev()
                        .find_map(|msg| {
                            crate::components::codex_renderer::is_codex_terminal_event(msg)
                        })
                        .unwrap_or(false)
                } else {
                    is_claude_awaiting(self.messages.iter())
                };
                let is_awaiting = is_result_awaiting || self.has_pending_permission;
                let session_id = ctx.props().session.id;
                ctx.props()
                    .on_awaiting_change
                    .emit((session_id, is_awaiting));
                false
            }
            SessionViewMsg::BranchChanged(branch, pr_url, repo_url) => {
                let session_id = ctx.props().session.id;
                ctx.props()
                    .on_branch_change
                    .emit((session_id, branch, pr_url, repo_url));
                false
            }
            SessionViewMsg::HistoryUp => {
                let current = self.get_input_text();
                if let Some(cmd) = self.command_history.navigate_up(&current) {
                    self.set_input_text(&cmd);
                }
                false
            }
            SessionViewMsg::HistoryDown => {
                if let Some(cmd) = self.command_history.navigate_down() {
                    self.set_input_text(&cmd);
                }
                false
            }
            SessionViewMsg::VoiceRecordingChanged(recording) => {
                self.is_recording = recording;
                if !recording {
                    self.interim_transcription = None;
                }
                true
            }
            SessionViewMsg::VoiceTranscription(text) => {
                self.interim_transcription = None;
                if !text.is_empty() {
                    let current = self.get_input_text();
                    let new_value = if current.is_empty() {
                        text
                    } else {
                        format!("{} {}", current, text)
                    };
                    self.set_input_text(&new_value);
                    ctx.link().send_message(SessionViewMsg::SendInput);
                }
                true
            }
            SessionViewMsg::VoiceInterimTranscription(text) => {
                self.interim_transcription = if text.is_empty() { None } else { Some(text) };
                true
            }
            SessionViewMsg::VoiceError(err) => {
                log::error!("Voice error: {}", err);
                self.is_recording = false;
                self.interim_transcription = None;
                true
            }
            SessionViewMsg::ToggleVoice => {
                if let Some(button) = self.voice_button_ref.cast::<web_sys::HtmlElement>() {
                    button.click();
                }
                false
            }
            SessionViewMsg::ToggleSendModeDropdown => {
                self.send_mode_dropdown_open = !self.send_mode_dropdown_open;
                true
            }
            SessionViewMsg::CloseSendModeDropdown => {
                if self.send_mode_dropdown_open {
                    self.send_mode_dropdown_open = false;
                    true
                } else {
                    false
                }
            }
            SessionViewMsg::SendWiggum => {
                self.send_mode_dropdown_open = false;
                self.handle_send_input_with_mode(ctx, SendMode::Wiggum)
            }
            SessionViewMsg::FilesSelected(files) => {
                // Close dropdown, clear drag state, and start uploading all files
                self.send_mode_dropdown_open = false;
                self.drag_hover = false;
                self.upload_progress = Some(0.0);
                self.upload_files = files.iter().map(|f| (f.name(), f.size() as u64)).collect();
                let link = ctx.link().clone();
                let sender = self.ws_sender.clone();
                let user_input = self.get_input_text().trim().to_string();
                self.set_input_text("");
                self.input_text.clear();
                if !user_input.is_empty() {
                    self.command_history.push(user_input.clone());
                }
                let session_id = ctx.props().session.id;
                ctx.props().on_message_sent.emit(session_id);

                spawn_local(async move {
                    let Some(ref ws) = sender else {
                        link.send_message(SessionViewMsg::FileUploadError(
                            "WebSocket not connected".into(),
                        ));
                        return;
                    };

                    let mut uploaded_files: Vec<(String, u64)> = Vec::new();
                    let total_files = files.len();

                    for (file_idx, file) in files.iter().enumerate() {
                        let file_name = file.name();
                        let file_size = file.size() as u64;
                        let content_type = file.type_();

                        let array_buffer =
                            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                                Ok(buf) => buf,
                                Err(_) => {
                                    link.send_message(SessionViewMsg::FileUploadError(format!(
                                        "Failed to read file: {}",
                                        file_name
                                    )));
                                    return;
                                }
                            };
                        let uint8_array = js_sys::Uint8Array::new(&array_buffer);
                        let bytes = uint8_array.to_vec();

                        const CHUNK_SIZE: usize = 1024;
                        let total_chunks = bytes.len().div_ceil(CHUNK_SIZE).max(1) as u32;
                        let upload_id = Uuid::new_v4().to_string();

                        let ct = if content_type.is_empty() {
                            "application/octet-stream".to_string()
                        } else {
                            content_type
                        };

                        send_message(
                            ws,
                            ClientToServer::FileUploadStart(shared::FileUploadStartFields {
                                upload_id: upload_id.clone(),
                                filename: file_name.clone(),
                                content_type: ct,
                                total_chunks,
                                total_size: file_size,
                            }),
                        );

                        for i in 0..total_chunks {
                            let start = i as usize * CHUNK_SIZE;
                            let end = ((i as usize + 1) * CHUNK_SIZE).min(bytes.len());
                            let chunk = &bytes[start..end];
                            let encoded = base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                chunk,
                            );

                            send_message(
                                ws,
                                ClientToServer::FileUploadChunk(shared::FileUploadChunkFields {
                                    upload_id: upload_id.clone(),
                                    chunk_index: i,
                                    data: encoded,
                                }),
                            );
                        }

                        uploaded_files.push((file_name, file_size));

                        // Update progress across all files
                        let overall_progress = (file_idx + 1) as f32 / total_files as f32;
                        link.send_message(SessionViewMsg::FileUploadProgress(overall_progress));
                    }

                    // Build the combined message: user text + formatted file list
                    let file_list: Vec<String> = uploaded_files
                        .iter()
                        .map(|(name, size)| {
                            let human_size = if *size < 1024 {
                                format!("{} B", size)
                            } else if *size < 1024 * 1024 {
                                format!("{:.1} KB", *size as f64 / 1024.0)
                            } else {
                                format!("{:.1} MB", *size as f64 / (1024.0 * 1024.0))
                            };
                            format!("- {} ({})", name, human_size)
                        })
                        .collect();

                    let combined = if user_input.is_empty() {
                        format!(
                            "I've uploaded the following files to your working directory:\n{}",
                            file_list.join("\n")
                        )
                    } else {
                        format!(
                            "{}\n\nI've uploaded the following files to your working directory:\n{}",
                            user_input,
                            file_list.join("\n")
                        )
                    };

                    send_message(
                        ws,
                        ClientToServer::ClaudeInput {
                            content: serde_json::Value::String(combined),
                            send_mode: None,
                        },
                    );

                    link.send_message(SessionViewMsg::FileUploaded(
                        uploaded_files
                            .iter()
                            .map(|(n, _)| n.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    ));
                });

                true
            }
            SessionViewMsg::FileUploadProgress(progress) => {
                self.upload_progress = Some(progress);
                true
            }
            SessionViewMsg::FileUploaded(_filename) => {
                self.upload_progress = Some(1.0);
                self.upload_departing = false;
                let link = ctx.link().clone();
                self.upload_dismiss_timer = Some(Timeout::new(2_000, move || {
                    link.send_message(SessionViewMsg::UploadDismiss);
                }));
                true
            }
            SessionViewMsg::UploadDismiss => {
                self.upload_departing = true;
                self.upload_dismiss_timer = None;
                let link = ctx.link().clone();
                // Clear after the CSS collapse animation finishes
                self.upload_dismiss_timer = Some(Timeout::new(400, move || {
                    link.send_message(SessionViewMsg::FileUploadError("dismiss".into()));
                }));
                true
            }
            SessionViewMsg::FileUploadError(_err) => {
                self.upload_progress = None;
                self.upload_files.clear();
                self.upload_departing = false;
                self.upload_dismiss_timer = None;
                true
            }
            SessionViewMsg::DragEnter => {
                self.drag_hover = true;
                true
            }
            SessionViewMsg::DragLeave => {
                self.drag_hover = false;
                true
            }
            SessionViewMsg::Noop => false,
            SessionViewMsg::TasksDispatcherRegistered(dispatcher) => {
                self.tasks_dispatcher = Some(dispatcher);
                false
            }
            SessionViewMsg::Interrupt => {
                if let Some(ref sender) = self.ws_sender {
                    log::info!("Sending interrupt to session");
                    send_message(sender, ClientToServer::Interrupt);
                }
                false
            }
            SessionViewMsg::AutoscrollChanged(at_bottom) => {
                // Scroll events fire continuously; only re-render on a real
                // transition so long message lists stay performant.
                match autoscroll_transition(self.should_autoscroll, at_bottom) {
                    Some(next) => {
                        self.should_autoscroll = next;
                        true
                    }
                    None => false,
                }
            }
            SessionViewMsg::JumpToLive => {
                self.should_autoscroll = true;
                // rendered() will see the flag and snap to bottom on the
                // next paint.
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();

        let handle_submit = link.callback(|e: SubmitEvent| {
            e.prevent_default();
            SessionViewMsg::SendInput
        });

        let handle_input = link.callback(|e: InputEvent| {
            let input: HtmlTextAreaElement = e.target_unchecked_into();
            let el: &Element = input.as_ref();
            // Measure content height with overflow hidden to prevent scrollbar
            // from narrowing the text area and causing layout bounce.
            el.set_attribute("style", "height: 0; overflow-y: hidden")
                .ok();
            el.set_attribute("style", &format!("height: {}px", input.scroll_height()))
                .ok();
            SessionViewMsg::UpdateInput(input.value())
        });

        let handle_keydown = link.callback(|e: KeyboardEvent| {
            if e.ctrl_key() && e.key().to_lowercase() == "m" {
                e.prevent_default();
                return SessionViewMsg::ToggleVoice;
            }

            match e.key().as_str() {
                "Enter" if !e.shift_key() => {
                    // Enter without Shift submits
                    e.prevent_default();
                    SessionViewMsg::SendInput
                }
                "Enter" => {
                    // Shift+Enter inserts newline (default behavior)
                    SessionViewMsg::Noop
                }
                "ArrowUp" => {
                    // Only trigger history if cursor is on the first line of the textarea.
                    // Otherwise, let the arrow key move the cursor normally.
                    let target: HtmlTextAreaElement = e.target_unchecked_into();
                    let value = target.value();
                    let cursor = target.selection_start().ok().flatten().unwrap_or(0) as usize;
                    let on_first_line = !value[..cursor.min(value.len())].contains('\n');
                    if on_first_line {
                        e.prevent_default();
                        SessionViewMsg::HistoryUp
                    } else {
                        SessionViewMsg::Noop
                    }
                }
                "ArrowDown" => {
                    // Only trigger history if cursor is on the last line of the textarea.
                    let target: HtmlTextAreaElement = e.target_unchecked_into();
                    let value = target.value();
                    let cursor = target.selection_start().ok().flatten().unwrap_or(0) as usize;
                    let on_last_line = !value[cursor.min(value.len())..].contains('\n');
                    if on_last_line {
                        e.prevent_default();
                        SessionViewMsg::HistoryDown
                    } else {
                        SessionViewMsg::Noop
                    }
                }
                _ => SessionViewMsg::Noop,
            }
        });

        let close_dropdown = link.callback(|_| SessionViewMsg::CloseSendModeDropdown);

        let handle_paste = link.callback(|e: Event| {
            let e: ClipboardEvent = e.unchecked_into();
            if let Some(data) = e.clipboard_data() {
                let items = data.items();
                let files: Vec<web_sys::File> = (0..items.length())
                    .filter_map(|i: u32| items.get(i))
                    .filter(|item: &web_sys::DataTransferItem| item.kind() == "file")
                    .filter_map(|item: web_sys::DataTransferItem| item.get_as_file().ok().flatten())
                    .collect();
                if !files.is_empty() {
                    e.prevent_default();
                    return SessionViewMsg::FilesSelected(files);
                }
            }
            SessionViewMsg::Noop
        });

        let handle_dragover = link.callback(|e: DragEvent| {
            e.prevent_default();
            SessionViewMsg::DragEnter
        });

        let handle_dragleave = link.callback(|e: DragEvent| {
            e.prevent_default();
            SessionViewMsg::DragLeave
        });

        let handle_drop = link.callback(|e: DragEvent| {
            e.prevent_default();
            let files: Vec<web_sys::File> = e
                .data_transfer()
                .and_then(|dt| dt.files())
                .map(|fl| (0..fl.length()).filter_map(|i: u32| fl.get(i)).collect())
                .unwrap_or_default();
            if !files.is_empty() {
                SessionViewMsg::FilesSelected(files)
            } else {
                SessionViewMsg::DragLeave
            }
        });

        let drag_hint = if self.drag_hover {
            "session-view-input drag-hover"
        } else {
            "session-view-input"
        };

        let is_tailing = self.should_autoscroll;
        let on_jump_to_live = link.callback(|e: MouseEvent| {
            e.stop_propagation();
            SessionViewMsg::JumpToLive
        });

        html! {
            <div class="session-view" onclick={close_dropdown}>
                <div class="session-view-scroll-area">
                    <div class="session-view-messages" ref={self.messages_ref.clone()}>
                        {
                            group_messages(
                                &self.messages,
                                ctx.props().session.agent_type,
                                ctx.props().current_user_id.as_deref(),
                            ).into_iter().enumerate().map(|(i, group)| {
                                let key = group.key(i);
                                html! { <MessageGroupRenderer {key} group={group} session_id={Some(ctx.props().session.id)} agent_type={ctx.props().session.agent_type} current_user_id={ctx.props().current_user_id.clone()} /> }
                            }).collect::<Html>()
                        }
                        { for self.pending_sends.iter().enumerate().map(|(i, json)| {
                            html! { <MessageRenderer key={format!("p{}", i)} json={json.clone()} session_id={Some(ctx.props().session.id)} agent_type={ctx.props().session.agent_type} current_user_id={ctx.props().current_user_id.clone()} /> }
                        })}
                    </div>
                    if !is_tailing {
                        <button
                            class="jump-to-live-pill"
                            onclick={on_jump_to_live}
                            title="Resume live tailing of new messages"
                        >
                            { "Jump to live ↓" }
                        </button>
                    }
                    { self.render_tasks_panel(ctx) }
                </div>

                { self.render_permission_handler(ctx) }
                { self.render_upload_bar() }

                <form
                    class={drag_hint}
                    onsubmit={handle_submit}
                    ondragover={handle_dragover}
                    ondragleave={handle_dragleave}
                    ondrop={handle_drop}
                >
                    <span class="input-prompt">{ ">" }</span>
                    { self.render_interim_transcription() }
                    <textarea
                        ref={self.input_ref.clone()}
                        class={classes!(
                            "message-input",
                            self.interim_transcription.is_some().then_some("has-interim")
                        )}
                        placeholder="Type your message... (Shift+Enter for new line)"
                        oninput={handle_input}
                        onkeydown={handle_keydown}
                        onpaste={handle_paste}
                        disabled={!self.ws_connected}
                        rows="1"
                    />
                    { self.render_voice_input(ctx) }
                    { self.render_send_button(ctx) }
                    <div class="drop-hint">{ "Drop files here to upload" }</div>
                </form>
            </div>
        }
    }
}

// Helper methods extracted from the main impl
impl SessionView {
    /// Read the current textarea value directly from the DOM.
    fn get_input_text(&self) -> String {
        self.input_ref
            .cast::<HtmlTextAreaElement>()
            .map(|el| el.value())
            .unwrap_or_default()
    }

    /// Write text to the textarea DOM element and auto-resize it.
    /// Does NOT trigger a Yew re-render.
    fn set_input_text(&self, text: &str) {
        if let Some(el) = self.input_ref.cast::<HtmlTextAreaElement>() {
            el.set_value(text);
            // Auto-resize
            let elem: &Element = el.as_ref();
            if text.is_empty() {
                elem.remove_attribute("style").ok();
            } else {
                elem.set_attribute("style", "height: 0; overflow-y: hidden")
                    .ok();
                elem.set_attribute("style", &format!("height: {}px", el.scroll_height()))
                    .ok();
            }
        }
    }

    fn handle_ws_event(&mut self, ctx: &Context<Self>, event: WsEvent) -> bool {
        match event {
            WsEvent::Connected(sender) => {
                ctx.link()
                    .send_message(SessionViewMsg::WebSocketConnected(sender));
                false
            }
            WsEvent::Error(err) => {
                ctx.link().send_message(SessionViewMsg::WebSocketError(err));
                false
            }
            WsEvent::Output(content, created_at) => {
                // Update the reconnect-replay watermark from the
                // server-assigned `created_at` (closes #784). Falling back to
                // `Date.now()` here — the prior behavior — could miss
                // messages on reconnect when the client/server clocks were
                // skewed: a message persisted at server time T2 < browser
                // `now()` T1 would be filtered out by `replay_history`'s
                // `created_at.gt(T1)` predicate. If the backend didn't send
                // a timestamp (pre-#784 server or an error envelope), keep
                // the prior watermark — a future timestamped message will
                // heal it.
                if let Some(ts) = created_at {
                    self.last_message_timestamp = Some(ts);
                }
                ctx.link()
                    .send_message(SessionViewMsg::ReceivedOutput(content));
                ctx.link().send_message(SessionViewMsg::CheckAwaiting);
                false
            }
            WsEvent::HistoryBatch(messages, last_created_at) => {
                self.messages.extend(messages);
                if self.messages.len() > MAX_MESSAGES_PER_SESSION {
                    let excess = self.messages.len() - MAX_MESSAGES_PER_SESSION;
                    self.messages.drain(0..excess);
                }
                // Set the reconnect-replay watermark to the server-assigned
                // timestamp of the latest message in the batch (closes
                // #784). Empty batches (or a pre-#784 backend that didn't
                // send `last_created_at`) leave the watermark unchanged.
                if let Some(ts) = last_created_at {
                    self.last_message_timestamp = Some(ts);
                }
                ctx.link().send_message(SessionViewMsg::CheckAwaiting);
                true
            }
            WsEvent::Permission(perm) => {
                self.last_permission_request = Some(perm.clone());
                if let Some(ref dispatcher) = self.permission_dispatcher {
                    dispatcher.emit(perm);
                }
                false
            }
            WsEvent::BranchChanged(branch, pr_url, repo_url) => {
                ctx.link()
                    .send_message(SessionViewMsg::BranchChanged(branch, pr_url, repo_url));
                false
            }
        }
    }

    fn handle_send_input_with_mode(&mut self, ctx: &Context<Self>, send_mode: SendMode) -> bool {
        crate::audio::ensure_audio_context();
        let input = self.get_input_text().trim().to_string();

        if input.is_empty() {
            return false;
        }

        self.command_history.push(input.clone());
        self.set_input_text("");
        self.input_text.clear();

        let session_id = ctx.props().session.id;
        ctx.props().on_message_sent.emit(session_id);

        // Optimistic local echo: show in pending queue at bottom of chat
        let now_iso = js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .unwrap_or_default();
        let optimistic_msg = serde_json::json!({
            "type": "user",
            "content": input,
            "_pending": true,
            "_created_at": now_iso,
        });
        self.pending_sends.push(optimistic_msg.to_string());

        // Send the text
        if let Some(ref sender) = self.ws_sender {
            let msg = ClientToServer::ClaudeInput {
                content: serde_json::Value::String(input),
                send_mode: if send_mode == SendMode::Normal {
                    None
                } else {
                    Some(send_mode)
                },
            };
            send_message(sender, msg);
        }
        true
    }

    fn handle_received_output(&mut self, ctx: &Context<Self>, output: String) -> bool {
        let mut msg_type = "unknown".to_string();
        if let Ok(claude_msg) = serde_json::from_str::<shared::ClaudeOutput>(&output) {
            msg_type = claude_msg.message_type();
            if let shared::ClaudeOutput::System(sys) = &claude_msg {
                if let Some(status) = sys.as_status() {
                    if status.status.as_ref().map(|s| s.as_str()) == Some("compacting") {
                        msg_type = "compaction_start".to_string();
                    }
                } else if shared::is_compaction_boundary(sys) {
                    msg_type = "compaction_end".to_string();
                } else if sys.as_task_started().is_some() {
                    msg_type = "task_start".to_string();
                } else if sys.as_task_notification().is_some() {
                    msg_type = "task_end".to_string();
                }
            }
            if let shared::ClaudeOutput::Result(res) = &claude_msg {
                let cost = res.total_cost_usd;
                if cost != self.total_cost {
                    self.total_cost = cost;
                    self.cost_flash = true;

                    let session_id = ctx.props().session.id;
                    ctx.props().on_cost_change.emit((session_id, cost));

                    let link = ctx.link().clone();
                    spawn_local(async move {
                        gloo::timers::future::TimeoutFuture::new(600).await;
                        link.send_message(SessionViewMsg::ClearCostFlash);
                    });
                }
            }
            // Live task events: the `created_at` field isn't part of the
            // live wire envelope, so the panel falls back to `Date.now()`
            // — see `derive_task_events` for the two paths.
            for ev in derive_task_events(&claude_msg, "", true) {
                self.dispatch_tasks(TasksInbound::Live(ev));
            }
        } else if let Ok(parsed) = serde_json::from_str::<ClaudeMessage>(&output) {
            // Fallback for portal messages and unknown types not in ClaudeOutput
            msg_type = message_type_tag(&parsed).to_string();
        }
        crate::audio::play_sound(crate::audio::SoundEvent::Activity);
        ctx.props().on_activity.emit((
            ctx.props().session.id,
            msg_type.clone(),
            js_sys::Date::now(),
        ));
        // Inject _created_at for tooltip display
        let output = if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&output) {
            if let Some(obj) = val.as_object_mut() {
                let now_iso = js_sys::Date::new_0()
                    .to_iso_string()
                    .as_string()
                    .unwrap_or_default();
                obj.insert(
                    "_created_at".to_string(),
                    serde_json::Value::String(now_iso),
                );
            }
            val.to_string()
        } else {
            output
        };
        // Drain pending sends when the server confirms our input.
        // - "user" echo: match by content so a lost message doesn't consume
        //   an unrelated pending entry.
        // - "assistant"/"result": Claude is responding. Slash commands like
        //   /cost, /status, /clear don't produce a user echo, so we use the
        //   assistant/result response as the signal that the input was
        //   received and clear all pending entries.
        if !self.pending_sends.is_empty() {
            match msg_type.as_str() {
                "user" => {
                    let echo_text = serde_json::from_str::<ClaudeMessage>(&output)
                        .ok()
                        .as_ref()
                        .and_then(extract_user_text);
                    if let Some(ref echo) = echo_text {
                        if let Some(pos) = self.pending_sends.iter().position(|pending| {
                            serde_json::from_str::<ClaudeMessage>(pending)
                                .ok()
                                .as_ref()
                                .and_then(extract_user_text)
                                .as_ref()
                                == Some(echo)
                        }) {
                            self.pending_sends.remove(pos);
                        }
                    }
                }
                "assistant" | "result" => {
                    // Claude is responding — any pending input was received.
                    // Slash commands don't produce a user echo so this is
                    // the only signal we get.
                    self.pending_sends.clear();
                }
                _ => {}
            }
        }
        self.messages.push(output);
        if self.messages.len() > MAX_MESSAGES_PER_SESSION {
            let excess = self.messages.len() - MAX_MESSAGES_PER_SESSION;
            self.messages.drain(0..excess);
        }
        // The reconnect-replay watermark (`last_message_timestamp`) is set
        // by the `WsEvent::Output` handler from the server-assigned
        // `created_at` — never `Date.now()` (closes #784).
        true
    }

    fn handle_ws_error(&mut self, ctx: &Context<Self>, err: String) -> bool {
        crate::audio::play_sound(crate::audio::SoundEvent::Error);
        self.ws_connected = false;
        self.ws_sender = None;
        let session_id = ctx.props().session.id;
        ctx.props().on_connected_change.emit((session_id, false));

        const MAX_ATTEMPTS: u32 = 10;
        if self.reconnect_attempt < MAX_ATTEMPTS {
            self.reconnect_attempt += 1;
            let delay_ms = calculate_backoff(self.reconnect_attempt - 1);
            log::info!(
                "WebSocket disconnected, reconnecting in {}ms (attempt {})",
                delay_ms,
                self.reconnect_attempt
            );

            let link = ctx.link().clone();
            self.reconnect_timer = Some(Timeout::new(delay_ms, move || {
                link.send_message(SessionViewMsg::AttemptReconnect);
            }));
        } else {
            let error_msg = ErrorMessage::new(format!("Connection lost: {}", err));
            self.messages
                .push(serde_json::to_string(&error_msg).unwrap_or_default());
        }
        true
    }

    fn attempt_reconnect(&self, ctx: &Context<Self>) {
        let link = ctx.link().clone();
        let session_id = ctx.props().session.id;
        let replay_after = self.last_message_timestamp.clone();

        let on_event = Callback::from(move |event: WsEvent| {
            link.send_message(SessionViewMsg::WsEvent(event));
        });
        connect_websocket(session_id, replay_after, true, on_event);
    }

    fn render_permission_handler(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let on_register = link.callback(SessionViewMsg::PermissionDispatcherRegistered);
        let on_pending_changed = link.callback(SessionViewMsg::PermissionPendingChanged);
        let on_response =
            link.callback(|(rid, kind)| SessionViewMsg::PermissionAnswered(rid, kind));
        let input_ref = self.input_ref.clone();
        let on_refocus_input = Callback::from(move |_| refocus_textarea(&input_ref));
        html! {
            <PermissionHandler
                focused={ctx.props().focused}
                {on_register}
                {on_pending_changed}
                {on_response}
                {on_refocus_input}
            />
        }
    }

    fn render_interim_transcription(&self) -> Html {
        if let Some(ref interim) = self.interim_transcription {
            let current = self.get_input_text();
            let preview = if current.is_empty() {
                interim.clone()
            } else {
                format!("{} {}", current, interim)
            };
            html! {
                <div class="interim-transcription">{ preview }</div>
            }
        } else {
            html! {}
        }
    }

    fn render_upload_bar(&self) -> Html {
        let progress = match self.upload_progress {
            Some(p) => p,
            None => return html! {},
        };

        let complete = progress >= 1.0;
        let file_count = self.upload_files.len();
        let header = if complete {
            "Upload complete".to_string()
        } else if file_count == 1 {
            "Uploading 1 file...".to_string()
        } else {
            format!("Uploading {} files...", file_count)
        };

        let files_html = self
            .upload_files
            .iter()
            .map(|(name, size)| {
                let human_size = if *size < 1024 {
                    format!("{} B", size)
                } else if *size < 1024 * 1024 {
                    format!("{:.1} KB", *size as f64 / 1024.0)
                } else {
                    format!("{:.1} MB", *size as f64 / (1024.0 * 1024.0))
                };
                html! { <div class="upload-bar-file">{ format!("{} ({})", name, human_size) }</div> }
            })
            .collect::<Html>();

        let pct = (progress * 100.0) as u32;
        let fill_style = format!("width: {}%", pct);

        let bar_class = if self.upload_departing {
            "upload-bar departing"
        } else {
            "upload-bar"
        };

        html! {
            <div class={bar_class}>
                <div class="upload-bar-header">{ header }</div>
                { files_html }
                <div class="upload-bar-track">
                    <div class="upload-bar-fill" style={fill_style} />
                </div>
            </div>
        }
    }

    fn render_voice_input(&self, ctx: &Context<Self>) -> Html {
        if ctx.props().voice_enabled {
            let link = ctx.link();
            let session_id = ctx.props().session.id;
            let on_recording_change = link.callback(SessionViewMsg::VoiceRecordingChanged);
            let on_transcription = link.callback(SessionViewMsg::VoiceTranscription);
            let on_interim_transcription = link.callback(SessionViewMsg::VoiceInterimTranscription);
            let on_error = link.callback(SessionViewMsg::VoiceError);
            let button_ref = self.voice_button_ref.clone();

            html! {
                <VoiceInput
                    {session_id}
                    {on_recording_change}
                    {on_transcription}
                    on_interim_transcription={Some(on_interim_transcription)}
                    {on_error}
                    disabled={!self.ws_connected}
                    button_ref={Some(button_ref)}
                />
            }
        } else {
            html! {}
        }
    }

    fn render_send_button(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let on_send = link.callback(|_| SessionViewMsg::SendInput);
        let on_toggle_dropdown = link.callback(|e: MouseEvent| {
            e.stop_propagation();
            SessionViewMsg::ToggleSendModeDropdown
        });
        let on_wiggum = link.callback(|_| SessionViewMsg::SendWiggum);

        let file_input_ref = self.file_input_ref.clone();
        let on_attach_dropdown = Callback::from(move |_: MouseEvent| {
            if let Some(input) = file_input_ref.cast::<web_sys::HtmlInputElement>() {
                input.click();
            }
        });
        let on_file_change = link.callback(|e: Event| {
            let input: web_sys::HtmlInputElement = e.target_unchecked_into();
            if let Some(files) = input.files() {
                if files.length() > 0 {
                    let mut file_list = Vec::new();
                    for i in 0..files.length() {
                        if let Some(file) = files.get(i) {
                            file_list.push(file);
                        }
                    }
                    input.set_value("");
                    if !file_list.is_empty() {
                        return SessionViewMsg::FilesSelected(file_list);
                    }
                }
            }
            SessionViewMsg::FileUploadError("No files selected".into())
        });

        let dropdown_class = if self.send_mode_dropdown_open {
            "send-mode-dropdown open"
        } else {
            "send-mode-dropdown"
        };

        let is_uploading = self.upload_progress.is_some_and(|p| p < 1.0);

        html! {
            <div class="send-button-container">
                <input
                    ref={self.file_input_ref.clone()}
                    type="file"
                    multiple=true
                    class="hidden-file-input"
                    onchange={on_file_change}
                />
                <button
                    type="submit"
                    class="send-button"
                    disabled={!self.ws_connected || is_uploading}
                    onclick={on_send}
                >
                    { "Send" }
                </button>
                <button
                    type="button"
                    class="send-mode-toggle"
                    disabled={!self.ws_connected || is_uploading}
                    onclick={on_toggle_dropdown}
                >
                    { "\u{25bc}" }
                </button>
                <div class={dropdown_class}>
                    <button
                        type="button"
                        class="dropdown-option selected"
                        onclick={link.callback(|_| SessionViewMsg::CloseSendModeDropdown)}
                    >
                        { "Send" }
                        <span class="option-hint">{ "Normal message" }</span>
                    </button>
                    <button
                        type="button"
                        class="dropdown-option wiggum"
                        onclick={on_wiggum}
                    >
                        <span class="wiggum-label">
                            <img src="wiggum.png" alt="" class="wiggum-icon" />
                            { "Wiggum" }
                        </span>
                        <span class="option-hint">{ "Loop until DONE" }</span>
                    </button>
                    <button
                        type="button"
                        class="dropdown-option attachment"
                        onclick={on_attach_dropdown}
                    >
                        { "Send with attachment(s)" }
                        <span class="option-hint">{ "Upload files + message" }</span>
                    </button>
                </div>
            </div>
        }
    }

    fn render_tasks_panel(&self, ctx: &Context<Self>) -> Html {
        let on_register = ctx
            .link()
            .callback(SessionViewMsg::TasksDispatcherRegistered);
        html! {
            <TasksPanel {on_register} />
        }
    }

    fn dispatch_tasks(&self, msg: TasksInbound) {
        if let Some(ref dispatcher) = self.tasks_dispatcher {
            dispatcher.emit(msg);
        }
    }
}

/// Derive zero or more typed [`TaskEvent`]s from a parsed `ClaudeOutput`.
///
/// Used by both the live `WsEvent::Output` path (with `live == true`, in
/// which case `created_at_iso` is ignored and timestamps fall back to
/// `js_sys::Date::now()`) and the REST `LoadHistory` replay path (with
/// `live == false`, parsing the row's server-assigned `created_at` so
/// elapsed-time labels reflect when the event actually happened rather
/// than when the browser hydrated it).
fn derive_task_events(
    claude_msg: &shared::ClaudeOutput,
    created_at_iso: &str,
    live: bool,
) -> Vec<TaskEvent> {
    let resolve_ts = || -> f64 {
        if live {
            js_sys::Date::now()
        } else {
            let ts = js_sys::Date::parse(created_at_iso);
            if ts.is_finite() {
                ts
            } else {
                0.0
            }
        }
    };

    let mut events = Vec::new();
    match claude_msg {
        shared::ClaudeOutput::System(sys) => {
            if let Some(task) = sys.as_task_started() {
                let task_type = match task.task_type {
                    shared::CCTaskType::LocalAgent => "local_agent",
                    shared::CCTaskType::LocalBash => "local_bash",
                }
                .to_string();
                events.push(TaskEvent::Started {
                    task_id: task.task_id.clone(),
                    tool_use_id: task.tool_use_id.clone(),
                    task_type,
                    description: task.description.clone(),
                    started_at: resolve_ts(),
                });
            } else if let Some(progress) = sys.as_task_progress() {
                events.push(TaskEvent::Progress {
                    task_id: progress.task_id.clone(),
                    description: progress.description.clone(),
                    last_tool_name: progress.last_tool_name.clone(),
                    duration_ms: progress.usage.duration_ms,
                    tool_uses: progress.usage.tool_uses,
                    total_tokens: progress.usage.total_tokens,
                    fallback_started_at: resolve_ts(),
                });
            } else if let Some(notif) = sys.as_task_notification() {
                let status = match notif.status {
                    shared::CCTaskStatus::Completed => TaskStatus::Completed,
                    shared::CCTaskStatus::Failed => TaskStatus::Failed,
                };
                let usage = notif
                    .usage
                    .as_ref()
                    .map(|u| (u.duration_ms, u.tool_uses, u.total_tokens));
                events.push(TaskEvent::Notification {
                    task_id: notif.task_id.clone(),
                    summary: notif.summary.clone(),
                    status,
                    completed_at: resolve_ts(),
                    usage,
                });
            }
        }
        shared::ClaudeOutput::User(user_msg) => {
            // Fallback: --print mode doesn't emit `task_notification`, so
            // any `tool_result` whose `tool_use_id` matches a tracked task
            // implicitly closes it. The panel owns the reverse index and
            // no-ops the event when there's no match.
            for block in &user_msg.message.content {
                if let shared::ContentBlock::ToolResult(tr) = block {
                    events.push(TaskEvent::ToolResult {
                        tool_use_id: tr.tool_use_id.clone(),
                        completed_at: resolve_ts(),
                    });
                }
            }
        }
        _ => {}
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn autoscroll_transition_returns_none_when_unchanged() {
        assert_eq!(autoscroll_transition(true, true), None);
        assert_eq!(autoscroll_transition(false, false), None);
    }

    #[test]
    fn autoscroll_transition_disables_when_user_scrolls_up() {
        // User was tailing, scrolled away from bottom -> tailing turns off
        // and the jump-to-live pill should render.
        assert_eq!(autoscroll_transition(true, false), Some(false));
    }

    #[test]
    fn autoscroll_transition_re_enables_when_user_scrolls_back_to_bottom() {
        // User had scrolled up, now scrolled back to bottom -> tailing
        // resumes and the jump-to-live pill should disappear.
        assert_eq!(autoscroll_transition(false, true), Some(true));
    }
}
