use axum::{
    extract::{Path, State},
    Json,
};
use diesel::prelude::*;
use serde::Serialize;
use shared::api::{
    AddMemberRequest, ResolveProxySessionRequest, ResolveProxySessionResponse, SessionMemberInfo,
    SessionMembersResponse, UpdateMemberRoleRequest,
};
use shared::{SessionRole, SessionStatus};
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    auth::CurrentUserId,
    errors::AppError,
    handlers::responses::EmptyResponse,
    models::{Message, NewSessionMember, Session, SessionMember},
    AppState,
};

/// Session with the current user's role included.
///
/// Serializes via `#[serde(flatten)]` of the full `Session` row plus a
/// typed `my_role` — this is the on-wire shape consumed by the frontend.
/// The frontend deserializes the same bytes into `shared::api::SessionsResponse`
/// (whose `sessions` field is `Vec<shared::SessionInfo>`); `SessionInfo`
/// silently drops the per-row stats fields it doesn't care about
/// (`total_cost_usd`, `input_tokens`, `input_seq`, etc.). Don't shrink this
/// struct without also auditing every other consumer of the wire shape.
#[derive(Debug, Serialize)]
pub struct SessionWithRole {
    #[serde(flatten)]
    pub session: Session,
    pub my_role: SessionRole,
}

#[derive(Debug, Serialize)]
pub struct SessionListResponse {
    pub sessions: Vec<SessionWithRole>,
}

pub async fn list_sessions(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
) -> Result<Json<SessionListResponse>, AppError> {
    let mut conn = app_state.conn()?;

    use crate::schema::{session_members, sessions};

    let results: Vec<(Session, String)> = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(session_members::user_id.eq(current_user_id))
        .filter(sessions::status.ne(SessionStatus::Replaced.as_str()))
        .select((Session::as_select(), session_members::role))
        .order((
            sessions::last_messaged_at.desc(),
            sessions::created_at.desc(),
            sessions::id.asc(),
        ))
        .load(&mut conn)?;

    let sessions_with_role = results
        .into_iter()
        .map(|(mut session, role)| {
            // `Session::launcher_version` (the DB column added in #1454) holds
            // the launcher version captured at session-create time. This wire
            // shape has always exposed the *live* launcher version, so replace
            // the flattened value with the currently-connected one. Carrying
            // both a flattened and a separate explicit `launcher_version` field
            // produced a duplicate JSON key that failed frontend deserialization
            // (emptying the whole session list) whenever a launcher was online.
            session.launcher_version = app_state
                .session_manager
                .launcher_version(session.launcher_id);
            SessionWithRole {
                session,
                my_role: role.parse().unwrap_or(SessionRole::Unknown),
            }
        })
        .collect();

    Ok(Json(SessionListResponse {
        sessions: sessions_with_role,
    }))
}

pub async fn resolve_proxy_session(
    State(app_state): State<Arc<AppState>>,
    Json(req): Json<ResolveProxySessionRequest>,
) -> Result<Json<ResolveProxySessionResponse>, AppError> {
    let mut conn = app_state.conn()?;
    let current_user_id = proxy_request_user_id(&app_state, &mut conn, req.auth_token.as_deref())?;

    use crate::schema::{session_members, sessions};

    let mut query = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(session_members::user_id.eq(current_user_id))
        .filter(sessions::working_directory.eq(req.working_directory))
        .filter(sessions::agent_type.eq(req.agent_type.as_str()))
        .filter(sessions::scheduled_task_id.is_null())
        .filter(sessions::status.ne(SessionStatus::Replaced.as_str()))
        .filter(sessions::paused.eq(false))
        .select(Session::as_select())
        .into_boxed();

    if let Some(hostname) = req.hostname.as_deref().filter(|h| !h.is_empty()) {
        query = query.filter(sessions::hostname.eq(hostname));
    }

    let session = query
        .order((sessions::last_activity.desc(), sessions::created_at.desc()))
        .first::<Session>(&mut conn)
        .optional()?;

    Ok(Json(match session {
        Some(session) => ResolveProxySessionResponse {
            session_id: Some(session.id),
            session_name: Some(session.session_name),
            created_at: Some(session.created_at.and_utc().to_rfc3339()),
            last_activity: Some(session.last_activity.and_utc().to_rfc3339()),
        },
        None => ResolveProxySessionResponse {
            session_id: None,
            session_name: None,
            created_at: None,
            last_activity: None,
        },
    }))
}

