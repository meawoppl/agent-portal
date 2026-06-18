//! Timezone canonicalization shared by the scheduler (launcher), the task API
//! (backend), and the schedule dialog (frontend).
//!
//! The launcher's scheduler resolves a task's `timezone` with
//! `chrono_tz::Tz`, which only accepts IANA names (e.g. `America/Los_Angeles`).
//! Users naturally type abbreviations like `PST`/`PDT`, which silently failed
//! to parse and fell back to UTC — firing scheduled tasks 7–8 hours off with no
//! visible error (see issue #1064).
//!
//! This module maps common abbreviations to IANA zones so they resolve
//! correctly, and is intentionally dependency-free (pure string mapping) so it
//! stays WASM-compatible for the frontend. IANA validation still happens where
//! `chrono_tz` is available (the launcher); everything here only canonicalizes.

/// Map a common timezone abbreviation (case-insensitive) to a canonical IANA
/// zone name, or `None` if the input isn't a recognized abbreviation.
///
/// Abbreviations are inherently ambiguous (e.g. `CST` is US Central *or* China
/// Standard); these resolve to the North American zones this tool's users mean.
/// Anything not listed should be treated as an IANA name by the caller.
pub fn abbrev_to_iana(input: &str) -> Option<&'static str> {
    let key = input.trim().to_ascii_uppercase();
    Some(match key.as_str() {
        "UTC" | "GMT" | "Z" | "ZULU" => "UTC",
        "PT" | "PST" | "PDT" => "America/Los_Angeles",
        "MT" | "MST" | "MDT" => "America/Denver",
        "CT" | "CST" | "CDT" => "America/Chicago",
        "ET" | "EST" | "EDT" => "America/New_York",
        "AKT" | "AKST" | "AKDT" => "America/Anchorage",
        "HT" | "HST" => "Pacific/Honolulu",
        "BST" => "Europe/London",
        "CET" | "CEST" => "Europe/Paris",
        "IST" => "Asia/Kolkata",
        "JST" => "Asia/Tokyo",
        "AEST" | "AEDT" => "Australia/Sydney",
        _ => return None,
    })
}

/// Canonicalize a user-supplied timezone string to an IANA name when possible.
///
/// Recognized abbreviations are mapped to their IANA zone; everything else is
/// returned trimmed and unchanged for the caller to parse/validate as an IANA
/// name. Never fails — an unknown value is passed through verbatim so the caller
/// can decide how to handle it.
pub fn canonicalize_timezone(input: &str) -> String {
    abbrev_to_iana(input)
        .map(str::to_string)
        .unwrap_or_else(|| input.trim().to_string())
}

/// Common IANA zones offered as suggestions in the schedule dialog. Not
/// exhaustive — the field still accepts any IANA name (or a mapped abbrev).
pub const COMMON_IANA_ZONES: &[&str] = &[
    "UTC",
    "America/Los_Angeles",
    "America/Denver",
    "America/Chicago",
    "America/New_York",
    "America/Anchorage",
    "Pacific/Honolulu",
    "Europe/London",
    "Europe/Paris",
    "Asia/Kolkata",
    "Asia/Tokyo",
    "Australia/Sydney",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_us_abbreviations_case_insensitively() {
        assert_eq!(abbrev_to_iana("PST"), Some("America/Los_Angeles"));
        assert_eq!(abbrev_to_iana("pdt"), Some("America/Los_Angeles"));
        assert_eq!(abbrev_to_iana(" PT "), Some("America/Los_Angeles"));
        assert_eq!(abbrev_to_iana("EST"), Some("America/New_York"));
        assert_eq!(abbrev_to_iana("CT"), Some("America/Chicago"));
    }

    #[test]
    fn utc_aliases_map_to_utc() {
        assert_eq!(abbrev_to_iana("UTC"), Some("UTC"));
        assert_eq!(abbrev_to_iana("gmt"), Some("UTC"));
        assert_eq!(abbrev_to_iana("Z"), Some("UTC"));
    }

    #[test]
    fn unknown_and_iana_names_are_not_abbreviations() {
        assert_eq!(abbrev_to_iana("America/Los_Angeles"), None);
        assert_eq!(abbrev_to_iana("Mars/Olympus"), None);
        assert_eq!(abbrev_to_iana(""), None);
    }

    #[test]
    fn canonicalize_maps_abbrev_and_passes_through_iana() {
        assert_eq!(canonicalize_timezone("PST"), "America/Los_Angeles");
        assert_eq!(
            canonicalize_timezone("  America/New_York  "),
            "America/New_York"
        );
        // Unknown input is preserved (trimmed) for the caller to validate.
        assert_eq!(canonicalize_timezone(" Invalid/Zone "), "Invalid/Zone");
    }
}
