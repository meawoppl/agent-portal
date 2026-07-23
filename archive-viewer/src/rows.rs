//! Manifest collection, flattening, filtering, and session-id resolution.
//!
//! Everything downstream (list / rollup / export / cat) works off a
//! `Vec<FlatRow>` gathered here. Collection is manifest-only — transcripts are
//! never read to build a row — and degrades gracefully: a corrupt manifest or
//! an unreadable user prefix is warned about on stderr and skipped rather than
//! aborting the whole scan.

use anyhow::{anyhow, Result};
use archive_format::{ArchiveStore, SessionArchiveManifest};
use chrono::{DateTime, NaiveDate, NaiveDateTime};
use uuid::Uuid;

/// Short session-id length used in tables, matching the launcher CLI.
pub const SHORT_ID_LEN: usize = 8;

/// One archived session, flattened to the (user, manifest) pair every command
/// reads from.
#[derive(Debug)]
pub struct FlatRow {
    pub user_id: Uuid,
    pub manifest: SessionArchiveManifest,
}

impl FlatRow {
    pub fn short_id(&self) -> String {
        self.manifest
            .session_id
            .simple()
            .to_string()
            .chars()
            .take(SHORT_ID_LEN)
            .collect()
    }

    /// Total messages across all roles.
    pub fn message_count(&self) -> i64 {
        self.manifest.message_counts.values().sum()
    }

    /// Number of archived media blobs.
    pub fn media_count(&self) -> usize {
        self.manifest.media.as_ref().map_or(0, Vec::len)
    }

    /// Total archived media bytes.
    pub fn media_bytes(&self) -> u64 {
        self.manifest
            .media
            .as_ref()
            .map_or(0, |m| m.iter().map(|e| e.bytes).sum())
    }

    /// Combined cache tokens (creation + read).
    pub fn cache_tokens(&self) -> i64 {
        self.manifest.tokens.cache_creation + self.manifest.tokens.cache_read
    }
}

/// Walk the archive and collect every readable manifest. Corrupt manifests
/// and unreadable per-user listings are logged to stderr and skipped; only a
/// failure to list users at all is fatal (nothing can be done without it).
pub fn collect_rows(store: &ArchiveStore) -> Result<Vec<FlatRow>> {
    let users = store
        .list_users()
        .map_err(|e| anyhow!("failed to list archive users: {e}"))?;

    let mut rows = Vec::new();
    for user_id in users {
        let sessions = match store.list_sessions(user_id) {
            Ok(sessions) => sessions,
            Err(e) => {
                eprintln!("warning: skipping user {user_id}: cannot list sessions: {e}");
                continue;
            }
        };
        for session_id in sessions {
            match store.get_session_manifest(user_id, session_id) {
                Ok(Some(manifest)) => rows.push(FlatRow { user_id, manifest }),
                // Missing manifest under a listed session dir: a partial or
                // in-progress write; nothing to show, skip silently.
                Ok(None) => {}
                Err(e) => {
                    eprintln!(
                        "warning: skipping corrupt manifest for session {session_id} \
                         (user {user_id}): {e}"
                    );
                }
            }
        }
    }
    Ok(rows)
}

/// Filters shared by `list` (and applied at collection time). All are
/// optional; an unset filter matches everything.
#[derive(Default)]
pub struct Filters {
    /// Substring match against `owner_email`, or a session/user UUID prefix.
    pub user: Option<String>,
    /// Exact agent-type match (`claude` / `codex`).
    pub agent: Option<String>,
    /// Substring match against `session_name` (case-insensitive).
    pub name: Option<String>,
    /// Activity window (matched against `last_activity`, the sort key).
    pub from: Option<NaiveDateTime>,
    pub to: Option<NaiveDateTime>,
}

impl Filters {
    pub fn matches(&self, row: &FlatRow) -> bool {
        let m = &row.manifest;
        if let Some(user) = &self.user {
            let needle = user.to_ascii_lowercase();
            let email_hit = m.owner_email.to_ascii_lowercase().contains(&needle);
            let uuid_hit = matches_uuid_prefix(m.user_id, &needle)
                || matches_uuid_prefix(m.session_id, &needle);
            if !email_hit && !uuid_hit {
                return false;
            }
        }
        if let Some(agent) = &self.agent {
            if !m.agent_type.eq_ignore_ascii_case(agent) {
                return false;
            }
        }
        if let Some(name) = &self.name {
            if !m
                .session_name
                .to_ascii_lowercase()
                .contains(&name.to_ascii_lowercase())
            {
                return false;
            }
        }
        if let Some(from) = self.from {
            if m.last_activity < from {
                return false;
            }
        }
        if let Some(to) = self.to {
            if m.last_activity > to {
                return false;
            }
        }
        true
    }
}

