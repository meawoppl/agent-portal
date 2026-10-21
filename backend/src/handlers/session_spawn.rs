//! Policy-gated creation of agent children. Approval is browser-only and every
//! request reserves its slot under the owner's row lock before any dispatch.
use crate::schema::{
    session_spawn_policies as policies, session_spawn_requests as requests, sessions, users,
};
use crate::{
    auth::{BrowserUserId, CurrentUserId},
    errors::AppError,
    models::Session,
    AppState,
};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::NaiveDateTime;
use diesel::prelude::*;
use shared::api::*;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug)]
pub struct SpawnFailure {
    status: StatusCode,
    code: &'static str,
    message: String,
}
impl SpawnFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
        }
    }
}
impl From<AppError> for SpawnFailure {
    fn from(e: AppError) -> Self {
        let status = e.into_response().status();
        Self {
            status,
            code: if status == StatusCode::FORBIDDEN {
                "forbidden"
            } else if status == StatusCode::UNAUTHORIZED {
                "unauthorized"
            } else {
                "request_failed"
            },
            message: "Spawn request could not be completed".into(),
        }
    }
}
impl From<diesel::result::Error> for SpawnFailure {
    fn from(e: diesel::result::Error) -> Self {
        AppError::from(e).into()
    }
}
impl IntoResponse for SpawnFailure {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(SpawnErrorResponse {
                code: self.code.into(),
                message: self.message,
                request_id: None,
            }),
        )
            .into_response()
    }
}

