//! Pure helpers extracted from `SessionView`.
//!
//! These functions take only typed arguments and return only typed results —
//! no `&self`, no `Context`, no DOM, no timers — so each one is independently
//! testable without mounting the Yew component. The orchestrator in
//! `component.rs` calls into them from inside the `update()` arms.
//!
//! See the per-function docstrings for which `SessionViewMsg` arm each helper
//! was extracted from.

use crate::components::message_renderer::types::ClaudeMessage;
use crate::components::message_renderer::RenderedMessage;
use crate::pages::dashboard::types::PendingPermission;
use codex_codes::io::items::{FileUpdateChange, ThreadItem};
use std::collections::HashSet;

/// Cross-agent activity classification used by the session-rail sparkline and
/// the pending-send reconciler. The same enum bridges Claude wire shapes
/// (`ClaudeOutput::Assistant` / `User` / etc.) and Codex `CodexEvent` shapes
/// — so a Codex agent reply lights up the rail in `assistant` color just like
/// a Claude assistant reply does, instead of falling through to `Unknown` and
/// rendering as a gray "other" smear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ActivityTag {
    /// Agent reply (Claude `assistant`, Codex `item.{started,updated,completed}`
    /// carrying agent / reasoning / tool-use items).
    Assistant,
    /// User input echo (Claude `user`).
    User,
    /// File-read style tool output. Uses the same green tick as Claude's
    /// user-shaped read tool-result envelope without participating in
    /// pending-send reconciliation as a real user echo.
    Read,
    /// End-of-turn result/summary (Claude `result`, Codex `turn.completed`).
    Result,
    /// Portal frame (connect/disconnect/reconnect notices, raw frame
    /// attachments). Protocol-agnostic.
    Portal,
    /// Error envelope or turn failure.
    Error,
    /// System-level message that doesn't fit elsewhere — renders as the
    /// neutral `tick-other` gray.
    System,
    /// Anthropic rate-limit event — neutral.
    RateLimit,
    /// Parse failure or completely unrecognized wire shape — neutral.
    Unknown,
    /// Bookkeeping frame with no user-visible content — no tick (keeps
    /// the rail readable). Distinct from `Unknown` which renders as gray
    /// `tick-other`; suppressed frames are invisible.
    Suppressed,
    /// Start of a compaction range (sparkline range marker).
    CompactionStart,
    /// End of a compaction range.
    CompactionEnd,
    /// Start of a sub-task range.
    TaskStart,
    /// End of a sub-task range.
    TaskEnd,
}

/// Enrich Codex FileChange permission requests with filenames resolved from
/// the already-streamed item events. The Codex approval request carries only
/// `itemId`, while the matching `item.started` / patch-updated frames carry
/// the human-readable paths and diffs.
pub(crate) fn enrich_codex_file_change_permission(
    mut perm: PendingPermission,
    messages: &[RenderedMessage],
) -> PendingPermission {
    let Ok(shared::CodexPermissionInput::FileChange {
        item_id,
        paths,
        reason,
        grant_root,
    }) = serde_json::from_value::<shared::CodexPermissionInput>(perm.input.clone())
    else {
        return perm;
    };

    if !paths.is_empty() {
        return perm;
    }

    let resolved_paths = codex_file_change_paths_for_item(messages, &item_id);
    if resolved_paths.is_empty() {
        return perm;
    }

    let enriched = shared::CodexPermissionInput::FileChange {
        item_id,
        paths: resolved_paths,
        reason,
        grant_root,
    };
    if let Ok(input) = serde_json::to_value(enriched) {
        perm.input = input;
    }
    perm
}

fn codex_file_change_paths_for_item(messages: &[RenderedMessage], item_id: &str) -> Vec<String> {
    use crate::components::codex_renderer::{CodexEvent, CodexItem};

    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for message in messages {
        let Ok(event) = serde_json::from_str::<CodexEvent>(&message.content) else {
            continue;
        };
        match event {
            CodexEvent::ItemStarted {
                item: Some(CodexItem::Thread(ThreadItem::FileChange(file_change))),
            }
            | CodexEvent::ItemUpdated {
                item: Some(CodexItem::Thread(ThreadItem::FileChange(file_change))),
            }
            | CodexEvent::ItemCompleted {
                item: Some(CodexItem::Thread(ThreadItem::FileChange(file_change))),
            } if file_change.id == item_id => {
                push_file_change_paths(&mut paths, &mut seen, &file_change.changes);
            }
            CodexEvent::FileChangePatchUpdated {
                params: Some(params),
            } if params.item_id.as_deref() == Some(item_id) => {
                if let Some(changes) = params.changes {
                    push_file_change_paths(&mut paths, &mut seen, &changes);
                }
            }
            _ => {}
        }
    }
    paths
}

fn push_file_change_paths(
    paths: &mut Vec<String>,
    seen: &mut HashSet<String>,
    changes: &[FileUpdateChange],
) {
    for change in changes {
        if seen.insert(change.path.clone()) {
            paths.push(change.path.clone());
        }
    }
}

impl ActivityTag {
    /// CSS class suffix for the sparkline tick — `format!("tick-{}", suffix)`
    /// matches `frontend/styles/session-rail.css:.sparkline-tick.tick-*`.
    /// Returns `None` for range markers (compaction / task), which are
    /// rendered as `.sparkline-range` rather than as point ticks.
    pub fn tick_css(self) -> Option<&'static str> {
        match self {
            Self::Assistant => Some("assistant"),
            Self::User | Self::Read => Some("user"),
            Self::Result => Some("result"),
            Self::Portal => Some("portal"),
            Self::Error => Some("error"),
            Self::System | Self::RateLimit | Self::Unknown => Some("other"),
            Self::Suppressed
            | Self::CompactionStart
            | Self::CompactionEnd
            | Self::TaskStart
            | Self::TaskEnd => None,
        }
    }

    /// Range markers don't render as ticks. Used by the sparkline tick-iteration
    /// to skip them in one pass.
    pub fn is_range_marker(self) -> bool {
        matches!(
            self,
            Self::CompactionStart | Self::CompactionEnd | Self::TaskStart | Self::TaskEnd
        )
    }

    /// Bookkeeping frames are suppressed — never rendered as ticks.
    pub fn is_suppressed(self) -> bool {
        matches!(self, Self::Suppressed)
    }

    pub fn is_compaction_start(self) -> bool {
        matches!(self, Self::CompactionStart)
    }
    pub fn is_compaction_end(self) -> bool {
        matches!(self, Self::CompactionEnd)
    }
    pub fn is_task_start(self) -> bool {
        matches!(self, Self::TaskStart)
    }
    pub fn is_task_end(self) -> bool {
        matches!(self, Self::TaskEnd)
    }
}

/// Wire `type` tag for a typed [`ClaudeMessage`] variant, expressed as an
/// [`ActivityTag`]. Returns `Unknown` only for the actual `Unknown` variant —
/// every other Claude shape maps to a real tag.
pub(super) fn message_type_tag(m: &ClaudeMessage) -> ActivityTag {
    match m {
        ClaudeMessage::System(_) => ActivityTag::System,
        ClaudeMessage::Assistant(_) => ActivityTag::Assistant,
        ClaudeMessage::Result(_) => ActivityTag::Result,
        ClaudeMessage::User(_) | ClaudeMessage::OptimisticUser(_) => ActivityTag::User,
        ClaudeMessage::Error(_) => ActivityTag::Error,
        ClaudeMessage::Portal(_) => ActivityTag::Portal,
        ClaudeMessage::RateLimitEvent(_) => ActivityTag::RateLimit,
        ClaudeMessage::ConversationReset(_) => ActivityTag::System,
        ClaudeMessage::LocalError(_) => ActivityTag::Error,
        ClaudeMessage::Unknown => ActivityTag::Unknown,
    }
}

