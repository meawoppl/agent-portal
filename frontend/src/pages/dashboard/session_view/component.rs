//! SessionView component - Main terminal view for a single session
//!
//! Residual orchestrator after the EPIC #809 decomposition: WebSocket
//! connect/reconnect, message-buffer rendering, awaiting-input gate, and
//! glue between the three sub-components (`PermissionHandler`, `TasksPanel`,
//! `InputBar`). Pure helpers (msg-type classification, metadata injection,
//! pending-send reconciliation, autoscroll-transition math) live in
//! `helpers.rs`; task-event derivation lives alongside its consumer in
//! `tasks_panel.rs`.

use crate::components::message_renderer::MessageRenderer;
use crate::components::{
    group_is_turn_terminator, group_messages, thinking_chip_starts, MessageGroupRenderer,
};
use crate::utils::{self, On401};
use gloo::timers::callback::Timeout;
use shared::api::{ErrorMessage, TurnMetricsResponse};
use shared::{ClientToServer, SendMode, SessionInfo, TurnMetrics};
use std::collections::HashMap;
use uuid::Uuid;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::Element;
use yew::prelude::*;

use super::helpers::{
    autoscroll_transition, classify_output_msg_type, inject_created_at_if_absent,
    inject_message_metadata, is_claude_awaiting, reconcile_pending_sends, ActivityTag,
};
use super::input_bar::{InputBar, InputBarInbound};
use super::permission_handler::{
    build_permission_response, PermissionHandler, PermissionResponseKind,
};
use super::state::{
    insert_turn_metrics_sorted, push_message_with_limit, retain_newest_items,
    sort_turn_metrics_by_start,
};
use super::tasks_panel::{derive_task_events, TasksInbound, TasksPanel};
use super::types::{PendingPermission, WsSender, MAX_MESSAGES_PER_SESSION};
use super::websocket::{connect_websocket, send_message, WsEvent};
use crate::pages::dashboard::types::{calculate_backoff, MessageData, MessagesResponse};

/// Props for the SessionView component
#[derive(Properties, PartialEq)]
pub struct SessionViewProps {
    pub session: SessionInfo,
    pub focused: bool,
    pub on_awaiting_change: Callback<(Uuid, bool)>,
    pub on_connected_change: Callback<(Uuid, bool)>,
    pub on_message_sent: Callback<Uuid>,
    #[allow(clippy::type_complexity)]
    pub on_branch_change: Callback<(
        Uuid,
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<shared::PrRef>,
    )>,
    #[prop_or_default]
    pub on_activity: Callback<(Uuid, ActivityTag, f64)>,
    #[prop_or_default]
    pub current_user_id: Option<String>,
    #[prop_or(0)]
    pub interrupt_signal: u32,
}

/// Messages for the SessionView component
pub enum SessionViewMsg {
    LoadHistory(Vec<MessageData>, Option<String>),
    ReceivedOutput(String),
    WebSocketConnected(WsSender),
    WebSocketError(String),
    AttemptReconnect,
    CheckAwaiting,
    BranchChanged(
        Option<String>,
        Option<String>,
        Option<String>,
        Vec<shared::PrRef>,
    ),
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
    /// TasksPanel is mounted and handed us its inbound-event dispatcher.
    /// We store it so live `WsEvent::Output` task events and REST replay
    /// task events can be forwarded without the parent owning any task
    /// state.
    TasksDispatcherRegistered(Callback<TasksInbound>),
    /// InputBar is mounted and handed us its inbound-event dispatcher. We
    /// store it so `PermissionHandler`'s "refocus textarea after answer"
    /// hook can be forwarded through to the bar without the parent owning
    /// the textarea `NodeRef`.
    InputBarDispatcherRegistered(Callback<InputBarInbound>),
    /// InputBar emitted a plain-text submission with the chosen send mode.
    /// We translate this into the optimistic local echo + the WS
    /// `ClientToServer::ClaudeInput` frame.
    SendText(String, SendMode),
    /// InputBar emitted a raw WS frame (used by the file-upload pipeline
    /// for `FileUploadStart` / `FileUploadChunk` / the final combined
    /// `ClaudeInput`). We just forward it over the WebSocket.
    SendFrame(ClientToServer),
    /// InputBar reports that a submission landed — bumps the parent's
    /// `on_message_sent` prop.
    MessageSent,
    /// Send an interrupt to stop the current Claude response
    Interrupt,
    /// Scroll listener reports the current at-bottom state. The `update()`
    /// arm flips `should_autoscroll` and re-renders only when the value
    /// changes, so the closure can dispatch on every scroll event without
    /// per-event re-renders.
    AutoscrollChanged(bool),
    /// User clicked the "Jump to live" pill: resume tailing and scroll to bottom.
    JumpToLive,
    /// REST hydration of historical per-turn metrics finished (PR 2 of N).
    /// Replaces any current buffer with the freshly-fetched list — fired
    /// once per session load alongside the existing `LoadHistory` path.
    LoadTurnMetrics(Vec<TurnMetrics>),
    /// Live per-turn metrics frame arrived over the WS (PR 2 of N). Inserted
    /// into `turn_metrics` in `started_at`-sorted order, deduping by `id`
    /// so a backfill-then-broadcast pair (or a duplicate replay) collapses.
    TurnMetricsReceived(Box<TurnMetrics>),
    ScheduleLimitContinuation(Uuid),
    ContinuationStatus(Uuid, String),
}