#[derive(Queryable, Selectable)]
#[diesel(table_name = policies)]
struct PolicyRow {
    approval_mode: String,
    max_children: i32,
    max_depth: i32,
    idle_timeout_seconds: i32,
    stop_on_parent_end: bool,
    first_approved_at: Option<NaiveDateTime>,
}
#[derive(Queryable, Selectable)]
#[diesel(table_name = requests)]
struct RequestRow {
    id: Uuid,
    user_id: Uuid,
    parent_session_id: Uuid,
    launcher_id: Uuid,
    request: serde_json::Value,
    state: String,
    child_session_id: Option<Uuid>,
    approved_by: Option<Uuid>,
    created_at: NaiveDateTime,
    error_code: Option<String>,
}
impl RequestRow {
    fn info(&self) -> Result<SpawnRequestInfo, SpawnFailure> {
        let request = serde_json::from_value(self.request.clone())
            .map_err(|_| SpawnFailure::new("invalid_record", "Stored launch request is invalid"))?;
        let state = match self.state.as_str() {
            "pending" => SpawnRequestState::Pending,
            "launching" => SpawnRequestState::Launching,
            "launched" => SpawnRequestState::Launched,
            "denied" => SpawnRequestState::Denied,
            "failed" => SpawnRequestState::Failed,
            _ => {
                return Err(SpawnFailure::new(
                    "invalid_record",
                    "Stored request state is invalid",
                ))
            }
        };
        Ok(SpawnRequestInfo {
            request_id: self.id,
            parent_session_id: self.parent_session_id,
            launcher_id: self.launcher_id,
            request,
            state,
            child_session_id: self.child_session_id,
            approved_by: self.approved_by,
            created_at: self.created_at.and_utc().to_rfc3339(),
            error_code: self.error_code.clone(),
        })
    }
}
fn owned(conn: &mut PgConnection, user: Uuid, parent: Uuid) -> Result<Session, SpawnFailure> {
    sessions::table
        .find(parent)
        .filter(sessions::user_id.eq(user))
        .select(Session::as_select())
        .first(conn)
        .optional()?
        .ok_or_else(|| AppError::Forbidden.into())
}
fn agent_parent(
    state: &AppState,
    headers: &HeaderMap,
    parent: Uuid,
) -> Result<Session, SpawnFailure> {
    let token = crate::auth::bearer_token(headers).ok_or(AppError::Unauthorized)?;
    let mut conn = state.conn()?;
    let verified = super::proxy_tokens::verify_and_get_user_with_token(state, &mut conn, token)?;
    if verified.token.name != super::proxy_tokens::LAUNCH_TOKEN_NAME
        || verified.token.session_id != Some(parent)
    {
        return Err(SpawnFailure {
            status: StatusCode::FORBIDDEN,
            code: "session_credential_required",
            message: "Use the session-bound credentials supplied by a current launcher".into(),
        });
    }
    owned(&mut conn, verified.user_id, parent)
}
fn policy(conn: &mut PgConnection, parent: Uuid) -> Result<SpawnPolicyResponse, SpawnFailure> {
    let row = policies::table
        .find(parent)
        .select(PolicyRow::as_select())
        .first(conn)
        .optional()?;
    let Some(row) = row else {
        return Ok(SpawnPolicyResponse {
            policy: SpawnPolicy::default(),
            first_spawn_approved: false,
        });
    };
    let approval_mode = match row.approval_mode.as_str() {
        "auto" => SpawnApprovalMode::Auto,
        "off" => SpawnApprovalMode::Off,
        "first_spawn" => SpawnApprovalMode::FirstSpawn,
        _ => {
            return Err(SpawnFailure::new(
                "invalid_policy",
                "Stored spawn policy is invalid",
            ))
        }
    };
    Ok(SpawnPolicyResponse {
        policy: SpawnPolicy {
            approval_mode,
            max_children: row.max_children as u32,
            max_depth: row.max_depth as u32,
            idle_timeout_seconds: row.idle_timeout_seconds as u32,
            stop_on_parent_end: row.stop_on_parent_end,
        },
        first_spawn_approved: row.first_approved_at.is_some(),
    })
}
fn mode(p: SpawnApprovalMode) -> &'static str {
    match p {
        SpawnApprovalMode::FirstSpawn => "first_spawn",
        SpawnApprovalMode::Auto => "auto",
        SpawnApprovalMode::Off => "off",
    }
}
fn validate_policy(p: &SpawnPolicy) -> Result<(), SpawnFailure> {
    if !(1..=32).contains(&p.max_children)
        || !(1..=8).contains(&p.max_depth)
        || !(60..=604800).contains(&p.idle_timeout_seconds)
    {
        return Err(SpawnFailure::new(
            "invalid_policy",
            "Children must be 1..32, depth 1..8, idle timeout 60..604800 seconds",
        ));
    }
    Ok(())
}
fn save_policy(conn: &mut PgConnection, parent: Uuid, p: &SpawnPolicy) -> Result<(), SpawnFailure> {
    validate_policy(p)?;
    let values = (
        policies::approval_mode.eq(mode(p.approval_mode)),
        policies::max_children.eq(p.max_children as i32),
        policies::max_depth.eq(p.max_depth as i32),
        policies::idle_timeout_seconds.eq(p.idle_timeout_seconds as i32),
        policies::stop_on_parent_end.eq(p.stop_on_parent_end),
    );
    diesel::insert_into(policies::table)
        .values((policies::session_id.eq(parent), values))
        .on_conflict(policies::session_id)
        .do_update()
        .set(values)
        .execute(conn)?;
    Ok(())
}
fn lock_owner(conn: &mut PgConnection, user: Uuid) -> Result<(), SpawnFailure> {
    users::table
        .find(user)
        .select(users::id)
        .for_update()
        .first::<Uuid>(conn)?;
    Ok(())
}
fn validate_launch(
    state: &AppState,
    parent: &Session,
    launcher: Uuid,
    p: &SpawnPolicy,
) -> Result<(), SpawnFailure> {
    if parent.paused {
        return Err(SpawnFailure::new(
            "parent_stopped",
            "Resume the parent before spawning",
        ));
    }
    if p.approval_mode == SpawnApprovalMode::Off {
        return Err(SpawnFailure::new("spawn_off", "Child spawning is disabled"));
    }
    if parent.spawn_depth >= p.max_depth as i32 {
        return Err(SpawnFailure::new(
            "depth_limit",
            format!("Maximum depth is {} (root depth is 1)", p.max_depth),
        ));
    }
    super::launcher_rpc::require_launcher_owner(state, launcher, parent.user_id)?;
    if !state
        .session_manager
        .launcher_supports_capability(launcher, shared::LAUNCHER_CAPABILITY_CLI_SPAWN)
    {
        return Err(SpawnFailure::new(
            "unsupported_launcher",
            "Update this launcher: launcher.cli_spawn is required",
        ));
    }
    Ok(())
}