/// Extract the user-text payload from a typed user message for pending-send
/// echo matching. Returns the top-level `content` string when present (used by
/// the frontend's optimistic-send synthesizer and the codex shim's synthesized
/// echo) and otherwise concatenates `ContentBlock::Text` blocks from
/// `message.content` (the shape Claude's `--replay-user-messages` emits).
pub(super) fn extract_user_text(m: &ClaudeMessage) -> Option<String> {
    let ClaudeMessage::User(u) = m else {
        if let ClaudeMessage::OptimisticUser(u) = m {
            return Some(u.content.clone());
        }
        return None;
    };
    let blocks = &u.message.content;
    let texts: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            shared::ContentBlock::Text(t) => Some(t.text.as_str()),
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
pub(super) fn autoscroll_transition(current: bool, new_at_bottom: bool) -> Option<bool> {
    if current == new_at_bottom {
        None
    } else {
        Some(new_at_bottom)
    }
}

/// Check if a session is awaiting user input by scanning messages
/// backwards. Uses `AgentFrameKind::is_terminator()` so every agent's
/// turn-end drives the same awaiting logic: Claude `Result`,
/// Codex `turn.completed`/`turn.failed`, Muse `run.terminal.completed`/
/// `run.terminal.failed`. The last meaningful signal determines the
/// pill color — a terminator means awaiting (orange), an assistant/user
/// or live item means working. Noise types are skipped.
pub(crate) fn is_awaiting(messages: impl DoubleEndedIterator<Item = impl AsRef<str>>) -> bool {
    use crate::components::agent_frame::AgentFrame;

    for msg in messages.rev() {
        let s = msg.as_ref();

        // Try Claude shape first.
        if let Ok(m) = ClaudeMessage::parse(s) {
            match m {
                ClaudeMessage::Result(_) => return true,
                ClaudeMessage::Assistant(_)
                | ClaudeMessage::User(_)
                | ClaudeMessage::OptimisticUser(_) => return false,
                _ => {}
            }
        }
        // Try Codex event.
        let codex = crate::components::codex_renderer::is_codex_terminal_event(s);

        if let Some(is_term) = codex {
            return is_term;
        }
        // Try Muse record.
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(s) {
            if val.get("type").and_then(|v| v.as_str()) == Some("muse_record") {
                let frame = AgentFrame::Muse(val);
                let kind = frame.kind();

                if kind.is_terminator() {
                    return true;
                }
                // Any other muse record is a working signal.
                return false;
            }
        }
    }
    false
}

/// Parse a server ISO timestamp to epoch-ms, treating a timezone-less string
/// as UTC.
///
/// The backend serializes message `created_at` as a *naive* datetime (e.g.
/// `2026-05-17T12:34:56.789`, no offset — see `backend/handlers/messages.rs`).
/// `js_sys::Date::parse` reads a date-time form without an offset as *local*
/// time, so on a browser west of UTC the event lands hours in the future
/// relative to `js_sys::Date::now()`. That pushed sparkline ticks far past the
/// 100% right edge (`left: ~8000%`) — invisibly off the pill. Appending `Z`
/// when the string carries no timezone pins it to UTC so it lines up with
/// `Date::now()`. Returns `NaN` when unparseable (callers check `is_finite`).
pub(super) fn parse_iso_ms_utc(iso: &str) -> f64 {
    js_sys::Date::parse(&shared::time::normalize_iso_utc(iso))
}

/// Derive the [`ActivityTag`] used by `on_activity` / `CheckAwaiting` from a
/// raw wire JSON string. Centralizes the parse-and-classify dance previously
/// duplicated between `LoadHistory` (REST replay) and `handle_received_output`
/// (live wire) so a classification change lands in one place.
///
/// Tries `shared::ClaudeOutput` first (the typed Claude wire shape, where
/// system messages disambiguate into the four sparkline range-marker tags),
/// then falls back to the local lenient `ClaudeMessage`. If both fail, tries
/// `CodexEvent` and finally `muse_record` shapes, mapping each into a
/// shared [`ActivityTag`] so Codex and Muse sessions get colored sparklines
/// with the same palette as Claude (assistant=blue, result=orange, error=red).
/// Returns [`ActivityTag::Unknown`] when nothing parses.
pub(super) fn classify_output_msg_type(output: &str) -> ActivityTag {
    if let Ok(claude_msg) = serde_json::from_str::<shared::ClaudeOutput>(output) {
        let mut tag = match claude_msg.message_type().as_str() {
            "assistant" => ActivityTag::Assistant,
            "user" => ActivityTag::User,
            "result" => ActivityTag::Result,
            "portal" => ActivityTag::Portal,
            "error" => ActivityTag::Error,
            "system" => ActivityTag::System,
            "rate_limit_event" => ActivityTag::RateLimit,
            _ => ActivityTag::Unknown,
        };
        if let shared::ClaudeOutput::System(sys) = &claude_msg {
            if let Some(status) = sys.as_status() {
                if status.status.as_ref().map(|s| s.as_str()) == Some("compacting") {
                    tag = ActivityTag::CompactionStart;
                }
            } else if shared::is_compaction_boundary(sys) {
                tag = ActivityTag::CompactionEnd;
            } else if sys.as_task_started().is_some() {
                tag = ActivityTag::TaskStart;
            } else if sys.as_task_notification().is_some() {
                tag = ActivityTag::TaskEnd;
            }
        }
        return tag;
    }
    if let Ok(parsed) = ClaudeMessage::parse(output) {
        let tag = message_type_tag(&parsed);
        if tag != ActivityTag::Unknown {
            return tag;
        }
    }
    if let Some(tag) = classify_codex_event(output) {
        return tag;
    }
    if let Some(tag) = classify_muse_event(output) {
        return tag;
    }
    ActivityTag::Unknown
}

/// Typed envelope for the frontend's `muse_record` wire frame.
/// The backend lifts `MuseRecord.payload_type` + `payload` into a `type:
/// "muse_record"` envelope (`muse-session-lib/src/classifier.rs::to_event`);
/// this mirrors that envelope without `serde_json::Value` pokes.
#[derive(serde::Deserialize)]
struct MuseActivityEnvelope {
    #[serde(rename = "type")]
    kind: String,
    payload_type: String,
    payload: serde_json::Value,
}

/// Map a Muse journal record to a cross-agent [`ActivityTag`] so the
/// sparkline lights up on Muse sessions the same way it does on Claude/Codex.
/// Muse emits ~100 `muse_record` JSON lines per turn; the reducer already
/// groups them into one task-tree card, but the sparkline still ticks per
/// record, so each record gets a color that reuses Claude's palette:
/// - `task.stream.linked` / `task.lifecycle.*` (proposed/accepted/scheduled/
///   started/status/output/side_effect_intent) → `Assistant` (blue, agent working)
/// - `task.lifecycle.*` terminal failed/rejected/cancelled → `Error` (red)
/// - `task.lifecycle.completed` → `Assistant` (work finished but not turn-end)
/// - `tool.result` success → `Assistant`, failure → `Error`
/// - `run.terminal.completed` (carries the final markdown answer) → `Result`
///   (orange, turn ended — same as Claude `result` / Codex `turn.completed`)
/// - `run.terminal.*` non-completed → `Error`
/// - other bookkeeping (`run.model.configured`, `command.received`, etc.)
///   → `Suppressed` (no tick, keeps the rail readable) — distinct from
///   returning `None` which would fall through to `Unknown` gray
fn classify_muse_event(output: &str) -> Option<ActivityTag> {
    let env = serde_json::from_str::<MuseActivityEnvelope>(output).ok()?;
    if env.kind != "muse_record" {
        return None;
    }
    let payload_type = env.payload_type;
    let typed = muse_codes::MusePayload::from_parts(&payload_type, env.payload).ok()?;
    match typed {
        muse_codes::MusePayload::TaskStreamLinked(_) => Some(ActivityTag::Assistant),
        muse_codes::MusePayload::TaskLifecycle(lc) => match lc.event {
            muse_codes::TaskLifecycleEvent::Failed { .. }
            | muse_codes::TaskLifecycleEvent::Rejected { .. }
            | muse_codes::TaskLifecycleEvent::Cancelled { .. } => Some(ActivityTag::Error),
            // `completed` is a task terminal but not a turn terminal — keep
            // it as assistant-blue so the rail doesn't strobe orange on
            // every tool-task; the turn's `run.terminal.completed` provides
            // the single orange tick.
            muse_codes::TaskLifecycleEvent::Completed { .. } => Some(ActivityTag::Assistant),
            _ => Some(ActivityTag::Assistant),
        },
        muse_codes::MusePayload::ToolResult(tr) => {
            // TODO(SDK #310): `ToolResult.correlation_facts` is an open-shaped
            // `serde_json::Value` — the SDK has no typed `outcome` accessor yet.
            // Narrow seam: only `.get("outcome")` is poked here; tracked in
            // https://github.com/meawoppl/rust-code-agent-sdks/issues/310.
            let outcome = tr
                .correlation_facts
                .as_ref()
                .and_then(|v| v.get("outcome"))
                .and_then(|o| o.as_str());
            match outcome {
                Some("failure") => Some(ActivityTag::Error),
                _ => Some(ActivityTag::Assistant),
            }
        }
        muse_codes::MusePayload::RunTerminal(rt) => {
            if rt.terminal == "completed" {
                Some(ActivityTag::Result)
            } else {
                Some(ActivityTag::Error)
            }
        }
        // Bookkeeping / streaming frames — no tick, distinct from "not a
        // muse_record" (which returns None and falls through to Unknown gray).
        muse_codes::MusePayload::ModelConfigured(_)
        | muse_codes::MusePayload::RunStarted(_)
        | muse_codes::MusePayload::CommandAccepted(_)
        | muse_codes::MusePayload::SessionRunLinked(_)
        | muse_codes::MusePayload::TurnInputUser(_)
        | muse_codes::MusePayload::RunOutputDelta(_)
        | muse_codes::MusePayload::Unknown { .. } => Some(ActivityTag::Suppressed),
    }
}

/// Map a Codex wire frame to a cross-agent [`ActivityTag`] so the sparkline
/// lights up on Codex sessions the same way it does on Claude. Returns `None`
/// for thread/turn-started signals and streaming deltas (those don't render
/// visible cards, so the sparkline stays clean) and for unparseable JSON.
fn classify_codex_event(output: &str) -> Option<ActivityTag> {
    use crate::components::codex_renderer::{CodexEvent, CodexItem};
    use codex_codes::io::items::ThreadItem;
    use codex_codes::protocol::ThreadItem as AppServerThreadItem;
    let event: CodexEvent = serde_json::from_str(output).ok()?;
    match event {
        CodexEvent::ItemStarted { item: Some(item) }
        | CodexEvent::ItemUpdated { item: Some(item) }
        | CodexEvent::ItemCompleted { item: Some(item) } => match item {
            CodexItem::AppServer(item) => match item.as_ref() {
                AppServerThreadItem::ContextCompaction { .. }
                | AppServerThreadItem::CollabAgentToolCall { .. } => Some(ActivityTag::Assistant),
                _ => None,
            },
            CodexItem::Thread(ThreadItem::Error(_)) => Some(ActivityTag::Error),
            CodexItem::Thread(ThreadItem::CommandExecution(ref it))
                if command_execution_reads_file(&it.command) =>
            {
                Some(ActivityTag::Read)
            }
            CodexItem::Thread(
                ThreadItem::AgentMessage(_)
                | ThreadItem::Reasoning(_)
                | ThreadItem::CommandExecution(_)
                | ThreadItem::FileChange(_)
                | ThreadItem::McpToolCall(_)
                | ThreadItem::WebSearch(_)
                | ThreadItem::TodoList(_)
                | ThreadItem::UserMessage(_),
            ) => Some(ActivityTag::Assistant),
        },
        CodexEvent::TurnCompleted { .. } | CodexEvent::TurnFailed { .. } => {
            Some(ActivityTag::Result)
        }
        CodexEvent::Error { .. } => Some(ActivityTag::Error),
        // `thread.started` / `turn.started` and the streaming deltas
        // (PlanDelta / ReasoningTextDelta / ReasoningSummaryPartAdded) and the
        // diff/plan/patch updates don't render visible per-event cards (the
        // consolidated content lands in `item.completed` / `turn/plan/updated`),
        // so emit no sparkline tick.
        _ => None,
    }
}

fn command_execution_reads_file(command: &str) -> bool {
    let command = command.trim();
    if command.is_empty() {
        return false;
    }

    let normalized = command.replace("\\\"", "\"");
    is_numbered_line_read(&normalized) || is_sed_print_read(&normalized)
}

fn is_numbered_line_read(command: &str) -> bool {
    command.contains("nl -ba ") && command.contains("| sed -n ")
}

fn is_sed_print_read(command: &str) -> bool {
    if command.contains("sed -i") || !command.contains("sed -n ") {
        return false;
    }

    command.contains('p')
}

/// Drain pending optimistic-send entries when the server confirms our input.
///
/// - [`ActivityTag::User`] echo: match by content (via [`extract_user_text`])
///   so a lost message doesn't consume an unrelated pending entry — only the
///   first matching pending entry is removed.
/// - [`ActivityTag::Assistant`] / [`ActivityTag::Result`]: agent is responding;
///   slash commands like `/cost`, `/status`, `/clear` don't produce a user
///   echo, so the assistant/result response is treated as the signal that
///   the input was received and clears *all* pending entries.
/// - Any other tag: no-op.
pub(super) fn reconcile_pending_sends(
    pending_sends: &mut Vec<RenderedMessage>,
    tag: ActivityTag,
    output: &str,
) {
    if pending_sends.is_empty() {
        return;
    }
    match tag {
        ActivityTag::User => {
            let echo_text = ClaudeMessage::parse(output)
                .ok()
                .as_ref()
                .and_then(extract_user_text);
            if let Some(ref echo) = echo_text {
                if let Some(pos) = pending_sends.iter().position(|pending| {
                    if pending_has_client_msg_id(pending) {
                        return false;
                    }
                    ClaudeMessage::parse(&pending.content)
                        .ok()
                        .as_ref()
                        .and_then(extract_user_text)
                        .as_ref()
                        == Some(echo)
                }) {
                    pending_sends.remove(pos);
                }
            }
        }
        ActivityTag::Assistant | ActivityTag::Result => {
            pending_sends.retain(pending_has_client_msg_id);
        }
        _ => {}
    }
}

pub(super) fn update_pending_send_delivery(
    pending_sends: &mut Vec<RenderedMessage>,
    client_msg_id: uuid::Uuid,
    stage: shared::InputDeliveryStage,
    message: Option<&str>,
) -> bool {
    let Some(pos) = pending_sends
        .iter()
        .position(|pending| pending_client_msg_id(pending) == Some(client_msg_id))
    else {
        return false;
    };

    if stage == shared::InputDeliveryStage::AgentAccepted {
        pending_sends.remove(pos);
        return true;
    }

    let Some(meta) = pending_sends[pos].meta.as_mut() else {
        return false;
    };
    let Some(delivery) = meta.delivery.as_mut() else {
        return false;
    };

    delivery.stage = Some(stage);
    delivery.message = message.map(ToOwned::to_owned);
    true
}

fn pending_has_client_msg_id(pending: &RenderedMessage) -> bool {
    pending_client_msg_id(pending).is_some()
}

fn pending_client_msg_id(pending: &RenderedMessage) -> Option<uuid::Uuid> {
    pending.delivery().map(|delivery| delivery.client_msg_id)
}

// --- Ephemeral tool-progress ("active tool" strip) ------------------------
//
// Live heartbeats for long-running tools arrive on the non-persisted
// `WsEvent::ToolProgress` side-channel (see `websocket.rs`) roughly every 30s.
// The view holds an ordered list of currently-running tools and renders a
// trailing status strip ("Bash running — 1m 30s"). We deliberately keep this
// OUT of the memoized message-render pipeline: folding a per-heartbeat-changing
// map into `MessageRenderer` props would re-render the whole transcript every
// 30s. A trailing strip re-renders only itself — the framework's grain.

/// One currently-running tool tracked for the live "active tool" strip.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ActiveToolProgress {
    /// Correlation key = the running tool's id (see [`running_tool_key`]).
    pub key: String,
    pub tool_name: String,
    pub elapsed_seconds: f64,
    /// The `Task` sub-agent behind this tool, when Claude reports one (#1474).
    /// Renders as a qualifier on the pill so a long `Task` says *which* agent
    /// is running.
    pub subagent_type: Option<String>,
    /// Retry state while the sub-agent is retrying after an error (#1474).
    /// Present only mid-retry, so a flaky sub-agent turn is legible instead of
    /// looking like an unexplained stall.
    pub subagent_retry: Option<shared::SubagentRetryStatus>,
}

