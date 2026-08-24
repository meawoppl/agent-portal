//! Cross-family classifier contracts: turn-terminator detection and the
//! one-row-per-realistic-message-kind sweep that guards every category.

use super::super::grouping::{classify, group_is_turn_terminator, GroupCategory, MessageGroup};
use super::fixtures::{
    assistant_with_tool_use, codex_item_started_agent_message, plain_user_text,
    portal_text_message, read_tool_result_user_message, rendered, result_message,
    thinking_tokens_message,
};

#[test]
fn turn_terminator_detection_covers_claude_and_codex() {
    let claude_result = result_message();
    let codex_completed = serde_json::json!({
        "type": "turn.completed",
        "usage": {"input_tokens": 1, "output_tokens": 2},
    })
    .to_string();
    let codex_failed = serde_json::json!({
        "type": "turn.failed",
        "error": {"message": "nope"},
    })
    .to_string();

    for (json, agent_type) in [
        (claude_result, shared::AgentType::Claude),
        (codex_completed, shared::AgentType::Codex),
        (codex_failed, shared::AgentType::Codex),
    ] {
        assert!(
            group_is_turn_terminator(&MessageGroup::Single(rendered(json)), agent_type),
            "single terminator frame should be recognized"
        );
    }
    // Muse terminal also terminates, but only when parsed as Muse.
    let muse_completed = serde_json::json!({
        "type": "muse_record",
        "payload_type": "run.terminal.completed",
        "payload": { "status": "completed" },
    })
    .to_string();
    assert!(group_is_turn_terminator(
        &MessageGroup::Single(rendered(muse_completed.clone())),
        shared::AgentType::Muse
    ));
    assert!(
        !group_is_turn_terminator(
            &MessageGroup::Single(rendered(muse_completed)),
            shared::AgentType::Claude
        ),
        "Muse terminal should not be terminator when parsed as Claude"
    );
    assert!(!group_is_turn_terminator(
        &MessageGroup::Single(rendered(plain_user_text("hello"))),
        shared::AgentType::Claude
    ));
    assert!(!group_is_turn_terminator(
        &MessageGroup::IdentityGroup {
            category: GroupCategory::User,
            label: "You".to_string(),
            badge_class: "user".to_string(),
            messages: vec![rendered(plain_user_text("hello"))],
        },
        shared::AgentType::Claude
    ));
}

/// One canonical wire shape per realistic message kind paired with the
/// `GroupCategory` the classifier MUST return on a Codex session. The
/// Codex agent type is the strictly-larger surface (Claude shapes
/// classify identically on both agent types, and Codex events only
/// classify on a Codex session), so a single Codex-agent sweep covers
/// the whole table.
///
/// If a new variant lands in `ClaudeMessage` or `CodexEvent`, extend
/// this table — the classifier is the only place that needs to know
/// about the new variant.
#[test]
fn classifier_exhaustive_over_realistic_messages() {
    let cases: Vec<(&str, String, Option<GroupCategory>)> = vec![
        (
            "assistant tool_use",
            assistant_with_tool_use("toolu_a", "Read"),
            Some(GroupCategory::Assistant),
        ),
        (
            "user tool_result envelope",
            read_tool_result_user_message("toolu_a"),
            Some(GroupCategory::Assistant),
        ),
        (
            "plain-text user prompt",
            plain_user_text("hello"),
            Some(GroupCategory::User),
        ),
        (
            "portal frame",
            portal_text_message("reconnected"),
            Some(GroupCategory::Portal),
        ),
        (
            "codex item.started",
            codex_item_started_agent_message("starting"),
            Some(GroupCategory::Codex),
        ),
        (
            "system message",
            serde_json::json!({
                "type": "system",
                "subtype": "init",
                "session_id": "01890000-0000-7000-8000-000000000001",
            })
            .to_string(),
            None,
        ),
        (
            "system thinking_tokens marker collapses into the Thinking group",
            thinking_tokens_message(150),
            Some(GroupCategory::Thinking),
        ),
        ("result message", result_message(), None),
        (
            "error message: on Codex agent the `{type: error}` shape \
             also matches `CodexEvent::Error` and lands in the Codex \
             group, preserved from the pre-refactor classifier",
            serde_json::json!({
                "type": "error",
                "message": "oops",
            })
            .to_string(),
            Some(GroupCategory::Codex),
        ),
        ("unparseable garbage", "not even json".to_string(), None),
    ];

    for (label, json, expected) in cases {
        let got = classify(&rendered(json), shared::AgentType::Codex, None).map(|i| i.category);
        assert_eq!(
            got, expected,
            "{label}: classifier returned {got:?}, expected {expected:?}"
        );
    }
}

