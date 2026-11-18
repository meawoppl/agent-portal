//! The per-message parse cache (`RenderedMessage::facts`): one parse per
//! frame, shared by clones, with the same answers the uncached helpers gave.

use std::rc::Rc;

use super::fixtures::{
    assistant_with_tool_use, codex_command_event, read_tool_result_user_message, rendered,
    result_message, thinking_tokens_message,
};

/// The cached budget flag must agree with the shared rule the backend and
/// history hydration still use, or live trimming would drift from replay.
#[test]
fn cached_budget_flag_matches_the_shared_rule() {
    let frames = [
        thinking_tokens_message(150),
        r#"{"type":"system","subtype":"init","session_id":"s"}"#.to_string(),
        assistant_with_tool_use("t1", "Read"),
        read_tool_result_user_message("t1"),
        result_message(),
        "not json".to_string(),
    ];
    for frame in frames {
        assert_eq!(
            rendered(frame.clone()).counts_toward_render_limit(shared::AgentType::Claude),
            shared::render_budget::counts_toward_render_limit(&frame),
            "{frame}"
        );
    }
}

/// A clone shares the cache, so grouping copies of a message never re-parse.
#[test]
fn clones_share_one_parse() {
    let message = rendered(thinking_tokens_message(150));
    let copy = message.clone();
    assert!(Rc::ptr_eq(
        &message.facts(shared::AgentType::Claude),
        &copy.facts(shared::AgentType::Claude),
    ));
}

/// The cache is keyed by agent type: a frame first parsed for one agent still
/// parses correctly for another, without the first answer leaking through.
#[test]
fn facts_follow_the_requested_agent_type() {
    let message = rendered(codex_command_event("item.completed", "cmd-1", "completed"));
    assert_eq!(
        message.facts(shared::AgentType::Claude).codex_item_id,
        None,
        "a Codex frame does not parse as Codex in a Claude session"
    );
    assert_eq!(
        message
            .facts(shared::AgentType::Codex)
            .codex_item_id
            .as_deref(),
        Some("cmd-1")
    );
}
