//! Thinking-family grouping: `thinking_tokens` markers collapse into one
//! counted chip, and the odometer seeds carry across tool-call splits.

use super::super::grouping::{self, GroupCategory, MessageGroup};
use super::fixtures::{
    group_for_tests, read_tool_result_user_message, rendered_vec, result_message,
    thinking_tokens_message,
};

/// A run of `thinking_tokens` markers must collapse into a single
/// `Thinking` group (one counted chip), not one empty badge per marker —
/// the regression target for the "wall of THINKING_TOKENS badges" symptom.
#[test]
fn serial_thinking_tokens_collapse_into_one_group() {
    let messages = vec![
        thinking_tokens_message(50),
        thinking_tokens_message(150),
        thinking_tokens_message(250),
    ];
    let groups = group_for_tests(&messages);
    assert_eq!(groups.len(), 1);
    match &groups[0] {
        MessageGroup::IdentityGroup {
            category: GroupCategory::Thinking,
            messages,
            label,
            ..
        } => {
            assert_eq!(messages.len(), 3);
            assert_eq!(label, "thinking");
        }
        other => panic!("expected Thinking run, got {:?}", other),
    }
}

/// The condensed chip shows a token estimate, not a pulse count: each
/// marker reports the cumulative `estimated_tokens`, so the run's peak
/// (last) value is the burst total.
#[test]
fn thinking_tokens_estimate_returns_peak() {
    let messages = vec![
        thinking_tokens_message(50),
        thinking_tokens_message(150),
        thinking_tokens_message(250),
    ];
    assert_eq!(
        grouping::thinking_tokens_estimate(&rendered_vec(&messages)),
        250
    );
    // No markers / unparseable input yields 0 (chip hides).
    assert_eq!(grouping::thinking_tokens_estimate(&[]), 0);
}

/// When a tool call splits a thinking run, the later chip's odometer is
/// seeded with the earlier burst's peak so the (turn-cumulative) count
/// continues instead of re-racing from 0. Terminators reset the seed so
/// the next turn's first chip starts at 0 again.
#[test]
fn thinking_chip_starts_seed_across_splits_and_reset_on_terminator() {
    let messages = vec![
        // Turn 1, burst 1: climbs to 150.
        thinking_tokens_message(50),
        thinking_tokens_message(150),
        // Tool call splits the run.
        read_tool_result_user_message("toolu_01"),
        // Turn 1, burst 2: cumulative continues to 400.
        thinking_tokens_message(300),
        thinking_tokens_message(400),
        result_message(),
        // Turn 2, burst 1: fresh turn, fresh count.
        thinking_tokens_message(60),
    ];
    let groups = group_for_tests(&messages);
    let starts = grouping::thinking_chip_starts(&groups, shared::AgentType::Claude);
    assert_eq!(starts.len(), groups.len());
    // Burst 1 starts at 0; burst 2 is seeded with burst 1's peak; the
    // turn-2 burst starts at 0 again after the Result terminator.
    let thinking_starts: Vec<i64> = groups
        .iter()
        .zip(&starts)
        .filter_map(|(g, s)| match g {
            MessageGroup::IdentityGroup {
                category: GroupCategory::Thinking,
                ..
            } => Some(*s),
            _ => None,
        })
        .collect();
    assert_eq!(thinking_starts, vec![0, 150, 0]);
    // Non-thinking groups carry a 0 seed.
    for (g, s) in groups.iter().zip(&starts) {
        if !matches!(
            g,
            MessageGroup::IdentityGroup {
                category: GroupCategory::Thinking,
                ..
            }
        ) {
            assert_eq!(*s, 0);
        }
    }
}
