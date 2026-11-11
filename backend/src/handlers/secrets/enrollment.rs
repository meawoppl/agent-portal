//! One-shot, browser-approved dedicated CLI vault enrollment. Existing owner
//! tokens may ask for consent, but cannot mint or redeem saved-value access.
use super::*;
use crate::schema::{secret_credentials as credentials, secret_enrollments as enrollments};
use tower_cookies::Cookies;
fn random_token(prefix: &str) -> Result<String, AppError> {
    let mut bytes = [0; 32];
    openssl::rand::rand_bytes(&mut bytes)
        .map_err(|_| AppError::Internal("Secure random unavailable".into()))?;
    Ok(format!("{prefix}{}", hex::encode(bytes)))
}
async fn start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    SecretJson(request): SecretJson<SecretAuthorizeStartRequest>,
) -> Result<Json<SecretAuthorizeStartResponse>, AppError> {
    state.secrets.crypto()?;
    let token = crate::auth::bearer_token(&headers).ok_or(AppError::Unauthorized)?;
    if token.starts_with(crate::handlers::federation::LINKED_TOKEN_PREFIX)
        || token.starts_with("aps_secret_")
    {
        return Err(AppError::Forbidden);
    };
    let mut conn = state.conn()?;
    let verified =
        crate::handlers::proxy_tokens::verify_and_get_user_with_token(&state, &mut conn, token)?;
    if verified.token.session_id.is_some()
        || verified.token.name == crate::handlers::proxy_tokens::LAUNCH_TOKEN_NAME
    {
        return Err(AppError::Forbidden);
    };
    let user = store::active_user(&mut conn, verified.user_id)?;
    if request.label.trim().is_empty() || request.label.len() > 128 {
        return Err(AppError::BadRequest(
            "Credential label must contain 1 to 128 characters",
        ));
    };
    let count = enrollments::table
        .filter(enrollments::user_id.eq(user.id))
        .filter(enrollments::created_at.gt(Utc::now() - chrono::Duration::hours(1)))
        .count()
        .get_result::<i64>(&mut conn)?;
    if count >= 12 {
        return Err(AppError::ServiceUnavailable(
            "Enrollment rate limit reached",
        ));
    };
    let device = random_token("aps_device_")?;
    let user_code = random_token("")?[..12].to_uppercase();
    let expires = Utc::now() + chrono::Duration::minutes(10);
    diesel::insert_into(enrollments::table)
        .values((
            enrollments::id.eq(Uuid::new_v4()),
            enrollments::user_id.eq(user.id),
            enrollments::device_hash.eq(crate::jwt::hash_token(&device)),
            enrollments::user_code.eq(&user_code),
            enrollments::label.eq(request.label),
            enrollments::expires_at.eq(expires),
        ))
        .execute(&mut conn)?;
    store::audit(&mut conn, user.id, None, "enrollment_start", "pending")?;
    Ok(Json(SecretAuthorizeStartResponse {
        device_code: SecretValue(device),
        user_code: user_code.clone(),
        verification_uri: format!(
            "{}/secrets/authorize?code={}",
            state.public_url.trim_end_matches('/'),
            user_code
        ),
        expires_in: 600,
        interval: 3,
    }))
}
async fn decide(
    state: Arc<AppState>,
    user: Uuid,
    headers: HeaderMap,
    cookies: Cookies,
    request: SecretAuthorizeApproveRequest,
    approved: bool,
) -> Result<Json<SecretAuthorizePollResponse>, AppError> {
    auth::csrf(&state, &headers)?;
    auth::require_recent_auth(&cookies, &state, user)?;
    let mut conn = state.conn()?;
    store::active_user(&mut conn, user)?;
    let changed = diesel::update(
        enrollments::table
            .filter(enrollments::user_code.eq(request.user_code.to_uppercase()))
            .filter(enrollments::user_id.eq(user))
            .filter(enrollments::state.eq("pending"))
            .filter(enrollments::expires_at.gt(Utc::now())),
    )
    .set(enrollments::state.eq(if approved { "approved" } else { "denied" }))
    .execute(&mut conn)?;
    if changed != 1 {
        return Err(AppError::NotFound(
            "Pending approval not found for this account",
        ));
    };
    store::audit(
        &mut conn,
        user,
        None,
        "enrollment_approval",
        if approved { "allowed" } else { "denied" },
    )?;
    Ok(Json(SecretAuthorizePollResponse {
        state: if approved {
            SecretAuthorizeState::Approved
        } else {
            SecretAuthorizeState::Denied
        },
        credential: None,
        expires_at: None,
    }))
}
async fn approve(
    State(state): State<Arc<AppState>>,
    crate::auth::BrowserUserId(user): crate::auth::BrowserUserId,
    headers: HeaderMap,
    cookies: Cookies,
    SecretJson(request): SecretJson<SecretAuthorizeApproveRequest>,
) -> Result<Json<SecretAuthorizePollResponse>, AppError> {
    decide(state, user, headers, cookies, request, true).await
}
async fn deny(
    State(state): State<Arc<AppState>>,
    crate::auth::BrowserUserId(user): crate::auth::BrowserUserId,
    headers: HeaderMap,
    cookies: Cookies,
    SecretJson(request): SecretJson<SecretAuthorizeApproveRequest>,
) -> Result<Json<SecretAuthorizePollResponse>, AppError> {
    decide(state, user, headers, cookies, request, false).await
}
async fn poll(
    State(state): State<Arc<AppState>>,
    SecretJson(mut request): SecretJson<SecretAuthorizePollRequest>,
) -> Result<Json<SecretAuthorizePollResponse>, AppError> {
    let device = zeroize::Zeroizing::new(std::mem::take(&mut request.device_code.0));
    let mut conn = state.conn()?;
    conn.transaction::<_, AppError, _>(|conn| {
        let row = enrollments::table
            .filter(enrollments::device_hash.eq(crate::jwt::hash_token(&device)))
            .for_update()
            .select((
                enrollments::id,
                enrollments::user_id,
                enrollments::state,
                enrollments::expires_at,
                enrollments::label,
            ))
            .first::<(Uuid, Uuid, String, chrono::DateTime<Utc>, String)>(conn)
            .map_err(|_| AppError::Unauthorized)?;
        let mut response = SecretAuthorizePollResponse {
            state: SecretAuthorizeState::Pending,
            credential: None,
            expires_at: None,
        };
        if row.3 <= Utc::now() {
            response.state = SecretAuthorizeState::Expired;
            return Ok(Json(response));
        };
        store::active_user(conn, row.1)?;
        match row.2.as_str() {
            "pending" => {}
            "approved" => {
                let credential = random_token("aps_secret_")?;
                let expires = Utc::now() + chrono::Duration::days(30);
                diesel::insert_into(credentials::table)
                    .values((
                        credentials::id.eq(Uuid::new_v4()),
                        credentials::user_id.eq(row.1),
                        credentials::token_hash.eq(crate::jwt::hash_token(&credential)),
                        credentials::label.eq(row.4),
                        credentials::metadata_scope.eq(true),
                        credentials::mutation_scope.eq(true),
                        credentials::read_scope.eq(true),
                        credentials::expires_at.eq(expires),
                    ))
                    .execute(conn)?;
                diesel::update(enrollments::table.find(row.0))
                    .set(enrollments::state.eq("consumed"))
                    .execute(conn)?;
                store::audit(conn, row.1, None, "credential_issued", "allowed")?;
                response.state = SecretAuthorizeState::Approved;
                response.credential = Some(SecretValue(credential));
                response.expires_at = Some(expires.to_rfc3339());
            }
            "denied" => response.state = SecretAuthorizeState::Denied,
            _ => response.state = SecretAuthorizeState::Expired,
        };
        Ok(Json(response))
    })
}
pub(super) fn router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/api/secrets/authorize/start", post(start))
        .route("/api/secrets/authorize/approve", post(approve))
        .route("/api/secrets/authorize/deny", post(deny))
        .route("/api/secrets/authorize/poll", post(poll))
        .route("/api/secrets/authorize/credentials", get(credentials_list))
        .route(
            "/api/secrets/authorize/credentials/{id}/revoke",
            post(credentials_revoke),
        )
}

