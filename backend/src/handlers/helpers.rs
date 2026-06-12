use crate::models::{Message, NewDeletedSessionCosts, Session};
use crate::schema::{
    deleted_session_costs, messages, pending_inputs, pending_permission_requests, session_members,
    sessions, users,
};
use chrono::NaiveDateTime;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, PooledConnection};
use diesel::PgConnection;
use std::collections::{HashMap, HashSet};
use tracing::error;
use uuid::Uuid;

/// Parse an ISO timestamp cursor, accepting both the fractional
/// (`%Y-%m-%dT%H:%M:%S%.f`) and second-precision (`%Y-%m-%dT%H:%M:%S`)
/// forms emitted by the frontend's `js_sys::Date.toISOString()` and by the
/// backend's own `NaiveDateTime` serializer respectively. Shared by the REST
/// `list_messages` pagination cursors and the WebSocket replay watermark.
pub fn parse_iso_cursor(s: &str) -> Option<NaiveDateTime> {
    // Trim trailing `Z` if present — `js_sys::Date::to_iso_string` emits
    // `2026-05-17T12:34:56.789Z`, but `NaiveDateTime::parse_from_str` rejects
    // the timezone marker on a naive timestamp.
    let trimmed = s.strip_suffix('Z').unwrap_or(s);
    NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S"))
        .ok()
}

/// Look up display names for the senders of the user-role messages in
/// `message_list`: collect the distinct `user_id`s, load `(id, name, email)`
/// for each, and map to name-or-email. Shared by the REST `list_messages`
/// handler and the WebSocket history replay so both enrich user messages
/// identically. Lookup failures degrade to an empty map (no sender names)
/// rather than erroring.
pub fn sender_names(conn: &mut PgConnection, message_list: &[Message]) -> HashMap<Uuid, String> {
    let user_ids: Vec<Uuid> = message_list
        .iter()
        .filter(|m| m.role == "user")
        .map(|m| m.user_id)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    if user_ids.is_empty() {
        return HashMap::new();
    }
    users::table
        .filter(users::id.eq_any(&user_ids))
        .select((users::id, users::name, users::email))
        .load::<(Uuid, Option<String>, String)>(conn)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, name, email)| (id, name.unwrap_or(email)))
        .collect()
}

/// Error type for helper operations
pub struct DeleteSessionError(String);

impl std::fmt::Debug for DeleteSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeleteSessionError({})", self.0)
    }
}

impl From<diesel::result::Error> for DeleteSessionError {
    fn from(err: diesel::result::Error) -> Self {
        DeleteSessionError(err.to_string())
    }
}

/// Delete a session and all associated data (messages, session_members).
/// Optionally records the session costs to deleted_session_costs for the owner.
///
/// Returns the number of deleted messages.
pub fn delete_session_with_data(
    conn: &mut PooledConnection<ConnectionManager<PgConnection>>,
    session: &Session,
    record_costs: bool,
) -> Result<usize, DeleteSessionError> {
    let session_id = session.id;

    // Record the cost and tokens from deleted session if requested
    if record_costs {
        let has_usage =
            session.total_cost_usd > 0.0 || session.input_tokens > 0 || session.output_tokens > 0;

        if has_usage {
            diesel::insert_into(deleted_session_costs::table)
                .values(NewDeletedSessionCosts {
                    user_id: session.user_id,
                    cost_usd: session.total_cost_usd,
                    session_count: 1,
                    input_tokens: session.input_tokens,
                    output_tokens: session.output_tokens,
                    cache_creation_tokens: session.cache_creation_tokens,
                    cache_read_tokens: session.cache_read_tokens,
                })
                .on_conflict(deleted_session_costs::user_id)
                .do_update()
                .set((
                    deleted_session_costs::cost_usd
                        .eq(deleted_session_costs::cost_usd + session.total_cost_usd),
                    deleted_session_costs::session_count
                        .eq(deleted_session_costs::session_count + 1),
                    deleted_session_costs::input_tokens
                        .eq(deleted_session_costs::input_tokens + session.input_tokens),
                    deleted_session_costs::output_tokens
                        .eq(deleted_session_costs::output_tokens + session.output_tokens),
                    deleted_session_costs::cache_creation_tokens
                        .eq(deleted_session_costs::cache_creation_tokens
                            + session.cache_creation_tokens),
                    deleted_session_costs::cache_read_tokens
                        .eq(deleted_session_costs::cache_read_tokens + session.cache_read_tokens),
                    deleted_session_costs::updated_at.eq(diesel::dsl::now),
                ))
                .execute(conn)
                .map_err(|e| {
                    error!("Failed to record deleted session cost: {}", e);
                    DeleteSessionError(format!("Failed to record costs: {}", e))
                })?;
        }
    }

    // Delete messages
    let deleted_messages =
        diesel::delete(messages::table.filter(messages::session_id.eq(session_id)))
            .execute(conn)
            .map_err(|e| {
                error!("Failed to delete session messages: {}", e);
                DeleteSessionError(format!("Failed to delete messages: {}", e))
            })?;

    // Delete session_members
    diesel::delete(session_members::table.filter(session_members::session_id.eq(session_id)))
        .execute(conn)
        .map_err(|e| {
            error!("Failed to delete session members: {}", e);
            DeleteSessionError(format!("Failed to delete session members: {}", e))
        })?;

    // Delete pending_inputs
    let _ = diesel::delete(pending_inputs::table.filter(pending_inputs::session_id.eq(session_id)))
        .execute(conn);

    // Delete pending_permission_requests
    let _ = diesel::delete(
        pending_permission_requests::table
            .filter(pending_permission_requests::session_id.eq(session_id)),
    )
    .execute(conn);

    // Revoke any (non-expiring) launch tokens bound to this session so they
    // don't outlive the session row. See #932.
    crate::handlers::proxy_tokens::revoke_tokens_for_session(conn, session_id);

    // Delete the session
    diesel::delete(sessions::table.filter(sessions::id.eq(session_id)))
        .execute(conn)
        .map_err(|e| {
            error!("Failed to delete session: {}", e);
            DeleteSessionError(format!("Failed to delete session: {}", e))
        })?;

    Ok(deleted_messages)
}

