//! Durable accounting for Claude results, separate from transcript retention.

use claude_codes::io::ResultMessage;
use diesel::prelude::*;
use diesel::sql_types::{Double, Text, Uuid as SqlUuid};
use shared::claude_cost::ClaudeResultCost;
use uuid::Uuid;

/// A result identity and its cost must commit together. Transport acknowledgement state
/// is in memory, so it cannot guard additions across a backend restart. The
/// ledger survives transcript retention and is deleted with the session.
///
/// Legacy results (or modern results without a stable UUID) cannot safely be
/// added: a cumulative snapshot has no CLI process identity. Preserve a
/// nondecreasing compatibility estimate instead of overwriting resumed totals.
pub(super) fn record_result_cost(
    conn: &mut PgConnection,
    session_id: Uuid,
    result: &ResultMessage,
) -> QueryResult<()> {
    let result_uuid = result.uuid.as_deref().filter(|id| !id.trim().is_empty());
    let cost = ClaudeResultCost::from_result(result);
    if cost == ClaudeResultCost::Unavailable {
        return Ok(());
    }
    conn.transaction(|conn| {
        if let Some(id) = result_uuid {
            let inserted = diesel::sql_query(
                "INSERT INTO claude_result_cost_ledger (session_id, result_uuid) \
                 VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind::<SqlUuid, _>(session_id)
            .bind::<Text, _>(id)
            .execute(conn)?;
            if inserted == 0 {
                return Ok(());
            }
        }
        let (amount, additive) = match cost {
            ClaudeResultCost::Delta(delta) if result_uuid.is_some() => (delta, true),
            ClaudeResultCost::Delta(_) | ClaudeResultCost::Cumulative(_) => {
                // Without identity, keep the cumulative compatibility path.
                let total = result.total_cost_usd;
                if !total.is_finite() || total < 0.0 {
                    return Ok(());
                }
                (total, false)
            }
            ClaudeResultCost::Unavailable => return Ok(()),
        };
        let query = if additive {
            "UPDATE sessions SET total_cost_usd = total_cost_usd + $2 WHERE id = $1"
        } else {
            "UPDATE sessions SET total_cost_usd = GREATEST(total_cost_usd, $2) WHERE id = $1"
        };
        diesel::sql_query(query)
            .bind::<SqlUuid, _>(session_id)
            .bind::<Double, _>(amount)
            .execute(conn)?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{claude_result_cost_ledger, sessions, users};
    use crate::test_support::{insert_session, insert_user, shared_pool};
    use claude_codes::io::ModelUsageEntry;
    use diesel::connection::SimpleConnection;
    use std::collections::BTreeMap;

    fn result(id: &str, cumulative: f64, delta: Option<f64>) -> ResultMessage {
        let mut result: ResultMessage = serde_json::from_str(
            r#"{"type":"result","subtype":"success","is_error":false,"duration_ms":1,"duration_api_ms":1,"num_turns":1,"session_id":"s","total_cost_usd":0}"#,
        ).unwrap();
        result.uuid = Some(id.to_string());
        result.total_cost_usd = cumulative;
        result.model_usage_since_last_result = delta.map(|cost| {
            BTreeMap::from([(
                "model".to_string(),
                ModelUsageEntry {
                    cost_usd: cost,
                    ..Default::default()
                },
            )])
        });
        result
    }

    fn total(conn: &mut PgConnection, id: Uuid) -> f64 {
        sessions::table
            .find(id)
            .select(sessions::total_cost_usd)
            .first(conn)
            .unwrap()
    }

    #[test]
    fn claude_cost_deltas_resume_and_replay_survive_a_new_backend_connection() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        let user = insert_user(&mut conn, "claude_cost_resume");
        let session = insert_session(&mut conn, user.id, "cost");
        let first = result("first", 10.0, Some(2.0));
        let next = result("next", 20.0, Some(3.0));
        record_result_cost(&mut conn, session.id, &first).unwrap();
        record_result_cost(&mut conn, session.id, &next).unwrap();
        assert_eq!(total(&mut conn, session.id), 5.0);
        drop(conn);

        // A restarted backend has no transport acknowledgement map. Only
        // database identity prevents charging a replayed result a second time.
        let mut conn = pool.get().unwrap();
        record_result_cost(&mut conn, session.id, &first).unwrap();
        record_result_cost(&mut conn, session.id, &next).unwrap();
        record_result_cost(&mut conn, session.id, &result("resumed", 1.0, Some(1.0))).unwrap();
        assert_eq!(total(&mut conn, session.id), 6.0);
        diesel::delete(users::table.find(user.id))
            .execute(&mut conn)
            .unwrap();
        assert_eq!(
            claude_result_cost_ledger::table
                .filter(claude_result_cost_ledger::session_id.eq(session.id))
                .count()
                .get_result::<i64>(&mut conn)
                .unwrap(),
            0
        );
    }

    #[test]
    fn claude_cost_legacy_to_modern_preserves_totals_and_empty_deltas() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        let user = insert_user(&mut conn, "claude_cost_legacy");
        let session = insert_session(&mut conn, user.id, "cost");
        let old = result("legacy", 10.0, None);
        record_result_cost(&mut conn, session.id, &old).unwrap();
        record_result_cost(&mut conn, session.id, &result("legacy-next", 15.0, None)).unwrap();
        record_result_cost(&mut conn, session.id, &result("legacy-reset", 1.0, None)).unwrap();
        assert_eq!(total(&mut conn, session.id), 15.0);
        record_result_cost(&mut conn, session.id, &result("modern", 2.0, Some(2.0))).unwrap();
        record_result_cost(&mut conn, session.id, &old).unwrap();
        let mut empty = result("empty", 100.0, Some(0.0));
        empty.model_usage_since_last_result = Some(BTreeMap::new());
        record_result_cost(&mut conn, session.id, &empty).unwrap();
        assert_eq!(total(&mut conn, session.id), 17.0);
        // Without a result identity adding is unsafe; the compatibility
        // snapshot cannot lower the known session total or duplicate-add.
        let mut anonymous = result("unused", 3.0, Some(1.0));
        anonymous.uuid = None;
        record_result_cost(&mut conn, session.id, &anonymous).unwrap();
        record_result_cost(&mut conn, session.id, &anonymous).unwrap();
        assert_eq!(total(&mut conn, session.id), 17.0);
        diesel::delete(users::table.find(user.id))
            .execute(&mut conn)
            .unwrap();
    }

    #[test]
    fn claude_cost_concurrent_results_are_atomic_and_duplicate_safe() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        let user = insert_user(&mut conn, "claude_cost_parallel");
        let session = insert_session(&mut conn, user.id, "cost");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let pool = pool.clone();
                std::thread::spawn(move || {
                    let mut conn = pool.get().unwrap();
                    record_result_cost(
                        &mut conn,
                        session.id,
                        &result("duplicate", 999.0, Some(2.0)),
                    )
                    .unwrap();
                    record_result_cost(
                        &mut conn,
                        session.id,
                        &result(&format!("unique-{i}"), 999.0, Some(1.0)),
                    )
                    .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(total(&mut conn, session.id), 10.0);
        diesel::delete(users::table.find(user.id))
            .execute(&mut conn)
            .unwrap();
    }

    #[test]
    fn claude_cost_migration_seeds_historical_identities_without_recrediting() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        conn.test_transaction::<_, diesel::result::Error, _>(|conn| {
            // A transaction-local schema exercises the actual migration,
            // including malformed old content, without touching shared tables.
            let name = format!("cost_test_{}", Uuid::new_v4().simple());
            conn.batch_execute(&format!("CREATE SCHEMA {name}; SET LOCAL search_path TO {name}; CREATE TABLE sessions (id uuid PRIMARY KEY, total_cost_usd double precision NOT NULL); CREATE TABLE messages (session_id uuid, role text, agent_type text, content text);"))?;
            let id = Uuid::new_v4();
            diesel::sql_query("INSERT INTO sessions VALUES ($1, 12.0)").bind::<SqlUuid,_>(id).execute(conn)?;
            let old = result("old-result", 100.0, Some(4.0));
            for content in [serde_json::to_string(&old).unwrap(), "not-json".to_string(), r#"{"uuid":""}"#.to_string()] {
                diesel::sql_query("INSERT INTO messages VALUES ($1, 'result', 'claude', $2)").bind::<SqlUuid,_>(id).bind::<Text,_>(content).execute(conn)?;
            }
            conn.batch_execute(include_str!("../../../migrations/2026-11-09-233000_claude_result_cost_ledger/up.sql"))?;
            record_result_cost(conn, id, &old)?;
            assert_eq!(total(conn, id), 12.0);
            record_result_cost(conn, id, &result("new-result", 1.0, Some(1.0)))?;
            assert_eq!(total(conn, id), 13.0);
            conn.batch_execute(include_str!("../../../migrations/2026-11-09-233000_claude_result_cost_ledger/down.sql"))?;
            Ok(())
        });
    }
}