/// Derive the correlation key for a heartbeat: the *running* tool's id.
///
/// Claude's `tool_progress` frame carries `tool_use_id` = `<base>-heartbeat-N`
/// and, in production, `parent_tool_use_id` = the base tool id. The base id is
/// what the eventual `tool_result` block carries, so keying on it lets us clear
/// the entry when the tool finishes. Prefer `parent_tool_use_id`; otherwise
/// strip the `-heartbeat-N` suffix from `tool_use_id` (older/edge wire shapes
/// where the parent is absent); fall back to `tool_use_id` verbatim.
pub(crate) fn running_tool_key(tool_use_id: &str, parent_tool_use_id: Option<&str>) -> String {
    if let Some(parent) = parent_tool_use_id.filter(|p| !p.is_empty()) {
        return parent.to_string();
    }
    match tool_use_id.rsplit_once("-heartbeat-") {
        Some((base, _)) if !base.is_empty() => base.to_string(),
        _ => tool_use_id.to_string(),
    }
}

/// Upsert a heartbeat into the ordered active-tool list: refresh an existing
/// entry in place (preserving display order) or append a new one.
/// Order-preserving so the strip doesn't reshuffle every 30s.
///
/// The two sub-agent fields (#1474) merge differently, on purpose:
///
/// - `subagent_type` is **sticky**. It identifies *which* `Task` agent is
///   running, which cannot change for a given tool id, so a later frame that
///   omits it keeps the known label rather than flickering it away.
/// - `subagent_retry` is **replaced every frame**, including with `None`.
///   It is transient state, and its absence is meaningful: it means the
///   sub-agent is no longer retrying, so the badge must clear.
pub(crate) fn upsert_tool_progress(
    list: &mut Vec<ActiveToolProgress>,
    incoming: ActiveToolProgress,
) {
    if let Some(existing) = list.iter_mut().find(|t| t.key == incoming.key) {
        existing.tool_name = incoming.tool_name;
        existing.elapsed_seconds = incoming.elapsed_seconds;
        if incoming.subagent_type.is_some() {
            existing.subagent_type = incoming.subagent_type;
        }
        existing.subagent_retry = incoming.subagent_retry;
    } else {
        list.push(incoming);
    }
}

