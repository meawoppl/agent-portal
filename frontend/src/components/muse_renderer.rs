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

pub use task_tree::{TaskNode, TaskTree};

/// Draw a task tree: one collapsible node per task showing lifecycle state,
/// tool outcomes, streamed output, and the policy decision muse applied to
/// any side effect (muse decides tool policy itself and never prompts, so
/// those render as an audit trail rather than an approval). Records the tree
/// holds no structure for are named in a muted footer — nothing on the wire
/// disappears silently.
pub fn render_task_tree(tree: &TaskTree) -> Html {
    let footer: Vec<String> = tree
        .other_records()
        .map(|(payload_type, count)| {
            if count > 1 {
                format!("{payload_type} ×{count}")
            } else {
                payload_type.to_string()
            }
        })
        .collect();
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
            if tree.nodes().next().is_some() || !footer.is_empty() {
                <div class="muse-task-tree">
                    { for tree.nodes().map(render_task_node) }
                    if !footer.is_empty() {
                        <div class="muse-journal-footer">{ footer.join(" · ") }</div>
                    }
                </div>
            }
        </>
    }
}

fn render_task_node(node: &TaskNode) -> Html {
    let state = node.state;
    let kind = node.task_kind.as_deref().unwrap_or("task");
    html! {
        <details class="muse-task" open={!state.is_terminal()}>
            <summary class="muse-task-summary">
                <span class={classes!("muse-task-badge", format!("muse-task-{}", state.label()))}>
                    { state.label() }
                </span>
                <span class="muse-task-kind">{ kind }</span>
                if let Some(status) = node.status.as_deref() {
                    <span class="muse-task-status">{ status }</span>
                }
            </summary>
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
        </details>
    }
}
