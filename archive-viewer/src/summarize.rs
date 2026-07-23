//! Pure, dependency-free summarizer over a transcript line's raw `content`
//! JSON. Both Claude and Codex store their message body as an opaque JSON
//! value in [`archive_format::ArchiveMessageLine::content`]; this collapses
//! whichever shape it is into one short human-readable line for `cat`'s
//! digest view. It is deliberately defensive (never panics, handles unknown
//! shapes) and pure (no I/O), so it is cheap to unit-test.

use serde_json::Value;

/// Max characters in a summarized line before it is truncated with an
/// ellipsis. Keeps `cat`'s digest to roughly one terminal line per message.
pub const SUMMARY_MAX_CHARS: usize = 200;

/// Summarize a message's raw `content` JSON into a compact single line.
///
/// Handles the common shapes:
/// * a bare string,
/// * `{ "text": "..." }`,
/// * `{ "content": <blocks> }` or a bare array of content blocks, where each
///   block is either a string or `{ "type": ..., ... }` (text / tool_use /
///   tool_result / image / thinking).
///
/// `thinking` blocks are skipped. Anything unrecognized falls back to a
/// compact JSON rendering. The result is whitespace-collapsed and truncated
/// to [`SUMMARY_MAX_CHARS`].
pub fn summarize_content(content: &Value) -> String {
    let raw = render(content);
    truncate(&collapse_ws(&raw), SUMMARY_MAX_CHARS)
}

/// Render the content to a (possibly long) string, before whitespace
/// collapsing/truncation.
fn render(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => render_blocks(blocks),
        Value::Object(map) => {
            // A message envelope with a nested content array/string.
            if let Some(inner) = map.get("content") {
                match inner {
                    Value::Array(blocks) => return render_blocks(blocks),
                    Value::String(s) => return s.clone(),
                    _ => {}
                }
            }
            // A single block object, or a `{ "text": ... }` shape.
            if let Some(rendered) = render_block(content) {
                return rendered;
            }
            compact(content)
        }
        _ => compact(content),
    }
}

/// Render an array of content blocks, joining the non-empty pieces.
fn render_blocks(blocks: &[Value]) -> String {
    blocks
        .iter()
        .filter_map(render_block)
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Render one content block. Returns `None` for blocks that contribute
/// nothing to the summary (e.g. `thinking`).
fn render_block(block: &Value) -> Option<String> {
    match block {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => {
            let kind = map.get("type").and_then(Value::as_str);
            match kind {
                Some("thinking") | Some("redacted_thinking") => None,
                Some("text") => map
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(Some(String::new())),
                Some("tool_use") => {
                    let name = map.get("name").and_then(Value::as_str).unwrap_or("unknown");
                    Some(format!("[tool_use: {name}]"))
                }
                Some("tool_result") => Some("[tool_result]".to_string()),
                Some("image") => Some("[image]".to_string()),
                // No `type` but a bare `text` field (common for user turns).
                None => map.get("text").and_then(Value::as_str).map(str::to_string),
                // A typed block we don't special-case: name it.
                Some(other) => Some(format!("[{other}]")),
            }
        }
        _ => None,
    }
}

/// Compact JSON fallback (single line, no pretty printing).
fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unrenderable>".to_string())
}

/// Collapse all runs of ASCII whitespace (including newlines) to single
/// spaces and trim the ends.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` characters (by `char`, so we never split a UTF-8
/// codepoint), appending an ellipsis when we cut.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bare_string() {
        assert_eq!(summarize_content(&json!("hello world")), "hello world");
    }

    #[test]
    fn text_field_object() {
        assert_eq!(summarize_content(&json!({"text": "hi there"})), "hi there");
    }

    #[test]
    fn content_blocks_text_and_tool_use() {
        let v = json!({
            "content": [
                {"type": "text", "text": "let me check"},
                {"type": "tool_use", "name": "Bash", "input": {"command": "ls"}},
            ]
        });
        assert_eq!(summarize_content(&v), "let me check [tool_use: Bash]");
    }

    #[test]
    fn thinking_blocks_are_skipped() {
        let v = json!([
            {"type": "thinking", "thinking": "secret reasoning that is long"},
            {"type": "text", "text": "visible answer"},
        ]);
        assert_eq!(summarize_content(&v), "visible answer");
    }

    #[test]
    fn tool_result_and_image() {
        let v = json!([
            {"type": "tool_result", "content": "output"},
            {"type": "image", "source": {}},
        ]);
        assert_eq!(summarize_content(&v), "[tool_result] [image]");
    }

    #[test]
    fn whitespace_is_collapsed() {
        assert_eq!(
            summarize_content(&json!("line one\n\n   line   two")),
            "line one line two"
        );
    }

    #[test]
    fn long_text_is_truncated_with_ellipsis() {
        let long = "x".repeat(500);
        let out = summarize_content(&json!(long));
        assert_eq!(out.chars().count(), SUMMARY_MAX_CHARS);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn unknown_shape_falls_back_to_compact_json() {
        let v = json!({"weird": 42});
        assert_eq!(summarize_content(&v), "{\"weird\":42}");
    }

    #[test]
    fn tool_use_without_name_is_safe() {
        let v = json!([{"type": "tool_use"}]);
        assert_eq!(summarize_content(&v), "[tool_use: unknown]");
    }
}
