//! Small string helpers shared across crates.
//!
//! Single home for the repeated `!s.trim().is_empty()` shape in `if` guards
//! and `filter` closures, plus ASCII case-insensitive substring matching.
//! Everything here is std-only so the crate keeps compiling for
//! `wasm32-unknown-unknown`.

/// True when `s` holds non-whitespace text.
#[must_use]
pub fn is_non_blank(s: &str) -> bool {
    !s.trim().is_empty()
}

/// Keep a borrowed string only when it is non-blank.
///
/// Trims before returning so whitespace-only input is treated as absent.
#[must_use]
pub fn non_blank(s: &str) -> Option<&str> {
    is_non_blank(s).then(|| s.trim())
}

/// Keep a trimmed owned copy of `s` only when it is non-blank.
///
/// Single home for the repeated
/// `(!s.trim().is_empty()).then(|| s.trim().to_string())` shape at
/// form-submit call sites so they cannot drift (e.g. one arm trimming while
/// another clones the untrimmed value).
#[must_use]
pub fn owned_non_blank(s: &str) -> Option<String> {
    non_blank(s).map(str::to_string)
}

/// True when `s` holds at least one byte.
///
/// Single home for the repeated `.filter(|v| !v.is_empty())` shape on
/// optional env/config/remembered strings so the call sites cannot drift
/// (e.g. one arm trimming while another keeps whitespace). Unlike
/// [`is_non_blank`], whitespace counts as content: only the truly empty
/// string is absent.
#[must_use]
pub fn is_non_empty(s: &str) -> bool {
    !s.is_empty()
}

/// True when `haystack` contains `needle`, comparing ASCII-case-insensitively.
///
/// Single home for the repeated `haystack.to_ascii_lowercase().contains(...)`
/// shape at filter and error-classification call sites so they cannot drift
/// (e.g. one arm lowering only the haystack while another lowers both sides).
/// An empty needle matches everything, mirroring [`str::contains`].
#[must_use]
pub fn contains_case_insensitive(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// The trimmed text when `value` holds non-whitespace text; `None` when it is
/// absent or blank. Single home for the repeated
/// `.map(str::trim).filter(|s| !s.is_empty())` chain on optional inputs.
#[must_use]
pub fn trimmed_non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

/// Collapse every run of whitespace (including newlines and tabs) into a
/// single space, trimming the ends.
///
/// Single home for the repeated
/// `split_whitespace().collect::<Vec<_>>().join(" ")` shape in one-line
/// preview and snippet surfaces so the call sites cannot drift (e.g. one
/// joining with two spaces while another forgets the trim that
/// `split_whitespace` provides for free).
#[must_use]
pub fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max_chars` on a char boundary, appending `…` when cut.
///
/// Short input returns as-is; otherwise the first `max_chars - 1` chars plus
/// the ellipsis marker, so output never exceeds `max_chars` chars. Single
/// home for the repeated `take(MAX - 1)` + `push('…')` shape in capped
/// preview and notification snippet surfaces so the call sites cannot drift
/// (e.g. one keeping `MAX` chars before the marker while another keeps
/// `MAX - 1`).
#[must_use]
pub fn truncate_with_ellipsis(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_non_blank_rejects_empty_and_whitespace_only() {
        assert!(!is_non_blank(""));
        assert!(!is_non_blank("   "));
        assert!(!is_non_blank("\t\n "));
        assert!(is_non_blank("  hi  "));
    }

    #[test]
    fn owned_non_blank_trims_and_treats_blank_as_absent() {
        assert_eq!(owned_non_blank(""), None);
        assert_eq!(owned_non_blank("   "), None);
        assert_eq!(owned_non_blank("  hi  "), Some("hi".to_string()));
    }

    #[test]
    fn is_non_empty_rejects_only_the_empty_string() {
        assert!(!is_non_empty(""));
        assert!(is_non_empty("   "));
        assert!(is_non_empty("hi"));
    }

    #[test]
    fn contains_case_insensitive_ignores_ascii_case_on_both_sides() {
        assert!(contains_case_insensitive("Claude-Opus-4-7[1M]", "[1m]"));
        assert!(contains_case_insensitive(
            "Request Aborted: ANOTHER REQUEST in flight",
            "another request"
        ));
        assert!(contains_case_insensitive("owner@Example.com", "EXAMPLE"));
        assert!(!contains_case_insensitive("owner@example.com", "other"));
    }

    #[test]
    fn contains_case_insensitive_empty_needle_matches_everything() {
        assert!(contains_case_insensitive("anything", ""));
    }

    #[test]
    fn trimmed_non_blank_trims_or_rejects() {
        assert_eq!(
            trimmed_non_blank(Some("  api-refactor  ")),
            Some("api-refactor")
        );
        assert_eq!(trimmed_non_blank(Some("hi")), Some("hi"));
        assert_eq!(trimmed_non_blank(Some("   ")), None);
        assert_eq!(trimmed_non_blank(Some("")), None);
        assert_eq!(trimmed_non_blank(None), None);
    }

    #[test]
    fn collapse_whitespace_joins_runs_with_single_spaces() {
        assert_eq!(collapse_whitespace("  hello   world  "), "hello world");
        assert_eq!(
            collapse_whitespace("line one\n\tline two\r\nline three"),
            "line one line two line three"
        );
        assert_eq!(collapse_whitespace(""), "");
        assert_eq!(collapse_whitespace("   "), "");
        assert_eq!(collapse_whitespace("already single"), "already single");
    }

    #[test]
    fn truncate_with_ellipsis_keeps_short_input_untouched() {
        assert_eq!(truncate_with_ellipsis("hello", 120), "hello");
        assert_eq!(truncate_with_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn truncate_with_ellipsis_caps_at_max_chars_with_marker() {
        let cut = truncate_with_ellipsis(&"x".repeat(500), 120);
        assert_eq!(cut.chars().count(), 120);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn truncate_with_ellipsis_is_char_boundary_safe() {
        let cut = truncate_with_ellipsis(&"é".repeat(500), 120);
        assert_eq!(cut.chars().count(), 120);
        assert!(cut.ends_with('…'));
    }
}
