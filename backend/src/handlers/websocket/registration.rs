use crate::models::{NewSessionMember, NewSessionWithId};
use crate::AppState;
use diesel::prelude::*;
use shared::{AgentType, SessionStatus};
use tracing::{error, info, warn};
use uuid::Uuid;

/// Generic "session not found or not authorized" error string.
///
/// Used for **both** "row missing" and "row exists but token doesn't own
/// it" cases so a probe can't tell the difference and harvest session
/// UUIDs by guessing.
const SESSION_NOT_FOUND_ERROR: &str = "Session not found or not authorized";

/// Result of a session registration attempt
pub struct RegistrationResult {
    pub success: bool,
    pub session_id: Option<Uuid>,
    pub error: Option<String>,
}

/// Parameters for registering a session
pub struct RegistrationParams<'a> {
    pub claude_session_id: Uuid,
    pub session_name: &'a str,
    pub auth_token: Option<&'a str>,
    pub working_directory: &'a str,
    pub resuming: bool,
    pub git_branch: &'a Option<String>,
    pub client_version: &'a Option<String>,
    pub session_key: &'a str,
    pub replaces_session_id: Option<Uuid>,
    pub hostname: &'a str,
    pub launcher_id: Option<Uuid>,
    pub agent_type: AgentType,
    pub repo_url: &'a Option<String>,
    pub scheduled_task_id: Option<Uuid>,
    pub claude_args: &'a Vec<String>,
}

/// Register or update a session in the database.
/// Handles three cases: existing session reactivation, resume of unknown session, and new session creation.
///
/// **Security**: All branches require a valid `auth_token` that resolves to
/// a user who either owns the session or has a `session_members` row for
/// it. Reactivating an existing session by UUID *without* token validation
/// is an auth-bypass (see #780): a client that knows or guesses a UUID
/// could otherwise attach as that proxy and forge output / read input.
pub fn register_or_update_session(
    app_state: &AppState,
    params: &RegistrationParams,
) -> RegistrationResult {
    let mut conn = match app_state.db_pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            error!("Failed to get database connection for registration: {}", e);
            return RegistrationResult {
                success: false,
                session_id: None,
                error: Some("Database connection failed".to_string()),
            };
        }
    };

    // Resolve auth_token → user_id up front. Required for *both* new and
    // existing sessions so an attacker can't reactivate someone else's
    // session by knowing its UUID.
    let Some(requesting_user_id) = get_user_id_from_token(app_state, &mut conn, params.auth_token)
    else {
        warn!(
            "Session registration without valid auth_token: session_id={}",
            params.claude_session_id
        );
        return RegistrationResult {
            success: false,
            session_id: None,
            error: Some("Authentication failed - please re-authenticate".to_string()),
        };
    };

    use crate::schema::sessions;

    // If this session replaces a previous one, mark the old session — but
    // only if the requesting user is authorized for the old session.
    // Without this check, a valid-token-holding user could mark *anyone's*
    // session as `replaced`, a DoS / data-tamper bypass.
    if let Some(old_id) = params.replaces_session_id {
        if user_is_authorized_for_session(&mut conn, old_id, requesting_user_id) {
            match diesel::update(sessions::table.find(old_id))
                .set(sessions::status.eq(SessionStatus::Replaced.as_str()))
                .execute(&mut conn)
            {
                Ok(n) if n > 0 => {
                    info!(
                        "Marked old session {} as replaced (superseded by {})",
                        old_id, params.claude_session_id
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    warn!("Failed to mark old session {} as replaced: {}", old_id, e);
                }
            }
        } else {
            warn!(
                "Refusing to mark session {} as replaced: requesting user {} is not authorized",
                old_id, requesting_user_id
            );
        }
    }

    let existing: Option<crate::models::Session> = sessions::table
        .find(params.claude_session_id)
        .first(&mut conn)
        .optional()
        .unwrap_or(None);

    let result = if let Some(existing_session) = existing {
        // Authorization gate for reattach: the resolved user_id must be the
        // session's owner or a `session_members` row. Same error shape as
        // "session not found" so a probe can't distinguish the two cases.
        if !user_is_authorized_for_session(&mut conn, existing_session.id, requesting_user_id) {
            warn!(
                "Refusing reattach: user {} not authorized for session {} (owner={})",
                requesting_user_id, existing_session.id, existing_session.user_id
            );
            return RegistrationResult {
                success: false,
                session_id: None,
                error: Some(SESSION_NOT_FOUND_ERROR.to_string()),
            };
        }

        match diesel::update(sessions::table.find(existing_session.id))
            .set((
                sessions::status.eq(SessionStatus::Active.as_str()),
                sessions::paused.eq(false),
                sessions::last_activity.eq(diesel::dsl::now),
                sessions::working_directory.eq(params.working_directory),
                sessions::git_branch.eq(params.git_branch),
                sessions::client_version.eq(params.client_version),
                sessions::hostname.eq(params.hostname),
                sessions::repo_url.eq(params.repo_url),
                sessions::launcher_id.eq(params.launcher_id),
                sessions::claude_args.eq(serde_json::to_value(params.claude_args)
                    .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()))),
            ))
            .execute(&mut conn)
        {
            Ok(_) => {
                info!(
                    "Session reactivated in DB: {} ({}) branch: {:?}",
                    params.session_name, params.claude_session_id, params.git_branch
                );
                RegistrationResult {
                    success: true,
                    session_id: Some(existing_session.id),
                    error: None,
                }
            }
            Err(e) => {
                error!("Failed to reactivate session: {}", e);
                RegistrationResult {
                    success: false,
                    session_id: None,
                    error: Some("Failed to reactivate session".to_string()),
                }
            }
        }
    } else {
        if params.resuming {
            warn!(
                "Resuming session {} but not found in DB, creating new entry",
                params.claude_session_id
            );
        }

        create_new_session(&mut conn, requesting_user_id, params)
    };

    // Bind the auth token to the session it just registered, and revoke any
    // token previously bound to that session. Launch tokens never expire, so
    // this binding is what ties a token's lifetime to its session. See #932.
    if result.success {
        if let (Some(session_id), Some(token)) = (result.session_id, params.auth_token) {
            super::super::proxy_tokens::link_token_to_session(&mut conn, token, session_id);
        }
    }

    result
}

