use codex_codes::{
    io::items::{FileUpdateChange, ThreadItem},
    protocol::ThreadItem as AppServerThreadItem,
};
use uuid::Uuid;
use yew::prelude::*;

mod events;
mod messages;
mod tools;
mod turns;
#[cfg(test)]
use events::thread_item_id;
#[cfg(test)]
use events::CodexUsage;
pub use events::{codex_event_item_id, is_codex_terminal_event, CodexEvent, CodexItem};
use events::{ContextCompactedParams, TurnPlanStep};
use messages::{
    render_agent_message, render_agent_message_content, render_error_block, render_reasoning,
};
use tools::{
    render_collab_agent_tool_call, render_command_execution, render_diff_card, render_file_change,
    render_mcp_tool_call, render_todo_list, render_web_search,
};
use turns::{render_turn_completed, render_turn_failed};

/// Render a parsed Codex frame as a standalone message card.
pub fn render_codex_frame(
    event: &CodexEvent,
    session_id: Uuid,
    turn_metrics: Option<&shared::TurnMetrics>,
) -> Html {
    render_codex_event(event, session_id, false, turn_metrics)
}

/// Render the content-only body for a parsed Codex frame inside an
/// `IdentityGroup`.
pub fn render_codex_frame_content(event: &CodexEvent, session_id: Uuid) -> Html {
    render_codex_event(event, session_id, true, None)
}

/// Single dispatcher over parsed `CodexEvent` for both the standalone card
/// path and the grouped content path. `bare_agent_message` selects the grouped
/// behavior: agent messages render content-only (no card chrome), while other
/// events render as they would standalone.
fn render_codex_event(
    event: &CodexEvent,
    session_id: Uuid,
    bare_agent_message: bool,
    turn_metrics: Option<&shared::TurnMetrics>,
) -> Html {
    match event {
        CodexEvent::ThreadStarted { .. } => html! {},
        CodexEvent::TurnStarted {} => html! {},
        CodexEvent::TurnCompleted {
            usage,
            duration_ms,
            turn_id,
            status,
        } => render_turn_completed(
            usage.as_ref(),
            *duration_ms,
            turn_id.as_deref(),
            status.as_deref(),
            turn_metrics,
        ),
        CodexEvent::TurnFailed { error } => render_turn_failed(error.as_ref(), turn_metrics),
        CodexEvent::ItemStarted { item } | CodexEvent::ItemUpdated { item } => {
            match item.as_ref() {
                Some(CodexItem::Thread(ThreadItem::AgentMessage(it))) if bare_agent_message => {
                    render_agent_message_content(&it.text, session_id)
                }
                item => render_item(item, false, session_id),
            }
        }
        CodexEvent::ItemCompleted { item } => match item.as_ref() {
            Some(CodexItem::Thread(ThreadItem::AgentMessage(it))) if bare_agent_message => {
                render_agent_message_content(&it.text, session_id)
            }
            item => render_item(item, true, session_id),
        },
        CodexEvent::Error { message } => render_error_block(message.as_deref()),
        CodexEvent::FileChangePatchUpdated { params } => {
            render_file_change_patch(params.as_ref().and_then(|p| p.changes.as_deref()))
        }
        CodexEvent::TurnPlanUpdated { params } => render_turn_plan(
            params.as_ref().and_then(|p| p.plan.as_deref()),
            params.as_ref().and_then(|p| p.explanation.as_deref()),
        ),
        CodexEvent::ThreadCompacted { params } => render_context_compacted(params.as_ref()),
        // Cumulative whole-turn diffs (`turn/diff/updated`) are dropped: Codex
        // re-sends the entire turn diff on every edit tick, so they pile up
        // O(ticks) redundant cards (each the size of the whole turn) on top of
        // the per-file `item.completed{file_change}` diffs that already render
        // the same edits. Dropped before grouping — see
        // `grouping::group_messages` — so they never reach this renderer in
        // practice; the no-op arm is kept for match exhaustiveness.
        CodexEvent::TurnDiffUpdated { .. }
        // Per-chunk deltas — the consolidated content lands in `turn/plan/updated`
        // (for plans) or `item.completed` (for reasoning). Emit nothing for the
        // streaming chunks to avoid visual noise without losing information.
        | CodexEvent::PlanDelta { .. }
        | CodexEvent::ReasoningSummaryPartAdded { .. }
        | CodexEvent::ReasoningTextDelta { .. }
        | CodexEvent::Unknown => html! {},
    }
}

