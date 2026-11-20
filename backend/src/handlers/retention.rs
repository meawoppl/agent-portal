//! Message retention and cleanup logic

use crate::schema::messages;
use chrono::Utc;
use diesel::prelude::*;
use std::collections::HashSet;
use tracing::{error, info};
use uuid::Uuid;

/// Maximum number of messages to delete in a single cleanup cycle.
/// If more remain, they will be caught in the next 60-second cycle.
pub(crate) const MAX_DELETES_PER_CYCLE: usize = 1000;

/// Configuration for message retention policy
#[derive(Clone, Copy, Debug)]
pub struct RetentionConfig {
    /// Maximum messages to keep per session
    pub max_messages_per_session: i64,
    /// Days to retain messages (0 = disabled)
    pub retention_days: u32,
}

impl RetentionConfig {
    pub fn new(max_messages_per_session: i64, retention_days: u32) -> Self {
        Self {
            max_messages_per_session,
            retention_days,
        }
    }
}

/// Whether `session_id` holds more than `max` messages.
///
/// Bounded and index-only: it counts at most `max + 1` entries of the
/// `session_id` index and never visits the table. A bare
/// `SELECT id ... OFFSET max LIMIT 1` with no `ORDER BY` planned as a bitmap
/// heap scan that materialized the whole session, every cycle, for every
/// session with new messages.
pub(crate) fn session_over_limit(
    conn: &mut diesel::pg::PgConnection,
    session_id: Uuid,
    max: i64,
) -> QueryResult<bool> {
    use diesel::sql_types::{BigInt, Bool, Uuid as SqlUuid};
    #[derive(diesel::QueryableByName)]
    struct Probe {
        #[diesel(sql_type = Bool)]
        over: bool,
    }
    let max = max.max(0);
    diesel::sql_query(
        "SELECT count(*) > $2 AS over
         FROM (SELECT 1 FROM messages WHERE session_id = $1 LIMIT $3) AS bounded",
    )
    .bind::<SqlUuid, _>(session_id)
    .bind::<BigInt, _>(max)
    .bind::<BigInt, _>(max.saturating_add(1))
    .get_result::<Probe>(conn)
    .map(|probe| probe.over)
}

/// Truncate messages for a single session to the configured maximum.
/// Deletes at most `budget` messages. Returns the number of deleted messages.
pub fn truncate_session_messages(
    conn: &mut diesel::pg::PgConnection,
    session_id: Uuid,
    config: RetentionConfig,
    budget: usize,
) -> Result<usize, diesel::result::Error> {
    if budget == 0 {
        return Ok(0);
    }
    // The seek below walks the retained tail through the ordered index and
    // fetches each row; skip it for the common case of a session under its cap.
    if !session_over_limit(conn, session_id, config.max_messages_per_session)? {
        return Ok(0);
    }

    // Seek to the first row outside the retained tail, then remove a
    // bounded number of older rows in the same statement and snapshot.
    use diesel::sql_types::{BigInt, Uuid as SqlUuid};
    let deleted = diesel::sql_query(
        "DELETE FROM messages WHERE id IN (
            SELECT id FROM messages
            WHERE session_id = $1 AND (created_at, id) <= (
                SELECT created_at, id FROM messages
                WHERE session_id = $1
                ORDER BY created_at DESC, id DESC OFFSET $2 LIMIT 1
            )
            ORDER BY created_at ASC, id ASC LIMIT $3
        )",
    )
    .bind::<SqlUuid, _>(session_id)
    .bind::<BigInt, _>(config.max_messages_per_session.max(0))
    .bind::<BigInt, _>(budget as i64)
    .execute(conn)?;

    if deleted == 0 {
        return Ok(0);
    }
    info!(
        "Truncated session {}: deleted {} old messages, keeping last {}",
        session_id, deleted, config.max_messages_per_session
    );

    Ok(deleted)
}

/// Delete messages older than the configured retention period, up to the per-cycle cap.
/// Returns the number of deleted messages.
///
/// `held_ids` names sessions whose pre-trim archive failed this cycle (#1258
/// phase 2): their messages are excluded from the bulk delete so the
/// unarchived delta survives to be retried next cycle. Archive-first is the
/// invariant — retention never destroys the last copy. When the set is empty
/// (the common case, and always when archiving is disabled) the query is
/// unchanged and stays index-friendly on `created_at`.
pub fn delete_old_messages(
    conn: &mut diesel::pg::PgConnection,
    config: RetentionConfig,
    budget: usize,
    held_ids: &HashSet<Uuid>,
) -> Result<usize, diesel::result::Error> {
    let candidates = old_message_candidates(conn, config, budget, held_ids, None)?;
    let ids: Vec<_> = candidates.into_iter().map(|(id, _, _)| id).collect();
    delete_message_ids(conn, &ids)
}

/// Plan exact age-trim IDs before archive capture so the deletion cannot
/// pick up rows that arrived or became eligible during the archive write.
pub(crate) type AgeCursor = (chrono::NaiveDateTime, Uuid);

