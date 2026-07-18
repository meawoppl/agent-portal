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
use shared::SessionStatus;
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
/// `my_role` string — this is the on-wire shape consumed by the frontend.
/// The frontend deserializes the same bytes into `shared::api::SessionsResponse`
/// (whose `sessions` field is `Vec<shared::SessionInfo>`); `SessionInfo`
/// silently drops the per-row stats fields it doesn't care about
/// (`total_cost_usd`, `input_tokens`, `input_seq`, etc.). Don't shrink this
/// struct without also auditing every other consumer of the wire shape.
#[derive(Debug, Serialize)]
pub struct SessionWithRole {
    #[serde(flatten)]
    pub session: Session,
    pub my_role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub launcher_version: Option<String>,
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
        .order(sessions::last_activity.desc())
        .load(&mut conn)?;

    let sessions_with_role = results
        .into_iter()
        .map(|(session, role)| SessionWithRole {
            launcher_version: app_state
                .session_manager
                .launcher_version(session.launcher_id),
            session,
            my_role: role,
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

    let stopped = app_state.session_manager.disconnect_session(session_id);
    let launcher_stopped = app_state.session_manager.stop_session_on_launcher(
        session_id,
        session.launcher_id,
        Some(session.working_directory.clone()),
    );

    if stopped || launcher_stopped {
        use crate::schema::sessions;
        diesel::update(sessions::table.find(session_id))
            .set((
                sessions::paused.eq(false),
                sessions::status.eq(SessionStatus::Disconnected.as_str()),
                sessions::updated_at.eq(diesel::dsl::now),
            ))
            .execute(&mut conn)?;
        Ok(EmptyResponse::ACCEPTED)
    } else if session.paused {
        Err(AppError::NotFound("Launcher not connected"))
    } else {
        Err(AppError::NotFound("Session not connected"))
    }
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
}

/// Validate a member role supplied by the caller
fn validate_member_role(role: &str) -> Result<(), AppError> {
    if role != "editor" && role != "viewer" {
        return Err(AppError::BadRequest("Invalid role"));
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
            (users::id, users::email, users::name),
        ))
        .load(&mut conn)?;

    let member_infos = members
        .into_iter()
        .map(|(member, user)| SessionMemberInfo {
            user_id: user.id,
            email: user.email,
            name: user.name,
            role: member.role,
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
    validate_member_role(&req.role)?;

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

    if existing.is_some() {
        return Err(AppError::BadRequest("User is already a member"));
    }

    let new_member = NewSessionMember {
        session_id,
        user_id: target_user_id,
        role: req.role,
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

    let is_owner = current_membership.role == "owner";

    if !is_owner && current_user_id != target_user_id {
        return Err(AppError::Forbidden);
    }

    if is_owner && current_user_id == target_user_id {
        return Err(AppError::BadRequest("Owner cannot remove themselves"));
    }

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
    validate_member_role(&req.role)?;

    let mut conn = app_state.conn()?;

    use crate::schema::session_members;

    crate::handlers::session_access::verify_owner_membership(
        &mut conn,
        session_id,
        current_user_id,
    )?;

    if current_user_id == target_user_id {
        return Err(AppError::BadRequest("Cannot change own role"));
    }

    let updated = diesel::update(
        session_members::table
            .filter(session_members::session_id.eq(session_id))
            .filter(session_members::user_id.eq(target_user_id)),
    )
    .set(session_members::role.eq(&req.role))
    .execute(&mut conn)?;

    if updated == 0 {
        return Err(AppError::NotFound("Member not found"));
    }

    Ok(EmptyResponse::OK)
}
