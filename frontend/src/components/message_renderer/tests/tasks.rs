//! Claude background-task frames: a finished task collapses to its
//! completion card, the way a Codex item's lifecycle frames collapse.

use super::super::grouping::MessageGroup;
use super::fixtures::group_for_tests;

const TEXT: &str = "Check whether the host is reachable";

fn task_started(task_id: &str, run_id: &str) -> String {
    serde_json::json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": "s-1",
        "task_id": task_id,
        "run_id": run_id,
        "task_type": "local_bash",
        "description": TEXT,
        "uuid": format!("u-{task_id}-{run_id}")
    })
    .to_string()
}

fn task_notification(task_id: &str, run_id: &str, status: &str, summary: &str) -> String {
    serde_json::json!({
        "type": "system",
        "subtype": "task_notification",
        "session_id": "s-1",
        "task_id": task_id,
        "run_id": run_id,
        "status": status,
        "summary": summary,
        "output_file": null
    })
    .to_string()
}

fn completed(task_id: &str, run_id: &str) -> String {
    task_notification(task_id, run_id, "completed", TEXT)
}

fn single_contents(groups: &[MessageGroup]) -> Vec<String> {
    groups
        .iter()
        .map(|group| match group {
            MessageGroup::Single(message) => message.content().to_string(),
            other => panic!("expected Single, got {:?}", other),
        })
        .collect()
}

#[test]
fn finished_task_renders_only_its_completion() {
    let done = completed("t-1", "r-1");
    let groups = group_for_tests(&[task_started("t-1", "r-1"), done.clone()]);
    assert_eq!(single_contents(&groups), vec![done]);
}

#[test]
fn running_task_keeps_its_started_card() {
    let started = task_started("t-1", "r-1");
    let other_done = completed("t-2", "r-9");
    let groups = group_for_tests(&[started.clone(), other_done.clone()]);
    assert_eq!(single_contents(&groups), vec![started, other_done]);
}

/// A completion of run A that arrives after run B started (late or
/// replayed) must not hide B: pairing is by `run_id`, not position.
#[test]
fn late_completion_of_an_earlier_run_does_not_hide_a_resumed_run() {
    let resumed = task_started("t-1", "r-2");
    let late_done = completed("t-1", "r-1");
    let groups = group_for_tests(&[
        task_started("t-1", "r-1"),
        resumed.clone(),
        late_done.clone(),
    ]);
    assert_eq!(single_contents(&groups), vec![resumed, late_done]);
}

/// Only a terminal status ends a run; a progress notification doesn't.
#[test]
fn progress_notification_does_not_close_a_task() {
    let started = task_started("t-1", "r-1");
    let progress = task_notification("t-1", "r-1", "running", TEXT);
    let groups = group_for_tests(&[started.clone(), progress.clone()]);
    assert_eq!(single_contents(&groups), vec![started, progress]);
}

/// A withheld handback card shows no text, even when its raw summary (harness
/// notes for the model) happens to repeat the description, so hiding the
/// start would lose the description: both cards stay.
#[test]
fn withheld_handback_keeps_the_start_even_with_a_matching_raw_summary() {
    let started = task_started("t-1", "r-1");
    let mut withheld: serde_json::Value = serde_json::from_str(&completed("t-1", "r-1")).unwrap();
    withheld["handback"] = serde_json::json!("withheld");
    let withheld = withheld.to_string();
    let groups = group_for_tests(&[started.clone(), withheld.clone()]);
    assert_eq!(single_contents(&groups), vec![started, withheld]);
}

/// When the completion says something different (a sub-agent's report),
/// hiding the start would lose its description, so both cards stay.
#[test]
fn differing_summary_keeps_both_cards() {
    let started = task_started("t-1", "r-1");
    let report = task_notification("t-1", "r-1", "completed", "Found three hosts.");
    let groups = group_for_tests(&[started.clone(), report.clone()]);
    assert_eq!(single_contents(&groups), vec![started, report]);
}
