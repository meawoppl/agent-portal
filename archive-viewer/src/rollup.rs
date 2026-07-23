//! `rollup` — aggregate manifest metrics into a grouped table.

use std::collections::BTreeMap;

use crate::rows::FlatRow;
use crate::table;

/// What to group rows by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum GroupBy {
    /// Group by owner email.
    User,
    /// Group by agent type (`claude` / `codex`).
    Agent,
    /// Group by model. A session that used multiple models contributes its
    /// full totals to *each* model's row (manifests carry no per-model token
    /// split), so per-model sessions/tokens can exceed the grand total.
    Model,
}

#[derive(Default)]
struct Agg {
    sessions: i64,
    turns: i64,
    input: i64,
    output: i64,
    cache: i64,
    thinking: i64,
    cost: f64,
    tool_calls: i64,
    media_bytes: u64,
}

impl Agg {
    fn add(&mut self, row: &FlatRow) {
        let m = &row.manifest;
        self.sessions += 1;
        self.turns += m.turns.count;
        self.input += m.tokens.input;
        self.output += m.tokens.output;
        self.cache += row.cache_tokens();
        self.thinking += m.tokens.thinking;
        self.cost += m.total_cost_usd;
        self.tool_calls += m.turns.tool_calls;
        self.media_bytes += row.media_bytes();
    }
}

const HEADERS: &[&str] = &[
    "GROUP",
    "SESSIONS",
    "TURNS",
    "INPUT",
    "OUTPUT",
    "CACHE",
    "THINKING",
    "COST",
    "TOOLS",
    "MEDIA_BYTES",
];

/// Aggregate `rows` by `group_by` and render the table. Returns a friendly
/// message when there is nothing to aggregate.
pub fn render(rows: &[FlatRow], group_by: GroupBy) -> String {
    if rows.is_empty() {
        return "No archived sessions to roll up.".to_string();
    }
    let mut groups: BTreeMap<String, Agg> = BTreeMap::new();
    for row in rows {
        for key in group_keys(row, group_by) {
            groups.entry(key).or_default().add(row);
        }
    }

    let table_rows: Vec<Vec<String>> = groups
        .iter()
        .map(|(key, a)| {
            vec![
                key.clone(),
                a.sessions.to_string(),
                a.turns.to_string(),
                a.input.to_string(),
                a.output.to_string(),
                a.cache.to_string(),
                a.thinking.to_string(),
                format!("${:.4}", a.cost),
                a.tool_calls.to_string(),
                a.media_bytes.to_string(),
            ]
        })
        .collect();
    table::render(HEADERS, &table_rows)
}

/// The group key(s) a row belongs to. Only `Model` can yield more than one.
fn group_keys(row: &FlatRow, group_by: GroupBy) -> Vec<String> {
    let m = &row.manifest;
    match group_by {
        GroupBy::User => vec![m.owner_email.clone()],
        GroupBy::Agent => vec![m.agent_type.clone()],
        GroupBy::Model => {
            if m.turns.models.is_empty() {
                vec!["(no model)".to_string()]
            } else {
                m.turns.models.clone()
            }
        }
    }
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
            .and_hms_opt(0, 0, 0)
            .unwrap()
    }

    fn row(email: &str, agent: &str, models: &[&str]) -> FlatRow {
        let mut m = manifest(Uuid::new_v4(), Uuid::new_v4(), email, "s", agent, dt());
        m.turns.count = 3;
        m.turns.tool_calls = 5;
        m.tokens.input = 100;
        m.tokens.output = 40;
        m.tokens.cache_creation = 10;
        m.tokens.cache_read = 20;
        m.tokens.thinking = 7;
        m.total_cost_usd = 0.25;
        m.turns.models = models.iter().map(|s| s.to_string()).collect();
        m.media = Some(vec![crate::rows::test_support::media_entry(
            Uuid::new_v4(),
            1024,
        )]);
        FlatRow {
            user_id: Uuid::new_v4(),
            manifest: m,
        }
    }

    #[test]
    fn groups_by_user_and_sums() {
        let rows = vec![
            row("a@x", "claude", &["opus"]),
            row("a@x", "claude", &["opus"]),
            row("b@x", "codex", &["gpt-5"]),
        ];
        let out = render(&rows, GroupBy::User);
        let lines: Vec<&str> = out.lines().collect();
        // a@x: 2 sessions, input 200, cache 60, cost 0.50, media 2048.
        let a_line = lines.iter().find(|l| l.starts_with("a@x")).unwrap();
        assert!(a_line.contains(" 2 "), "sessions: {a_line}");
        assert!(a_line.contains("200"), "input: {a_line}");
        assert!(a_line.contains("60"), "cache: {a_line}");
        assert!(a_line.contains("$0.5000"), "cost: {a_line}");
        assert!(a_line.contains("2048"), "media bytes: {a_line}");
    }

    #[test]
    fn groups_by_agent() {
        let rows = vec![
            row("a@x", "claude", &["opus"]),
            row("b@x", "codex", &["gpt-5"]),
        ];
        let out = render(&rows, GroupBy::Agent);
        assert!(out.lines().any(|l| l.starts_with("claude")));
        assert!(out.lines().any(|l| l.starts_with("codex")));
    }

    #[test]
    fn multi_model_session_counts_in_each_model() {
        let rows = vec![row("a@x", "claude", &["opus", "sonnet"])];
        let out = render(&rows, GroupBy::Model);
        let opus = out.lines().find(|l| l.starts_with("opus")).unwrap();
        let sonnet = out.lines().find(|l| l.starts_with("sonnet")).unwrap();
        // The single session contributes its full turn count to both models.
        assert!(opus.contains(" 3 "), "{opus}");
        assert!(sonnet.contains(" 3 "), "{sonnet}");
    }

    #[test]
    fn empty_is_friendly() {
        assert_eq!(
            render(&[], GroupBy::User),
            "No archived sessions to roll up."
        );
    }
}
