//! Small string guards shared across backend checks.
//!
//! Backend counterpart to the `is_non_blank` family in `frontend/src/utils.rs`
//! (the frontend cannot share this crate, and the helper is two lines, so it
//! lives in both places rather than growing `shared/` for backend
//! convenience).

/// True when `s` holds non-whitespace text.
///
/// Single home for the repeated `!s.trim().is_empty()` shape in `if` guards,
/// match guards, and `filter` closures.
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