/// CSS class set for any item-card wrapper. Adds `codex-item-in-progress` for
/// pre-completion (`item.started` / `item.updated`) renders so the stylesheet
/// can pulse the indicator and dim the text.
fn item_card_classes(completed: bool) -> &'static str {
    if completed {
        "claude-message assistant-message codex-item"
    } else {
        "claude-message assistant-message codex-item codex-item-in-progress"
    }
}

fn render_item(item: Option<&CodexItem>, completed: bool, session_id: Uuid) -> Html {
    let Some(item) = item else {
        return html! {};
    };
    match item {
        CodexItem::Thread(item) => match item {
            ThreadItem::AgentMessage(it) => render_agent_message(&it.text, completed, session_id),
            ThreadItem::Reasoning(it) => render_reasoning(&it.text, completed),
            ThreadItem::CommandExecution(it) => render_command_execution(it, completed),
            ThreadItem::FileChange(it) => render_file_change(it, completed),
            ThreadItem::McpToolCall(it) => render_mcp_tool_call(it, completed),
            ThreadItem::WebSearch(it) => render_web_search(&it.query, completed),
            ThreadItem::TodoList(it) => render_todo_list(&it.items, completed),
            ThreadItem::Error(it) => render_error_block(Some(&it.message)),
            // UserMessage is the user's prompt for the turn — emitted by the
            // app-server protocol as the first item; the portal renders the
            // user-typed prompt out-of-band (Claude wire shape), so suppress
            // here to avoid duplication.
            ThreadItem::UserMessage(_) => html! {},
        },
        CodexItem::AppServer(AppServerThreadItem::ContextCompaction { .. }) => {
            render_context_compaction_item(completed)
        }
        CodexItem::AppServer(AppServerThreadItem::CollabAgentToolCall {
            agents_states,
            model,
            prompt,
            reasoning_effort,
            status,
            tool,
            ..
        }) => render_collab_agent_tool_call(
            tool,
            model.as_deref(),
            reasoning_effort.as_ref(),
            status,
            prompt.as_deref(),
            agents_states,
            completed,
        ),
        CodexItem::AppServer(_) => html! {},
    }
}

fn render_file_change_patch(changes: Option<&[FileUpdateChange]>) -> Html {
    let changes = changes.unwrap_or(&[]);
    let cards: Vec<Html> = changes
        .iter()
        .filter(|c| !c.diff.trim().is_empty())
        .map(render_diff_card)
        .collect();
    if cards.is_empty() {
        return html! {};
    }
    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                { for cards.into_iter() }
            </div>
        </div>
    }
}

