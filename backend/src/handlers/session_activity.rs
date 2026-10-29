//! Turn state is a property of agent events, not the last transcript row.
//! Portal notices, usage reports and unknown frames preserve the previous state.
use std::collections::HashMap;

use diesel::prelude::*;
use diesel::sql_types::{Array, BigInt, Nullable, Text, Timestamp, Uuid as SqlUuid};
use shared::api::SessionActivityState;
use uuid::Uuid;

use crate::{db::DbConnection, errors::AppError, AppState};

pub(crate) fn session_activity_states(
    app_state: &AppState,
    conn: &mut DbConnection,
    session_ids: &[Uuid],
) -> Result<HashMap<Uuid, SessionActivityState>, AppError> {
    let connected: Vec<_> = session_ids
        .iter()
        .copied()
        .filter(|id| {
            app_state
                .session_manager
                .is_proxy_connected(&id.to_string())
        })
        .collect();
    let mut states = persisted_activity_states(conn, &connected)?;
    for id in session_ids {
        states.entry(*id).or_insert(SessionActivityState::Idle);
    }
    Ok(states)
}

#[derive(QueryableByName)]
struct SignalRow {
    #[diesel(sql_type = SqlUuid)]
    id: Uuid,
    #[diesel(sql_type = Timestamp)]
    created_at: chrono::NaiveDateTime,
    #[diesel(sql_type = SqlUuid)]
    session_id: Uuid,
    #[diesel(sql_type = Text)]
    agent_type: String,
    #[diesel(sql_type = Text)]
    role: String,
    #[diesel(sql_type = Text)]
    content: String,
}

/// Read newest-to-oldest until each session has a meaningful event. LATERAL
/// bounds each session's page independently, avoiding both N+1 queries and an
/// arbitrary lookback cutoff that turns a long tail of notices into idle.
/// Each unresolved session uses a timestamp/id cursor, so concurrent retention
/// cannot shift an OFFSET past the signal we are looking for.
/// Content is parsed in Rust: malformed historical text must not break SQL.
pub(crate) fn persisted_activity_states(
    conn: &mut diesel::PgConnection,
    session_ids: &[Uuid],
) -> Result<HashMap<Uuid, SessionActivityState>, AppError> {
    const PAGE: i64 = 64;
    let mut unresolved = session_ids.to_vec();
    unresolved.sort_unstable();
    unresolved.dedup();
    let mut states = HashMap::new();
    let mut cursors = HashMap::<Uuid, (chrono::NaiveDateTime, Uuid)>::new();
    while !unresolved.is_empty() {
        let before_at: Vec<_> = unresolved
            .iter()
            .map(|id| cursors.get(id).map(|c| c.0))
            .collect();
        let before_id: Vec<_> = unresolved
            .iter()
            .map(|id| cursors.get(id).map(|c| c.1))
            .collect();
        let rows = diesel::sql_query(
            "SELECT m.id, m.created_at, m.session_id, m.agent_type, m.role, m.content
             FROM unnest($1::uuid[], $2::timestamp[], $3::uuid[]) AS wanted(id, before_at, before_id)
             CROSS JOIN LATERAL (
                 SELECT session_id, agent_type, role, content, created_at, id
                 FROM messages WHERE session_id = wanted.id
                 AND role IN ('user', 'assistant', 'result', 'unknown', 'error')
                 AND (wanted.before_at IS NULL OR (created_at, id) < (wanted.before_at, wanted.before_id))
                 ORDER BY created_at DESC, id DESC LIMIT $4
             ) m ORDER BY m.session_id, m.created_at DESC, m.id DESC",
        )
        .bind::<Array<SqlUuid>, _>(&unresolved)
        .bind::<Array<Nullable<Timestamp>>, _>(&before_at)
        .bind::<Array<Nullable<SqlUuid>>, _>(&before_id)
        .bind::<BigInt, _>(PAGE)
        .load::<SignalRow>(conn)?;
        let mut counts = HashMap::<Uuid, i64>::new();
        for row in rows {
            cursors.insert(row.session_id, (row.created_at, row.id));
            *counts.entry(row.session_id).or_default() += 1;
            if let std::collections::hash_map::Entry::Vacant(entry) = states.entry(row.session_id) {
                if let Some(state) = activity_signal(&row.agent_type, &row.role, &row.content) {
                    entry.insert(state);
                }
            }
        }
        unresolved.retain(|id| {
            if states.contains_key(id) {
                false
            } else if counts.get(id).copied().unwrap_or(0) < PAGE {
                states.insert(*id, SessionActivityState::Idle);
                false
            } else {
                true
            }
        });
    }
    Ok(states)
}

