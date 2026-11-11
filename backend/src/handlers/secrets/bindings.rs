//! Explicit human approval of one session/process audience. Ordinary session
//! credentials are confined to that binding and never gain general vault reads.
use super::*;
use crate::schema::{secret_bindings as bindings, secret_redemption_tickets as tickets, sessions};
#[derive(Queryable, Selectable)]
#[diesel(table_name=bindings)]
struct Binding {
    id: Uuid,
    secret_id: Uuid,
    session_id: Uuid,
    launcher_id: Uuid,
    created_by: Uuid,
    operation: String,
    expires_at: chrono::DateTime<Utc>,
    generation: i64,
    revoked_at: Option<chrono::DateTime<Utc>>,
    version_id: Uuid,
    env_name: String,
    proxy_token_id: Option<Uuid>,
}
fn load(conn: &mut PgConnection, id: Uuid) -> Result<Binding, AppError> {
    bindings::table
        .find(id)
        .for_update()
        .select(Binding::as_select())
        .first(conn)
        .map_err(|_| AppError::NotFound("Binding not found"))
}
fn metadata(conn: &mut PgConnection, row: &Binding) -> Result<SecretBindingMetadata, AppError> {
    let version = secret_versions::table
        .find(row.version_id)
        .select(secret_versions::ordinal)
        .first(conn)?;
    Ok(SecretBindingMetadata {
        id: row.id,
        secret_id: row.secret_id,
        session_id: row.session_id,
        launcher_id: row.launcher_id,
        version,
        env_name: row.env_name.clone(),
        generation: row.generation,
        expires_at: row.expires_at.to_rfc3339(),
        revoked_at: row.revoked_at.map(|time| time.to_rfc3339()),
    })
}
pub(super) fn replay_metadata(
    conn: &mut PgConnection,
    id: Uuid,
    user: Uuid,
) -> Result<SecretBindingMetadata, AppError> {
    let row = load(conn, id)?;
    if row.created_by != user {
        return Err(AppError::NotFound("Binding not found"));
    };
    metadata(conn, &row)
}
fn session_audience(
    state: &AppState,
    conn: &mut PgConnection,
    row: &Binding,
    user: Uuid,
) -> Result<(), AppError> {
    let session = sessions::table
        .find(row.session_id)
        .first::<crate::models::Session>(conn)
        .map_err(|_| AppError::Forbidden)?;
    if session.user_id != user
        || row.created_by != user
        || session.launcher_id != Some(row.launcher_id)
        || state.session_manager.launcher_owner(row.launcher_id) != Some(user)
        || !state
            .session_manager
            .is_proxy_connected(&session.session_key)
    {
        return Err(AppError::Forbidden);
    };
    Ok(())
}
fn policy(
    state: &AppState,
    conn: &mut PgConnection,
    row: &Binding,
    user: Uuid,
) -> Result<SecretRow, AppError> {
    if row.revoked_at.is_some() || row.expires_at <= Utc::now() || row.operation != "env" {
        return Err(AppError::Forbidden);
    };
    let secret = store::load(conn, row.secret_id)?;
    store::require_live(&secret)?;
    store::require_generation(&secret, row.generation)?;
    let rights = store::rights(conn, &secret, user)?;
    if !rights.read || !rights.use_value {
        return Err(AppError::Forbidden);
    };
    if Some(current_process(conn, user, row.session_id)?) != row.proxy_token_id {
        return Err(AppError::Forbidden);
    }
    session_audience(state, conn, row, user)?;
    Ok(secret)
}
fn proxy(
    state: &AppState,
    conn: &mut PgConnection,
    headers: &HeaderMap,
    binding: &Binding,
) -> Result<crate::handlers::proxy_tokens::VerifiedProxyToken, AppError> {
    let token = crate::auth::bearer_token(headers).ok_or(AppError::Unauthorized)?;
    if token.starts_with("aps_secret_")
        || token.starts_with(crate::handlers::federation::LINKED_TOKEN_PREFIX)
    {
        return Err(AppError::Forbidden);
    };
    let verified =
        crate::handlers::proxy_tokens::verify_and_get_user_with_token(state, conn, token)?;
    if verified.token.session_id != Some(binding.session_id)
        || binding.proxy_token_id != Some(verified.token.id)
    {
        return Err(AppError::Forbidden);
    };
    store::active_user(conn, verified.user_id)?;
    policy(state, conn, binding, verified.user_id)?;
    Ok(verified)
}
#[derive(Clone)]
pub(super) struct ProcessAudience {
    pub user: Uuid,
    pub session: Uuid,
    pub launcher: Uuid,
    pub token: Uuid,
}
pub(super) fn current_process(
    conn: &mut PgConnection,
    user: Uuid,
    session: Uuid,
) -> Result<Uuid, AppError> {
    use crate::schema::proxy_auth_tokens as tokens;
    tokens::table
        .filter(tokens::session_id.eq(Some(session)))
        .filter(tokens::user_id.eq(user))
        .filter(tokens::name.eq(crate::handlers::proxy_tokens::LAUNCH_TOKEN_NAME))
        .filter(tokens::revoked.eq(false))
        .filter(tokens::scope.is_null())
        .filter(tokens::linked_grant_id.is_null())
        .filter(
            tokens::expires_at
                .is_null()
                .or(tokens::expires_at.gt(Utc::now().naive_utc())),
        )
        .order((tokens::created_at.desc(), tokens::id.desc()))
        .select(tokens::id)
        .first::<Uuid>(conn)
        .map_err(|_| AppError::Forbidden)
}
pub(super) fn authenticated_process(
    state: &AppState,
    conn: &mut PgConnection,
    headers: &HeaderMap,
) -> Result<ProcessAudience, AppError> {
    let raw = crate::auth::bearer_token(headers).ok_or(AppError::Unauthorized)?;
    if raw.starts_with("aps_secret_")
        || raw.starts_with(crate::handlers::federation::LINKED_TOKEN_PREFIX)
    {
        return Err(AppError::Forbidden);
    }
    let verified = crate::handlers::proxy_tokens::verify_and_get_user_with_token(state, conn, raw)?;
    let session = verified.token.session_id.ok_or(AppError::Forbidden)?;
    if verified.token.name != crate::handlers::proxy_tokens::LAUNCH_TOKEN_NAME
        || current_process(conn, verified.user_id, session)? != verified.token.id
    {
        return Err(AppError::Forbidden);
    }
    store::active_user(conn, verified.user_id)?;
    let row = sessions::table
        .find(session)
        .first::<crate::models::Session>(conn)
        .map_err(|_| AppError::Forbidden)?;
    if row.user_id != verified.user_id {
        return Err(AppError::Forbidden);
    }
    Ok(ProcessAudience {
        user: verified.user_id,
        session,
        launcher: row.launcher_id.ok_or(AppError::Forbidden)?,
        token: verified.token.id,
    })
}
pub(super) fn validate_terms(env: &str, lifetime: u64) -> Result<(), AppError> {
    if lifetime == 0
        || lifetime > 3600
        || env.is_empty()
        || env.len() > 128
        || !env
            .bytes()
            .enumerate()
            .all(|(i, c)| c == b'_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit()))
    {
        return Err(AppError::BadRequest(
            "Binding needs a valid environment name and lifetime of 1 to 3600 seconds",
        ));
    }
    Ok(())
}
pub(super) struct Approval {
    pub id: Uuid,
    pub secret_id: Uuid,
    pub session_id: Uuid,
    pub launcher_id: Uuid,
    pub owner_user_id: Uuid,
    pub version_id: Uuid,
    pub proxy_token_id: Uuid,
    pub generation: i64,
    pub env_name: String,
    pub lifetime_seconds: u64,
}
/// Both human binding and in-thread approval use this transaction-local policy
/// and insertion path. No caller can weaken the version/process audience pin.
pub(super) fn approve(
    state: &AppState,
    conn: &mut PgConnection,
    approval: Approval,
) -> Result<SecretBindingMetadata, AppError> {
    validate_terms(&approval.env_name, approval.lifetime_seconds)?;
    let row = secrets::table
        .find(approval.secret_id)
        .for_share()
        .first::<SecretRow>(conn)?;
    store::require_live(&row)?;
    store::require_generation(&row, approval.generation)?;
    let rights = store::rights(conn, &row, approval.owner_user_id)?;
    if !rights.read || !rights.use_value {
        return Err(AppError::Forbidden);
    }
    crate::schema::users::table
        .find(approval.owner_user_id)
        .for_update()
        .select(crate::schema::users::id)
        .first::<Uuid>(conn)?;
    if current_process(conn, approval.owner_user_id, approval.session_id)?
        != approval.proxy_token_id
    {
        return Err(AppError::Forbidden);
    }
    let version = secret_versions::table
        .find(approval.version_id)
        .filter(secret_versions::secret_id.eq(row.id))
        .first::<VersionRow>(conn)?;
    let candidate = Binding {
        id: approval.id,
        secret_id: row.id,
        session_id: approval.session_id,
        launcher_id: approval.launcher_id,
        created_by: approval.owner_user_id,
        operation: "env".into(),
        expires_at: Utc::now() + chrono::Duration::seconds(approval.lifetime_seconds as i64),
        generation: approval.generation,
        revoked_at: None,
        version_id: version.id,
        env_name: approval.env_name,
        proxy_token_id: Some(approval.proxy_token_id),
    };
    session_audience(state, conn, &candidate, approval.owner_user_id)?;
    diesel::delete(
        bindings::table.filter(bindings::expires_at.lt(Utc::now() - chrono::Duration::days(1))),
    )
    .execute(conn)?;
    let count = bindings::table
        .filter(bindings::created_by.eq(approval.owner_user_id))
        .filter(bindings::revoked_at.is_null())
        .filter(bindings::expires_at.gt(Utc::now()))
        .count()
        .get_result::<i64>(conn)?;
    if count >= 64 || bindings::table.count().get_result::<i64>(conn)? > 10000 {
        return Err(AppError::Conflict("Execution binding quota reached"));
    }
    diesel::insert_into(bindings::table)
        .values((
            bindings::id.eq(candidate.id),
            bindings::secret_id.eq(candidate.secret_id),
            bindings::session_id.eq(candidate.session_id),
            bindings::launcher_id.eq(candidate.launcher_id),
            bindings::created_by.eq(candidate.created_by),
            bindings::operation.eq("env"),
            bindings::expires_at.eq(candidate.expires_at),
            bindings::generation.eq(candidate.generation),
            bindings::version_id.eq(candidate.version_id),
            bindings::env_name.eq(&candidate.env_name),
            bindings::proxy_token_id.eq(candidate.proxy_token_id),
        ))
        .execute(conn)?;
    store::audit(
        conn,
        candidate.created_by,
        Some(row.id),
        "binding_created",
        "allowed",
    )?;
    metadata(conn, &candidate)
}
pub(super) async fn create(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    headers: HeaderMap,
    cookies: tower_cookies::Cookies,
    SecretJson(request): SecretJson<SecretBindingCreateRequest>,
) -> Result<Json<SecretBindingMetadata>, AppError> {
    mutation(&state, &headers, principal)?;
    if principal.browser {
        auth::require_recent_auth(&cookies, &state, principal.user)?
    }
    if !principal.browser && !principal.read {
        return Err(AppError::Forbidden);
    };
    validate_terms(&request.env_name, request.expires_in_seconds)?;
    state.secrets.crypto()?;
    let id = Uuid::new_v5(&principal.user, idempotency::key(&headers)?.as_bytes());
    let mut conn = state.conn()?;
    conn.transaction::<_, AppError, _>(|conn| {
        let row = store::load(conn, request.secret_id)?;
        store::require_live(&row)?;
        store::require_generation(&row, request.expected_generation)?;
        let rights = store::rights(conn, &row, principal.user)?;
        if !rights.read || !rights.use_value {
            return Err(AppError::Forbidden);
        };
        if let Some(previous) = bindings::table
            .find(id)
            .select(Binding::as_select())
            .first::<Binding>(conn)
            .optional()?
        {
            if previous.created_by != principal.user {
                return Err(AppError::Forbidden);
            };
            return Ok(Json(metadata(conn, &previous)?));
        }
        let version_query = secret_versions::table.filter(secret_versions::secret_id.eq(row.id));
        let version = if let Some(version) = request.version {
            version_query
                .filter(secret_versions::ordinal.eq(version))
                .select(secret_versions::id)
                .first::<Uuid>(conn)
        } else {
            version_query
                .filter(secret_versions::id.nullable().eq(row.current_version_id))
                .select(secret_versions::id)
                .first::<Uuid>(conn)
        }
        .map_err(|_| AppError::NotFound("Secret version not found"))?;
        let approved_process = current_process(conn, principal.user, request.session_id)?;
        Ok(Json(approve(
            state.as_ref(),
            conn,
            Approval {
                id,
                secret_id: row.id,
                session_id: request.session_id,
                launcher_id: request.launcher_id,
                owner_user_id: principal.user,
                version_id: version,
                proxy_token_id: approved_process,
                generation: row.acl_generation,
                env_name: request.env_name,
                lifetime_seconds: request.expires_in_seconds,
            },
        )?))
    })
}
pub(super) async fn list(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
) -> Result<Json<SecretBindingListResponse>, AppError> {
    if !principal.metadata {
        return Err(AppError::Forbidden);
    };
    let mut conn = state.conn()?;
    store::active_user(&mut conn, principal.user)?;
    let rows = bindings::table
        .filter(bindings::created_by.eq(principal.user))
        .order(bindings::created_at.desc())
        .select(Binding::as_select())
        .load::<Binding>(&mut conn)?;
    let mut result = vec![];
    for row in rows {
        result.push(metadata(&mut conn, &row)?)
    }
    Ok(Json(SecretBindingListResponse { bindings: result }))
}
pub(super) async fn revoke(
    State(state): State<Arc<AppState>>,
    SecretPrincipal(principal): SecretPrincipal,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretBindingMetadata>, AppError> {
    mutation(&state, &headers, principal)?;
    let mut conn = state.conn()?;
    let row = load(&mut conn, id)?;
    if row.created_by != principal.user {
        return Err(AppError::Forbidden);
    };
    if row.revoked_at.is_none() {
        diesel::update(bindings::table.find(id))
            .set(bindings::revoked_at.eq(Some(Utc::now())))
            .execute(&mut conn)?;
        store::audit(
            &mut conn,
            principal.user,
            Some(row.secret_id),
            "binding_revoked",
            "allowed",
        )?
    };
    let latest = load(&mut conn, id)?;
    Ok(Json(metadata(&mut conn, &latest)?))
}
pub(super) async fn ticket(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretBindingTicketResponse>, AppError> {
    state.secrets.crypto()?;
    let mut conn = state.conn()?;
    conn.transaction::<_, AppError, _>(|conn| {
        let row = load(conn, id)?;
        let verified = proxy(&state, conn, &headers, &row)?;
        diesel::delete(
            tickets::table
                .filter(tickets::expires_at.lt(Utc::now() - chrono::Duration::minutes(1))),
        )
        .execute(conn)?;
        let active = tickets::table
            .filter(tickets::binding_id.eq(id))
            .filter(tickets::consumed_at.is_null())
            .filter(tickets::expires_at.gt(Utc::now()))
            .count()
            .get_result::<i64>(conn)?;
        let recent = tickets::table
            .filter(tickets::binding_id.eq(id))
            .filter(tickets::created_at.gt(Utc::now() - chrono::Duration::minutes(1)))
            .count()
            .get_result::<i64>(conn)?;
        if active >= 4 || recent >= 30 {
            return Err(AppError::ServiceUnavailable(
                "Binding ticket rate limit reached",
            ));
        };
        let mut bytes = [0; 32];
        openssl::rand::rand_bytes(&mut bytes)
            .map_err(|_| AppError::Internal("Secure random unavailable".into()))?;
        let token = format!("aps_redeem_{}", hex::encode(bytes));
        let expires = std::cmp::min(row.expires_at, Utc::now() + chrono::Duration::seconds(30));
        diesel::insert_into(tickets::table)
            .values((
                tickets::id.eq(Uuid::new_v4()),
                tickets::binding_id.eq(id),
                tickets::proxy_token_id.eq(verified.token.id),
                tickets::token_hash.eq(crate::jwt::hash_token(&token)),
                tickets::expires_at.eq(expires),
            ))
            .execute(conn)?;
        store::audit(
            conn,
            verified.user_id,
            Some(row.secret_id),
            "binding_ticket",
            "allowed",
        )?;
        Ok(Json(SecretBindingTicketResponse {
            ticket: SecretValue(token),
            expires_in: (expires - Utc::now()).num_seconds().max(0) as u64,
        }))
    })
}
pub(super) async fn redeem(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    SecretJson(mut request): SecretJson<SecretBindingRedeemRequest>,
) -> Result<Json<SecretBindingRedeemResponse>, AppError> {
    let ticket = zeroize::Zeroizing::new(std::mem::take(&mut request.ticket.0));
    state.secrets.refresh_redaction(&state.db_pool).await?;
    let (row, secret, version, token_id, user, ticket_expiry) = {
        let mut conn = state.conn()?;
        conn.transaction::<_, AppError, _>(|conn| {
            let row = load(conn, id)?;
            let verified = proxy(&state, conn, &headers, &row)?;
            let secret = policy(&state, conn, &row, verified.user_id)?;
            let ticket_expiry = diesel::update(
                tickets::table
                    .filter(tickets::token_hash.eq(crate::jwt::hash_token(&ticket)))
                    .filter(tickets::binding_id.eq(id))
                    .filter(tickets::proxy_token_id.eq(verified.token.id))
                    .filter(tickets::consumed_at.is_null())
                    .filter(tickets::expires_at.gt(Utc::now())),
            )
            .set(tickets::consumed_at.eq(Some(Utc::now())))
            .returning(tickets::expires_at)
            .get_result::<chrono::DateTime<Utc>>(conn)
            .optional()?
            .ok_or(AppError::Forbidden)?;
            let version = secret_versions::table
                .find(row.version_id)
                .first::<VersionRow>(conn)?;
            store::audit(
                conn,
                verified.user_id,
                Some(row.secret_id),
                "binding_redemption_claimed",
                "allowed",
            )?;
            Ok((
                row,
                secret,
                version,
                verified.token.id,
                verified.user_id,
                ticket_expiry,
            ))
        })?
    };
    let value = state
        .secrets
        .crypto()?
        .open(
            Identity {
                organization: secret.organization_id,
                secret: secret.id,
                version: version.id,
            },
            &version.envelope(),
        )
        .await
        .map_err(crypto_error)?;
    {
        let mut conn = state.conn()?;
        if ticket_expiry <= Utc::now() {
            return Err(AppError::Forbidden);
        }
        let current = load(&mut conn, id)?;
        let verified = proxy(&state, &mut conn, &headers, &current)?;
        if verified.token.id != token_id {
            return Err(AppError::Forbidden);
        };
        policy(&state, &mut conn, &current, user)?;
    }
    let added = {
        let mut conn = state.conn()?;
        register_reader(&mut conn, &secret, &version, user)?
    };
    if added {
        state.secrets.refresh_redaction(&state.db_pool).await?;
    }
    {
        let mut conn = state.conn()?;
        if ticket_expiry <= Utc::now() {
            return Err(AppError::Forbidden);
        }
        let current = load(&mut conn, id)?;
        let verified = proxy(&state, &mut conn, &headers, &current)?;
        if verified.token.id != token_id {
            return Err(AppError::Forbidden);
        };
        policy(&state, &mut conn, &current, user)?;
        state.secrets.redaction.snapshot(&mut conn)?;
        store::audit(
            &mut conn,
            user,
            Some(secret.id),
            "binding_redeemed",
            "allowed",
        )?;
    }
    Ok(Json(SecretBindingRedeemResponse {
        value: SecretValue(
            String::from_utf8(value.bytes().to_vec())
                .map_err(|_| AppError::ServiceUnavailable("Invalid secret encoding"))?,
        ),
        env_name: row.env_name,
        version: version.ordinal,
    }))
}

#[cfg(test)]
mod tests {
    use super::super::tests::{credential, request, Database};
    use super::*;
    use crate::handlers::websocket::{conn_channel, LauncherConnection};
    fn proxy_token(state: &AppState, user: Uuid, session: Option<Uuid>) -> String {
        let issued = crate::handlers::proxy_tokens::issue_proxy_token(
            &mut state.conn().unwrap(),
            state.jwt_secret.as_bytes(),
            user,
            crate::handlers::proxy_tokens::TokenPersist::Create {
                name: crate::handlers::proxy_tokens::LAUNCH_TOKEN_NAME,
            },
            Some(1),
        )
        .unwrap();
        if let Some(session) = session {
            crate::handlers::proxy_tokens::link_token_to_session(
                &mut state.conn().unwrap(),
                &issued.token,
                session,
            )
        };
        issued.token
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn approved_binding_is_process_bound_single_use_and_revocation_safe() {
        let Some(db) = Database::new() else { return };
        let mut state = crate::test_support::test_app_state(db.pool.clone());
        state.secrets = Arc::new(crate::secrets::SecretsService::new(Some(
            crate::secrets::crypto::EnvelopeCrypto {
                key: Arc::new(zeroize::Zeroizing::new(vec![4; 32])),
            },
        )));
        let state = Arc::new(state);
        let owner =
            crate::test_support::insert_user(&mut state.conn().unwrap(), "binding_owner").id;
        let other =
            crate::test_support::insert_user(&mut state.conn().unwrap(), "binding_other").id;
        let launcher = Uuid::new_v4();
        let (sender, _launcher_rx) = conn_channel(8);
        state
            .session_manager
            .try_register_launcher(
                launcher,
                LauncherConnection {
                    log_span: tracing::Span::none(),
                    sender,
                    launcher_name: "test".into(),
                    hostname: "test".into(),
                    user_id: owner,
                    running_sessions: vec![],
                    working_directory: None,
                    version: "test".into(),
                    capabilities: vec![],
                    cancel: tokio_util::sync::CancellationToken::new(),
                    gen: 0,
                    last_seen: std::sync::atomic::AtomicU64::new(0),
                    system: None,
                },
            )
            .unwrap();
        let session =
            crate::test_support::insert_session(&mut state.conn().unwrap(), owner, "binding");
        diesel::update(sessions::table.find(session.id))
            .set(sessions::launcher_id.eq(Some(launcher)))
            .execute(&mut state.conn().unwrap())
            .unwrap();
        let (sender, _proxy_rx) = conn_channel(8);
        state.session_manager.register_session(
            session.session_key.clone().into(),
            sender,
            tokio_util::sync::CancellationToken::new(),
        );
        let different =
            crate::test_support::insert_session(&mut state.conn().unwrap(), owner, "different");
        let human = credential(&state, owner, "human");
        let unrelated = credential(&state, other, "unrelated");
        let normal = proxy_token(&state, owner, None);
        let mut bound = proxy_token(&state, owner, Some(session.id));
        let wrong = proxy_token(&state, owner, Some(different.id));
        let router = super::super::router(state.clone())
            .with_state(state.clone())
            .layer(tower_cookies::CookieManagerLayer::new());
        let (status, secret) = request(
            &router,
            &human,
            "POST",
            "/api/secrets",
            serde_json::json!({"namespace":"personal","name":"BOUND","value":"bound-value"}),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "{secret}");
        let secret_id = secret["id"].as_str().unwrap();
        let version = secret_versions::table
            .filter(secret_versions::secret_id.eq(Uuid::parse_str(secret_id).unwrap()))
            .select(secret_versions::id)
            .first::<Uuid>(&mut state.conn().unwrap())
            .unwrap();
        let unpinned = diesel::insert_into(bindings::table)
            .values((
                bindings::id.eq(Uuid::new_v4()),
                bindings::secret_id.eq(Uuid::parse_str(secret_id).unwrap()),
                bindings::session_id.eq(session.id),
                bindings::launcher_id.eq(launcher),
                bindings::created_by.eq(owner),
                bindings::operation.eq("env"),
                bindings::expires_at.eq(Utc::now() + chrono::Duration::seconds(60)),
                bindings::generation.eq(1i64),
                bindings::version_id.eq(version),
                bindings::env_name.eq("APP_TOKEN"),
            ))
            .execute(&mut state.conn().unwrap());
        assert!(
            unpinned.is_err(),
            "Database must reject an active approval without a process pin"
        );

        let body = serde_json::json!({"secret_id":secret_id,"session_id":session.id,"launcher_id":launcher,"version":null,"env_name":"APP_TOKEN","expected_generation":1,"expires_in_seconds":60});
        let cookies = tower_cookies::Cookies::default();
        cookies
            .signed(&state.cookie_key)
            .add(tower_cookies::Cookie::new(
                shared::protocol::SESSION_COOKIE_NAME,
                owner.to_string(),
            ));
        let browser_request = |cookies: &tower_cookies::Cookies| {
            let cookie = cookies
                .list()
                .iter()
                .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
                .collect::<Vec<_>>()
                .join("; ");
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/secrets/bindings")
                .header("cookie", cookie)
                .header("origin", "http://localhost:3000")
                .header("x-portal-secrets", "1")
                .header("idempotency-key", Uuid::new_v4().to_string())
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap()
        };
        use tower::ServiceExt;
        let response = router
            .clone()
            .oneshot(browser_request(&cookies))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::FORBIDDEN,
            "An old browser cookie cannot approve saved-value execution"
        );
        auth::record_recent_auth(&cookies, &state, owner);
        let response = router
            .clone()
            .oneshot(browser_request(&cookies))
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "A fresh authenticated browser can explicitly approve execution"
        );
        for unauthorized in [&bound, &normal, &unrelated] {
            let (status, _) = request(
                &router,
                unauthorized,
                "POST",
                "/api/secrets/bindings",
                body.clone(),
                Uuid::new_v4(),
            )
            .await;
            assert!(!status.is_success());
        }
        let key = Uuid::new_v4();
        let (status, binding) = request(
            &router,
            &human,
            "POST",
            "/api/secrets/bindings",
            body.clone(),
            key,
        )
        .await;
        assert!(status.is_success(), "{binding}");
        assert!(!binding.to_string().contains("bound-value"));
        let mut binding_id = binding["id"].as_str().unwrap().to_owned();
        let (status, replayed) =
            request(&router, &human, "POST", "/api/secrets/bindings", body, key).await;
        assert!(status.is_success());
        assert_eq!(binding["id"], replayed["id"]);
        let mut ticket_path = format!("/api/secrets/bindings/{binding_id}/ticket");
        let mut redeem_path = format!("/api/secrets/bindings/{binding_id}/redeem");
        for unauthorized in [&human, &normal, &wrong] {
            let (status, _) = request(
                &router,
                unauthorized,
                "POST",
                &ticket_path,
                serde_json::Value::Null,
                Uuid::new_v4(),
            )
            .await;
            assert!(!status.is_success())
        }
        let (_, expired) = request(
            &router,
            &bound,
            "POST",
            &ticket_path,
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        diesel::update(tickets::table.filter(
            tickets::token_hash.eq(crate::jwt::hash_token(expired["ticket"].as_str().unwrap())),
        ))
        .set(tickets::expires_at.eq(Utc::now() - chrono::Duration::seconds(1)))
        .execute(&mut state.conn().unwrap())
        .unwrap();
        let (expired_status, _) = request(
            &router,
            &bound,
            "POST",
            &redeem_path,
            serde_json::json!({"ticket":expired["ticket"]}),
            Uuid::new_v4(),
        )
        .await;
        assert_eq!(expired_status, axum::http::StatusCode::FORBIDDEN);
        let (status, ticket) = request(
            &router,
            &bound,
            "POST",
            &ticket_path,
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "{ticket}");
        let redeem = serde_json::json!({"ticket":ticket["ticket"]});
        let (status, _) = request(
            &router,
            &wrong,
            "POST",
            &redeem_path,
            redeem.clone(),
            Uuid::new_v4(),
        )
        .await;
        assert!(!status.is_success());
        let (status, value) = request(
            &router,
            &bound,
            "POST",
            &redeem_path,
            redeem.clone(),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "{value}");
        assert_eq!(value["value"], "bound-value");
        assert_eq!(value["env_name"], "APP_TOKEN");
        assert_eq!(value["version"], 1);
        let (status, _) = request(
            &router,
            &bound,
            "POST",
            &redeem_path,
            redeem,
            Uuid::new_v4(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
        // The same session's replacement proxy credential is a distinct
        // process audience and cannot redeem a predecessor's outstanding ticket.
        let (_, previous_process) = request(
            &router,
            &bound,
            "POST",
            &ticket_path,
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        let previous_token = bound.clone();
        bound = proxy_token(&state, owner, Some(session.id));
        // Even a missed revocation cannot authorize a superseded process.
        diesel::update(
            crate::schema::proxy_auth_tokens::table.filter(
                crate::schema::proxy_auth_tokens::token_hash
                    .eq(crate::jwt::hash_token(&previous_token)),
            ),
        )
        .set(crate::schema::proxy_auth_tokens::revoked.eq(false))
        .execute(&mut state.conn().unwrap())
        .unwrap();
        for (path, body) in [
            (&ticket_path, serde_json::Value::Null),
            (
                &redeem_path,
                serde_json::json!({"ticket":previous_process["ticket"]}),
            ),
        ] {
            let (status, _) =
                request(&router, &previous_token, "POST", path, body, Uuid::new_v4()).await;
            assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
        }

        let (status, _) = request(
            &router,
            &bound,
            "POST",
            &redeem_path,
            serde_json::json!({"ticket":previous_process["ticket"]}),
            Uuid::new_v4(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
        let (status, _) = request(
            &router,
            &bound,
            "POST",
            &ticket_path,
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        assert_eq!(
            status,
            axum::http::StatusCode::FORBIDDEN,
            "New processes need a fresh human binding approval"
        );
        let (status,renewed)=request(&router,&human,"POST","/api/secrets/bindings",serde_json::json!({"secret_id":secret_id,"session_id":session.id,"launcher_id":launcher,"version":null,"env_name":"APP_TOKEN","expected_generation":1,"expires_in_seconds":60}),Uuid::new_v4()).await;
        assert!(status.is_success(), "{renewed}");
        binding_id = renewed["id"].as_str().unwrap().to_owned();
        ticket_path = format!("/api/secrets/bindings/{binding_id}/ticket");
        redeem_path = format!("/api/secrets/bindings/{binding_id}/redeem");
        let (_, single) = request(
            &router,
            &bound,
            "POST",
            &ticket_path,
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        let body = serde_json::json!({"ticket":single["ticket"]});
        let left = request(
            &router,
            &bound,
            "POST",
            &redeem_path,
            body.clone(),
            Uuid::new_v4(),
        );
        let right = request(&router, &bound, "POST", &redeem_path, body, Uuid::new_v4());
        let (left, right) = tokio::join!(left, right);
        assert_eq!(
            usize::from(left.0.is_success()) + usize::from(right.0.is_success()),
            1
        );
        let (_, old_ticket) = request(
            &router,
            &bound,
            "POST",
            &ticket_path,
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        let (status, _) = request(
            &router,
            &human,
            "POST",
            &format!("/api/secrets/{secret_id}/versions"),
            serde_json::json!({"expected_generation":1,"value":"replacement-value"}),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success());
        let (status, _) = request(
            &router,
            &bound,
            "POST",
            &redeem_path,
            serde_json::json!({"ticket":old_ticket["ticket"]}),
            Uuid::new_v4(),
        )
        .await;
        assert!(!status.is_success());
        let (status, _) = request(
            &router,
            &human,
            "POST",
            &format!("/api/secrets/bindings/{binding_id}/revoke"),
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success());
        let (status, _) = request(
            &router,
            &bound,
            "POST",
            &ticket_path,
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        assert!(!status.is_success());
        // An explicitly approved historical version remains pinned; it never
        // silently switches to the current replacement value.
        let (status,historical)=request(&router,&human,"POST","/api/secrets/bindings",serde_json::json!({"secret_id":secret_id,"session_id":session.id,"launcher_id":launcher,"version":1,"env_name":"OLD_TOKEN","expected_generation":2,"expires_in_seconds":60}),Uuid::new_v4()).await;
        assert!(status.is_success(), "{historical}");
        let historical_id = historical["id"].as_str().unwrap();
        let ticket_path = format!("/api/secrets/bindings/{historical_id}/ticket");
        let redeem_path = format!("/api/secrets/bindings/{historical_id}/redeem");
        let (_, ticket) = request(
            &router,
            &bound,
            "POST",
            &ticket_path,
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        let (status, old) = request(
            &router,
            &bound,
            "POST",
            &redeem_path,
            serde_json::json!({"ticket":ticket["ticket"]}),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success());
        assert_eq!(old["value"], "bound-value");
        assert_eq!(old["version"], 1);
        let (_, before_delete) = request(
            &router,
            &bound,
            "POST",
            &ticket_path,
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        let (status, _) = request(
            &router,
            &human,
            "POST",
            &format!("/api/secrets/{secret_id}/delete"),
            serde_json::json!({"expected_generation":2}),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success());
        let (status, _) = request(
            &router,
            &human,
            "POST",
            &format!("/api/secrets/{secret_id}/restore"),
            serde_json::json!({"expected_generation":3,"confirm_sharing":true}),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success());
        let (status, _) = request(
            &router,
            &bound,
            "POST",
            &redeem_path,
            serde_json::json!({"ticket":before_delete["ticket"]}),
            Uuid::new_v4(),
        )
        .await;
        assert!(!status.is_success());
    }
}
