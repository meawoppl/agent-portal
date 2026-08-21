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

use muse_codes::CommandResult;
use yew::prelude::*;

use super::expandable::ExpandableText;
use super::tool_renderers::OUTPUT_PREVIEW_CHARS;

pub mod task_tree;

pub use task_tree::{TaskNode, TaskState, TaskTree};

/// Recognize a command-execution `text` payload. Cheap guard first (only a JSON
/// object can be one), then use muse-codes' provider-owned typed binding. The
/// non-empty command guard prevents a stray future JSON blob from being drawn
/// as a shell command.
fn parse_command_result(text: &str) -> Option<CommandResult> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    serde_json::from_str::<CommandResult>(trimmed)
        .ok()
        .filter(|c| !c.command.trim().is_empty())
}

/// Render a muse command result via the shared [`CommandResultCard`] — the same
/// green/collapsible treatment the Claude/Codex tool results use — rather than
/// printing the raw JSON muse packs into `text`. Intent first, then the `$`
/// command line, then the collapsible output.
fn render_command_card(cmd: &CommandResult) -> Html {
    use crate::components::tool_renderers::CommandResultCard;
    // muse truncates long output itself and flags it; surface that as a note
    // appended to the output so the card doesn't imply it saw everything.
    let output = if cmd.truncated {
        format!("{}\n[output truncated]", cmd.output)
    } else {
        cmd.output.clone()
    };
    html! {
        <CommandResultCard
            command={AttrValue::from(cmd.command.clone())}
            description={Some(AttrValue::from(cmd.description.clone()))}
            output={Some(AttrValue::from(output))}
            exit_code={Some(i64::from(cmd.exit_code))}
        />
    }
}

#[derive(Debug, PartialEq)]
struct MuseReadResult<'a> {
    path: &'a str,
    content: &'a str,
}

/// Muse's read result is a provider-owned prose envelope around the file
/// contents. Keep this tiny adapter here, then hand the normalized fields to
/// the same Read card Claude uses. The tool-name gate prevents unrelated prose
/// containing the same words from being misclassified.
fn parse_read_result<'a>(tool_name: Option<&str>, text: &'a str) -> Option<MuseReadResult<'a>> {
    if tool_name != Some("read_file") {
        return None;
    }
    let rest = text.strip_prefix("Read text file `")?;
    let (path, content) = rest
        .split_once("`.\n")
        .or_else(|| rest.split_once("`.\r\n"))
        .or_else(|| rest.strip_suffix("`.").map(|path| (path, "")))?;
    Some(MuseReadResult { path, content })
}