async fn credentials_list(
    State(state): State<Arc<AppState>>,
    crate::auth::BrowserUserId(user): crate::auth::BrowserUserId,
) -> Result<Json<SecretCredentialsResponse>, AppError> {
    let mut conn = state.conn()?;
    let rows = credentials::table
        .filter(credentials::user_id.eq(user))
        .order(credentials::created_at.desc())
        .select((
            credentials::id,
            credentials::label,
            credentials::created_at,
            credentials::expires_at,
            credentials::revoked_at,
        ))
        .load::<(
            Uuid,
            String,
            chrono::DateTime<Utc>,
            chrono::DateTime<Utc>,
            Option<chrono::DateTime<Utc>>,
        )>(&mut conn)?;
    Ok(Json(SecretCredentialsResponse {
        credentials: rows
            .into_iter()
            .map(
                |(id, label, created_at, expires_at, revoked_at)| SecretCredentialMetadata {
                    id,
                    label,
                    created_at: created_at.to_rfc3339(),
                    expires_at: expires_at.to_rfc3339(),
                    revoked_at: revoked_at.map(|time| time.to_rfc3339()),
                },
            )
            .collect(),
    }))
}
async fn credentials_revoke(
    State(state): State<Arc<AppState>>,
    crate::auth::BrowserUserId(user): crate::auth::BrowserUserId,
    headers: HeaderMap,
    cookies: Cookies,
    Path(id): Path<Uuid>,
) -> Result<Json<SecretCredentialsResponse>, AppError> {
    auth::csrf(&state, &headers)?;
    auth::require_recent_auth(&cookies, &state, user)?;
    {
        let mut conn = state.conn()?;
        conn.transaction::<_, AppError, _>(|conn| {
            let changed = diesel::update(
                credentials::table
                    .find(id)
                    .filter(credentials::user_id.eq(user)),
            )
            .set(credentials::revoked_at.eq(Some(Utc::now())))
            .execute(conn)?;
            if changed == 0 {
                return Err(AppError::NotFound("Credential not found"));
            };
            store::audit(conn, user, None, "credential_revoked", "allowed")?;
            Ok(())
        })?;
    }
    credentials_list(State(state), crate::auth::BrowserUserId(user)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn approval_is_recent_browser_user_bound_and_poll_is_single_use() {
        let Some(db) = super::super::tests::Database::new() else {
            return;
        };
        let mut state = crate::test_support::test_app_state(db.pool.clone());
        state.secrets = Arc::new(crate::secrets::SecretsService::new(Some(
            crate::secrets::crypto::EnvelopeCrypto {
                key: Arc::new(zeroize::Zeroizing::new(vec![5; 32])),
            },
        )));
        let state = Arc::new(state);
        let owner = crate::test_support::insert_user(&mut state.conn().unwrap(), "enroll").id;
        let other = crate::test_support::insert_user(&mut state.conn().unwrap(), "unrelated").id;
        let old = crate::handlers::proxy_tokens::issue_proxy_token(
            &mut state.conn().unwrap(),
            state.jwt_secret.as_bytes(),
            owner,
            crate::handlers::proxy_tokens::TokenPersist::Create { name: "human CLI" },
            Some(1),
        )
        .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", old.token).parse().unwrap(),
        );
        assert!(auth::credential(&state, &headers).is_err());
        let challenge = start(
            State(state.clone()),
            headers,
            SecretJson(SecretAuthorizeStartRequest {
                label: "Laptop".into(),
            }),
        )
        .await
        .unwrap()
        .0;
        let mut browser_headers = HeaderMap::new();
        browser_headers.insert("origin", "http://localhost:3000".parse().unwrap());
        browser_headers.insert("x-portal-secrets", "1".parse().unwrap());
        let request = SecretAuthorizeApproveRequest {
            user_code: challenge.user_code.clone(),
        };
        let cookies = Cookies::default();
        assert!(decide(
            state.clone(),
            owner,
            browser_headers.clone(),
            cookies.clone(),
            request.clone(),
            true
        )
        .await
        .is_err());
        auth::record_recent_auth(&cookies, &state, other);
        assert!(decide(
            state.clone(),
            other,
            browser_headers.clone(),
            cookies.clone(),
            request.clone(),
            true
        )
        .await
        .is_err());
        auth::record_recent_auth(&cookies, &state, owner);
        let _approved = decide(
            state.clone(),
            owner,
            browser_headers,
            cookies,
            request,
            true,
        )
        .await
        .unwrap();
        let first = poll(
            State(state.clone()),
            SecretJson(SecretAuthorizePollRequest {
                device_code: challenge.device_code.clone(),
            }),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(first.state, SecretAuthorizeState::Approved);
        let credential = first.credential.unwrap();
        let second = poll(
            State(state.clone()),
            SecretJson(SecretAuthorizePollRequest {
                device_code: challenge.device_code,
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(second.credential.is_none());
        assert_eq!(second.state, SecretAuthorizeState::Expired);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", credential.0).parse().unwrap(),
        );
        assert_eq!(
            auth::credential(&state, &headers).unwrap().unwrap().user,
            owner
        );
        let id = credentials::table
            .filter(credentials::user_id.eq(owner))
            .select(credentials::id)
            .first::<Uuid>(&mut state.conn().unwrap())
            .unwrap();
        let cookies = Cookies::default();
        auth::record_recent_auth(&cookies, &state, owner);
        let mut browser = HeaderMap::new();
        browser.insert("origin", "http://localhost:3000".parse().unwrap());
        browser.insert("x-portal-secrets", "1".parse().unwrap());
        let _revoked = credentials_revoke(
            State(state.clone()),
            crate::auth::BrowserUserId(owner),
            browser,
            cookies,
            Path(id),
        )
        .await
        .unwrap();
        assert!(auth::credential(&state, &headers).is_err());
    }
}
