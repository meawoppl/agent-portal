//! An agent can ask, but only the owning human browser can approve. The
//! transcript carries an opaque request ID; all authority comes from this row.
use super::*;
use crate::schema::{
    proxy_auth_tokens as tokens, secret_access_requests as requests, secret_bindings, sessions,
};
use bindings::{Approval, ProcessAudience};
#[derive(Queryable, Selectable)]
#[diesel(table_name=requests)]
struct AccessRequest {
    id: Uuid,
    owner_user_id: Uuid,
    session_id: Uuid,
    launcher_id: Uuid,
    proxy_token_id: Uuid,
    secret_id: Uuid,
    version_id: Uuid,
    generation: i64,
    reference: String,
    requested_version: Option<i32>,
    env_name: String,
    lifetime_seconds: i32,
    state: String,
    binding_id: Option<Uuid>,
    notice_message_id: Option<Uuid>,
    created_at: chrono::DateTime<Utc>,
    expires_at: chrono::DateTime<Utc>,
}
struct BoundProcess;
impl FromRequestParts<Arc<AppState>> for BoundProcess {
    type Rejection = Response;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Response> {
        let mut conn = state.conn().map_err(IntoResponse::into_response)?;
        bindings::authenticated_process(state, &mut conn, &parts.headers)
            .map_err(IntoResponse::into_response)?;
        Ok(Self)
    }
}
/// Bound the body before any transaction/connection is retained. Parser
/// diagnostics are fixed strings, never reflections of supplied scalar data.
struct RequestJson<T>(T);
impl<T: serde::de::DeserializeOwned + Send> axum::extract::FromRequest<Arc<AppState>>
    for RequestJson<T>
{
    type Rejection = AppError;
    async fn from_request(
        request: axum::extract::Request,
        state: &Arc<AppState>,
    ) -> Result<Self, AppError> {
        let _permit = state
            .secrets
            .body_gate
            .clone()
            .try_acquire_owned()
            .map_err(|_| AppError::ServiceUnavailable("Secret request capacity reached"))?;
        let (parts, body) = request.into_parts();
        let bytes = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            axum::body::to_bytes(body, 8192),
        )
        .await
        .map_err(|_| AppError::GatewayTimeout("Secret request body timed out"))?
        .map_err(|_| {
            AppError::PayloadTooLarge("Secret access request exceeds body limit".into())
        })?;
        let request = axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes));
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|_| AppError::BadRequest("Invalid secret access request"))
    }
}
fn load(conn: &mut PgConnection, id: Uuid) -> Result<AccessRequest, AppError> {
    requests::table
        .find(id)
        .for_update()
        .select(AccessRequest::as_select())
        .first(conn)
        .map_err(|_| AppError::NotFound("Secret access request not found"))
}
fn same_process(row: &AccessRequest, process: &ProcessAudience) -> bool {
    row.owner_user_id == process.user
        && row.session_id == process.session
        && row.launcher_id == process.launcher
        && row.proxy_token_id == process.token
}
fn owner(conn: &mut PgConnection, row: &AccessRequest, user: Uuid) -> Result<(), AppError> {
    store::active_user(conn, user)?;
    let current = sessions::table
        .find(row.session_id)
        .select(sessions::user_id)
        .first::<Uuid>(conn)?;
    if row.owner_user_id != user || current != user {
        return Err(AppError::Forbidden);
    };
    Ok(())
}
fn valid_policy(conn: &mut PgConnection, row: &AccessRequest) -> Result<bool, AppError> {
    let secret = store::load(conn, row.secret_id)?;
    let live = secret.deleted_at.is_none()
        && !secret
            .system_tags
            .iter()
            .flatten()
            .any(|tag| tag == "deleted")
        && secret.acl_generation == row.generation;
    let rights = store::rights(conn, &secret, row.owner_user_id)?;
    let session = sessions::table
        .find(row.session_id)
        .first::<crate::models::Session>(conn)?;
    let token = tokens::table
        .find(row.proxy_token_id)
        .first::<crate::models::ProxyAuthToken>(conn)
        .optional()?;
    Ok(live
        && rights.read
        && rights.use_value
        && session.user_id == row.owner_user_id
        && session.launcher_id == Some(row.launcher_id)
        && token.is_some_and(|token| {
            !token.revoked
                && token.user_id == row.owner_user_id
                && token.session_id == Some(row.session_id)
                && token.name == crate::handlers::proxy_tokens::LAUNCH_TOKEN_NAME
                && token.scope.is_none()
                && token.linked_grant_id.is_none()
                && token
                    .expires_at
                    .is_none_or(|time| time > Utc::now().naive_utc())
        })
        && bindings::current_process(conn, row.owner_user_id, row.session_id).ok()
            == Some(row.proxy_token_id))
}
fn refresh_state(conn: &mut PgConnection, row: &mut AccessRequest) -> Result<(), AppError> {
    if row.state != "pending" && row.state != "approved" {
        return Ok(());
    }
    let next = if row.expires_at <= Utc::now() {
        Some("expired")
    } else if !valid_policy(conn, row)? {
        Some("invalidated")
    } else if row.state == "approved" {
        let binding = match row.binding_id {
            Some(id) => secret_bindings::table
                .find(id)
                .select((secret_bindings::revoked_at, secret_bindings::expires_at))
                .first::<(Option<chrono::DateTime<Utc>>, chrono::DateTime<Utc>)>(conn)
                .optional()?,
            None => None,
        };
        match binding {
            Some((None, expires)) if expires > Utc::now() => None,
            _ => Some("invalidated"),
        }
    } else {
        None
    };
    if let Some(state) = next {
        diesel::update(requests::table.find(row.id))
            .set(requests::state.eq(state))
            .execute(conn)?;
        row.state = state.into();
    }
    Ok(())
}
fn info(conn: &mut PgConnection, row: &AccessRequest) -> Result<SecretAccessRequestInfo, AppError> {
    let secret = store::load(conn, row.secret_id)?;
    let version = secret_versions::table
        .find(row.version_id)
        .select(secret_versions::ordinal)
        .first(conn)?;
    let state = match row.state.as_str() {
        "pending" => SecretAccessState::Pending,
        "approved" => SecretAccessState::Approved,
        "denied" => SecretAccessState::Denied,
        "expired" => SecretAccessState::Expired,
        _ => SecretAccessState::Invalidated,
    };
    Ok(SecretAccessRequestInfo {
        id: row.id,
        secret_id: row.secret_id,
        owner_user_id: secret.owner_user_id,
        namespace: if secret.namespace == "personal" {
            SecretNamespace::Personal
        } else {
            SecretNamespace::Organization
        },
        name: secret.name,
        version,
        session_id: row.session_id,
        launcher_id: row.launcher_id,
        env_name: row.env_name.clone(),
        expires_in_seconds: row.lifetime_seconds as u64,
        state,
        created_at: row.created_at.to_rfc3339(),
        expires_at: row.expires_at.to_rfc3339(),
        binding_id: row.binding_id,
    })
}
fn resolve(conn: &mut PgConnection, user: Uuid, reference: &str) -> Result<SecretRow, AppError> {
    let (namespace, name) = reference.split_once('/').ok_or(AppError::BadRequest(
        "Use personal/NAME, org/NAME or id/UUID",
    ))?;
    let row = match namespace {
        "id" => secrets::table
            .find(Uuid::parse_str(name).map_err(|_| AppError::BadRequest("Invalid secret UUID"))?)
            .for_share()
            .first::<SecretRow>(conn)
            .map_err(|_| AppError::NotFound("Secret not found"))?,
        "personal" => secrets::table
            .filter(secrets::namespace.eq("personal"))
            .filter(secrets::owner_user_id.eq(user))
            .filter(secrets::name.eq(name))
            .for_share()
            .first::<SecretRow>(conn)
            .map_err(|_| AppError::NotFound("Secret not found"))?,
        "org" => secrets::table
            .filter(secrets::namespace.eq("organization"))
            .filter(secrets::name.eq(name))
            .for_share()
            .first::<SecretRow>(conn)
            .map_err(|_| AppError::NotFound("Secret not found"))?,
        _ => {
            return Err(AppError::BadRequest(
                "Use personal/NAME, org/NAME or id/UUID",
            ))
        }
    };
    store::require_live(&row)?;
    let rights = store::rights(conn, &row, user)?;
    if !rights.read || !rights.use_value {
        return Err(AppError::Forbidden);
    }
    Ok(row)
}
fn publish(state: &AppState, message: Option<crate::models::Message>) -> Result<(), AppError> {
    if let Some(message) = message {
        let agent_type = shared::AgentType::parse_or_default(&message.agent_type);
        let content = serde_json::from_str(&message.content)
            .map_err(|_| AppError::ServiceUnavailable("Stored approval notice unavailable"))?;
        state.session_manager.broadcast_to_web_clients(
            &message.session_id.to_string(),
            shared::ServerToClient::AgentOutput {
                content,
                agent_type,
                meta: Some(message.portal_meta(None)),
            },
        );
    }
    Ok(())
}
fn notice(
    conn: &mut PgConnection,
    row: &AccessRequest,
) -> Result<Option<crate::models::Message>, AppError> {
    use crate::schema::messages;
    match row.notice_message_id {
        Some(id) => Ok(messages::table
            .find(id)
            .filter(messages::session_id.eq(row.session_id))
            .first::<crate::models::Message>(conn)
            .optional()?),
        None => Ok(None),
    }
}
async fn create(
    State(state): State<Arc<AppState>>,
    _process: BoundProcess,
    headers: HeaderMap,
    RequestJson(request): RequestJson<SecretAccessCreateRequest>,
) -> Result<Json<SecretAccessRequestInfo>, AppError> {
    state.secrets.crypto()?;
    bindings::validate_terms(&request.env_name, request.expires_in_seconds)?;
    if request.reference.len() > 256 || request.reference.trim() != request.reference {
        return Err(AppError::BadRequest("Invalid secret reference"));
    };
    let _permit = state
        .secrets
        .mutation_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::ServiceUnavailable("Another secrets mutation is in progress; retry")
        })?;
    let mut conn = state.conn()?;
    let (response, message, created) = conn.transaction::<_, AppError, _>(|conn| {
        let process = bindings::authenticated_process(&state, conn, &headers)?;
        diesel::sql_query("SELECT pg_advisory_xact_lock(1936024434,2)").execute(conn)?;
        if let Some(mut existing) = requests::table
            .find(request.request_id)
            .for_update()
            .select(AccessRequest::as_select())
            .first::<AccessRequest>(conn)
            .optional()?
        {
            if !same_process(&existing, &process) {
                return Err(AppError::Forbidden);
            }
            if existing.reference != request.reference
                || existing.requested_version != request.version
                || existing.env_name != request.env_name
                || existing.lifetime_seconds as u64 != request.expires_in_seconds
            {
                return Err(AppError::Conflict(
                    "Request ID belongs to a different approval payload",
                ));
            }
            refresh_state(conn, &mut existing)?;
            return Ok((info(conn, &existing)?, notice(conn, &existing)?, false));
        }
        let recent = requests::table
            .filter(requests::owner_user_id.eq(process.user))
            .filter(requests::created_at.gt(Utc::now() - chrono::Duration::hours(1)))
            .count()
            .get_result::<i64>(conn)?;
        let pending = requests::table
            .filter(requests::state.eq("pending"))
            .filter(requests::expires_at.gt(Utc::now()));
        let user_pending = pending
            .filter(requests::owner_user_id.eq(process.user))
            .count()
            .get_result::<i64>(conn)?;
        let process_pending = pending
            .filter(requests::proxy_token_id.eq(process.token))
            .count()
            .get_result::<i64>(conn)?;
        if recent >= 30 || user_pending >= 16 || process_pending >= 4 {
            return Err(AppError::ServiceUnavailable(
                "Secret approval request rate limit reached",
            ));
        }
        let secret = resolve(conn, process.user, &request.reference)?;
        let query = secret_versions::table.filter(secret_versions::secret_id.eq(secret.id));
        let version = if let Some(version) = request.version {
            query
                .filter(secret_versions::ordinal.eq(version))
                .select(secret_versions::id)
                .first::<Uuid>(conn)
        } else {
            query
                .filter(secret_versions::id.nullable().eq(secret.current_version_id))
                .select(secret_versions::id)
                .first::<Uuid>(conn)
        }
        .map_err(|_| AppError::NotFound("Secret version not found"))?;
        let session = sessions::table
            .find(process.session)
            .first::<crate::models::Session>(conn)?;
        if state.session_manager.launcher_owner(process.launcher) != Some(process.user)
            || !state
                .session_manager
                .is_proxy_connected(&session.session_key)
        {
            return Err(AppError::Conflict("Requesting process is not connected"));
        }
        let expires = Utc::now() + chrono::Duration::minutes(10);
        diesel::insert_into(requests::table)
            .values((
                requests::id.eq(request.request_id),
                requests::owner_user_id.eq(process.user),
                requests::session_id.eq(process.session),
                requests::launcher_id.eq(process.launcher),
                requests::proxy_token_id.eq(process.token),
                requests::secret_id.eq(secret.id),
                requests::version_id.eq(version),
                requests::generation.eq(secret.acl_generation),
                requests::reference.eq(&request.reference),
                requests::requested_version.eq(request.version),
                requests::env_name.eq(&request.env_name),
                requests::lifetime_seconds.eq(request.expires_in_seconds as i32),
                requests::expires_at.eq(expires),
            ))
            .execute(conn)?;
        let portal =
            shared::PortalMessage::with_content(vec![shared::PortalContent::SecretAccessRequest {
                request_id: request.request_id,
            }]);
        let message = crate::secrets::redaction::insert_message(
            conn,
            crate::models::NewMessage {
                session_id: process.session,
                role: shared::MessageRole::Portal.to_string(),
                content: portal.to_json().to_string(),
                user_id: process.user,
                agent_type: session.agent_type,
                provenance_kind: None,
                provenance_session_id: None,
                provenance_agent_type: None,
            },
        )?;
        diesel::update(requests::table.find(request.request_id))
            .set(requests::notice_message_id.eq(Some(message.id)))
            .execute(conn)?;
        store::audit(
            conn,
            process.user,
            Some(secret.id),
            "access_requested",
            "pending",
        )?;
        let row = load(conn, request.request_id)?;
        Ok((info(conn, &row)?, Some(message), true))
    })?;
    drop(conn);
    publish(&state, message)?;
    if created {
        state
            .notifications
            .emit(crate::push::NotificationEvent::PermissionRequest {
                session_id: response.session_id,
                session_name: String::new(),
                tool_name: "Secret access".into(),
                input_snippet: None,
            })
    };
    Ok(Json(response))
}
async fn get_request(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: tower_cookies::Cookies,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretAccessRequestInfo>, AppError> {
    let _permit = state
        .secrets
        .body_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::ServiceUnavailable("Secret request capacity reached"))?;
    let mut conn = state
        .db_pool
        .try_get()
        .ok_or(AppError::ServiceUnavailable("Database is busy; retry"))?;
    let response = conn.transaction::<_, AppError, _>(|conn| {
        diesel::sql_query("SET LOCAL lock_timeout = '250ms'").execute(conn)?;
        let mut row = load(conn, id)?;
        if headers.contains_key(header::AUTHORIZATION) {
            let process = bindings::authenticated_process(&state, conn, &headers)?;
            if !same_process(&row, &process) {
                return Err(AppError::Forbidden);
            }
        } else {
            let principal = super::principal(&state, &headers, &cookies, false)?;
            if !principal.browser {
                return Err(AppError::Forbidden);
            };
            owner(conn, &row, principal.user)?
        };
        refresh_state(conn, &mut row)?;
        info(conn, &row)
    })?;
    Ok(Json(response))
}
async fn decision(
    State(state): State<Arc<AppState>>,
    crate::auth::BrowserUserId(user): crate::auth::BrowserUserId,
    headers: HeaderMap,
    cookies: tower_cookies::Cookies,
    Path(id): Path<Uuid>,
    RequestJson(request): RequestJson<SecretAccessDecisionRequest>,
) -> Result<Json<SecretAccessRequestInfo>, AppError> {
    auth::csrf(&state, &headers)?;
    if request.approve {
        auth::require_verification(&cookies, &state, user)?
    };
    let _permit = state
        .secrets
        .mutation_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            AppError::ServiceUnavailable("Another secrets mutation is in progress; retry")
        })?;
    let mut conn = state.conn()?;
    let (response, message) = conn.transaction::<_, AppError, _>(|conn| {
        let mut row = load(conn, id)?;
        owner(conn, &row, user)?;
        refresh_state(conn, &mut row)?;
        if row.state == "pending" {
            if request.approve {
                let binding = bindings::approve(
                    &state,
                    conn,
                    Approval {
                        id: Uuid::new_v5(&row.id, b"agent-portal:secret-access-binding:v1"),
                        secret_id: row.secret_id,
                        session_id: row.session_id,
                        launcher_id: row.launcher_id,
                        owner_user_id: row.owner_user_id,
                        version_id: row.version_id,
                        proxy_token_id: row.proxy_token_id,
                        generation: row.generation,
                        env_name: row.env_name.clone(),
                        lifetime_seconds: row.lifetime_seconds as u64,
                    },
                )?;
                if row.expires_at <= Utc::now() {
                    return Err(AppError::Conflict("Approval request expired"));
                }
                let expires = chrono::DateTime::parse_from_rfc3339(&binding.expires_at)
                    .map_err(|_| AppError::Internal("Invalid binding deadline".into()))?
                    .with_timezone(&Utc);
                diesel::update(requests::table.find(id))
                    .set((
                        requests::state.eq("approved"),
                        requests::binding_id.eq(Some(binding.id)),
                        requests::expires_at.eq(expires),
                    ))
                    .execute(conn)?;
                row.state = "approved".into();
                row.binding_id = Some(binding.id);
                row.expires_at = expires;
            } else {
                diesel::update(requests::table.find(id))
                    .set(requests::state.eq("denied"))
                    .execute(conn)?;
                row.state = "denied".into();
            }
            store::audit(
                conn,
                user,
                Some(row.secret_id),
                "access_decision",
                &row.state,
            )?;
        }
        Ok((info(conn, &row)?, notice(conn, &row)?))
    })?;
    drop(conn);
    publish(&state, message)?;
    Ok(Json(response))
}
pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/secrets/requests", post(create))
        .route("/api/secrets/requests/{id}", get(get_request))
        .route("/api/secrets/requests/{id}/decision", post(decision))
}
#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