pub async fn get_policy(
    State(state): State<Arc<AppState>>,
    BrowserUserId(user): BrowserUserId,
    Path(parent): Path<Uuid>,
) -> Result<Json<SpawnPolicyResponse>, SpawnFailure> {
    let mut conn = state.conn()?;
    owned(&mut conn, user, parent)?;
    Ok(Json(policy(&mut conn, parent)?))
}
pub async fn set_policy(
    State(state): State<Arc<AppState>>,
    BrowserUserId(user): BrowserUserId,
    Path(parent): Path<Uuid>,
    Json(p): Json<SpawnPolicy>,
) -> Result<Json<SpawnPolicyResponse>, SpawnFailure> {
    let mut conn = state.conn()?;
    conn.transaction::<_, SpawnFailure, _>(|conn| {
        lock_owner(conn, user)?;
        owned(conn, user, parent)?;
        save_policy(conn, parent, &p)
    })?;
    tracing::info!(%user, %parent, policy = ?p, "Agent spawn policy changed by browser owner");
    Ok(Json(policy(&mut conn, parent)?))
}
pub async fn list_requests(
    State(state): State<Arc<AppState>>,
    BrowserUserId(user): BrowserUserId,
    Path(parent): Path<Uuid>,
) -> Result<Json<SpawnRequestsResponse>, SpawnFailure> {
    let mut conn = state.conn()?;
    owned(&mut conn, user, parent)?;
    let rows = requests::table
        .filter(requests::parent_session_id.eq(parent))
        .order(requests::created_at.desc())
        .limit(100)
        .select(RequestRow::as_select())
        .load::<RequestRow>(&mut conn)?;
    Ok(Json(SpawnRequestsResponse {
        requests: rows
            .iter()
            .map(RequestRow::info)
            .collect::<Result<_, _>>()?,
    }))
}