pub(crate) fn old_message_candidates(
    conn: &mut diesel::PgConnection,
    config: RetentionConfig,
    budget: usize,
    held_ids: &HashSet<Uuid>,
    after: Option<AgeCursor>,
) -> QueryResult<Vec<(Uuid, Uuid, chrono::NaiveDateTime)>> {
    if config.retention_days == 0 || budget == 0 {
        return Ok(Vec::new());
    }

    let cutoff = Utc::now().naive_utc() - chrono::Duration::days(config.retention_days as i64);
    let mut query = messages::table
        .filter(messages::created_at.lt(cutoff))
        .into_boxed();
    if !held_ids.is_empty() {
        let held: Vec<Uuid> = held_ids.iter().copied().collect();
        query = query.filter(messages::session_id.ne_all(held));
    }
    if let Some((timestamp, id)) = after {
        query = query.filter(
            messages::created_at
                .gt(timestamp)
                .or(messages::created_at.eq(timestamp).and(messages::id.gt(id))),
        );
    }
    query
        .order((messages::created_at.asc(), messages::id.asc()))
        .limit(budget as i64)
        .select((messages::id, messages::session_id, messages::created_at))
        .load(conn)
}

fn delete_message_ids(conn: &mut diesel::PgConnection, ids: &[Uuid]) -> QueryResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    diesel::delete(messages::table.filter(messages::id.eq_any(ids))).execute(conn)
}

/// Apply a timeout scoped to this transaction and its pooled connection.
pub(crate) fn with_cleanup_timeout<T, E>(
    conn: &mut diesel::PgConnection,
    operation: impl FnOnce(&mut diesel::PgConnection) -> Result<T, E>,
) -> Result<T, E>
where
    E: From<diesel::result::Error>,
{
    conn.transaction(|conn| {
        diesel::sql_query("SET LOCAL statement_timeout = '5000'").execute(conn)?;
        operation(conn)
    })
}

/// Delete old messages, then trim the supplied sessions to the count cap.
/// Returns any session that still needs a count pass due to a failed or
/// budget-limited attempt.
pub fn run_retention_cleanup(
    conn: &mut diesel::PgConnection,
    pending_session_ids: Vec<Uuid>,
    config: RetentionConfig,
    held_ids: &HashSet<Uuid>,
    archived_age_ids: Option<&[Uuid]>,
) -> (usize, usize, Vec<Uuid>) {
    let mut budget = MAX_DELETES_PER_CYCLE;
    let mut retry = Vec::new();
    let mut count_deleted = 0;
    let age_deleted = with_cleanup_timeout(conn, |conn| match archived_age_ids {
        Some(ids) => delete_message_ids(conn, ids),
        None => delete_old_messages(conn, config, budget, held_ids),
    })
    .unwrap_or_else(|e| {
        error!("Failed to delete old messages: {e}");
        0
    });
    budget -= age_deleted;

    for session_id in pending_session_ids {
        if budget == 0 || held_ids.contains(&session_id) {
            retry.push(session_id);
            continue;
        }
        match with_cleanup_timeout(conn, |conn| {
            truncate_session_messages(conn, session_id, config, budget)
        }) {
            Ok(deleted) => {
                count_deleted += deleted;
                budget -= deleted;
                if budget == 0 {
                    retry.push(session_id);
                }
            }
            Err(e) => {
                error!("Failed to truncate session {session_id}: {e}");
                retry.push(session_id);
            }
        }
    }

    (age_deleted, count_deleted, retry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{insert_session, insert_user, shared_pool};

    /// The bounded probe answers "more than `max`" exactly at the boundary:
    /// a session holding the cap is not over it; one more message is.
    #[test]
    fn over_limit_probe_is_exact_at_the_cap() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        conn.test_transaction::<_, diesel::result::Error, _>(|conn| {
            let user = insert_user(conn, "retention-probe");
            let session = insert_session(conn, user.id, "retention-probe");
            let add = |conn: &mut diesel::pg::PgConnection, n: i32| {
                diesel::sql_query(
                    "INSERT INTO messages (session_id, user_id, role, content, agent_type)
                     SELECT $1, $2, 'assistant', '{}', 'claude' FROM generate_series(1, $3)",
                )
                .bind::<diesel::sql_types::Uuid, _>(session.id)
                .bind::<diesel::sql_types::Uuid, _>(user.id)
                .bind::<diesel::sql_types::Integer, _>(n)
                .execute(conn)
            };

            assert!(!session_over_limit(conn, session.id, 3)?, "empty");
            add(conn, 3)?;
            assert!(
                !session_over_limit(conn, session.id, 3)?,
                "exactly at the cap"
            );
            add(conn, 1)?;
            assert!(session_over_limit(conn, session.id, 3)?, "one over the cap");
            assert!(
                session_over_limit(conn, session.id, 0)?,
                "a zero cap with rows"
            );
            Ok(())
        });
    }
}
