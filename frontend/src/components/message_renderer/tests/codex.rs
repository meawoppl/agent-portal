//! Codex-family grouping plus the per-`item_id` lifecycle dedup (#776) that
//! keeps `started → updated → completed` from drawing three cards.

use super::super::grouping::{visible_group_indices, GroupCategory, MessageGroup};
use super::fixtures::{
    classify_codex_category, codex_command_event, codex_item_started_agent_message,
    group_for_codex_tests, portal_text_message, rendered_vec,
};

#[test]
fn codex_event_classifies_into_codex_group() {
    let msg = codex_item_started_agent_message("hi");
    assert_eq!(classify_codex_category(&msg), Some(GroupCategory::Codex));
}

#[test]
fn serial_codex_events_collapse_into_codex_group() {
    let messages = vec![
        codex_item_started_agent_message("starting"),
        codex_item_started_agent_message("more progress"),
        codex_item_started_agent_message("done"),
    ];
    let groups = group_for_codex_tests(&messages);
    assert_eq!(groups.len(), 1);
    match &groups[0] {
        MessageGroup::IdentityGroup {
            category: GroupCategory::Codex,
            messages,
            ..
        } => assert_eq!(messages.len(), 3),
        other => panic!("expected Codex identity run, got {:?}", other),
    }
}

/// A portal message between two codex events must split the run —
/// codex-group only collapses *consecutive* codex events.
#[test]
fn codex_run_breaks_on_intervening_portal() {
    let messages = vec![
        codex_item_started_agent_message("first"),
        portal_text_message("reconnected"),
        codex_item_started_agent_message("second"),
    ];
    let groups = group_for_codex_tests(&messages);
    assert_eq!(groups.len(), 3);
    let cats: Vec<_> = groups
        .iter()
        .filter_map(|g| match g {
            MessageGroup::IdentityGroup { category, .. } => Some(*category),
            MessageGroup::Single(_) => None,
        })
        .collect();
    assert_eq!(
        cats,
        vec![
            GroupCategory::Codex,
            GroupCategory::Portal,
            GroupCategory::Codex,
        ]
    );
}

// ---- #776: codex lifecycle dedup ----

/// `item.started` + `item.completed` for the same `item_id` should collapse
/// to a single visible card (the completed one), not render as two
/// near-identical cards. Regression target for #776.
#[test]
fn codex_command_lifecycle_dedupes_to_completed() {
    let messages = vec![
        codex_command_event("item.started", "cmd_1", "in_progress"),
        codex_command_event("item.completed", "cmd_1", "completed"),
    ];
    let visible = visible_group_indices(GroupCategory::Codex, &rendered_vec(&messages));
    assert_eq!(
        visible,
        vec![1],
        "expected only the completed event to remain visible (#776), got {:?}",
        visible
    );
}

/// A `started → updated → completed` triple for the same item collapses to
/// the final completed event. The updated stages add nothing visible past
/// what completed already shows.
#[test]
fn codex_command_started_updated_completed_dedupes_to_completed() {
    let messages = vec![
        codex_command_event("item.started", "cmd_1", "in_progress"),
        codex_command_event("item.updated", "cmd_1", "in_progress"),
        codex_command_event("item.completed", "cmd_1", "completed"),
    ];
    let visible = visible_group_indices(GroupCategory::Codex, &rendered_vec(&messages));
    assert_eq!(visible, vec![2]);
}

/// Two distinct items in the same group keep their own cards — dedup is
/// per-`item_id`, never collapses different items together.
#[test]
fn codex_two_distinct_items_each_keep_one_card() {
    let messages = vec![
        codex_command_event("item.started", "cmd_a", "in_progress"),
        codex_command_event("item.completed", "cmd_a", "completed"),
        codex_command_event("item.started", "cmd_b", "in_progress"),
        codex_command_event("item.completed", "cmd_b", "completed"),
    ];
    let visible = visible_group_indices(GroupCategory::Codex, &rendered_vec(&messages));
    // Indices 1 (cmd_a completed) and 3 (cmd_b completed) remain.
    assert_eq!(visible, vec![1, 3]);
}

/// Non-item events in a codex group (turn-level, deltas, errors) carry no
/// `item_id` and must always pass through the dedup unchanged — they're
/// standalone signals, not lifecycle stages.
#[test]
fn codex_non_item_events_always_visible() {
    let turn_completed = serde_json::json!({
        "type": "turn.completed",
        "usage": {"input_tokens": 1, "output_tokens": 2},
    })
    .to_string();
    let messages = vec![
        codex_command_event("item.started", "cmd_1", "in_progress"),
        turn_completed.clone(),
        codex_command_event("item.completed", "cmd_1", "completed"),
    ];
    let visible = visible_group_indices(GroupCategory::Codex, &rendered_vec(&messages));
    // turn.completed (index 1) is kept; the started (index 0) drops in
    // favor of the completed (index 2).
    assert_eq!(visible, vec![1, 2]);
}

/// Dedup is Codex-only — assistant, portal, user, and non-grouped paths
/// must keep every index. Even a degenerate same-id codex-shaped JSON in
/// a non-Codex group should still render fully (the predicate only runs
/// for `GroupCategory::Codex`).
#[test]
fn visible_group_indices_is_codex_only() {
    let messages = vec![
        codex_command_event("item.started", "cmd_1", "in_progress"),
        codex_command_event("item.completed", "cmd_1", "completed"),
    ];
    for cat in [
        GroupCategory::Assistant,
        GroupCategory::Portal,
        GroupCategory::User,
    ] {
        let visible = visible_group_indices(cat, &rendered_vec(&messages));
        assert_eq!(
            visible,
            vec![0, 1],
            "dedup must not fire for {:?}; got {:?}",
            cat,
            visible
        );
    }
}

/// A Codex item with no `id` field must not collapse into a same-shape
/// neighbor — dedup is keyed on `item_id`, so a missing id means
/// "definitely not the same item".
#[test]
fn codex_items_without_id_do_not_collapse() {
    let no_id_a = serde_json::json!({
        "type": "item.started",
        "item": {"type": "agent_message", "text": "first"},
    })
    .to_string();
    let no_id_b = serde_json::json!({
        "type": "item.completed",
        "item": {"type": "agent_message", "text": "second"},
    })
    .to_string();
    let visible = visible_group_indices(GroupCategory::Codex, &rendered_vec(&[no_id_a, no_id_b]));
    assert_eq!(visible, vec![0, 1]);
}