fn proxy_request_user_id(
    app_state: &AppState,
    conn: &mut diesel::pg::PgConnection,
    auth_token: Option<&str>,
) -> Result<Uuid, AppError> {
    if let Some(token) = auth_token {
        return crate::handlers::proxy_tokens::verify_and_get_user(app_state, conn, token)
            .map(|(user_id, _)| user_id);
    }

    if app_state.dev_mode {
        return Ok(crate::auth::dev_user(conn)?.id);
    }

    Err(AppError::Unauthorized)
}

#[derive(Debug, Serialize)]
pub struct SessionDetailResponse {
    pub session: Session,
    pub recent_messages: Vec<Message>,
}

pub async fn get_session(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<Uuid>,
) -> Result<Json<SessionDetailResponse>, AppError> {
    let mut conn = app_state.conn()?;

    use crate::schema::{messages, session_members, sessions};

    let session = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .select(Session::as_select())
        .first::<Session>(&mut conn)
        .optional()?
        .ok_or(AppError::NotFound("Session not found"))?;

    let recent_messages = messages::table
        .filter(messages::session_id.eq(session_id))
        .order(messages::created_at.desc())
        .limit(50)
        .load::<Message>(&mut conn)?;

    Ok(Json(SessionDetailResponse {
        session,
        recent_messages,
    }))
}

pub async fn delete_session(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;

    // Delete remains owner-only — editors can mutate session state but not
    // destroy the row. See `session_access::verify_session_owner` for the
    // sessions.user_id / owner-member-row acceptance rules.
    let session = crate::handlers::session_access::verify_session_owner(
        &mut conn,
        session_id,
        current_user_id,
    )?;

    // Close = archive-then-delete. When the archive is enabled, take a final
    // snapshot before destroying the hot rows so the session stays readable in
    // History even if the sweep never got to it (closed before the idle
    // window). Archive failure aborts the close — the same no-data-loss
    // invariant as retention's held trims (`RETENTION_TRIM_HELD`) — so an
    // archive outage never silently discards history. Runs on the blocking
    // pool: the object store blocks on a captured runtime handle.
    if let Some(runtime) = app_state.archive.clone() {
        let archive_session = session.clone();
        let pool = app_state.db_pool.clone();
        let archived = tokio::task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|e| AppError::Internal(format!("db pool: {e}")))?;
            Ok::<bool, AppError>(crate::background::ensure_session_archived(
                &mut conn,
                &runtime,
                &archive_session,
            ))
        })
        .await
        .map_err(|e| AppError::Internal(format!("archive task failed: {e}")))??;
        if !archived {
            return Err(AppError::ServiceUnavailable(
                "history archive is unavailable; session was not closed - try again",
            ));
        }
    }

    app_state.session_manager.disconnect_session(session_id);
    app_state.session_manager.stop_session_on_launcher(
        session_id,
        session.launcher_id,
        Some(session.working_directory.clone()),
    );

    super::helpers::delete_session_with_data(&mut conn, &session, true)
        .map_err(|e| AppError::Internal(format!("{:?}", e)))?;

    Ok(EmptyResponse::NO_CONTENT)
}

pub async fn stop_session(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;

    // Stopping a session is a mutation — viewer-role members must not be
    // able to terminate sessions they only have read access to. The helper
    // also accepts the session's `sessions.user_id` owner row, so owners
    // without a `session_members` row still work.
    let session = crate::handlers::session_access::verify_session_mutator(
        &mut conn,
        session_id,
        current_user_id,
    )?;

    // Persist the desired state before touching the live process. Launcher
    // heartbeat reconciliation starts every unpaused session that is missing
    // from the launcher's running set, so leaving `paused = false` here makes a
    // successful stop (including `agent-portal seppuku`) relaunch itself on the
    // next heartbeat. Writing first also closes the race where reconciliation
    // could run between killing the process and recording that it must stay
    // down.
    use crate::schema::sessions;
    diesel::update(sessions::table.find(session_id))
        .set((
            sessions::paused.eq(true),
            sessions::status.eq(SessionStatus::Disconnected.as_str()),
            sessions::updated_at.eq(diesel::dsl::now),
        ))
        .execute(&mut conn)?;

    // Stopping is idempotent: the durable do-not-relaunch state is the result.
    // A missing live proxy/launcher process already satisfies that result.
    app_state.session_manager.disconnect_session(session_id);
    app_state.session_manager.stop_session_on_launcher(
        session_id,
        session.launcher_id,
        Some(session.working_directory.clone()),
    );

    Ok(EmptyResponse::ACCEPTED)
}

