//! Device approval for linked portals. Approval requires a native browser
//! session and an origin-bound CSRF nonce; polling can only consume once.
use super::{credentials, load_policy, Grant};
use crate::{
    errors::AppError,
    schema::{
        linked_client_enrollments as enrollments, linked_client_grants as grants, portal_instance,
        users,
    },
    AppState,
};
use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    Form, Json,
};
use diesel::prelude::*;
use serde::Deserialize;
use shared::api::{
    DeviceCodeResponse, DeviceFlowPollRequest, FederationPolicy,
    LinkedClientDenialReason as Reason, LinkedClientEnrollmentRequest,
    LinkedClientPollResponse as Poll, TokenScope,
};
use std::sync::Arc;
use tower_cookies::{
    cookie::{time::Duration, SameSite},
    Cookie, Cookies,
};
use uuid::Uuid;

const CSRF_COOKIE: &str = "linked_approval_csrf";
#[derive(Queryable, Selectable)]
#[diesel(table_name = enrollments)]
struct Enrollment {
    device_code_hash: String,
    user_code: String,
    client_origin: String,
    requested_scope: String,
    device_label: Option<String>,
    #[allow(dead_code)]
    created_at: chrono::NaiveDateTime,
    expires_at: chrono::NaiveDateTime,
    status: String,
    user_id: Option<Uuid>,
    denial_reason: Option<String>,
}

fn origin(headers: &HeaderMap) -> Result<String, AppError> {
    headers
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| shared::urls::canonical_origin(v).ok())
        .ok_or(AppError::Forbidden)
}
fn denial(policy: &FederationPolicy, origin: &str, scope: TokenScope) -> Option<Reason> {
    let cap = match policy {
        FederationPolicy::Off => return Some(Reason::PolicyDisabled),
        FederationPolicy::Any { max_scope } => max_scope,
        FederationPolicy::Allowlist { origins, max_scope } => {
            if !origins.iter().any(|s| s == origin) {
                return Some(Reason::OriginNotAllowed);
            }
            max_scope
        }
    };
    (!cap.permits(scope)).then_some(Reason::ScopeExceedsCap)
}
fn no_store(response: impl IntoResponse) -> Response {
    let mut response = response.into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "private, no-store".parse().unwrap());
    response
}

pub async fn start(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<LinkedClientEnrollmentRequest>,
) -> Result<Response, AppError> {
    let client_origin = shared::urls::canonical_origin(&request.client_origin)
        .map_err(|_| AppError::BadRequest("Invalid client origin"))?;
    if origin(&headers)? != client_origin {
        return Err(AppError::Forbidden);
    }
    if request
        .device_label
        .as_ref()
        .is_some_and(|s| s.len() > 128 || s.chars().any(char::is_control))
    {
        return Err(AppError::BadRequest("Invalid device label"));
    }
    let mut conn = state.conn()?;
    if let Some(reason) = denial(
        &load_policy(&mut conn)?,
        &client_origin,
        request.requested_scope,
    ) {
        return Ok(no_store((
            StatusCode::FORBIDDEN,
            Json(Poll::Denied { reason }),
        )));
    }
    let code = credentials::random_secret();
    let user_code = format!("LP-{}", &credentials::random_secret()[..12].to_uppercase());
    let now = chrono::Utc::now().naive_utc();
    // Bound abandoned enrollment storage as well as request frequency.
    diesel::delete(enrollments::table.filter(enrollments::expires_at.lt(now)))
        .execute(&mut conn)?;
    diesel::insert_into(enrollments::table)
        .values((
            enrollments::device_code_hash.eq(crate::jwt::hash_token(&code)),
            enrollments::user_code.eq(&user_code),
            enrollments::client_origin.eq(&client_origin),
            enrollments::requested_scope.eq(request.requested_scope.as_str()),
            enrollments::device_label.eq(request.device_label),
            enrollments::expires_at.eq(now + chrono::Duration::minutes(10)),
        ))
        .execute(&mut conn)?;
    let portal = shared::urls::canonical_origin(&state.public_url)
        .map_err(|_| AppError::ServiceUnavailable("Invalid portal origin"))?;
    Ok(no_store(Json(DeviceCodeResponse {
        device_code: code,
        verification_uri: format!("{portal}/api/auth/linked?user_code={user_code}"),
        user_code,
        expires_in: 600,
        interval: 5,
    })))
}

