//! Portal-family classification: consecutive portal frames collapse into one
//! group, and any other frame breaks the run.

use super::super::grouping::{GroupCategory, MessageGroup};
use super::fixtures::{
    assistant_with_tool_use, classify_category, group_for_tests, portal_text_message,
};

#[test]
fn portal_messages_classify_into_portal_group() {
    let msg = portal_text_message("Connection restored");
    assert_eq!(classify_category(&msg), Some(GroupCategory::Portal));
}

#[test]
fn serial_portal_messages_collapse_into_one_group() {
    let messages = vec![
        portal_text_message("Disconnected at 2026-05-18T05:00:00Z"),
        portal_text_message("Reconnected at 2026-05-18T05:01:00Z"),
        portal_text_message("Codex frame attached"),
    ];
    let groups = group_for_tests(&messages);
    assert_eq!(
        groups.len(),
        1,
        "expected one Portal group, got {} groups",
        groups.len()
    );
    match &groups[0] {
        MessageGroup::IdentityGroup {
            category: GroupCategory::Portal,
            messages,
            ..
        } => assert_eq!(messages.len(), 3),
        other => panic!("expected Portal identity run, got {:?}", other),
    }
}

/// An assistant message between two portal messages must split the run —
/// portal-group only collapses *consecutive* portal messages.
#[test]
fn portal_run_breaks_on_intervening_assistant() {
    let messages = vec![
        portal_text_message("first portal"),
        assistant_with_tool_use("toolu_01", "Read"),
        portal_text_message("second portal"),
    ];
    let groups = group_for_tests(&messages);
    assert_eq!(
        groups.len(),
        3,
        "expected 3 groups (Portal, Assistant, Portal), got {}",
        groups.len()
    );
    let cats: Vec<_> = groups
        .iter()
        .map(|g| match g {
            MessageGroup::IdentityGroup { category, .. } => Some(*category),
            MessageGroup::Single(_) => None,
        })
        .collect();
    assert_eq!(
        cats,
        vec![
            Some(GroupCategory::Portal),
            Some(GroupCategory::Assistant),
            Some(GroupCategory::Portal),
        ]
    );
}
