use codex_codes::{io::items::ThreadItem, protocol::ThreadItem as AppServerThreadItem};
use uuid::Uuid;
use yew::prelude::*;

mod events;
mod lifecycle;
mod messages;
mod patch;
#[cfg(test)]
mod tests;
mod tool_calls;
mod tool_card;
mod turns;
pub use events::{codex_event_item_id, is_codex_terminal_event, CodexEvent, CodexItem};
use lifecycle::{render_context_compacted, render_context_compaction_item, render_turn_plan};
use messages::{
    render_agent_message, render_agent_message_content, render_error_block, render_reasoning,
};
use patch::{render_file_change, render_file_change_patch};
use tool_calls::{
    render_collab_agent_tool_call, render_command_execution, render_mcp_tool_call,
    render_todo_list, render_web_search,
};
use turns::{render_turn_completed, render_turn_failed};

/// Render a parsed Codex frame as a standalone message card. Events that carry
/// no visible body render as an empty node.
pub fn render_codex_frame(
    event: &CodexEvent,
    session_id: Uuid,
    turn_metrics: Option<&shared::TurnMetrics>,
) -> Html {
    render_codex_event(event, session_id, false, turn_metrics).unwrap_or_default()
}

/// Render the content-only body for a parsed Codex frame inside an
/// `IdentityGroup`. Returns `None` when the event has no visible body, so the
/// caller can omit the `grouped-message-part` wrapper rather than emit an
/// empty, flex-gap-spaced blank row (mirrors the Claude group-part renderers).
pub fn render_codex_frame_content(event: &CodexEvent, session_id: Uuid) -> Option<Html> {
    render_codex_event(event, session_id, true, None)
}

/// Single dispatcher over parsed `CodexEvent` for both the standalone card
/// path and the grouped content path. `bare_agent_message` selects the grouped
/// behavior: agent messages render content-only (no card chrome), while other
/// events render as they would standalone.
///
/// Returns `None` for events with no visible body (turn lifecycle markers,
/// streaming deltas, the user-prompt echo) so both paths can drop them cleanly.
fn render_codex_event(
    event: &CodexEvent,
    session_id: Uuid,
    bare_agent_message: bool,
    turn_metrics: Option<&shared::TurnMetrics>,
) -> Option<Html> {
    match event {
        CodexEvent::ThreadStarted { .. } => None,
        CodexEvent::TurnStarted {} => None,
        CodexEvent::TurnCompleted {
            usage,
            duration_ms,
            turn_id,
            status,
        } => Some(render_turn_completed(
            usage.as_ref(),
            *duration_ms,
            turn_id.as_deref(),
            status.as_deref(),
            turn_metrics,
        )),
        CodexEvent::TurnFailed { error } => Some(render_turn_failed(error.as_ref(), turn_metrics)),
        CodexEvent::ItemStarted { item } | CodexEvent::ItemUpdated { item } => {
            match item.as_ref() {
                Some(CodexItem::Thread(ThreadItem::AgentMessage(it))) if bare_agent_message => {
                    Some(render_agent_message_content(&it.text, session_id))
                }
                item => render_item(item, false, session_id),
            }
        }
        CodexEvent::ItemCompleted { item } => match item.as_ref() {
            Some(CodexItem::Thread(ThreadItem::AgentMessage(it))) if bare_agent_message => {
                Some(render_agent_message_content(&it.text, session_id))
            }
            item => render_item(item, true, session_id),
        },
        CodexEvent::Error { message } => Some(render_error_block(message.as_deref())),
        CodexEvent::FileChangePatchUpdated { params } => Some(render_file_change_patch(
            params.as_ref().and_then(|p| p.changes.as_deref()),
        )),
        CodexEvent::TurnPlanUpdated { params } => Some(render_turn_plan(
            params.as_ref().and_then(|p| p.plan.as_deref()),
            params.as_ref().and_then(|p| p.explanation.as_deref()),
        )),
        CodexEvent::ThreadCompacted { params } => Some(render_context_compacted(params.as_ref())),
        // Cumulative whole-turn diffs (`turn/diff/updated`) are dropped: Codex
        // re-sends the entire turn diff on every edit tick, so they pile up
        // O(ticks) redundant cards (each the size of the whole turn) on top of
        // the per-file `item.completed{file_change}` diffs that already render
        // the same edits. Dropped before grouping — see
        // `grouping::group_messages` — so they never reach this renderer in
        // practice; the no-op arm is kept for match exhaustiveness.
        CodexEvent::TurnDiffUpdated { .. }
        // Per-chunk deltas — the consolidated content lands in `turn/plan/updated`
        // (for plans) or `item.completed` (for reasoning). Emit nothing for the
        // streaming chunks to avoid visual noise without losing information.
        | CodexEvent::PlanDelta { .. }
        | CodexEvent::ReasoningSummaryPartAdded { .. }
        | CodexEvent::ReasoningTextDelta { .. }
        | CodexEvent::Unknown => None,
    }
}

/// CSS class set for any item-card wrapper. Adds `codex-item-in-progress` for
/// pre-completion (`item.started` / `item.updated`) renders so the stylesheet
/// can pulse the indicator and dim the text.
fn item_card_classes(completed: bool) -> &'static str {
    if completed {
        "claude-message assistant-message codex-item"
    } else {
        "claude-message assistant-message codex-item codex-item-in-progress"
    }
}

fn render_item(item: Option<&CodexItem>, completed: bool, session_id: Uuid) -> Option<Html> {
    let item = item?;
    match item {
        CodexItem::Thread(item) => match item {
            ThreadItem::AgentMessage(it) => {
                Some(render_agent_message(&it.text, completed, session_id))
            }
            ThreadItem::Reasoning(it) => Some(render_reasoning(&it.text, completed)),
            ThreadItem::CommandExecution(it) => Some(render_command_execution(it, completed)),
            ThreadItem::FileChange(it) => Some(render_file_change(it, completed)),
            ThreadItem::McpToolCall(it) => Some(render_mcp_tool_call(it, completed)),
            ThreadItem::WebSearch(it) => Some(render_web_search(&it.query, completed)),
            ThreadItem::TodoList(it) => Some(render_todo_list(&it.items, completed)),
            ThreadItem::Error(it) => Some(render_error_block(Some(&it.message))),
            // UserMessage is the user's prompt for the turn — emitted by the
            // app-server protocol as the first item; the portal renders the
            // user-typed prompt out-of-band (Claude wire shape), so suppress
            // here to avoid duplication.
            ThreadItem::UserMessage(_) => None,
        },
        CodexItem::AppServer(item) => match item.as_ref() {
            AppServerThreadItem::ContextCompaction { .. } => {
                Some(render_context_compaction_item(completed))
            }
            AppServerThreadItem::CollabAgentToolCall {
                agents_states,
                model,
                prompt,
                reasoning_effort,
                status,
                tool,
                ..
            } => Some(render_collab_agent_tool_call(
                tool,
                model.as_deref(),
                reasoning_effort.as_ref(),
                status,
                prompt.as_deref(),
                agents_states,
                completed,
            )),
            _ => None,
        },
    }
}
