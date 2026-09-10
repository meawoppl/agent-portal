//! Small string guards shared across claude-session-lib checks.
//!
//! Claude-crate counterpart to the `is_non_blank` helpers in `session-lib`,
//! `archive-format`, and the frontend utils (none is linkable from here
//! without growing this crate's dependency surface for a two-line check, so
//! it lives in each place instead).

/// True when `s` holds non-whitespace text.
///
/// Single home for the repeated `!s.trim().is_empty()` shape in `if` guards
/// and `||` predicates.
pub fn is_non_blank(s: &str) -> bool {
    !s.trim().is_empty()
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
}
