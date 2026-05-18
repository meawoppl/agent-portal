//! Centralized session-access checks used by HTTP and WebSocket handlers.
//!
//! Membership is layered: any session_members row grants read access, while
//! mutating actions (sending input, stopping the session, posting messages,
//! responding to permission prompts, interrupting) additionally require the
//! `editor` or `owner` role. The session row's `user_id` is treated as the
//! canonical owner — they can mutate even if no session_members row exists,
//! which keeps owner-only paths working for sessions persisted before the
//! membership table existed.
//!
//! Use [`can_mutate_role`] for pure role-string checks (the only thing the
//! WebSocket fast-path can test cheaply), [`verify_session_mutator`] for the
//! REST handlers (returns the typed Session row or an `AppError`), and
//! [`is_session_mutator`] for the WebSocket handlers (returns a bool, logs
//! warnings on database errors).
use crate::errors::AppError;
use crate::AppState;
use diesel::prelude::*;
use tracing::{error, warn};
use uuid::Uuid;

/// Returns true if the given role string permits mutating the session.
///
/// The valid roles in the `session_members` table are `owner`, `editor`, and
/// `viewer`. Only `owner` and `editor` may mutate session state; `viewer` is
/// read-only.
pub fn can_mutate_role(role: &str) -> bool {
    role == "owner" || role == "editor"
}

