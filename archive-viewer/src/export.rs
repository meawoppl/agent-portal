//! `export` — flatten every manifest row (all fields, including provenance)
//! to CSV or JSON. Same row shape as `list`, plus the full token/turn
//! breakdown and provenance columns.

use serde::Serialize;

use crate::rows::FlatRow;

/// Output format for `export`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Format {
    Csv,
    Json,
}

/// A fully-flattened manifest row. Every field is a scalar (or a small joined
/// string) so it serializes cleanly to both CSV and JSON. `Option` renders as
/// an empty CSV cell / JSON `null`.
#[derive(Serialize)]
pub struct ExportRow {
    pub session_id: String,
    pub user_id: String,
    pub owner_email: String,
    pub owner_name: Option<String>,
    pub session_name: String,
    pub agent_type: String,
    pub status: String,
    pub working_directory: String,
    pub hostname: String,
    pub git_branch: Option<String>,
    pub repo_url: Option<String>,
    pub pr_url: Option<String>,
    pub created_at: String,
    pub last_activity: String,
    pub archived_at: String,
    pub message_count: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub thinking_tokens: i64,
    pub subagent_tokens: i64,
    pub total_cost_usd: f64,
    pub turn_count: i64,
    pub turn_errored: i64,
    pub tool_calls: i64,
    pub stream_restarts: i64,
    pub total_duration_ms: i64,
    /// Distinct models, joined with `|`.
    pub models: String,
    pub media_count: usize,
    pub media_bytes: u64,
    // --- Provenance ---
    pub client_version: Option<String>,
    pub launcher_id: Option<String>,
    pub launcher_version: Option<String>,
    pub scheduled_task_id: Option<String>,
    /// Extra agent CLI args, joined with a space.
    pub claude_args: String,
    pub archived_by_version: Option<String>,
}

impl ExportRow {
    pub fn from_row(row: &FlatRow) -> Self {
        let m = &row.manifest;
        Self {
            session_id: m.session_id.to_string(),
            user_id: m.user_id.to_string(),
            owner_email: m.owner_email.clone(),
            owner_name: m.owner_name.clone(),
            session_name: m.session_name.clone(),
            agent_type: m.agent_type.clone(),
            status: m.status.clone(),
            working_directory: m.working_directory.clone(),
            hostname: m.hostname.clone(),
            git_branch: m.git_branch.clone(),
            repo_url: m.repo_url.clone(),
            pr_url: m.pr_url.clone(),
            created_at: fmt(&m.created_at),
            last_activity: fmt(&m.last_activity),
            archived_at: fmt(&m.archived_at),
            message_count: row.message_count(),
            input_tokens: m.tokens.input,
            output_tokens: m.tokens.output,
            cache_creation_tokens: m.tokens.cache_creation,
            cache_read_tokens: m.tokens.cache_read,
            thinking_tokens: m.tokens.thinking,
            subagent_tokens: m.tokens.subagent,
            total_cost_usd: m.total_cost_usd,
            turn_count: m.turns.count,
            turn_errored: m.turns.errored,
            tool_calls: m.turns.tool_calls,
            stream_restarts: m.turns.stream_restarts,
            total_duration_ms: m.turns.total_duration_ms,
            models: m.turns.models.join("|"),
            media_count: row.media_count(),
            media_bytes: row.media_bytes(),
            client_version: m.client_version.clone(),
            launcher_id: m.launcher_id.map(|id| id.to_string()),
            launcher_version: m.launcher_version.clone(),
            scheduled_task_id: m.scheduled_task_id.map(|id| id.to_string()),
            claude_args: m.claude_args.join(" "),
            archived_by_version: m.archived_by_version.clone(),
        }
    }

    /// CSV cells in `CSV_HEADERS` order.
    fn csv_cells(&self) -> Vec<String> {
        vec![
            self.session_id.clone(),
            self.user_id.clone(),
            self.owner_email.clone(),
            opt(&self.owner_name),
            self.session_name.clone(),
            self.agent_type.clone(),
            self.status.clone(),
            self.working_directory.clone(),
            self.hostname.clone(),
            opt(&self.git_branch),
            opt(&self.repo_url),
            opt(&self.pr_url),
            self.created_at.clone(),
            self.last_activity.clone(),
            self.archived_at.clone(),
            self.message_count.to_string(),
            self.input_tokens.to_string(),
            self.output_tokens.to_string(),
            self.cache_creation_tokens.to_string(),
            self.cache_read_tokens.to_string(),
            self.thinking_tokens.to_string(),
            self.subagent_tokens.to_string(),
            format!("{:.6}", self.total_cost_usd),
            self.turn_count.to_string(),
            self.turn_errored.to_string(),
            self.tool_calls.to_string(),
            self.stream_restarts.to_string(),
            self.total_duration_ms.to_string(),
            self.models.clone(),
            self.media_count.to_string(),
            self.media_bytes.to_string(),
            opt(&self.client_version),
            opt(&self.launcher_id),
            opt(&self.launcher_version),
            opt(&self.scheduled_task_id),
            self.claude_args.clone(),
            opt(&self.archived_by_version),
        ]
    }
}

