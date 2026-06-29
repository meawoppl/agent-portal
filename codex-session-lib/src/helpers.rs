//! Small helpers shared by `io_task` and `handler`.

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
    use codex_codes::RequestId;

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