/// `/clear` used to fall through the `_ => Unknown` wildcard and render as a
/// raw "Unrecognized Message" bubble even though claude-codes has typed it for
/// months (rust-code-agent-sdks#315).
#[test]
fn conversation_reset_parses_to_its_own_variant() {
    use super::super::types::ClaudeMessage;
    let json = r#"{
        "type": "conversation_reset",
        "new_conversation_id": "11111111-1111-1111-1111-111111111111",
        "uuid": "22222222-2222-2222-2222-222222222222",
        "session_id": "33333333-3333-3333-3333-333333333333"
    }"#;
    assert!(
        matches!(
            ClaudeMessage::parse(json),
            Ok(ClaudeMessage::ConversationReset(_))
        ),
        "conversation_reset must not fall through to Unknown"
    );
}

/// The portal's own error envelope is flat (`{type:"error", message:"…"}`),
/// unlike Anthropic's nested `{type:"error", error:{…}}`. It matched neither
/// `ClaudeOutput` nor the local-frame fallback, so every failed file upload
/// rendered as an "Unrecognized Message" raw bubble — the portal reporting its
/// own errors as frames it did not recognize.
#[test]
fn portal_error_envelope_renders_as_an_error_not_an_unknown_frame() {
    use super::super::types::ClaudeMessage;
    let json = r#"{"type":"error","message":"File upload failed: file is too large (limit 10 MB) — your message was not sent"}"#;
    match ClaudeMessage::parse(json) {
        Ok(ClaudeMessage::LocalError(e)) => {
            assert!(e.message.contains("too large"), "message preserved: {e:?}")
        }
        other => panic!("expected LocalError, got {other:?}"),
    }
}

/// Anthropic's nested envelope must keep routing to the existing error arm —
/// the new flat variant must not shadow it.
#[test]
fn anthropic_nested_error_still_parses_as_error() {
    use super::super::types::ClaudeMessage;
    let json = r#"{"type":"error","error":{"type":"api_error","message":"boom"}}"#;
    assert!(
        matches!(ClaudeMessage::parse(json), Ok(ClaudeMessage::Error(_))),
        "nested Anthropic errors must not regress to LocalError"
    );
}

/// The portal error must render as an error on non-Codex agents too — but
/// without stealing Codex's own `{type:"error"}` frame, which the exhaustive
/// sweep above pins. Same JSON, different agent, different owner.
#[test]
fn portal_error_routes_to_claude_but_leaves_codex_frames_alone() {
    use super::super::types::ClaudeMessage;
    use crate::components::agent_frame::{AgentFrame, AgentFrameRegistry};
    let json = r#"{"type":"error","message":"File upload failed"}"#;
    assert!(
        matches!(
            AgentFrameRegistry::parse(json, shared::AgentType::Claude),
            AgentFrame::Claude(ClaudeMessage::LocalError(_))
        ),
        "a Claude session must render the portal's own error"
    );
    assert!(
        matches!(
            AgentFrameRegistry::parse(json, shared::AgentType::Codex),
            AgentFrame::Codex(_)
        ),
        "Codex owns this shape and must keep it"
    );
}

/// The invariant that makes the single injection door worth having: **anything
/// the portal can author, the portal can render.**
///
/// `RenderedMessage::local` is the only way a portal-authored frame enters a
/// transcript, so the set of shapes the renderer must understand is exactly the
/// set of `LocalFrame` variants. Each one must parse to a real `ClaudeMessage`
/// variant — never `Unknown`, which is the raw-bubble fallback reserved for
/// *foreign* agent frames. Add a `LocalFrame` variant without teaching
/// `parse_local_frame` about it and this fails.
#[test]
fn every_local_frame_the_portal_can_author_renders() {
    use super::super::types::ClaudeMessage;

    let frames = [
        shared::LocalFrame::Portal(shared::PortalMessage::text("hi".into())),
        shared::LocalFrame::user("hi"),
        shared::LocalFrame::error("boom"),
    ];

    for frame in frames {
        let tag = frame.message_type();
        let json = frame.to_json();
        let parsed = ClaudeMessage::parse(&json)
            .unwrap_or_else(|e| panic!("{tag} frame must parse: {e} ({json})"));
        assert!(
            !matches!(parsed, ClaudeMessage::Unknown),
            "{tag} frame fell through to Unknown — it would render as a raw \
             \"Unrecognized Message\" bubble: {json}"
        );
    }
}