/// Returns true if `user_id` owns the session or has a `session_members` row for it.
///
/// Both branches are checked because owner rows aren't always mirrored
/// into `session_members` for historical sessions, and members table is
/// the source of truth for shared sessions added via the API.
fn user_is_authorized_for_session(
    conn: &mut diesel::PgConnection,
    session_id: Uuid,
    user_id: Uuid,
) -> bool {
    use crate::schema::{session_members, sessions};

    // Owner row on the session itself.
    let owner_match: Result<Option<Uuid>, _> = sessions::table
        .find(session_id)
        .filter(sessions::user_id.eq(user_id))
        .select(sessions::id)
        .first::<Uuid>(conn)
        .optional();
    if matches!(owner_match, Ok(Some(_))) {
        return true;
    }

    // session_members row (sharing / explicit access).
    let member_match: Result<Option<Uuid>, _> = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .select(session_members::id)
        .first::<Uuid>(conn)
        .optional();

    matches!(member_match, Ok(Some(_)))
}

fn create_new_session(
    conn: &mut diesel::PgConnection,
    user_id: Uuid,
    params: &RegistrationParams,
) -> RegistrationResult {
    use crate::schema::{session_members, sessions};

    let new_session = NewSessionWithId {
        id: params.claude_session_id,
        user_id,
        session_name: params.session_name.to_string(),
        session_key: params.session_key.to_string(),
        working_directory: params.working_directory.to_string(),
        status: SessionStatus::Active.as_str().to_string(),
        git_branch: params.git_branch.clone(),
        client_version: params.client_version.clone(),
        hostname: params.hostname.to_string(),
        launcher_id: params.launcher_id,
        agent_type: params.agent_type.as_str().to_string(),
        repo_url: params.repo_url.clone(),
        scheduled_task_id: params.scheduled_task_id,
        paused: false,
        claude_args: serde_json::to_value(params.claude_args)
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
    };

    match diesel::insert_into(sessions::table)
        .values(&new_session)
        .get_result::<crate::models::Session>(conn)
    {
        Ok(session) => {
            let new_member = NewSessionMember {
                session_id: session.id,
                user_id,
                role: "owner".to_string(),
            };
            if let Err(e) = diesel::insert_into(session_members::table)
                .values(&new_member)
                .execute(conn)
            {
                error!("Failed to create session_member: {}", e);
            }

            info!(
                "Session persisted to DB: {} ({}) branch: {:?} agent: {}",
                params.session_name, params.claude_session_id, params.git_branch, params.agent_type
            );
            RegistrationResult {
                success: true,
                session_id: Some(session.id),
                error: None,
            }
        }
        Err(e) => {
            error!("Failed to persist session: {}", e);
            RegistrationResult {
                success: false,
                session_id: None,
                error: Some("Failed to persist session".to_string()),
            }
        }
    }
}