/// SessionView - Main terminal view for a single session
pub struct SessionView {
    messages: Vec<String>,
    ws_connected: bool,
    ws_sender: Option<WsSender>,
    messages_ref: NodeRef,
    should_autoscroll: bool,
    #[allow(dead_code)]
    scroll_listener: Option<Closure<dyn Fn()>>,
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
    last_message_timestamp: Option<String>,
    /// Dispatcher into the mounted `TasksPanel`. Stored once at child
    /// `create` time via `TasksDispatcherRegistered`; live task events
    /// derived from `WsEvent::Output` and replay events derived from the
    /// REST `LoadHistory` path are forwarded through it so this component
    /// holds zero task UI state itself.
    tasks_dispatcher: Option<Callback<TasksInbound>>,
    /// Dispatcher into the mounted `InputBar`. Stored once at child
    /// `create` time via `InputBarDispatcherRegistered`; used to forward
    /// `PermissionHandler`'s "refocus textarea after answer" event so this
    /// component holds zero textarea / upload / send-mode state itself.
    input_bar_dispatcher: Option<Callback<InputBarInbound>>,
    /// Messages sent but not yet confirmed by the server echo
    pending_sends: Vec<String>,
    /// Per-turn performance metrics, sorted by `started_at ASC` (PR 2 of N).
    /// Hydrated by `LoadTurnMetrics` on initial REST load and topped up by
    /// `TurnMetricsReceived` on every live WS frame. Joined to terminator
    /// messages in `view()` by ordering: the Nth terminator card pairs
    /// with the Nth entry. See the PR 2 changelog entry for the rationale
    /// (the proxy-emit shape doesn't populate `user_message_id` yet, so a
    /// key-based join would fail on every row). Vec rather than HashMap
    /// because the join walk is sequential — a HashMap with a positional
    /// counter would buy nothing.
    turn_metrics: Vec<TurnMetrics>,
    continuation_statuses: HashMap<Uuid, String>,
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

            if let Ok(data) = utils::fetch_json::<MessagesResponse>(
                &format!("/api/sessions/{}/messages", session_id),
                On401::Ignore,
            )
            .await
            {
                let is_awaiting = is_claude_awaiting(data.messages.iter().map(|m| &m.content));
                on_awaiting_change.emit((session_id, is_awaiting));

                last_message_time = data.messages.last().map(|m| m.created_at.clone());

                link.send_message(SessionViewMsg::LoadHistory(
                    data.messages,
                    last_message_time.clone(),
                ));
            }

