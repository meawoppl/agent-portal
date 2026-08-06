//! Filter state for the history browser, and its translation to query params.
//!
//! Filtering, sorting and paging all happen server-side: `/api/history/sessions`
//! returns one page of already-filtered rows plus the whole-set totals. This
//! module therefore only models the control state and how to ask for it — it
//! deliberately does **not** filter rows in the browser, because the client only
//! ever holds a single page and filtering that would silently narrow one page
//! instead of the archive.

/// Active filter selections from the browser controls. Empty/`None` fields
/// are "no constraint".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionFilter {
    /// Exact `user_id` match (admin-only control). Sent as `user`, which the
    /// backend matches as an email substring *or* a UUID prefix — a full UUID
    /// is a prefix of itself, so an exact id works.
    pub user_id: Option<String>,
    /// Exact `agent_type` match (e.g. "claude", "codex").
    pub agent_type: Option<String>,
    /// Inclusive lower bound on `last_activity` (`YYYY-MM-DD` or RFC3339).
    pub from: Option<String>,
    /// Inclusive upper bound on `last_activity`; the backend widens a bare
    /// `YYYY-MM-DD` to the end of that day.
    pub to: Option<String>,
    /// Case-insensitive substring match on `session_name`.
    pub query: Option<String>,
}

impl SessionFilter {
    /// Build the `/api/history/sessions` query string for this filter and page.
    ///
    /// Values are percent-encoded; blank/whitespace-only fields are omitted
    /// rather than sent empty, so a cleared control is a removed constraint
    /// instead of a match-nothing one.
    pub fn to_query(&self, offset: usize, limit: usize) -> String {
        let mut parts = vec![format!("limit={limit}"), format!("offset={offset}")];
        for (key, value) in [
            ("user", &self.user_id),
            ("agent", &self.agent_type),
            ("from", &self.from),
            ("to", &self.to),
            ("q", &self.query),
        ] {
            if let Some(v) = non_empty(value) {
                parts.push(format!("{key}={}", encode_component(v)));
            }
        }
        parts.join("&")
    }
}

fn non_empty(opt: &Option<String>) -> Option<&str> {
    opt.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// Percent-encode everything outside the unreserved set. Hand-rolled to keep
/// `shared`/frontend free of a URL-encoding dependency for five short values.
fn encode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_filter_sends_only_paging() {
        assert_eq!(
            SessionFilter::default().to_query(0, 50),
            "limit=50&offset=0"
        );
    }

    #[test]
    fn set_fields_are_appended_in_a_stable_order() {
        let f = SessionFilter {
            user_id: Some("u1".into()),
            agent_type: Some("codex".into()),
            from: Some("2026-07-01".into()),
            to: Some("2026-07-31".into()),
            query: Some("refactor".into()),
        };
        assert_eq!(
            f.to_query(100, 50),
            "limit=50&offset=100&user=u1&agent=codex&from=2026-07-01&to=2026-07-31&q=refactor"
        );
    }

    #[test]
    fn blank_and_whitespace_fields_are_omitted_not_sent_empty() {
        let f = SessionFilter {
            user_id: Some("   ".into()),
            query: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(f.to_query(0, 50), "limit=50&offset=0");
    }

    #[test]
    fn special_characters_are_percent_encoded() {
        let f = SessionFilter {
            query: Some("fix & ship/now?".into()),
            ..Default::default()
        };
        assert_eq!(
            f.to_query(0, 50),
            "limit=50&offset=0&q=fix%20%26%20ship%2Fnow%3F"
        );
    }
}