/// REST-facing check: verify the caller is allowed to mutate the session.
///
/// Returns the session row on success, [`AppError::NotFound`] otherwise (we
/// 404 rather than 403 to avoid leaking session existence to non-members,
/// matching the existing `verify_session_access` shape).
pub fn verify_session_mutator(
    conn: &mut diesel::pg::PgConnection,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<crate::models::Session, AppError> {
    use crate::schema::{session_members, sessions};

    // First check: is the caller the session owner (sessions.user_id)? This
    // path handles owner-only mutations even if no session_members row exists.
    if let Some(session) = sessions::table
        .filter(sessions::id.eq(session_id))
        .filter(sessions::user_id.eq(user_id))
        .select(crate::models::Session::as_select())
        .first::<crate::models::Session>(conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?
    {
        return Ok(session);
    }

    // Otherwise: look for a session_members row with a mutator role.
    sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .filter(
            session_members::role
                .eq("owner")
                .or(session_members::role.eq("editor")),
        )
        .select(crate::models::Session::as_select())
        .first::<crate::models::Session>(conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?
        .ok_or(AppError::NotFound("Session not found"))
}

/// WebSocket-facing check: returns true if the user may mutate the session.
///
/// Re-queried on each mutating message rather than cached on the connection
/// so role revocations take effect immediately for already-connected viewers.
/// Logs at warn-level on permission denial and at error-level on DB errors.
pub fn is_session_mutator(app_state: &AppState, session_id: Uuid, user_id: Uuid) -> bool {
    let mut conn = match app_state.db_pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            error!(
                "Failed to get database connection for session mutator check: {}",
                e
            );
            return false;
        }
    };

    use crate::schema::{session_members, sessions};

    // Owner check (no member row required).
    let is_owner: bool = match sessions::table
        .filter(sessions::id.eq(session_id))
        .filter(sessions::user_id.eq(user_id))
        .select(sessions::id)
        .first::<Uuid>(&mut conn)
        .optional()
    {
        Ok(opt) => opt.is_some(),
        Err(e) => {
            error!(
                "Failed to check owner status for session {} user {}: {}",
                session_id, user_id, e
            );
            return false;
        }
    };

    if is_owner {
        return true;
    }

    // Member-role check.
    let role: Option<String> = match session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .select(session_members::role)
        .first::<String>(&mut conn)
        .optional()
    {
        Ok(opt) => opt,
        Err(e) => {
            error!(
                "Failed to query session member role for session {} user {}: {}",
                session_id, user_id, e
            );
            return false;
        }
    };

    match role.as_deref() {
        Some(r) if can_mutate_role(r) => true,
        Some(r) => {
            warn!(
                "User {} attempted mutating action on session {} with non-mutator role '{}'",
                user_id, session_id, r
            );
            false
        }
        None => {
            warn!(
                "User {} attempted mutating action on session {} without membership",
                user_id, session_id
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_role_can_mutate() {
        assert!(can_mutate_role("owner"));
    }

    #[test]
    fn editor_role_can_mutate() {
        assert!(can_mutate_role("editor"));
    }

    #[test]
    fn viewer_role_cannot_mutate() {
        assert!(!can_mutate_role("viewer"));
    }

    #[test]
    fn unknown_role_cannot_mutate() {
        assert!(!can_mutate_role(""));
        assert!(!can_mutate_role("admin"));
        assert!(!can_mutate_role("OWNER")); // case sensitive — DB stores lowercase
    }
}

// =============================================================================
// DB-touching integration tests
// =============================================================================
//
// These exercise the layered owner/member-role rules end-to-end against a real
// Postgres database. They auto-skip (return early) when `DATABASE_URL` is not
// set, so they pass cleanly in the project's existing CI (which has no DB) and
// can be run locally via:
//
//   DATABASE_URL=postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal \
//     cargo test -p backend session_access::db_tests
//
// They use random Uuids + a fresh user per test so they don't conflict with
// dev/prod data, and they clean up after themselves on success.
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::models::{NewSessionMember, NewSessionWithId, NewUser, Session, User};
    use diesel::r2d2::{ConnectionManager, Pool};

    type DbPool = Pool<ConnectionManager<diesel::pg::PgConnection>>;

    /// Try to build a DB pool from `DATABASE_URL`; returns `None` if the env
    /// var isn't set (so the test silently skips on CI without DB).
    fn try_pool() -> Option<DbPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let manager = ConnectionManager::<diesel::pg::PgConnection>::new(url);
        Pool::builder().build(manager).ok()
    }

    /// Insert a throwaway user with a random google_id/email and return its row.
    fn make_user(conn: &mut diesel::pg::PgConnection, label: &str) -> User {
        use crate::schema::users;
        let nonce = Uuid::new_v4();
        let new_user = NewUser {
            google_id: format!("test_session_access_{}_{}", label, nonce),
            email: format!("test_session_access_{}_{}@example.invalid", label, nonce),
            name: Some(format!("Test {}", label)),
            avatar_url: None,
        };
        diesel::insert_into(users::table)
            .values(&new_user)
            .get_result::<User>(conn)
            .expect("insert test user")
    }

    /// Insert a throwaway session owned by `owner_id`.
    fn make_session(conn: &mut diesel::pg::PgConnection, owner_id: Uuid) -> Session {
        use crate::schema::sessions;
        let session_id = Uuid::new_v4();
        let new_session = NewSessionWithId {
            id: session_id,
            user_id: owner_id,
            session_name: format!("test-session-{}", session_id),
            session_key: session_id.to_string(),
            working_directory: "/tmp".to_string(),
            status: "active".to_string(),
            git_branch: None,
            client_version: None,
            hostname: "test-host".to_string(),
            launcher_id: None,
            agent_type: "claude".to_string(),
            repo_url: None,
            scheduled_task_id: None,
        };
        diesel::insert_into(sessions::table)
            .values(&new_session)
            .get_result::<Session>(conn)
            .expect("insert test session")
    }

    fn add_member(
        conn: &mut diesel::pg::PgConnection,
        session_id: Uuid,
        user_id: Uuid,
        role: &str,
    ) {
        use crate::schema::session_members;
        let new_member = NewSessionMember {
            session_id,
            user_id,
            role: role.to_string(),
        };
        diesel::insert_into(session_members::table)
            .values(&new_member)
            .execute(conn)
            .expect("insert session member");
    }

    /// Delete the test session + dependent members + the test users so the
    /// test is idempotent across runs.
    fn cleanup(conn: &mut diesel::pg::PgConnection, session_id: Uuid, user_ids: &[Uuid]) {
        use crate::schema::{session_members, sessions, users};
        let _ = diesel::delete(
            session_members::table.filter(session_members::session_id.eq(session_id)),
        )
        .execute(conn);
        let _ = diesel::delete(sessions::table.find(session_id)).execute(conn);
        for uid in user_ids {
            let _ = diesel::delete(users::table.find(uid)).execute(conn);
        }
    }

    #[test]
    fn viewer_cannot_mutate_and_editor_can() {
        let Some(pool) = try_pool() else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let mut conn = pool.get().expect("conn");

        let owner = make_user(&mut conn, "owner");
        let viewer = make_user(&mut conn, "viewer");
        let editor = make_user(&mut conn, "editor");
        let session = make_session(&mut conn, owner.id);
        // The session-creation code path adds an owner member row; here we
        // skip that and add only viewer + editor, then verify the
        // owner-without-members-row fallback still admits the owner.
        add_member(&mut conn, session.id, viewer.id, "viewer");
        add_member(&mut conn, session.id, editor.id, "editor");

        let viewer_result = verify_session_mutator(&mut conn, session.id, viewer.id);
        let editor_result = verify_session_mutator(&mut conn, session.id, editor.id);
        let owner_result = verify_session_mutator(&mut conn, session.id, owner.id);

        cleanup(&mut conn, session.id, &[owner.id, viewer.id, editor.id]);

        assert!(
            matches!(viewer_result, Err(AppError::NotFound(_))),
            "viewer should be denied"
        );
        assert!(editor_result.is_ok(), "editor should be allowed");
        assert!(
            owner_result.is_ok(),
            "owner (via sessions.user_id) should be allowed without a member row"
        );
    }

    #[test]
    fn non_member_cannot_mutate() {
        let Some(pool) = try_pool() else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let mut conn = pool.get().expect("conn");

        let owner = make_user(&mut conn, "owner_nm");
        let stranger = make_user(&mut conn, "stranger");
        let session = make_session(&mut conn, owner.id);

        let stranger_result = verify_session_mutator(&mut conn, session.id, stranger.id);
        cleanup(&mut conn, session.id, &[owner.id, stranger.id]);

        assert!(
            matches!(stranger_result, Err(AppError::NotFound(_))),
            "non-member should be denied"
        );
    }

    #[test]
    fn owner_member_row_can_mutate() {
        let Some(pool) = try_pool() else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let mut conn = pool.get().expect("conn");

        // Verifies the canonical path that `registration.rs::create_new_session`
        // produces: owner has both sessions.user_id AND a `role = owner` row.
        let owner = make_user(&mut conn, "owner_mr");
        let session = make_session(&mut conn, owner.id);
        add_member(&mut conn, session.id, owner.id, "owner");

        let owner_result = verify_session_mutator(&mut conn, session.id, owner.id);
        cleanup(&mut conn, session.id, &[owner.id]);

        assert!(owner_result.is_ok());
    }
}