pub async fn pause_session(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;
    crate::handlers::session_access::verify_session_mutator(
        &mut conn,
        session_id,
        current_user_id,
    )?;

    use crate::schema::sessions;
    diesel::update(sessions::table.find(session_id))
        .set((
            sessions::paused.eq(true),
            sessions::status.eq(SessionStatus::Disconnected.as_str()),
            sessions::updated_at.eq(diesel::dsl::now),
        ))
        .execute(&mut conn)?;

    app_state.session_manager.disconnect_session(session_id);
    app_state
        .session_manager
        .pause_session_on_launcher(session_id);

    Ok(EmptyResponse::ACCEPTED)
}

pub async fn resume_session(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;
    let session = crate::handlers::session_access::verify_session_mutator(
        &mut conn,
        session_id,
        current_user_id,
    )?;

    let launcher_id = resolve_resume_launcher(&app_state, &session)
        .ok_or(AppError::NotFound("Launcher not connected"))?;

    let auth_token = crate::handlers::launchers::mint_launch_token(&app_state, session.user_id)?;
    let request_id = Uuid::new_v4();
    let claude_args =
        serde_json::from_value::<Vec<String>>(session.claude_args.clone()).unwrap_or_default();
    let agent_type = session
        .agent_type
        .parse()
        .unwrap_or(shared::AgentType::Claude);

    use crate::schema::sessions;
    diesel::update(sessions::table.find(session_id))
        .set((
            sessions::paused.eq(false),
            sessions::updated_at.eq(diesel::dsl::now),
        ))
        .execute(&mut conn)?;

    let launch_msg = shared::ServerToLauncher::LaunchSession {
        request_id,
        user_id: session.user_id,
        auth_token,
        working_directory: session.working_directory.clone(),
        session_name: Some(session.session_name.clone()),
        claude_args,
        agent_type,
        scheduled_task_id: session.scheduled_task_id,
        resume_session_id: Some(session.id),
        // Resuming an existing session: run in its recorded working directory,
        // never create a new worktree.
        resume: Some(true),
        create_worktree: false,
        worktree_branch: None,
        fork_from_session_id: None,
        fork_point_turn_id: None,
    };

    if !app_state
        .session_manager
        .send_to_launcher(&launcher_id, launch_msg)
    {
        let _ = diesel::update(sessions::table.find(session_id))
            .set(sessions::paused.eq(true))
            .execute(&mut conn);
        return Err(AppError::Internal(
            "Failed to send resume request to launcher".to_string(),
        ));
    }

    Ok(EmptyResponse::ACCEPTED)
}

fn resolve_resume_launcher(app_state: &AppState, session: &Session) -> Option<Uuid> {
    if let Some(launcher_id) = session.launcher_id {
        if app_state
            .session_manager
            .launchers
            .get(&launcher_id)
            .is_some_and(|l| l.user_id == session.user_id)
        {
            return Some(launcher_id);
        }
    }

    app_state
        .session_manager
        .launchers
        .iter()
        .find(|entry| {
            entry.value().user_id == session.user_id && entry.value().hostname == session.hostname
        })
        .map(|entry| *entry.key())
}

// ============================================================================
// Session Member Management
// ============================================================================

/// User info selected from joined query
#[derive(Debug, Queryable)]
struct UserBasicInfo {
    id: Uuid,
    email: String,
    name: Option<String>,
    nickname: Option<String>,
}

/// Validate a member role supplied by the caller
fn validate_member_role(role: SessionRole) -> Result<(), AppError> {
    if !role.is_assignable_member_role() {
        return Err(AppError::BadRequest("Invalid role"));
    }
    Ok(())
}

fn ensure_member_not_existing(member_exists: bool) -> Result<(), AppError> {
    if member_exists {
        return Err(AppError::BadRequest("User is already a member"));
    }
    Ok(())
}

