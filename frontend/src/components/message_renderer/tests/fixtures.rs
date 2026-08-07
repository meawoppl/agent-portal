//! Canonical wire shapes and grouping helpers shared by the per-family
//! classifier/grouping tests.

use super::super::grouping::{classify, group_messages, GroupCategory, MessageGroup};
use super::super::RenderedMessage;
use uuid::Uuid;

pub(super) fn rendered(json: impl Into<String>) -> RenderedMessage {
    RenderedMessage::new(json.into(), None)
}

pub(super) fn rendered_vec(messages: &[String]) -> Vec<RenderedMessage> {
    messages.iter().cloned().map(rendered).collect()
}

pub(super) fn classify_category(json: &str) -> Option<GroupCategory> {
    classify(&rendered(json), shared::AgentType::Claude, None).map(|identity| identity.category)
}

pub(super) fn classify_codex_category(json: &str) -> Option<GroupCategory> {
    classify(&rendered(json), shared::AgentType::Codex, None).map(|identity| identity.category)
}

pub(super) fn group_for_tests(messages: &[String]) -> Vec<MessageGroup> {
    group_messages(&rendered_vec(messages), shared::AgentType::Claude, None)
}

pub(super) fn group_for_codex_tests(messages: &[String]) -> Vec<MessageGroup> {
    group_messages(&rendered_vec(messages), shared::AgentType::Codex, None)
}

pub(super) fn group_for_muse_tests(messages: &[String]) -> Vec<MessageGroup> {
    group_messages(&rendered_vec(messages), shared::AgentType::Muse, None)
}

/// A `system`/`thinking_tokens` marker — the bodyless per-reasoning-step
/// event the Claude CLI emits, which the portal collapses into one chip.
/// `estimated_tokens` is the cumulative running thinking-token estimate.
pub(super) fn thinking_tokens_message(estimated_tokens: i64) -> String {
    serde_json::json!({
        "type": "system",
        "subtype": "thinking_tokens",
        "estimated_tokens": estimated_tokens,
        "estimated_tokens_delta": estimated_tokens,
        "session_id": "01890000-0000-7000-8000-000000000001",
        "uuid": format!("01890000-0000-7000-8000-{estimated_tokens:012}"),
    })
    .to_string()
}

/// Realistic Claude wire shape for a user message containing a single
/// `tool_result` content block (the kind Read / Bash / Edit etc. produce).
/// Matches `claude-codes` 2.1.x `ClaudeOutput::User(UserMessage)`
/// serialization with portal metadata carried out-of-band in `PortalMeta`.
pub(super) fn read_tool_result_user_message(tool_use_id: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": "file contents...",
            }]
        },
        "session_id": "01890000-0000-7000-8000-000000000001",
    })
    .to_string()
}

/// Realistic Claude assistant message with a single `tool_use` block.
pub(super) fn assistant_with_tool_use(tool_use_id: &str, tool_name: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "message": {
            "id": format!("msg_{tool_use_id}"),
            "role": "assistant",
            "model": "claude-sonnet-4-5-20250929",
            "content": [{
                "type": "tool_use",
                "id": tool_use_id,
                "name": tool_name,
                "input": {"file_path": "/some/path"},
            }]
        },
        "session_id": "01890000-0000-7000-8000-000000000001",
    })
    .to_string()
}

/// A plain-text user message (the Claude echo shape) carrying `text`.
pub(super) fn user_text_message(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
        "session_id": "01890000-0000-7000-8000-000000000001",
    })
    .to_string()
}

/// PR 2/4 of #758: portal messages must classify together.
pub(super) fn portal_text_message(text: &str) -> String {
    serde_json::json!({
        "type": "portal",
        "content": [{"type": "text", "text": text}],
    })
    .to_string()
}

pub(super) fn plain_user_text(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        },
        "session_id": "01890000-0000-7000-8000-000000000001",
    })
    .to_string()
}

pub(super) fn plain_user_text_from_sender(
    text: &str,
    user_id: Uuid,
    name: &str,
) -> RenderedMessage {
    let content = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}]
        },
        "session_id": "01890000-0000-7000-8000-000000000001",
    })
    .to_string();
    RenderedMessage::new(
        content,
        Some(shared::PortalMeta {
            created_at: Some("2026-05-17T10:00:00.000Z".to_string()),
            source: Some(shared::MessageSource::Human {
                account_id: user_id,
                name: name.to_string(),
            }),
            delivery: None,
        }),
    )
}

pub(super) fn tool_result_from_sender(
    tool_use_id: &str,
    user_id: Uuid,
    name: &str,
) -> RenderedMessage {
    RenderedMessage::new(
        read_tool_result_user_message(tool_use_id),
        Some(shared::PortalMeta {
            created_at: Some("2026-05-17T10:00:00.000Z".to_string()),
            source: Some(shared::MessageSource::Human {
                account_id: user_id,
                name: name.to_string(),
            }),
            delivery: None,
        }),
    )
}

pub(super) fn codex_item_started_agent_message(text: &str) -> String {
    serde_json::json!({
        "type": "item.started",
        "item": {
            "type": "agent_message",
            "id": "item_abc",
            "text": text,
        },
    })
    .to_string()
}

/// Lifecycle helper: a CommandExecution event at a given lifecycle stage.
/// `stage` is one of `"item.started"` / `"item.updated"` / `"item.completed"`.
/// All three carry the same `item_id`, mirroring the Codex wire flow that
/// produced the duplicate-card regression of #776.
///
/// `status` must be a real `CommandExecutionStatus` value (`in_progress`,
/// `completed`, `failed`, `declined`) — upstream `codex-codes` types
/// are strict here, the pre-#827 local mirror was looser (any string).
pub(super) fn codex_command_event(stage: &str, item_id: &str, status: &str) -> String {
    serde_json::json!({
        "type": stage,
        "item": {
            "type": "command_execution",
            "id": item_id,
            "command": "echo hello",
            "status": status,
        },
    })
    .to_string()
}

/// A durable muse journal record exactly as the classifier persists it —
/// the shape that live testing showed raining into the transcript as
/// "Unrecognized Message" bubbles (one per record, ~100 per turn).
pub(super) fn muse_lifecycle_message(kind: &str, task_id: &str) -> String {
    serde_json::json!({
        "type": "muse_record",
        "payload_type": format!("task.lifecycle.{kind}"),
        "stream_id": "9e369f06-72b8-423e-8c43-8051424922d9",
        "record_id": "018f0000-0000-7000-8000-00000000c3c7",
        "causation_id": "d9f89d76-d145-4155-a672-0c074ea940ac",
        "sequence": 62,
        "durability": "durable",
        "record_type": "event",
        "recorded_at": 1780531400000119u64,
        "payload": {
            "kind": "task_lifecycle",
            "task_id": task_id,
            "event": {"kind": kind, "task_id": task_id},
        },
    })
    .to_string()
}

/// The Claude `result` frame — the turn terminator that resets the thinking
/// odometer and ends a turn for the metrics/grouping paths.
pub(super) fn result_message() -> String {
    serde_json::json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "duration_ms": 100,
        "duration_api_ms": 80,
        "num_turns": 1,
        "session_id": "01890000-0000-7000-8000-000000000001",
        "total_cost_usd": 0.0,
    })
    .to_string()
}
