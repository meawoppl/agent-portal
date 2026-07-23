//! `list` — one table row per archived session, manifest-only.

use crate::rows::FlatRow;
use crate::table;

const HEADERS: &[&str] = &[
    "SESSION", "NAME", "AGENT", "STATUS", "USER", "HOST", "CREATED", "ACTIVITY", "MSGS", "COST",
    "MODELS", "MEDIA",
];

/// Render the list table for `rows` (already filtered and sorted). Returns a
/// friendly message instead of an empty table when there is nothing to show.
pub fn render(rows: &[FlatRow]) -> String {
    if rows.is_empty() {
        return "No archived sessions match.".to_string();
    }
    let table_rows: Vec<Vec<String>> = rows.iter().map(row_cells).collect();
    table::render(HEADERS, &table_rows)
}

fn row_cells(row: &FlatRow) -> Vec<String> {
    let m = &row.manifest;
    vec![
        row.short_id(),
        truncate(&m.session_name, 30),
        m.agent_type.clone(),
        m.status.clone(),
        truncate(&m.owner_email, 24),
        truncate(&m.hostname, 16),
        fmt_dt(&m.created_at),
        fmt_dt(&m.last_activity),
        row.message_count().to_string(),
        format!("${:.4}", m.total_cost_usd),
        if m.turns.models.is_empty() {
            "-".to_string()
        } else {
            m.turns.models.join(",")
        },
        row.media_count().to_string(),
    ]
}

/// Format a manifest timestamp (stored UTC-naive) as `YYYY-MM-DD HH:MM`.
pub fn fmt_dt(dt: &chrono::NaiveDateTime) -> String {
    dt.format("%Y-%m-%d %H:%M").to_string()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let kept: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::test_support::manifest;
    use crate::rows::FlatRow;
    use chrono::NaiveDate;
    use uuid::Uuid;

    fn dt() -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, 11)
            .unwrap()
            .and_hms_opt(9, 30, 0)
            .unwrap()
    }

    #[test]
    fn empty_is_friendly() {
        assert_eq!(render(&[]), "No archived sessions match.");
    }

    #[test]
    fn row_shows_short_id_name_agent_and_cost() {
        let mut m = manifest(
            Uuid::from_u128(1),
            Uuid::from_u128(0xabcdef12),
            "dev@x.io",
            "my session",
            "claude",
            dt(),
        );
        m.total_cost_usd = 1.5;
        m.turns.models = vec!["opus".to_string(), "sonnet".to_string()];
        let rows = vec![FlatRow {
            user_id: Uuid::from_u128(1),
            manifest: m,
        }];
        let out = render(&rows);
        assert!(out.contains("my session"));
        assert!(out.contains("claude"));
        assert!(out.contains("dev@x.io"));
        assert!(out.contains("$1.5000"));
        assert!(out.contains("opus,sonnet"));
        // Short id is the first 8 hex chars of the simple form.
        assert!(out.contains(&Uuid::from_u128(0xabcdef12).simple().to_string()[..8]));
    }
}