fn ensure_owner_not_self_removal(
    is_owner: bool,
    current_user_id: Uuid,
    target_user_id: Uuid,
) -> Result<(), AppError> {
    if is_owner && current_user_id == target_user_id {
        return Err(AppError::BadRequest("Owner cannot remove themselves"));
    }
    Ok(())
}

fn ensure_not_self_role_change(
    current_user_id: Uuid,
    target_user_id: Uuid,
) -> Result<(), AppError> {
    if current_user_id == target_user_id {
        return Err(AppError::BadRequest("Cannot change own role"));
    }
    Ok(())
}

/// List all members of a session
pub async fn list_session_members(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<Uuid>,
) -> Result<Json<SessionMembersResponse>, AppError> {
    let mut conn = app_state.conn()?;

    use crate::schema::{session_members, users};

    session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .first::<SessionMember>(&mut conn)
        .optional()?
        .ok_or(AppError::NotFound("Session not found"))?;

    let members: Vec<(SessionMember, UserBasicInfo)> = session_members::table
        .inner_join(users::table.on(users::id.eq(session_members::user_id)))
        .filter(session_members::session_id.eq(session_id))
        .select((
            SessionMember::as_select(),
            (users::id, users::email, users::name, users::nickname),
        ))
        .load(&mut conn)?;

    let member_infos = members
        .into_iter()
        .map(|(member, user)| SessionMemberInfo {
            user_id: user.id,
            // Prefer the member's chosen nickname for the label; email stays
            // shown alongside so the row still maps to a person (#1485).
            name: crate::handlers::helpers::preferred_name(
                user.nickname.as_deref(),
                user.name.as_deref(),
            ),
            email: user.email,
            role: member.role.parse().unwrap_or(SessionRole::Unknown),
            created_at: member.created_at,
        })
        .collect();

    Ok(Json(SessionMembersResponse {
        members: member_infos,
    }))
}

/// Add a member to a session (owner only)
pub async fn add_session_member(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<Uuid>,
    Json(req): Json<AddMemberRequest>,
) -> Result<EmptyResponse, AppError> {
    validate_member_role(req.role)?;

    let mut conn = app_state.conn()?;

    use crate::schema::{session_members, users};

    crate::handlers::session_access::verify_owner_membership(
        &mut conn,
        session_id,
        current_user_id,
    )?;

    let target_user_id: Uuid = users::table
        .filter(users::email.eq(&req.email))
        .select(users::id)
        .first(&mut conn)
        .optional()?
        .ok_or(AppError::NotFound("User not found"))?;

    let existing = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(target_user_id))
        .first::<SessionMember>(&mut conn)
        .optional()?;

    ensure_member_not_existing(existing.is_some())?;

    let new_member = NewSessionMember {
        session_id,
        user_id: target_user_id,
        role: req.role.as_str().to_string(),
    };

    diesel::insert_into(session_members::table)
        .values(&new_member)
        .execute(&mut conn)?;

    Ok(EmptyResponse::CREATED)
}

