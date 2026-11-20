//! Claude background-task frames: a finished task collapses to its
//! completion card, the way a Codex item's lifecycle frames collapse.

use super::super::grouping::MessageGroup;
use super::fixtures::group_for_tests;

fn task_started(task_id: &str) -> String {
    serde_json::json!({
        "type": "system",
        "subtype": "task_started",
        "session_id": "s-1",
        "task_id": task_id,

        "task_type": "local_bash",
        "uuid": "u-1",
        "description": "Check whether the host is reachable"
    })
    .to_string()
}

fn task_notification(task_id: &str) -> String {
    serde_json::json!({
        "type": "system",
        "subtype": "task_notification",
        "session_id": "s-1",
        "task_id": task_id,

        "status": "completed",
        "summary": "Check whether the host is reachable",
        "output_file": null
    })
    .to_string()
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
    let done = task_notification("t-1");
    let groups = group_for_tests(&[task_started("t-1"), done.clone()]);
    assert_eq!(single_contents(&groups), vec![done]);
}

#[test]
fn running_task_keeps_its_started_card() {
    let started = task_started("t-1");
    let other_done = task_notification("t-2");
    let groups = group_for_tests(&[started.clone(), other_done.clone()]);
    assert_eq!(single_contents(&groups), vec![started, other_done]);
}

#[test]
fn resumed_run_is_not_hidden_by_an_earlier_runs_completion() {
    let first_done = task_notification("t-1");
    let resumed = task_started("t-1");
    let groups = group_for_tests(&[task_started("t-1"), first_done.clone(), resumed.clone()]);
    assert_eq!(single_contents(&groups), vec![first_done, resumed]);
}