/// True when `id`'s hyphen-free hex rendering starts with `prefix` (already
/// lowercased, hyphens allowed and stripped).
fn matches_uuid_prefix(id: Uuid, prefix: &str) -> bool {
    let prefix = prefix.replace('-', "");
    if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    id.simple().to_string().starts_with(&prefix)
}

/// Apply filters and sort by `last_activity` descending (most recent first).
pub fn filter_and_sort(rows: Vec<FlatRow>, filters: &Filters) -> Vec<FlatRow> {
    let mut kept: Vec<FlatRow> = rows.into_iter().filter(|r| filters.matches(r)).collect();
    kept.sort_by(|a, b| b.manifest.last_activity.cmp(&a.manifest.last_activity));
    kept
}

/// Parse a `--from`/`--to` argument: RFC3339 (any offset, normalized to UTC)
/// or a bare `YYYY-MM-DD`. A bare date becomes start-of-day for `--from` and
/// end-of-day for `--to` (`end_of_day = true`) so a single date is an
/// inclusive full-day bound.
pub fn parse_date_arg(input: &str, end_of_day: bool) -> Result<NaiveDateTime> {
    let input = input.trim();
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Ok(dt.naive_utc());
    }
    if let Ok(date) = NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        let time = if end_of_day {
            date.and_hms_opt(23, 59, 59)
        } else {
            date.and_hms_opt(0, 0, 0)
        };
        return time.ok_or_else(|| anyhow!("invalid date `{input}`"));
    }
    Err(anyhow!(
        "invalid date `{input}`: expected RFC3339 (e.g. 2026-07-11T10:00:00Z) or YYYY-MM-DD"
    ))
}

/// Resolve a session-id prefix (hex, hyphens optional) across all collected
/// rows. Mirrors the launcher's `resolve_session_id`: unique prefix wins,
/// empty match errors, ambiguous match lists the candidates.
pub fn resolve_session<'a>(input: &str, rows: &'a [FlatRow]) -> Result<&'a FlatRow> {
    let prefix = normalize_prefix(input)?;
    let matches: Vec<&FlatRow> = rows
        .iter()
        .filter(|r| {
            r.manifest
                .session_id
                .simple()
                .to_string()
                .starts_with(&prefix)
        })
        .collect();
    match matches.as_slice() {
        [row] => Ok(row),
        [] => Err(anyhow!("no archived session matches `{}`", input.trim())),
        many => {
            let ids = many
                .iter()
                .map(|r| r.manifest.session_id.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            Err(anyhow!(
                "session id prefix `{}` is ambiguous; use more characters or a full id \
                 (matches: {ids})",
                input.trim()
            ))
        }
    }
}

fn normalize_prefix(input: &str) -> Result<String> {
    let prefix = input.trim().replace('-', "").to_ascii_lowercase();
    if prefix.is_empty() {
        return Err(anyhow!("session id prefix cannot be empty"));
    }
    if !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "session id prefix `{}` must contain only hex digits",
            input.trim()
        ));
    }
    Ok(prefix)
}

#[cfg(test)]
pub(crate) mod test_support {
    use archive_format::{
        ArchiveTokenTotals, ArchiveTurnStats, MediaEntry, SessionArchiveManifest,
        ARCHIVE_SCHEMA_VERSION,
    };
    use chrono::NaiveDateTime;
    use std::collections::BTreeMap;
    use uuid::Uuid;

    /// Build a manifest with sensible defaults; callers override fields.
    pub fn manifest(
        user_id: Uuid,
        session_id: Uuid,
        owner_email: &str,
        session_name: &str,
        agent_type: &str,
        last_activity: NaiveDateTime,
    ) -> SessionArchiveManifest {
        SessionArchiveManifest {
            schema_version: ARCHIVE_SCHEMA_VERSION,
            session_id,
            user_id,
            owner_email: owner_email.to_string(),
            owner_name: None,
            session_name: session_name.to_string(),
            agent_type: agent_type.to_string(),
            status: "disconnected".to_string(),
            working_directory: "/repo".to_string(),
            hostname: "host-1".to_string(),
            git_branch: None,
            repo_url: None,
            pr_url: None,
            client_version: None,
            created_at: last_activity,
            last_activity,
            archived_at: last_activity,
            message_counts: BTreeMap::new(),
            tokens: ArchiveTokenTotals::default(),
            total_cost_usd: 0.0,
            turns: ArchiveTurnStats::default(),
            transcript: None,
            media: None,
            launcher_id: None,
            launcher_version: None,
            scheduled_task_id: None,
            claude_args: Vec::new(),
            archived_by_version: None,
        }
    }

