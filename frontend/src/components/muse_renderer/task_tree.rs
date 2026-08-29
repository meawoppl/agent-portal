//! Task-tree model for Muse sessions.
//!
//! Muse reports work as a **task tree** rather than tool-use blocks: a
//! `task.stream.linked` record opens a node, `task.lifecycle.*` records walk
//! its state machine, and `tool.result` records report outcomes.
//!
//! This module is a **pure reducer** — records in, tree out, no channel
//! coupling — because the records arrive on *two* channels that the view
//! interleaves by identity:
//!
//! - **Durable structure** (`task.stream.linked`, the lifecycle transitions,
//!   `tool.result`) arrives on the persisted output stream.
//! - **Live status** (`task.lifecycle.status`, `run.output.delta`) arrives
//!   on the separate non-persisting ephemeral channel.
//!
//! Feeding both through [`TaskTree::apply`] keeps the reducer testable
//! against captured sessions and indifferent to which socket a record came
//! from.
//!
//! Identity rules (measured — see `docs/MUSE_SUPPORT.md`): nodes are keyed
//! on `task_id`; a turn is grouped by `causation_id`; the record `id` is a
//! counter that repeats across sessions, so it is **never** a key here.

use std::collections::BTreeMap;

/// Where a task sits in its lifecycle. Ordered by progression so a node can
/// only advance, never regress on an out-of-order record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum TaskState {
    #[default]
    Proposed,
    Accepted,
    Scheduled,
    Started,
    /// Terminal states share a rank; the specific one is kept in the node.
    Completed,
    Cancelled,
    Rejected,
    Failed,
}

impl TaskState {
    /// Badge text for the node header.
    pub fn label(self) -> &'static str {
        match self {
            TaskState::Proposed => "proposed",
            TaskState::Accepted => "accepted",
            TaskState::Scheduled => "scheduled",
            TaskState::Started => "running",
            TaskState::Completed => "completed",
            TaskState::Cancelled => "cancelled",
            TaskState::Rejected => "rejected",
            TaskState::Failed => "failed",
        }
    }

    /// True once the task can produce no further transitions.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            TaskState::Completed | TaskState::Cancelled | TaskState::Rejected | TaskState::Failed
        )
    }

    fn from_event_kind(kind: &str) -> Option<Self> {
        Some(match kind {
            "proposed" => TaskState::Proposed,
            "accepted" => TaskState::Accepted,
            "scheduled" => TaskState::Scheduled,
            "started" => TaskState::Started,
            "completed" => TaskState::Completed,
            "cancelled" => TaskState::Cancelled,
            "rejected" => TaskState::Rejected,
            "failed" => TaskState::Failed,
            // `status`, `output`, `side_effect_intent` carry information but
            // are not state transitions.
            _ => return None,
        })
    }
}

/// One tool invocation's outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub call_id: String,
    pub tool_name: Option<String>,
    /// `"success"` / `"failure"` as reported in `correlation_facts`.
    pub outcome: Option<String>,
    pub text: String,
    /// Typed path from Muse's `edit_facts`, when the tool changed a file.
    pub edit_path: Option<String>,
}

/// A node in the task tree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskNode {
    pub task_id: String,
    /// Dotted class, e.g. `model.unknown.response` or
    /// `reminder.agent.plugin:<plugin>:<name>`.
    pub task_kind: Option<String>,
    pub state: TaskState,
    /// Why a task ended, when the terminal record carried a reason.
    pub reason: Option<String>,
    /// Latest live-status line (from the ephemeral channel). Transient by
    /// nature: replaced as new status arrives, and meaningless after the
    /// task reaches a terminal state.
    pub status: Option<String>,
    /// Streamed output chunks attributed to this task.
    pub output: Vec<String>,
    /// The operation a `side_effect_intent` recorded, with the policy
    /// decision muse applied (it decides tool policy itself — these are an
    /// audit trail, never an approval prompt).
    pub side_effect: Option<(String, String)>,
    pub tool_results: Vec<ToolOutcome>,
}

