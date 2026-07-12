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
//! Use [`verify_session_reader`] for read-only access (any membership role),
//! [`verify_session_mutator`] for mutating REST handlers (returns the typed
//! Session row or an `AppError`), [`is_session_mutator`] for the WebSocket
//! handlers (returns a bool, logs warnings on database errors), and
//! [`verify_session_owner`] / [`verify_owner_membership`] for owner-only
//! paths.
use crate::errors::AppError;
use crate::AppState;
use diesel::prelude::*;
use tracing::{error, warn};
use uuid::Uuid;

/// Read-access check: verify the caller is a member of the session with any
/// role (including `viewer`).
///
/// Returns the session row on success, [`AppError::NotFound`] otherwise (we
/// 404 rather than 403 to avoid leaking session existence to non-members).
pub fn verify_session_reader(
    conn: &mut diesel::pg::PgConnection,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<crate::models::Session, AppError> {
    use crate::schema::{session_members, sessions};
    sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .select(crate::models::Session::as_select())
        .first::<crate::models::Session>(conn)
        .optional()?
        .ok_or(AppError::NotFound("Session not found"))
}

/// Owner check for destructive paths (e.g. session delete): accepts either
/// the `sessions.user_id` owner (works even when no membership row exists,
/// covering sessions persisted before the membership table) OR a
/// `session_members.role = owner` row (the post-membership-table canonical
/// owner).
///
/// Returns the session row on success, [`AppError::NotFound`] otherwise.
pub fn verify_session_owner(
    conn: &mut diesel::pg::PgConnection,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<crate::models::Session, AppError> {
    use crate::schema::{session_members, sessions};

    if let Some(session) = sessions::table
        .filter(sessions::id.eq(session_id))
        .filter(sessions::user_id.eq(user_id))
        .select(crate::models::Session::as_select())
        .first::<crate::models::Session>(conn)
        .optional()?
    {
        return Ok(session);
    }

    sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .filter(session_members::role.eq("owner"))
        .select(crate::models::Session::as_select())
        .first::<crate::models::Session>(conn)
        .optional()?
        .ok_or(AppError::NotFound("Session not found"))
}

/// Owner gate for member-management paths (add member, change member role):
/// requires a `session_members.role = owner` row — deliberately *without* the
/// `sessions.user_id` fallback [`verify_session_owner`] has, and returning
/// [`AppError::Forbidden`] (403) rather than 404, matching the historical
/// member-management semantics.
pub fn verify_owner_membership(
    conn: &mut diesel::pg::PgConnection,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<(), AppError> {
    use crate::schema::session_members;
    session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .filter(session_members::role.eq("owner"))
        .select(session_members::user_id)
        .first::<Uuid>(conn)
        .optional()?
        .ok_or(AppError::Forbidden)?;
    Ok(())
}

/// REST-facing check: verify the caller is allowed to mutate the session.
///
/// Returns the session row on success, [`AppError::NotFound`] otherwise (we
/// 404 rather than 403 to avoid leaking session existence to non-members,
/// matching the [`verify_session_reader`] shape).
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
        .optional()?
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
        .optional()?
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

    match verify_session_mutator(&mut conn, session_id, user_id) {
        Ok(_) => true,
        Err(AppError::NotFound(_)) => {
            warn!(
                "User {} attempted mutating action on session {} without owner/editor access",
                user_id, session_id
            );
            false
        }
        Err(e) => {
            error!(
                "Failed to verify mutator access for session {} user {}: {:?}",
                session_id, user_id, e
            );
            false
        }
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
            status: shared::SessionStatus::Active.as_str().to_string(),
            git_branch: None,
            client_version: None,
            hostname: "test-host".to_string(),
            launcher_id: None,
            agent_type: "claude".to_string(),
            repo_url: None,
            scheduled_task_id: None,
            paused: false,
            claude_args: serde_json::Value::Array(Vec::new()),
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
        let Some(pool) = crate::test_support::shared_pool() else {
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
        let Some(pool) = crate::test_support::shared_pool() else {
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
        let Some(pool) = crate::test_support::shared_pool() else {
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