pub async fn poll(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<DeviceFlowPollRequest>,
) -> Result<Response, AppError> {
    if request.device_code.len() != 64 {
        return Ok(no_store(Json(Poll::Expired)));
    }
    let client_origin = origin(&headers)?;
    let mut conn = state.conn()?;
    let result = conn.transaction::<_, AppError, _>(|conn| {
        let Some(row) = enrollments::table
            .find(crate::jwt::hash_token(&request.device_code))
            .for_update()
            .first::<Enrollment>(conn)
            .optional()?
        else {
            return Ok(Poll::Expired);
        };
        if row.client_origin != client_origin {
            return Err(AppError::Forbidden);
        }
        let now = chrono::Utc::now().naive_utc();
        if row.expires_at <= now || row.status == "consumed" {
            return Ok(Poll::Expired);
        }
        let scope = TokenScope::parse(&row.requested_scope).ok_or(AppError::Forbidden)?;
        if let Some(reason) = denial(&load_policy(conn)?, &client_origin, scope) {
            return Ok(Poll::Denied { reason });
        }
        match row.status.as_str() {
            "pending" => return Ok(Poll::Pending),
            "denied" => {
                return Ok(Poll::Denied {
                    reason: if row.denial_reason.as_deref() == Some("user_denied") {
                        Reason::UserDenied
                    } else {
                        Reason::Unknown
                    },
                })
            }
            "approved" => {}
            _ => return Err(AppError::Forbidden),
        }
        let user_id = row.user_id.ok_or(AppError::Forbidden)?;
        if users::table
            .find(user_id)
            .select(users::disabled)
            .first::<bool>(conn)?
        {
            return Err(AppError::Forbidden);
        }
        let grant = diesel::insert_into(grants::table)
            .values((
                grants::user_id.eq(user_id),
                grants::client_origin.eq(&client_origin),
                grants::scope.eq(scope.as_str()),
                grants::device_label.eq(row.device_label),
                grants::expires_at.eq(now + chrono::Duration::days(30)),
            ))
            .get_result::<Grant>(conn)?;
        let token = credentials::mint(conn, &grant)?;
        diesel::update(enrollments::table.find(row.device_code_hash))
            .set(enrollments::status.eq("consumed"))
            .execute(conn)?;
        let instance_id = portal_instance::table
            .select(portal_instance::instance_id)
            .first(conn)?;
        Ok(Poll::Complete {
            access_token: token.access_token,
            expires_at: token.expires_at,
            portal_origin: shared::urls::canonical_origin(&state.public_url)
                .map_err(|_| AppError::ServiceUnavailable("Invalid portal origin"))?,
            instance_id,
            grant: Box::new(credentials::wire_grant(conn, grant)?),
        })
    })?;
    Ok(no_store(Json(result)))
}

#[derive(Deserialize)]
pub struct ApprovalQuery {
    user_code: String,
}
#[derive(Deserialize)]
pub struct ApprovalForm {
    user_code: String,
    csrf: String,
    decision: String,
}
fn valid_code(code: &str) -> bool {
    code.len() == 15 && code.starts_with("LP-") && code[3..].bytes().all(|b| b.is_ascii_hexdigit())
}