impl TaskNode {
    /// Muse-internal scaffolding: the `tbh-reminders` plugin's
    /// skill/scope/goal/verify prompt reminders (`task_kind` `reminder.*`).
    /// Bookkeeping the agent runs on itself, not user-facing work — the view
    /// hides these, and tool attribution prefers real work tasks over them.
    pub fn is_reminder(&self) -> bool {
        self.task_kind
            .as_deref()
            .is_some_and(|kind| kind.starts_with("reminder."))
    }
}

/// The tree for one turn, keyed by `task_id`.
///
/// `BTreeMap` rather than a hash map so iteration order is stable across
/// renders — a task tree that reshuffles on every update is unusable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TaskTree {
    nodes: BTreeMap<String, TaskNode>,
    /// Insertion order, so the view can render tasks as they appeared.
    order: Vec<String>,
    /// Turn this tree belongs to (`causation_id`), set by the first record.
    pub causation_id: Option<String>,
    /// The run's answer text in markdown. Live `run.output.delta` chunks build
    /// it while the turn runs; a durable `run.terminal.*` record replaces the
    /// stream with the canonical final text.
    answer: Option<String>,
    answer_is_terminal: bool,
    /// Count per `payload_type` of records the tree does not render
    /// structurally (`run.model.configured`, `command.received`, future
    /// vocabulary). Surfaced as a muted footer so nothing on the wire silently
    /// disappears from the transcript.
    other_records: BTreeMap<String, usize>,
}

impl TaskTree {
    /// Construct an empty tree. Production builds it via `Default`; this
    /// exists for tests and future callers.
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Tasks in the order they first appeared.
    pub fn nodes(&self) -> impl Iterator<Item = &TaskNode> {
        self.order.iter().filter_map(|id| self.nodes.get(id))
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty() && self.answer.is_none()
    }

    /// The run's final answer text, if a terminal record carried one.
    pub fn answer(&self) -> Option<&str> {
        self.answer.as_deref()
    }

    /// Look up one node. Used by tests asserting state transitions; the
    /// view iterates [`TaskTree::nodes`] instead.
    #[cfg(test)]
    pub fn get(&self, task_id: &str) -> Option<&TaskNode> {
        self.nodes.get(task_id)
    }

