//! Compact model-version extraction for the session-pill watermark.
//!
//! The dashboard rail renders a faint model number next to the agent logo on
//! each session pill so a glance distinguishes, say, an Opus 4.8 session from
//! a Fable 5 one. This module turns a full model **id** (as it arrives from the
//! agent's per-turn result, e.g. `"claude-opus-4-8"` or `"gpt-5.5-codex"`) into
//! a compact token like `"4.8"` / `"5.5"`.
//!
//! Why digit-pattern extraction rather than the SDK model catalogs
//! (`claude_codes::ClaudeModel` / `codex_codes::CodexModel`): the catalogs key
//! on picker **cli args** (`"opus"`, `"sonnet"`, aliases) — not the fully
//! qualified id string the runtime reports on each turn — so they can't map
//! `"claude-haiku-4-5-20251001"` back to a version. The id form always carries
//! the version as a dash- or dot-separated run of small numbers, optionally
//! followed by a date/build suffix (`20251001`), so we extract that directly.
//! This also gracefully handles ids the catalog has never heard of (a
//! newly-shipped model): as long as it follows the `family-<version>` shape we
//! still get a sensible token, and anything unrecognizable yields `None` (the
//! caller renders the logo alone — never the raw id).

/// Extract a compact display version from a model id string.
///
/// Returns `None` when no plausible version can be found, so the caller renders
/// the logo watermark alone rather than an ugly raw id.
///
/// Examples:
/// ```
/// use shared::compact_model_version;
/// assert_eq!(compact_model_version("claude-opus-4-8").as_deref(), Some("4.8"));
/// assert_eq!(compact_model_version("claude-fable-5").as_deref(), Some("5"));
/// assert_eq!(compact_model_version("claude-sonnet-5").as_deref(), Some("5"));
/// assert_eq!(
///     compact_model_version("claude-haiku-4-5-20251001").as_deref(),
///     Some("4.5")
/// );
/// assert_eq!(compact_model_version("gpt-5.5-codex").as_deref(), Some("5.5"));
/// assert_eq!(compact_model_version("garbled-nonsense"), None);
/// ```
pub fn compact_model_version(model_id: &str) -> Option<String> {
    // Version components are short numbers. A pure-digit run of 4+ digits is a
    // date/build snapshot (`20251001`, `1106`), not a version part, so we treat
    // it as a terminator rather than a component.
    const MAX_VERSION_PART_DIGITS: usize = 3;

    #[derive(PartialEq)]
    enum Kind {
        /// A version component, e.g. `4`, `8`, or `5.5` (dotted is always one).
        Version,
        /// A date/build suffix like `20251001` — terminates the version run.
        DateOrBuild,
        /// Anything else (a family word like `opus`, `codex`).
        Other,
    }

    fn classify(tok: &str) -> Kind {
        if tok.is_empty() {
            return Kind::Other;
        }
        // Numeric-ish: digits with optional internal dots (`5`, `5.5`).
        let numeric_ish = tok
            .split('.')
            .all(|seg| !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_digit()));
        if !numeric_ish {
            return Kind::Other;
        }
        if tok.contains('.') {
            // A dotted number is unambiguously a version ("5.5").
            Kind::Version
        } else if tok.len() <= MAX_VERSION_PART_DIGITS {
            Kind::Version
        } else {
            Kind::DateOrBuild
        }
    }

    let mut parts: Vec<&str> = Vec::new();
    for tok in model_id.split('-') {
        match classify(tok) {
            Kind::Version => parts.push(tok),
            // Once the version run has started, any non-version token ends it
            // (so a trailing date or family suffix isn't mixed in). Before it
            // starts, skip leading family words.
            Kind::DateOrBuild | Kind::Other => {
                if parts.is_empty() {
                    continue;
                } else {
                    break;
                }
            }
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

#[cfg(test)]
mod tests {
    use super::compact_model_version;

    #[test]
    fn claude_dashed_major_minor() {
        assert_eq!(
            compact_model_version("claude-opus-4-8").as_deref(),
            Some("4.8")
        );
    }

    #[test]
    fn claude_single_component() {
        assert_eq!(
            compact_model_version("claude-fable-5").as_deref(),
            Some("5")
        );
        assert_eq!(
            compact_model_version("claude-sonnet-5").as_deref(),
            Some("5")
        );
    }

    #[test]
    fn claude_with_date_suffix_dropped() {
        assert_eq!(
            compact_model_version("claude-haiku-4-5-20251001").as_deref(),
            Some("4.5")
        );
    }

    #[test]
    fn codex_dotted_version() {
        assert_eq!(
            compact_model_version("gpt-5.5-codex").as_deref(),
            Some("5.5")
        );
    }

    #[test]
    fn codex_trailing_family_word_stops_run() {
        // The `-codex` suffix must not leak into the token.
        assert_eq!(compact_model_version("gpt-5-codex").as_deref(), Some("5"));
    }

    #[test]
    fn unknown_or_garbled_yields_none() {
        assert_eq!(compact_model_version(""), None);
        assert_eq!(compact_model_version("garbled-nonsense"), None);
        assert_eq!(compact_model_version("claude"), None);
        // A bare uuid-ish token has no small numeric run.
        assert_eq!(compact_model_version("abc123def"), None);
    }

    #[test]
    fn date_only_after_family_is_none() {
        // No version parts before the date suffix → nothing to show.
        assert_eq!(compact_model_version("some-model-20251001"), None);
    }

    #[test]
    fn three_component_version_joins_all() {
        assert_eq!(compact_model_version("foo-1-2-3").as_deref(), Some("1.2.3"));
    }
}