/// None means no turn-state information, rather than "busy" or "idle".
/// Inspect only protocol discriminators; tool output quoting an error is not
/// itself a rate limit. User input starts work even when stored as plain text.
pub(crate) fn activity_signal(
    agent: &str,
    role: &str,
    content: &str,
) -> Option<SessionActivityState> {
    use SessionActivityState::{Busy, Idle, Throttled};
    if matches!(role, "portal" | "system") {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(content).ok();
    let kind = value
        .as_ref()
        .and_then(|v| v.get("type"))
        .and_then(|v| v.as_str());
    if kind == Some("portal") {
        return None;
    }
    if role == "user" {
        return Some(Busy);
    }
    let value = value.as_ref()?;
    let kind = kind?;
    if kind == "rate_limit_event" {
        return match value
            .pointer("/rate_limit_info/status")
            .and_then(|v| v.as_str())
        {
            Some("allowed" | "allowed_warning") => None,
            Some("rejected") => Some(Throttled),
            _ => None,
        };
    }
    if kind == "rate_limit_error"
        || (kind == "error"
            && value.pointer("/error/type").and_then(|v| v.as_str()) == Some("rate_limit_error"))
    {
        return Some(Throttled);
    }
    match (agent, kind) {
        ("claude", "result" | "error") => Some(Idle),
        ("claude", "assistant" | "stream_event") => Some(Busy),
        ("codex", "turn.completed" | "turn.failed" | "error") => Some(Idle),
        ("codex", "turn.started" | "item.started" | "item.updated" | "item.completed") => {
            Some(Busy)
        }
        ("muse", "muse_record") => {
            let payload = value.get("payload_type")?.as_str()?;
            if payload.starts_with("run.terminal.") {
                Some(Idle)
            } else if payload.starts_with("run.output.")
                || payload.starts_with("tool.")
                || payload == "run.started"
            {
                Some(Busy)
            } else {
                None
            }
        }
        ("antigravity", "antigravity_turn_completed") => Some(Idle),
        ("antigravity", "antigravity_event") => Some(Busy),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::api::SessionActivityState::{Busy, Idle, Throttled};

    #[test]
    fn agent_turn_signals() {
        for (agent, frame, expected) in [
            ("claude", r#"{"type":"assistant"}"#, Busy),
            ("claude", r#"{"type":"result"}"#, Idle),
            ("codex", r#"{"type":"item.started"}"#, Busy),
            ("codex", r#"{"type":"item.completed"}"#, Busy),
            ("codex", r#"{"type":"turn.completed"}"#, Idle),
            ("codex", r#"{"type":"turn.failed"}"#, Idle),
            (
                "muse",
                r#"{"type":"muse_record","payload_type":"tool.result"}"#,
                Busy,
            ),
            (
                "muse",
                r#"{"type":"muse_record","payload_type":"run.terminal.completed"}"#,
                Idle,
            ),
            (
                "antigravity",
                r#"{"type":"antigravity_turn_completed"}"#,
                Idle,
            ),
            (
                "claude",
                r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected"}}"#,
                Throttled,
            ),
            (
                "claude",
                r#"{"type":"error","error":{"type":"rate_limit_error"}}"#,
                Throttled,
            ),
        ] {
            assert_eq!(
                activity_signal(agent, "assistant", frame),
                Some(expected),
                "{frame}"
            );
        }
        assert_eq!(
            activity_signal("codex", "user", "Please continue"),
            Some(Busy)
        );
    }

    #[test]
    fn bookkeeping_is_not_a_turn_signal() {
        for frame in [
            r#"{"type":"portal","payload":{"type":"rate_limit_event"}}"#,
            r#"{"type":"thread.started"}"#,
            r#"{"type":"thread.tokenUsage.updated"}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}"#,
            r#"{"type":"rate_limit_event","rate_limit_info":{"status":"future_status"}}"#,
            r#"{"type":"rate_limit_event"}"#,
            "not-json",
        ] {
            for agent in ["claude", "codex", "muse", "antigravity"] {
                assert_eq!(
                    activity_signal(agent, "unknown", frame),
                    None,
                    "{agent}: {frame}"
                );
            }
        }
        assert_eq!(
            activity_signal("claude", "portal", r#"{"type":"assistant"}"#),
            None
        );
        // An error quoted in tool/message content is not a protocol error.
        assert_eq!(
            activity_signal(
                "claude",
                "assistant",
                r#"{"type":"assistant","message":{"type":"rate_limit_error"}}"#
            ),
            Some(Busy)
        );
    }

    #[test]
    fn latest_meaningful_event_survives_long_notice_tails() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().unwrap();
        conn.test_transaction::<_, diesel::result::Error, _>(|conn| {
            use crate::schema::messages;
            let user = crate::test_support::insert_user(conn, "activity");
            let done = crate::test_support::insert_session(conn, user.id, "done").id;
            let busy = crate::test_support::insert_session(conn, user.id, "busy").id;
            let empty = crate::test_support::insert_session(conn, user.id, "empty").id;
            let base = chrono::Utc::now().naive_utc();
            let mut insert = |session, step, role: &str, content: &str| {
                diesel::insert_into(messages::table)
                    .values((
                        messages::session_id.eq(session),
                        messages::user_id.eq(user.id),
                        messages::role.eq(role),
                        messages::agent_type.eq("codex"),
                        messages::content.eq(content),
                        messages::created_at.eq(base + chrono::Duration::seconds(step)),
                    ))
                    .execute(conn)
                    .unwrap();
            };
            insert(done, 0, "result", r#"{"type":"turn.completed"}"#);
            insert(busy, 0, "assistant", r#"{"type":"item.started"}"#);
            for step in 1..=140 {
                for session in [done, busy] {
                    insert(
                        session,
                        step,
                        "unknown",
                        r#"{"type":"portal","message":"notice"}"#,
                    );
                }
            }
            insert(done, 141, "portal", "notice");
            insert(busy, 141, "portal", "notice");
            let states = persisted_activity_states(conn, &[done, busy, empty, done]).unwrap();
            assert_eq!(states[&done], Idle);
            assert_eq!(states[&busy], Busy);
            assert_eq!(states[&empty], Idle);
            Ok(())
        });
    }
}
