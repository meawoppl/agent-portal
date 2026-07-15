//! Message retention and cleanup logic

use crate::schema::messages;
use chrono::Utc;
use diesel::prelude::*;
use std::collections::HashSet;
use tracing::{error, info, warn};
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

    let total_count: i64 = messages::table
        .filter(messages::session_id.eq(session_id))
        .count()
        .get_result(conn)?;

    if total_count <= config.max_messages_per_session {
        return Ok(0);
    }

    let to_delete = (total_count - config.max_messages_per_session) as usize;
    let capped = to_delete.min(budget);

    // Get the IDs of the oldest messages to delete
    let ids_to_delete: Vec<Uuid> = messages::table
        .filter(messages::session_id.eq(session_id))
        .order(messages::created_at.asc())
        .limit(capped as i64)
        .select(messages::id)
        .load(conn)?;

    if ids_to_delete.is_empty() {
        return Ok(0);
    }

    let deleted = diesel::delete(messages::table.filter(messages::id.eq_any(&ids_to_delete)))
        .execute(conn)?;

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

/// Run the full retention cleanup process:
/// 1. Delete messages older than retention_days
/// 2. Truncate per-session message counts
///
/// Applies a 5-second statement timeout and caps total deletes at 1000 per cycle.
///
/// `held_ids` names sessions whose pre-trim archive failed this cycle (#1258
/// phase 2). Both delete paths exclude them so no unarchived message is lost
/// during an archive outage — the trim is held and retried next cycle. The
/// set is always empty when archiving is disabled, in which case the trim
/// runs exactly as before.
pub fn run_retention_cleanup(
    conn: &mut diesel::pg::PgConnection,
    pending_session_ids: Vec<Uuid>,
    config: RetentionConfig,
    held_ids: &HashSet<Uuid>,
) -> (usize, usize) {
    // Set a 5-second timeout for cleanup queries
    if let Err(e) = diesel::sql_query("SET LOCAL statement_timeout = '5000'").execute(conn) {
        warn!(
            "Failed to set statement_timeout for retention cleanup: {}",
            e
        );
    }

    let mut budget = MAX_DELETES_PER_CYCLE;
    let mut age_deleted = 0;
    let mut count_deleted = 0;

    // First, bulk delete old messages (up to budget), skipping held sessions
    match delete_old_messages(conn, config, budget, held_ids) {
        Ok(deleted) => {
            age_deleted = deleted;
            budget = budget.saturating_sub(deleted);
        }
        Err(e) => error!("Failed to delete old messages: {:?}", e),
    }

    // Then truncate per-session counts (remaining budget)
    for session_id in pending_session_ids {
        if budget == 0 {
            break;
        }
        // Hold the trim for any session whose pre-trim archive failed.
        if held_ids.contains(&session_id) {
            continue;
        }
        match truncate_session_messages(conn, session_id, config, budget) {
            Ok(deleted) => {
                count_deleted += deleted;
                budget = budget.saturating_sub(deleted);
            }
            Err(e) => error!("Failed to truncate session {}: {:?}", session_id, e),
        }
    }

    (age_deleted, count_deleted)
}
