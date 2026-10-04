//! SessionView component - Main terminal view for a single session
//!
//! Residual orchestrator after the EPIC #809 decomposition: WebSocket
//! connect/reconnect, message-buffer rendering, awaiting-input gate, and
//! glue between the three sub-components (`PermissionHandler`, `TasksPanel`,
//! `InputBar`). Pure helpers (msg-type classification, metadata injection,
//! pending-send reconciliation, autoscroll-transition math) live in
//! `helpers.rs`; task-event derivation lives alongside its consumer in
//! `tasks_panel.rs`.

use crate::components::message_renderer::{MessageRenderer, RenderedMessage};
use crate::components::plugin_discovery::{
    plugin_context_label, plugin_inventory_api_path, suggested_plugins,
};
use crate::components::{
    group_is_turn_terminator, group_messages, thinking_chip_starts, ForkDialog,
    MessageGroupRenderer,
};
use crate::utils::{self, On401};
use gloo::events::EventListener;
use gloo::timers::callback::Timeout;
use gloo_net::http::Request;
use shared::api::{
    EditStackItem, EditStackResponse, ForwardInfo, PluginInventoryResponse, PortalPluginInfo,
    TurnMetricsResponse, UpdateEditStackItemRequest,
};
use shared::strings::truncate_with_ellipsis;
use shared::{
    ClientToServer, DeliveryMeta, PortalMeta, ReasoningEffort, SendMode, SessionInfo, TurnMetrics,
};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
use wasm_bindgen::{closure::Closure, JsCast};
use wasm_bindgen_futures::spawn_local;
use web_sys::{Element, KeyboardEvent, MouseEvent, PointerEvent};
use yew::prelude::*;

use super::forward_chips::ForwardChips;
use super::forward_surface::ForwardSurface;
use super::helpers::{
    autoscroll_transition, classify_output_msg_type, clear_completed_tools,
    enrich_codex_file_change_permission, ephemeral_summary, format_tool_elapsed, is_awaiting,
    parse_iso_ms_utc, reconcile_pending_sends, running_tool_key, update_pending_send_delivery,
    upsert_tool_progress, ActiveToolProgress, ActivityTag, MuseLiveTurn,
};
use super::input_bar::{InputBar, InputBarInbound};
use super::outbox::Outbox;
use super::permission_handler::{
    build_permission_response, PermissionHandler, PermissionResponseKind,
};
use super::session_surface::{
    clamp_split_percent, clear_open_surface, load_open_surface, load_split_percent,
    save_open_surface, save_split_percent, ForwardSurfaceMemory, SessionSurface,
    SessionSurfaceMode,
};
use super::state::{
    counts_toward_render_limit, insert_turn_metrics_sorted, push_message_with_cost_limit,
    retain_newest_items_by_cost, sort_turn_metrics_by_start,
};
use super::tasks_panel::{derive_task_events, TasksInbound, TasksPanel};
use super::types::{PendingPermission, WsSender, MAX_MESSAGES_PER_SESSION};
use super::websocket::{connect_websocket, send_message, WsEvent};
use crate::pages::dashboard::types::{MessageData, MessagesResponse};
use crate::pages::settings::agent_login::AgentLoginModal;
use crate::utils::calculate_backoff;

const EDIT_STACK_BODY_PREVIEW_CHARS: usize = 260;
const EDIT_STACK_CONTEXT_PREVIEW_CHARS: usize = 700;
const PLUGIN_NOTICE_STORAGE_PREFIX: &str = "agent_portal.plugin_notice.expanded.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditStackView {
    Pending,
    Past,
}

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
    pub on_agent_message: Callback<(Uuid, Uuid, f64)>,
    #[prop_or_default]
    pub current_user_id: Option<String>,
    #[prop_or(0)]
    pub interrupt_signal: u32,
    /// Bumped by the dashboard when nav-mode `G` is pressed; the focused session
    /// jumps its transcript to the newest message and resumes live tailing.
    #[prop_or(0)]
    pub jump_to_latest_signal: u32,
    /// Whether the server can transcribe audio (`AppConfig::stt_enabled`).
    /// Forwarded to the voice button, which picks its capture strategy from it.
    #[prop_or(false)]
    pub stt_enabled: bool,
}

fn optimistic_user_message(
    content: &str,
    created_at: &str,
    client_msg_id: Uuid,
) -> RenderedMessage {
    RenderedMessage::local(
        shared::LocalFrame::user(content),
        Some(PortalMeta {
            created_at: Some(created_at.to_string()),
            source: None,
            delivery: Some(DeliveryMeta {
                client_msg_id,
                stage: None,
                message: None,
            }),
        }),
    )
}

fn is_mobile_surface_viewport() -> bool {
    web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|value| value.as_f64())
        .is_some_and(|width| width < 700.0)
}

fn pretty_json(value: &serde_json::Value) -> Option<String> {
    serde_json::to_string_pretty(value).ok()
}

fn edit_stack_api_path(session_id: Uuid) -> String {
    format!("/api/sessions/{session_id}/edit-stack")
}

fn edit_stack_item_api_path(session_id: Uuid, item_id: Uuid) -> String {
    format!("/api/sessions/{session_id}/edit-stack/{item_id}")
}

fn browser_now_iso() -> String {
    js_sys::Date::new_0()
        .to_iso_string()
        .as_string()
        .unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

fn elapsed_ms_between(start: &str, end: &str) -> Option<u64> {
    let start = parse_iso_ms_utc(start);
    let end = parse_iso_ms_utc(end);
    if !start.is_finite() || !end.is_finite() {
        return None;
    }
    Some((end - start).max(0.0).round() as u64)
}

fn elapsed_ms_since(start: &str, now_ms: f64) -> Option<u64> {
    let start = parse_iso_ms_utc(start);
    if !start.is_finite() || !now_ms.is_finite() {
        return None;
    }
    Some((now_ms - start).max(0.0).round() as u64)
}

fn format_edit_stack_elapsed(ms: u64) -> String {
    if ms < 1000 {
        "now".to_string()
    } else {
        shared::fmt::format_duration(ms)
    }
}

fn edit_stack_status_label(status: &str) -> &'static str {
    match status {
        "pending" => "Pending",
        "sent" => "Sent",
        "completed" => "Done",
        "failed" => "Failed",
        "dismissed" => "Dismissed",
        _ => "Past",
    }
}

fn next_sent_at_after<'a>(items: &'a [EditStackItem], item: &EditStackItem) -> Option<&'a str> {
    let sent_at = item.sent_at.as_deref()?;
    items
        .iter()
        .filter_map(|candidate| {
            let candidate_sent_at = candidate.sent_at.as_deref()?;
            (candidate_sent_at > sent_at).then_some(candidate_sent_at)
        })
        .min()
}

fn edit_stack_elapsed_label(
    item: &EditStackItem,
    items: &[EditStackItem],
    now_ms: f64,
) -> Option<String> {
    match item.status.as_str() {
        "pending" => elapsed_ms_since(&item.created_at, now_ms)
            .map(|ms| format!("queued {}", format_edit_stack_elapsed(ms))),
        "completed" => item
            .sent_at
            .as_deref()
            .and_then(|sent_at| elapsed_ms_between(sent_at, &item.updated_at))
            .map(|ms| format!("took {}", format_edit_stack_elapsed(ms))),
        "failed" => item
            .sent_at
            .as_deref()
            .and_then(|sent_at| elapsed_ms_between(sent_at, &item.updated_at))
            .map(|ms| format!("failed after {}", format_edit_stack_elapsed(ms))),
        "sent" => {
            let sent_at = item.sent_at.as_deref()?;
            next_sent_at_after(items, item)
                .and_then(|next| elapsed_ms_between(sent_at, next))
                .map(|ms| format!("took {}", format_edit_stack_elapsed(ms)))
                .or_else(|| {
                    elapsed_ms_since(sent_at, now_ms)
                        .map(|ms| format!("sent {}", format_edit_stack_elapsed(ms)))
                })
        }
        _ => elapsed_ms_between(&item.created_at, &item.updated_at)
            .map(|ms| format!("open {}", format_edit_stack_elapsed(ms))),
    }
}

fn edit_stack_prompt_content(item: &EditStackItem) -> String {
    let mut content = String::new();
    content.push_str("Portal edit-stack annotation.\n\n");
    content.push_str(&format!("Title: {}\n\n", item.title));
    if let Some(source) = item.source.as_ref().and_then(pretty_json) {
        content.push_str("Source:\n```json\n");
        content.push_str(&source);
        content.push_str("\n```\n\n");
    }
    if let Some(context) = item.context.as_ref().and_then(pretty_json) {
        content.push_str("Selected region / view context:\n```json\n");
        content.push_str(&context);
        content.push_str("\n```\n\n");
    }
    if !utils::is_non_blank(&item.body) {
        content.push_str("User note: (no text note provided)\n\n");
    } else {
        content.push_str("User note:\n");
        content.push_str(&item.body);
        content.push_str("\n\n");
    }
    if let Some(image) = item.image_data_url.as_deref() {
        content.push_str("Captured region:\n");
        content.push_str(&format!("![edit-stack capture]({image})\n\n"));
    }
    content.push_str(
        "Please handle this single edit-stack item as one focused change. Verify it, then stop so Portal can send the next pending item if requested.",
    );
    content
}

fn edit_stack_fetch(link: yew::html::Scope<SessionView>, session_id: Uuid) {
    spawn_local(async move {
        match utils::fetch_json::<EditStackResponse>(
            &edit_stack_api_path(session_id),
            On401::Ignore,
        )
        .await
        {
            Ok(data) => link.send_message(SessionViewMsg::EditStackLoaded(data.items)),
            Err(err) => link.send_message(SessionViewMsg::EditStackRequestFailed(format!(
                "Could not load edit stack: {err}"
            ))),
        }
    });
}

fn plugin_inventory_fetch(link: yew::html::Scope<SessionView>, session_id: Uuid) {
    spawn_local(async move {
        let path = plugin_inventory_api_path(Some(session_id), None);
        match utils::fetch_json::<PluginInventoryResponse>(&path, On401::Ignore).await {
            Ok(data) => link.send_message(SessionViewMsg::PluginInventoryLoaded(data.plugins)),
            Err(err) => link.send_message(SessionViewMsg::PluginInventoryFailed(format!(
                "Could not load plugins: {err}"
            ))),
        }
    });
}

fn plugin_notice_storage_key(session_id: Uuid) -> String {
    format!("{PLUGIN_NOTICE_STORAGE_PREFIX}{session_id}")
}

fn load_plugin_notice_expanded(session_id: Uuid) -> bool {
    utils::storage_get(&plugin_notice_storage_key(session_id)).as_deref() == Some("true")
}

fn save_plugin_notice_expanded(session_id: Uuid, expanded: bool) {
    utils::storage_set(
        &plugin_notice_storage_key(session_id),
        if expanded { "true" } else { "false" },
    );
}