/// Prune finished tools from the active-tool list given a freshly-arrived
/// output message. A turn terminator (`result`) means nothing is running, so
/// the whole list clears; otherwise any tool whose id appears as a
/// `tool_result` in the message is done and its entry is dropped. Returns
/// whether anything changed (so the caller can skip a re-render).
pub(crate) fn clear_completed_tools(list: &mut Vec<ActiveToolProgress>, content: &str) -> bool {
    if list.is_empty() {
        return false;
    }
    let Ok(output) = serde_json::from_str::<shared::ClaudeOutput>(content) else {
        return false;
    };
    match output {
        // Turn over → nothing is running anymore.
        shared::ClaudeOutput::Result(_) => {
            list.clear();
            true
        }
        shared::ClaudeOutput::User(user) => {
            let finished = tool_result_ids(&user.message.content);
            prune_keys(list, &finished)
        }
        shared::ClaudeOutput::Assistant(asst) => {
            let finished = tool_result_ids(&asst.message.content);
            prune_keys(list, &finished)
        }
        _ => false,
    }
}

fn tool_result_ids(blocks: &[shared::ContentBlock]) -> Vec<String> {
    blocks
        .iter()
        .filter_map(|b| match b {
            shared::ContentBlock::ToolResult(tr) => Some(tr.tool_use_id.clone()),
            _ => None,
        })
        .collect()
}

fn prune_keys(list: &mut Vec<ActiveToolProgress>, finished: &[String]) -> bool {
    if finished.is_empty() {
        return false;
    }
    let before = list.len();
    list.retain(|t| !finished.contains(&t.key));
    list.len() != before
}

