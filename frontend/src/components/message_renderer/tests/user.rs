//! User-family classification: plain prose groups as "You" (per sender), and
//! inter-agent echoes must escape the User group entirely.

use super::super::grouping::{classify, group_messages, GroupCategory, MessageGroup};
use super::fixtures::{
    assistant_with_tool_use, classify_category, group_for_tests, plain_user_text,
    plain_user_text_from_sender, rendered, user_text_message,
};
use uuid::Uuid;

/// A claude-echoed inter-agent message (`[message from …]`, no provenance
/// metadata) must render as its own "Message from …" card, so `classify`
/// keeps it out of the User group (returns `None` → `Single`). Ordinary
/// prose still groups as User. Regression: grouped inter-agent messages
/// were rendering their raw `[message from …]` / system-reminder wrapper.
#[test]
fn inter_agent_user_text_is_not_grouped() {
    let sid = "e2d342f5-68c6-4134-a5d8-63cb4afcee9e";
    let interagent = user_text_message(&format!(
        "[message from codex {sid}]\nExactly.\n\n<system-reminder> reply to that agent </system-reminder>"
    ));
    assert!(
        classify(&rendered(&interagent), shared::AgentType::Claude, None).is_none(),
        "inter-agent message must render as a Single card, not group"
    );

    let prose = user_text_message("just some ordinary prose");
    assert_eq!(
        classify(&rendered(&prose), shared::AgentType::Claude, None).map(|i| i.category),
        Some(GroupCategory::User),
    );
}

/// A claude-echoed inter-agent message that DOES carry provenance metadata
/// must also escape the User group. The backend stamps every role="user"
/// row with `meta.source = Human(owner)` — including claude's echo of an
/// injected inter-agent message — so a `source.is_none()`-gated detector
/// skipped exactly the claude→claude case and the raw `[message from …]` /
/// system-reminder wrapper rendered inside a "You" group.
#[test]
fn inter_agent_user_text_with_source_is_not_grouped() {
    let sid = "0c9ecefe-c17e-48b6-873d-53df32ab47a2";
    let body = format!(
        "[message from claude {sid}]\n\u{1F44D} All set — signing off.\n\n\
         <system-reminder>\nThis message came from another agent. Reply to that agent, not the user.\n</system-reminder>"
    );
    let echo = plain_user_text_from_sender(&body, Uuid::nil(), "Matthew Goodman");
    assert!(
        classify(&echo, shared::AgentType::Claude, None).is_none(),
        "source-stamped inter-agent echo must render as a Single card, not group as You"
    );

    // Ordinary prose with the same source still groups as User.
    let prose = plain_user_text_from_sender("ordinary prose", Uuid::nil(), "Matthew Goodman");
    assert_eq!(
        classify(&prose, shared::AgentType::Claude, None).map(|i| i.category),
        Some(GroupCategory::User),
    );
}

/// The synthetic-echo (optimistic-user) shape carrying an inter-agent
/// message must ALSO render as its own card, not fold into "You". Codex's
/// `UserEchoEvent` and older proxies' echoes parse as `OptimisticUser`
/// (top-level `content` string), which the `User`-arm detection missed —
/// this is the regression that surfaced the raw `[message from …]` /
/// system-reminder wrapper again.
#[test]
fn inter_agent_optimistic_user_is_not_grouped() {
    let sid = "e2d342f5-68c6-4134-a5d8-63cb4afcee9e";
    let body = format!(
        "[message from codex {sid}]\nReview started.\n\n\
         <system-reminder>\nReply to that agent, not the user.\n</system-reminder>"
    );

    // Pure optimistic shape (no `message` field) — parses as OptimisticUser.
    let bare = serde_json::json!({ "type": "user", "content": body }).to_string();
    assert!(
        classify(&rendered(&bare), shared::AgentType::Codex, None).is_none(),
        "optimistic inter-agent echo must render as a Single card, not group"
    );

    // The full Codex `UserEchoEvent` shape (blocks + top-level content).
    let codex_echo = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": body }] },
        "content": body,
    })
    .to_string();
    assert!(
        classify(&rendered(&codex_echo), shared::AgentType::Codex, None).is_none(),
        "codex UserEchoEvent inter-agent shape must render as a Single card"
    );

    // An ordinary optimistic echo still groups as User.
    let prose =
        serde_json::json!({ "type": "user", "content": "ordinary pending prose" }).to_string();
    assert_eq!(
        classify(&rendered(&prose), shared::AgentType::Codex, None).map(|i| i.category),
        Some(GroupCategory::User),
    );
}

/// Edge case: real user input (plain text typed by the human, not a
/// tool result) must NOT join the assistant group, otherwise prose
/// would silently get rolled into a previous assistant block.
#[test]
fn real_user_text_does_not_group_with_assistant() {
    let plain_user = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": "hello agent"}]
        },
        "session_id": "01890000-0000-7000-8000-000000000001",
    })
    .to_string();
    assert_eq!(
        classify_category(&plain_user),
        Some(GroupCategory::User),
        "plain-text user message must classify into User, not Assistant"
    );
}

#[test]
fn plain_text_user_classifies_into_user_group() {
    let msg = plain_user_text("hello agent");
    assert_eq!(classify_category(&msg), Some(GroupCategory::User));
}

#[test]
fn serial_user_text_collapses_into_user_group() {
    let messages = vec![
        plain_user_text("first prompt"),
        plain_user_text("follow-up"),
        plain_user_text("one more thing"),
    ];
    let groups = group_for_tests(&messages);
    assert_eq!(groups.len(), 1);
    match &groups[0] {
        MessageGroup::IdentityGroup {
            category: GroupCategory::User,
            messages,
            ..
        } => assert_eq!(messages.len(), 3),
        other => panic!("expected User identity run, got {:?}", other),
    }
}

#[test]
fn user_grouping_splits_by_sender_identity() {
    let user_a = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let user_b = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let messages = vec![
        plain_user_text_from_sender("first from me", user_a, "Matt"),
        plain_user_text_from_sender("second from me", user_a, "Matt"),
        plain_user_text_from_sender("from someone else", user_b, "Alex"),
        plain_user_text_from_sender("back to me", user_a, "Matt"),
    ];

    let current_user_id = user_a.to_string();
    let groups = group_messages(&messages, shared::AgentType::Claude, Some(&current_user_id));
    assert_eq!(groups.len(), 3);

    let labels: Vec<_> = groups
        .iter()
        .map(|group| match group {
            MessageGroup::IdentityGroup {
                category: GroupCategory::User,
                label,
                ..
            } => label.as_str(),
            other => panic!("expected User identity group, got {:?}", other),
        })
        .collect();
    assert_eq!(labels, vec!["You", "Alex", "You"]);

    match &groups[0] {
        MessageGroup::IdentityGroup { messages, .. } => assert_eq!(messages.len(), 2),
        other => panic!("expected first User group, got {:?}", other),
    }
}

#[test]
fn user_run_breaks_on_intervening_assistant() {
    let messages = vec![
        plain_user_text("question one"),
        assistant_with_tool_use("toolu_01", "Read"),
        plain_user_text("question two"),
    ];
    let groups = group_for_tests(&messages);
    assert_eq!(groups.len(), 3);
}
