use axum::{
    extract::{Path, State},
    http::StatusCode,
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
    models::{Message, NewSessionMember, Session, SessionMember},
    AppState,
};

use shared::protocol::SESSION_COOKIE_NAME;

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
) -> Result<Json<SessionListResponse>, StatusCode> {
    // Extract user_id from session cookie
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    use crate::schema::{session_members, sessions};

    // Get all sessions the user is a member of, including their role
    let results: Vec<(Session, String)> = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(session_members::user_id.eq(current_user_id))
        .filter(sessions::status.ne("replaced"))
        .select((Session::as_select(), session_members::role))
        .order(sessions::last_activity.desc())
        .load(&mut conn)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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

/// Extract user_id from signed session cookie
fn extract_user_id(app_state: &AppState, cookies: &Cookies) -> Result<Uuid, StatusCode> {
    // In dev mode, allow unauthenticated access with test user
    if app_state.dev_mode {
        let mut conn = app_state
            .db_pool
            .get()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        use crate::schema::users;
        return users::table
            .filter(users::email.eq("testing@testing.local"))
            .select(users::id)
            .first::<Uuid>(&mut conn)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
    }

    // Extract from signed cookie
    let cookie = cookies
        .signed(&app_state.cookie_key)
        .get(SESSION_COOKIE_NAME)
        .ok_or(StatusCode::UNAUTHORIZED)?;

    cookie.value().parse().map_err(|_| StatusCode::UNAUTHORIZED)
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
) -> Result<Json<SessionDetailResponse>, StatusCode> {
    // Require authentication
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    use crate::schema::{messages, session_members, sessions};

    // Only return session if user is a member (owner, editor, or viewer)
    let session = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .select(Session::as_select())
        .first::<Session>(&mut conn)
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let recent_messages = messages::table
        .filter(messages::session_id.eq(session_id))
        .order(messages::created_at.desc())
        .limit(50)
        .load::<Message>(&mut conn)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(SessionDetailResponse {
        session,
        recent_messages,
    }))
}

pub async fn delete_session(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(session_id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    use crate::schema::{session_members, sessions};

    // Only owners can delete sessions - verify user is an owner
    let session = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .filter(session_members::role.eq("owner"))
        .select(Session::as_select())
        .first::<Session>(&mut conn)
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Delete session and all associated data, recording costs
    super::helpers::delete_session_with_data(&mut conn, &session, true)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn stop_session(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(session_id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    use crate::schema::{session_members, sessions};

    // Verify user has access to this session
    sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .select(Session::as_select())
        .first::<Session>(&mut conn)
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // The proxy always connects directly, so disconnect_session() is the
    // primary kill path. Also tell the launcher to clean up the process.
    let stopped = app_state.session_manager.disconnect_session(session_id);
    app_state
        .session_manager
        .stop_session_on_launcher(session_id);

    if stopped {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(StatusCode::NOT_FOUND)
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
) -> Result<Json<SessionMembersResponse>, StatusCode> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    use crate::schema::{session_members, users};

    // Verify the current user is a member of this session
    let _membership = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .first::<SessionMember>(&mut conn)
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Get all members with user info
    let members: Vec<(SessionMember, UserBasicInfo)> = session_members::table
        .inner_join(users::table.on(users::id.eq(session_members::user_id)))
        .filter(session_members::session_id.eq(session_id))
        .select((
            SessionMember::as_select(),
            (users::id, users::email, users::name),
        ))
        .load(&mut conn)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

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
) -> Result<StatusCode, StatusCode> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    // Validate role
    if req.role != "editor" && req.role != "viewer" {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    use crate::schema::{session_members, users};

    // Verify the current user is the owner
    let _owner_membership = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .filter(session_members::role.eq("owner"))
        .first::<SessionMember>(&mut conn)
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::FORBIDDEN)?;

    // Find the user by email
    let target_user_id: Uuid = users::table
        .filter(users::email.eq(&req.email))
        .select(users::id)
        .first(&mut conn)
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    // Check if user is already a member
    let existing = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(target_user_id))
        .first::<SessionMember>(&mut conn)
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if existing.is_some() {
        return Err(StatusCode::CONFLICT);
    }

    // Add the member
    let new_member = NewSessionMember {
        session_id,
        user_id: target_user_id,
        role: req.role,
    };

    diesel::insert_into(session_members::table)
        .values(&new_member)
        .execute(&mut conn)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(StatusCode::CREATED)
}

/// Remove a member from a session
/// Owner can remove anyone; non-owner can only remove themselves (leave)
pub async fn remove_session_member(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path((session_id, target_user_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, StatusCode> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    use crate::schema::session_members;

    // Get the current user's membership
    let current_membership = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .first::<SessionMember>(&mut conn)
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;

    let is_owner = current_membership.role == "owner";

    // Non-owners can only remove themselves
    if !is_owner && current_user_id != target_user_id {
        return Err(StatusCode::FORBIDDEN);
    }

    // Owners cannot remove themselves (would leave session ownerless)
    if is_owner && current_user_id == target_user_id {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Remove the member
    let deleted = diesel::delete(
        session_members::table
            .filter(session_members::session_id.eq(session_id))
            .filter(session_members::user_id.eq(target_user_id)),
    )
    .execute(&mut conn)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if deleted == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(StatusCode::NO_CONTENT)
}

/// Update a member's role (owner only)
pub async fn update_session_member_role(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path((session_id, target_user_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<UpdateMemberRoleRequest>,
) -> Result<StatusCode, StatusCode> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    // Validate role
    if req.role != "editor" && req.role != "viewer" {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    use crate::schema::session_members;

    // Verify the current user is the owner
    let _owner_membership = session_members::table
        .filter(session_members::session_id.eq(session_id))
        .filter(session_members::user_id.eq(current_user_id))
        .filter(session_members::role.eq("owner"))
        .first::<SessionMember>(&mut conn)
        .optional()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::FORBIDDEN)?;

    // Cannot change own role
    if current_user_id == target_user_id {
        return Err(StatusCode::BAD_REQUEST);
    }

    // Update the role
    let updated = diesel::update(
        session_members::table
            .filter(session_members::session_id.eq(session_id))
            .filter(session_members::user_id.eq(target_user_id)),
    )
    .set(session_members::role.eq(&req.role))
    .execute(&mut conn)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if updated == 0 {
        return Err(StatusCode::NOT_FOUND);
    }

    Ok(StatusCode::OK)
}
