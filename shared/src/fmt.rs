//! Small string/number formatting helpers shared between the frontend
//! (WASM) and native crates. Everything here must stay std-only so the
//! crate keeps compiling for `wasm32-unknown-unknown`.

/// Truncate a string to at most `max_len` bytes, backing off to the
/// nearest UTF-8 character boundary so the slice is always valid.
#[must_use]
pub fn truncate_str(s: &str, max_len: usize) -> &str {
    if s.len() <= max_len {
        s
    } else {
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Format a millisecond duration as a short human-readable string:
/// `"950ms"`, `"2.5s"`, or `"3m 17s"`.
#[must_use]
pub fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let mins = ms / 60000;
        let secs = (ms % 60000) / 1000;
        format!("{}m {}s", mins, secs)
    }
}

/// Formats a byte count as a human-readable size (`"512 B"`, `"1.5 KB"`, `"2.0 MB"`).
/// Single `B/KB/MB` implementation for the workspace — frontend and launcher
/// use this via `shared` instead of ad-hoc formatting.
#[must_use]
pub fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// Formats a token count with `K`/`M` suffixes for readability.
/// Takes `i64` to match `usage` counts without casts at call sites (counts can
/// be negative in tests; `u64` would require casts).
#[must_use]
pub fn format_token_count(count: i64) -> String {
    if count < 1000 {
        count.to_string()
    } else if count < 1_000_000 {
        format!("{:.1}K", count as f64 / 1000.0)
    } else {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_str_short_string_untouched() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello", 5), "hello");
        assert_eq!(truncate_str("", 0), "");
    }

    #[test]
    fn truncate_str_cuts_at_byte_limit() {
        assert_eq!(truncate_str("hello world", 5), "hello");
        assert_eq!(truncate_str("hello", 0), "");
    }

    #[test]
    fn truncate_str_respects_utf8_boundaries() {
        // "é" is 2 bytes; cutting mid-character must back off.
        assert_eq!(truncate_str("éé", 1), "");
        assert_eq!(truncate_str("éé", 2), "é");
        assert_eq!(truncate_str("éé", 3), "é");
        // 4-byte emoji
        assert_eq!(truncate_str("a😀b", 2), "a");
        assert_eq!(truncate_str("a😀b", 5), "a😀");
    }

    #[test]
    fn format_duration_milliseconds() {
        assert_eq!(format_duration(0), "0ms");
        assert_eq!(format_duration(999), "999ms");
    }

    #[test]
    fn format_duration_seconds() {
        assert_eq!(format_duration(1000), "1.0s");
        assert_eq!(format_duration(2500), "2.5s");
        assert_eq!(format_duration(59999), "60.0s");
    }

    #[test]
    fn format_duration_minutes() {
        assert_eq!(format_duration(60000), "1m 0s");
        assert_eq!(format_duration(197_000), "3m 17s");
        assert_eq!(format_duration(3_599_999), "59m 59s");
    }

    #[test]
    fn format_file_size_bytes_and_kb_and_mb() {
        assert_eq!(format_file_size(0), "0 B");
        assert_eq!(format_file_size(1), "1 B");
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(1023), "1023 B");
        assert_eq!(format_file_size(1024), "1.0 KB");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(1024 * 1024 - 1), "1024.0 KB");
        assert_eq!(format_file_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_file_size(2 * 1024 * 1024), "2.0 MB");
        assert_eq!(format_file_size(5 * 1024 * 1024 + 512 * 1024), "5.5 MB");
    }

    #[test]
    fn format_token_count_k_and_m() {
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1000), "1.0K");
        assert_eq!(format_token_count(1_500), "1.5K");
        assert_eq!(format_token_count(999_999), "1000.0K");
        assert_eq!(format_token_count(1_000_000), "1.0M");
        assert_eq!(format_token_count(2_345_678), "2.3M");
    }
}
