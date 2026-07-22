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
            Self::CompactionStart | Self::CompactionEnd | Self::TaskStart | Self::TaskEnd => None,
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

/// Check if a Claude session is awaiting user input by scanning messages
/// backwards. Skips noise types (portal, error, system, rate_limit_event)
/// and returns true if `Result` is found before `User` or `Assistant`.
pub(super) fn is_claude_awaiting(
    messages: impl DoubleEndedIterator<Item = impl AsRef<str>>,
) -> bool {
    messages
        .rev()
        .find_map(|msg| {
            ClaudeMessage::parse(msg.as_ref())
                .ok()
                .filter(|m| {
                    matches!(
                        m,
                        ClaudeMessage::Result(_)
                            | ClaudeMessage::Assistant(_)
                            | ClaudeMessage::User(_)
                            | ClaudeMessage::OptimisticUser(_)
                    )
                })
                .map(|m| message_type_tag(&m))
        })
        .is_some_and(|t| t == ActivityTag::Result)
}

/// Derive the [`ActivityTag`] used by `on_activity` / `CheckAwaiting` from a
/// raw wire JSON string. Centralizes the parse-and-classify dance previously
/// duplicated between `LoadHistory` (REST replay) and `handle_received_output`
/// (live wire) so a classification change lands in one place.
///
/// Tries `shared::ClaudeOutput` first (the typed Claude wire shape, where
/// system messages disambiguate into the four sparkline range-marker tags),
/// then falls back to the local lenient `ClaudeMessage`. If both fail and
/// the wire shape parses as a `CodexEvent`, maps the Codex variant into a
/// shared [`ActivityTag`] so Codex sessions get a colored sparkline (#TBD).
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
    classify_codex_event(output).unwrap_or(ActivityTag::Unknown)
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

/// Upsert a heartbeat into the ordered active-tool list: refresh the elapsed
/// time of an existing entry in place (preserving display order) or append a
/// new one. Order-preserving so the strip doesn't reshuffle every 30s.
pub(crate) fn upsert_tool_progress(
    list: &mut Vec<ActiveToolProgress>,
    key: String,
    tool_name: String,
    elapsed_seconds: f64,
) {
    if let Some(existing) = list.iter_mut().find(|t| t.key == key) {
        existing.tool_name = tool_name;
        existing.elapsed_seconds = elapsed_seconds;
    } else {
        list.push(ActiveToolProgress {
            key,
            tool_name,
            elapsed_seconds,
        });
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

#[cfg(test)]
mod tests {
    use super::*;

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

    // --- is_claude_awaiting ---

    #[test]
    fn is_claude_awaiting_true_when_last_signal_is_result() {
        let msgs = [
            r#"{"type":"user","content":"q"}"#.to_string(),
            r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[]},"session_id":"01890000-0000-7000-8000-000000000001"}"#.to_string(),
            r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1,"duration_api_ms":1,"num_turns":1,"session_id":"01890000-0000-7000-8000-000000000001","total_cost_usd":0.0}"#.to_string(),
        ];
        assert!(is_claude_awaiting(msgs.iter()));
    }

    #[test]
    fn is_claude_awaiting_false_when_last_signal_is_assistant() {
        let msgs = [
            r#"{"type":"user","content":"q"}"#.to_string(),
            r#"{"type":"assistant","message":{"id":"msg_1","role":"assistant","model":"claude-sonnet-4-5-20250929","content":[]},"session_id":"01890000-0000-7000-8000-000000000001"}"#.to_string(),
        ];
        assert!(!is_claude_awaiting(msgs.iter()));
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
        assert!(is_claude_awaiting(msgs.iter()));
    }

    #[test]
    fn is_claude_awaiting_false_for_empty_history() {
        let msgs: Vec<String> = vec![];
        assert!(!is_claude_awaiting(msgs.iter()));
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

    #[test]
    fn upsert_tool_progress_refreshes_in_place_preserving_order() {
        let mut list = Vec::new();
        upsert_tool_progress(&mut list, "a".into(), "Bash".into(), 30.0);
        upsert_tool_progress(&mut list, "b".into(), "Read".into(), 30.0);
        // Refresh "a": elapsed updates, order stays [a, b].
        upsert_tool_progress(&mut list, "a".into(), "Bash".into(), 60.0);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].key, "a");
        assert_eq!(list[0].elapsed_seconds, 60.0);
        assert_eq!(list[1].key, "b");
    }

    #[test]
    fn clear_completed_tools_drops_matching_tool_result() {
        let mut list = vec![
            ActiveToolProgress {
                key: "toolu_01".into(),
                tool_name: "Bash".into(),
                elapsed_seconds: 90.0,
            },
            ActiveToolProgress {
                key: "toolu_02".into(),
                tool_name: "Read".into(),
                elapsed_seconds: 30.0,
            },
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
        let mut list = vec![ActiveToolProgress {
            key: "toolu_01".into(),
            tool_name: "Bash".into(),
            elapsed_seconds: 90.0,
        }];
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
        let mut list = vec![ActiveToolProgress {
            key: "toolu_01".into(),
            tool_name: "Bash".into(),
            elapsed_seconds: 90.0,
        }];
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
