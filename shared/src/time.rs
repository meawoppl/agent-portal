//! Shared ISO timestamp helpers — WASM-compatible.

/// Append a `Z` when an ISO timestamp carries no timezone designator, so it is
/// read as UTC rather than local time. A designator is a `Z` or a `+`/`-`
/// offset in the time portion (after `T`); date hyphens don't count.
/// Date-only values (no `T`) are left untouched and returned as `Borrowed`.
#[must_use]
pub fn normalize_iso_utc(iso: &str) -> std::borrow::Cow<'_, str> {
    let Some((_, time)) = iso.split_once('T') else {
        return std::borrow::Cow::Borrowed(iso);
    };
    let has_tz = time.contains(['Z', '+', '-']);
    if has_tz {
        std::borrow::Cow::Borrowed(iso)
    } else {
        std::borrow::Cow::Owned(format!("{iso}Z"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_z_only_when_no_timezone() {
        assert_eq!(
            normalize_iso_utc("2026-05-17T12:34:56.789"),
            "2026-05-17T12:34:56.789Z"
        );
        assert_eq!(
            normalize_iso_utc("2026-05-17T12:34:56"),
            "2026-05-17T12:34:56Z"
        );
        assert_eq!(
            normalize_iso_utc("2026-05-17T12:34:56Z"),
            "2026-05-17T12:34:56Z"
        );
        assert_eq!(
            normalize_iso_utc("2026-05-17T12:34:56+00:00"),
            "2026-05-17T12:34:56+00:00"
        );
        assert_eq!(
            normalize_iso_utc("2026-05-17T12:34:56-05:00"),
            "2026-05-17T12:34:56-05:00"
        );
        assert_eq!(normalize_iso_utc("2026-05-17"), "2026-05-17");
    }
}