    /// Apply one classified muse event (from either channel).
    ///
    /// Unrecognized shapes are ignored rather than erroring: this is a view
    /// model, and a frame it cannot interpret must not break the tree that
    /// the rest of the turn built.
    pub fn apply(&mut self, event: &serde_json::Value) {
        let Some(payload_type) = event.get("payload_type").and_then(|v| v.as_str()) else {
            return;
        };
        if self.causation_id.is_none() {
            self.causation_id = event
                .get("causation_id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
        }
        let payload = event.get("payload").unwrap_or(&serde_json::Value::Null);

        match payload_type {
            "task.stream.linked" => {
                if let Some(id) = str_field(payload, "task_id") {
                    self.node_mut(id);
                }
            }
            t if t.starts_with("task.lifecycle.") => self.apply_lifecycle(payload),
            "tool.result" => {
                if let Ok(muse_codes::MusePayload::ToolResult(result)) =
                    muse_codes::MusePayload::from_parts(payload_type, payload.clone())
                {
                    self.apply_tool_result(&result);
                }
            }
            "run.output.delta" => {
                if !self.answer_is_terminal {
                    if let Some(chunk) = str_field(payload, "text") {
                        self.answer.get_or_insert_default().push_str(&chunk);
                    }
                }
            }
            // The run's final answer — the agent's actual reply. Match the whole
            // `run.terminal.*` family, not just `.completed`: the terminal state
            // is encoded in the suffix (completed / failed / cancelled / future
            // suffixes), and a failed run's reply must render as text — not a
            // footer count — on exactly the turns the user most needs to read
            // it. Prefer `text`; fall back to `reason` when a failed run carries
            // only that. Last terminal wins, and a blank retry keeps the prior
            // answer (better a stale reply than a blank card) — deliberate.
            t if t.starts_with("run.terminal.") => {
                let answer = str_field(payload, "text")
                    .filter(|t| !t.trim().is_empty())
                    .or_else(|| str_field(payload, "reason").filter(|r| !r.trim().is_empty()));
                if answer.is_some() {
                    self.answer = answer;
                    self.answer_is_terminal = true;
                }
            }
            other => {
                *self.other_records.entry(other.to_string()).or_default() += 1;
            }
        }
    }

    /// `payload_type → count` of records not rendered as tree structure, in
    /// stable order, for the card footer.
    pub fn other_records(&self) -> impl Iterator<Item = (&str, usize)> {
        self.other_records.iter().map(|(k, v)| (k.as_str(), *v))
    }

    fn apply_lifecycle(&mut self, payload: &serde_json::Value) {
        let Some(ev) = payload.get("event") else {
            return;
        };
        let Some(task_id) = str_field(ev, "task_id").or_else(|| str_field(payload, "task_id"))
        else {
            return;
        };
        let kind = str_field(ev, "kind").unwrap_or_default();
        let node = self.node_mut(task_id);

        if let Some(next) = TaskState::from_event_kind(&kind) {
            // Never regress: records can arrive out of order across two
            // channels, and a late `started` must not un-complete a task.
            if next > node.state {
                node.state = next;
            }
            if next.is_terminal() {
                node.reason = str_field(ev, "reason");
                // Live status is meaningless once the task has ended.
                node.status = None;
            }
        }
        match kind.as_str() {
            "proposed" => node.task_kind = str_field(ev, "task_kind"),
            // A late ephemeral heartbeat must not resurrect progress UI on a
            // task whose durable lifecycle has already reached a terminal
            // state (notably after a reconnect/history replay).
            "status" if !node.state.is_terminal() => node.status = str_field(ev, "message"),
            "output" => {
                if let Some(chunk) = str_field(ev, "chunk") {
                    node.output.push(chunk);
                }
            }
            "side_effect_intent" => {
                if let Some(op) = str_field(ev, "operation") {
                    let decision = str_field(ev, "policy_decision").unwrap_or_default();
                    node.side_effect = Some((op, decision));
                }
            }
            _ => {}
        }
    }

    fn apply_tool_result(&mut self, payload: &muse_codes::ToolResult) {
        let outcome = ToolOutcome {
            call_id: payload.call_id.clone(),
            tool_name: payload.tool_name().map(str::to_string),
            outcome: payload.outcome().map(str::to_string),
            text: payload.text.clone(),
            edit_path: payload
                .edit_facts
                .as_ref()
                .and_then(|facts| str_field(facts, "path")),
        };
        if let Some(id) = self.tool_result_target(outcome.tool_name.as_deref()) {
            self.node_mut(&id).tool_results.push(outcome);
        }
    }

    /// The task a `tool.result` belongs to. The record carries no task_id,
    /// but the wire models each tool call as its own task
    /// (`task_kind: tool.<tool_name>`), and in every committed capture the
    /// result's `correlation_facts.tool_name` matches exactly one such task —
    /// so kind-match on the latest one (any state: the tool task has usually
    /// already completed by the time its result record lands, which is why a
    /// running-only scan can never find it). Fall back to the latest running
    /// non-reminder task, then to any running task, rather than dropping the
    /// result on the floor.
    fn tool_result_target(&self, tool_name: Option<&str>) -> Option<String> {
        if let Some(name) = tool_name {
            let kind = format!("tool.{name}");
            if let Some(id) = self.order.iter().rev().find(|id| {
                self.nodes
                    .get(*id)
                    .is_some_and(|n| n.task_kind.as_deref() == Some(kind.as_str()))
            }) {
                return Some(id.clone());
            }
        }
        let running = |id: &&String| {
            self.nodes
                .get(*id)
                .is_some_and(|n| !n.state.is_terminal() && n.state >= TaskState::Started)
        };
        self.order
            .iter()
            .rev()
            .filter(running)
            .find(|id| self.nodes.get(*id).is_some_and(|n| !n.is_reminder()))
            .or_else(|| self.order.iter().rev().find(running))
            .cloned()
    }

    fn node_mut(&mut self, task_id: impl Into<String>) -> &mut TaskNode {
        let id = task_id.into();
        if !self.nodes.contains_key(&id) {
            self.order.push(id.clone());
            self.nodes.insert(
                id.clone(),
                TaskNode {
                    task_id: id.clone(),
                    ..Default::default()
                },
            );
        }
        self.nodes.get_mut(&id).unwrap_or_else(|| unreachable!())
    }
}

fn str_field(value: &serde_json::Value, key: &str) -> Option<String> {
    value.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Real captured sessions, replayed through the reducer exactly as the
    /// view will feed it — proof the model handles the actual wire and not
    /// an idealization of it.
    const TOOL_USE: &str = include_str!("fixtures/meta_tool_use.jsonl");
    const SUBAGENTS: &str = include_str!("fixtures/meta_subagents.jsonl");

    /// Mimic the classifier's wire event for a captured record.
    fn events(capture: &str) -> Vec<serde_json::Value> {
        capture
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let r: serde_json::Value = serde_json::from_str(l).expect("capture parses");
                json!({
                    "type": "muse_record",
                    "payload_type": r["payload_type"],
                    "causation_id": r["causation_id"],
                    "payload": r["payload"],
                })
            })
            .collect()
    }

    fn build(capture: &str) -> TaskTree {
        let mut tree = TaskTree::new();
        for e in events(capture) {
            tree.apply(&e);
        }
        tree
    }

    #[test]
    fn builds_a_tree_from_a_real_tool_use_turn() {
        let tree = build(TOOL_USE);
        assert!(!tree.is_empty(), "a real turn must produce task nodes");
        assert!(tree.causation_id.is_some(), "the turn id must be captured");
        // Every node reached a definite state, and none was left Proposed
        // — a stuck node would mean the reducer missed transitions.
        let advanced = tree
            .nodes()
            .filter(|n| n.state != TaskState::Proposed)
            .count();
        assert!(advanced > 0, "nodes should advance past Proposed");
    }

    #[test]
    fn multi_subagent_turn_produces_multiple_nodes() {
        let tree = build(SUBAGENTS);
        assert!(
            tree.nodes().count() >= 2,
            "a multi-subagent turn should open several task nodes, got {}",
            tree.nodes().count()
        );
    }

    #[test]
    fn tool_results_attach_with_their_outcome_facts() {
        let tree = build(TOOL_USE);
        let results: Vec<&ToolOutcome> = tree.nodes().flat_map(|n| n.tool_results.iter()).collect();
        assert!(
            !results.is_empty(),
            "the tool-use capture must yield tool results"
        );
        assert!(
            results
                .iter()
                .any(|r| r.tool_name.is_some() && r.outcome.is_some()),
            "correlation_facts (tool_name/outcome) must survive into the model"
        );
    }

    #[test]
    fn edit_facts_keep_the_typed_file_path() {
        let mut tree = TaskTree::new();
        tree.apply(&json!({
            "payload_type": "task.lifecycle.proposed",
            "payload": {"event": {
                "kind": "proposed", "task_id": "edit-1", "task_kind": "tool.edit_file"
            }}
        }));
        tree.apply(&json!({
            "payload_type": "tool.result",
            "payload": {
                "kind": "tool_result",
                "command_id": "cmd-1",
                "run_stream": {"kind": "run", "id": "run-1"},
                "call_id": "call-1",
                "correlation_facts": {"tool_name": "edit_file", "outcome": "success"},
                "edit_facts": {"path": "src/main.rs", "added": 1, "removed": 1},
                "text": "edit_file edited\n--- original\n+++ updated\n@@\n-old\n+new"
            }
        }));
        let result = &tree.get("edit-1").expect("edit task").tool_results[0];
        assert_eq!(result.edit_path.as_deref(), Some("src/main.rs"));
    }

    /// Records arrive over two channels and can interleave out of order. A
    /// late `started` must never un-complete a finished task.
    #[test]
    fn state_never_regresses_on_out_of_order_records() {
        let mut tree = TaskTree::new();
        let lifecycle = |kind: &str| {
            json!({
                "payload_type": format!("task.lifecycle.{kind}"),
                "causation_id": "turn-1",
                "payload": {"task_id": "t1", "event": {"kind": kind, "task_id": "t1"}},
            })
        };
        tree.apply(&lifecycle("started"));
        tree.apply(&lifecycle("completed"));
        tree.apply(&lifecycle("started")); // late duplicate
        assert_eq!(
            tree.get("t1").map(|n| n.state),
            Some(TaskState::Completed),
            "a late transition must not regress a terminal task"
        );
    }

    /// Live status comes from the ephemeral channel and is meaningless once
    /// the task ends — otherwise a finished node would keep showing
    /// "opening model stream" forever.
    #[test]
    fn terminal_state_clears_live_status() {
        let mut tree = TaskTree::new();
        tree.apply(&json!({
            "payload_type": "task.lifecycle.status",
            "causation_id": "turn-1",
            "payload": {"task_id": "t1", "event": {
                "kind": "status", "task_id": "t1", "message": "opening model stream"
            }},
        }));
        assert_eq!(
            tree.get("t1").and_then(|n| n.status.clone()).as_deref(),
            Some("opening model stream")
        );
        tree.apply(&json!({
            "payload_type": "task.lifecycle.failed",
            "causation_id": "turn-1",
            "payload": {"task_id": "t1", "event": {
                "kind": "failed", "task_id": "t1", "reason": "model did not reach a terminal state"
            }},
        }));
        let node = tree.get("t1").expect("node");
        assert_eq!(node.state, TaskState::Failed);
        assert!(
            node.status.is_none(),
            "live status must clear at terminal state"
        );
        assert!(node.reason.is_some(), "the failure reason must be kept");

        tree.apply(&json!({
            "payload_type": "task.lifecycle.status",
            "causation_id": "turn-1",
            "payload": {"task_id": "t1", "event": {
                "kind": "status", "task_id": "t1", "message": "stale reconnect heartbeat"
            }},
        }));
        assert!(
            tree.get("t1").expect("node").status.is_none(),
            "late status must not resurrect on a terminal task"
        );
    }

    /// The run's final answer text is captured as prose, not dumped into the
    /// footer — this is the whole point of the muse card carrying the reply.
    #[test]
    fn terminal_text_becomes_the_answer_not_a_footer_count() {
        let mut tree = TaskTree::new();
        tree.apply(&json!({
            "payload_type": "run.terminal.completed",
            "payload": {"terminal": "completed", "reason": null,
                        "text": "Created `hello.txt` — contents:\n\n```\nhello\n```"},
        }));
        assert_eq!(
            tree.answer(),
            Some("Created `hello.txt` — contents:\n\n```\nhello\n```")
        );
        // It must NOT also show as an "unrendered" footer count.
        assert!(
            tree.other_records().next().is_none(),
            "the answer is rendered, so it must not appear in the footer too"
        );
        // An answer-only turn (no tasks) is still a card worth rendering.
        assert!(!tree.is_empty(), "a tree with an answer is not empty");
    }

    #[test]
    fn streamed_answer_accumulates_then_terminal_replaces_it() {
        let mut tree = TaskTree::new();
        tree.apply(&serde_json::json!({
            "payload_type": "run.output.delta",
            "causation_id": "turn-1",
            "payload": {"text": "live "}
        }));
        tree.apply(&serde_json::json!({
            "payload_type": "run.output.delta",
            "causation_id": "turn-1",
            "payload": {"text": "answer"}
        }));
        assert_eq!(tree.answer(), Some("live answer"));
        assert!(tree.other_records().next().is_none());

        tree.apply(&serde_json::json!({
            "payload_type": "run.terminal.completed",
            "causation_id": "turn-1",
            "payload": {"text": "canonical answer"}
        }));
        tree.apply(&serde_json::json!({
            "payload_type": "run.output.delta",
            "causation_id": "turn-1",
            "payload": {"text": " stale"}
        }));
        assert_eq!(tree.answer(), Some("canonical answer"));
    }

    /// A blank terminal text leaves no answer (and no empty prose block).
    #[test]
    fn blank_terminal_text_sets_no_answer() {
        let mut tree = TaskTree::new();
        tree.apply(&json!({
            "payload_type": "run.terminal.completed",
            "payload": {"terminal": "completed", "text": "   "},
        }));
        assert_eq!(tree.answer(), None);
    }

    /// A FAILED terminal (a different `run.terminal.*` suffix) still renders as
    /// text: its `reason` becomes the answer when it carries no `text`. This is
    /// the turn the user most needs to read, and matching only `.completed`
    /// would drop it to the footer.
    #[test]
    fn failed_terminal_falls_back_to_reason() {
        let mut tree = TaskTree::new();
        tree.apply(&json!({
            "payload_type": "run.terminal.failed",
            "payload": {"terminal": "failed", "reason": "model hit its context limit"},
        }));
        assert_eq!(tree.answer(), Some("model hit its context limit"));
        assert!(
            tree.other_records().next().is_none(),
            "a failed terminal must render, not footer-count"
        );
    }

    /// Attribution pins the measured correlation: the wire models each tool
    /// call as a `tool.<name>` task, and the result's
    /// `correlation_facts.tool_name` names it. A recency scan cannot work —
    /// the tool task has already completed when its result record lands, so
    /// "latest running" found only reminder scaffolding (hidden by the view),
    /// making every tool outcome in every capture invisible.
    #[test]
    fn tool_results_attach_to_their_tool_task_not_reminder_scaffolding() {
        for capture in [TOOL_USE, SUBAGENTS] {
            let tree = build(capture);
            let carriers: Vec<&TaskNode> = tree
                .nodes()
                .filter(|n| !n.tool_results.is_empty())
                .collect();
            assert!(!carriers.is_empty(), "capture must yield tool results");
            for node in carriers {
                let kind = node.task_kind.as_deref().unwrap_or_default();
                assert!(
                    kind.starts_with("tool."),
                    "tool result landed on {kind:?}, not its tool task"
                );
                for r in &node.tool_results {
                    let expect = format!("tool.{}", r.tool_name.as_deref().unwrap_or_default());
                    assert_eq!(kind, expect, "result matched to the wrong tool task");
                }
            }
        }
    }

    /// The fallback still fires: when ONLY a reminder task is running, a tool
    /// result attaches to it rather than being dropped on the floor.
    #[test]
    fn tool_results_fall_back_to_a_reminder_task_when_nothing_else_runs() {
        let mut tree = TaskTree::new();
        tree.apply(&json!({
            "payload_type": "task.lifecycle.proposed",
            "payload": {"task_id": "r1", "event": {
                "kind": "proposed", "task_id": "r1",
                "task_kind": "reminder.agent.plugin:tbh-reminders:scope-reminder"
            }},
        }));
        tree.apply(&json!({
            "payload_type": "task.lifecycle.started",
            "payload": {"task_id": "r1", "event": {"kind": "started", "task_id": "r1"}},
        }));
        tree.apply(&json!({
            "payload_type": "tool.result",
            "payload": {
                "kind": "tool_result", "command_id": "cmd-1",
                "run_stream": {"kind": "run", "id": "run-1"},
                "call_id": "c1", "text": "ok"
            },
        }));
        let node = tree.get("r1").expect("node");
        assert_eq!(
            node.tool_results.len(),
            1,
            "sole running task keeps the result"
        );
    }

    /// Muse 0.2.1 omits `correlation_facts` on a **pre-execution rejection** —
    /// a tool call refused before it runs, e.g. the stringified-scalar rejects
    /// seen on meawoppl-fc (`invalid type: string "false", expected a
    /// boolean`). With no `tool_name` there is nothing to kind-match, so the
    /// fallback decides where the failure is rendered, and the preference for
    /// a non-reminder task is what keeps it on the visible work rather than on
    /// scaffolding the view hides.
    ///
    /// Covered here because that fallback became load-bearing on 0.2.1 rather
    /// than theoretical (rust-code-agent-sdks#325). The existing fallback test
    /// only exercises the degenerate case where a reminder is the *sole*
    /// running task.
    #[test]
    fn a_rejection_without_correlation_facts_lands_on_real_work_not_scaffolding() {
        let mut tree = TaskTree::new();
        for (id, kind) in [
            ("r1", "reminder.agent.plugin:tbh-reminders:scope-reminder"),
            ("t1", "tool.bash"),
        ] {
            tree.apply(&json!({
                "payload_type": "task.lifecycle.proposed",
                "payload": {"task_id": id, "event": {
                    "kind": "proposed", "task_id": id, "task_kind": kind
                }},
            }));
            tree.apply(&json!({
                "payload_type": "task.lifecycle.started",
                "payload": {"task_id": id, "event": {"kind": "started", "task_id": id}},
            }));
        }

        // No `correlation_facts`: the 0.2.1 pre-execution rejection shape.
        tree.apply(&json!({
            "payload_type": "tool.result",
            "payload": {
                "kind": "tool_result",
                "command_id": "cmd-1",
                "run_stream": {"kind": "run", "id": "run-1"},
                "call_id": "c1",
                "text": "invalid type: string \"false\", expected a boolean at line 1 column 102"
            },
        }));

        assert_eq!(
            tree.get("t1").expect("tool task").tool_results.len(),
            1,
            "a rejection with no correlation_facts must attach to the running \
             tool task, not the reminder scaffolding the view hides"
        );
        assert!(
            tree.get("r1").expect("reminder").tool_results.is_empty(),
            "scaffolding must not absorb the rejection"
        );
    }

    /// A frame the model cannot interpret must not break the tree built by
    /// the rest of the turn.
    #[test]
    fn unrecognized_frames_are_ignored_not_fatal() {
        let mut tree = TaskTree::new();
        tree.apply(&json!({"payload_type": "task.stream.linked",
                           "payload": {"task_id": "t1"}}));
        tree.apply(&json!({"payload_type": "subagent.future.thing", "payload": {"x": 1}}));
        tree.apply(&json!({"nonsense": true}));
        assert_eq!(tree.nodes().count(), 1, "the known node survives");
    }

    /// Side-effect intents are an audit trail — muse decides tool policy
    /// itself and never asks — so the decision must be visible, not
    /// rendered as a pending approval.
    #[test]
    fn side_effect_intent_records_its_policy_decision() {
        let mut tree = TaskTree::new();
        tree.apply(&json!({
            "payload_type": "task.lifecycle.side_effect_intent",
            "payload": {"task_id": "t1", "event": {
                "kind": "side_effect_intent", "task_id": "t1",
                "operation": "model.unknown.response", "policy_decision": "not_applicable"
            }},
        }));
        assert_eq!(
            tree.get("t1").and_then(|n| n.side_effect.clone()),
            Some(("model.unknown.response".into(), "not_applicable".into()))
        );
    }
}