pub async fn approval(
    State(state): State<Arc<AppState>>,
    cookies: Cookies,
    Query(query): Query<ApprovalQuery>,
) -> Result<Response, AppError> {
    if !valid_code(&query.user_code) {
        return Err(AppError::BadRequest("Invalid approval code"));
    }
    let user = match crate::auth::extract_user(&state, None, &cookies) {
        Ok(user) => user,
        Err(AppError::Unauthorized) => {
            return Ok(no_store(Redirect::temporary(&format!(
                "/api/auth/device/login?device_user_code={}",
                query.user_code
            ))))
        }
        Err(error) => return Err(error),
    };
    let mut conn = state.conn()?;
    let row = enrollments::table
        .filter(enrollments::user_code.eq(&query.user_code))
        .first::<Enrollment>(&mut conn)
        .optional()?
        .ok_or(AppError::NotFound("Approval expired"))?;
    if row.status != "pending" || row.expires_at <= chrono::Utc::now().naive_utc() {
        return Err(AppError::Conflict("Approval is no longer pending"));
    }
    let scope = TokenScope::parse(&row.requested_scope).ok_or(AppError::Forbidden)?;
    if denial(&load_policy(&mut conn)?, &row.client_origin, scope).is_some() {
        return Err(AppError::Forbidden);
    }
    let nonce = credentials::random_secret();
    let mut cookie = Cookie::new(
        CSRF_COOKIE,
        format!("{}:{}:{nonce}", user.id, row.user_code),
    );
    cookie.set_path("/api/auth/linked");
    cookie.set_http_only(true);
    cookie.set_secure(state.public_url.starts_with("https://"));
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(Duration::minutes(10));
    cookies.signed(&state.cookie_key).add(cookie);
    let escape = crate::handlers::helpers::escape_html_text;
    let page = format!(
        r#"<!doctype html><html><head><meta name="viewport" content="width=device-width,initial-scale=1"><title>Link a portal</title></head><body><main><h1>Link a portal</h1><p>Signed in as <strong>{}</strong>.</p><p>Allow <strong>{}</strong> to access this account with <strong>{}</strong> permission?</p><p>Device: {}</p><p>The linked portal can read your sessions{}. Only approve a portal you trust. Its operator controls the code that holds this credential.</p><form method="post"><input type="hidden" name="user_code" value="{}"><input type="hidden" name="csrf" value="{}"><button name="decision" value="approve">Approve</button> <button name="decision" value="deny">Deny</button></form></main></body></html>"#,
        escape(&user.email),
        escape(&row.client_origin),
        scope.as_str(),
        escape(row.device_label.as_deref().unwrap_or("Unspecified device")),
        if scope == TokenScope::Write {
            " and send commands using your session permissions"
        } else {
            ""
        },
        row.user_code,
        nonce
    );
    let mut response = no_store(Html(page));
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'"
            .parse()
            .unwrap(),
    );
    response
        .headers_mut()
        .insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    Ok(response)
}

