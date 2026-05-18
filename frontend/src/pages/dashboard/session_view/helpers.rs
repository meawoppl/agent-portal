//! Pure helpers extracted from `SessionView`.
//!
//! These functions take only typed arguments and return only typed results —
//! no `&self`, no `Context`, no DOM, no timers — so each one is independently
//! testable without mounting the Yew component. The orchestrator in
//! `component.rs` calls into them from inside the `update()` arms.
//!
//! See the per-function docstrings for which `SessionViewMsg` arm each helper
//! was extracted from.

use crate::components::message_renderer::types::{ClaudeMessage, ContentBlock};

/// Wire `type` tag for a typed [`ClaudeMessage`] variant. Centralizes the
/// variant-to-tag mapping so call sites that still trade in `msg_type: String`
/// can derive it from the typed enum instead of poking `.get("type")`.
pub(super) fn message_type_tag(m: &ClaudeMessage) -> &'static str {
    match m {
        ClaudeMessage::System(_) => "system",
        ClaudeMessage::Assistant(_) => "assistant",
        ClaudeMessage::Result(_) => "result",
        ClaudeMessage::User(_) => "user",
        ClaudeMessage::Error(_) => "error",
        ClaudeMessage::Portal(_) => "portal",
        ClaudeMessage::RateLimitEvent(_) => "rate_limit_event",
        ClaudeMessage::Unknown => "unknown",
    }
}

/// Extract the user-text payload from a typed user message for pending-send
/// echo matching. Returns the top-level `content` string when present (used by
/// the frontend's optimistic-send synthesizer and the codex shim's synthesized
/// echo) and otherwise concatenates `ContentBlock::Text` blocks from
/// `message.content` (the shape Claude's `--replay-user-messages` emits).
pub(super) fn extract_user_text(m: &ClaudeMessage) -> Option<String> {
    let ClaudeMessage::User(u) = m else {
        return None;
    };
    if let Some(text) = u.content.as_ref() {
        return Some(text.clone());
    }
    let blocks = u.message.as_ref().and_then(|m| m.content.as_ref())?;
    let texts: Vec<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join(""))
    }
}

/// Compute the next `should_autoscroll` value when the scroll listener
/// reports a new at-bottom reading. Returns `None` when no transition has
/// occurred (caller should skip the re-render) and `Some(new_value)` when
/// the flag flips. The transition gate lives here, outside the component,
/// so it can be unit-tested without a Yew `Context`.
pub(super) fn autoscroll_transition(current: bool, new_at_bottom: bool) -> Option<bool> {
    if current == new_at_bottom {
        None
    } else {
        Some(new_at_bottom)
    }
}

/// Check if a Claude session is awaiting user input by scanning messages
/// backwards. Skips noise types (portal, error, system, rate_limit_event)
/// and returns true if "result" is found before "user" or "assistant".
pub(super) fn is_claude_awaiting(
    messages: impl DoubleEndedIterator<Item = impl AsRef<str>>,
) -> bool {
    messages
        .rev()
        .find_map(|msg| {
            serde_json::from_str::<ClaudeMessage>(msg.as_ref())
                .ok()
                .filter(|m| {
                    matches!(
                        m,
                        ClaudeMessage::Result(_)
                            | ClaudeMessage::Assistant(_)
                            | ClaudeMessage::User(_)
                    )
                })
                .map(|m| message_type_tag(&m).to_string())
        })
        .is_some_and(|t| t == "result")
}

/// Derive the activity-tag string used by `on_activity` / `CheckAwaiting`
/// from a raw wire JSON string. Centralizes the two-step parse-and-classify
/// dance that was previously duplicated between `LoadHistory` (REST replay)
/// and `handle_received_output` (live wire) — both paths classified
/// identically but diverged in surrounding code, which made any future
/// classification change a two-site update.
///
/// Tries `shared::ClaudeOutput` first (the typed Claude wire shape, where
/// system messages disambiguate into `compaction_start` / `compaction_end`
/// / `task_start` / `task_end`), falling back to the local lenient
/// `ClaudeMessage` (portal frames, error envelopes, unknown shapes) via
/// [`message_type_tag`]. Returns `"unknown"` when neither parse succeeds.
pub(super) fn classify_output_msg_type(output: &str) -> String {
    if let Ok(claude_msg) = serde_json::from_str::<shared::ClaudeOutput>(output) {
        let mut msg_type = claude_msg.message_type();
        if let shared::ClaudeOutput::System(sys) = &claude_msg {
            if let Some(status) = sys.as_status() {
                if status.status.as_ref().map(|s| s.as_str()) == Some("compacting") {
                    msg_type = "compaction_start".to_string();
                }
            } else if shared::is_compaction_boundary(sys) {
                msg_type = "compaction_end".to_string();
            } else if sys.as_task_started().is_some() {
                msg_type = "task_start".to_string();
            } else if sys.as_task_notification().is_some() {
                msg_type = "task_end".to_string();
            }
        }
        return msg_type;
    }
    if let Ok(parsed) = serde_json::from_str::<ClaudeMessage>(output) {
        return message_type_tag(&parsed).to_string();
    }
    "unknown".to_string()
}