/// Format an elapsed-seconds count as a compact human duration: `45s`,
/// `1m 30s`, `1h 05m` (seconds dropped past an hour to stay short).
pub(crate) fn format_tool_elapsed(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let hours = total / 3600;
    let minutes = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

/// Derive a one-line human summary from a neutral ephemeral live-status frame
/// (`WsEvent::Ephemeral`) for the transient status strip.
///
/// The payload is opaque wire JSON by design — the frontend can't depend on
/// the native `muse-codes` types — so this reads it defensively: streaming
/// text (`payload.text`) or a status message (`payload.event.message`) when
/// present, else the dotted `payload_type` as a fallback label so an unmodeled
/// frame still names itself instead of showing nothing. Returns `None` only
/// when the frame carries no usable signal at all.
///
/// Muse records bypass this summary and replay into their task tree; this is
/// the conservative fallback for other ephemeral producers.
pub(crate) fn ephemeral_summary(payload: &serde_json::Value) -> Option<String> {
    let inner = payload.get("payload");
    // Streaming output text (muse `run.output.delta`).
    if let Some(text) = inner
        .and_then(|p| p.get("text"))
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        return Some(text.to_string());
    }
    // Status message (muse `task.lifecycle.status`).
    if let Some(msg) = inner
        .and_then(|p| p.get("event"))
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .map(str::trim)
        .filter(|m| !m.is_empty())
    {
        return Some(msg.to_string());
    }
    // Fallback: name the frame by its type rather than showing nothing.
    payload
        .get("payload_type")
        .and_then(|t| t.as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Transient Muse records for the currently-running turn. Durable journal
/// records remain the source of truth; this buffer is only replayed over the
/// matching durable tree so status and streamed answer text update in place.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct MuseLiveTurn {
    pub(crate) causation_id: Option<String>,
    pub(crate) events: Vec<serde_json::Value>,
}

impl MuseLiveTurn {
    const MAX_EVENTS: usize = 256;

    pub(crate) fn push(&mut self, event: serde_json::Value) {
        let causation_id = event
            .get("causation_id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        if causation_id.is_some() && causation_id != self.causation_id {
            self.events.clear();
            self.causation_id = causation_id;
        }

        let payload_type = event.get("payload_type").and_then(|value| value.as_str());
        // Status is replacement state, not history. Keeping only the newest
        // line per task avoids a heartbeat-heavy turn growing without bound.
        if payload_type == Some("task.lifecycle.status") {
            let task_id = muse_task_id(&event);
            if let Some(existing) = self.events.iter_mut().rev().find(|candidate| {
                candidate
                    .get("payload_type")
                    .and_then(|value| value.as_str())
                    == Some("task.lifecycle.status")
                    && muse_task_id(candidate) == task_id
            }) {
                *existing = event;
                return;
            }
        }

        // Output deltas are one logical assistant response. Coalesce adjacent
        // chunks so rendering cost follows turns, not token count.
        if payload_type == Some("run.output.delta") {
            if let (Some(chunk), Some(previous)) = (
                event
                    .get("payload")
                    .and_then(|payload| payload.get("text"))
                    .and_then(|value| value.as_str()),
                self.events.last_mut(),
            ) {
                if previous
                    .get("payload_type")
                    .and_then(|value| value.as_str())
                    == Some("run.output.delta")
                {
                    if let Some(text) = previous
                        .get_mut("payload")
                        .and_then(|payload| payload.get_mut("text"))
                        .and_then(|value| value.as_str())
                    {
                        let mut joined = text.to_string();
                        joined.push_str(chunk);
                        previous["payload"]["text"] = serde_json::Value::String(joined);
                        return;
                    }
                }
            }
        }

        self.events.push(event);
        if self.events.len() > Self::MAX_EVENTS {
            self.events.remove(0);
        }
    }

    pub(crate) fn clear_if_terminal(&mut self, durable: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(durable) else {
            return;
        };
        let terminal = value
            .get("payload_type")
            .and_then(|value| value.as_str())
            .is_some_and(|kind| kind.starts_with("run.terminal."));
        let same_turn = value.get("causation_id").and_then(|value| value.as_str())
            == self.causation_id.as_deref();
        if terminal && same_turn {
            self.events.clear();
            self.causation_id = None;
        }
    }
}

fn muse_task_id(value: &serde_json::Value) -> Option<&str> {
    let payload = value.get("payload")?;
    payload
        .get("event")
        .and_then(|event| event.get("task_id"))
        .or_else(|| payload.get("task_id"))
        .and_then(|value| value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ephemeral_summary_prefers_streaming_text() {
        let frame = json!({
            "payload_type": "run.output.delta",
            "payload": {"text": "  hello world  "}
        });
        assert_eq!(ephemeral_summary(&frame).as_deref(), Some("hello world"));
    }

    #[test]
    fn ephemeral_summary_falls_back_to_status_message() {
        let frame = json!({
            "payload_type": "task.lifecycle.status",
            "payload": {"event": {"message": "opening stream"}}
        });
        assert_eq!(ephemeral_summary(&frame).as_deref(), Some("opening stream"));
    }

    #[test]
    fn ephemeral_summary_labels_unmodeled_frames_by_type() {
        // No text/message — an unmodeled ephemeral still names itself.
        let frame = json!({"payload_type": "subagent.progress.heartbeat", "payload": {}});
        assert_eq!(
            ephemeral_summary(&frame).as_deref(),
            Some("subagent.progress.heartbeat")
        );
    }

    #[test]
    fn ephemeral_summary_none_when_no_signal() {
        assert_eq!(ephemeral_summary(&json!({"payload": {}})), None);
    }

    #[test]
    fn muse_live_turn_coalesces_output_and_replaces_task_status() {
        let mut live = MuseLiveTurn::default();
        live.push(json!({
            "payload_type": "task.lifecycle.status",
            "causation_id": "turn-1",
            "payload": {"event": {"task_id": "task-1", "message": "starting"}}
        }));
        live.push(json!({
            "payload_type": "task.lifecycle.status",
            "causation_id": "turn-1",
            "payload": {"event": {"task_id": "task-1", "message": "running"}}
        }));
        live.push(json!({
            "payload_type": "run.output.delta",
            "causation_id": "turn-1",
            "payload": {"text": "hello "}
        }));
        live.push(json!({
            "payload_type": "run.output.delta",
            "causation_id": "turn-1",
            "payload": {"text": "world"}
        }));

        assert_eq!(live.events.len(), 2);
        assert_eq!(live.events[0]["payload"]["event"]["message"], "running");
        assert_eq!(live.events[1]["payload"]["text"], "hello world");
    }

    #[test]
    fn muse_live_turn_resets_on_new_causation_and_terminal() {
        let mut live = MuseLiveTurn::default();
        live.push(json!({
            "payload_type": "run.output.delta",
            "causation_id": "turn-1",
            "payload": {"text": "old"}
        }));
        live.push(json!({
            "payload_type": "run.output.delta",
            "causation_id": "turn-2",
            "payload": {"text": "new"}
        }));
        assert_eq!(live.events.len(), 1);
        assert_eq!(live.causation_id.as_deref(), Some("turn-2"));

        live.clear_if_terminal(
            &json!({
                "payload_type": "run.terminal.completed",
                "causation_id": "turn-2",
                "payload": {"text": "done"}
            })
            .to_string(),
        );
        assert!(live.events.is_empty());
        assert!(live.causation_id.is_none());
    }

    fn pending(content: &str) -> RenderedMessage {
        RenderedMessage::new(format!(r#"{{"type":"user","content":"{content}"}}"#), None)
    }

    fn tracked_pending(content: &str, id: uuid::Uuid) -> RenderedMessage {
        RenderedMessage::new(
            format!(r#"{{"type":"user","content":"{content}"}}"#),
            Some(shared::PortalMeta {
                created_at: None,
                source: None,
                delivery: Some(shared::DeliveryMeta {
                    client_msg_id: id,
                    stage: None,
                    message: None,
                }),
            }),
        )
    }

    // --- autoscroll_transition ---

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

    // --- ActivityTag ---

    #[test]
    fn activity_tag_tick_css_matches_existing_css_classes() {
        // The string suffixes here must match `.sparkline-tick.tick-*` rules
        // in `frontend/styles/session-rail.css`. If a rename happens, this
        // test pins both sides.
        assert_eq!(ActivityTag::Assistant.tick_css(), Some("assistant"));
        assert_eq!(ActivityTag::User.tick_css(), Some("user"));
        assert_eq!(ActivityTag::Read.tick_css(), Some("user"));
        assert_eq!(ActivityTag::Result.tick_css(), Some("result"));
        assert_eq!(ActivityTag::Portal.tick_css(), Some("portal"));
        assert_eq!(ActivityTag::Error.tick_css(), Some("error"));
        assert_eq!(ActivityTag::System.tick_css(), Some("other"));
        assert_eq!(ActivityTag::RateLimit.tick_css(), Some("other"));
        assert_eq!(ActivityTag::Unknown.tick_css(), Some("other"));
        assert_eq!(ActivityTag::CompactionStart.tick_css(), None);
        assert_eq!(ActivityTag::CompactionEnd.tick_css(), None);
        assert_eq!(ActivityTag::TaskStart.tick_css(), None);
        assert_eq!(ActivityTag::TaskEnd.tick_css(), None);
    }

    #[test]
    fn activity_tag_range_marker_predicates() {
        assert!(ActivityTag::CompactionStart.is_range_marker());
        assert!(ActivityTag::CompactionEnd.is_range_marker());
        assert!(ActivityTag::TaskStart.is_range_marker());
        assert!(ActivityTag::TaskEnd.is_range_marker());
        assert!(!ActivityTag::Assistant.is_range_marker());
        assert!(!ActivityTag::Read.is_range_marker());
        assert!(!ActivityTag::Unknown.is_range_marker());

        assert!(ActivityTag::CompactionStart.is_compaction_start());
        assert!(ActivityTag::CompactionEnd.is_compaction_end());
        assert!(ActivityTag::TaskStart.is_task_start());
        assert!(ActivityTag::TaskEnd.is_task_end());
    }

    #[test]
    fn enrich_codex_file_change_permission_resolves_paths_from_item_events() {
        let messages = vec![
            RenderedMessage::new(
                r#"{"type":"item.started","item":{"type":"fileChange","id":"fc1","changes":[{"path":"src/main.rs","kind":{"type":"update"},"diff":"@@ -1 +1 @@"},{"path":"src/lib.rs","kind":{"type":"add"},"diff":"new"}],"status":"inProgress"}}"#
                    .to_string(),
                None,
            ),
            RenderedMessage::new(
                r#"{"type":"item/fileChange/patchUpdated","params":{"itemId":"fc1","changes":[{"path":"src/main.rs","kind":{"type":"update"},"diff":"@@ -1 +1 @@"},{"path":"tests/app.rs","kind":{"type":"delete"},"diff":"gone"}]}}"#
                    .to_string(),
                None,
            ),
        ];
        let perm = PendingPermission {
            request_id: "rid-1".to_string(),
            tool_name: "FileChange".to_string(),
            input: serde_json::json!({
                "tool": "fileChange",
                "itemId": "fc1"
            }),
            permission_suggestions: vec![],
        };

        let enriched = enrich_codex_file_change_permission(perm, &messages);
        let parsed: shared::CodexPermissionInput = serde_json::from_value(enriched.input).unwrap();

        assert_eq!(
            parsed,
            shared::CodexPermissionInput::FileChange {
                item_id: "fc1".to_string(),
                paths: vec![
                    "src/main.rs".to_string(),
                    "src/lib.rs".to_string(),
                    "tests/app.rs".to_string()
                ],
                reason: None,
                grant_root: None,
            }
        );
    }

    // --- classify_output_msg_type ---

    #[test]
    fn classify_output_msg_type_returns_unknown_for_garbage() {
        assert_eq!(classify_output_msg_type("not-json"), ActivityTag::Unknown);
        assert_eq!(classify_output_msg_type(""), ActivityTag::Unknown);
    }

    #[test]
    fn classify_output_msg_type_recognizes_assistant_envelope() {
        let json = r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[]},"session_id":"01890000-0000-7000-8000-000000000001"}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Assistant);
    }

    #[test]
    fn classify_output_msg_type_recognizes_user_envelope() {
        let json = r#"{"type":"user","content":"hi"}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::User);
    }

    #[test]
    fn classify_output_msg_type_recognizes_portal_envelope() {
        // Portal frames aren't part of `shared::ClaudeOutput` — the first
        // parse fails and the classifier falls through to the local lenient
        // `ClaudeMessage::Portal` shape via `message_type_tag`.
        let json = r#"{"type":"portal","content":[{"type":"text","text":"hi"}]}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Portal);
    }

    #[test]
    fn classify_output_msg_type_recognizes_error_envelope() {
        let json = r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Error);
    }

    // --- classify_codex_event: regression target for "gray ticks on Codex" ---

    #[test]
    fn classify_codex_item_completed_agent_message_is_assistant() {
        let json =
            r#"{"type":"item.completed","item":{"type":"agent_message","id":"i1","text":"hi"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Assistant);
    }

    #[test]
    fn classify_codex_item_started_command_execution_is_assistant() {
        // Tool-use lifecycle events count as "agent working" for sparkline
        // purposes — same color as the agent's text reply.
        let json = r#"{"type":"item.started","item":{"type":"command_execution","id":"c1","command":"echo hi","status":"in_progress"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Assistant);
    }

    #[test]
    fn classify_codex_numbered_file_read_command_is_read() {
        let json = r#"{"type":"item.completed","item":{"type":"command_execution","id":"c1","command":"/bin/bash -lc \"nl -ba claude-session-lib/src/proxy_session/output_forwarder.rs | sed -n '45,82p'\"","aggregated_output":"45\tlet max_bytes = max_image_mb;","exit_code":0,"status":"completed"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Read);
        assert_eq!(classify_output_msg_type(json).tick_css(), Some("user"));
    }

    #[test]
    fn classify_codex_sed_print_file_read_command_is_read() {
        let json = r#"{"type":"item.completed","item":{"type":"command_execution","id":"c1","command":"sed -n '1,40p' frontend/src/pages/dashboard/session_view/helpers.rs","aggregated_output":"//! Pure helpers","exit_code":0,"status":"completed"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Read);
    }

    #[test]
    fn classify_codex_non_read_command_execution_stays_assistant() {
        let json = r#"{"type":"item.completed","item":{"type":"command_execution","id":"c1","command":"cargo test -p frontend","aggregated_output":"ok","exit_code":0,"status":"completed"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Assistant);
    }

    #[test]
    fn classify_codex_item_completed_file_change_is_assistant() {
        // FileChange must carry a real `status` (PatchApplyStatus) for the
        // typed `ThreadItem` to deserialize — upstream's struct is strict
        // here. Pre-#827 the local mirror tolerated a missing status.
        let json = r#"{"type":"item.completed","item":{"type":"file_change","id":"f1","changes":[],"status":"completed"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Assistant);
    }

    #[test]
    fn classify_codex_item_completed_error_is_error() {
        let json =
            r#"{"type":"item.completed","item":{"type":"error","id":"e1","message":"boom"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Error);
    }

    #[test]
    fn classify_codex_turn_completed_is_result() {
        // Turn-end summary mirrors Claude's `result` semantic (orange tick).
        let json = r#"{"type":"turn.completed","usage":{}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Result);
    }

    #[test]
    fn classify_codex_turn_failed_is_result() {
        let json = r#"{"type":"turn.failed","error":{"message":"oops"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Result);
    }

    #[test]
    fn classify_codex_error_event_is_error() {
        // Top-level `Error` event (not `item.completed{error}`).
        let json = r#"{"type":"error","message":"boom"}"#;
        // This matches BOTH the local `ClaudeMessage::Error` shape and the
        // typed `CodexEvent::Error` shape. The Claude path wins because it's
        // checked first and `ClaudeMessage::Error` is a recognized variant —
        // the result is still `Error`, just sourced from the Claude arm.
        assert_eq!(classify_output_msg_type(json), ActivityTag::Error);
    }

    #[test]
    fn classify_codex_streaming_deltas_are_unknown() {
        // Streaming deltas don't render visible cards, so they shouldn't
        // light up the sparkline either — they fall through to Unknown.
        let json = r#"{"type":"item/reasoning/textDelta","params":{"delta":"…"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Unknown);
        let json = r#"{"type":"item/plan/delta","params":{"delta":"…"}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Unknown);
        let json = r#"{"type":"thread.started","thread_id":"t1"}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Unknown);
        let json = r#"{"type":"turn.started"}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Unknown);
    }

    // --- classify_muse_event: Muse ticks reuse Claude palette ---

    #[test]
    fn classify_muse_task_lifecycle_started_is_assistant() {
        let json = r#"{"type":"muse_record","payload_type":"task.lifecycle.started","causation_id":"c1","payload":{"kind":"task_lifecycle","command_id":"c1","run_stream":{"kind":"run","id":"r1"},"task_id":"t1","task_stream":{"kind":"task","id":"t1"},"event":{"kind":"started","task_id":"t1"}}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Assistant);
        assert_eq!(classify_muse_event(json), Some(ActivityTag::Assistant));
    }

    #[test]
    fn classify_muse_task_stream_linked_is_assistant() {
        let json = r#"{"type":"muse_record","payload_type":"task.stream.linked","causation_id":"c1","payload":{"kind":"task_stream_linked","command_id":"c1","run_stream":{"kind":"run","id":"r1"},"task_id":"t1","task_stream":{"kind":"task","id":"t1"}}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Assistant);
    }

    #[test]
    fn classify_muse_tool_result_success_is_assistant_and_failure_is_error() {
        let ok = r#"{"type":"muse_record","payload_type":"tool.result","causation_id":"c1","payload":{"kind":"tool_result","command_id":"c1","run_stream":{"kind":"run","id":"r1"},"call_id":"c1","correlation_facts":{"tool_name":"bash","outcome":"success"},"text":"ok"}}"#;
        let fail = r#"{"type":"muse_record","payload_type":"tool.result","causation_id":"c1","payload":{"kind":"tool_result","command_id":"c1","run_stream":{"kind":"run","id":"r1"},"call_id":"c1","correlation_facts":{"tool_name":"bash","outcome":"failure"},"text":"boom"}}"#;
        assert_eq!(classify_output_msg_type(ok), ActivityTag::Assistant);
        assert_eq!(classify_output_msg_type(fail), ActivityTag::Error);
    }

    #[test]
    fn classify_muse_run_terminal_completed_is_result_and_failed_is_error() {
        let completed = r##"{"type":"muse_record","payload_type":"run.terminal.completed","causation_id":"c1","payload":{"kind":"run_terminal","command_id":"c1","run_stream":{"kind":"run","id":"r1"},"terminal":"completed","text":"Answer hello"}}"##;
        let failed = r#"{"type":"muse_record","payload_type":"run.terminal.failed","causation_id":"c1","payload":{"kind":"run_terminal","command_id":"c1","run_stream":{"kind":"run","id":"r1"},"terminal":"failed","reason":"oops"}}"#;
        assert_eq!(classify_output_msg_type(completed), ActivityTag::Result);
        assert_eq!(classify_muse_event(completed), Some(ActivityTag::Result));
        assert_eq!(classify_output_msg_type(failed), ActivityTag::Error);
    }

    #[test]
    fn classify_muse_run_terminal_authoritative_over_payload_type() {
        // payload_type says completed but payload.terminal says failed — the
        // typed `RunTerminal.terminal` field must win, not the envelope string.
        let mismatch = r#"{"type":"muse_record","payload_type":"run.terminal.completed","causation_id":"c1","payload":{"kind":"run_terminal","command_id":"c1","run_stream":{"kind":"run","id":"r1"},"terminal":"failed","reason":"oops"}}"#;
        assert_eq!(classify_muse_event(mismatch), Some(ActivityTag::Error));
        assert_eq!(classify_output_msg_type(mismatch), ActivityTag::Error);
        // Opposite: payload_type says failed but terminal says completed → Result.
        let mismatch2 = r#"{"type":"muse_record","payload_type":"run.terminal.failed","causation_id":"c1","payload":{"kind":"run_terminal","command_id":"c1","run_stream":{"kind":"run","id":"r1"},"terminal":"completed","text":"hi"}}"#;
        assert_eq!(classify_muse_event(mismatch2), Some(ActivityTag::Result));
        assert_eq!(classify_output_msg_type(mismatch2), ActivityTag::Result);
    }

    #[test]
    fn classify_muse_lifecycle_failed_is_error() {
        let json = r#"{"type":"muse_record","payload_type":"task.lifecycle.failed","causation_id":"c1","payload":{"kind":"task_lifecycle","command_id":"c1","run_stream":{"kind":"run","id":"r1"},"task_id":"t1","task_stream":{"kind":"task","id":"t1"},"event":{"kind":"failed","task_id":"t1","reason":"oops"}}}"#;
        assert_eq!(classify_output_msg_type(json), ActivityTag::Error);
    }

    #[test]
    fn classify_muse_bookkeeping_has_no_tick() {
        // run.model.configured / command.received are bookkeeping — suppressed
        // (no sparkline tick), distinct from Unknown gray.
        let json = r#"{"type":"muse_record","payload_type":"run.model.configured","causation_id":"c1","payload":{"kind":"model_configured","command_id":"c1","run_stream":{"kind":"run","id":"r1"},"model_id":"m1","display_label":"m1","profile_id":"p1","provider_id":"prov","source":"startup"}}"#;
        assert_eq!(classify_muse_event(json), Some(ActivityTag::Suppressed));
        assert_eq!(classify_output_msg_type(json), ActivityTag::Suppressed);
        assert_eq!(ActivityTag::Suppressed.tick_css(), None);
        assert!(ActivityTag::Suppressed.is_suppressed());
        assert!(!ActivityTag::Suppressed.is_range_marker());
    }

    // --- reconcile_pending_sends ---

    #[test]
    fn reconcile_pending_sends_noop_when_empty() {
        let mut pending: Vec<RenderedMessage> = vec![];
        reconcile_pending_sends(
            &mut pending,
            ActivityTag::User,
            r#"{"type":"user","content":"x"}"#,
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn reconcile_pending_sends_user_echo_removes_first_matching_entry() {
        let mut pending = vec![pending("hello"), pending("world")];
        reconcile_pending_sends(
            &mut pending,
            ActivityTag::User,
            r#"{"type":"user","content":"hello"}"#,
        );
        assert_eq!(pending.len(), 1);
        assert!(pending[0].content.contains("world"));
    }

    #[test]
    fn reconcile_pending_sends_user_echo_no_match_keeps_pending() {
        // A user echo for a message we didn't optimistically send must NOT
        // consume an unrelated pending entry — otherwise a multi-tab scenario
        // would drop legitimate pending sends.
        let mut pending = vec![pending("hello")];
        reconcile_pending_sends(
            &mut pending,
            ActivityTag::User,
            r#"{"type":"user","content":"unrelated"}"#,
        );
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn reconcile_pending_sends_assistant_clears_all() {
        // Slash commands (/cost, /clear, /status) don't echo as "user",
        // so the assistant response is the only signal we get for
        // pre-InputProgress pending rows. Id-tracked rows wait for
        // InputProgress::AgentAccepted.
        let mut pending = vec![pending("a"), pending("b")];
        reconcile_pending_sends(
            &mut pending,
            ActivityTag::Assistant,
            r#"{"type":"assistant","message":{"content":[]}}"#,
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn reconcile_pending_sends_preserves_id_tracked_rows() {
        let id = uuid::Uuid::new_v4();
        let mut pending = vec![tracked_pending("hello", id), pending("legacy")];

        reconcile_pending_sends(
            &mut pending,
            ActivityTag::User,
            r#"{"type":"user","content":"hello"}"#,
        );
        assert_eq!(pending.len(), 2, "user echo must not clear id-tracked row");

        reconcile_pending_sends(
            &mut pending,
            ActivityTag::Assistant,
            r#"{"type":"assistant","message":{"content":[]}}"#,
        );
        assert_eq!(pending.len(), 1, "assistant clears only legacy rows");
        assert_eq!(pending[0].delivery().map(|d| d.client_msg_id), Some(id));
    }

    #[test]
    fn update_pending_send_delivery_updates_stage() {
        let id = uuid::Uuid::new_v4();
        let mut pending = vec![tracked_pending("hello", id)];

        assert!(update_pending_send_delivery(
            &mut pending,
            id,
            shared::InputDeliveryStage::ServerReceived,
            None,
        ));
        let delivery = pending[0].delivery().expect("delivery");
        assert_eq!(
            delivery.stage,
            Some(shared::InputDeliveryStage::ServerReceived)
        );
        assert!(delivery.pending());
    }

    #[test]
    fn update_pending_send_delivery_failed_marks_not_pending() {
        let id = uuid::Uuid::new_v4();
        let mut pending = vec![tracked_pending("hello", id)];

        assert!(update_pending_send_delivery(
            &mut pending,
            id,
            shared::InputDeliveryStage::Failed,
            Some("permission denied"),
        ));
        let delivery = pending[0].delivery().expect("delivery");
        assert_eq!(delivery.stage, Some(shared::InputDeliveryStage::Failed));
        assert_eq!(delivery.message.as_deref(), Some("permission denied"));
        assert!(!delivery.pending());
    }

    #[test]
    fn update_pending_send_delivery_agent_accepted_removes_row() {
        let id = uuid::Uuid::new_v4();
        let mut pending = vec![tracked_pending("hello", id)];

        assert!(update_pending_send_delivery(
            &mut pending,
            id,
            shared::InputDeliveryStage::AgentAccepted,
            None,
        ));
        assert!(pending.is_empty());
    }

    #[test]
    fn reconcile_pending_sends_result_clears_all() {
        let mut pending = vec![pending("a")];
        reconcile_pending_sends(
            &mut pending,
            ActivityTag::Result,
            r#"{"type":"result","total_cost_usd":0.0}"#,
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn reconcile_pending_sends_ignores_other_tags() {
        let mut pending = vec![pending("a")];
        reconcile_pending_sends(&mut pending, ActivityTag::System, r#"{"type":"system"}"#);
        assert_eq!(pending.len(), 1);
    }

    // --- is_awaiting ---

    #[test]
    fn is_claude_awaiting_true_when_last_signal_is_result() {
        let msgs = [
            r#"{"type":"user","content":"q"}"#.to_string(),
            r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[]},"session_id":"01890000-0000-7000-8000-000000000001"}"#.to_string(),
            r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1,"duration_api_ms":1,"num_turns":1,"session_id":"01890000-0000-7000-8000-000000000001","total_cost_usd":0.0}"#.to_string(),
        ];
        assert!(is_awaiting(msgs.iter()));
    }

    #[test]
    fn is_claude_awaiting_false_when_last_signal_is_assistant() {
        let msgs = [
            r#"{"type":"user","content":"q"}"#.to_string(),
            r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[]},"session_id":"01890000-0000-7000-8000-000000000001"}"#.to_string(),
        ];
        assert!(!is_awaiting(msgs.iter()));
    }

    #[test]
    fn is_claude_awaiting_skips_noise_types_when_finding_last_signal() {
        // Portal / error / system messages don't gate awaiting — the last
        // result before any of those still counts.
        let msgs = [
            r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1,"duration_api_ms":1,"num_turns":1,"session_id":"01890000-0000-7000-8000-000000000001","total_cost_usd":0.0}"#.to_string(),
            r#"{"type":"portal","content":[{"type":"text","text":"x"}]}"#.to_string(),
            r#"{"type":"error","error":{"type":"api_error","message":"y"}}"#.to_string(),
        ];
        assert!(is_awaiting(msgs.iter()));
    }

    #[test]
    fn is_claude_awaiting_false_for_empty_history() {
        let msgs: Vec<String> = vec![];
        assert!(!is_awaiting(msgs.iter()));
    }

    #[test]
    fn is_awaiting_true_for_muse_terminal_completed() {
        let msgs = [
            r#"{"type":"muse_record","payload_type":"run.terminal.completed","payload":{"status":"completed"}}"#.to_string(),
        ];
        assert!(is_awaiting(msgs.iter()));
    }

    #[test]
    fn is_awaiting_true_for_muse_terminal_failed() {
        let msgs = [
            r#"{"type":"muse_record","payload_type":"run.terminal.failed","payload":{"status":"failed"}}"#.to_string(),
        ];
        assert!(is_awaiting(msgs.iter()));
    }

    #[test]
    fn is_awaiting_false_for_muse_working_record() {
        let msgs = [
            r#"{"type":"muse_record","payload_type":"task.started","payload":{"task_id":"1"}}"#
                .to_string(),
        ];
        assert!(!is_awaiting(msgs.iter()));
    }

    #[test]
    fn is_awaiting_true_for_codex_turn_completed() {
        let msgs = [
            r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}"#.to_string(),
        ];
        assert!(is_awaiting(msgs.iter()));
    }

    // --- extract_user_text ---

    #[test]
    fn extract_user_text_prefers_top_level_content() {
        let m: ClaudeMessage =
            serde_json::from_str(r#"{"type":"user","content":"hello"}"#).unwrap();
        assert_eq!(extract_user_text(&m).as_deref(), Some("hello"));
    }

    #[test]
    fn extract_user_text_falls_back_to_concatenated_text_blocks() {
        let m: ClaudeMessage = serde_json::from_str(
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"foo"},{"type":"text","text":"bar"}]},"session_id":"01890000-0000-7000-8000-000000000001"}"#,
        )
        .unwrap();
        assert_eq!(extract_user_text(&m).as_deref(), Some("foobar"));
    }

    #[test]
    fn extract_user_text_returns_none_for_non_user_variant() {
        let m: ClaudeMessage = serde_json::from_str(
            r#"{"type":"system","subtype":"init","session_id":"01890000-0000-7000-8000-000000000001"}"#,
        )
        .unwrap();
        assert_eq!(extract_user_text(&m), None);
    }

    #[test]
    fn extract_user_text_returns_none_when_no_text_blocks_and_no_top_level_content() {
        let m: ClaudeMessage =
            serde_json::from_str(r#"{"type":"user","message":{"role":"user","content":[]},"session_id":"01890000-0000-7000-8000-000000000001"}"#).unwrap();
        assert_eq!(extract_user_text(&m), None);
    }

    // --- message_type_tag ---

    #[test]
    fn message_type_tag_returns_expected_variant_for_each_claude_shape() {
        assert_eq!(
            message_type_tag(
                &serde_json::from_str::<ClaudeMessage>(
                    r#"{"type":"system","subtype":"init","session_id":"01890000-0000-7000-8000-000000000001"}"#
                )
                .unwrap()
            ),
            ActivityTag::System
        );
        assert_eq!(
            message_type_tag(
                &serde_json::from_str::<ClaudeMessage>(r#"{"type":"user","content":"x"}"#).unwrap()
            ),
            ActivityTag::User
        );
        assert_eq!(
            message_type_tag(
                &serde_json::from_str::<ClaudeMessage>(
                    r#"{"type":"error","error":{"type":"api_error","message":"x"}}"#
                )
                .unwrap()
            ),
            ActivityTag::Error
        );
    }

    // --- tool-progress ("active tool" strip) ---

    #[test]
    fn running_tool_key_prefers_parent_then_strips_heartbeat_suffix() {
        // Production shape: parent is the base tool id → key on it.
        assert_eq!(
            running_tool_key("toolu_01abc-heartbeat-3", Some("toolu_01abc")),
            "toolu_01abc"
        );
        // No parent → strip the -heartbeat-N suffix from tool_use_id.
        assert_eq!(
            running_tool_key("toolu_01abc-heartbeat-0", None),
            "toolu_01abc"
        );
        // Empty parent is treated as absent.
        assert_eq!(
            running_tool_key("toolu_01abc-heartbeat-0", Some("")),
            "toolu_01abc"
        );
        // No suffix and no parent → verbatim.
        assert_eq!(running_tool_key("toolu_01abc", None), "toolu_01abc");
    }

    /// A plain tool heartbeat (no sub-agent fields) — the common case.
    fn progress(key: &str, tool_name: &str, elapsed_seconds: f64) -> ActiveToolProgress {
        ActiveToolProgress {
            key: key.into(),
            tool_name: tool_name.into(),
            elapsed_seconds,
            subagent_type: None,
            subagent_retry: None,
        }
    }

    fn retry(attempt: u64, max_retries: u64) -> shared::SubagentRetryStatus {
        shared::SubagentRetryStatus {
            attempt,
            max_retries,
            error_category: "overloaded".into(),
        }
    }

    #[test]
    fn upsert_tool_progress_refreshes_in_place_preserving_order() {
        let mut list = Vec::new();
        upsert_tool_progress(&mut list, progress("a", "Bash", 30.0));
        upsert_tool_progress(&mut list, progress("b", "Read", 30.0));
        // Refresh "a": elapsed updates, order stays [a, b].
        upsert_tool_progress(&mut list, progress("a", "Bash", 60.0));
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].key, "a");
        assert_eq!(list[0].elapsed_seconds, 60.0);
        assert_eq!(list[1].key, "b");
    }

    #[test]
    fn upsert_keeps_known_subagent_type_when_a_later_frame_omits_it() {
        // `subagent_type` identifies which Task agent is running and cannot
        // change for a tool id, so a frame without it must not blank the label.
        let mut list = Vec::new();
        let mut first = progress("a", "Task", 30.0);
        first.subagent_type = Some("code-reviewer".into());
        upsert_tool_progress(&mut list, first);
        upsert_tool_progress(&mut list, progress("a", "Task", 60.0));
        assert_eq!(list[0].subagent_type.as_deref(), Some("code-reviewer"));
        assert_eq!(list[0].elapsed_seconds, 60.0);
    }

    #[test]
    fn upsert_clears_retry_when_the_next_frame_is_not_retrying() {
        // Retry state is transient: its ABSENCE means the sub-agent stopped
        // retrying, so the badge has to clear or it would stick forever.
        let mut list = Vec::new();
        let mut retrying = progress("a", "Task", 30.0);
        retrying.subagent_retry = Some(retry(2, 3));
        upsert_tool_progress(&mut list, retrying);
        assert!(list[0].subagent_retry.is_some());

        upsert_tool_progress(&mut list, progress("a", "Task", 60.0));
        assert!(
            list[0].subagent_retry.is_none(),
            "a non-retrying frame must clear the retry badge"
        );
    }

    #[test]
    fn upsert_advances_the_retry_attempt() {
        let mut list = Vec::new();
        let mut first = progress("a", "Task", 30.0);
        first.subagent_retry = Some(retry(1, 3));
        upsert_tool_progress(&mut list, first);
        let mut second = progress("a", "Task", 60.0);
        second.subagent_retry = Some(retry(2, 3));
        upsert_tool_progress(&mut list, second);
        assert_eq!(list[0].subagent_retry.as_ref().unwrap().attempt, 2);
    }

    #[test]
    fn clear_completed_tools_drops_matching_tool_result() {
        let mut list = vec![
            progress("toolu_01", "Bash", 90.0),
            progress("toolu_02", "Read", 30.0),
        ];
        let user_result = serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "toolu_01",
                    "content": "done",
                }]
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
        })
        .to_string();
        assert!(clear_completed_tools(&mut list, &user_result));
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].key, "toolu_02");
    }

    #[test]
    fn clear_completed_tools_clears_all_on_turn_result() {
        let mut list = vec![progress("toolu_01", "Bash", 90.0)];
        let result = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "duration_ms": 100,
            "duration_api_ms": 80,
            "num_turns": 1,
            "session_id": "01890000-0000-7000-8000-000000000001",
            "total_cost_usd": 0.0,
        })
        .to_string();
        assert!(clear_completed_tools(&mut list, &result));
        assert!(list.is_empty());
    }

    #[test]
    fn clear_completed_tools_is_noop_for_unrelated_message() {
        let mut list = vec![progress("toolu_01", "Bash", 90.0)];
        let assistant = serde_json::json!({
            "type": "assistant",
            "message": {
                "id": "msg_1",
                "role": "assistant",
                "model": "claude-sonnet-4-5",
                "content": [{"type": "text", "text": "still working"}],
            },
            "session_id": "01890000-0000-7000-8000-000000000001",
        })
        .to_string();
        assert!(!clear_completed_tools(&mut list, &assistant));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn format_tool_elapsed_shapes() {
        assert_eq!(format_tool_elapsed(0.0), "0s");
        assert_eq!(format_tool_elapsed(45.0), "45s");
        assert_eq!(format_tool_elapsed(90.0), "1m 30s");
        assert_eq!(format_tool_elapsed(3725.0), "1h 02m");
        // Negative guards to zero rather than panicking.
        assert_eq!(format_tool_elapsed(-5.0), "0s");
    }
}
