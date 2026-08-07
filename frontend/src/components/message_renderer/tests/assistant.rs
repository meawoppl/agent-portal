//! Assistant-family classification: `tool_use` frames and the `tool_result`
//! user envelopes that answer them must collapse into one assistant run.

use super::super::grouping::{classify, group_messages, GroupCategory, MessageGroup};
use super::fixtures::{
    assistant_with_tool_use, classify_category, group_for_tests, read_tool_result_user_message,
    rendered, tool_result_from_sender,
};
use uuid::Uuid;

/// A tool-result user message coming from a Claude session MUST classify
/// into the assistant group — otherwise serial Read tool uses don't roll
/// together with their preceding assistant turn.
///
/// This is the regression target for the "serial Read tool uses don't
/// group" symptom on Claude sessions.
#[test]
fn user_tool_result_classifies_with_assistant() {
    let user_tool_result = read_tool_result_user_message("toolu_01abc");
    assert_eq!(
        classify_category(&user_tool_result),
        Some(GroupCategory::Assistant),
        "user-tool-result message should classify into Assistant"
    );
}

/// Sanity: two consecutive (assistant tool_use + user tool_result) pairs
/// must collapse into a single assistant identity group of length 4. If the
/// classifier above is broken, this falls apart.
#[test]
fn serial_read_tool_uses_collapse_into_one_group() {
    let messages = vec![
        assistant_with_tool_use("toolu_01", "Read"),
        read_tool_result_user_message("toolu_01"),
        assistant_with_tool_use("toolu_02", "Read"),
        read_tool_result_user_message("toolu_02"),
    ];
    let groups = group_for_tests(&messages);
    assert_eq!(
        groups.len(),
        1,
        "expected one Assistant identity group carrying all 4 messages, got {} groups",
        groups.len()
    );
    match &groups[0] {
        MessageGroup::IdentityGroup {
            category: GroupCategory::Assistant,
            messages,
            ..
        } => assert_eq!(messages.len(), 4),
        other => panic!("expected an Assistant identity run, got {:?}", other),
    }
}

/// Edge case: top-level `content` field on a user-tool-result message
/// (e.g. from the optimistic-send envelope leaking onto a real echo)
/// trips the existing `msg.content.is_some()` early-bail and breaks the
/// run. This is a candidate root cause for the reported regression on
/// production Claude sessions even though the canonical wire shape
/// doesn't carry top-level `content`.
#[test]
fn user_tool_result_with_top_level_content_still_groups() {
    let with_top_level_content = serde_json::json!({
        "type": "user",
        "content": "stale optimistic content",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "toolu_01",
                "content": "file contents...",
            }]
        },
        "session_id": "01890000-0000-7000-8000-000000000001",
    })
    .to_string();
    assert_eq!(
        classify_category(&with_top_level_content),
        Some(GroupCategory::Assistant),
        "user-tool-result with a stale top-level `content` field should \
         still classify into Assistant; the dispatch must look at the \
         nested message blocks, not the envelope's top-level field"
    );
}

/// Predicate ordering guard: a tool-result user envelope must STILL go
/// into Assistant, not User. If `is_plain_text_user` claimed it first,
/// every Read tool-result on Claude would silently break the assistant
/// run.
#[test]
fn tool_result_user_envelope_stays_in_assistant_group() {
    let msg = read_tool_result_user_message("toolu_01");
    assert_eq!(classify_category(&msg), Some(GroupCategory::Assistant));
}

#[test]
fn tool_result_user_envelope_with_human_source_stays_in_assistant_group() {
    let user_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let msg = tool_result_from_sender("toolu_01", user_id, "Matt");

    assert_eq!(
        classify(&msg, shared::AgentType::Claude, Some(&user_id.to_string()))
            .map(|identity| identity.category),
        Some(GroupCategory::Assistant)
    );
}

#[test]
fn tool_result_user_envelope_with_human_source_renders_in_assistant_group() {
    let user_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let messages = vec![
        rendered(assistant_with_tool_use("toolu_01", "Read")),
        tool_result_from_sender("toolu_01", user_id, "Matt"),
    ];

    let groups = group_messages(
        &messages,
        shared::AgentType::Claude,
        Some(&user_id.to_string()),
    );

    assert_eq!(groups.len(), 1);
    match &groups[0] {
        MessageGroup::IdentityGroup {
            category: GroupCategory::Assistant,
            label,
            messages,
            ..
        } => {
            assert!(label.starts_with("Claude"));
            assert_eq!(messages.len(), 2);
        }
        other => panic!("expected Assistant identity group, got {:?}", other),
    }
}

#[test]
fn assistant_group_label_uses_claude_model() {
    let messages = vec![serde_json::json!({
        "type": "assistant",
        "message": {
            "id": "msg_1",
            "role": "assistant",
            "model": "claude-opus-4-7-20260501",
            "content": [{"type": "text", "text": "hello"}],
        },
        "session_id": "01890000-0000-7000-8000-000000000001",
    })
    .to_string()];

    let groups = group_for_tests(&messages);
    match &groups[0] {
        MessageGroup::IdentityGroup { label, .. } => {
            assert_eq!(label, "Claude - Opus 4.7");
        }
        other => panic!("expected assistant identity group, got {:?}", other),
    }
}
