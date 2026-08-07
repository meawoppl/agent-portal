//! `ThreadItem` deserialization — one case per item family.
//!
//! Both snake_case (`agent_message`) and camelCase (`agentMessage`) type
//! tags must parse cleanly — the codex exec protocol uses snake_case,
//! the app-server protocol uses camelCase, and the SDK accepts both.

use super::super::events::thread_item_id;
use codex_codes::io::items::{PatchChangeKind, ThreadItem};

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
    assert!(matches!(item, ThreadItem::McpToolCall(ref m) if m.server == "srv" && m.tool == "t"));
}

#[test]
fn item_web_search_snake_case() {
    let json = r#"{"type":"web_search","id":"w1","query":"rust serde"}"#;
    let item: ThreadItem = serde_json::from_str(json).unwrap();
    assert!(matches!(item, ThreadItem::WebSearch(ref w) if w.query == "rust serde"));
}

#[test]
fn item_todo_list_snake_case() {
    let json = r#"{"type":"todo_list","id":"t1","items":[{"text":"fix bug","completed":false}]}"#;
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
