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
