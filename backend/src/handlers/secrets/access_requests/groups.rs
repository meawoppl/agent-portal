//! Group membership is durable; individual member endpoints cannot split approval.
use super::*;
use crate::schema::secret_access_request_groups as groups;
use std::collections::HashSet;

pub(super) fn require_single(conn: &mut PgConnection, id: Uuid) -> Result<(), AppError> {
    if groups::table
        .filter(groups::request_id.eq(id).or(groups::group_id.eq(id)))
        .select(groups::request_id)
        .first::<Uuid>(conn)
        .optional()?
        .is_some()
    {
        return Err(AppError::Conflict(
            "Review this credential through its combined request",
        ));
    }
    Ok(())
}
fn ids(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Uuid>, AppError> {
    let rows = groups::table
        .filter(groups::group_id.eq(id))
        .order(groups::position.asc())
        .select((groups::request_id, groups::position, groups::group_size))
        .load::<(Uuid, i32, i32)>(conn)?;
    if rows
        .iter()
        .enumerate()
        .any(|(index, (_, position, total))| {
            *position != index as i32 || *total as usize != rows.len()
        })
    {
        return Err(AppError::Conflict(
            "Credential set is incomplete; request the set again",
        ));
    }
    Ok(rows.into_iter().map(|(id, _, _)| id).collect())
}
fn member_request(
    request: &SecretAccessGroupCreateRequest,
    index: usize,
) -> SecretAccessCreateRequest {
    let item = &request.items[index];
    SecretAccessCreateRequest {
        request_id: Uuid::new_v5(
            &request.request_id,
            format!("secret-access-group:v1:{index}").as_bytes(),
        ),
        reference: item.reference.clone(),
        env_name: item.env_name.clone(),
        version: item.version,
        expires_in_seconds: request.expires_in_seconds,
    }
}
fn validate(request: &SecretAccessGroupCreateRequest) -> Result<(), AppError> {
    if request.items.is_empty() || request.items.len() > MAX_SECRET_ACCESS_GROUP_ITEMS {
        return Err(AppError::BadRequest("Request between 1 and 16 credentials"));
    }
    let mut envs = HashSet::new();
    for item in &request.items {
        bindings::validate_terms(&item.env_name, request.expires_in_seconds)?;
        if !envs.insert(item.env_name.to_ascii_uppercase())
            || item.reference.len() > 256
            || item.reference.trim() != item.reference
            || item.version.is_some_and(|v| v < 1)
        {
            return Err(AppError::BadRequest(
                "Invalid credential reference, version, or duplicate environment variable",
            ));
        }
    }
    Ok(())
}
fn summary(id: Uuid, items: Vec<SecretAccessRequestInfo>) -> SecretAccessGroupInfo {
    let state = if items
        .iter()
        .any(|i| i.state == SecretAccessState::Invalidated)
    {
        SecretAccessState::Invalidated
    } else if items.iter().any(|i| i.state == SecretAccessState::Expired) {
        SecretAccessState::Expired
    } else if items.iter().any(|i| i.state == SecretAccessState::Denied) {
        SecretAccessState::Denied
    } else if items.iter().all(|i| i.state == SecretAccessState::Approved) {
        SecretAccessState::Approved
    } else {
        SecretAccessState::Pending
    };
    SecretAccessGroupInfo { id, state, items }
}
fn load_group(
    state: &AppState,
    conn: &mut PgConnection,
    headers: &HeaderMap,
    cookies: &tower_cookies::Cookies,
    id: Uuid,
) -> Result<SecretAccessGroupInfo, AppError> {
    let ids = ids(conn, id)?;
    if ids.is_empty() {
        return Err(AppError::NotFound("Credential request not found"));
    }
    let process = if headers.contains_key(header::AUTHORIZATION) {
        Some(bindings::authenticated_process(state, conn, headers)?)
    } else {
        None
    };
    let browser = if process.is_none() {
        let principal = super::super::principal(state, headers, cookies, false)?;
        if !principal.browser {
            return Err(AppError::Forbidden);
        }
        Some(principal.user)
    } else {
        None
    };
    let mut items = Vec::new();
    for id in ids {
        let mut row = load(conn, id)?;
        if let Some(process) = &process {
            if !same_process(&row, process) {
                return Err(AppError::Forbidden);
            }
        } else {
            owner(conn, &row, browser.ok_or(AppError::Forbidden)?)?;
        }
        refresh_state(conn, &mut row)?;
        items.push(info(conn, &row)?);
    }
    Ok(summary(id, items))
}
async fn create_group(
    State(state): State<Arc<AppState>>,
    _process: BoundProcess,
    headers: HeaderMap,
    RequestJson(request): RequestJson<SecretAccessGroupCreateRequest>,
) -> Result<Json<SecretAccessGroupInfo>, AppError> {
    state.secrets.crypto()?;
    validate(&request)?;
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
        diesel::sql_query("SELECT pg_advisory_xact_lock(1936024434,2)").execute(conn)?;
        if requests::table
            .find(request.request_id)
            .select(requests::id)
            .first::<Uuid>(conn)
            .optional()?
            .is_some()
        {
            return Err(AppError::Conflict(
                "Request ID already identifies a single credential",
            ));
        }
        let existing = ids(conn, request.request_id)?;
        let expected: Vec<_> = (0..request.items.len())
            .map(|i| member_request(&request, i).request_id)
            .collect();
        if !existing.is_empty() && existing != expected {
            return Err(AppError::Conflict(
                "Request ID belongs to a different credential set",
            ));
        }
        let created = existing.is_empty();
        let mut items = Vec::new();
        for (index, expected_id) in expected.iter().enumerate() {
            // Do not adopt pre-existing single requests into a new group.
            if created
                && requests::table
                    .find(expected_id)
                    .select(requests::id)
                    .first::<Uuid>(conn)
                    .optional()?
                    .is_some()
            {
                return Err(AppError::Conflict("Credential request ID already exists"));
            }
            let (item, _, _) = create_row(
                &state,
                conn,
                &headers,
                &member_request(&request, index),
                true,
            )?;
            if created {
                diesel::insert_into(groups::table)
                    .values((
                        groups::request_id.eq(item.id),
                        groups::group_id.eq(request.request_id),
                        groups::position.eq(index as i32),
                        groups::group_size.eq(request.items.len() as i32),
                    ))
                    .execute(conn)?;
            }
            items.push(item);
        }
        let first = &items[0];
        let message = if created {
            let session = sessions::table
                .find(first.session_id)
                .first::<crate::models::Session>(conn)?;
            let portal = shared::PortalMessage::with_content(vec![
                shared::PortalContent::SecretAccessGroup {
                    request_id: request.request_id,
                },
            ]);
            let message = crate::secrets::redaction::insert_message(
                conn,
                crate::models::NewMessage {
                    session_id: first.session_id,
                    role: shared::MessageRole::Portal.to_string(),
                    content: portal.to_json().to_string(),
                    user_id: first.owner_user_id,
                    agent_type: session.agent_type,
                    provenance_kind: None,
                    provenance_session_id: None,
                    provenance_agent_type: None,
                },
            )?;
            diesel::update(requests::table.filter(requests::id.eq_any(&expected)))
                .set(requests::notice_message_id.eq(Some(message.id)))
                .execute(conn)?;
            Some(message)
        } else {
            {
                let row = load(conn, first.id)?;
                notice(conn, &row)?
            }
        };
        Ok((summary(request.request_id, items), message, created))
    })?;
    drop(conn);
    publish(&state, message)?;
    if created {
        state
            .notifications
            .emit(crate::push::NotificationEvent::PermissionRequest {
                session_id: response.items[0].session_id,
                session_name: String::new(),
                tool_name: "Secret access".into(),
                input_snippet: None,
            });
    }
    Ok(Json(response))
}
async fn get_group(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: tower_cookies::Cookies,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretAccessGroupInfo>, AppError> {
    let _permit = state
        .secrets
        .body_gate
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::ServiceUnavailable("Secret request capacity reached"))?;
    let mut conn = state.conn()?;
    let response = conn.transaction::<_, AppError, _>(|conn| {
        diesel::sql_query("SET LOCAL lock_timeout = '250ms'").execute(conn)?;
        load_group(&state, conn, &headers, &cookies, id)
    })?;
    Ok(Json(response))
}
async fn decide_group(
    State(state): State<Arc<AppState>>,
    crate::auth::BrowserUserId(user): crate::auth::BrowserUserId,
    headers: HeaderMap,
    cookies: tower_cookies::Cookies,
    Path(id): Path<Uuid>,
    RequestJson(request): RequestJson<SecretAccessDecisionRequest>,
) -> Result<Json<SecretAccessGroupInfo>, AppError> {
    auth::csrf(&state, &headers)?;
    if request.approve {
        auth::require_verification(&cookies, &state, user)?;
    }
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
        let group = load_group(&state, conn, &headers, &cookies, id)?;
        // A failed member invalidates the whole approval. No partial grant is committed.
        if request.approve
            && !matches!(
                group.state,
                SecretAccessState::Pending | SecretAccessState::Approved
            )
        {
            return Err(AppError::Conflict(
                "A credential is no longer available; request the set again",
            ));
        }
        let mut items = Vec::new();
        let mut message = None;
        for item in group.items {
            let (item, notice) = decide_row(&state, conn, item.id, user, request.approve)?;
            items.push(item);
            if message.is_none() {
                message = notice;
            }
        }
        Ok((summary(id, items), message))
    })?;
    drop(conn);
    publish(&state, message)?;
    Ok(Json(response))
}
pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/secrets/request-groups", post(create_group))
        .route("/api/secrets/request-groups/{id}", get(get_group))
        .route(
            "/api/secrets/request-groups/{id}/decision",
            post(decide_group),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn group_scope_rejects_empty_oversized_and_colliding_environments() {
        let item = SecretAccessGroupItem {
            reference: "personal/TOKEN".into(),
            env_name: "TOKEN".into(),
            version: None,
        };
        let mut request = SecretAccessGroupCreateRequest {
            request_id: Uuid::new_v4(),
            items: vec![],
            expires_in_seconds: 900,
        };
        assert!(validate(&request).is_err());
        request.items = vec![item.clone(); MAX_SECRET_ACCESS_GROUP_ITEMS + 1];
        assert!(validate(&request).is_err());
        request.items = vec![
            item.clone(),
            SecretAccessGroupItem {
                env_name: "token".into(),
                ..item.clone()
            },
        ];
        assert!(validate(&request).is_err());
        request.items = vec![item];
        assert!(validate(&request).is_ok());
        request.items[0].version = Some(0);
        assert!(validate(&request).is_err());
    }
}
