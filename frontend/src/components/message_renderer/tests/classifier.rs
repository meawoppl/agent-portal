//! Cross-family classifier contracts: turn-terminator detection and the
//! one-row-per-realistic-message-kind sweep that guards every category.

use super::super::grouping::{classify, group_is_turn_terminator, GroupCategory, MessageGroup};
use super::fixtures::{
    assistant_with_tool_use, codex_item_started_agent_message, plain_user_text,
    portal_text_message, read_tool_result_user_message, rendered, result_message,
    thinking_tokens_message,
};

#[test]
fn turn_terminator_detection_covers_claude_and_codex() {
    let claude_result = result_message();
    let codex_completed = serde_json::json!({
        "type": "turn.completed",
        "usage": {"input_tokens": 1, "output_tokens": 2},
    })
    .to_string();
    let codex_failed = serde_json::json!({
        "type": "turn.failed",
        "error": {"message": "nope"},
    })
    .to_string();

    for json in [claude_result, codex_completed, codex_failed] {
        assert!(
            group_is_turn_terminator(&MessageGroup::Single(rendered(json))),
            "single terminator frame should be recognized"
        );
    }
    assert!(!group_is_turn_terminator(&MessageGroup::Single(rendered(
        plain_user_text("hello")
    ))));
    assert!(!group_is_turn_terminator(&MessageGroup::IdentityGroup {
        category: GroupCategory::User,
        label: "You".to_string(),
        badge_class: "user".to_string(),
        messages: vec![rendered(plain_user_text("hello"))],
    }));
}

/// One canonical wire shape per realistic message kind paired with the
/// `GroupCategory` the classifier MUST return on a Codex session. The
/// Codex agent type is the strictly-larger surface (Claude shapes
/// classify identically on both agent types, and Codex events only
/// classify on a Codex session), so a single Codex-agent sweep covers
/// the whole table.
///
/// If a new variant lands in `ClaudeMessage` or `CodexEvent`, extend
/// this table — the classifier is the only place that needs to know
/// about the new variant.
#[test]
fn classifier_exhaustive_over_realistic_messages() {
    let cases: Vec<(&str, String, Option<GroupCategory>)> = vec![
        (
            "assistant tool_use",
            assistant_with_tool_use("toolu_a", "Read"),
            Some(GroupCategory::Assistant),
        ),
        (
            "user tool_result envelope",
            read_tool_result_user_message("toolu_a"),
            Some(GroupCategory::Assistant),
        ),
        (
            "plain-text user prompt",
            plain_user_text("hello"),
            Some(GroupCategory::User),
        ),
        (
            "portal frame",
            portal_text_message("reconnected"),
            Some(GroupCategory::Portal),
        ),
        (
            "codex item.started",
            codex_item_started_agent_message("starting"),
            Some(GroupCategory::Codex),
        ),
        (
            "system message",
            serde_json::json!({
                "type": "system",
                "subtype": "init",
                "session_id": "01890000-0000-7000-8000-000000000001",
            })
            .to_string(),
            None,
        ),
        (
            "system thinking_tokens marker collapses into the Thinking group",
            thinking_tokens_message(150),
            Some(GroupCategory::Thinking),
        ),
        ("result message", result_message(), None),
        (
            "error message: on Codex agent the `{type: error}` shape \
             also matches `CodexEvent::Error` and lands in the Codex \
             group, preserved from the pre-refactor classifier",
            serde_json::json!({
                "type": "error",
                "message": "oops",
            })
            .to_string(),
            Some(GroupCategory::Codex),
        ),
        ("unparseable garbage", "not even json".to_string(), None),
    ];

    for (label, json, expected) in cases {
        let got = classify(&rendered(json), shared::AgentType::Codex, None).map(|i| i.category);
        assert_eq!(
            got, expected,
            "{label}: classifier returned {got:?}, expected {expected:?}"
        );
    }
}
