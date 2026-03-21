use axum::{
    extract::{Path, State},
    Json,
};
use chrono::NaiveDateTime;
use diesel::prelude::*;
use serde::Serialize;
use shared::api::{AddMemberRequest, UpdateMemberRoleRequest};
use std::sync::Arc;
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::{
    auth::extract_user_id,
    errors::AppError,
    models::{Message, NewSessionMember, Session, SessionMember},
    AppState,
};

/// Session with the current user's role included
#[derive(Debug, Serialize)]
pub struct SessionWithRole {
    #[serde(flatten)]
    pub session: Session,
    pub my_role: String,
}

#[derive(Debug, Serialize)]
pub struct SessionListResponse {
    pub sessions: Vec<SessionWithRole>,
}

pub async fn list_sessions(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<SessionListResponse>, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::{session_members, sessions};

    let results: Vec<(Session, String)> = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(session_members::user_id.eq(current_user_id))
        .filter(sessions::status.ne("replaced"))
        .select((Session::as_select(), session_members::role))
        .order(sessions::last_activity.desc())
        .load(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    let sessions_with_role = results
        .into_iter()
        .map(|(session, role)| SessionWithRole {
            session,
            my_role: role,
        })
        .collect();

    Ok(Json(SessionListResponse {
        sessions: sessions_with_role,
    }))
}

#[derive(Debug, Serialize)]
pub struct SessionDetailResponse {
    pub session: Session,
    pub recent_messages: Vec<Message>,
}

pub async fn get_session(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(session_id): Path<Uuid>,
) -> Result<Json<SessionDetailResponse>, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::{messages, session_members, sessions};

    let session = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .select(Session::as_select())
        .first::<Session>(&mut conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?
        .ok_or(AppError::NotFound("Session not found"))?;

    let recent_messages = messages::table
        .filter(messages::session_id.eq(session_id))
        .order(messages::created_at.desc())
        .limit(50)
        .load::<Message>(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    Ok(Json(SessionDetailResponse {
        session,
        recent_messages,
    }))
}

pub async fn delete_session(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(session_id): Path<Uuid>,
) -> Result<axum::http::StatusCode, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::{session_members, sessions};

    let session = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .filter(session_members::role.eq("owner"))
        .select(Session::as_select())
        .first::<Session>(&mut conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?
        .ok_or(AppError::NotFound("Session not found"))?;

    super::helpers::delete_session_with_data(&mut conn, &session, true)
        .map_err(|e| AppError::Internal(format!("{:?}", e)))?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn stop_session(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(session_id): Path<Uuid>,
) -> Result<axum::http::StatusCode, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::{session_members, sessions};

    sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .select(Session::as_select())
        .first::<Session>(&mut conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?
        .ok_or(AppError::NotFound("Session not found"))?;

    let stopped = app_state.session_manager.disconnect_session(session_id);
    app_state
        .session_manager
        .stop_session_on_launcher(session_id);

    if stopped {
        Ok(axum::http::StatusCode::ACCEPTED)
    } else {
        Err(AppError::NotFound("Session not connected"))
    }
}

// ============================================================================
// Session Member Management
// ============================================================================

#[derive(Debug, Serialize)]
pub struct SessionMemberInfo {
    pub user_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Serialize)]
pub struct SessionMembersResponse {
    pub members: Vec<SessionMemberInfo>,
}

/// User info selected from joined query
#[derive(Debug, Queryable)]
struct UserBasicInfo {
    id: Uuid,
    email: String,
    name: Option<String>,
}

/// List all members of a session
pub async fn list_session_members(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(session_id): Path<Uuid>,
) -> Result<Json<SessionMembersResponse>, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::{session_members, users};

    session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .first::<SessionMember>(&mut conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?
        .ok_or(AppError::NotFound("Session not found"))?;

    let members: Vec<(SessionMember, UserBasicInfo)> = session_members::table
        .inner_join(users::table.on(users::id.eq(session_members::user_id)))
        .filter(session_members::session_id.eq(session_id))
        .select((
            SessionMember::as_select(),
            (users::id, users::email, users::name),
        ))
        .load(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

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
    cookies: Cookies,
    Path(session_id): Path<Uuid>,
    Json(req): Json<AddMemberRequest>,
) -> Result<axum::http::StatusCode, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    if req.role != "editor" && req.role != "viewer" {
        return Err(AppError::Internal("Invalid role".to_string()));
    }

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::{session_members, users};

    session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .filter(session_members::role.eq("owner"))
        .first::<SessionMember>(&mut conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?
        .ok_or(AppError::Forbidden)?;

    let target_user_id: Uuid = users::table
        .filter(users::email.eq(&req.email))
        .select(users::id)
        .first(&mut conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?
        .ok_or(AppError::NotFound("User not found"))?;

    let existing = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(target_user_id))
        .first::<SessionMember>(&mut conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    if existing.is_some() {
        return Err(AppError::Internal("User is already a member".to_string()));
    }

    let new_member = NewSessionMember {
        session_id,
        user_id: target_user_id,
        role: req.role,
    };

    diesel::insert_into(session_members::table)
        .values(&new_member)
        .execute(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    Ok(axum::http::StatusCode::CREATED)
}

/// Remove a member from a session
/// Owner can remove anyone; non-owner can only remove themselves (leave)
pub async fn remove_session_member(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path((session_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<axum::http::StatusCode, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::session_members;

    let current_membership = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .first::<SessionMember>(&mut conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?
        .ok_or(AppError::NotFound("Session not found"))?;

    let is_owner = current_membership.role == "owner";

    if !is_owner && current_user_id != target_user_id {
        return Err(AppError::Forbidden);
    }

    if is_owner && current_user_id == target_user_id {
        return Err(AppError::Internal(
            "Owner cannot remove themselves".to_string(),
        ));
    }

    let deleted = diesel::delete(
        session_members::table
            .filter(session_members::session_id.eq(session_id))
            .filter(session_members::user_id.eq(target_user_id)),
    )
    .execute(&mut conn)
    .map_err(|e| AppError::DbQuery(e.to_string()))?;

    if deleted == 0 {
        return Err(AppError::NotFound("Member not found"));
    }

    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Update a member's role (owner only)
pub async fn update_session_member_role(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path((session_id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateMemberRoleRequest>,
) -> Result<axum::http::StatusCode, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    if req.role != "editor" && req.role != "viewer" {
        return Err(AppError::Internal("Invalid role".to_string()));
    }

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::session_members;

    session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .filter(session_members::role.eq("owner"))
        .first::<SessionMember>(&mut conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?
        .ok_or(AppError::Forbidden)?;

    if current_user_id == target_user_id {
        return Err(AppError::Internal("Cannot change own role".to_string()));
    }

    let updated = diesel::update(
        session_members::table
            .filter(session_members::session_id.eq(session_id))
            .filter(session_members::user_id.eq(target_user_id)),
    )
    .set(session_members::role.eq(&req.role))
    .execute(&mut conn)
    .map_err(|e| AppError::DbQuery(e.to_string()))?;

    if updated == 0 {
        return Err(AppError::NotFound("Member not found"));
    }

    Ok(axum::http::StatusCode::OK)
}
