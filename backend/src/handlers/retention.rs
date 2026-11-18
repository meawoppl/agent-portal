//! Message retention and cleanup logic

use crate::schema::messages;
use chrono::Utc;
use diesel::prelude::*;
use std::collections::HashSet;
use tracing::{error, info};
use uuid::Uuid;

/// Maximum number of messages to delete in a single cleanup cycle.
/// If more remain, they will be caught in the next 60-second cycle.
const MAX_DELETES_PER_CYCLE: usize = 1000;

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
    if config.retention_days == 0 || budget == 0 {
        return Ok(0);
    }

    let cutoff = Utc::now().naive_utc() - chrono::Duration::days(config.retention_days as i64);

    // Select IDs to delete, capped by remaining budget. Boxed so we can add
    // the held-session exclusion only when there is one (keeps the empty-set
    // path identical to the pre-#1258 query plan).
    let mut query = messages::table
        .filter(messages::created_at.lt(cutoff))
        .into_boxed();
    if !held_ids.is_empty() {
        let held: Vec<Uuid> = held_ids.iter().copied().collect();
        query = query.filter(messages::session_id.ne_all(held));
    }
    let ids_to_delete: Vec<Uuid> = query
        .order(messages::created_at.asc())
        .limit(budget as i64)
        .select(messages::id)
        .load(conn)?;

    if ids_to_delete.is_empty() {
        return Ok(0);
    }

    let deleted = diesel::delete(messages::table.filter(messages::id.eq_any(&ids_to_delete)))
        .execute(conn)?;

    if deleted > 0 {
        info!(
            "Retention cleanup: deleted {} messages older than {} days",
            deleted, config.retention_days
        );
    }

    Ok(deleted)
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
) -> (usize, usize, Vec<Uuid>) {
    let mut budget = MAX_DELETES_PER_CYCLE;
    let mut retry = Vec::new();
    let mut count_deleted = 0;
    let age_deleted = with_cleanup_timeout(conn, |conn| {
        delete_old_messages(conn, config, budget, held_ids)
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
