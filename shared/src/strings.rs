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
/// The trimmed text when `value` holds non-whitespace text; `None` when it is
/// absent or blank. Single home for the repeated
/// `.map(str::trim).filter(|s| !s.is_empty())` chain on optional inputs.
#[must_use]
pub fn trimmed_non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
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
}