/// Delete multiple sessions for a user (bulk delete for banning).
/// Does NOT record costs (banned users forfeit their cost history).
///
/// Returns (sessions_deleted, messages_deleted, members_deleted)
pub fn delete_user_sessions(
    conn: &mut PooledConnection<ConnectionManager<PgConnection>>,
    user_id: Uuid,
) -> Result<(usize, usize, usize), DeleteSessionError> {
    // Get all session IDs for this user
    let session_ids: Vec<Uuid> = sessions::table
        .filter(sessions::user_id.eq(user_id))
        .select(sessions::id)
        .load(conn)
        .map_err(|e| {
            error!("Failed to get user sessions: {}", e);
            DeleteSessionError(format!("Failed to get sessions: {}", e))
        })?;

    if session_ids.is_empty() {
        return Ok((0, 0, 0));
    }

    // Delete messages for all user's sessions
    let deleted_messages =
        diesel::delete(messages::table.filter(messages::session_id.eq_any(&session_ids)))
            .execute(conn)
            .map_err(|e| {
                error!("Failed to delete user messages: {}", e);
                DeleteSessionError(format!("Failed to delete messages: {}", e))
            })?;

    // Delete session_members for all user's sessions
    let deleted_members = diesel::delete(
        session_members::table.filter(session_members::session_id.eq_any(&session_ids)),
    )
    .execute(conn)
    .map_err(|e| {
        error!("Failed to delete session members: {}", e);
        DeleteSessionError(format!("Failed to delete session members: {}", e))
    })?;

    // Delete all sessions
    let deleted_sessions = diesel::delete(sessions::table.filter(sessions::user_id.eq(user_id)))
        .execute(conn)
        .map_err(|e| {
            error!("Failed to delete user sessions: {}", e);
            DeleteSessionError(format!("Failed to delete sessions: {}", e))
        })?;

    Ok((deleted_sessions, deleted_messages, deleted_members))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_iso_cursor_accepts_fractional_seconds() {
        let parsed = parse_iso_cursor("2026-05-17T12:34:56.789").expect("parse");
        assert_eq!(
            parsed.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            "2026-05-17 12:34:56.789"
        );
    }

    #[test]
    fn parse_iso_cursor_accepts_second_precision() {
        let parsed = parse_iso_cursor("2026-05-17T12:34:56").expect("parse");
        assert_eq!(
            parsed.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2026-05-17 12:34:56"
        );
    }

    #[test]
    fn parse_iso_cursor_strips_trailing_z() {
        // js_sys::Date::to_iso_string emits a trailing 'Z' marker; the
        // NaiveDateTime parser rejects timezone info, so the helper must
        // trim it. Otherwise frontend-emitted timestamps would all 400.
        let parsed = parse_iso_cursor("2026-05-17T12:34:56.789Z").expect("parse");
        assert_eq!(
            parsed.format("%Y-%m-%d %H:%M:%S%.3f").to_string(),
            "2026-05-17 12:34:56.789"
        );
    }

    #[test]
    fn parse_iso_cursor_rejects_garbage() {
        assert!(parse_iso_cursor("not-a-timestamp").is_none());
        assert!(parse_iso_cursor("").is_none());
    }
}
