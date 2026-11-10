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
    let mut history = ActivityHistory::default();
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
            history.observe(row.session_id, &row.agent_type, &row.role, &row.content);
        }
        unresolved.retain(|id| {
            if history.states.contains_key(id) {
                false
            } else if counts.get(id).copied().unwrap_or(0) < PAGE {
                history.states.insert(*id, SessionActivityState::Idle);
                false
            } else {
                true
            }
        });
    }
    Ok(history.states)
}

/// A failed Claude result does not carry the preceding assistant's typed API
/// error. Read back within that turn so its usage-limit classification survives
/// the generic terminator. Never inherit a throttle across a user/turn boundary;
/// cap this special lookback independently of the ordinary notice-tail scan.
#[derive(Default)]
struct ActivityHistory {
    states: HashMap<Uuid, SessionActivityState>,
    failed_claude_turns: HashMap<Uuid, usize>,
}

impl ActivityHistory {
    fn observe(&mut self, id: Uuid, agent: &str, role: &str, content: &str) {
        use SessionActivityState::{Idle, Throttled};
        if self.states.contains_key(&id) {
            return;
        }
        let signal = activity_signal(agent, role, content);
        if let Some(scanned) = self.failed_claude_turns.get_mut(&id) {
            *scanned += 1;
            if let Some(state) = signal {
                self.states
                    .insert(id, if state == Throttled { Throttled } else { Idle });
            } else if *scanned >= 64 {
                self.states.insert(id, Idle);
            }
        } else if agent == "claude"
            && role == "result"
            && serde_json::from_str::<shared::ResultMessage>(content).is_ok_and(|r| r.is_error)
        {
            self.failed_claude_turns.insert(id, 0);
        } else if let Some(state) = signal {
            self.states.insert(id, state);
        }
    }
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
    if agent == "codex" {
        // Completion follows the visible terminal error. The shared typed
        // projection preserves its capacity refusal instead of claiming Idle.
        if let Ok(frame) =
            serde_json::from_str::<shared::codex_errors::CodexLifecycleFrame>(content)
        {
            return Some(frame.activity());
        }
    }
    if agent == "claude" && kind == "assistant" {
        if let Ok(message) = serde_json::from_str::<shared::AssistantMessage>(content) {
            // Only the top-level assistant's own typed error is a signal.
            // Subagent limits and quoted content do not throttle this session.
            if message.parent_tool_use_id.is_none()
                && message.agent_id.is_none()
                && matches!(
                    message.api_error.as_deref(),
                    Some("usage_limit_reached" | "model_requires_usage_credits")
                )
            {
                return Some(Throttled);
            }
        }
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
    fn codex_retry_capacity_and_completion_keep_correct_activity() {
        for (frame, expected) in [
            (
                r#"{"type":"error","message":"retrying network","willRetry":true}"#,
                Busy,
            ),
            (
                r#"{"type":"error","message":"bad request","willRetry":false}"#,
                Idle,
            ),
            (r#"{"type":"error","message":"legacy error"}"#, Idle),
            (
                r#"{"type":"error","message":"retry capacity","codexErrorInfo":"serverOverloaded","willRetry":true}"#,
                Throttled,
            ),
            (
                r#"{"type":"turn.completed","status":"failed","error":{"message":"limit","codexErrorInfo":"usageLimitExceeded"}}"#,
                Throttled,
            ),
            (
                r#"{"type":"turn.completed","status":"failed","error":{"message":"context","codexErrorInfo":"contextWindowExceeded"}}"#,
                Idle,
            ),
            (r#"{"type":"turn.completed","status":"completed"}"#, Idle),
            (r#"{"type":"item.started"}"#, Busy),
        ] {
            assert_eq!(
                activity_signal("codex", "assistant", frame),
                Some(expected),
                "{frame}"
            );
        }
        // The completion immediately after an error must not erase throttling.
        for frame in [
            r#"{"type":"error","message":"capacity","codexErrorInfo":"rateLimitExceeded"}"#,
            r#"{"type":"turn.completed","error":{"message":"capacity","codexErrorInfo":"rateLimitExceeded"}}"#,
        ] {
            assert_eq!(activity_signal("codex", "error", frame), Some(Throttled));
        }
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
    fn typed_limit_frame() -> String {
        r#"{"type":"assistant","session_id":"session","api_error":"usage_limit_reached","message":{"id":"msg","type":"message","role":"assistant","model":"<synthetic>","content":[]}}"#.to_string()
    }

    fn failed_result() -> &'static str {
        r#"{"type":"result","subtype":"error_during_execution","is_error":true,"duration_ms":0,"duration_api_ms":0,"num_turns":1,"total_cost_usd":0,"session_id":"session"}"#
    }

    #[test]
    fn typed_usage_limit_survives_error_result_but_not_a_new_turn() {
        let id = Uuid::nil();
        assert_eq!(
            activity_signal("claude", "assistant", &typed_limit_frame()),
            Some(Throttled)
        );
        let mut scan = ActivityHistory::default();
        scan.observe(id, "claude", "result", failed_result());
        scan.observe(id, "claude", "assistant", &typed_limit_frame());
        assert_eq!(scan.states[&id], Throttled);

        for boundary in [
            ("user", "continue"),
            ("result", r#"{"type":"result"}"#),
            ("assistant", r#"{"type":"assistant"}"#),
        ] {
            let mut scan = ActivityHistory::default();
            scan.observe(id, "claude", "result", failed_result());
            scan.observe(id, "claude", boundary.0, boundary.1);
            scan.observe(id, "claude", "assistant", &typed_limit_frame());
            assert_eq!(scan.states[&id], Idle, "boundary: {boundary:?}");
        }
        let mut scan = ActivityHistory::default();
        scan.observe(id, "claude", "user", "continue");
        scan.observe(id, "claude", "result", failed_result());
        scan.observe(id, "claude", "assistant", &typed_limit_frame());
        assert_eq!(scan.states[&id], Busy);
    }

    #[test]
    fn failed_result_lookback_is_bounded_and_subagent_limit_is_not_session_limit() {
        let id = Uuid::nil();
        let mut scan = ActivityHistory::default();
        scan.observe(id, "claude", "result", failed_result());
        for _ in 0..64 {
            scan.observe(id, "claude", "unknown", "malformed historical notice");
        }
        scan.observe(id, "claude", "assistant", &typed_limit_frame());
        assert_eq!(scan.states[&id], Idle);
        let mut subagent: serde_json::Value = serde_json::from_str(&typed_limit_frame()).unwrap();
        subagent["parent_tool_use_id"] = "toolu_123".into();
        assert_eq!(
            activity_signal("claude", "assistant", &subagent.to_string()),
            Some(Busy)
        );
    }
}
