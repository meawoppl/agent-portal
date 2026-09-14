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

/// Keep a trimmed owned copy of `s` only when it is non-blank.
///
/// Single home for the repeated
/// `if is_non_blank(&v) { Some(v.trim().to_string()) } else { None }` shape at
/// env-var and form-submit call sites so one arm can't drift (e.g. trimming
/// in one place while cloning the untrimmed value in another).
#[must_use]
pub fn owned_non_blank(s: &str) -> Option<String> {
    is_non_blank(s).then(|| s.trim().to_string())
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
    fn owned_non_blank_returns_trimmed_owned_string() {
        assert_eq!(owned_non_blank(""), None);
        assert_eq!(owned_non_blank("   "), None);
        assert_eq!(owned_non_blank("  hi  "), Some("hi".to_string()));
    }
}