/// Extract the unified diff Muse places after its short edit summary. The
/// path is carried separately in typed `edit_facts` and is never scraped from
/// the prose result.
fn parse_edit_diff(tool_name: Option<&str>, text: &str) -> Option<String> {
    if tool_name != Some("edit_file") {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    let header = lines
        .iter()
        .position(|line| line.trim_start() == "--- original")?;
    let indent = lines[header].len() - lines[header].trim_start().len();
    Some(
        lines[header..]
            .iter()
            .map(|line| line.get(indent..).unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn render_muse_tool_result(result: &task_tree::ToolOutcome) -> Html {
    use crate::components::diff::{DiffCard, DiffSource};
    use crate::components::tool_renderers::ReadToolCard;

    let outcome = result.outcome.as_deref().unwrap_or("unknown");
    let tool = result.tool_name.as_deref().unwrap_or("tool");
    if let Some(cmd) = parse_command_result(&result.text) {
        return html! {
            <div class={classes!("muse-tool-command", format!("muse-tool-{outcome}"))}>
                { render_command_card(&cmd) }
            </div>
        };
    }
    if let Some(read) = parse_read_result(result.tool_name.as_deref(), &result.text) {
        return html! {
            <div class={classes!("muse-tool-card", format!("muse-tool-{outcome}"))}>
                <ReadToolCard
                    file_path={AttrValue::from(read.path.to_string())}
                    content={AttrValue::from(read.content.to_string())}
                />
            </div>
        };
    }
    if let Some(diff) = parse_edit_diff(result.tool_name.as_deref(), &result.text) {
        return html! {
            <div class={classes!("muse-tool-card", format!("muse-tool-{outcome}"))}>
                <DiffCard
                    source={DiffSource::Unified { text: diff }}
                    file_path={result.edit_path.clone().map(AttrValue::from)}
                    kind="update"
                />
            </div>
        };
    }
    // Non-command tool results (search, list, …) contract past the shared
    // preview threshold like the Claude/Codex tool output does — a bounded
    // search alone can be 80 matches long (#1628).
    html! {
        <div class={classes!("muse-tool-result", format!("muse-tool-{outcome}"))}>
            <span class="muse-tool-name">{ tool }</span>
            <ExpandableText
                full_text={result.text.clone()}
                max_len={OUTPUT_PREVIEW_CHARS}
                tag="span"
                class={classes!("muse-tool-text")}
                ansi=true
            />
        </div>
    }
}

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
            if is_hidden_scaffolding(node) {
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
            // Match Claude/Codex's transcript order: work appears where it
            // happened, then the assistant's answer closes the turn. Tool
            // cards keep their own focused expand/collapse controls; the
            // whole execution trace is never hidden behind a second toggle.
            if !visible.is_empty() || !footer.is_empty() {
                <div class="muse-task-tree">
                    { for visible.iter().map(|node| render_task_node(node)) }
                    if !footer.is_empty() {
                        <div class="muse-journal-footer">{ footer.join(" · ") }</div>
                    }
                </div>
            }
            if let Some(answer) = tree.answer() {
                <div class="muse-answer">
                    { crate::components::markdown::render_markdown(answer) }
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

/// A reminder scaffolding task we can safely hide: only when it carries **no
/// user-facing content**. The reducer's current tool-result attribution
/// ("latest running task") lands tool outcomes on a running scaffolding task
/// in every captured turn, so blindly hiding all reminders would silently drop
/// the tool results Matt most wants to see. Rendering any reminder node that
/// holds a tool result, streamed output, or a side-effect keeps that content
/// visible; the attribution itself is corrected upstream in the reducer.
fn is_hidden_scaffolding(node: &TaskNode) -> bool {
    is_reminder_task(node)
        && node.tool_results.is_empty()
        && node.output.is_empty()
        && node.side_effect.is_none()
}

/// Whether a side-effect policy decision deserves a line on the task card.
/// Muse's routine decisions (`allow:policy`, `not_applicable`) are stamped on
/// effectively every task and carry no information; a denial is the anomaly
/// the audit line exists for. Anything outside the known-boring vocabulary
/// renders too, so future decision kinds surface instead of vanishing.
fn side_effect_is_noteworthy(decision: &str) -> bool {
    !(decision == "not_applicable" || decision.starts_with("allow"))
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
    // Muse emits the same content on BOTH a `tool.result` and a
    // `task.lifecycle.output` chunk (byte-identical), so an output chunk already
    // shown as a tool result is skipped below — otherwise every command renders
    // twice (the original bug).
    let shown_as_result: std::collections::HashSet<&str> =
        node.tool_results.iter().map(|r| r.text.as_str()).collect();
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
            // The side-effect audit line is noise when muse allowed the
            // operation — every routine card reads "… — policy: allow:policy"
            // / "not_applicable" and merely repeats the header's task kind. It
            // only carries signal when muse REFUSED something, so render it
            // for denials (and any unrecognized future vocabulary — unknown
            // decisions fail visible, not silent).
            if let Some((op, decision)) = node
                .side_effect
                .as_ref()
                .filter(|(_, decision)| side_effect_is_noteworthy(decision))
            {
                <div class="muse-task-side-effect">
                    { format!("{op} — policy: {decision}") }
                </div>
            }
            { for node.tool_results.iter().map(render_muse_tool_result) }
            { for node.output.iter().filter(|c| !shown_as_result.contains(c.as_str())).map(|chunk| {
                // A surviving chunk that is itself a command payload still gets
                // the card treatment rather than a raw dump.
                if let Some(cmd) = parse_command_result(chunk) {
                    html! { <div class="muse-tool-command">{ render_command_card(&cmd) }</div> }
                } else {
                    html! {
                        <ExpandableText
                            full_text={chunk.clone()}
                            max_len={OUTPUT_PREVIEW_CHARS}
                            tag="div"
                            class={classes!("muse-task-output")}
                            ansi=true
                        />
                    }
                }
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

    const REMINDER: &str = "reminder.agent.plugin:tbh-reminders:scope-reminder";

    /// Real capture (meawoppl-fc): muse's `bash` tool packs its structured
    /// result into `tool.result.text` as JSON, and emits the identical string a
    /// second time as a `task.lifecycle.output` chunk — the two sources of the
    /// original "raw JSON, rendered twice" bug.
    const BASH_FIXTURE: &str = include_str!("muse_renderer/fixtures/bash_tool_result.jsonl");

    fn bash_tree() -> TaskTree {
        let mut tree = TaskTree::default();
        for line in BASH_FIXTURE.lines().filter(|l| !l.trim().is_empty()) {
            tree.apply(&serde_json::from_str(line).expect("fixture line is JSON"));
        }
        tree
    }

    #[test]
    fn bash_text_parses_into_a_command_result() {
        let tree = bash_tree();
        let node = tree
            .nodes()
            .find(|n| !n.tool_results.is_empty())
            .expect("a node holds the bash tool result");
        let cmd = parse_command_result(&node.tool_results[0].text)
            .expect("bash tool text is a command result, not opaque prose");
        assert!(cmd.command.starts_with("curl -s -X POST"));
        assert_eq!(cmd.exit_code, 0);
        assert!(!cmd.truncated);
        assert!(cmd.output.contains("d99dce066453"));
    }

    #[test]
    fn prose_tool_text_is_not_mistaken_for_a_command() {
        // write_file / read_file summaries are plain strings — must stay text.
        assert!(parse_command_result("wrote 6 bytes to /tmp/hello.txt").is_none());
        assert!(parse_command_result("Read text file `hello.txt`.\n1|hello").is_none());
        // A JSON object without a command is not a command card either.
        assert!(parse_command_result(r#"{"note":"hi"}"#).is_none());
    }

    #[test]
    fn read_file_result_is_normalized_for_the_shared_read_card() {
        let text =
            "Read text file `frontend/src/lib.rs`.\n    (9 line(s) above)\n  10|fn main() {}";
        let read = parse_read_result(Some("read_file"), text).expect("Muse read envelope");
        assert_eq!(read.path, "frontend/src/lib.rs");
        assert_eq!(read.content, "    (9 line(s) above)\n  10|fn main() {}");
        assert!(parse_read_result(Some("write_file"), text).is_none());
    }

    #[test]
    fn edit_file_result_yields_a_unified_diff_body() {
        let text = "edit_file edited\n    changed lines: lines 3-4\n    --- original\n    +++ updated\n    @@\n    -old\n    +new";
        let diff = parse_edit_diff(Some("edit_file"), text).expect("Muse edit diff");
        assert!(diff.starts_with("--- original\n"));
        assert!(diff.contains("-old\n+new"));
        assert!(parse_edit_diff(Some("read_file"), text).is_none());
    }

    #[test]
    fn bash_output_chunk_duplicates_the_tool_result_so_dedup_fires() {
        // The de-dup in render_task_node keys on tool_result.text == output
        // chunk. This proves that equality holds on the real capture, so the
        // command renders once, not twice.
        let tree = bash_tree();
        let node = tree
            .nodes()
            .find(|n| !n.tool_results.is_empty())
            .expect("bash node");
        let result_text = &node.tool_results[0].text;
        assert!(
            node.output.iter().any(|chunk| chunk == result_text),
            "the output chunk must be byte-identical to the tool result for de-dup to suppress it"
        );
    }

    #[test]
    fn contentless_reminder_scaffolding_is_hidden() {
        assert!(is_hidden_scaffolding(&node(Some(REMINDER))));
        assert!(is_hidden_scaffolding(&node(Some(
            "reminder.agent.plugin:tbh-reminders:goal-reminder"
        ))));
    }

    #[test]
    fn reminder_carrying_a_tool_result_is_kept() {
        // The reducer attributes tool results to a running scaffolding task in
        // every captured turn — hiding it would silently drop the tool output.
        let mut n = node(Some(REMINDER));
        n.tool_results.push(task_tree::ToolOutcome {
            call_id: "c1".to_string(),
            tool_name: Some("write_file".to_string()),
            outcome: Some("success".to_string()),
            text: "wrote hello.txt".to_string(),
            edit_path: Some("hello.txt".to_string()),
        });
        assert!(
            !is_hidden_scaffolding(&n),
            "a reminder with a tool result must render"
        );

        let mut with_output = node(Some(REMINDER));
        with_output.output.push("some streamed text".to_string());
        assert!(!is_hidden_scaffolding(&with_output));
    }

    #[test]
    fn real_tasks_and_unknowns_are_kept() {
        assert!(!is_hidden_scaffolding(&node(Some(
            "model.unknown.response"
        ))));
        assert!(!is_hidden_scaffolding(&node(None)));
        // Must START with `reminder.` — a kind that merely contains it stays.
        assert!(!is_hidden_scaffolding(&node(Some("agent.reminder.thing"))));
    }

    #[test]
    fn routine_policy_decisions_are_not_noteworthy() {
        // The two decisions muse stamps on every routine task (observed in
        // the captured fixtures) must stay off the card.
        assert!(!side_effect_is_noteworthy("allow:policy"));
        assert!(!side_effect_is_noteworthy("allow:user"));
        assert!(!side_effect_is_noteworthy("not_applicable"));
    }

    #[test]
    fn denials_and_unknown_decisions_stay_visible() {
        assert!(side_effect_is_noteworthy("deny:policy"));
        assert!(side_effect_is_noteworthy("ask:user"));
        // Unknown future vocabulary fails visible, including a missing value.
        assert!(side_effect_is_noteworthy("quarantine"));
        assert!(side_effect_is_noteworthy(""));
    }
}