/// Messages for the SessionView component
pub enum SessionViewMsg {
    LoadHistory(Vec<MessageData>, Option<String>),
    ReceivedOutput(RenderedMessage),
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
    SendText(String, SendMode, Option<ReasoningEffort>),
    /// InputBar emitted a raw WS frame (used by the file-upload pipeline
    /// for `FileUploadStart` / `FileUploadChunk`). We just forward it over
    /// the WebSocket.
    SendFrame(ClientToServer),
    /// InputBar finished emitting upload chunks and hands us the composed
    /// prompt plus the upload ids it references. We hold the prompt until
    /// every id commits (`WsEvent::UploadResult`), then dispatch it as a
    /// normal agent input (#939 phase 4).
    UploadPrompt {
        content: String,
        upload_ids: Vec<String>,
        reasoning_effort: Option<ReasoningEffort>,
    },
    SecretDrop {
        upload_id: String,
        file_size: u64,
        reasoning_effort: Option<ReasoningEffort>,
    },
    /// Open a session-owned surface for the selected forwarded app.
    OpenForwardSurface(ForwardInfo),
    /// The forward chip strip fetched the current forward set.
    ForwardsLoaded(Vec<ForwardInfo>),
    PluginInventoryLoaded(Vec<PortalPluginInfo>),
    PluginInventoryFailed(String),
    TogglePluginsPanel,
    HidePluginsPanel,
    TogglePluginNotice,
    EditStackLoaded(Vec<EditStackItem>),
    EditStackRequestFailed(String),
    ToggleEditStackPanel,
    HideEditStackPanel,
    ShowPendingEditStack,
    ShowPastEditStack,
    SendNextEditStackItem,
    SendEditStackItem(Uuid),
    SendAllEditStack,
    DismissEditStackItem(Uuid),
    /// Close the active session surface.
    CloseSurface,
    ToggleSurfaceCollapsed,
    ToggleSurfaceMode,
    SurfaceResizeStart(PointerEvent),
    SurfaceResizeTo(f64),
    SurfaceResizeEnd,
    EscapeSurface,
    /// The upload-commit wait expired (old proxy or very slow link):
    /// dispatch the held prompt anyway — pre-transactional behavior.
    UploadCommitTimeout,
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
    ShowForkDialog,
    HideForkDialog,
    ShowClaudeLogin,
    HideClaudeLogin,
}

/// SessionView - Main terminal view for a single session
/// A composed upload prompt held back until every file it references has
/// been committed on the proxy host (#939 phase 4).
struct PendingUploadPrompt {
    remaining: std::collections::HashSet<String>,
    content: String,
    reasoning_effort: Option<ReasoningEffort>,
    /// Compat fallback: proxies that predate upload acks never send
    /// `FileUploadResult`, so fire the prompt anyway after this window
    /// (pre-transactional behavior). Cancelled by drop.
    _timeout: Timeout,
}

struct PendingSecretDrop {
    upload_id: String,
    file_size: u64,
    reasoning_effort: Option<ReasoningEffort>,
    _timeout: Timeout,
}

pub struct SessionView {
    messages: Vec<RenderedMessage>,
    ws_connected: bool,
    ws_sender: Option<WsSender>,
    /// Unacked `AgentInput` frames, resent on reconnect so inputs typed while
    /// the socket is down aren't silently dropped. See [`Outbox`].
    outbox: Outbox,
    /// See [`PendingUploadPrompt`].
    pending_upload_prompt: Option<PendingUploadPrompt>,
    pending_secret_drop: Option<PendingSecretDrop>,
    /// Upload outcomes that arrived before the prompt handoff (a small
    /// file can commit while later files are still streaming). Bounded;
    /// consumed by `handle_upload_prompt`.
    early_upload_results: std::collections::HashMap<String, shared::FileUploadResultFields>,
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
    pending_sends: Vec<RenderedMessage>,
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
    /// Currently-running tools, fed by the ephemeral `WsEvent::ToolProgress`
    /// heartbeat side-channel and rendered as a trailing "active tool" status
    /// strip. Never persisted; pruned when a tool's result arrives or the turn
    /// ends. Kept out of the memoized message-render props on purpose (see the
    /// `helpers` tool-progress section).
    active_tools: Vec<ActiveToolProgress>,
    /// Agent-driven progress bars (`agent-portal progress`), replaced wholesale
    /// by each `WsEvent::AgentProgress` snapshot. Live-only, like `active_tools`.
    agent_progress: Vec<shared::api::ProgressBar>,
    /// Latest non-Muse neutral ephemeral status line, shown as a transient
    /// transcript-tail strip and cleared by durable output. Muse owns the
    /// richer turn-local overlay below instead.
    ephemeral_status: Option<String>,
    /// Muse's current ephemeral turn, replayed over the matching persisted
    /// task tree so live work updates the same card instead of a bottom bar.
    muse_live_turn: MuseLiveTurn,
    /// Monotonic tick bumped on every `ForwardsChanged` frame; passed to the
    /// forward-chip strip as a prop so it refetches (docs/PORT_FORWARDING.md).
    forwards_refresh: u32,
    /// One-shot hint from an explicit agent forward registration. The surface
    /// opens only after the authoritative REST refetch returns its metadata.
    open_forward_on_load: bool,
    /// Persisted surface layout waiting for the live forward list to validate
    /// its port and provide a fresh URL.
    pending_surface_restore: Option<ForwardSurfaceMemory>,
    active_surface: Option<SessionSurface>,
    plugins: Vec<PortalPluginInfo>,
    plugin_error: Option<String>,
    plugin_notice_expanded: bool,
    edit_stack_items: Vec<EditStackItem>,
    edit_stack_active: Option<(Uuid, Uuid)>,
    edit_stack_send_all: bool,
    edit_stack_error: Option<String>,
    edit_stack_panel_open: bool,
    edit_stack_view: EditStackView,
    surface_split_percent: f64,
    body_ref: NodeRef,
    resize_listeners: Vec<EventListener>,
    #[allow(dead_code)]
    surface_escape_listener: Option<EventListener>,
    show_fork_dialog: bool,
    show_claude_login: bool,
}

impl Component for SessionView {
    type Message = SessionViewMsg;
    type Properties = SessionViewProps;