pub async fn decide(
    State(state): State<Arc<AppState>>,
    cookies: Cookies,
    headers: HeaderMap,
    Form(form): Form<ApprovalForm>,
) -> Result<Response, AppError> {
    if !valid_code(&form.user_code)
        || !matches!(form.decision.as_str(), "approve" | "deny")
        || form.csrf.len() != 64
    {
        return Err(AppError::BadRequest("Invalid approval"));
    }
    let portal_origin =
        shared::urls::canonical_origin(&state.public_url).map_err(|_| AppError::Forbidden)?;
    if origin(&headers)? != portal_origin {
        return Err(AppError::Forbidden);
    }
    let user = crate::auth::extract_user(&state, None, &cookies)?;
    let expected = format!("{}:{}:{}", user.id, form.user_code, form.csrf);
    if cookies
        .signed(&state.cookie_key)
        .get(CSRF_COOKIE)
        .is_none_or(|cookie| cookie.value() != expected)
    {
        return Err(AppError::Forbidden);
    }
    let mut conn = state.conn()?;
    conn.transaction::<_, AppError, _>(|conn| {
        let row = enrollments::table
            .filter(enrollments::user_code.eq(&form.user_code))
            .for_update()
            .first::<Enrollment>(conn)
            .optional()?
            .ok_or(AppError::NotFound("Approval expired"))?;
        if row.status != "pending" || row.expires_at <= chrono::Utc::now().naive_utc() {
            return Err(AppError::Conflict("Approval is no longer pending"));
        }
        let scope = TokenScope::parse(&row.requested_scope).ok_or(AppError::Forbidden)?;
        if denial(&load_policy(conn)?, &row.client_origin, scope).is_some() {
            return Err(AppError::Forbidden);
        }
        diesel::update(enrollments::table.find(row.device_code_hash))
            .set((
                enrollments::status.eq(if form.decision == "approve" {
                    "approved"
                } else {
                    "denied"
                }),
                enrollments::user_id.eq(user.id),
                enrollments::denial_reason.eq(if form.decision == "deny" {
                    Some("user_denied")
                } else {
                    None
                }),
            ))
            .execute(conn)?;
        Ok(())
    })?;
    let mut cookie = Cookie::new(CSRF_COOKIE, "");
    cookie.set_path("/api/auth/linked");
    cookies.signed(&state.cookie_key).remove(cookie);
    Ok(no_store(Html("<!doctype html><title>Link decision saved</title><p>Decision saved. You may return to the requesting portal.</p>")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{federation_policy, proxy_auth_tokens};
    use axum::body::to_bytes;
    use serde::de::DeserializeOwned;
    fn headers(value: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::ORIGIN, value.parse().unwrap());
        h
    }
    async fn decode<T: DeserializeOwned>(r: Response) -> T {
        let bytes = to_bytes(r.into_body(), 1_000_000).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    #[test]
    fn denial_reasons_preserve_policy_boundaries() {
        assert_eq!(
            denial(
                &FederationPolicy::Off,
                "https://a.example",
                TokenScope::Read
            ),
            Some(Reason::PolicyDisabled)
        );
        let p = FederationPolicy::Allowlist {
            origins: vec!["https://a.example".into()],
            max_scope: TokenScope::Read,
        };
        assert_eq!(
            denial(&p, "https://b.example", TokenScope::Read),
            Some(Reason::OriginNotAllowed)
        );
        assert_eq!(
            denial(&p, "https://a.example", TokenScope::Write),
            Some(Reason::ScopeExceedsCap)
        );
        assert!(denial(&p, "https://a.example", TokenScope::Read).is_none());
        assert!(!valid_code("LP-<script>bad"));
        assert!(valid_code("LP-012345ABCDEF"));
    }
    #[tokio::test]
    async fn consent_consumption_rotation_and_revoke_keep_the_grant_binding() {
        let _guard = super::super::TEST_POLICY_LOCK.lock().await;
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let state = Arc::new(crate::test_support::test_app_state(pool.clone()));
        let mut conn = pool.get().unwrap();
        let user = crate::test_support::insert_user(&mut conn, "linked-consent");
        let set_policy = |conn: &mut PgConnection, p: FederationPolicy| {
            diesel::update(federation_policy::table)
                .set(federation_policy::policy.eq(serde_json::to_value(p).unwrap()))
                .execute(conn)
                .unwrap();
        };
        let request = || LinkedClientEnrollmentRequest {
            client_origin: "https://home.example".into(),
            requested_scope: TokenScope::Read,
            device_label: Some("Test device".into()),
        };
        set_policy(&mut conn, FederationPolicy::Off);
        let denied = start(
            State(state.clone()),
            headers("https://home.example"),
            Json(request()),
        )
        .await
        .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert!(matches!(
            decode::<Poll>(denied).await,
            Poll::Denied {
                reason: Reason::PolicyDisabled
            }
        ));
        set_policy(
            &mut conn,
            FederationPolicy::Any {
                max_scope: TokenScope::Write,
            },
        );
        assert!(start(
            State(state.clone()),
            headers("https://evil.example"),
            Json(request())
        )
        .await
        .is_err());
        let code: DeviceCodeResponse = decode(
            start(
                State(state.clone()),
                headers("https://home.example"),
                Json(request()),
            )
            .await
            .unwrap(),
        )
        .await;
        let poll_request = || {
            Json(DeviceFlowPollRequest {
                device_code: code.device_code.clone(),
            })
        };
        assert!(matches!(
            decode::<Poll>(
                poll(
                    State(state.clone()),
                    headers("https://home.example"),
                    poll_request()
                )
                .await
                .unwrap()
            )
            .await,
            Poll::Pending
        ));
        assert!(poll(
            State(state.clone()),
            headers("https://evil.example"),
            poll_request()
        )
        .await
        .is_err());
        let cookies = Cookies::default();
        cookies.signed(&state.cookie_key).add(Cookie::new(
            shared::protocol::SESSION_COOKIE_NAME,
            user.id.to_string(),
        ));
        let page = approval(
            State(state.clone()),
            cookies.clone(),
            Query(ApprovalQuery {
                user_code: code.user_code.clone(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let nonce = cookies
            .signed(&state.cookie_key)
            .get(CSRF_COOKIE)
            .unwrap()
            .value()
            .rsplit(':')
            .next()
            .unwrap()
            .to_string();
        let decision = |csrf: String| {
            Form(ApprovalForm {
                user_code: code.user_code.clone(),
                csrf,
                decision: "approve".into(),
            })
        };
        assert!(decide(
            State(state.clone()),
            cookies.clone(),
            headers("https://home.example"),
            decision(nonce.clone())
        )
        .await
        .is_err());
        assert!(decide(
            State(state.clone()),
            cookies.clone(),
            headers(&state.public_url),
            decision("0".repeat(64))
        )
        .await
        .is_err());
        decide(
            State(state.clone()),
            cookies.clone(),
            headers(&state.public_url),
            decision(nonce),
        )
        .await
        .unwrap();
        let (token, grant) = match decode::<Poll>(
            poll(
                State(state.clone()),
                headers("https://home.example"),
                poll_request(),
            )
            .await
            .unwrap(),
        )
        .await
        {
            Poll::Complete {
                access_token,
                grant,
                ..
            } => (access_token, grant),
            _ => panic!("approval must complete"),
        };
        assert_eq!(grant.user_id, user.id);
        assert_eq!(grant.scope, TokenScope::Read);
        assert!(matches!(
            decode::<Poll>(
                poll(
                    State(state.clone()),
                    headers("https://home.example"),
                    poll_request()
                )
                .await
                .unwrap()
            )
            .await,
            Poll::Expired
        ));
        let auth =
            super::super::authenticate(&state, &mut conn, &token, &headers("https://home.example"))
                .unwrap();
        assert!(auth.authorize(&state, TokenScope::Write).is_err());
        let Json(rotated) =
            credentials::refresh(State(state.clone()), Some(axum::Extension(auth.clone())))
                .await
                .unwrap();
        assert_ne!(token, rotated.access_token);
        assert!(
            credentials::refresh(State(state.clone()), Some(axum::Extension(auth.clone())))
                .await
                .is_err()
        );
        assert!(super::super::authenticate(
            &state,
            &mut conn,
            &token,
            &headers("https://home.example")
        )
        .is_err());
        let new_auth = super::super::authenticate(
            &state,
            &mut conn,
            &rotated.access_token,
            &headers("https://home.example"),
        )
        .unwrap();
        assert_eq!(new_auth.grant_id, grant.id);
        assert_eq!(new_auth.scope, TokenScope::Read);
        let expiry = proxy_auth_tokens::table
            .find(new_auth.token_id)
            .select(proxy_auth_tokens::expires_at)
            .first::<Option<chrono::NaiveDateTime>>(&mut conn)
            .unwrap()
            .unwrap();
        assert!(
            expiry.and_utc() <= chrono::DateTime::parse_from_rfc3339(&grant.expires_at).unwrap()
        );
        set_policy(&mut conn, FederationPolicy::Off);
        assert!(credentials::refresh(
            State(state.clone()),
            Some(axum::Extension(new_auth.clone()))
        )
        .await
        .is_err());
        set_policy(
            &mut conn,
            FederationPolicy::Any {
                max_scope: TokenScope::Read,
            },
        );
        assert!(credentials::revoke(
            State(state.clone()),
            crate::auth::CurrentUserId(user.id),
            Some(axum::Extension(new_auth.clone())),
            axum::extract::Path(Uuid::new_v4())
        )
        .await
        .is_err());
        credentials::revoke(
            State(state.clone()),
            crate::auth::CurrentUserId(user.id),
            Some(axum::Extension(new_auth)),
            axum::extract::Path(grant.id),
        )
        .await
        .unwrap();
        assert!(super::super::authenticate(
            &state,
            &mut conn,
            &rotated.access_token,
            &headers("https://home.example")
        )
        .is_err());
        set_policy(&mut conn, FederationPolicy::Off);
        diesel::delete(users::table.find(user.id))
            .execute(&mut conn)
            .unwrap();
    }
}
