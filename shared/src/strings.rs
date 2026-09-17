//! Small blank-string guard shared across crates.
//!
//! Single home for the repeated `!s.trim().is_empty()` shape in `if` guards
//! and `filter` closures. Everything here is std-only so the crate keeps
//! compiling for `wasm32-unknown-unknown`.

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
}