pub async fn start(
    State(state): State<Arc<AppState>>,
    Path(parent_id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<SpawnSessionRequest>,
) -> Result<Json<SpawnRequestInfo>, SpawnFailure> {
    let parent = agent_parent(&state, &headers, parent_id)?;
    if req.launch.agent_type == shared::AgentType::Antigravity {
        return Err(SpawnFailure::new(
            "unsupported_agent",
            "Antigravity does not yet propagate session credentials to tools",
        ));
    }
    if req.prompt.trim().is_empty()
        || req.prompt.len() > 200_000
        || req.launch.working_directory.trim().is_empty()
    {
        return Err(SpawnFailure::new(
            "invalid_request",
            "A directory and nonempty prompt (at most 200000 bytes) are required",
        ));
    }
    let launcher = req.launcher_id.or(parent.launcher_id).ok_or_else(|| {
        SpawnFailure::new(
            "launcher_required",
            "Parent has no launcher; specify a launcher id",
        )
    })?;
    let value = serde_json::to_value(&req).map_err(|e| AppError::Internal(e.to_string()))?;
    let mut conn = state.conn()?;
    let (row, note) = conn.transaction::<_, SpawnFailure, _>(|conn| {
        lock_owner(conn, parent.user_id)?;
        if let Some(existing) = requests::table
            .find(req.request_id)
            .select(RequestRow::as_select())
            .first::<RequestRow>(conn)
            .optional()?
        {
            if existing.user_id != parent.user_id
                || existing.parent_session_id != parent_id
                || existing.request != value
            {
                return Err(SpawnFailure::new(
                    "request_conflict",
                    "Request id already belongs to a different request",
                ));
            }
            return Ok((existing, None));
        }
        let parent = owned(conn, parent.user_id, parent_id)?;
        let p = policy(conn, parent_id)?;
        validate_launch(&state, &parent, launcher, &p.policy)?;
        // Lifetime child cap includes pending reservations. Stopping a child
        // cannot recycle its slot and silently evade the owner's approval.
        let count: i64 = requests::table
            .filter(requests::parent_session_id.eq(parent_id))
            .filter(requests::state.ne_all(["denied", "failed"]))
            .count()
            .get_result(conn)?;
        if count >= p.policy.max_children as i64 {
            return Err(SpawnFailure::new(
                "child_limit",
                format!("Maximum children is {}", p.policy.max_children),
            ));
        }
        let since = chrono::Utc::now().naive_utc() - chrono::Duration::minutes(1);
        let recent: i64 = requests::table
            .filter(requests::user_id.eq(parent.user_id))
            .filter(requests::created_at.gt(since))
            .count()
            .get_result(conn)?;
        if recent >= 12 {
            return Err(SpawnFailure::new(
                "rate_limit",
                "Maximum 12 spawn requests per user per minute",
            ));
        }
        let initial =
            if p.policy.approval_mode == SpawnApprovalMode::FirstSpawn && !p.first_spawn_approved {
                "pending"
            } else {
                "launching"
            };
        diesel::insert_into(requests::table)
            .values((
                requests::id.eq(req.request_id),
                requests::user_id.eq(parent.user_id),
                requests::parent_session_id.eq(parent_id),
                requests::launcher_id.eq(launcher),
                requests::request.eq(&value),
                requests::state.eq(initial),
            ))
            .execute(conn)?;
        let note = if initial == "pending" {
            Some(persist_notice(conn, &parent, format!("Child session request {} needs approval. Open **Children** in this session's header to review the host, directory, prompt, and launch settings.", req.request_id))?)
        } else { None };
        let row = requests::table.find(req.request_id).select(RequestRow::as_select()).first(conn)?;
        Ok((row, note))
    })?;
    if let Some(note) = note {
        broadcast_notice(&state, &parent, note)?;
    }
    tracing::info!(user=%parent.user_id, parent=%parent_id, request=%req.request_id, %launcher, agent=?req.launch.agent_type, directory=%req.launch.working_directory, state=%row.state, "Agent spawn requested");
    drop(conn);
    dispatch(state, row).map(Json)
}

pub async fn approve(
    State(state): State<Arc<AppState>>,
    BrowserUserId(user): BrowserUserId,
    Path((parent_id, request_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<SpawnRequestInfo>, SpawnFailure> {
    let mut conn = state.conn()?;
    let row = conn.transaction::<_, SpawnFailure, _>(|conn| {
        lock_owner(conn, user)?;
        let parent = owned(conn, user, parent_id)?;
        let row = requests::table
            .find(request_id)
            .filter(requests::parent_session_id.eq(parent_id))
            .select(RequestRow::as_select())
            .first::<RequestRow>(conn)?;
        if row.state != "pending" {
            return Ok(row);
        }
        let p = policy(conn, parent_id)?;
        validate_launch(&state, &parent, row.launcher_id, &p.policy)?;
        let count: i64 = requests::table
            .filter(requests::parent_session_id.eq(parent_id))
            .filter(requests::state.ne_all(["denied", "failed"]))
            .count()
            .get_result(conn)?;
        if count > p.policy.max_children as i64 {
            return Err(SpawnFailure::new(
                "child_limit",
                "Policy child limit was reduced; deny excess requests first",
            ));
        }
        save_policy(conn, parent_id, &p.policy)?;
        diesel::update(policies::table.find(parent_id))
            .set(policies::first_approved_at.eq(diesel::dsl::now))
            .execute(conn)?;
        diesel::update(requests::table.find(request_id))
            .set((
                requests::state.eq("launching"),
                requests::approved_by.eq(user),
                requests::updated_at.eq(diesel::dsl::now),
            ))
            .execute(conn)?;
        requests::table
            .find(request_id)
            .select(RequestRow::as_select())
            .first(conn)
            .map_err(Into::into)
    })?;
    tracing::info!(%user, parent=%parent_id, request=%request_id, "Agent spawn approved by browser owner");
    drop(conn);
    dispatch(state, row).map(Json)
}
pub async fn deny(
    State(state): State<Arc<AppState>>,
    BrowserUserId(user): BrowserUserId,
    Path((parent, request)): Path<(Uuid, Uuid)>,
) -> Result<Json<SpawnRequestInfo>, SpawnFailure> {
    let mut conn = state.conn()?;
    conn.transaction::<_, SpawnFailure, _>(|conn| {
        lock_owner(conn, user)?;
        owned(conn, user, parent)?;
        diesel::update(
            requests::table
                .find(request)
                .filter(requests::parent_session_id.eq(parent))
                .filter(requests::state.eq("pending")),
        )
        .set((
            requests::state.eq("denied"),
            requests::approved_by.eq(user),
            requests::updated_at.eq(diesel::dsl::now),
        ))
        .execute(conn)?;
        let row = requests::table
            .find(request)
            .filter(requests::parent_session_id.eq(parent))
            .select(RequestRow::as_select())
            .first::<RequestRow>(conn)?;
        tracing::info!(%user, %parent, %request, state=%row.state, "Agent spawn denial evaluated");
        row.info().map(Json)
    })
}

/// Prepared child context passed through the same launch implementation as
/// the dialog, so worktree names/modes and launcher token handling stay equal.
pub(crate) struct ChildLaunch {
    pub request_id: Uuid,
    pub session_id: Uuid,
    pub parent: Session,
    pub policy: SpawnPolicy,
    pub prompt: String,
    pub plugins: Option<std::collections::BTreeMap<String, bool>>,
}
fn dispatch(state: Arc<AppState>, row: RequestRow) -> Result<SpawnRequestInfo, SpawnFailure> {
    if row.state != "launching" || row.child_session_id.is_some() {
        return row.info();
    }
    let mut conn = state.conn()?;
    // Claim once; retries observe the reserved child rather than dispatching it twice.
    let (child_id, parent, p) = conn.transaction::<_, SpawnFailure, _>(|conn| {
        lock_owner(conn, row.user_id)?;
        let current = requests::table
            .find(row.id)
            .select(RequestRow::as_select())
            .first::<RequestRow>(conn)?;
        if current.state != "launching" {
            return Err(SpawnFailure::new(
                "request_not_launching",
                "Request is no longer approved for dispatch",
            ));
        }
        if current.child_session_id.is_some() {
            return Err(SpawnFailure::new(
                "launch_in_progress",
                "This request is already being dispatched",
            ));
        }
        let parent = owned(conn, row.user_id, row.parent_session_id)?;
        let p = policy(conn, row.parent_session_id)?.policy;
        validate_launch(&state, &parent, row.launcher_id, &p)?;
        let child_id = Uuid::new_v4();
        diesel::update(requests::table.find(row.id))
            .set((
                requests::child_session_id.eq(child_id),
                requests::updated_at.eq(diesel::dsl::now),
            ))
            .execute(conn)?;
        Ok((child_id, parent, p))
    })?;
    let info = row.info()?;
    let request = LaunchRequest {
        launch: info.request.launch,
        launcher_id: Some(row.launcher_id),
        name: None,
        create_worktree: false,
        plugin_selection: None,
    };
    let child = ChildLaunch {
        request_id: row.id,
        session_id: child_id,
        parent,
        policy: p,
        prompt: info.request.prompt,
        plugins: info.request.plugins,
    };
    drop(conn);
    if let Err(e) =
        super::launchers::launch_owned_session(&state, row.user_id, request, Some(child))
    {
        let mut conn = state.conn()?;
        diesel::update(requests::table.find(row.id))
            .set((
                requests::state.eq("failed"),
                requests::error_code.eq("launch_failed"),
                requests::updated_at.eq(diesel::dsl::now),
            ))
            .execute(&mut conn)?;
        tracing::warn!(request=%row.id, ?e, "Agent spawn dispatch failed");
    }
    let mut conn = state.conn()?;
    requests::table
        .find(row.id)
        .select(RequestRow::as_select())
        .first::<RequestRow>(&mut conn)?
        .info()
}

pub(crate) fn persist_child(
    conn: &mut PgConnection,
    child: &ChildLaunch,
) -> Result<crate::models::Message, AppError> {
    use crate::schema::pending_inputs;
    // The expiry sweep takes this same request lock. Either preparation wins
    // and commits a recoverable child, or the expired claim rejects preparation
    // and the outer transaction rolls the child row back.
    let (state, id) = requests::table
        .find(child.request_id)
        .select((requests::state, requests::child_session_id))
        .for_update()
        .first::<(String, Option<Uuid>)>(conn)?;
    if state != "launching" || id != Some(child.session_id) {
        return Err(AppError::BadRequest(
            "Spawn claim expired before child preparation",
        ));
    }
    diesel::update(sessions::table.find(child.session_id))
        .set((
            sessions::spawned_by.eq(child.parent.id),
            sessions::spawn_depth.eq(child.parent.spawn_depth + 1),
            sessions::spawn_idle_timeout_seconds.eq(child.policy.idle_timeout_seconds as i32),
            sessions::spawn_stop_on_parent_end.eq(child.policy.stop_on_parent_end),
            sessions::input_seq.eq(1),
            sessions::launch_lease_until
                .eq(chrono::Utc::now().naive_utc() + chrono::Duration::seconds(60)),
        ))
        .execute(conn)?;
    let content = shared::PortalMessage::agent_message(
        child.parent.agent_type.clone(),
        child.parent.id.to_string(),
        format!(
            "Started by {} ({}).\n\n{}",
            child.parent.session_name, child.parent.id, child.prompt
        ),
    )
    .to_json();
    diesel::insert_into(pending_inputs::table)
        .values(crate::models::NewPendingInput {
            session_id: child.session_id,
            seq_num: 1,
            content: serde_json::to_string(&content)
                .map_err(|e| AppError::Internal(e.to_string()))?,
            send_mode: shared::SendMode::Normal.as_str().into(),
            client_msg_id: Some(child.request_id),
            reasoning_effort: None,
        })
        .execute(conn)?;
    diesel::update(requests::table.find(child.request_id))
        .set((
            requests::child_session_id.eq(child.session_id),
            requests::updated_at.eq(diesel::dsl::now),
        ))
        .execute(conn)?;
    persist_notice(
        conn,
        &child.parent,
        format!(
            "Launching child session [{}](/dashboard?session={})",
            child.session_id, child.session_id
        ),
    )
}

#[derive(serde::Serialize)]
pub struct ChildrenView {
    children: Vec<super::sessions::SessionWithRole>,
}
pub async fn children(
    State(state): State<Arc<AppState>>,
    Path(parent): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<ChildrenView>, SpawnFailure> {
    let parent = agent_parent(&state, &headers, parent)?;
    let mut conn = state.conn()?;
    let children = sessions::table
        .filter(sessions::spawned_by.eq(parent.id))
        .filter(sessions::user_id.eq(parent.user_id))
        .select(Session::as_select())
        .load::<Session>(&mut conn)?
        .into_iter()
        .map(|session| super::sessions::SessionWithRole {
            session,
            my_role: shared::SessionRole::Owner,
        })
        .collect();
    Ok(Json(ChildrenView { children }))
}
pub async fn stop_child(
    State(state): State<Arc<AppState>>,
    Path((parent, child)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> Result<super::responses::EmptyResponse, SpawnFailure> {
    let parent = agent_parent(&state, &headers, parent)?;
    let mut conn = state.conn()?;
    let child = owned(&mut conn, parent.user_id, child)?;
    if child.spawned_by != Some(parent.id) {
        return Err(SpawnFailure::new(
            "not_own_child",
            "Only direct children of this session can be stopped",
        ));
    }
    drop(conn);
    super::sessions::stop_session(
        State(state),
        CurrentUserId(parent.user_id),
        Path(child.id),
        headers,
    )
    .await
    .map_err(Into::into)
}

/// Registration, rather than process-spawn acknowledgement, proves that the
/// durable first-input recipe can now be resumed on subsequent reconnects.
pub(crate) fn registered(conn: &mut PgConnection, child: Uuid) -> QueryResult<()> {
    diesel::update(
        requests::table
            .filter(requests::child_session_id.eq(child))
            .filter(requests::state.eq("launching")),
    )
    .set((
        requests::state.eq("launched"),
        requests::updated_at.eq(diesel::dsl::now),
    ))
    .execute(conn)?;
    Ok(())
}

pub(crate) fn launch_failed(conn: &mut PgConnection, child: Uuid) -> QueryResult<()> {
    conn.transaction(|conn| {
        let affected = diesel::update(
            requests::table
                .filter(requests::child_session_id.eq(child))
                .filter(requests::state.eq("launching")),
        )
        .set((
            requests::state.eq("failed"),
            requests::error_code.eq("launcher_rejected"),
            requests::updated_at.eq(diesel::dsl::now),
        ))
        .execute(conn)?;
        if affected > 0 {
            crate::db::session_lifecycle::pause(conn, child)?;
        }
        Ok(())
    })
}

/// Pending inputs already have sequence numbers before the child exists in the
/// agent CLI. Preserve its original worktree/plugin recipe until registration.
pub(crate) fn launch_recipe(
    conn: &mut PgConnection,
    child: Uuid,
) -> Result<Option<(SpawnSessionRequest, bool)>, AppError> {
    let row = requests::table
        .filter(requests::child_session_id.eq(child))
        .select((requests::request, requests::state))
        .first::<(serde_json::Value, String)>(conn)
        .optional()?;
    row.map(|(value, state)| {
        serde_json::from_value(value)
            .map(|req| (req, state == "launching"))
            .map_err(|e| AppError::Internal(e.to_string()))
    })
    .transpose()
}

fn persist_notice(
    conn: &mut PgConnection,
    parent: &Session,
    text: String,
) -> Result<crate::models::Message, AppError> {
    let content = shared::PortalMessage::text(text).to_json();
    Ok(diesel::insert_into(crate::schema::messages::table)
        .values(crate::models::NewMessage {
            session_id: parent.id,
            role: shared::MessageRole::Portal.to_string(),
            content: content.to_string(),
            user_id: parent.user_id,
            agent_type: parent.agent_type.clone(),
            provenance_kind: None,
            provenance_session_id: None,
            provenance_agent_type: None,
        })
        .get_result(conn)?)
}
pub(crate) fn broadcast_notice(
    state: &AppState,
    parent: &Session,
    inserted: crate::models::Message,
) -> Result<(), SpawnFailure> {
    state.session_manager.broadcast_to_web_clients(
        &parent.session_key,
        shared::ServerToClient::AgentOutput {
            content: serde_json::from_str(&inserted.content)
                .map_err(|e| AppError::Internal(e.to_string()))?,
            agent_type: shared::AgentType::parse_or_default(&parent.agent_type),
            meta: Some(inserted.portal_meta(None)),
        },
    );
    Ok(())
}
pub(crate) async fn sweep(state: Arc<AppState>) -> Result<(), AppError> {
    let mut conn = state.conn()?;
    // An interrupted dispatch before the session transaction committed has no
    // process to recover. Release that reservation explicitly, never retry it
    // under potentially changed approval/policy.
    let abandoned = requests::table
        .filter(requests::state.eq("launching"))
        .filter(
            requests::updated_at.lt(chrono::Utc::now().naive_utc() - chrono::Duration::minutes(2)),
        )
        .select((requests::id, requests::child_session_id))
        .load::<(Uuid, Option<Uuid>)>(&mut conn)?;
    for (id, _) in abandoned {
        conn.transaction::<_, AppError, _>(|conn| {
            let current = requests::table
                .find(id)
                .filter(requests::state.eq("launching"))
                .filter(
                    requests::updated_at
                        .lt(chrono::Utc::now().naive_utc() - chrono::Duration::minutes(2)),
                )
                .select(requests::child_session_id)
                .for_update()
                .first::<Option<Uuid>>(conn)
                .optional()?;
            let Some(child) = current else {
                return Ok(());
            };
            let exists = child
                .map(|id| {
                    sessions::table
                        .find(id)
                        .select(sessions::id)
                        .first::<Uuid>(conn)
                        .optional()
                })
                .transpose()?
                .flatten()
                .is_some();
            if !exists {
                diesel::update(requests::table.find(id))
                    .set((
                        requests::state.eq("failed"),
                        requests::error_code.eq("dispatch_interrupted"),
                        requests::updated_at.eq(diesel::dsl::now),
                    ))
                    .execute(conn)?;
            }
            Ok(())
        })?;
    }
    let children = sessions::table
        .filter(sessions::spawn_depth.gt(1))
        .filter(sessions::paused.eq(false))
        .select(Session::as_select())
        .load::<Session>(&mut conn)?;
    let ids: Vec<_> = children.iter().map(|c| c.id).collect();
    let activity = super::agent_comms::session_activity_states(&state, &mut conn, &ids)?;
    let now = chrono::Utc::now().naive_utc();
    for child in children {
        let parent_ended = if child.spawn_stop_on_parent_end {
            match child.spawned_by {
                Some(id) => sessions::table
                    .find(id)
                    .select((sessions::paused, sessions::status))
                    .first::<(bool, String)>(&mut conn)
                    .optional()?
                    .is_none_or(|(paused, status)| {
                        paused || status == shared::SessionStatus::Replaced.as_str()
                    }),
                None => true,
            }
        } else {
            false
        };
        let last = child.last_messaged_at.max(child.last_activity);
        let idle = child
            .spawn_idle_timeout_seconds
            .is_some_and(|timeout| (now - last).num_seconds() >= i64::from(timeout))
            && !activity.get(&child.id).is_some_and(|s| s.is_active());
        if parent_ended || idle {
            crate::db::session_lifecycle::pause(&mut conn, child.id)?;
            state.session_manager.disconnect_session(child.id);
            state.session_manager.stop_session_on_launcher(
                child.id,
                child.launcher_id,
                Some(child.working_directory),
            );
            tracing::info!(child=%child.id, parent_ended, idle, "Agent child stopped by lifecycle policy");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