    fn create(ctx: &Context<Self>) -> Self {
        let link = ctx.link().clone();
        let session_id = ctx.props().session.id;
        let agent_type = ctx.props().session.agent_type;
        let on_awaiting_change = ctx.props().on_awaiting_change.clone();
        let escape_link = ctx.link().clone();
        let surface_escape_listener = web_sys::window().and_then(|window| {
            window.document().map(|document| {
                EventListener::new(&document, "keydown", move |event| {
                    let Some(event) = event.dyn_ref::<KeyboardEvent>() else {
                        return;
                    };
                    if event.key() == "Escape" {
                        escape_link.send_message(SessionViewMsg::EscapeSurface);
                    }
                })
            })
        });

        edit_stack_fetch(ctx.link().clone(), session_id);
        plugin_inventory_fetch(ctx.link().clone(), session_id);

        // Hydrate the per-turn metrics buffer in its own task, off the
        // history→WebSocket critical path (#1915): metrics only feed the
        // chip-strip footer for past turns, so they must never delay live
        // connectivity. Failure is non-fatal — the footer simply stays empty
        // for past turns; live broadcasts still populate the buffer.
        {
            let link = link.clone();
            spawn_local(async move {
                if let Ok(data) = utils::fetch_json::<TurnMetricsResponse>(
                    &format!("/api/sessions/{}/turn-metrics", session_id),
                    On401::Ignore,
                )
                .await
                {
                    link.send_message(SessionViewMsg::LoadTurnMetrics(data.metrics));
                }
            });
        }

        // Fetch existing messages via REST, then connect WebSocket. History
        // must precede the socket: its newest timestamp becomes the
        // `replay_after` watermark that keeps the server replay to a delta.
        spawn_local(async move {
            let mut last_message_time: Option<String> = None;

            // `render_limit` sizes the page server-side to exactly what the
            // local trim keeps (`shared::render_budget`, #1915) — no rows
            // fetched just to be discarded, no underfilled buffer when a turn
            // was dense with free-riding thinking-token markers.
            if let Ok(data) = utils::fetch_json::<MessagesResponse>(
                &format!(
                    "/api/sessions/{}/messages?render_limit={}",
                    session_id, MAX_MESSAGES_PER_SESSION
                ),
                On401::Ignore,
            )
            .await
            {
                let is_awaiting = is_awaiting(data.messages.iter().map(|m| &m.content), agent_type);
                on_awaiting_change.emit((session_id, is_awaiting));

                last_message_time = data.messages.last().map(|m| m.created_at.clone());

                link.send_message(SessionViewMsg::LoadHistory(
                    data.messages,
                    last_message_time.clone(),
                ));
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
            outbox: Outbox::default(),
            pending_upload_prompt: None,
            pending_secret_drop: None,
            early_upload_results: HashMap::new(),
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
            active_tools: Vec::new(),
            agent_progress: Vec::new(),
            ephemeral_status: None,
            muse_live_turn: MuseLiveTurn::default(),
            forwards_refresh: 0,
            open_forward_on_load: false,
            pending_surface_restore: load_open_surface(session_id),
            active_surface: None,
            plugins: Vec::new(),
            plugin_error: None,
            plugin_notice_expanded: load_plugin_notice_expanded(session_id),
            edit_stack_items: Vec::new(),
            edit_stack_active: None,
            edit_stack_send_all: false,
            edit_stack_error: None,
            edit_stack_panel_open: false,
            edit_stack_view: EditStackView::Pending,
            surface_split_percent: load_split_percent(session_id),
            body_ref: NodeRef::default(),
            resize_listeners: Vec::new(),
            surface_escape_listener,
            show_fork_dialog: false,
            show_claude_login: false,
        }
    }

    fn changed(&mut self, ctx: &Context<Self>, old_props: &Self::Properties) -> bool {
        if ctx.props().session.id != old_props.session.id {
            self.active_surface = None;
            self.plugins.clear();
            self.plugin_error = None;
            self.plugin_notice_expanded = load_plugin_notice_expanded(ctx.props().session.id);
            self.edit_stack_items.clear();
            self.edit_stack_active = None;
            self.edit_stack_send_all = false;
            self.edit_stack_error = None;
            self.edit_stack_panel_open = false;
            self.edit_stack_view = EditStackView::Pending;
            self.open_forward_on_load = false;
            self.pending_surface_restore = load_open_surface(ctx.props().session.id);
            self.surface_split_percent = load_split_percent(ctx.props().session.id);
            edit_stack_fetch(ctx.link().clone(), ctx.props().session.id);
            plugin_inventory_fetch(ctx.link().clone(), ctx.props().session.id);
        }

        // Detect interrupt signal change on the focused session. Textarea
        // focus on focused-transition is owned by `InputBar` (it sees the
        // `focused` prop directly through its own `changed()`).
        if ctx.props().focused
            && ctx.props().interrupt_signal != old_props.interrupt_signal
            && ctx.props().interrupt_signal > 0
        {
            ctx.link().send_message(SessionViewMsg::Interrupt);
        }

        // Nav-mode `G` on the focused session: jump to the newest message and
        // resume live tailing. Same counter-prop pattern as `interrupt_signal`.
        if ctx.props().focused
            && ctx.props().jump_to_latest_signal != old_props.jump_to_latest_signal
            && ctx.props().jump_to_latest_signal > 0
        {
            ctx.link().send_message(SessionViewMsg::JumpToLive);
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
            SessionViewMsg::ShowForkDialog => {
                self.show_fork_dialog = true;
                true
            }
            SessionViewMsg::HideForkDialog => {
                self.show_fork_dialog = false;
                true
            }
            SessionViewMsg::ShowClaudeLogin => {
                self.show_claude_login = true;
                true
            }
            SessionViewMsg::HideClaudeLogin => {
                self.show_claude_login = false;
                true
            }
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
                // Cold-load telemetry (#1915): first live connection of the
                // focused session, measured from navigation start.
                if !self.ws_connected && self.reconnect_attempt == 0 && ctx.props().focused {
                    if let Some(now) = web_sys::window()
                        .and_then(|w| w.performance())
                        .map(|p| p.now())
                    {
                        log::info!(
                            "cold-load: focused session {} live in {:.0}ms",
                            ctx.props().session.id,
                            now
                        );
                    }
                }
                self.ws_connected = true;
                self.ws_sender = Some(sender);
                self.reconnect_attempt = 0;
                self.reconnect_timer = None;
                // Flush inputs typed while the socket was down. Only frames
                // never handed to the transport are resent, so this can't
                // duplicate anything the backend already received.
                self.flush_outbox();
                self.drain_edit_stack_queue_if_idle(ctx);
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
                let is_result_awaiting = is_awaiting(
                    self.messages.iter().map(|m| &m.content),
                    ctx.props().session.agent_type,
                );
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
            SessionViewMsg::SendText(input, mode, reasoning_effort) => {
                self.send_text_input(input, mode, reasoning_effort);
                true
            }
            SessionViewMsg::SendFrame(frame) => match frame {
                // User input goes through the outbox so it survives a
                // reconnect; other frames (interrupts, permission responses)
                // are transient and fire-and-forget.
                ClientToServer::AgentInput {
                    content,
                    send_mode,
                    reasoning_effort,
                    ..
                } => {
                    self.dispatch_agent_input(content, send_mode, reasoning_effort);
                    true
                }
                other => {
                    if let Some(ref sender) = self.ws_sender {
                        send_message(sender, other);
                    }
                    false
                }
            },
            SessionViewMsg::UploadPrompt {
                content,
                upload_ids,
                reasoning_effort,
            } => self.handle_upload_prompt(ctx, content, upload_ids, reasoning_effort),
            SessionViewMsg::SecretDrop {
                upload_id,
                file_size,
                reasoning_effort,
            } => self.handle_secret_drop(ctx, upload_id, file_size, reasoning_effort),
            SessionViewMsg::OpenForwardSurface(forward) => {
                if let Some(surface) = self.active_surface.as_mut() {
                    let same_forward = surface
                        .forward()
                        .is_some_and(|current| current.port == forward.port);
                    if same_forward {
                        surface.update_forward(forward);
                        surface.collapsed = !surface.collapsed;
                        save_open_surface(surface);
                        return true;
                    }
                }
                let mode = if is_mobile_surface_viewport() {
                    SessionSurfaceMode::Fullscreen
                } else {
                    SessionSurfaceMode::Split
                };
                let surface = SessionSurface::from_forward(ctx.props().session.id, forward, mode);
                save_open_surface(&surface);
                self.edit_stack_panel_open = false;
                self.active_surface = Some(surface);
                true
            }
            SessionViewMsg::EditStackLoaded(items) => {
                self.edit_stack_items = items;
                self.edit_stack_error = None;
                self.drain_edit_stack_queue_if_idle(ctx);
                true
            }
            SessionViewMsg::EditStackRequestFailed(err) => {
                self.edit_stack_error = Some(err);
                true
            }
            SessionViewMsg::PluginInventoryLoaded(plugins) => {
                self.plugins = plugins;
                self.plugin_error = None;
                true
            }
            SessionViewMsg::PluginInventoryFailed(err) => {
                self.plugins.clear();
                self.plugin_error = Some(err);
                true
            }
            SessionViewMsg::TogglePluginsPanel => {
                let plugins_open = self
                    .active_surface
                    .as_ref()
                    .is_some_and(SessionSurface::is_plugins);
                if plugins_open {
                    self.active_surface = None;
                    self.resize_listeners.clear();
                } else {
                    let mode = if is_mobile_surface_viewport() {
                        SessionSurfaceMode::Fullscreen
                    } else {
                        SessionSurfaceMode::Split
                    };
                    self.active_surface =
                        Some(SessionSurface::from_plugins(ctx.props().session.id, mode));
                    self.edit_stack_panel_open = false;
                    plugin_inventory_fetch(ctx.link().clone(), ctx.props().session.id);
                }
                true
            }
            SessionViewMsg::HidePluginsPanel => {
                if self
                    .active_surface
                    .as_ref()
                    .is_some_and(SessionSurface::is_plugins)
                {
                    self.active_surface = None;
                    self.resize_listeners.clear();
                    return true;
                }
                false
            }
            SessionViewMsg::TogglePluginNotice => {
                self.plugin_notice_expanded = !self.plugin_notice_expanded;
                save_plugin_notice_expanded(ctx.props().session.id, self.plugin_notice_expanded);
                true
            }
            SessionViewMsg::ToggleEditStackPanel => {
                let queue_open = self
                    .active_surface
                    .as_ref()
                    .is_some_and(SessionSurface::is_work_queue);
                if queue_open {
                    self.active_surface = None;
                    self.edit_stack_panel_open = false;
                    self.resize_listeners.clear();
                } else {
                    let mode = if is_mobile_surface_viewport() {
                        SessionSurfaceMode::Fullscreen
                    } else {
                        SessionSurfaceMode::Split
                    };
                    self.active_surface = Some(SessionSurface::from_work_queue(
                        ctx.props().session.id,
                        mode,
                    ));
                    self.edit_stack_panel_open = true;
                    edit_stack_fetch(ctx.link().clone(), ctx.props().session.id);
                }
                true
            }
            SessionViewMsg::HideEditStackPanel => {
                self.edit_stack_panel_open = false;
                if self
                    .active_surface
                    .as_ref()
                    .is_some_and(SessionSurface::is_work_queue)
                {
                    self.active_surface = None;
                    self.resize_listeners.clear();
                }
                true
            }
            SessionViewMsg::ShowPendingEditStack => {
                self.edit_stack_view = EditStackView::Pending;
                true
            }
            SessionViewMsg::ShowPastEditStack => {
                self.edit_stack_view = EditStackView::Past;
                true
            }
            SessionViewMsg::SendNextEditStackItem => {
                if let Some(item_id) = self
                    .edit_stack_items
                    .iter()
                    .find(|item| item.status == "pending")
                    .map(|item| item.id)
                {
                    self.edit_stack_send_all = false;
                    self.send_edit_stack_item(ctx, item_id)
                } else {
                    false
                }
            }
            SessionViewMsg::SendEditStackItem(item_id) => self.send_edit_stack_item(ctx, item_id),
            SessionViewMsg::SendAllEditStack => {
                self.edit_stack_send_all = true;
                self.drain_edit_stack_queue_if_idle(ctx);
                true
            }
            SessionViewMsg::DismissEditStackItem(item_id) => {
                self.dismiss_edit_stack_item(ctx, item_id);
                true
            }
            SessionViewMsg::ForwardsLoaded(forwards) => {
                if self.open_forward_on_load {
                    self.open_forward_on_load = false;
                    if let Some(forward) = forwards.first() {
                        let mode = if is_mobile_surface_viewport() {
                            SessionSurfaceMode::Fullscreen
                        } else {
                            SessionSurfaceMode::Split
                        };
                        let surface = SessionSurface::from_forward(
                            ctx.props().session.id,
                            forward.clone(),
                            mode,
                        );
                        save_open_surface(&surface);
                        self.edit_stack_panel_open = false;
                        self.pending_surface_restore = None;
                        self.active_surface = Some(surface);
                        return true;
                    }
                }
                if self.active_surface.is_none() {
                    if let Some(memory) = self.pending_surface_restore.take() {
                        if let Some(surface) = memory.restore(ctx.props().session.id, &forwards) {
                            self.edit_stack_panel_open = false;
                            self.active_surface = Some(surface);
                            return true;
                        }
                        clear_open_surface(ctx.props().session.id);
                    }
                }
                let Some(surface) = self.active_surface.as_mut() else {
                    return false;
                };
                let Some(current) = surface.forward() else {
                    return false;
                };
                if let Some(next) = forwards.iter().find(|forward| forward.port == current.port) {
                    if next != current {
                        surface.update_forward(next.clone());
                        return true;
                    }
                    return false;
                }
                self.active_surface = None;
                clear_open_surface(ctx.props().session.id);
                true
            }
            SessionViewMsg::CloseSurface => {
                let was_transient = self
                    .active_surface
                    .as_ref()
                    .is_some_and(|surface| surface.is_work_queue() || surface.is_plugins());
                self.active_surface = None;
                self.pending_surface_restore = None;
                if was_transient {
                    self.edit_stack_panel_open = false;
                } else {
                    clear_open_surface(ctx.props().session.id);
                }
                self.resize_listeners.clear();
                true
            }
            SessionViewMsg::ToggleSurfaceCollapsed => {
                let Some(surface) = self.active_surface.as_mut() else {
                    return false;
                };
                surface.collapsed = !surface.collapsed;
                save_open_surface(surface);
                true
            }
            SessionViewMsg::ToggleSurfaceMode => {
                let Some(surface) = self.active_surface.as_mut() else {
                    return false;
                };
                surface.mode = match surface.mode {
                    SessionSurfaceMode::Split => SessionSurfaceMode::Fullscreen,
                    SessionSurfaceMode::Fullscreen => SessionSurfaceMode::Split,
                };
                save_open_surface(surface);
                true
            }
            SessionViewMsg::SurfaceResizeStart(event) => {
                event.prevent_default();
                if event.button() != 0 {
                    return false;
                }
                if self.active_surface.is_none() {
                    return false;
                }
                let Some(element) = self.body_ref.cast::<Element>() else {
                    return false;
                };
                // Capture the pointer on the handle. The surface next to it is
                // an iframe, and without capture the parent document stops
                // receiving move/up events the moment the cursor crosses into
                // it, which ends the drag after a pixel or two.
                let Some(handle) = event.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
                    return false;
                };
                if handle.set_pointer_capture(event.pointer_id()).is_err() {
                    return false;
                }
                let rect = element.get_bounding_client_rect();
                let vertical = rect.width() < 900.0;
                let link = ctx.link().clone();
                let move_listener = EventListener::new(&handle, "pointermove", move |event| {
                    let Some(event) = event.dyn_ref::<PointerEvent>() else {
                        return;
                    };
                    let raw_percent = if vertical {
                        ((rect.bottom() - f64::from(event.client_y())) / rect.height()) * 100.0
                    } else {
                        ((rect.right() - f64::from(event.client_x())) / rect.width()) * 100.0
                    };
                    link.send_message(SessionViewMsg::SurfaceResizeTo(raw_percent));
                });
                // `lostpointercapture` fires after pointerup/pointercancel and
                // whenever the browser revokes capture, so it is the one
                // reliable end-of-drag signal.
                let link = ctx.link().clone();
                let end_listener = EventListener::new(&handle, "lostpointercapture", move |_| {
                    link.send_message(SessionViewMsg::SurfaceResizeEnd);
                });
                self.resize_listeners = vec![move_listener, end_listener];
                true
            }
            SessionViewMsg::SurfaceResizeTo(percent) => {
                self.surface_split_percent = clamp_split_percent(percent);
                true
            }
            SessionViewMsg::SurfaceResizeEnd => {
                self.resize_listeners.clear();
                save_split_percent(ctx.props().session.id, self.surface_split_percent);
                true
            }
            SessionViewMsg::EscapeSurface => {
                let Some(surface) = self.active_surface.as_mut() else {
                    return false;
                };
                if surface.mode == SessionSurfaceMode::Fullscreen {
                    surface.mode = SessionSurfaceMode::Split;
                    save_open_surface(surface);
                } else {
                    let was_transient = surface.is_work_queue() || surface.is_plugins();
                    self.active_surface = None;
                    if was_transient {
                        self.edit_stack_panel_open = false;
                    } else {
                        clear_open_surface(ctx.props().session.id);
                    }
                }
                true
            }
            SessionViewMsg::UploadCommitTimeout => {
                if self.pending_secret_drop.take().is_some() {
                    self.push_upload_error("secret upload commit timed out");
                    return true;
                }
                if let Some(pending) = self.pending_upload_prompt.take() {
                    // Old proxy (no upload acks) or a very slow link: fall
                    // back to pre-transactional behavior instead of eating
                    // the prompt.
                    log::warn!(
                        "Upload commit ack timed out ({} outstanding); sending prompt anyway",
                        pending.remaining.len()
                    );
                    self.dispatch_agent_input(
                        serde_json::Value::String(pending.content),
                        None,
                        pending.reasoning_effort,
                    );
                    true
                } else {
                    false
                }
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
                if group_is_turn_terminator(g, ctx.props().session.agent_type) {
                    metrics_iter.next().cloned()
                } else {
                    None
                }
            })
            .collect();
        // Seed each thinking chip's odometer with the prior burst's max in
        // its turn so tool-call splits don't re-race the count from 0.
        let thinking_starts = thinking_chip_starts(&groups, ctx.props().session.agent_type);
        let live_muse_group = self.muse_live_turn.causation_id.as_deref().and_then(|id| {
            groups
                .iter()
                .rposition(|group| group.muse_causation_id().as_deref() == Some(id))
        });
        let unmatched_muse_tree = if ctx.props().session.agent_type == shared::AgentType::Muse
            && !self.muse_live_turn.events.is_empty()
            && live_muse_group.is_none()
        {
            let mut tree = crate::components::muse_renderer::TaskTree::default();
            for event in &self.muse_live_turn.events {
                tree.apply(event);
            }
            Some(tree)
        } else {
            None
        };
        let launcher_version = ctx
            .props()
            .session
            .launcher_version
            .as_deref()
            .filter(|version| shared::strings::is_non_empty(version));
        let status_class = if ctx.props().session.status.as_str() == "active" {
            "status connected"
        } else {
            "status disconnected"
        };
        // Only the session owner may revoke a forward; the chip strip hides
        // the revoke button for other members (the backend enforces this too).
        let is_forward_owner = ctx
            .props()
            .current_user_id
            .as_deref()
            .is_some_and(|uid| uid == ctx.props().session.user_id.to_string());
        let pending_work_count = self
            .edit_stack_items
            .iter()
            .filter(|item| item.status == "pending")
            .count();
        let past_work_count = self
            .edit_stack_items
            .iter()
            .filter(|item| item.status != "pending")
            .count();
        let queue_open = self
            .active_surface
            .as_ref()
            .is_some_and(SessionSurface::is_work_queue);
        let work_queue_needs_attention = pending_work_count > 0 || self.edit_stack_error.is_some();
        let work_queue_active = queue_open || work_queue_needs_attention;
        let suggested_plugin_count = suggested_plugins(&self.plugins).len();
        let plugin_count_label = if suggested_plugin_count > 0 {
            suggested_plugin_count
        } else {
            self.plugins.len()
        };
        let plugins_open = self
            .active_surface
            .as_ref()
            .is_some_and(SessionSurface::is_plugins);
        let plugins_active =
            plugins_open || suggested_plugin_count > 0 || self.plugin_error.is_some();

        html! {
            <div class={classes!("session-view", ctx.props().focused.then_some("focused"))}>
                <div class="session-view-header">
                    <span class="session-name">{ &ctx.props().session.session_name }</span>
                    // Which model this session is running — claude's transcript
                    // badges carry it per-message, but codex and muse wire
                    // events don't, so the header is the one place it reads
                    // uniformly. `last_model` comes from turn metrics (all
                    // three agents emit them) with the live-turn overlay from
                    // the page, so it tracks mid-session model switches.
                    if let Some(model) = ctx.props().session.last_model.as_deref() {
                        <span class="session-model" title="Model this session last reported">
                            { model }
                        </span>
                    }
                    <span class="session-hostname">{ &ctx.props().session.hostname }</span>
                    <span class="session-path">{ &ctx.props().session.working_directory }</span>
                    if let Some(version) = launcher_version {
                        <span
                            class="session-launcher-version"
                            title="Launcher version"
                        >
                            { format!("launcher v{}", version) }
                        </span>
                    }
                    <button
                        type="button"
                        class={classes!(
                            "session-header-action",
                            "plugin-action",
                            plugins_active.then_some("active"),
                        )}
                        title={if plugins_open { "Hide plugins" } else { "View plugin discovery" }}
                        aria-controls="session-plugins"
                        aria-expanded={plugins_open.to_string()}
                        onclick={ctx.link().callback(|_| SessionViewMsg::TogglePluginsPanel)}
                    >
                        {
                            if plugin_count_label > 0 {
                                format!("Plugins {plugin_count_label}")
                            } else {
                                "Plugins".to_string()
                            }
                        }
                    </button>
                    <ForwardChips
                        session_id={ctx.props().session.id}
                        is_owner={is_forward_owner}
                        refresh={self.forwards_refresh}
                        on_open={ctx.link().callback(SessionViewMsg::OpenForwardSurface)}
                        on_loaded={ctx.link().callback(SessionViewMsg::ForwardsLoaded)}
                    />
                    <button
                        type="button"
                        class={classes!(
                            "session-header-action",
                            "work-queue-action",
                            work_queue_active.then_some("active"),
                        )}
                        title={if queue_open { "Hide work queue" } else { "View work queue" }}
                        aria-controls="session-work-queue"
                        aria-expanded={queue_open.to_string()}
                        onclick={ctx.link().callback(|_| SessionViewMsg::ToggleEditStackPanel)}
                    >
                        {
                            if pending_work_count > 0 {
                                format!("Queue {pending_work_count}")
                            } else if past_work_count > 0 {
                                format!("Queue · {past_work_count}")
                            } else {
                                "Queue".to_string()
                            }
                        }
                    </button>
                    <span class={status_class}>{ ctx.props().session.status.as_str() }</span>
                    if ctx.props().session.my_role == shared::SessionRole::Owner
                        && ctx.props().session.launcher_id.is_some()
                        && !matches!(
                            ctx.props().session.agent_type,
                            shared::AgentType::Muse | shared::AgentType::Antigravity
                        )
                    {
                        <button
                            type="button"
                            class="session-header-action"
                            onclick={ctx.link().callback(|_| SessionViewMsg::ShowForkDialog)}
                        >{ "Fork…" }</button>
                    }
                </div>
                <div
                    ref={self.body_ref.clone()}
                    class={classes!(
                    "session-view-body",
                    self.active_surface.is_some().then_some("has-surface"),
                    self.active_surface.as_ref().and_then(|surface| {
                        (surface.mode == SessionSurfaceMode::Fullscreen).then_some("fullscreen")
                    }),
                    self.active_surface.as_ref().and_then(|surface| {
                        surface.collapsed.then_some("surface-collapsed")
                    }),
                    (!self.resize_listeners.is_empty()).then_some("surface-resizing"),
                    )}
                    style={format!("--surface-basis: {:.1}%;", self.surface_split_percent)}
                >
                    <div class="session-view-chat">
                        <div class="session-view-scroll-area">
                            <div class="session-view-messages" ref={self.messages_ref.clone()}>
                                if let Some(source_id) = ctx.props().session.forked_from_session_id {
                                    <div class="fork-lineage-card">
                                        { "Forked from " }
                                        <a href={format!("/dashboard?session={source_id}")}>{ shared::short_uuid(&source_id) }</a>
                                        if let Some(point) = &ctx.props().session.fork_point_turn_id {
                                            <span>{ format!(" · turn {point}") }</span>
                                        }
                                        <span>{ " · shared agent history remains on the source launcher" }</span>
                                    </div>
                                }
                                { self.render_plugin_notice(ctx) }
                                {
                                    groups.into_iter().enumerate().map(|(i, group)| {
                                        let key = group.key(i);
                                        let metrics = group_metrics.get(i).cloned().flatten();
                                        let thinking_start = thinking_starts.get(i).copied().unwrap_or(0);
                                        let muse_live_events = if live_muse_group == Some(i) { self.muse_live_turn.events.clone() } else { Vec::new() };
                                        let on_claude_login = self.claude_login_callback(ctx);
                                        html! { <MessageGroupRenderer {key} group={group} session_id={ctx.props().session.id} agent_type={ctx.props().session.agent_type} current_user_id={ctx.props().current_user_id.clone()} turn_metrics={metrics} {thinking_start} {muse_live_events} continuation_statuses={self.continuation_statuses.clone()} on_schedule_continuation={on_schedule_continuation.clone()} {on_claude_login} /> }
                                    }).collect::<Html>()
                                }
                                { for self.pending_sends.iter().enumerate().map(|(i, message)| {
                                    html! { <MessageRenderer key={format!("p{}", i)} message={message.clone()} session_id={ctx.props().session.id} agent_type={ctx.props().session.agent_type} current_user_id={ctx.props().current_user_id.clone()} continuation_statuses={self.continuation_statuses.clone()} on_schedule_continuation={on_schedule_continuation.clone()} on_claude_login={self.claude_login_callback(ctx)} /> }
                                })}
                                if let Some(tree) = unmatched_muse_tree {
                                    <div class="claude-message muse-message muse-task-card muse-live-card">
                                        <div class="message-header">
                                            <span class="message-type-badge muse">{ "Muse" }</span>
                                        </div>
                                        <div class="message-body">
                                            { crate::components::muse_renderer::render_task_tree(&tree) }
                                        </div>
                                    </div>
                                }
                                { self.render_active_tools() }
                                { self.render_ephemeral_status() }
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
                        { self.render_agent_progress() }
                        { self.render_input_bar(ctx) }
                    </div>
                    if let Some(surface) = self.active_surface.clone() {
                        if surface.mode == SessionSurfaceMode::Split && !surface.collapsed {
                            <div
                                class="session-surface-resize-handle"
                                role="separator"
                                aria-orientation="vertical"
                                title="Resize surface"
                                onpointerdown={ctx.link().callback(SessionViewMsg::SurfaceResizeStart)}
                            />
                        }
                        if let Some(forward) = surface.forward().cloned() {
                            <ForwardSurface
                                session_id={ctx.props().session.id}
                                {surface}
                                {forward}
                                on_close={ctx.link().callback(|_| SessionViewMsg::CloseSurface)}
                                on_toggle_collapsed={ctx.link().callback(|_| SessionViewMsg::ToggleSurfaceCollapsed)}
                                on_toggle_mode={ctx.link().callback(|_| SessionViewMsg::ToggleSurfaceMode)}
                            />
                        } else if surface.is_work_queue() {
                            { self.render_edit_stack_surface(ctx, surface) }
                        } else if surface.is_plugins() {
                            { self.render_plugin_surface(ctx, surface) }
                        }
                    }
                </div>

                if self.show_fork_dialog {
                    <ForkDialog
                        session={ctx.props().session.clone()}
                        on_close={ctx.link().callback(|_| SessionViewMsg::HideForkDialog)}
                    />
                }
                if self.show_claude_login {
                    if let Some(launcher_id) = ctx.props().session.launcher_id {
                        <AgentLoginModal
                            {launcher_id}
                            agent_type={shared::AgentType::Claude}
                            agent_name={"Claude"}
                            on_close={ctx.link().callback(|_| SessionViewMsg::HideClaudeLogin)}
                            on_success={Callback::noop()}
                        />
                    }
                }
            </div>
        }
    }
}

// Helper methods extracted from the main impl
impl SessionView {
    fn claude_login_callback(&self, ctx: &Context<Self>) -> Option<Callback<()>> {
        (ctx.props().session.agent_type == shared::AgentType::Claude
            && ctx.props().session.my_role == shared::SessionRole::Owner
            && ctx.props().session.launcher_id.is_some())
        .then(|| ctx.link().callback(|_| SessionViewMsg::ShowClaudeLogin))
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
            WsEvent::Output(message) => {
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
                if let Some(ts) = message.meta.as_ref().and_then(|m| m.created_at.clone()) {
                    self.last_message_timestamp = Some(ts);
                }
                ctx.link()
                    .send_message(SessionViewMsg::ReceivedOutput(message));
                ctx.link().send_message(SessionViewMsg::CheckAwaiting);
                false
            }
            WsEvent::HistoryBatch(messages, last_created_at) => {
                // A reconnect batch is the authoritative state since the last
                // watermark. Discard pre-disconnect Muse ephemera before
                // replaying it; a terminal record may have landed while this
                // browser was offline and therefore never visited the normal
                // live-output cleanup path.
                self.muse_live_turn = MuseLiveTurn::default();
                self.messages.extend(messages);
                retain_newest_items_by_cost(
                    &mut self.messages,
                    MAX_MESSAGES_PER_SESSION,
                    |message| counts_toward_render_limit(&message.content),
                );
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
                let perm = enrich_codex_file_change_permission(perm, &self.messages);
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
            WsEvent::InputProgress {
                client_msg_id,
                stage,
                message,
            } => {
                // Terminal outcome — the backend has taken responsibility
                // (accepted) or given up (failed); either way stop tracking it
                // for resend so a later reconnect won't re-deliver.
                if matches!(
                    stage,
                    shared::InputDeliveryStage::AgentAccepted | shared::InputDeliveryStage::Failed
                ) {
                    self.outbox.resolve(client_msg_id);
                }
                let updated = update_pending_send_delivery(
                    &mut self.pending_sends,
                    client_msg_id,
                    stage,
                    message.as_deref(),
                );
                if let Some((item_id, active_client_msg_id)) = self.edit_stack_active {
                    if active_client_msg_id == client_msg_id {
                        match stage {
                            shared::InputDeliveryStage::AgentAccepted => {
                                self.mark_edit_stack_item_sent(ctx, item_id, client_msg_id);
                            }
                            shared::InputDeliveryStage::Failed => {
                                self.edit_stack_active = None;
                                self.edit_stack_send_all = false;
                                self.edit_stack_error = Some(message.unwrap_or_else(|| {
                                    "Edit-stack item failed before the agent accepted it"
                                        .to_string()
                                }));
                            }
                            _ => {}
                        }
                    }
                }
                updated
            }
            WsEvent::ContinuationStatus {
                continuation_id,
                status,
            } => {
                ctx.link()
                    .send_message(SessionViewMsg::ContinuationStatus(continuation_id, status));
                false
            }
            WsEvent::ForwardsChanged { open_preview } => {
                // Bump the counter the chip strip watches; it refetches the
                // forward list (docs/PORT_FORWARDING.md).
                self.open_forward_on_load |= open_preview;
                self.forwards_refresh = self.forwards_refresh.wrapping_add(1);
                true
            }
            WsEvent::EditStackUpdated { items } => {
                self.edit_stack_items = items;
                self.edit_stack_error = None;
                true
            }
            WsEvent::UploadResult(fields) => self.handle_upload_result(fields),
            WsEvent::ToolProgress {
                tool_use_id,
                parent_tool_use_id,
                tool_name,
                elapsed_time_seconds,
                subagent_type,
                subagent_retry,
            } => {
                // Ephemeral live status: refresh the running-tool strip. Never
                // touches `messages` (no persistence, no replay watermark).
                let key = running_tool_key(&tool_use_id, parent_tool_use_id.as_deref());
                upsert_tool_progress(
                    &mut self.active_tools,
                    ActiveToolProgress {
                        key,
                        tool_name,
                        elapsed_seconds: elapsed_time_seconds,
                        subagent_type,
                        subagent_retry,
                    },
                );
                true
            }
            WsEvent::AgentProgress(bars) => {
                if self.agent_progress == bars {
                    return false;
                }
                self.agent_progress = bars;
                true
            }
            WsEvent::Ephemeral(payload) => {
                if payload.get("type").and_then(|value| value.as_str()) == Some("prompt_suggestion")
                {
                    if let Ok(shared::ClaudeOutput::PromptSuggestion(suggestion)) =
                        serde_json::from_value(payload.clone())
                    {
                        if let Some(dispatcher) = &self.input_bar_dispatcher {
                            dispatcher
                                .emit(InputBarInbound::PromptSuggestion(suggestion.suggestion));
                        }
                    }
                    return false;
                }
                if ctx.props().session.agent_type == shared::AgentType::Muse
                    && payload.get("type").and_then(|value| value.as_str()) == Some("muse_record")
                {
                    self.muse_live_turn.push(payload);
                    return true;
                }
                // Non-Muse transient status: replace the strip line. Never touches
                // `messages` (no persistence, no replay watermark). A frame we
                // can't summarize is ignored rather than clearing a good line.
                // Muse was routed into its task-tree overlay above.
                match ephemeral_summary(&payload) {
                    Some(summary) => {
                        self.ephemeral_status = Some(summary);
                        true
                    }
                    None => false,
                }
            }
        }
    }

