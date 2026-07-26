//! CLI-side glue over [`archive_format::scan`]: anyhow-flavored wrappers with
//! stderr warning output, plus session-id prefix resolution for `cat`.

use anyhow::{anyhow, Context, Result};
use archive_format::scan;
use archive_format::ArchiveStore;
use chrono::NaiveDateTime;

pub use archive_format::scan::{filter_and_sort, Filters, FlatRow};

#[cfg(test)]
pub(crate) use archive_format::scan::test_support;

/// Collect every readable manifest, warning on stderr for skipped entries.
pub fn collect_rows(store: &ArchiveStore) -> Result<Vec<FlatRow>> {
    scan::collect_rows(store, &mut |w| eprintln!("warning: {w}"))
        .context("failed to list archive users")
}

/// Parse a `--from`/`--to` argument (RFC3339 or `YYYY-MM-DD`).
pub fn parse_date_arg(input: &str, end_of_day: bool) -> Result<NaiveDateTime> {
    scan::parse_date_arg(input, end_of_day).map_err(|e| anyhow!(e))
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
mod tests {
    use super::test_support::manifest;
    use super::*;
    use chrono::NaiveDate;
    use uuid::Uuid;

    fn dt(day: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, day)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

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