            // Hydrate the per-turn metrics buffer in parallel (PR 2 of N).
            // Failure here is non-fatal: the chip-strip footer simply stays
            // empty for past turns; live broadcasts still populate the
            // buffer for new turns. Same `MeResponse`-style typed deserialize
            // pattern the existing `MessagesResponse` path uses.
            if let Ok(data) = utils::fetch_json::<TurnMetricsResponse>(
                &format!("/api/sessions/{}/turn-metrics", session_id),
                On401::Ignore,
            )
            .await
            {
                link.send_message(SessionViewMsg::LoadTurnMetrics(data.metrics));
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
            should_autoscroll: true,
            scroll_listener: None,
            permission_dispatcher: None,
            has_pending_permission: false,
            last_permission_request: None,
            reconnect_attempt: 0,
            reconnect_timer: None,
            last_message_timestamp: None,
            tasks_dispatcher: None,
            input_bar_dispatcher: None,
            pending_sends: Vec::new(),
            turn_metrics: Vec::new(),
            continuation_statuses: HashMap::new(),
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, old_props: &Self::Properties) -> bool {
        // Detect interrupt signal change on the focused session. Textarea
        // focus on focused-transition is owned by `InputBar` (it sees the
        // `focused` prop directly through its own `changed()`).
        if ctx.props().focused
            && ctx.props().interrupt_signal != old_props.interrupt_signal
            && ctx.props().interrupt_signal > 0
        {
            ctx.link().send_message(SessionViewMsg::Interrupt);
        }

        true
    }

