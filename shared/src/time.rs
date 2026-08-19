//! Shared ISO timestamp helpers — WASM-compatible.

/// Append a `Z` when an ISO timestamp carries no timezone designator, so it is
/// read as UTC rather than local time. A designator is a `Z` or a `+`/`-`
/// offset in the time portion (after `T`); date hyphens don't count.
///
/// Previously duplicated as a `normalize_iso_utc` helper in
/// `frontend/src/pages/dashboard/session_view/helpers.rs` — at this head
/// `grep` finds no `normalize_iso_utc` or equivalent `T`-plus-`Z` logic in
/// `backend/src/handlers/messages.rs` or `web_client_socket.rs`, so the
/// frontend was the only production caller to wire. One `shared::time`
/// implementation keeps the `js_sys::Date::parse` UTC fix (`left: 8000%`
/// sparkline bug) in a single test suite.
///
/// Date-only values (no `T`, e.g. `"2026-05-17"`) are left untouched — the
/// old local helper appended `Z` to them (`"2026-05-17Z"`), but a bare date
/// is already unambiguous and never appears as a `created_at` wire value;
/// the new helper returns it `Borrowed` as pinned by the `appends_z_only`
/// test.
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