/// Inject `_created_at` (and optionally `_sender`) metadata into a wire-JSON
/// message string, returning a new JSON string. Used by both the REST
/// `LoadHistory` path (where the row carries `user_id` / `sender_name`
/// columns to fold into `_sender` for user messages) and the live
/// `WsEvent::Output` path (where only `_created_at` matters; `role_user` is
/// false so the `_sender` branch is skipped).
///
/// Returns the original `content` unchanged when the JSON parse fails — the
/// caller still pushes the raw string so a malformed wire frame doesn't
/// silently disappear from the message list.
pub(super) fn inject_message_metadata(
    content: &str,
    created_at: &str,
    role_user: bool,
    user_id: Option<&str>,
    sender_name: Option<&str>,
) -> String {
    let Ok(mut val) = serde_json::from_str::<serde_json::Value>(content) else {
        return content.to_string();
    };
    let Some(obj) = val.as_object_mut() else {
        return content.to_string();
    };
    if role_user && (user_id.is_some() || sender_name.is_some()) {
        obj.insert(
            "_sender".to_string(),
            serde_json::json!({
                "user_id": user_id.unwrap_or_default(),
                "name": sender_name.unwrap_or_default(),
            }),
        );
    }
    obj.insert(
        "_created_at".to_string(),
        serde_json::Value::String(created_at.to_string()),
    );
    val.to_string()
}

