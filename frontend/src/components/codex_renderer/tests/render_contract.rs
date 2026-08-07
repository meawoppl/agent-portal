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
    ] {
        let event: CodexEvent = serde_json::from_str(json).unwrap();
        assert!(
            render_codex_frame_content(&event, Uuid::nil()).is_some(),
            "expected Some for: {json}"
        );
    }
}