fn render_turn_plan(plan: Option<&[TurnPlanStep]>, explanation: Option<&str>) -> Html {
    let plan = plan.unwrap_or(&[]);
    let explanation = explanation.unwrap_or("");
    if plan.is_empty() && explanation.trim().is_empty() {
        return html! {};
    }
    html! {
        <div class="claude-message assistant-message">
            <div class="message-body">
                <div class="tool-use-section">
                    <div class="tool-use-header">
                        <span class="tool-icon">{ "\u{1f5d2}" }</span>
                        <span class="tool-name">{ "Plan" }</span>
                    </div>
                    {
                        if !explanation.trim().is_empty() {
                            html! { <div class="assistant-text">{ explanation }</div> }
                        } else {
                            html! {}
                        }
                    }
                    {
                        if !plan.is_empty() {
                            html! {
                                <div class="codex-todo-list">
                                    { for plan.iter().enumerate().map(|(i, step)| {
                                        let status = step.status.as_deref().unwrap_or("pending");
                                        let text = step.step.as_deref().unwrap_or("");
                                        let (marker, class) = match status {
                                            "completed" => ("\u{2611}", "codex-todo done"),
                                            "inProgress" | "in_progress" => ("\u{25b6}", "codex-todo"),
                                            _ => ("\u{2610}", "codex-todo"),
                                        };
                                        html! {
                                            <div class={class}>
                                                <span class="codex-todo-marker">{ marker }</span>
                                                <span class="codex-todo-text">
                                                    { format!("{}. {}", i + 1, text) }
                                                </span>
                                            </div>
                                        }
                                    })}
                                </div>
                            }
                        } else {
                            html! {}
                        }
                    }
                </div>
            </div>
        </div>
    }
}

fn render_context_compacted(params: Option<&ContextCompactedParams>) -> Html {
    let title = params
        .and_then(|p| p.turn_id.as_deref())
        .map(|turn_id| format!("Codex compacted context for turn {}", turn_id))
        .unwrap_or_else(|| "Codex compacted the conversation context".to_string());

    html! {
        <div class="claude-message compaction-message">
            <div class="message-header">
                <span class="message-type-badge compaction">{ "Context Compacted" }</span>
            </div>
            <div class="message-body">
                <div class="compaction-content">
                    <div class="compaction-icon">{ "\u{1f4e6}" }</div>
                    <div class="compaction-text">
                        <div class="compaction-description">{ title }</div>
                    </div>
                </div>
            </div>
        </div>
    }
}