pub const CSV_HEADERS: &[&str] = &[
    "session_id",
    "user_id",
    "owner_email",
    "owner_name",
    "session_name",
    "agent_type",
    "status",
    "working_directory",
    "hostname",
    "git_branch",
    "repo_url",
    "pr_url",
    "created_at",
    "last_activity",
    "archived_at",
    "message_count",
    "input_tokens",
    "output_tokens",
    "cache_creation_tokens",
    "cache_read_tokens",
    "thinking_tokens",
    "subagent_tokens",
    "total_cost_usd",
    "turn_count",
    "turn_errored",
    "tool_calls",
    "stream_restarts",
    "total_duration_ms",
    "models",
    "media_count",
    "media_bytes",
    "client_version",
    "launcher_id",
    "launcher_version",
    "scheduled_task_id",
    "claude_args",
    "archived_by_version",
];

/// Serialize `rows` in `format`. JSON is a pretty array; CSV is RFC4180 with a
/// header line. Returns an error only if JSON serialization somehow fails.
pub fn render(rows: &[FlatRow], format: Format) -> anyhow::Result<String> {
    let export: Vec<ExportRow> = rows.iter().map(ExportRow::from_row).collect();
    match format {
        Format::Json => Ok(serde_json::to_string_pretty(&export)?),
        Format::Csv => Ok(render_csv(&export)),
    }
}

fn render_csv(rows: &[ExportRow]) -> String {
    let mut out = String::new();
    out.push_str(
        &CSV_HEADERS
            .iter()
            .map(|h| escape_csv(h))
            .collect::<Vec<_>>()
            .join(","),
    );
    for row in rows {
        out.push('\n');
        out.push_str(
            &row.csv_cells()
                .iter()
                .map(|c| escape_csv(c))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    out
}

/// RFC4180 field escaping: quote when the field contains a comma, quote,
/// CR, or LF; internal quotes are doubled.
fn escape_csv(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

fn opt(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}

fn fmt(dt: &chrono::NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
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

    fn sample_row() -> FlatRow {
        let mut m = manifest(
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            "dev@x.io",
            "name, with comma",
            "claude",
            dt(),
        );
        m.tokens.input = 100;
        m.total_cost_usd = 1.25;
        m.turns.models = vec!["opus".to_string(), "sonnet".to_string()];
        m.client_version = Some("2.13.5".to_string());
        m.archived_by_version = Some("2.13.9".to_string());
        FlatRow {
            user_id: Uuid::from_u128(1),
            manifest: m,
        }
    }

    #[test]
    fn csv_has_header_and_quotes_commas() {
        let out = render(&[sample_row()], Format::Csv).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], CSV_HEADERS.join(","));
        assert_eq!(lines.len(), 2);
        // The comma-containing name must be quoted.
        assert!(lines[1].contains("\"name, with comma\""));
        // Joined models and provenance are present.
        assert!(lines[1].contains("opus|sonnet"));
        assert!(lines[1].contains("2.13.5"));
        assert!(lines[1].contains("2.13.9"));
    }

    #[test]
    fn json_is_array_of_objects_with_expected_fields() {
        let out = render(&[sample_row()], Format::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        let obj = &arr[0];
        assert_eq!(obj["owner_email"], "dev@x.io");
        assert_eq!(obj["input_tokens"], 100);
        assert_eq!(obj["models"], "opus|sonnet");
        assert_eq!(obj["client_version"], "2.13.5");
        // Unset provenance is JSON null.
        assert!(obj["launcher_id"].is_null());
    }

    #[test]
    fn escape_csv_doubles_internal_quotes() {
        assert_eq!(escape_csv("a\"b"), "\"a\"\"b\"");
        assert_eq!(escape_csv("plain"), "plain");
    }
}