    /// InputBar handed over the composed upload prompt: consume any commit
    /// results that raced ahead of the handoff, then either dispatch
    /// immediately, fail loudly, or hold for the outstanding ids (#939).
    fn handle_upload_prompt(
        &mut self,
        ctx: &Context<Self>,
        content: String,
        upload_ids: Vec<String>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> bool {
        let mut remaining: std::collections::HashSet<String> = upload_ids.into_iter().collect();
        let mut early_failure: Option<String> = None;
        remaining.retain(|id| match self.early_upload_results.remove(id) {
            Some(fields) if fields.success => false,
            Some(fields) => {
                early_failure = Some(fields.error.unwrap_or_else(|| "upload failed".to_string()));
                true
            }
            None => true,
        });
        self.early_upload_results.clear();

        if let Some(err) = early_failure {
            self.push_upload_error(&err);
            return true;
        }
        if remaining.is_empty() {
            self.dispatch_agent_input(serde_json::Value::String(content), None, reasoning_effort);
            return true;
        }

        let link = ctx.link().clone();
        let timeout = Timeout::new(45_000, move || {
            link.send_message(SessionViewMsg::UploadCommitTimeout);
        });
        self.pending_upload_prompt = Some(PendingUploadPrompt {
            remaining,
            content,
            reasoning_effort,
            _timeout: timeout,
        });
        false
    }

    /// A `FileUploadResult` arrived from the proxy (or was synthesized by
    /// the backend). Resolve the held prompt, or stash the result if the
    /// prompt handoff hasn't happened yet.
    fn handle_upload_result(&mut self, fields: shared::FileUploadResultFields) -> bool {
        if self
            .pending_secret_drop
            .as_ref()
            .is_some_and(|pending| pending.upload_id == fields.upload_id)
        {
            let Some(pending) = self.pending_secret_drop.take() else {
                return false;
            };
            if fields.success {
                if let Some(path) = fields.path {
                    let message = shared::PortalMessage::with_content(vec![
                        shared::PortalContent::SecretDrop {
                            path,
                            file_size: pending.file_size,
                        },
                    ]);
                    self.dispatch_agent_input(message.to_json(), None, pending.reasoning_effort);
                } else {
                    self.push_upload_error("secret-drop result did not include a path");
                }
            } else {
                self.push_upload_error(
                    &fields
                        .error
                        .unwrap_or_else(|| "secret upload failed".to_string()),
                );
            }
            return true;
        }
        if let Some(ref mut pending) = self.pending_upload_prompt {
            if pending.remaining.contains(&fields.upload_id) {
                if fields.success {
                    pending.remaining.remove(&fields.upload_id);
                    if pending.remaining.is_empty() {
                        if let Some(done) = self.pending_upload_prompt.take() {
                            self.dispatch_agent_input(
                                serde_json::Value::String(done.content),
                                None,
                                done.reasoning_effort,
                            );
                        }
                    }
                } else {
                    self.pending_upload_prompt = None;
                    let err = fields.error.unwrap_or_else(|| "upload failed".to_string());
                    self.push_upload_error(&err);
                }
                return true;
            }
        }
        // Pre-handoff (or unrelated) result: stash for handle_upload_prompt.
        if self.early_upload_results.len() < 64 {
            self.early_upload_results
                .insert(fields.upload_id.clone(), fields);
        }
        false
    }

    fn handle_secret_drop(
        &mut self,
        ctx: &Context<Self>,
        upload_id: String,
        file_size: u64,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> bool {
        let link = ctx.link().clone();
        let timeout = Timeout::new(45_000, move || {
            link.send_message(SessionViewMsg::UploadCommitTimeout);
        });
        self.pending_secret_drop = Some(PendingSecretDrop {
            upload_id: upload_id.clone(),
            file_size,
            reasoning_effort,
            _timeout: timeout,
        });
        if let Some(fields) = self.early_upload_results.remove(&upload_id) {
            return self.handle_upload_result(fields);
        }
        false
    }

    /// Surface a terminal upload failure in the transcript. The prompt
    /// referencing the file is deliberately NOT sent — the agent must never
    /// be told about a file that isn't fully on disk.
    fn push_upload_error(&mut self, err: &str) {
        self.messages.push(RenderedMessage::local(
            shared::LocalFrame::error(format!(
                "File upload failed: {err} — your message was not sent"
            )),
            None,
        ));
    }

    /// Hydrate the message buffer + task panel from a REST history batch.
    /// Each message is classified once via [`classify_output_msg_type`],
    /// task events are forwarded to the panel via [`derive_task_events`],
    /// and portal metadata stays in the typed sidecar carried by the API.
    fn handle_load_history(
        &mut self,
        ctx: &Context<Self>,
        mut messages: Vec<MessageData>,
        last_timestamp: Option<String>,
    ) {
        retain_newest_items_by_cost(&mut messages, MAX_MESSAGES_PER_SESSION, |message| {
            counts_toward_render_limit(&message.content)
        });
        let session_id = ctx.props().session.id;
        self.dispatch_tasks(TasksInbound::ClearForReplay);
        for msg in &messages {
            let tag = classify_output_msg_type(&msg.content);
            if let Ok(claude_msg) = serde_json::from_str::<shared::ClaudeOutput>(&msg.content) {
                for ev in derive_task_events(&claude_msg, &msg.created_at, false) {
                    self.dispatch_tasks(TasksInbound::Replay(ev));
                }
            }
            let ts_ms = parse_iso_ms_utc(&msg.created_at);
            if ts_ms.is_finite() && !tag.is_suppressed() {
                ctx.props().on_activity.emit((session_id, tag, ts_ms));
            }
        }
        self.messages = messages
            .into_iter()
            .map(|m| RenderedMessage::new(m.content, m.meta))
            .collect();
        self.last_message_timestamp = last_timestamp;
        ctx.link().send_message(SessionViewMsg::CheckAwaiting);
    }

    fn send_edit_stack_item(&mut self, ctx: &Context<Self>, item_id: Uuid) -> bool {
        let Some(item) = self
            .edit_stack_items
            .iter()
            .find(|item| item.id == item_id && item.status == "pending")
            .cloned()
        else {
            return false;
        };
        if self.edit_stack_active.is_some() {
            self.edit_stack_send_all = true;
            return true;
        }
        let content = edit_stack_prompt_content(&item);
        let client_msg_id =
            self.dispatch_agent_input(serde_json::Value::String(content), None, None);
        self.edit_stack_active = Some((item.id, client_msg_id));
        self.edit_stack_error = None;
        self.ephemeral_status = Some(format!("Sent edit-stack item: {}", item.title));
        ctx.props().on_message_sent.emit(ctx.props().session.id);
        true
    }

    fn drain_edit_stack_queue_if_idle(&mut self, ctx: &Context<Self>) {
        if !self.edit_stack_send_all {
            return;
        }
        if self.edit_stack_active.is_some()
            || self.has_pending_permission
            || !self.pending_sends.is_empty()
            || is_awaiting(
                self.messages.iter().map(|message| &message.content),
                ctx.props().session.agent_type,
            )
        {
            return;
        }
        let Some(next_id) = self
            .edit_stack_items
            .iter()
            .find(|item| item.status == "pending")
            .map(|item| item.id)
        else {
            self.edit_stack_send_all = false;
            return;
        };
        self.send_edit_stack_item(ctx, next_id);
    }

    fn mark_edit_stack_item_sent(
        &mut self,
        ctx: &Context<Self>,
        item_id: Uuid,
        client_msg_id: Uuid,
    ) {
        if let Some(item) = self
            .edit_stack_items
            .iter_mut()
            .find(|item| item.id == item_id)
        {
            let now = browser_now_iso();
            item.status = "sent".to_string();
            item.sent_client_msg_id = Some(client_msg_id);
            item.sent_at = Some(now.clone());
            item.updated_at = now;
        }
        let session_id = ctx.props().session.id;
        let link = ctx.link().clone();
        let body = UpdateEditStackItemRequest {
            status: Some("sent".to_string()),
            sent_client_msg_id: Some(client_msg_id),
        };
        spawn_local(async move {
            let result = utils::send_json(
                Request::patch(&utils::api_url(&edit_stack_item_api_path(
                    session_id, item_id,
                ))),
                &body,
            )
            .await;
            match result {
                Ok(response) if response.ok() => match response.json::<EditStackResponse>().await {
                    Ok(data) => link.send_message(SessionViewMsg::EditStackLoaded(data.items)),
                    Err(err) => link.send_message(SessionViewMsg::EditStackRequestFailed(format!(
                        "Edit-stack response was malformed: {err}"
                    ))),
                },
                Ok(response) => {
                    let message = utils::error_body(response).await;
                    link.send_message(SessionViewMsg::EditStackRequestFailed(format!(
                        "Could not update edit-stack item: {message}"
                    )));
                }
                Err(err) => link.send_message(SessionViewMsg::EditStackRequestFailed(format!(
                    "Could not update edit-stack item: {err}"
                ))),
            }
        });
    }

    fn complete_edit_stack_item(
        &mut self,
        ctx: &Context<Self>,
        item_id: Uuid,
        status: &'static str,
    ) {
        if let Some(item) = self
            .edit_stack_items
            .iter_mut()
            .find(|item| item.id == item_id)
        {
            item.status = status.to_string();
            item.updated_at = browser_now_iso();
        }
        let session_id = ctx.props().session.id;
        let link = ctx.link().clone();
        let body = UpdateEditStackItemRequest {
            status: Some(status.to_string()),
            sent_client_msg_id: None,
        };
        spawn_local(async move {
            let result = utils::send_json(
                Request::patch(&utils::api_url(&edit_stack_item_api_path(
                    session_id, item_id,
                ))),
                &body,
            )
            .await;
            match result {
                Ok(response) if response.ok() => match response.json::<EditStackResponse>().await {
                    Ok(data) => link.send_message(SessionViewMsg::EditStackLoaded(data.items)),
                    Err(err) => link.send_message(SessionViewMsg::EditStackRequestFailed(format!(
                        "Edit-stack response was malformed: {err}"
                    ))),
                },
                Ok(response) => {
                    let message = utils::error_body(response).await;
                    link.send_message(SessionViewMsg::EditStackRequestFailed(format!(
                        "Could not update edit-stack item: {message}"
                    )));
                }
                Err(err) => link.send_message(SessionViewMsg::EditStackRequestFailed(format!(
                    "Could not update edit-stack item: {err}"
                ))),
            }
        });
    }

    fn dismiss_edit_stack_item(&mut self, ctx: &Context<Self>, item_id: Uuid) {
        self.edit_stack_items.retain(|item| item.id != item_id);
        if self
            .edit_stack_active
            .is_some_and(|(active_id, _)| active_id == item_id)
        {
            self.edit_stack_active = None;
        }
        let session_id = ctx.props().session.id;
        let link = ctx.link().clone();
        spawn_local(async move {
            let url = utils::api_url(&edit_stack_item_api_path(session_id, item_id));
            match Request::delete(&url).send().await {
                Ok(response) if response.ok() => match response.json::<EditStackResponse>().await {
                    Ok(data) => link.send_message(SessionViewMsg::EditStackLoaded(data.items)),
                    Err(err) => link.send_message(SessionViewMsg::EditStackRequestFailed(format!(
                        "Edit-stack response was malformed: {err}"
                    ))),
                },
                Ok(response) => {
                    let message = utils::error_body(response).await;
                    link.send_message(SessionViewMsg::EditStackRequestFailed(format!(
                        "Could not dismiss edit-stack item: {message}"
                    )));
                }
                Err(err) => link.send_message(SessionViewMsg::EditStackRequestFailed(format!(
                    "Could not dismiss edit-stack item: {err}"
                ))),
            }
        });
    }

    /// Translate a plain-text submission from `InputBar` into an outbox-tracked
    /// `AgentInput`. The bar has already trimmed and cleared its textarea and
    /// emitted `MessageSent` separately; we just dispatch the input.
    fn send_text_input(
        &mut self,
        input: String,
        send_mode: SendMode,
        reasoning_effort: Option<ReasoningEffort>,
    ) {
        if input.is_empty() {
            return;
        }
        let send_mode = (send_mode != SendMode::Normal).then_some(send_mode);
        self.dispatch_agent_input(
            serde_json::Value::String(input),
            send_mode,
            reasoning_effort,
        );
    }

    /// Optimistically echo an `AgentInput`, record it in the outbox (assigning a
    /// fresh `client_msg_id`), and try to transmit. If the socket is down — or
    /// the send fails — the entry stays queued and is flushed on the next
    /// reconnect, so the input is never silently lost.
    fn dispatch_agent_input(
        &mut self,
        content: serde_json::Value,
        send_mode: Option<SendMode>,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Uuid {
        let client_msg_id = Uuid::new_v4();
        if let Some(text) = content.as_str() {
            let now_iso = js_sys::Date::new_0()
                .to_iso_string()
                .as_string()
                .unwrap_or_default();
            self.pending_sends
                .push(optimistic_user_message(text, &now_iso, client_msg_id));
        }
        let frame = ClientToServer::AgentInput {
            content,
            send_mode,
            reasoning_effort,
            client_msg_id: Some(client_msg_id),
        };
        for dropped in self.outbox.record(client_msg_id, frame.clone()) {
            // Evicted from a full outbox — surface as failed rather than leave
            // it displaying as forever-pending.
            update_pending_send_delivery(
                &mut self.pending_sends,
                dropped,
                shared::InputDeliveryStage::Failed,
                Some("send backlog full"),
            );
        }
        self.transmit_input(client_msg_id, frame);
        client_msg_id
    }

    /// Hand a recorded frame to the transport, marking it transmitted on
    /// success. A failure (no socket / closing channel) leaves it queued for
    /// the reconnect flush.
    fn transmit_input(&mut self, client_msg_id: Uuid, frame: ClientToServer) {
        if let Some(sender) = self.ws_sender.clone() {
            if send_message(&sender, frame) {
                self.outbox.mark_transmitted(client_msg_id);
            }
        }
    }

    /// Resend every unresolved outbox frame on reconnect — including ones
    /// already handed to the old (now dead) transport, closing the
    /// in-flight-loss window. The backend dedupes by `client_msg_id` and
    /// re-acks anything it already handled (#1236), so this is at-least-once
    /// with idempotent delivery, never duplicate delivery.
    fn flush_outbox(&mut self) {
        let Some(sender) = self.ws_sender.clone() else {
            return;
        };
        for (client_msg_id, frame) in self.outbox.unresolved() {
            if send_message(&sender, frame) {
                self.outbox.mark_transmitted(client_msg_id);
            }
        }
    }

    fn handle_received_output(&mut self, ctx: &Context<Self>, output: RenderedMessage) -> bool {
        let tag = classify_output_msg_type(&output.content);
        if let Some(shared::MessageSource::Agent { session_id, .. }) = output.source() {
            ctx.props().on_agent_message.emit((
                *session_id,
                ctx.props().session.id,
                js_sys::Date::now(),
            ));
        }
        if let Ok(claude_msg) = serde_json::from_str::<shared::ClaudeOutput>(&output.content) {
            // Live task events use the server-assigned row timestamp when the
            // backend supplied it, falling back to browser time only for
            // pre-metadata/error frames.
            for ev in derive_task_events(&claude_msg, output.raw_iso().unwrap_or_default(), true) {
                self.dispatch_tasks(TasksInbound::Live(ev));
            }
        }
        crate::audio::play_sound(crate::audio::SoundEvent::Activity);
        let activity_ts = output
            .raw_iso()
            .map(parse_iso_ms_utc)
            .filter(|ts| ts.is_finite())
            .unwrap_or_else(js_sys::Date::now);
        if !tag.is_suppressed() {
            ctx.props()
                .on_activity
                .emit((ctx.props().session.id, tag, activity_ts));
        }
        reconcile_pending_sends(&mut self.pending_sends, tag, &output.content);
        if let Some((item_id, _)) = self.edit_stack_active {
            match tag {
                ActivityTag::Result => {
                    self.complete_edit_stack_item(ctx, item_id, "completed");
                    self.edit_stack_active = None;
                }
                ActivityTag::Error => {
                    self.complete_edit_stack_item(ctx, item_id, "failed");
                    self.edit_stack_active = None;
                    self.edit_stack_send_all = false;
                }
                _ => {}
            }
        }

        // Retire any active-tool strip entries this message completes: a
        // tool_result for the running tool, or a turn `result` that ends the
        // turn entirely. (The live heartbeat side-channel only adds entries.)
        clear_completed_tools(&mut self.active_tools, &output.content);
        // Generic status is superseded by any durable message. Muse live
        // records instead stay overlaid on their matching group until the
        // durable terminal record makes the whole turn replayable.
        self.ephemeral_status = None;
        self.muse_live_turn.clear_if_terminal(&output.content);

        push_message_with_cost_limit(
            &mut self.messages,
            output,
            MAX_MESSAGES_PER_SESSION,
            |message| counts_toward_render_limit(&message.content),
        );
        self.drain_edit_stack_queue_if_idle(ctx);
        true
    }

    /// Trailing "active tool" strip: one live pill per currently-running tool,
    /// showing "{tool} running — {elapsed}" and refreshed by the ephemeral
    /// `WsEvent::ToolProgress` heartbeats. Renders nothing when idle. Lives at
    /// the transcript tail rather than mutating historical tool cards so it
    /// stays outside the memoized message-render pipeline (see the `helpers`
    /// tool-progress section for the rationale).
    fn render_active_tools(&self) -> Html {
        if self.active_tools.is_empty() {
            return html! {};
        }
        html! {
            <div class="active-tool-strip">
                { for self.active_tools.iter().map(|tool| {
                    html! {
                        <div class="active-tool-pill" key={tool.key.clone()}>
                            <span class="active-tool-spinner" />
                            <span class="active-tool-name">{ tool.tool_name.clone() }</span>
                            // Which Task sub-agent is running (#1474) — a long
                            // `Task` otherwise reads as an anonymous stall.
                            if let Some(subagent) = tool.subagent_type.as_deref() {
                                <span class="active-tool-subagent">{ subagent }</span>
                            }
                            <span class="active-tool-status">
                                { format!("running — {}", format_tool_elapsed(tool.elapsed_seconds)) }
                            </span>
                            // Only while the sub-agent is retrying: says the
                            // turn is flaky-but-progressing, not hung.
                            if let Some(retry) = tool.subagent_retry.as_ref() {
                                <span class="active-tool-retry">
                                    { format!(
                                        "retrying {}/{} — {}",
                                        retry.attempt, retry.max_retries, retry.error_category
                                    ) }
                                </span>
                            }
                        </div>
                    }
                }) }
            </div>
        }
    }

    /// Agent-driven progress bars, pinned above the input bar so a long job's
    /// progress stays visible while the transcript scrolls. Reuses the upload
    /// bar's track/fill styling; a bar without a fraction is indeterminate.
    fn render_agent_progress(&self) -> Html {
        if self.agent_progress.is_empty() {
            return html! {};
        }
        html! {
            <div class="agent-progress">
                { for self.agent_progress.iter().map(|bar| {
                    let name = bar.label.clone().unwrap_or_else(|| bar.id.clone());
                    let (fill_class, fill_style, pct_text, now) = match bar.fraction {
                        Some(fraction) => {
                            let pct = (fraction * 100.0).round() as u32;
                            (
                                "upload-bar-fill",
                                format!("width: {pct}%"),
                                format!("{pct}%"),
                                Some(pct.to_string()),
                            )
                        }
                        None => (
                            "upload-bar-fill indeterminate",
                            String::new(),
                            String::new(),
                            None,
                        ),
                    };
                    html! {
                        <div class="agent-progress-bar" key={bar.id.clone()}>
                            <div class="agent-progress-header">
                                <span class="agent-progress-label">{ name.clone() }</span>
                                <span class="agent-progress-pct">{ pct_text }</span>
                            </div>
                            <div
                                class="upload-bar-track"
                                role="progressbar"
                                aria-label={name}
                                aria-valuemin="0"
                                aria-valuemax="100"
                                aria-valuenow={now}
                            >
                                <div class={fill_class} style={fill_style} />
                            </div>
                        </div>
                    }
                }) }
            </div>
        }
    }

    /// Transient live-status strip fed by the neutral `WsEvent::Ephemeral`
    /// channel for agents without a richer live renderer. One line, replaced per
    /// frame, cleared when a durable message arrives. Renders nothing when
    /// idle. Deliberately minimal rather than a second transcript model.
    fn render_ephemeral_status(&self) -> Html {
        let Some(status) = self.ephemeral_status.as_deref() else {
            return html! {};
        };
        html! {
            <div class="ephemeral-status-strip">
                <span class="ephemeral-status-spinner" />
                <span class="ephemeral-status-text">{ status }</span>
            </div>
        }
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
            self.messages.push(RenderedMessage::local(
                shared::LocalFrame::error(format!("Connection lost: {}", err)),
                None,
            ));
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

    fn render_plugin_notice(&self, ctx: &Context<Self>) -> Html {
        let suggested = suggested_plugins(&self.plugins);
        if suggested.is_empty() {
            return html! {};
        }
        let context_bytes = suggested
            .iter()
            .map(|plugin| plugin.context_bytes)
            .sum::<u64>();
        let estimated_tokens = suggested
            .iter()
            .map(|plugin| plugin.estimated_tokens)
            .sum::<u64>();
        let label = suggested
            .iter()
            .map(|plugin| plugin.display_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let expanded = self.plugin_notice_expanded;
        let toggle = ctx.link().callback(|_| SessionViewMsg::TogglePluginNotice);

        html! {
            <section class={classes!("plugin-context-card", expanded.then_some("expanded"))}>
                <button
                    type="button"
                    class="plugin-context-summary"
                    onclick={toggle}
                    aria-expanded={expanded.to_string()}
                >
                    <span class="plugin-context-caret">{ if expanded { "▾" } else { "▸" } }</span>
                    <span class="plugin-context-title">{ "Plugin context" }</span>
                    <span class="plugin-context-names">{ label }</span>
                    <span class="plugin-context-size">
                        { plugin_context_label(context_bytes, estimated_tokens) }
                    </span>
                </button>
                if expanded {
                    <div class="plugin-context-body">
                        { for suggested.into_iter().map(|plugin| {
                            html! {
                                <article class="plugin-context-plugin" key={plugin.name.clone()}>
                                    <div class="plugin-context-plugin-title">
                                        <span>{ &plugin.display_name }</span>
                                        <span>{ plugin_context_label(plugin.context_bytes, plugin.estimated_tokens) }</span>
                                    </div>
                                    if let Some(reason) = plugin.reason.as_deref() {
                                        <div class="plugin-context-detail">{ reason }</div>
                                    }
                                    if let Some(source) = plugin.source.as_deref() {
                                        <code class="plugin-context-source">{ source }</code>
                                    }
                                </article>
                            }
                        }) }
                    </div>
                }
            </section>
        }
    }

    fn render_plugin_surface(&self, ctx: &Context<Self>, surface: SessionSurface) -> Html {
        let suggested_count = suggested_plugins(&self.plugins).len();
        let collapsed = surface.collapsed;
        let fullscreen = surface.mode == SessionSurfaceMode::Fullscreen;
        let title = if suggested_count > 0 {
            format!(
                "Plugins · {suggested_count} suggested · {} installed",
                self.plugins.len()
            )
        } else {
            format!("Plugins · {} installed", self.plugins.len())
        };
        let status_class = (suggested_count > 0).then_some("is-up");
        let status_title = if suggested_count > 0 {
            "Plugins match this session directory"
        } else {
            "No plugin matched this session directory"
        };
        let toggle_collapsed = ctx
            .link()
            .callback(|_| SessionViewMsg::ToggleSurfaceCollapsed);
        let toggle_mode = ctx.link().callback(|_| SessionViewMsg::ToggleSurfaceMode);
        let close = ctx.link().callback(|_| SessionViewMsg::HidePluginsPanel);

        html! {
            <aside
                id="session-plugins"
                class={classes!(
                    "session-forward-surface",
                    "session-plugins-surface",
                    collapsed.then_some("collapsed"),
                    fullscreen.then_some("fullscreen"),
                )}
                aria-label="Plugins"
            >
                <div class="session-forward-toolbar session-plugins-toolbar">
                    <button
                        type="button"
                        class="surface-icon-button"
                        title={ if collapsed { "Expand plugins" } else { "Collapse plugins" } }
                        onclick={toggle_collapsed}
                    >
                        { if collapsed { "▸" } else { "▾" } }
                    </button>
                    <span class={classes!("surface-status", status_class)} title={status_title}></span>
                    <span class="session-forward-title" title={title.clone()}>{ title }</span>
                    <button
                        type="button"
                        class="surface-icon-button surface-mode-button"
                        title={ if fullscreen { "Return to split view" } else { "Full screen" } }
                        onclick={toggle_mode}
                    >
                        { if fullscreen { "⇲" } else { "⛶" } }
                    </button>
                    <button
                        type="button"
                        class="surface-icon-button surface-close-button"
                        title="Close plugins"
                        onclick={close}
                    >
                        { "×" }
                    </button>
                </div>
                if !collapsed {
                    <section class="plugin-panel">
                        <div class="plugin-panel-header">
                            <div class="edit-stack-title">
                                <span class="edit-stack-kicker">{ "Plugin discovery" }</span>
                                <span class="edit-stack-count">
                                    { format!("{suggested_count} suggested · {} installed", self.plugins.len()) }
                                </span>
                            </div>
                        </div>
                        if let Some(error) = self.plugin_error.as_deref() {
                            <div class="edit-stack-error">{ error }</div>
                        }
                        <div class="plugin-list">
                            if self.plugins.is_empty() {
                                <div class="edit-stack-empty">{ "No installed Portal plugins were discovered on this host." }</div>
                            }
                            { for self.plugins.iter().map(|plugin| self.render_plugin_card(plugin)) }
                        </div>
                    </section>
                }
            </aside>
        }
    }

    fn render_plugin_card(&self, plugin: &PortalPluginInfo) -> Html {
        let is_suggested = plugin.active || plugin.suggested;
        html! {
            <article class={classes!("plugin-card", is_suggested.then_some("suggested"))} key={plugin.name.clone()}>
                <div class="plugin-card-topline">
                    <div class="plugin-card-title">
                        <span>{ &plugin.display_name }</span>
                        if is_suggested {
                            <span class="plugin-card-badge">{ "suggested" }</span>
                        }
                    </div>
                    <span class="plugin-card-context">
                        { plugin_context_label(plugin.context_bytes, plugin.estimated_tokens) }
                    </span>
                </div>
                if let Some(description) = plugin.description.as_deref() {
                    <p class="plugin-card-description">{ description }</p>
                }
                if let Some(reason) = plugin.reason.as_deref() {
                    <div class="plugin-card-reason">{ reason }</div>
                }
                if let Some(source) = plugin.source.as_deref() {
                    <code class="plugin-card-source">{ source }</code>
                }
                if !plugin.skills.is_empty() {
                    <div class="plugin-card-section">
                        <span class="plugin-card-section-title">{ "Skills" }</span>
                        <div class="plugin-skill-list">
                            { for plugin.skills.iter().map(|skill| {
                                let title = skill
                                    .description
                                    .clone()
                                    .unwrap_or_else(|| skill.path.clone());
                                html! {
                                    <span class="plugin-skill-pill" title={title} key={skill.name.clone()}>
                                        { &skill.name }
                                    </span>
                                }
                            }) }
                        </div>
                    </div>
                }
                if !plugin.commands.is_empty() {
                    <div class="plugin-card-section">
                        <span class="plugin-card-section-title">{ "Commands" }</span>
                        <div class="plugin-skill-list">
                            { for plugin.commands.iter().map(|command| {
                                let title = command
                                    .description
                                    .clone()
                                    .unwrap_or_else(|| "Plugin command".to_string());
                                html! {
                                    <span class="plugin-command-pill" title={title} key={command.name.clone()}>
                                        { &command.name }
                                    </span>
                                }
                            }) }
                        </div>
                    </div>
                }
            </article>
        }
    }

    fn render_edit_stack_surface(&self, ctx: &Context<Self>, surface: SessionSurface) -> Html {
        let pending_count = self
            .edit_stack_items
            .iter()
            .filter(|item| item.status == "pending")
            .count();
        let past_count = self
            .edit_stack_items
            .iter()
            .filter(|item| item.status != "pending")
            .count();
        let collapsed = surface.collapsed;
        let fullscreen = surface.mode == SessionSurfaceMode::Fullscreen;
        let title = format!("Work queue · {pending_count} pending · {past_count} past");
        let status_class = (pending_count > 0).then_some("is-up");
        let status_title = if pending_count > 0 {
            "Queued work is waiting"
        } else {
            "No pending queued work"
        };
        let toggle_collapsed = ctx
            .link()
            .callback(|_| SessionViewMsg::ToggleSurfaceCollapsed);
        let toggle_mode = ctx.link().callback(|_| SessionViewMsg::ToggleSurfaceMode);
        let close = ctx.link().callback(|_| SessionViewMsg::HideEditStackPanel);

        html! {
            <aside
                id="session-work-queue"
                class={classes!(
                    "session-forward-surface",
                    "session-work-queue-surface",
                    collapsed.then_some("collapsed"),
                    fullscreen.then_some("fullscreen"),
                )}
                aria-label="Work queue"
            >
                <div class="session-forward-toolbar session-work-queue-toolbar">
                    <button
                        type="button"
                        class="surface-icon-button"
                        title={ if collapsed { "Expand work queue" } else { "Collapse work queue" } }
                        onclick={toggle_collapsed}
                    >
                        { if collapsed { "▸" } else { "▾" } }
                    </button>
                    <span class={classes!("surface-status", status_class)} title={status_title}></span>
                    <span class="session-forward-title" title={title.clone()}>{ title }</span>
                    <button
                        type="button"
                        class="surface-icon-button surface-mode-button"
                        title={ if fullscreen { "Return to split view" } else { "Full screen" } }
                        onclick={toggle_mode}
                    >
                        { if fullscreen { "⇲" } else { "⛶" } }
                    </button>
                    <button
                        type="button"
                        class="surface-icon-button surface-close-button"
                        title="Close work queue"
                        onclick={close}
                    >
                        { "×" }
                    </button>
                </div>
                if !collapsed {
                    { self.render_edit_stack_panel(ctx) }
                }
            </aside>
        }
    }

    fn render_edit_stack_panel(&self, ctx: &Context<Self>) -> Html {
        let pending = self
            .edit_stack_items
            .iter()
            .filter(|item| item.status == "pending")
            .collect::<Vec<_>>();
        let past = self
            .edit_stack_items
            .iter()
            .filter(|item| item.status != "pending")
            .collect::<Vec<_>>();
        let visible_items = match self.edit_stack_view {
            EditStackView::Pending => pending.clone(),
            EditStackView::Past => past.clone(),
        };
        let creator_count = visible_items
            .iter()
            .map(|item| item.created_by)
            .collect::<HashSet<_>>()
            .len();
        let show_creator = creator_count > 1;
        let busy = self.edit_stack_active.is_some()
            || self.has_pending_permission
            || !self.pending_sends.is_empty()
            || is_awaiting(
                self.messages.iter().map(|message| &message.content),
                ctx.props().session.agent_type,
            );
        let active_item = self.edit_stack_active.map(|(item_id, _)| item_id);
        let send_next = ctx
            .link()
            .callback(|_| SessionViewMsg::SendNextEditStackItem);
        let send_all = ctx.link().callback(|_| SessionViewMsg::SendAllEditStack);
        let show_pending = ctx
            .link()
            .callback(|_| SessionViewMsg::ShowPendingEditStack);
        let show_past = ctx.link().callback(|_| SessionViewMsg::ShowPastEditStack);
        let now_ms = js_sys::Date::now();
        let empty_text = match self.edit_stack_view {
            EditStackView::Pending => "No pending work items.",
            EditStackView::Past => "No past work items.",
        };
        html! {
            <section class="edit-stack-panel">
                <div class="edit-stack-header">
                    <div class="edit-stack-title">
                        <span class="edit-stack-kicker">{ "Work queue" }</span>
                        <span class="edit-stack-count">
                            { format!("{} pending · {} past", pending.len(), past.len()) }
                        </span>
                    </div>
                    <div class="edit-stack-actions">
                        <button
                            type="button"
                            class="edit-stack-action"
                            disabled={pending.is_empty() || busy}
                            onclick={send_next}
                        >
                            { "Send next" }
                        </button>
                        <button
                            type="button"
                            class="edit-stack-action primary"
                            disabled={pending.is_empty() || busy}
                            onclick={send_all}
                        >
                            { if self.edit_stack_send_all { "Sending…" } else { "Send all" } }
                        </button>
                    </div>
                </div>
                <div class="edit-stack-tabs" role="tablist" aria-label="Work queue filters">
                    <button
                        type="button"
                        class={classes!("edit-stack-tab", (self.edit_stack_view == EditStackView::Pending).then_some("active"))}
                        onclick={show_pending}
                    >
                        { format!("Pending {}", pending.len()) }
                    </button>
                    <button
                        type="button"
                        class={classes!("edit-stack-tab", (self.edit_stack_view == EditStackView::Past).then_some("active"))}
                        onclick={show_past}
                    >
                        { format!("Past {}", past.len()) }
                    </button>
                </div>
                if let Some(error) = self.edit_stack_error.as_deref() {
                    <div class="edit-stack-error">{ error }</div>
                }
                <div class="edit-stack-list">
                    if visible_items.is_empty() {
                        <div class="edit-stack-empty">{ empty_text }</div>
                    }
                    { visible_items.into_iter().map(|item| {
                        let item_id = item.id;
                        let is_active = active_item == Some(item_id);
                        let image = item.image_data_url.clone();
                        let body = if !utils::is_non_blank(&item.body) {
                            "(no text note)".to_string()
                        } else {
                            truncate_with_ellipsis(&item.body, EDIT_STACK_BODY_PREVIEW_CHARS)
                        };
                        let context = item
                            .context
                            .as_ref()
                            .and_then(pretty_json)
                            .map(|value| truncate_with_ellipsis(&value, EDIT_STACK_CONTEXT_PREVIEW_CHARS));
                        let creator = item.created_by_name.clone();
                        let send_one = ctx.link().callback(move |_| SessionViewMsg::SendEditStackItem(item_id));
                        let dismiss = ctx.link().callback(move |_| SessionViewMsg::DismissEditStackItem(item_id));
                        let status_label = edit_stack_status_label(&item.status);
                        let status_class = match item.status.as_str() {
                            "pending" => "pending",
                            "sent" => "sent",
                            "completed" => "completed",
                            "failed" => "failed",
                            "dismissed" => "dismissed",
                            _ => "other",
                        };
                        let elapsed = edit_stack_elapsed_label(item, &self.edit_stack_items, now_ms);
                        html! {
                            <article class={classes!("edit-stack-item", is_active.then_some("active"))} key={item.id.to_string()}>
                                if let Some(src) = image {
                                    <img class="edit-stack-thumb" src={src} alt="Captured annotation region" />
                                } else {
                                    <div class="edit-stack-thumb empty">{ "TXT" }</div>
                                }
                                <div class="edit-stack-item-main">
                                    <div class="edit-stack-item-topline">
                                        <span class="edit-stack-item-title">{ &item.title }</span>
                                        if show_creator {
                                            if let Some(name) = creator {
                                                <span class="edit-stack-creator">{ name }</span>
                                            }
                                        }
                                    </div>
                                    <div class="edit-stack-item-meta">
                                        <span class={classes!("edit-stack-status", status_class)}>
                                            { status_label }
                                        </span>
                                        if let Some(elapsed) = elapsed {
                                            <span class="edit-stack-elapsed">{ elapsed }</span>
                                        }
                                    </div>
                                    <div class="edit-stack-note">{ body }</div>
                                    if let Some(context) = context {
                                        <pre class="edit-stack-context">{ context }</pre>
                                    }
                                </div>
                                <div class="edit-stack-item-actions">
                                    if item.status == "pending" {
                                        <button
                                            type="button"
                                            class="edit-stack-icon-action"
                                            disabled={busy || is_active}
                                            onclick={send_one}
                                            title="Send this item"
                                        >
                                            { "Send" }
                                        </button>
                                    }
                                    <button
                                        type="button"
                                        class="edit-stack-icon-action"
                                        onclick={dismiss}
                                        title={ if item.status == "pending" { "Dismiss this item" } else { "Remove this item" } }
                                    >
                                        { if item.status == "pending" { "Dismiss" } else { "Remove" } }
                                    </button>
                                </div>
                            </article>
                        }
                    }).collect::<Html>() }
                </div>
            </section>
        }
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
        let on_send_text = link.callback(
            |(text, mode, reasoning_effort): (String, SendMode, Option<ReasoningEffort>)| {
                SessionViewMsg::SendText(text, mode, reasoning_effort)
            },
        );
        let on_send_frame = link.callback(SessionViewMsg::SendFrame);
        let on_upload_prompt = link.callback(
            |(content, upload_ids, reasoning_effort): (
                String,
                Vec<String>,
                Option<ReasoningEffort>,
            )| {
                SessionViewMsg::UploadPrompt {
                    content,
                    upload_ids,
                    reasoning_effort,
                }
            },
        );
        let on_secret_drop = link.callback(
            |(upload_id, file_size, reasoning_effort): (String, u64, Option<ReasoningEffort>)| {
                SessionViewMsg::SecretDrop {
                    upload_id,
                    file_size,
                    reasoning_effort,
                }
            },
        );
        let on_message_sent = link.callback(|_| SessionViewMsg::MessageSent);
        html! {
            <InputBar
                session_id={ctx.props().session.id}
                focused={ctx.props().focused}
                ws_connected={self.ws_connected}
                stt_enabled={ctx.props().stt_enabled}
                {on_register}
                {on_send_text}
                {on_send_frame}
                {on_upload_prompt}
                {on_secret_drop}
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