    fn rendered(&mut self, ctx: &Context<Self>, first_render: bool) {
        // Textarea focus + content restoration are owned by `InputBar`.

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
            SessionViewMsg::LoadHistory(messages, last_timestamp) => {
                self.handle_load_history(ctx, messages, last_timestamp);
                true
            }
            SessionViewMsg::ReceivedOutput(output) => self.handle_received_output(ctx, output),
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
                // Textarea refocus is handled separately via
                // `PermissionHandlerProps::on_refocus_input`, which the
                // parent routes through the `InputBar` dispatcher.
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
            SessionViewMsg::BranchChanged(branch, pr_url, repo_url, open_prs) => {
                let session_id = ctx.props().session.id;
                ctx.props()
                    .on_branch_change
                    .emit((session_id, branch, pr_url, repo_url, open_prs));
                false
            }
            SessionViewMsg::TasksDispatcherRegistered(dispatcher) => {
                self.tasks_dispatcher = Some(dispatcher);
                false
            }
            SessionViewMsg::InputBarDispatcherRegistered(dispatcher) => {
                self.input_bar_dispatcher = Some(dispatcher);
                false
            }
            SessionViewMsg::SendText(input, mode) => {
                self.send_text_input(input, mode);
                true
            }
            SessionViewMsg::SendFrame(frame) => {
                if let Some(ref sender) = self.ws_sender {
                    send_message(sender, frame);
                }
                false
            }
            SessionViewMsg::MessageSent => {
                let session_id = ctx.props().session.id;
                ctx.props().on_message_sent.emit(session_id);
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
            SessionViewMsg::LoadTurnMetrics(mut metrics) => {
                // REST hydration arrives once per session load. Sort by
                // started_at ASC defensively even though the backend
                // already orders that way — the join walk depends on
                // strict order.
                sort_turn_metrics_by_start(&mut metrics);
                self.turn_metrics = metrics;
                true
            }
            SessionViewMsg::TurnMetricsReceived(metrics) => {
                insert_turn_metrics_sorted(&mut self.turn_metrics, *metrics);
                true
            }
            SessionViewMsg::ScheduleLimitContinuation(continuation_id) => {
                self.continuation_statuses
                    .insert(continuation_id, "scheduling".to_string());
                if let Some(ref sender) = self.ws_sender {
                    send_message(
                        sender,
                        ClientToServer::ScheduleLimitContinuation { continuation_id },
                    );
                }
                true
            }
            SessionViewMsg::ContinuationStatus(continuation_id, status) => {
                self.continuation_statuses.insert(continuation_id, status);
                true
            }
        }
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let is_tailing = self.should_autoscroll;
        let on_jump_to_live = link.callback(|e: MouseEvent| {
            e.stop_propagation();
            SessionViewMsg::JumpToLive
        });
        let on_schedule_continuation = link.callback(SessionViewMsg::ScheduleLimitContinuation);

        // Per-turn metrics join (PR 2 of N): walk grouped messages in order
        // and pair the Nth terminator card with `turn_metrics[N]`. The
        // pairs computed here are passed down to `MessageGroupRenderer` so
        // the renderer doesn't have to know its own position in the
        // transcript. See the PR 2 changelog entry for the rationale.
        let groups = group_messages(
            &self.messages,
            ctx.props().session.agent_type,
            ctx.props().current_user_id.as_deref(),
        );
        let mut metrics_iter = self.turn_metrics.iter();
        let group_metrics: Vec<Option<TurnMetrics>> = groups
            .iter()
            .map(|g| {
                if group_is_turn_terminator(g) {
                    metrics_iter.next().cloned()
                } else {
                    None
                }
            })
            .collect();
        // Seed each thinking chip's odometer with the prior burst's max in
        // its turn so tool-call splits don't re-race the count from 0.
        let thinking_starts = thinking_chip_starts(&groups);

        html! {
            <div class="session-view">
                <div class="session-view-scroll-area">
                    <div class="session-view-messages" ref={self.messages_ref.clone()}>
                        {
                            groups.into_iter().enumerate().map(|(i, group)| {
                                let key = group.key(i);
                                let metrics = group_metrics.get(i).cloned().flatten();
                                let thinking_start = thinking_starts.get(i).copied().unwrap_or(0);
                                html! { <MessageGroupRenderer {key} group={group} session_id={ctx.props().session.id} agent_type={ctx.props().session.agent_type} current_user_id={ctx.props().current_user_id.clone()} turn_metrics={metrics} {thinking_start} continuation_statuses={self.continuation_statuses.clone()} on_schedule_continuation={on_schedule_continuation.clone()} /> }
                            }).collect::<Html>()
                        }
                        { for self.pending_sends.iter().enumerate().map(|(i, json)| {
                            html! { <MessageRenderer key={format!("p{}", i)} json={json.clone()} session_id={ctx.props().session.id} agent_type={ctx.props().session.agent_type} current_user_id={ctx.props().current_user_id.clone()} continuation_statuses={self.continuation_statuses.clone()} on_schedule_continuation={on_schedule_continuation.clone()} /> }
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
                { self.render_input_bar(ctx) }
            </div>
        }
    }
}

// Helper methods extracted from the main impl
impl SessionView {
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
                retain_newest_items(&mut self.messages, MAX_MESSAGES_PER_SESSION);
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
            WsEvent::BranchChanged(branch, pr_url, repo_url, open_prs) => {
                ctx.link().send_message(SessionViewMsg::BranchChanged(
                    branch, pr_url, repo_url, open_prs,
                ));
                false
            }
            WsEvent::TurnMetrics(metrics) => {
                ctx.link()
                    .send_message(SessionViewMsg::TurnMetricsReceived(metrics));
                false
            }
            WsEvent::ContinuationStatus {
                continuation_id,
                status,
            } => {
                ctx.link()
                    .send_message(SessionViewMsg::ContinuationStatus(continuation_id, status));
                false
            }
        }
    }

    /// Hydrate the message buffer + task panel from a REST history batch.
    /// Each message is classified once via [`classify_output_msg_type`],
    /// task events are forwarded to the panel via [`derive_task_events`],
    /// and metadata (`_sender` for user messages, `_created_at` for every
    /// row) is folded into the JSON via [`inject_message_metadata`].
    fn handle_load_history(
        &mut self,
        ctx: &Context<Self>,
        mut messages: Vec<MessageData>,
        last_timestamp: Option<String>,
    ) {
        if messages.len() > MAX_MESSAGES_PER_SESSION {
            retain_newest_items(&mut messages, MAX_MESSAGES_PER_SESSION);
        }
        let session_id = ctx.props().session.id;
        self.dispatch_tasks(TasksInbound::ClearForReplay);
        for msg in &messages {
            let tag = classify_output_msg_type(&msg.content);
            if let Ok(claude_msg) = serde_json::from_str::<shared::ClaudeOutput>(&msg.content) {
                for ev in derive_task_events(&claude_msg, &msg.created_at, false) {
                    self.dispatch_tasks(TasksInbound::Replay(ev));
                }
            }
            let ts_ms = js_sys::Date::parse(&msg.created_at);
            if ts_ms.is_finite() {
                ctx.props().on_activity.emit((session_id, tag, ts_ms));
            }
        }
        self.messages = messages
            .into_iter()
            .map(|m| {
                inject_message_metadata(
                    &m.content,
                    &m.created_at,
                    m.role == "user",
                    m.user_id.as_deref(),
                    m.sender_name.as_deref(),
                    m.origin.as_ref(),
                )
            })
            .collect();
        self.last_message_timestamp = last_timestamp;
        ctx.link().send_message(SessionViewMsg::CheckAwaiting);
    }

    /// Translate a plain-text submission from `InputBar` into the optimistic
    /// local echo + the `ClientToServer::ClaudeInput` WS frame. The bar has
    /// already trimmed and cleared its textarea, and already emitted
    /// `MessageSent` (which we forward to `on_message_sent` separately);
    /// we just package the wire frame and local echo.
    fn send_text_input(&mut self, input: String, send_mode: SendMode) {
        if input.is_empty() {
            return;
        }
        let now_iso = js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .unwrap_or_default();
        // Local-only stub for the message list while the wire round-trips.
        // Typed so the wire shape lives in source — the consumer renderer
        // still parses `pending_sends` as JSON (cleanup tracked separately).
        #[derive(serde::Serialize)]
        struct OptimisticUserMessage<'a> {
            #[serde(rename = "type")]
            message_type: &'static str,
            content: &'a str,
            #[serde(rename = "_pending")]
            pending: bool,
            #[serde(rename = "_created_at")]
            created_at: &'a str,
        }
        let optimistic_msg = serde_json::to_string(&OptimisticUserMessage {
            message_type: "user",
            content: &input,
            pending: true,
            created_at: &now_iso,
        })
        .unwrap_or_default();
        self.pending_sends.push(optimistic_msg);

        if let Some(ref sender) = self.ws_sender {
            let msg = ClientToServer::AgentInput {
                content: serde_json::Value::String(input),
                send_mode: if send_mode == SendMode::Normal {
                    None
                } else {
                    Some(send_mode)
                },
            };
            send_message(sender, msg);
        }
    }