/// Remove a member from a session
/// Owner can remove anyone; non-owner can only remove themselves (leave)
pub async fn remove_session_member(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path((session_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;

    use crate::schema::session_members;

    let current_membership = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .first::<SessionMember>(&mut conn)
        .optional()?
        .ok_or(AppError::NotFound("Session not found"))?;

    let is_owner = current_membership
        .role
        .parse::<SessionRole>()
        .is_ok_and(SessionRole::can_manage_members);

    if !is_owner && current_user_id != target_user_id {
        return Err(AppError::Forbidden);
    }

    ensure_owner_not_self_removal(is_owner, current_user_id, target_user_id)?;

    let deleted = diesel::delete(
        session_members::table
            .filter(session_members::session_id.eq(session_id))
            .filter(session_members::user_id.eq(target_user_id)),
    )
    .execute(&mut conn)?;

    if deleted == 0 {
        return Err(AppError::NotFound("Member not found"));
    }

    Ok(EmptyResponse::NO_CONTENT)
}

/// Update a member's role (owner only)
pub async fn update_session_member_role(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path((session_id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateMemberRoleRequest>,
) -> Result<EmptyResponse, AppError> {
    validate_member_role(req.role)?;

    let mut conn = app_state.conn()?;

    use crate::schema::session_members;

    crate::handlers::session_access::verify_owner_membership(
        &mut conn,
        session_id,
        current_user_id,
    )?;

    ensure_not_self_role_change(current_user_id, target_user_id)?;

    let updated = diesel::update(
        session_members::table
            .filter(session_members::session_id.eq(session_id))
            .filter(session_members::user_id.eq(target_user_id)),
    )
    .set(session_members::role.eq(req.role.as_str()))
    .execute(&mut conn)?;

    if updated == 0 {
        return Err(AppError::NotFound("Member not found"));
    }

    Ok(EmptyResponse::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_member_role_validation_is_bad_request() {
        let err = validate_member_role(SessionRole::Owner).unwrap_err();
        assert!(matches!(err, AppError::BadRequest("Invalid role")));
    }

    #[test]
    fn duplicate_member_validation_is_bad_request() {
        let err = ensure_member_not_existing(true).unwrap_err();
        assert!(matches!(
            err,
            AppError::BadRequest("User is already a member")
        ));
    }

    #[test]
    fn owner_self_removal_validation_is_bad_request() {
        let user_id = Uuid::nil();
        let err = ensure_owner_not_self_removal(true, user_id, user_id).unwrap_err();
        assert!(matches!(
            err,
            AppError::BadRequest("Owner cannot remove themselves")
        ));
    }

    #[test]
    fn self_role_change_validation_is_bad_request() {
        let user_id = Uuid::nil();
        let err = ensure_not_self_role_change(user_id, user_id).unwrap_err();
        assert!(matches!(
            err,
            AppError::BadRequest("Cannot change own role")
        ));
    }

    /// A fully-populated `Session`. Every field is listed explicitly on purpose:
    /// because `SessionWithRole` flattens the whole model onto the wire, a new
    /// DB column silently becomes a new `/api/sessions` field, so a new column
    /// must break this fixture's compile and force a wire-shape review (#1456).
    fn sample_session(launcher_version: Option<&str>) -> Session {
        let ts = chrono::NaiveDate::from_ymd_opt(2026, 7, 29)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        Session {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            session_name: "session".to_string(),
            session_key: "key".to_string(),
            working_directory: "/repo".to_string(),
            status: SessionStatus::Active.as_str().to_string(),
            last_activity: ts,
            created_at: ts,
            updated_at: ts,
            git_branch: None,
            total_cost_usd: 0.0,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            client_version: None,
            input_seq: 0,
            hostname: "host".to_string(),
            launcher_id: None,
            pr_url: None,
            agent_type: "claude".to_string(),
            repo_url: None,
            scheduled_task_id: None,
            paused: false,
            claude_args: serde_json::Value::Array(vec![]),
            launch_failure_count: 0,
            last_launch_attempt_at: None,
            launch_lease_until: None,
            open_prs: serde_json::Value::Array(vec![]),
            archived_at: None,
            last_model: None,
            launcher_version: launcher_version.map(str::to_string),
            forked_from_session_id: None,
            fork_point_turn_id: None,
            fork_launch_pending: false,
            fork_create_worktree: false,
            last_messaged_at: "2026-08-29T00:00:00".parse().unwrap(),
        }
    }

    /// Regression guard for #1454/#1456. `/api/sessions` flattens the whole
    /// `Session` model onto the wire, and the frontend rejects duplicate JSON
    /// keys. A duplicate `launcher_version` (a DB column colliding with the
    /// explicit live-value field) emptied the entire session list in prod — but
    /// only when a launcher was connected (`launcher_version = Some`), the one
    /// case the test suite never exercised. Round-trip the launcher-connected
    /// shape through the real frontend type so any such collision fails CI
    /// rather than a user's browser.
    #[test]
    fn sessions_response_roundtrips_with_launcher_version_present() {
        use shared::api::SessionsResponse;

        let response = SessionListResponse {
            sessions: vec![SessionWithRole {
                session: sample_session(Some("2.13.1000")),
                my_role: SessionRole::Owner,
            }],
        };

        let json = serde_json::to_string(&response).expect("serialize");
        let parsed: SessionsResponse =
            serde_json::from_str(&json).expect("frontend must parse /api/sessions cleanly");

        assert_eq!(parsed.sessions.len(), 1);
        let info = &parsed.sessions[0];
        assert_eq!(info.launcher_version.as_deref(), Some("2.13.1000"));
        assert!(matches!(info.status, SessionStatus::Active));
        assert!(matches!(info.my_role, SessionRole::Owner));
    }
}