/// Drain pending optimistic-send entries when the server confirms our input.
///
/// - `"user"` echo: match by content (via [`extract_user_text`]) so a lost
///   message doesn't consume an unrelated pending entry — only the first
///   matching pending entry is removed.
/// - `"assistant"` / `"result"`: Claude is responding; slash commands like
///   `/cost`, `/status`, `/clear` don't produce a user echo, so we treat
///   the assistant/result response as the signal that the input was
///   received and clear *all* pending entries.
/// - Any other `msg_type`: no-op.
pub(super) fn reconcile_pending_sends(
    pending_sends: &mut Vec<String>,
    msg_type: &str,
    output: &str,
) {
    if pending_sends.is_empty() {
        return;
    }
    match msg_type {
        "user" => {
            let echo_text = serde_json::from_str::<ClaudeMessage>(output)
                .ok()
                .as_ref()
                .and_then(extract_user_text);
            if let Some(ref echo) = echo_text {
                if let Some(pos) = pending_sends.iter().position(|pending| {
                    serde_json::from_str::<ClaudeMessage>(pending)
                        .ok()
                        .as_ref()
                        .and_then(extract_user_text)
                        .as_ref()
                        == Some(echo)
                }) {
                    pending_sends.remove(pos);
                }
            }
        }
        "assistant" | "result" => {
            pending_sends.clear();
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- autoscroll_transition ---

    #[test]
    fn autoscroll_transition_returns_none_when_unchanged() {
        assert_eq!(autoscroll_transition(true, true), None);
        assert_eq!(autoscroll_transition(false, false), None);
    }

    #[test]
    fn autoscroll_transition_disables_when_user_scrolls_up() {
        // User was tailing, scrolled away from bottom -> tailing turns off
        // and the jump-to-live pill should render.
        assert_eq!(autoscroll_transition(true, false), Some(false));
    }

    #[test]
    fn autoscroll_transition_re_enables_when_user_scrolls_back_to_bottom() {
        // User had scrolled up, now scrolled back to bottom -> tailing
        // resumes and the jump-to-live pill should disappear.
        assert_eq!(autoscroll_transition(false, true), Some(true));
    }

    // --- classify_output_msg_type ---

    #[test]
    fn classify_output_msg_type_returns_unknown_for_garbage() {
        assert_eq!(classify_output_msg_type("not-json"), "unknown");
        assert_eq!(classify_output_msg_type(""), "unknown");
    }

    #[test]
    fn classify_output_msg_type_recognizes_assistant_envelope() {
        let json = r#"{"type":"assistant","message":{"content":[]}}"#;
        assert_eq!(classify_output_msg_type(json), "assistant");
    }

    #[test]
    fn classify_output_msg_type_recognizes_user_envelope() {
        let json = r#"{"type":"user","content":"hi"}"#;
        assert_eq!(classify_output_msg_type(json), "user");
    }

    #[test]
    fn classify_output_msg_type_recognizes_portal_envelope() {
        // Portal frames aren't part of `shared::ClaudeOutput` — the first
        // parse fails and the classifier falls through to the local lenient
        // `ClaudeMessage::Portal` shape via `message_type_tag`.
        let json = r#"{"type":"portal","content":[{"type":"text","text":"hi"}]}"#;
        assert_eq!(classify_output_msg_type(json), "portal");
    }

    #[test]
    fn classify_output_msg_type_recognizes_error_envelope() {
        let json = r#"{"type":"error","content":"boom"}"#;
        assert_eq!(classify_output_msg_type(json), "error");
    }

    // --- inject_message_metadata ---

    #[test]
    fn inject_message_metadata_adds_created_at_for_non_user_role() {
        let out = inject_message_metadata(
            r#"{"type":"assistant","content":"hi"}"#,
            "2026-05-18T12:00:00Z",
            false,
            None,
            None,
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["_created_at"], "2026-05-18T12:00:00Z");
        // Non-user role should never get _sender injected.
        assert!(v.get("_sender").is_none());
    }

    #[test]
    fn inject_message_metadata_adds_sender_for_user_role_with_attribution() {
        let out = inject_message_metadata(
            r#"{"type":"user","content":"hi"}"#,
            "2026-05-18T12:00:00Z",
            true,
            Some("uid-1"),
            Some("Alice"),
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["_created_at"], "2026-05-18T12:00:00Z");
        assert_eq!(v["_sender"]["user_id"], "uid-1");
        assert_eq!(v["_sender"]["name"], "Alice");
    }

    #[test]
    fn inject_message_metadata_skips_sender_for_user_role_without_attribution() {
        let out = inject_message_metadata(
            r#"{"type":"user","content":"hi"}"#,
            "2026-05-18T12:00:00Z",
            true,
            None,
            None,
        );
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["_created_at"], "2026-05-18T12:00:00Z");
        // User role with no attribution → no _sender either.
        assert!(v.get("_sender").is_none());
    }

    #[test]
    fn inject_message_metadata_returns_input_unchanged_for_invalid_json() {
        // Malformed wire frame: the caller still pushes the raw string so
        // the message doesn't silently disappear from the list.
        let out = inject_message_metadata("not-json", "ts", false, None, None);
        assert_eq!(out, "not-json");
    }

    #[test]
    fn inject_message_metadata_returns_input_unchanged_for_non_object_json() {
        // Valid JSON but not an object (e.g. a top-level string) — no
        // _created_at slot to inject into, return as-is so callers don't
        // re-serialize it into a quoted-string blob.
        let out = inject_message_metadata("\"hello\"", "ts", false, None, None);
        assert_eq!(out, "\"hello\"");
    }

    // --- reconcile_pending_sends ---

    #[test]
    fn reconcile_pending_sends_noop_when_empty() {
        let mut pending: Vec<String> = vec![];
        reconcile_pending_sends(&mut pending, "user", r#"{"type":"user","content":"x"}"#);
        assert!(pending.is_empty());
    }

    #[test]
    fn reconcile_pending_sends_user_echo_removes_first_matching_entry() {
        let mut pending = vec![
            r#"{"type":"user","content":"hello"}"#.to_string(),
            r#"{"type":"user","content":"world"}"#.to_string(),
        ];
        reconcile_pending_sends(&mut pending, "user", r#"{"type":"user","content":"hello"}"#);
        assert_eq!(pending.len(), 1);
        assert!(pending[0].contains("world"));
    }

    #[test]
    fn reconcile_pending_sends_user_echo_no_match_keeps_pending() {
        // A user echo for a message we didn't optimistically send must NOT
        // consume an unrelated pending entry — otherwise a multi-tab scenario
        // would drop legitimate pending sends.
        let mut pending = vec![r#"{"type":"user","content":"hello"}"#.to_string()];
        reconcile_pending_sends(
            &mut pending,
            "user",
            r#"{"type":"user","content":"unrelated"}"#,
        );
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn reconcile_pending_sends_assistant_clears_all() {
        // Slash commands (/cost, /clear, /status) don't echo as "user",
        // so the assistant response is the only signal we get that input
        // was received. Clear everything.
        let mut pending = vec![
            r#"{"type":"user","content":"a"}"#.to_string(),
            r#"{"type":"user","content":"b"}"#.to_string(),
        ];
        reconcile_pending_sends(
            &mut pending,
            "assistant",
            r#"{"type":"assistant","message":{"content":[]}}"#,
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn reconcile_pending_sends_result_clears_all() {
        let mut pending = vec![r#"{"type":"user","content":"a"}"#.to_string()];
        reconcile_pending_sends(
            &mut pending,
            "result",
            r#"{"type":"result","total_cost_usd":0.0}"#,
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn reconcile_pending_sends_ignores_other_msg_types() {
        let mut pending = vec![r#"{"type":"user","content":"a"}"#.to_string()];
        reconcile_pending_sends(&mut pending, "system", r#"{"type":"system"}"#);
        assert_eq!(pending.len(), 1);
    }

    // --- is_claude_awaiting ---

    #[test]
    fn is_claude_awaiting_true_when_last_signal_is_result() {
        let msgs = [
            r#"{"type":"user","content":"q"}"#.to_string(),
            r#"{"type":"assistant","message":{"content":[]}}"#.to_string(),
            r#"{"type":"result","total_cost_usd":0.0}"#.to_string(),
        ];
        assert!(is_claude_awaiting(msgs.iter()));
    }

    #[test]
    fn is_claude_awaiting_false_when_last_signal_is_assistant() {
        let msgs = [
            r#"{"type":"user","content":"q"}"#.to_string(),
            r#"{"type":"assistant","message":{"content":[]}}"#.to_string(),
        ];
        assert!(!is_claude_awaiting(msgs.iter()));
    }

    #[test]
    fn is_claude_awaiting_skips_noise_types_when_finding_last_signal() {
        // Portal / error / system messages don't gate awaiting — the last
        // result before any of those still counts.
        let msgs = [
            r#"{"type":"result","total_cost_usd":0.0}"#.to_string(),
            r#"{"type":"portal","content":[{"type":"text","text":"x"}]}"#.to_string(),
            r#"{"type":"error","content":"y"}"#.to_string(),
        ];
        assert!(is_claude_awaiting(msgs.iter()));
    }

    #[test]
    fn is_claude_awaiting_false_for_empty_history() {
        let msgs: Vec<String> = vec![];
        assert!(!is_claude_awaiting(msgs.iter()));
    }

    // --- extract_user_text ---

    #[test]
    fn extract_user_text_prefers_top_level_content() {
        let m: ClaudeMessage =
            serde_json::from_str(r#"{"type":"user","content":"hello"}"#).unwrap();
        assert_eq!(extract_user_text(&m).as_deref(), Some("hello"));
    }

    #[test]
    fn extract_user_text_falls_back_to_concatenated_text_blocks() {
        let m: ClaudeMessage = serde_json::from_str(
            r#"{"type":"user","message":{"content":[{"type":"text","text":"foo"},{"type":"text","text":"bar"}]}}"#,
        )
        .unwrap();
        assert_eq!(extract_user_text(&m).as_deref(), Some("foobar"));
    }

    #[test]
    fn extract_user_text_returns_none_for_non_user_variant() {
        let m: ClaudeMessage = serde_json::from_str(r#"{"type":"system"}"#).unwrap();
        assert_eq!(extract_user_text(&m), None);
    }

    #[test]
    fn extract_user_text_returns_none_when_no_text_blocks_and_no_top_level_content() {
        let m: ClaudeMessage =
            serde_json::from_str(r#"{"type":"user","message":{"content":[]}}"#).unwrap();
        assert_eq!(extract_user_text(&m), None);
    }

    // --- message_type_tag ---

    #[test]
    fn message_type_tag_returns_expected_string_for_each_variant() {
        assert_eq!(
            message_type_tag(
                &serde_json::from_str::<ClaudeMessage>(r#"{"type":"system"}"#).unwrap()
            ),
            "system"
        );
        assert_eq!(
            message_type_tag(
                &serde_json::from_str::<ClaudeMessage>(r#"{"type":"user","content":"x"}"#).unwrap()
            ),
            "user"
        );
        assert_eq!(
            message_type_tag(
                &serde_json::from_str::<ClaudeMessage>(r#"{"type":"error","content":"x"}"#)
                    .unwrap()
            ),
            "error"
        );
    }
}
