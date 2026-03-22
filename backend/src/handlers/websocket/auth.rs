use crate::AppState;
use diesel::prelude::*;
use tracing::{error, warn};
use uuid::Uuid;

/// Verify that a user has access to a session (is a member with any role)
pub fn verify_session_access(
    app_state: &AppState,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<crate::models::Session, ()> {
    let mut conn = app_state.db_pool.get().map_err(|e| {
        error!(
            "Failed to get database connection for session access check: {}",
            e
        );
    })?;
    use crate::schema::{session_members, sessions};
    sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .select(crate::models::Session::as_select())
        .first::<crate::models::Session>(&mut conn)
        .map_err(|e| {
            warn!(
                "Session access check failed for user {} on session {}: {}",
                user_id, session_id, e
            );
        })
}
