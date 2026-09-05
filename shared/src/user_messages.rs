//! Deciding whether a stored user-role wire record is a *substantive* user
//! message — something a human actually typed — as opposed to machinery that
//! rides the user role: tool-result frames, and the reinjected notice blocks
//! (`<system-reminder>` / `<task-notification>`) the CLI sends when a
//! background task completes.
//!
//! Used to compute the history viewer's "User msgs" column (the archive
//! manifest's `user_message_count`). Lives in `shared` so the backend counter
//! and any frontend consumer apply the same rule, and so the notice-stripping
//! stays in lockstep with [`crate::system_reminder`], which defines what the
//! frontend collapses.

use crate::system_reminder::strip_collapsible_notices;

/// Human-visible text of a stored user-role wire record's `content` JSON.
///
/// Handles the shapes that reach the messages table across agents:
/// - Claude wire frames: `{"type":"user","message":{"content": "…" | [blocks]}}`
///   — of a block array, only `{"type":"text","text":…}` blocks count, which
///   is what excludes tool-result-only frames;
/// - portal [`crate::UserFrame`]s (codex/muse inputs): `{"type":"user","content":"…"}`;
/// - bare strings (non-JSON content degraded to a JSON string at archive time).
pub fn user_visible_text(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Object(obj) => {
            if let Some(message) = obj.get("message") {
                return message_content_text(message);
            }
            match obj.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(other) => blocks_text(other),
                None => String::new(),
            }
        }
        _ => String::new(),
    }
}

fn message_content_text(message: &serde_json::Value) -> String {
    match message.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(other) => blocks_text(other),
        None => String::new(),
    }
}

/// Concatenated `text` of the `type == "text"` blocks in a content array.
fn blocks_text(content: &serde_json::Value) -> String {
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for block in blocks {
        if block.get("type").and_then(|t| t.as_str()) == Some("text") {
            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
    }
    out
}

/// True when the text still says something after the machine-authored notice
/// blocks are stripped. A record whose text is *only* reinjected XML (the
/// task-complete `<task-notification>`, a standalone `<system-reminder>`)
/// is not a user message in any sense the history viewer cares about.
pub fn is_substantive_user_text(text: &str) -> bool {
    !strip_collapsible_notices(text).trim().is_empty()
}

/// [`user_visible_text`] + [`is_substantive_user_text`] over a stored record.
pub fn is_substantive_user_record(content: &serde_json::Value) -> bool {
    is_substantive_user_text(&user_visible_text(content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_typed_message_counts() {
        let v = json!({"type":"user","message":{"role":"user","content":"fix the bug"}});
        assert!(is_substantive_user_record(&v));
    }

    #[test]
    fn text_block_array_counts() {
        let v = json!({"type":"user","message":{"content":[{"type":"text","text":"hello"}]}});
        assert!(is_substantive_user_record(&v));
    }

    #[test]
    fn tool_result_only_frame_does_not_count() {
        let v = json!({"type":"user","message":{"content":[
            {"type":"tool_result","tool_use_id":"t1","content":"file contents here"}
        ]}});
        assert!(!is_substantive_user_record(&v));
    }

    #[test]
    fn task_notification_reinjection_does_not_count() {
        let v = json!({"type":"user","message":{"content":
            "<task-notification>\nagent finished: build ok\n</task-notification>"
        }});
        assert!(!is_substantive_user_record(&v));
    }

    #[test]
    fn system_reminder_only_does_not_count() {
        let v = json!({"type":"user","message":{"content":[
            {"type":"text","text":"<system-reminder>\nnudge text\n</system-reminder>"}
        ]}});
        assert!(!is_substantive_user_record(&v));
    }

    #[test]
    fn reminder_prefixed_first_input_still_counts() {
        // Since #1649 the portal reminder is folded into the first user input;
        // the human prose after it must keep the message substantive.
        let v = json!({"type":"user","message":{"content":
            "<system-reminder>portal features…</system-reminder>\nplease add a login page"
        }});
        assert!(is_substantive_user_record(&v));
    }

    #[test]
    fn portal_user_frame_counts() {
        let v = json!({"type":"user","content":"codex, run the tests"});
        assert!(is_substantive_user_record(&v));
    }

    #[test]
    fn bare_string_and_empty_shapes() {
        assert!(is_substantive_user_record(&json!("typed as raw text")));
        assert!(!is_substantive_user_record(&json!("   ")));
        assert!(!is_substantive_user_record(&json!({"type":"user"})));
        assert!(!is_substantive_user_record(&json!(null)));
    }
}
