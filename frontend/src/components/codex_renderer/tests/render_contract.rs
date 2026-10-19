//! Group-part emptiness contract.
//!
//! Codex identity groups drop members whose content renders empty, so the
//! content path must report emptiness as `None` — otherwise each no-body
//! event (turn lifecycle markers, streaming deltas, the user-prompt echo)
//! leaves an empty `grouped-message-part`: a zero-height flex item the body
//! `gap` still spaces into a blank row.

use super::super::{render_codex_frame_content, CodexEvent};
use uuid::Uuid;

fn content_is_none(json: &str) -> bool {
    let event: CodexEvent = serde_json::from_str(json).unwrap();
    render_codex_frame_content(&event, Uuid::nil()).is_none()
}

#[test]
fn no_body_events_render_none_in_group() {
    for json in [
        r#"{"type":"thread.started","thread_id":"t"}"#,
        r#"{"type":"turn.started"}"#,
        r#"{"type":"item/reasoning/textDelta","delta":"x"}"#,
        r#"{"type":"item/reasoning/summaryPartAdded"}"#,
        r#"{"type":"item/plan/delta"}"#,
        r#"{"type":"some.future.event","data":1}"#,
        // The user-prompt echo item is rendered out-of-band, so it must not
        // leave a blank part in the middle of a Codex run.
        r#"{"type":"item.completed","item":{"type":"user_message","id":"u1","content":[{"type":"text","text":"hi"}]}}"#,
        // A reply opens with an agent message that has no text yet, beside a
        // reasoning item whose trace the model keeps private. Rendering them
        // as empty parts drew a "Codex × 2" header over a blank body until the
        // reply completed.
        r#"{"type":"item.started","item":{"id":"msg_1","phase":"commentary","text":"","type":"agentMessage"}}"#,
        r#"{"type":"item.completed","item":{"content":[],"id":"rs_1","summary":[],"type":"reasoning"}}"#,
        r#"{"type":"item.completed","item":{"content":["  "],"id":"rs_2","summary":["\n"],"type":"reasoning"}}"#,
        r#"{"type":"item.completed","item":{"type":"agent_message","id":"m2","text":"  \n"}}"#,
    ] {
        assert!(content_is_none(json), "expected None for: {json}");
    }
}

#[test]
fn content_bearing_events_render_some_in_group() {
    for json in [
        r#"{"type":"item.completed","item":{"type":"agent_message","id":"m1","text":"hello"}}"#,
        r#"{"type":"item.completed","item":{"type":"command_execution","id":"c1","command":"ls","status":"completed"}}"#,
        r#"{"type":"item.completed","item":{"type":"reasoning","id":"r1","text":"thinking"}}"#,
        // App-server reasoning with a trace renders it, collapsed.
        r#"{"type":"item.completed","item":{"content":[],"id":"rs_3","summary":["**Plan** read the tests"],"type":"reasoning"}}"#,
        r#"{"type":"item.completed","item":{"content":["raw reasoning"],"id":"rs_4","summary":[],"type":"reasoning"}}"#,
        // Private reasoning still in progress shows that the agent is working.
        r#"{"type":"item.started","item":{"content":[],"id":"rs_5","summary":[],"type":"reasoning"}}"#,
    ] {
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(
            render_codex_frame_content(&event, Uuid::nil()).is_some(),
            "expected Some for: {json}"
        );
    }
}

#[test]
fn app_server_reasoning_keeps_its_trace_and_lifecycle_id() {
    use super::super::events::codex_item_id;
    use super::super::CodexItem;
    use codex_codes::protocol::ThreadItem as AppServerThreadItem;

    let item: CodexItem = serde_json::from_str(
        r#"{"content":[],"id":"rs_9","summary":["first","second"],"type":"reasoning"}"#,
    )
    .unwrap();
    match &item {
        CodexItem::AppServer(inner) => match inner.as_ref() {
            AppServerThreadItem::Reasoning { summary, .. } => {
                assert_eq!(
                    summary.as_deref(),
                    Some(&["first".into(), "second".into()][..])
                )
            }
            other => panic!("expected app-server reasoning, got {other:?}"),
        },
        other => panic!("expected app-server reasoning, got {other:?}"),
    }
    assert_eq!(
        codex_item_id(&item),
        Some("rs_9"),
        "started/completed must dedup"
    );

    // The exec-level shape still parses as before.
    let exec: CodexItem =
        serde_json::from_str(r#"{"type":"reasoning","id":"r1","text":"hi"}"#).unwrap();
    assert!(matches!(exec, CodexItem::Thread(_)));
}
