//! Rendering support for Muse sessions.
//!
//! Muse's journal is a third protocol shape (see `docs/MUSE_SUPPORT.md`),
//! and its distinguishing feature for the view is that work arrives as a
//! **task tree** rather than tool-use blocks. [`task_tree`] holds the pure
//! reducer that turns classified records into that tree; [`render_task_tree`]
//! draws it. The transcript groups a run of consecutive muse records into one
//! card (see `MessageGroupRenderer`), builds a tree from the group, and
//! renders it here — so a live turn's ~100 journal records read as one
//! structural view instead of a hundred raw-JSON bubbles.

use yew::prelude::*;

pub mod task_tree;

pub use task_tree::{TaskNode, TaskState, TaskTree};

/// Draw a task tree: one stacked card per task showing lifecycle state,
/// tool outcomes, streamed output, and the policy decision muse applied to
/// any side effect (muse decides tool policy itself and never prompts, so
/// those render as an audit trail rather than an approval). Internal
/// `reminder.*` scaffolding tasks are hidden (counted in the footer); records
/// the tree holds no structure for are also named in the footer — nothing on
/// the wire disappears silently.
pub fn render_task_tree(tree: &TaskTree) -> Html {
    // Muse injects internal scaffolding tasks — the `tbh-reminders` plugin's
    // skill/scope/goal/verify reminders, whose `task_kind` is `reminder.*`.
    // They're prompt bookkeeping, not user-facing work, so hide them from the
    // task list; a muted footer count keeps the "nothing drops silently"
    // invariant.
    let mut hidden_reminders = 0usize;
    let visible: Vec<&TaskNode> = tree
        .nodes()
        .filter(|node| {
            if is_reminder_task(node) {
                hidden_reminders += 1;
                false
            } else {
                true
            }
        })
        .collect();

    let mut footer: Vec<String> = tree
        .other_records()
        .map(|(payload_type, count)| {
            if count > 1 {
                format!("{payload_type} ×{count}")
            } else {
                payload_type.to_string()
            }
        })
        .collect();
    if hidden_reminders > 0 {
        let plural = if hidden_reminders == 1 { "" } else { "s" };
        footer.push(format!("{hidden_reminders} reminder task{plural} hidden"));
    }

    html! {
        <>
            // The agent's actual reply, rendered as markdown prose like a
            // Claude/Codex assistant message — the task tree below is the
            // supporting work log, the same way tool-use cards sit under
            // assistant text.
            if let Some(answer) = tree.answer() {
                <div class="muse-answer">
                    { crate::components::markdown::render_markdown(answer) }
                </div>
            }
            if !visible.is_empty() || !footer.is_empty() {
                <div class="muse-task-tree">
                    // Each task node carries a stable `key={task_id}` (set in
                    // render_task_node) so Yew updates a task in place as it
                    // advances running → completed, instead of positionally
                    // diffing (which made an updating task appear to jump).
                    // Mirrors how the Codex item list keys its cards.
                    { for visible.iter().map(|node| render_task_node(node)) }
                    if !footer.is_empty() {
                        <div class="muse-journal-footer">{ footer.join(" · ") }</div>
                    }
                </div>
            }
        </>
    }
}

/// Internal scaffolding task (the `tbh-reminders` plugin's skill/scope/goal/
/// verify reminders) — `task_kind` starts `reminder.`. Not user-facing work.
fn is_reminder_task(node: &TaskNode) -> bool {
    node.task_kind
        .as_deref()
        .is_some_and(|kind| kind.starts_with("reminder."))
}

fn render_task_node(node: &TaskNode) -> Html {
    let state = node.state;
    let kind = node.task_kind.as_deref().unwrap_or("task");
    // Codex-style stacked item: one card per task, no accordion. A running
    // task (Started, not yet terminal) carries the same in-progress cue as a
    // `.codex-item-in-progress` card — a pulsing dot + dimmed text.
    let running = state == TaskState::Started;
    let item_class = classes!(
        "muse-task",
        format!("muse-task-{}", state.label()),
        running.then_some("muse-task-in-progress"),
    );
    html! {
        <div class={item_class} key={node.task_id.clone()}>
            <div class="muse-task-header">
                <span class={classes!("muse-task-badge", format!("muse-task-{}", state.label()))}>
                    { state.label() }
                </span>
                <span class="muse-task-kind">{ kind }</span>
                if let Some(status) = node.status.as_deref() {
                    <span class="muse-task-status">{ status }</span>
                }
            </div>
            if let Some(reason) = node.reason.as_deref() {
                <div class="muse-task-reason">{ reason }</div>
            }
            if let Some((op, decision)) = node.side_effect.as_ref() {
                <div class="muse-task-side-effect">
                    { format!("{op} — policy: {decision}") }
                </div>
            }
            { for node.tool_results.iter().map(|r| {
                let outcome = r.outcome.as_deref().unwrap_or("unknown");
                let tool = r.tool_name.as_deref().unwrap_or("tool");
                html! {
                    <div class={classes!("muse-tool-result", format!("muse-tool-{outcome}"))}>
                        <span class="muse-tool-name">{ tool }</span>
                        <span class="muse-tool-text">{ &r.text }</span>
                    </div>
                }
            }) }
            { for node.output.iter().map(|chunk| html! {
                <div class="muse-task-output">{ chunk }</div>
            }) }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(kind: Option<&str>) -> TaskNode {
        TaskNode {
            task_kind: kind.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn reminder_scaffolding_tasks_are_hidden() {
        assert!(is_reminder_task(&node(Some(
            "reminder.agent.plugin:tbh-reminders:skill-reminder"
        ))));
        assert!(is_reminder_task(&node(Some(
            "reminder.agent.plugin:tbh-reminders:goal-reminder"
        ))));
    }

    #[test]
    fn real_tasks_and_unknowns_are_kept() {
        assert!(!is_reminder_task(&node(Some("model.unknown.response"))));
        assert!(!is_reminder_task(&node(None)));
        // Must START with `reminder.` — a kind that merely contains it stays.
        assert!(!is_reminder_task(&node(Some("agent.reminder.thing"))));
    }
}
