//! Muse-family grouping: a turn's journal records must be recognized frames
//! and collapse into the single task-tree card the group renderer draws.

use super::super::grouping::{GroupCategory, MessageGroup};
use super::fixtures::{group_for_muse_tests, muse_lifecycle_message, portal_text_message};
use crate::components::agent_frame::{AgentFrame, AgentFrameRegistry};

/// The regression behind the live bug report: on a Muse session a journal
/// record must classify as a Muse frame, never fall through to RawJson
/// (which renders the "Unrecognized Message" bubble).
#[test]
fn muse_record_is_a_recognized_frame_on_muse_sessions() {
    let json = muse_lifecycle_message("proposed", "t1");
    let frame = AgentFrameRegistry::parse(&json, shared::AgentType::Muse);
    assert!(
        matches!(frame, AgentFrame::Muse(_)),
        "muse_record fell through to {frame:?} — it will render as raw JSON"
    );
    // On non-muse sessions the shape stays raw: another agent's transcript
    // must not grow muse cards from a stray look-alike payload.
    let frame = AgentFrameRegistry::parse(&json, shared::AgentType::Claude);
    assert!(matches!(frame, AgentFrame::RawJson));
}

/// A turn's worth of journal records collapses into ONE muse group — the
/// group renderer draws it as a single task-tree card.
#[test]
fn serial_muse_records_collapse_into_one_group() {
    let messages = vec![
        muse_lifecycle_message("proposed", "t1"),
        muse_lifecycle_message("started", "t1"),
        muse_lifecycle_message("completed", "t1"),
    ];
    let groups = group_for_muse_tests(&messages);
    assert_eq!(groups.len(), 1);
    match &groups[0] {
        MessageGroup::IdentityGroup {
            category: GroupCategory::Muse,
            messages,
            ..
        } => assert_eq!(messages.len(), 3),
        other => panic!("expected one Muse identity run, got {other:?}"),
    }
}

/// A user message between turns splits the run, so each turn renders its
/// own task-tree card rather than merging across the conversation.
#[test]
fn muse_run_breaks_on_intervening_portal_message() {
    let messages = vec![
        muse_lifecycle_message("proposed", "t1"),
        portal_text_message("reconnected"),
        muse_lifecycle_message("proposed", "t2"),
    ];
    let groups = group_for_muse_tests(&messages);
    assert_eq!(groups.len(), 3);
}
