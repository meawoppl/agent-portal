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

/// An expected cycle must survive the wire as its own typed variant rather
/// than a prose card — the whole point is that the frontend can render it
/// thin. Round-tripped because the proxy serializes it and the frontend
/// deserializes it, with a backend and a DB in between.
#[test]
fn connection_cycle_round_trips_as_its_own_variant() {
    let json = serde_json::to_string(&shared::PortalContent::ConnectionCycle {
        duration: Some("35s".to_string()),
    })
    .expect("serializes");
    assert!(
        json.contains("connection_cycle"),
        "tagged for the frontend: {json}"
    );
    match serde_json::from_str::<shared::PortalContent>(&json).expect("round trips") {
        shared::PortalContent::ConnectionCycle { duration } => {
            assert_eq!(duration.as_deref(), Some("35s"))
        }
        other => panic!("expected ConnectionCycle, got {other:?}"),
    }
}

fn connection_cycle(duration: &str) -> String {
    shared::PortalMessage::with_content(vec![shared::PortalContent::ConnectionCycle {
        duration: Some(duration.to_string()),
    }])
    .to_json()
    .to_string()
}

fn commands_changed() -> String {
    serde_json::json!({
        "type": "system",
        "subtype": "commands_changed",
        "session_id": "01890000-0000-7000-8000-000000000001",
        "uuid": "01890000-0000-7000-8000-000000000002"
    })
    .to_string()
}

/// Reconnects and the `commands_changed` marker Claude re-sends after each
/// one are transport churn: an idle session used to fill with alternating
/// rows of both. Typed and historical prose notices are both hidden.
#[test]
fn reconnect_notices_and_commands_changed_are_hidden() {
    let messages = vec![
        connection_cycle("38s"),
        commands_changed(),
        shared::PortalMessage::text(
            "**Proxy reconnected** after 4s (unexpected disconnect)\n  reconnected at 2026-09-30T09:11:28Z (UTC)".into(),
        )
        .to_json()
        .to_string(),
        commands_changed(),
    ];
    assert!(group_for_tests(&messages).is_empty());
}

/// Hidden frames must not split the run around them.
#[test]
fn hidden_frames_do_not_split_a_portal_run() {
    let messages = vec![
        portal_text_message("first"),
        connection_cycle("3s"),
        commands_changed(),
        portal_text_message("second"),
    ];
    let groups = group_for_tests(&messages);
    assert_eq!(groups.len(), 1);
    match &groups[0] {
        MessageGroup::IdentityGroup { messages, .. } => assert_eq!(messages.len(), 2),
        other => panic!("expected one Portal group, got {other:?}"),
    }
}