    pub fn media_entry(media_id: Uuid, bytes: u64) -> MediaEntry {
        MediaEntry {
            media_id,
            kind: "image".to_string(),
            content_type: "image/png".to_string(),
            bytes,
            object_key: format!("media/{media_id}"),
            uploaded_at: chrono::NaiveDate::from_ymd_opt(2026, 7, 11)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::manifest;
    use super::*;
    use chrono::{Datelike, NaiveDate};

    fn dt(day: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, day)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    // Put the day in the top nibble so each session id has a distinct leading
    // hex digit (small `from_u128` values are all leading zeros → ambiguous).
    fn session_for(day: u32) -> Uuid {
        Uuid::from_u128((day as u128) << 124)
    }

    fn row(email: &str, name: &str, agent: &str, day: u32) -> FlatRow {
        FlatRow {
            user_id: Uuid::from_u128(day as u128),
            manifest: manifest(
                Uuid::from_u128(day as u128),
                session_for(day),
                email,
                name,
                agent,
                dt(day),
            ),
        }
    }

    fn sample() -> Vec<FlatRow> {
        vec![
            row("alice@x.io", "refactor rail", "claude", 10),
            row("bob@y.io", "codex spike", "codex", 12),
            row("alice@x.io", "docs pass", "claude", 11),
        ]
    }

    #[test]
    fn sorts_by_last_activity_desc() {
        let out = filter_and_sort(sample(), &Filters::default());
        let days: Vec<u32> = out
            .iter()
            .map(|r| r.manifest.last_activity.date().day())
            .collect();
        assert_eq!(days, vec![12, 11, 10]);
    }

    #[test]
    fn filter_by_user_email_substring() {
        let filters = Filters {
            user: Some("alice".to_string()),
            ..Default::default()
        };
        let out = filter_and_sort(sample(), &filters);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.manifest.owner_email == "alice@x.io"));
    }

    #[test]
    fn filter_by_agent() {
        let filters = Filters {
            agent: Some("codex".to_string()),
            ..Default::default()
        };
        let out = filter_and_sort(sample(), &filters);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].manifest.agent_type, "codex");
    }

    #[test]
    fn filter_by_name_substring_case_insensitive() {
        let filters = Filters {
            name: Some("REFACTOR".to_string()),
            ..Default::default()
        };
        let out = filter_and_sort(sample(), &filters);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].manifest.session_name, "refactor rail");
    }

    #[test]
    fn filter_by_date_window_inclusive() {
        let filters = Filters {
            from: Some(parse_date_arg("2026-07-11", false).unwrap()),
            to: Some(parse_date_arg("2026-07-11", true).unwrap()),
            ..Default::default()
        };
        let out = filter_and_sort(sample(), &filters);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].manifest.session_name, "docs pass");
    }

    #[test]
    fn filter_by_user_uuid_prefix() {
        let rows = sample();
        // The day-12 (codex) session has a distinct leading hex nibble.
        let target = session_for(12).simple().to_string();
        let filters = Filters {
            user: Some(target[..4].to_string()),
            ..Default::default()
        };
        let out = filter_and_sort(rows, &filters);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].manifest.agent_type, "codex");
    }

    #[test]
    fn parse_date_rfc3339_and_plain() {
        assert_eq!(
            parse_date_arg("2026-07-11T10:30:00Z", false).unwrap(),
            NaiveDate::from_ymd_opt(2026, 7, 11)
                .unwrap()
                .and_hms_opt(10, 30, 0)
                .unwrap()
        );
        assert_eq!(
            parse_date_arg("2026-07-11", true).unwrap(),
            NaiveDate::from_ymd_opt(2026, 7, 11)
                .unwrap()
                .and_hms_opt(23, 59, 59)
                .unwrap()
        );
        assert!(parse_date_arg("not-a-date", false).is_err());
    }

    fn row_with_session(session: Uuid) -> FlatRow {
        FlatRow {
            user_id: Uuid::from_u128(1),
            manifest: manifest(Uuid::from_u128(1), session, "a@a", "s", "claude", dt(10)),
        }
    }

    #[test]
    fn resolve_unique_prefix() {
        let rows = sample();
        let full = session_for(10).simple().to_string();
        let resolved = resolve_session(&full[..4], &rows).unwrap();
        assert_eq!(resolved.manifest.session_id, session_for(10));
    }

    #[test]
    fn resolve_missing_prefix_errors() {
        assert!(resolve_session("ffffffff", &sample()).is_err());
    }

    #[test]
    fn resolve_ambiguous_prefix_errors() {
        // Two ids sharing the leading hex nibble `1...`.
        let rows = vec![
            row_with_session(Uuid::from_u128(0x1000_0000_0000_0000_0000_0000_0000_0001)),
            row_with_session(Uuid::from_u128(0x1000_0000_0000_0000_0000_0000_0000_0002)),
        ];
        let err = resolve_session("1000", &rows).unwrap_err().to_string();
        assert!(err.contains("ambiguous"), "got: {err}");
    }
}
