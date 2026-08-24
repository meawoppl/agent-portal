//! `CodexEvent` deserialization: item-lifecycle composition, terminal-event
//! detection, and the turn-level / streaming-delta variants.

use super::super::events::CodexUsage;
use super::super::{codex_event_item_id, CodexEvent, CodexItem};
use crate::components::agent_frame::{AgentFrameKind, AgentFrameRegistry};
use codex_codes::io::items::{PatchChangeKind, ThreadItem};
use codex_codes::protocol::ThreadItem as AppServerThreadItem;

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
        item: Some(CodexItem::AppServer(item)),
    } = event
    else {
        panic!("expected ItemStarted{{ContextCompaction}}, got {:?}", event);
    };
    let AppServerThreadItem::ContextCompaction { id } = item.as_ref() else {
        panic!("expected ContextCompaction item, got {:?}", item);
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
        item: Some(CodexItem::AppServer(item)),
    } = event
    else {
        panic!(
            "expected ItemCompleted{{CollabAgentToolCall}}, got {:?}",
            event
        );
    };
    let AppServerThreadItem::CollabAgentToolCall {
        agents_states,
        id,
        model,
        prompt,
        reasoning_effort,
        receiver_thread_ids,
        sender_thread_id,
        status,
        tool,
    } = item.as_ref()
    else {
        panic!("expected CollabAgentToolCall item, got {:?}", item);
    };
    assert_eq!(id, "call_i1HC5jbTllWgsrMnJjqmRU05");
    assert_eq!(tool, &serde_json::Value::String("spawnAgent".to_string()));
    assert_eq!(model.as_deref(), Some("gpt-5.5"));
    assert_eq!(
        reasoning_effort.as_ref().map(|effort| effort.0.as_str()),
        Some("medium")
    );
    assert_eq!(status, &serde_json::Value::String("completed".to_string()));
    assert_eq!(sender_thread_id, "019ed195-44b1-77e0-a234-10307ce08eac");
    assert_eq!(
        receiver_thread_ids.as_slice(),
        ["019ed247-768f-7603-8c71-911fd841766e"]
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

// --- Terminal event detection via AgentFrameKind::is_terminator ---

#[test]
fn terminal_event_turn_completed() {
    let json = r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":20}}"#;
    let kind = AgentFrameRegistry::parse(json, shared::AgentType::Codex).kind();
    assert!(kind.is_terminator());
    assert_eq!(kind, AgentFrameKind::CodexTurnCompleted);
}

#[test]
fn terminal_event_turn_failed() {
    let json = r#"{"type":"turn.failed","error":{"message":"oops"}}"#;
    let kind = AgentFrameRegistry::parse(json, shared::AgentType::Codex).kind();
    assert!(kind.is_terminator());
    assert_eq!(kind, AgentFrameKind::CodexTurnFailed);
}

#[test]
fn terminal_event_item_completed_is_not_terminal() {
    let json = r#"{"type":"item.completed","item":{"type":"agent_message","id":"m1","text":"hi"}}"#;
    let kind = AgentFrameRegistry::parse(json, shared::AgentType::Codex).kind();
    assert!(!kind.is_terminator());
    assert_eq!(kind, AgentFrameKind::CodexItemCompleted);
}

#[test]
fn terminal_event_unknown_returns_none() {
    let json = r#"{"type":"something.else"}"#;
    let kind = AgentFrameRegistry::parse(json, shared::AgentType::Codex).kind();
    assert!(!kind.is_terminator());
    assert_eq!(kind, AgentFrameKind::RawJson);
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