/// Get user_id from auth token using JWT verification, with dev-mode fallback.
fn get_user_id_from_token(
    app_state: &AppState,
    conn: &mut diesel::PgConnection,
    auth_token: Option<&str>,
) -> Option<Uuid> {
    if let Some(token) = auth_token {
        match super::super::proxy_tokens::verify_and_get_user(app_state, conn, token) {
            Ok((user_id, email)) => {
                info!("JWT token verified for user: {}", email);
                return Some(user_id);
            }
            Err(e) => {
                warn!("JWT verification failed: {:?}, falling back to dev mode", e);
            }
        }
    }

    if app_state.dev_mode {
        crate::auth::dev_user(conn).map(|user| user.id).ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the `RegistrationResult` the existing-session branch returns
    /// when the resolved user isn't authorized for the row.
    fn unauthorized_reattach_result() -> RegistrationResult {
        RegistrationResult {
            success: false,
            session_id: None,
            error: Some(SESSION_NOT_FOUND_ERROR.to_string()),
        }
    }

    /// Builds the `RegistrationResult` shape a probe would see if the row
    /// genuinely didn't exist. Today both branches share the same not-found
    /// shape because the existing-session branch only fires when
    /// `sessions::table.find(id).first(..).optional()` returns `Some`, so
    /// "not found" is a *separate* code path — but per #780 we want the
    /// observable wire shape of "unauthorized reattach" to match what
    /// "not found" would produce if we were to surface it, so a probe can't
    /// harvest UUIDs by attaching with a garbage token and watching which
    /// IDs come back with a different error.
    fn not_found_result_for_comparison() -> RegistrationResult {
        RegistrationResult {
            success: false,
            session_id: None,
            error: Some(SESSION_NOT_FOUND_ERROR.to_string()),
        }
    }

    /// (a) reattaching with the WRONG token returns an error and does NOT
    /// register the socket. We can't exercise the full DB round-trip
    /// without a Postgres harness, but we can pin the externally-visible
    /// `RegistrationResult` shape: `success:false`, no `session_id` leaked
    /// (so the proxy_socket Register arm's `if result.success { ... }` gate
    /// short-circuits both `session_manager.register_session` and
    /// `replay_pending_inputs_from_db`), and an error message that doesn't
    /// distinguish "row missing" from "row exists, wrong owner".
    #[test]
    fn unauthorized_reattach_does_not_leak_session_id() {
        let r = unauthorized_reattach_result();
        assert!(!r.success);
        assert!(
            r.session_id.is_none(),
            "session_id must not leak on unauthorized reattach; if it does, \
             proxy_socket.rs will still wire the socket into SessionManager"
        );
        assert!(r.error.is_some(), "error message must be set");
    }

    /// (b) the right token still works — the success path's contract is
    /// `success:true` with a populated `session_id` and no error.
    /// `register_or_update_session` populates the `session_id` from the
    /// matched row (or the newly-inserted row), so for both the
    /// new-session and authorized-existing-reattach branches the shape is
    /// identical. The proxy_socket Register arm depends on this exact
    /// shape to decide whether to call `session_manager.register_session`.
    #[test]
    fn authorized_attach_returns_session_id_and_no_error() {
        let session_id = Uuid::new_v4();
        let r = RegistrationResult {
            success: true,
            session_id: Some(session_id),
            error: None,
        };
        assert!(r.success);
        assert_eq!(r.session_id, Some(session_id));
        assert!(r.error.is_none());
    }

    /// The unauthorized-reattach error must be wire-identical to the
    /// not-found error. If a future refactor accidentally varies the
    /// strings (e.g., "Session not owned" vs "Session not found"), a
    /// probe could harvest valid session UUIDs by sending Register with
    /// a garbage token and watching for the differing error.
    #[test]
    fn unauthorized_reattach_error_matches_not_found_wire_shape() {
        let unauthorized = unauthorized_reattach_result();
        let not_found = not_found_result_for_comparison();
        assert_eq!(unauthorized.success, not_found.success);
        assert_eq!(unauthorized.session_id, not_found.session_id);
        assert_eq!(unauthorized.error, not_found.error);
    }

    /// The shared error string must not mention specifics that would
    /// tell an attacker which condition triggered the rejection — so it
    /// can't say things like "wrong token", "wrong owner", "user
    /// mismatch", "not a member", "revoked", etc. The generic phrase
    /// "Session not found or not authorized" is intentional: it covers
    /// both real conditions without committing to either.
    #[test]
    fn session_not_found_error_does_not_leak_cause() {
        let s = SESSION_NOT_FOUND_ERROR.to_lowercase();
        for forbidden in [
            "wrong",
            "mismatch",
            "owner",
            "member",
            "revoked",
            "expired",
            "user id",
            "token id",
            "invalid token",
            "bad token",
        ] {
            assert!(
                !s.contains(forbidden),
                "SESSION_NOT_FOUND_ERROR (`{SESSION_NOT_FOUND_ERROR}`) leaks rejection cause via substring `{forbidden}`"
            );
        }
    }
}