    fn handle_received_output(&mut self, ctx: &Context<Self>, output: String) -> bool {
        let tag = classify_output_msg_type(&output);
        if let Ok(claude_msg) = serde_json::from_str::<shared::ClaudeOutput>(&output) {
            // Live task events: the `created_at` field isn't part of the
            // live wire envelope, so the panel falls back to `Date.now()`
            // — see `derive_task_events` for the two paths.
            for ev in derive_task_events(&claude_msg, "", true) {
                self.dispatch_tasks(TasksInbound::Live(ev));
            }
        }
        crate::audio::play_sound(crate::audio::SoundEvent::Activity);
        ctx.props()
            .on_activity
            .emit((ctx.props().session.id, tag, js_sys::Date::now()));
        // Inject _created_at for tooltip display only when the frame doesn't
        // already carry one: `websocket.rs` folds the server-assigned
        // `created_at` into the content before emitting `WsEvent::Output`,
        // and that authoritative timestamp must win over the browser clock
        // (#981). Frames without one (error envelopes, pre-#784 backends)
        // fall back to `Date::now()`; the reconnect-replay watermark is set
        // separately from the server `created_at` in `WsEvent::Output`
        // (closes #784).
        let now_iso = js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .unwrap_or_default();
        let output = inject_created_at_if_absent(&output, &now_iso);

        reconcile_pending_sends(&mut self.pending_sends, tag, &output);

        push_message_with_limit(&mut self.messages, output, MAX_MESSAGES_PER_SESSION);
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
        // Re-focus the textarea after an answer by forwarding through the
        // `InputBar`'s dispatcher (which we got at the bar's `create` time).
        // Snapshot the `Option` once so the callback doesn't capture `&self`.
        let input_bar = self.input_bar_dispatcher.clone();
        let on_refocus_input = Callback::from(move |_| {
            if let Some(ref dispatcher) = input_bar {
                dispatcher.emit(InputBarInbound::FocusTextarea);
            }
        });
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

    fn render_input_bar(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let on_register = link.callback(SessionViewMsg::InputBarDispatcherRegistered);
        let on_send_text =
            link.callback(|(text, mode): (String, SendMode)| SessionViewMsg::SendText(text, mode));
        let on_send_frame = link.callback(SessionViewMsg::SendFrame);
        let on_message_sent = link.callback(|_| SessionViewMsg::MessageSent);
        html! {
            <InputBar
                session_id={ctx.props().session.id}
                focused={ctx.props().focused}
                ws_connected={self.ws_connected}
                {on_register}
                {on_send_text}
                {on_send_frame}
                {on_message_sent}
            />
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
