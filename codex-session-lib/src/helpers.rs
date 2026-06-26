//! Small helpers shared by `io_task` and `handler`.

use claude_codes::ClaudeInput;

/// Extract prompt text from ClaudeInput.
pub(crate) fn extract_prompt_text(input: &ClaudeInput) -> String {
    match input {
        ClaudeInput::User(msg) => msg
            .message
            .content
            .iter()
            .filter_map(|block| {
                if let claude_codes::io::ContentBlock::Text(tb) = block {
                    Some(tb.text.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Format a `codex_codes::RequestId` as a String.
pub(crate) fn format_request_id(id: &codex_codes::RequestId) -> String {
    match id {
        codex_codes::RequestId::Integer(n) => n.to_string(),
        codex_codes::RequestId::String(s) => s.clone(),
    }
}

/// Parse a String back to `codex_codes::RequestId`.
pub(crate) fn parse_request_id(s: &str) -> codex_codes::RequestId {
    if let Ok(n) = s.parse::<i64>() {
        codex_codes::RequestId::Integer(n)
    } else {
        codex_codes::RequestId::String(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use claude_codes::io::{ContentBlock, TextBlock};
    use codex_codes::RequestId;
    use uuid::Uuid;

    fn text_block(s: &str) -> ContentBlock {
        ContentBlock::Text(TextBlock {
            text: s.to_string(),
            citations: Vec::new(),
        })
    }

    #[test]
    fn extract_prompt_text_single_block() {
        let input = ClaudeInput::user_message("hello world", Uuid::nil());
        assert_eq!(extract_prompt_text(&input), "hello world");
    }

    #[test]
    fn extract_prompt_text_joins_text_blocks_and_skips_non_text() {
        // Non-text blocks (images, tool results, unknown) must be dropped, and
        // the surviving text joined with newlines — a bug here silently sends
        // a malformed or truncated prompt to the codex agent.
        let input = ClaudeInput::user_message_blocks(
            vec![
                text_block("first"),
                ContentBlock::Unknown(serde_json::json!({"type": "image"})),
                text_block("second"),
            ],
            Uuid::nil(),
        );
        assert_eq!(extract_prompt_text(&input), "first\nsecond");
    }

    #[test]
    fn extract_prompt_text_non_user_is_empty() {
        let input = ClaudeInput::Raw(serde_json::json!({"type": "whatever"}));
        assert_eq!(extract_prompt_text(&input), "");
    }

    #[test]
    fn request_id_round_trips_both_variants() {
        // Asymmetric parse/format silently breaks request↔result correlation,
        // so exercise both directions for integer and string ids.
        assert_eq!(format_request_id(&RequestId::Integer(42)), "42");
        assert_eq!(format_request_id(&RequestId::String("abc".into())), "abc");
        assert_eq!(parse_request_id("42"), RequestId::Integer(42));
        assert_eq!(parse_request_id("abc"), RequestId::String("abc".into()));

        for id in [RequestId::Integer(-7), RequestId::String("tool-9".into())] {
            assert_eq!(parse_request_id(&format_request_id(&id)), id);
        }
    }
}
