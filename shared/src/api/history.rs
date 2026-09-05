//! Session-history (archive browser) API types.
//!
//! `GET /api/history/*` exposes the long-term session archive to the portal
//! frontend, visibility-scoped per user (own sessions + sessions shared via
//! `session_members` + everything for admins). The list endpoint returns
//! typed rows built server-side; the manifest and NDJSON transcript endpoints
//! stream the archived objects verbatim, so their mirrors here are permissive
//! (`#[serde(default)]` everywhere, unknown fields ignored) — an older or
//! newer manifest schema must still populate the UI.

use serde::{Deserialize, Serialize};

/// Rows per page when the caller doesn't say.
pub const DEFAULT_HISTORY_PAGE_SIZE: usize = 50;
/// Upper bound on `limit`, so a caller can't ask for the whole archive again.
pub const MAX_HISTORY_PAGE_SIZE: usize = 200;

/// `GET /api/history/sessions` response — **one page** of rows plus the
/// whole-result-set facts the UI needs alongside it.
///
/// The page alone isn't enough to render the browser: the stats strip totals the
/// entire filtered set, and the admin user dropdown lists every owner. Deriving
/// either from `sessions` would silently start describing one page, so both are
/// computed server-side over all matching rows and returned here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistorySessionsResponse {
    /// This page of archived sessions, most recently active first.
    pub sessions: Vec<HistorySessionSummary>,
    /// Whether the caller is an admin (drives the all-users filter UI).
    pub is_admin: bool,
    /// Rows matching the filter *before* paging — drives the page count.
    #[serde(default)]
    pub total: i64,
    /// Aggregates over every matching row, not just this page.
    #[serde(default)]
    pub totals: HistoryTotals,
    /// Per-owner rollup, admin-only, computed over rows matching every filter
    /// **except** `user`.
    ///
    /// That one exclusion lets a single list serve both consumers: the dropdown
    /// shows every owner (so picking one doesn't shrink the options), and the
    /// per-user tiles stay correct because selecting a user just narrows this
    /// list to that entry.
    #[serde(default)]
    pub owners: Vec<HistoryOwnerRollup>,
}

/// Totals across every row matching the active filter.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct HistoryTotals {
    #[serde(default)]
    pub session_count: i64,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub total_cost_usd: f64,
}

/// One owner's slice of the filtered set (admin stats tiles + user dropdown).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryOwnerRollup {
    pub user_id: String,
    /// Display name, falling back to email then id — resolved server-side so
    /// the label is stable even when the owner has no session on this page.
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub session_count: i64,
    #[serde(default)]
    pub total_cost_usd: f64,
}

/// One archived session the caller may view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistorySessionSummary {
    pub session_id: String,
    /// Archive owner — needed to address the per-session endpoints
    /// (`/api/history/sessions/{user}/{session}/…`).
    pub user_id: String,
    #[serde(default)]
    pub owner_email: String,
    #[serde(default)]
    pub owner_name: Option<String>,
    #[serde(default)]
    pub session_name: String,
    #[serde(default)]
    pub agent_type: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub hostname: String,
    /// `YYYY-MM-DDTHH:MM:SS` (UTC, no offset suffix).
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub last_activity: String,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub message_count: i64,
    /// Substantive user messages (tool results and reinjected notice blocks
    /// excluded — `shared::user_messages`). `None` until the archive manifest
    /// has the count (written at archive time; backfilled when the session's
    /// transcript is next viewed).
    #[serde(default)]
    pub user_message_count: Option<i64>,
    #[serde(default)]
    pub media_count: i64,
    #[serde(default)]
    pub models: Vec<String>,
}

/// Token totals sub-object of the manifest (mirrors `ArchiveTokenTotals`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HistoryTokenTotals {
    #[serde(default)]
    pub input: i64,
    #[serde(default)]
    pub output: i64,
    #[serde(default)]
    pub cache_creation: i64,
    #[serde(default)]
    pub cache_read: i64,
    #[serde(default)]
    pub thinking: i64,
    #[serde(default)]
    pub subagent: i64,
}

impl HistoryTokenTotals {
    pub fn total(&self) -> i64 {
        self.input + self.output + self.cache_creation + self.cache_read
    }
}

/// Permissive mirror of the archived `SessionArchiveManifest`
/// (`GET /api/history/sessions/{user}/{session}/manifest` returns the stored
/// JSON verbatim). Only the fields the transcript header renders.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct HistoryManifest {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub owner_email: String,
    #[serde(default)]
    pub owner_name: Option<String>,
    #[serde(default)]
    pub session_name: String,
    #[serde(default)]
    pub agent_type: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub working_directory: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default)]
    pub pr_url: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub last_activity: String,
    #[serde(default)]
    pub archived_at: String,
    #[serde(default)]
    pub tokens: HistoryTokenTotals,
    #[serde(default)]
    pub total_cost_usd: f64,
    #[serde(default)]
    pub launcher_version: Option<String>,
    #[serde(default)]
    pub archived_by_version: Option<String>,
}

/// One transcript line from the NDJSON stream
/// (`GET /api/history/sessions/{user}/{session}/messages`); permissive mirror
/// of the archived `ArchiveMessageLine`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryMessageLine {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub agent_type: String,
    /// Raw stored message content, embedded as a JSON value.
    #[serde(default)]
    pub content: serde_json::Value,
}

/// Parse an NDJSON transcript body into message lines, skipping blank and
/// unparseable lines (a corrupt line must not lose the whole transcript).
pub fn parse_history_ndjson(body: &str) -> Vec<HistoryMessageLine> {
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<HistoryMessageLine>(line).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ndjson_skips_blank_and_bad_lines() {
        let body = "\
{\"id\":\"a\",\"role\":\"user\",\"created_at\":\"t1\",\"agent_type\":\"claude\",\"content\":{\"type\":\"user\"}}

not json
{\"id\":\"b\",\"role\":\"assistant\",\"created_at\":\"t2\",\"agent_type\":\"claude\",\"content\":{}}
";
        let lines = parse_history_ndjson(body);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].id, "a");
        assert_eq!(lines[1].role, "assistant");
    }

    #[test]
    fn manifest_mirror_tolerates_unknown_and_missing_fields() {
        let json = r#"{
            "schema_version": 1,
            "session_id": "s",
            "user_id": "u",
            "session_name": "refactor",
            "agent_type": "claude",
            "some_future_field": {"nested": true}
        }"#;
        let m: HistoryManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.session_name, "refactor");
        assert_eq!(m.tokens, HistoryTokenTotals::default());
        assert_eq!(m.total_cost_usd, 0.0);
    }
}
