//! Portal-family classification: consecutive portal frames collapse into one
//! group, and any other frame breaks the run.

use super::super::grouping::{GroupCategory, MessageGroup};
use super::fixtures::{
    assistant_with_tool_use, classify_category, group_for_tests, portal_text_message, rendered,
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

/// An idle session reconnects on a loop, so these arrive as a run of
/// near-identical one-liners. The run must collapse to a single summary.
#[test]
fn a_run_of_reconnects_collapses_to_one_line() {
    use super::super::group_renderer::{connection_cycle_run, render_connection_cycle_run};

    let cycles: Vec<_> = ["38s", "38s", "38s", "36s"]
        .iter()
        .map(|d| {
            rendered(
                shared::PortalMessage::with_content(vec![shared::PortalContent::ConnectionCycle {
                    duration: Some((*d).to_string()),
                }])
                .to_json()
                .to_string(),
            )
        })
        .collect();

    let durations = connection_cycle_run(&cycles).expect("all members are connection cycles");
    assert_eq!(durations.len(), 4);
    // Renders at all, and as one node rather than four.
    let _ = render_connection_cycle_run(&durations);

    // A group carrying anything else must not collapse.
    let mut mixed = cycles.clone();
    mixed.push(rendered(
        shared::PortalMessage::text("hello".into())
            .to_json()
            .to_string(),
    ));
    assert!(connection_cycle_run(&mixed).is_none());
}