fn render_context_compaction_item(completed: bool) -> Html {
    let title = if completed {
        "Codex compacted the conversation context"
    } else {
        "Codex is compacting the conversation context"
    };

    html! {
        <div class="claude-message compaction-message">
            <div class="message-header">
                <span class="message-type-badge compaction">{ "Context Compaction" }</span>
            </div>
            <div class="message-body">
                <div class="compaction-content">
                    <div class="compaction-icon">{ "\u{1f4e6}" }</div>
                    <div class="compaction-text">
                        <div class="compaction-description">{ title }</div>
                    </div>
                </div>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_codes::io::items::PatchChangeKind;

    // --- ThreadItem deserialization (replaces the pre-#827 local CodexItem) ---
    //
    // Both snake_case (`agent_message`) and camelCase (`agentMessage`) type
    // tags must parse cleanly — the codex exec protocol uses snake_case,
    // the app-server protocol uses camelCase, and the SDK accepts both.

    #[test]
    fn item_agent_message_snake_case() {
        let json = r#"{"type":"agent_message","id":"m1","text":"hello"}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, ThreadItem::AgentMessage(ref m) if m.text == "hello"));
    }

    #[test]
    fn item_reasoning_snake_case() {
        let json = r#"{"type":"reasoning","id":"r1","text":"thinking..."}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, ThreadItem::Reasoning(ref r) if r.text == "thinking..."));
    }

    #[test]
    fn item_command_execution_snake_case() {
        let json = r#"{"type":"command_execution","id":"c1","command":"ls","aggregated_output":"foo","exit_code":0,"status":"completed"}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(matches!(
            item,
            ThreadItem::CommandExecution(ref c)
                if c.command == "ls"
                && c.aggregated_output.as_deref() == Some("foo")
                && c.exit_code == Some(0)
        ));
    }

    /// #827 regression target — the exact wire shape the proxy forwards for
    /// `item.started{file_change}`: `kind` is the typed `{"type": "update"}`
    /// object, not a bare string. With the old local mirror this round-trip
    /// failed (kind: Option<String> couldn't deserialize the object) and the
    /// whole event silently dropped, leaving the permission dialog blind.
    #[test]
    fn item_file_change_snake_case_with_typed_kind_and_diff() {
        let json = r#"{"type":"file_change","id":"f1","changes":[{"path":"a.rs","kind":{"type":"update"},"diff":"@@ -1 +1 @@\n-a\n+b\n"}],"status":"completed"}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        let ThreadItem::FileChange(ref fc) = item else {
            panic!("expected FileChange variant, got {:?}", item);
        };
        assert_eq!(fc.changes.len(), 1);
        assert_eq!(fc.changes[0].path, "a.rs");
        assert!(matches!(fc.changes[0].kind, PatchChangeKind::Update { .. }));
        assert!(fc.changes[0].diff.contains("+b"));
    }

    #[test]
    fn item_mcp_tool_call_snake_case() {
        let json = r#"{"type":"mcp_tool_call","id":"mcp1","server":"srv","tool":"t","arguments":{},"status":"completed"}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(
            matches!(item, ThreadItem::McpToolCall(ref m) if m.server == "srv" && m.tool == "t")
        );
    }

    #[test]
    fn item_web_search_snake_case() {
        let json = r#"{"type":"web_search","id":"w1","query":"rust serde"}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, ThreadItem::WebSearch(ref w) if w.query == "rust serde"));
    }

    #[test]
    fn item_todo_list_snake_case() {
        let json =
            r#"{"type":"todo_list","id":"t1","items":[{"text":"fix bug","completed":false}]}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, ThreadItem::TodoList(ref t) if t.items.len() == 1));
    }

    #[test]
    fn item_error_snake_case() {
        let json = r#"{"type":"error","id":"e1","message":"oops"}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, ThreadItem::Error(ref e) if e.message == "oops"));
    }

    // --- camelCase aliases ---

    #[test]
    fn item_agent_message_camel_case() {
        let json = r#"{"type":"agentMessage","id":"m1","text":"hello"}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        assert!(matches!(item, ThreadItem::AgentMessage(ref m) if m.text == "hello"));
    }

    /// #827 regression target — camelCase variant of the typed-kind + diff
    /// shape (this is what the wire dump in the issue actually showed).
    #[test]
    fn item_file_change_camel_case_with_typed_kind_and_diff() {
        let json = r#"{"type":"fileChange","id":"call_abc","changes":[{"path":"/p/x.rs","kind":{"type":"update"},"diff":"@@ -1 +1 @@\n-a\n+b\n"}],"status":"inProgress"}"#;
        let item: ThreadItem = serde_json::from_str(json).unwrap();
        let ThreadItem::FileChange(ref fc) = item else {
            panic!("expected FileChange, got {:?}", item);
        };
        assert_eq!(fc.id, "call_abc");
        assert_eq!(fc.changes.len(), 1);
        assert_eq!(fc.changes[0].path, "/p/x.rs");
    }

    // --- CodexEvent ↔ ThreadItem composition ---

    #[test]
    fn event_item_completed_with_camel_case_item() {
        let json =
            r#"{"type":"item.completed","item":{"type":"agentMessage","id":"m1","text":"done"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            CodexEvent::ItemCompleted {
                item: Some(CodexItem::Thread(ThreadItem::AgentMessage(_)))
            }
        ));
    }

    #[test]
    fn event_item_updated_with_camel_case_command() {
        let json = r#"{"type":"item.updated","item":{"type":"commandExecution","id":"c1","command":"ls","aggregatedOutput":"out","exitCode":1,"status":"failed"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            CodexEvent::ItemUpdated {
                item: Some(CodexItem::Thread(ThreadItem::CommandExecution(ref c)))
            } if c.exit_code == Some(1)
        ));
    }

    /// #827 part 1 — the actual wire frame from the issue's bug report. With
    /// the old local mirror this parsed to `CodexEvent::Unknown` (the
    /// `CodexItem` deserialization failed on the typed `kind` object, so the
    /// outer event also failed and fell through `#[serde(other)]`) and
    /// rendered nothing in the transcript. Now it parses successfully into
    /// `ItemStarted { item: Some(FileChange { … }) }` and `render_item`
    /// dispatches into the diff card.
    #[test]
    fn event_item_started_with_file_change_no_longer_silently_drops() {
        // Verbatim from the issue's wire dump.
        let json = r#"{
            "_created_at": "2026-05-18T23:04:21.140Z",
            "item": {
                "changes": [{
                    "diff": "@@ -136,2 +136,3 @@\n     let hostname = props.session.hostname.clone();\n+    let session_agent_type = props.session.agent_type;\n",
                    "kind": {"type": "update"},
                    "path": "/home/meawoppl/repos/agent-portal-2/frontend/src/components/schedule_dialog.rs"
                }],
                "id": "call_apLovlbfsFz11MCYpiVcv0UK",
                "status": "inProgress",
                "type": "fileChange"
            },
            "type": "item.started"
        }"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        let CodexEvent::ItemStarted {
            item: Some(CodexItem::Thread(ThreadItem::FileChange(fc))),
        } = event
        else {
            panic!("expected ItemStarted{{FileChange}}, got {:?}", event);
        };
        assert_eq!(fc.changes.len(), 1);
        assert_eq!(
            fc.changes[0].path,
            "/home/meawoppl/repos/agent-portal-2/frontend/src/components/schedule_dialog.rs"
        );
        assert!(fc.changes[0].diff.contains("session_agent_type"));
    }

    /// #930 regression target — Codex emits compaction as an item lifecycle
    /// event whose item type is now typed in codex-codes' app-server model. It
    /// should parse through the SDK item and render via the compaction card,
    /// not fall through to the raw JSON renderer.
    #[test]
    fn event_item_started_context_compaction_no_longer_renders_raw() {
        let json = r#"{
            "_created_at": "2026-06-01T23:58:42.384Z",
            "item": {
                "id": "9edb35c0-6b6b-407f-84e3-d03a03050a2a",
                "type": "contextCompaction"
            },
            "type": "item.started"
        }"#;

        let event: CodexEvent = serde_json::from_str(json).unwrap();
        let CodexEvent::ItemStarted {
            item: Some(CodexItem::AppServer(AppServerThreadItem::ContextCompaction { ref id })),
        } = event
        else {
            panic!("expected ItemStarted{{ContextCompaction}}, got {:?}", event);
        };
        assert_eq!(id, "9edb35c0-6b6b-407f-84e3-d03a03050a2a");
        assert_eq!(
            codex_event_item_id(json).as_deref(),
            Some("9edb35c0-6b6b-407f-84e3-d03a03050a2a")
        );
    }

    /// agent-portal#1049 — Codex emits multi-agent `collabAgentToolCall`
    /// items (e.g. `spawnAgent`). They must parse through codex-codes'
    /// app-server `ThreadItem` variant and render through the spawn-agent card,
    /// not fall through to the raw JSON renderer.
    #[test]
    fn event_item_completed_collab_agent_tool_call() {
        let json = r#"{
            "type": "item.completed",
            "item": {
                "type": "collabAgentToolCall",
                "tool": "spawnAgent",
                "id": "call_i1HC5jbTllWgsrMnJjqmRU05",
                "model": "gpt-5.5",
                "reasoningEffort": "medium",
                "status": "completed",
                "senderThreadId": "019ed195-44b1-77e0-a234-10307ce08eac",
                "receiverThreadIds": ["019ed247-768f-7603-8c71-911fd841766e"],
                "agentsStates": {
                    "019ed247-768f-7603-8c71-911fd841766e": { "status": "pendingInit" }
                },
                "prompt": "In /home/... inspect the current main branch shape ..."
            }
        }"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        let CodexEvent::ItemCompleted {
            item:
                Some(CodexItem::AppServer(AppServerThreadItem::CollabAgentToolCall {
                    agents_states,
                    id,
                    model,
                    prompt,
                    reasoning_effort,
                    receiver_thread_ids,
                    sender_thread_id,
                    status,
                    tool,
                })),
        } = event
        else {
            panic!(
                "expected ItemCompleted{{CollabAgentToolCall}}, got {:?}",
                event
            );
        };
        assert_eq!(id, "call_i1HC5jbTllWgsrMnJjqmRU05");
        assert_eq!(tool, serde_json::Value::String("spawnAgent".to_string()));
        assert_eq!(model.as_deref(), Some("gpt-5.5"));
        assert_eq!(
            reasoning_effort.as_ref().map(|effort| effort.0.as_str()),
            Some("medium")
        );
        assert_eq!(status, serde_json::Value::String("completed".to_string()));
        assert_eq!(sender_thread_id, "019ed195-44b1-77e0-a234-10307ce08eac");
        assert_eq!(
            receiver_thread_ids,
            vec!["019ed247-768f-7603-8c71-911fd841766e".to_string()]
        );
        assert_eq!(agents_states.len(), 1);
        assert!(matches!(
            agents_states["019ed247-768f-7603-8c71-911fd841766e"].status,
            codex_codes::protocol::CollabAgentStatus::PendingInit
        ));
        assert!(prompt.as_deref().unwrap().contains("main branch"));

        assert_eq!(
            codex_event_item_id(json).as_deref(),
            Some("call_i1HC5jbTllWgsrMnJjqmRU05")
        );
    }

    #[test]
    fn event_unknown_type_falls_through() {
        let json = r#"{"type":"some.future.event","data":123}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, CodexEvent::Unknown));
    }

    // --- thread_item_id ---

    #[test]
    fn thread_item_id_extracts_id_per_variant() {
        let cases = [
            (r#"{"type":"agent_message","id":"m1","text":"x"}"#, "m1"),
            (r#"{"type":"reasoning","id":"r1","text":"x"}"#, "r1"),
            (
                r#"{"type":"command_execution","id":"c1","command":"x","status":"completed"}"#,
                "c1",
            ),
            (
                r#"{"type":"file_change","id":"f1","changes":[],"status":"completed"}"#,
                "f1",
            ),
            (r#"{"type":"web_search","id":"w1","query":"q"}"#, "w1"),
            (r#"{"type":"todo_list","id":"t1","items":[]}"#, "t1"),
            (r#"{"type":"error","id":"e1","message":"x"}"#, "e1"),
        ];
        for (json, expected_id) in cases {
            let item: ThreadItem = serde_json::from_str(json).unwrap();
            assert_eq!(thread_item_id(&item), expected_id, "json: {}", json);
        }
    }

    #[test]
    fn round_trip_codex_event() {
        let event = CodexEvent::TurnCompleted {
            usage: Some(CodexUsage {
                input_tokens: Some(100),
                cached_input_tokens: Some(50),
                output_tokens: Some(200),
                ..Default::default()
            }),
            duration_ms: Some(4200),
            turn_id: Some("turn-1".into()),
            status: Some("completed".into()),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: CodexEvent = serde_json::from_str(&json).unwrap();
        assert!(
            matches!(back, CodexEvent::TurnCompleted { usage: Some(ref u), duration_ms: Some(4200), .. } if u.output_tokens() == 200)
        );
    }

    // --- Terminal event detection ---

    #[test]
    fn terminal_event_turn_completed() {
        let json = r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":20}}"#;
        assert_eq!(is_codex_terminal_event(json), Some(true));
    }

    #[test]
    fn terminal_event_turn_failed() {
        let json = r#"{"type":"turn.failed","error":{"message":"oops"}}"#;
        assert_eq!(is_codex_terminal_event(json), Some(true));
    }

    #[test]
    fn terminal_event_item_completed_is_not_terminal() {
        let json =
            r#"{"type":"item.completed","item":{"type":"agent_message","id":"m1","text":"hi"}}"#;
        assert_eq!(is_codex_terminal_event(json), Some(false));
    }

    #[test]
    fn terminal_event_unknown_returns_none() {
        let json = r#"{"type":"something.else"}"#;
        assert_eq!(is_codex_terminal_event(json), None);
    }

    // --- Streaming-delta / plan / diff variants ---

    #[test]
    fn event_turn_diff_updated() {
        let json = r#"{"type":"turn/diff/updated","params":{"diff":"--- a/foo\n+++ b/foo\n@@ -1 +1 @@\n-bar\n+baz\n","threadId":"x","turnId":"y"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::TurnDiffUpdated { params: Some(p) } => {
                assert!(p.diff.as_deref().unwrap().contains("+baz"));
                assert_eq!(p.thread_id.as_deref(), Some("x"));
                assert_eq!(p.turn_id.as_deref(), Some("y"));
            }
            other => panic!("expected TurnDiffUpdated, got {:?}", other),
        }
    }

    #[test]
    fn event_file_change_patch_updated_camel_case() {
        // The wire here matches the *upstream* FileUpdateChange shape:
        // kind is the typed `{"type": "update"}` object, path/diff are
        // strings (not Option<String>). Pre-#827 the local mirror accepted
        // `"kind":"update"` as a bare string, but the wire never actually
        // shipped that shape — upstream's doc explicitly notes it.
        let json = r#"{"type":"item/fileChange/patchUpdated","params":{"changes":[{"path":"a.rs","kind":{"type":"update"},"diff":"--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n"}],"itemId":"i","threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::FileChangePatchUpdated { params: Some(p) } => {
                let changes = p.changes.unwrap();
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].path, "a.rs");
                assert!(matches!(changes[0].kind, PatchChangeKind::Update { .. }));
                assert!(changes[0].diff.contains("+new"));
            }
            other => panic!("expected FileChangePatchUpdated, got {:?}", other),
        }
    }

    #[test]
    fn event_turn_plan_updated() {
        let json = r#"{"type":"turn/plan/updated","params":{"plan":[{"status":"completed","step":"first"},{"status":"inProgress","step":"second"}],"explanation":"so far","threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::TurnPlanUpdated { params: Some(p) } => {
                let plan = p.plan.unwrap();
                assert_eq!(plan.len(), 2);
                assert_eq!(plan[0].status.as_deref(), Some("completed"));
                assert_eq!(plan[1].status.as_deref(), Some("inProgress"));
                assert_eq!(p.explanation.as_deref(), Some("so far"));
            }
            other => panic!("expected TurnPlanUpdated, got {:?}", other),
        }
    }

    #[test]
    fn event_thread_compacted() {
        let json = r#"{"type":"thread/compacted","params":{"threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        match event {
            CodexEvent::ThreadCompacted { params: Some(p) } => {
                assert_eq!(p.thread_id.as_deref(), Some("t"));
                assert_eq!(p.turn_id.as_deref(), Some("u"));
            }
            other => panic!("expected ThreadCompacted, got {:?}", other),
        }
    }

    #[test]
    fn event_plan_delta_typed_no_op() {
        let json = r#"{"type":"item/plan/delta","params":{"delta":"chunk","itemId":"i","threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, CodexEvent::PlanDelta { .. }));
    }

    #[test]
    fn event_reasoning_summary_part_added_typed_no_op() {
        let json = r#"{"type":"item/reasoning/summaryPartAdded","params":{"itemId":"i","summaryIndex":0,"threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(
            event,
            CodexEvent::ReasoningSummaryPartAdded { .. }
        ));
    }

    #[test]
    fn event_reasoning_text_delta_typed_no_op() {
        let json = r#"{"type":"item/reasoning/textDelta","params":{"contentIndex":0,"delta":"...","itemId":"i","threadId":"t","turnId":"u"}}"#;
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(matches!(event, CodexEvent::ReasoningTextDelta { .. }));
    }
}
