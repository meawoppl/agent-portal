//! Client-side filtering and sorting for the session browser.
//!
//! At v1 archive scale the whole session list is fetched once and filtered in
//! the browser (the pinned contract also accepts server-side query params, but
//! doing it client-side keeps the browser responsive while typing).

use crate::api::SessionSummary;

/// Active filter selections from the browser controls. Empty/`None` fields are
/// "no constraint".
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionFilter {
    /// Exact `user_id` match.
    pub user_id: Option<String>,
    /// Exact `agent_type` match (e.g. "claude", "codex").
    pub agent_type: Option<String>,
    /// Inclusive lower bound on `last_activity` (ISO date/datetime prefix).
    pub from: Option<String>,
    /// Inclusive upper bound on `last_activity` (ISO date prefix; compared as
    /// `< from_next_day` via a trailing high sentinel so a bare `YYYY-MM-DD`
    /// upper bound includes that whole day).
    pub to: Option<String>,
    /// Case-insensitive substring match on `session_name`.
    pub query: Option<String>,
}

impl SessionFilter {
    fn matches(&self, s: &SessionSummary) -> bool {
        if let Some(user) = non_empty(&self.user_id) {
            if s.user_id != *user {
                return false;
            }
        }
        if let Some(agent) = non_empty(&self.agent_type) {
            if s.agent_type != *agent {
                return false;
            }
        }
        if let Some(from) = non_empty(&self.from) {
            if s.last_activity.as_str() < from.as_str() {
                return false;
            }
        }
        if let Some(to) = non_empty(&self.to) {
            // Treat a date-only upper bound inclusively: everything on that day
            // sorts before "<date>~" (`~` > any ISO time char).
            let upper = format!("{to}~");
            if s.last_activity.as_str() >= upper.as_str() {
                return false;
            }
        }
        if let Some(q) = non_empty(&self.query) {
            if !s.session_name.to_lowercase().contains(&q.to_lowercase()) {
                return false;
            }
        }
        true
    }
}

fn non_empty(opt: &Option<String>) -> Option<&String> {
    opt.as_ref().filter(|s| !s.trim().is_empty())
}

/// Filter then sort by `last_activity` descending (newest first). Ties break on
/// `session_name` for a stable, human-predictable order.
pub fn filter_and_sort(sessions: &[SessionSummary], filter: &SessionFilter) -> Vec<SessionSummary> {
    let mut out: Vec<SessionSummary> = sessions
        .iter()
        .filter(|s| filter.matches(s))
        .cloned()
        .collect();
    out.sort_by(|a, b| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.session_name.cmp(&b.session_name))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str, user: &str, agent: &str, last: &str) -> SessionSummary {
        SessionSummary {
            session_id: format!("sess-{name}"),
            user_id: user.into(),
            session_name: name.into(),
            agent_type: agent.into(),
            status: "archived".into(),
            hostname: "host".into(),
            created_at: last.into(),
            last_activity: last.into(),
            total_cost_usd: 0.0,
            message_count: 0,
            media_count: 0,
            models: vec![],
        }
    }

    fn corpus() -> Vec<SessionSummary> {
        vec![
            session("alpha", "u1", "claude", "2026-07-01T10:00:00"),
            session("beta", "u2", "codex", "2026-07-03T10:00:00"),
            session("gamma", "u1", "codex", "2026-07-02T10:00:00"),
        ]
    }

    #[test]
    fn sorts_by_last_activity_desc() {
        let out = filter_and_sort(&corpus(), &SessionFilter::default());
        let names: Vec<_> = out.iter().map(|s| s.session_name.as_str()).collect();
        assert_eq!(names, vec!["beta", "gamma", "alpha"]);
    }

    #[test]
    fn filters_by_user() {
        let filter = SessionFilter {
            user_id: Some("u1".into()),
            ..Default::default()
        };
        let out = filter_and_sort(&corpus(), &filter);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| s.user_id == "u1"));
    }

    #[test]
    fn filters_by_agent() {
        let filter = SessionFilter {
            agent_type: Some("codex".into()),
            ..Default::default()
        };
        let out = filter_and_sort(&corpus(), &filter);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|s| s.agent_type == "codex"));
    }

    #[test]
    fn filters_by_name_substring_case_insensitive() {
        let filter = SessionFilter {
            query: Some("AMM".into()),
            ..Default::default()
        };
        let out = filter_and_sort(&corpus(), &filter);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_name, "gamma");
    }

    #[test]
    fn filters_by_date_range_inclusive_of_day() {
        let filter = SessionFilter {
            from: Some("2026-07-02".into()),
            to: Some("2026-07-03".into()),
            ..Default::default()
        };
        let out = filter_and_sort(&corpus(), &filter);
        let names: Vec<_> = out.iter().map(|s| s.session_name.as_str()).collect();
        // beta (07-03) is included thanks to the inclusive upper bound.
        assert_eq!(names, vec!["beta", "gamma"]);
    }

    #[test]
    fn blank_filters_are_no_constraint() {
        let filter = SessionFilter {
            user_id: Some("   ".into()),
            query: Some("".into()),
            ..Default::default()
        };
        assert_eq!(filter_and_sort(&corpus(), &filter).len(), 3);
    }
}
