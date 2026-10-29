use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use diesel::prelude::*;
use std::sync::Arc;
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::User;
use crate::AppState;

/// Email of the test user that dev mode auto-creates and logs in as.
pub const DEV_USER_EMAIL: &str = "testing@testing.local";

/// Look up the dev-mode test user by email.
pub fn dev_user(conn: &mut PgConnection) -> QueryResult<User> {
    use crate::schema::users;

    users::table
        .filter(users::email.eq(DEV_USER_EMAIL))
        .first::<User>(conn)
}

/// Return `Forbidden` for disabled accounts.
fn reject_disabled_user(user: User) -> Result<User, AppError> {
    if user.disabled {
        tracing::warn!(
            "Disabled user {} attempted authenticated access",
            user.email
        );
        Err(AppError::Forbidden)
    } else {
        Ok(user)
    }
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
}

fn load_cookie_user(
    app_state: &AppState,
    conn: &mut PgConnection,
    cookies: &Cookies,
) -> Result<User, AppError> {
    use crate::schema::users;

    let cookie = cookies
        .signed(&app_state.cookie_key)
        .get(shared::protocol::SESSION_COOKIE_NAME)
        .ok_or(AppError::Unauthorized)?;

    let user_id: Uuid = cookie.value().parse().map_err(|_| AppError::Unauthorized)?;
    users::table
        .find(user_id)
        .first::<User>(conn)
        .map_err(|_| AppError::Unauthorized)
}

fn load_bearer_user(
    app_state: &AppState,
    conn: &mut PgConnection,
    headers: &HeaderMap,
) -> Result<Option<User>, AppError> {
    let Some(token) = bearer_token(headers) else {
        return Ok(None);
    };
    let user_id = if token.starts_with(crate::handlers::federation::LINKED_TOKEN_PREFIX) {
        crate::handlers::federation::authenticate(app_state, conn, token, headers)?.user_id
    } else {
        crate::handlers::proxy_tokens::verify_and_get_user(app_state, conn, token)?.0
    };

    use crate::schema::users;
    let user = users::table
        .find(user_id)
        .first::<User>(conn)
        .map_err(|_| AppError::Unauthorized)?;
    Ok(Some(user))
}

/// Extract and load the authenticated user from a bearer token or session cookie.
///
/// `Authorization: Bearer` takes precedence when present, matching the
/// programmatic API auth path: a bad bearer must not silently fall through to a
/// valid browser cookie. In dev mode with no bearer, returns the test user
/// without checking cookies. Disabled users are rejected in every path so a ban
/// takes effect for existing browser sessions and mobile tokens immediately.
fn extract_user_unbound(
    app_state: &AppState,
    headers: Option<&HeaderMap>,
    cookies: &Cookies,
) -> Result<User, AppError> {
    let mut conn = app_state.conn()?;

    if let Some(headers) = headers {
        if let Some(user) = load_bearer_user(app_state, &mut conn, headers)? {
            return reject_disabled_user(user);
        }
    }

    if app_state.dev_mode {
        let user = dev_user(&mut conn)?;
        return reject_disabled_user(user);
    }

    let user = load_cookie_user(app_state, &mut conn, cookies)?;
    reject_disabled_user(user)
}

/// A rendered view may bind requests to the account that created it. This
/// prevents another tab changing the login cookie from redirecting an old
/// view's mutation into the newly signed-in account. The header only narrows
/// authority; it never authenticates a user.
pub fn extract_user(
    app_state: &AppState,
    headers: Option<&HeaderMap>,
    cookies: &Cookies,
) -> Result<User, AppError> {
    let user = extract_user_unbound(app_state, headers, cookies)?;
    crate::log_context::record_user(&user);
    if let Some(expected) = headers.and_then(|h| h.get("X-Portal-Account")) {
        let expected = expected.to_str().ok().and_then(|s| Uuid::parse_str(s).ok());
        if expected != Some(user.id) {
            return Err(AppError::Forbidden);
        }
    }
    Ok(user)
}

/// Extract the authenticated user's ID from the session cookie.
///
/// In dev mode, returns the test user's ID without checking cookies.
/// In production, reads the signed session cookie and parses the user ID.
/// Disabled users are rejected in both modes.
pub fn extract_user_id(
    app_state: &AppState,
    headers: Option<&HeaderMap>,
    cookies: &Cookies,
) -> Result<Uuid, AppError> {
    extract_user(app_state, headers, cookies).map(|user| user.id)
}

/// Axum extractor for the authenticated user's ID.
///
/// Runs [`extract_user_id`] before the handler body, so handlers that take
/// this parameter can't forget the auth check. The rejection renders exactly
/// what the manual `extract_user_id(&app_state, &cookies)?` call produced
/// (`AppError` via `IntoResponse`).
pub struct CurrentUserId(pub Uuid);

impl FromRequestParts<Arc<AppState>> for CurrentUserId {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        let cookies = Cookies::from_request_parts(parts, state)
            .await
            .map_err(|rejection| rejection.into_response())?;
        extract_user_id(state, Some(&parts.headers), &cookies)
            .map(CurrentUserId)
            .map_err(|e| e.into_response())
    }
}

/// Browser consent must not be forgeable with an agent's bearer credential.
/// Unlike CurrentUserId this always requires the signed browser cookie, even
/// in dev mode; the normal dev login issues that cookie.
pub struct BrowserUserId(pub Uuid);

impl FromRequestParts<Arc<AppState>> for BrowserUserId {
    type Rejection = Response;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &Arc<AppState>,
    ) -> Result<Self, Self::Rejection> {
        if parts.headers.contains_key(header::AUTHORIZATION) {
            return Err(AppError::Forbidden.into_response());
        }
        let cookies = Cookies::from_request_parts(parts, state)
            .await
            .map_err(|e| e.into_response())?;
        let result = (|| {
            let mut conn = state.conn()?;
            let user = reject_disabled_user(load_cookie_user(state, &mut conn, &cookies)?)?;
            if let Some(expected) = parts.headers.get("X-Portal-Account") {
                if expected.to_str().ok().and_then(|s| Uuid::parse_str(s).ok()) != Some(user.id) {
                    return Err(AppError::Forbidden);
                }
            }
            Ok::<_, AppError>(BrowserUserId(user.id))
        })();
        result.map_err(|e| e.into_response())
    }
}

/// Session credentials must use the policy-gated child-spawn API. Personal
/// owner API tokens keep their existing manual launch capability. This checks
/// the launch-token name too, so an as-yet-unbound launch token cannot bypass it.
pub(crate) fn require_manual_launch_credential(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), AppError> {
    if let Some(token) = bearer_token(headers) {
        if !token.starts_with(crate::handlers::federation::LINKED_TOKEN_PREFIX) {
            let verified = crate::handlers::proxy_tokens::verify_and_get_user_with_token(
                state,
                &mut *state.conn()?,
                token,
            )?;
            if verified.token.session_id.is_some()
                || verified.token.name == crate::handlers::proxy_tokens::LAUNCH_TOKEN_NAME
            {
                return Err(AppError::Forbidden);
            }
        }
    }
    Ok(())
}

/// Session credentials may stop/delete themselves or direct children, never a
/// sibling. Personal owner tokens and browser requests retain their old scope.
pub(crate) fn require_session_lifecycle_scope(
    state: &AppState,
    headers: &HeaderMap,
    target: Uuid,
) -> Result<(), AppError> {
    let Some(token) = bearer_token(headers) else {
        return Ok(());
    };
    if token.starts_with(crate::handlers::federation::LINKED_TOKEN_PREFIX) {
        return Ok(());
    }
    let mut conn = state.conn()?;
    let verified =
        crate::handlers::proxy_tokens::verify_and_get_user_with_token(state, &mut conn, token)?;
    if verified.token.name == crate::handlers::proxy_tokens::LAUNCH_TOKEN_NAME
        || verified.token.session_id.is_some()
    {
        let parent = verified.token.session_id.ok_or(AppError::Forbidden)?;
        if parent != target {
            use crate::schema::sessions;
            let owned_child = sessions::table
                .find(target)
                .filter(sessions::user_id.eq(verified.user_id))
                .filter(sessions::spawned_by.eq(parent))
                .select(sessions::id)
                .first::<Uuid>(&mut conn)
                .optional()?
                .is_some();
            if !owned_child {
                return Err(AppError::Forbidden);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::proxy_tokens::{issue_proxy_token_with_type, TokenPersist};
    use crate::models::NewUser;
    use crate::schema::{proxy_auth_tokens, users};
    use axum::http::{header::AUTHORIZATION, HeaderValue};
    use tower_cookies::Cookie;

    #[tokio::test]
    async fn spawn_consent_requires_cookie_even_in_dev_and_rejects_bearer_and_stale_account() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut state = crate::test_support::test_app_state(pool);
        state.dev_mode = true;
        let user = insert_user(&mut state.conn().unwrap(), false);
        let state = Arc::new(state);
        async fn extract(state: &Arc<AppState>, cookies: Cookies, headers: HeaderMap) -> bool {
            let (mut parts, _) = axum::http::Request::new(()).into_parts();
            parts.headers = headers;
            parts.extensions.insert(cookies);
            BrowserUserId::from_request_parts(&mut parts, state)
                .await
                .is_ok()
        }
        assert!(!extract(&state, Cookies::default(), HeaderMap::new()).await);
        assert!(extract(&state, signed_cookie(&state, user.id), HeaderMap::new()).await);
        assert!(
            !extract(
                &state,
                signed_cookie(&state, user.id),
                bearer_headers("anything")
            )
            .await
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-Portal-Account",
            Uuid::new_v4().to_string().parse().unwrap(),
        );
        assert!(!extract(&state, signed_cookie(&state, user.id), headers).await);
    }

    #[test]
    fn session_credentials_cannot_use_manual_launch_or_mint_credentials() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let state = crate::test_support::test_app_state(pool);
        let mut conn = state.conn().unwrap();
        let user = insert_user(&mut conn, false);
        let issued = crate::handlers::proxy_tokens::issue_proxy_token(
            &mut conn,
            state.jwt_secret.as_bytes(),
            user.id,
            TokenPersist::Create {
                name: crate::handlers::proxy_tokens::LAUNCH_TOKEN_NAME,
            },
            None,
        )
        .unwrap();
        let headers = bearer_headers(&issued.token);
        // Rejected before first registration too, closing the unbound-token window.
        assert!(require_manual_launch_credential(&state, &headers).is_err());
        let personal = crate::handlers::proxy_tokens::issue_proxy_token(
            &mut conn,
            state.jwt_secret.as_bytes(),
            user.id,
            TokenPersist::Create { name: "owner-cli" },
            None,
        )
        .unwrap();
        assert!(require_manual_launch_credential(&state, &bearer_headers(&personal.token)).is_ok());
        assert!(require_manual_launch_credential(&state, &HeaderMap::new()).is_ok());
    }

    fn user(disabled: bool) -> User {
        User {
            id: Uuid::new_v4(),
            email: "user@example.com".to_string(),
            name: Some("User".to_string()),
            avatar_url: None,
            created_at: chrono::Utc::now().naive_utc(),
            updated_at: chrono::Utc::now().naive_utc(),
            is_admin: false,
            disabled,
            ban_reason: None,
            notification_prefs: None,
            nickname: None,
        }
    }

    #[test]
    fn enabled_user_is_allowed() {
        let user = user(false);
        assert_eq!(reject_disabled_user(user.clone()).unwrap().id, user.id);
    }

    #[test]
    fn disabled_user_is_forbidden() {
        assert!(matches!(
            reject_disabled_user(user(true)),
            Err(AppError::Forbidden)
        ));
    }

    fn insert_user(conn: &mut PgConnection, disabled: bool) -> User {
        let id = Uuid::new_v4();
        let mut user: User = diesel::insert_into(users::table)
            .values(&NewUser {
                email: format!("auth-{id}@example.com"),
                name: Some("Auth Test".to_string()),
                avatar_url: None,
            })
            .get_result(conn)
            .expect("insert user");
        if disabled {
            user = diesel::update(users::table.find(user.id))
                .set(users::disabled.eq(true))
                .get_result(conn)
                .expect("disable user");
        }
        user
    }

    fn signed_cookie(app_state: &AppState, user_id: Uuid) -> Cookies {
        let cookies = Cookies::default();
        cookies.signed(&app_state.cookie_key).add(Cookie::new(
            shared::protocol::SESSION_COOKIE_NAME,
            user_id.to_string(),
        ));
        cookies
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("valid header"),
        );
        headers
    }

    fn issue_mobile_token(
        app_state: &AppState,
        conn: &mut PgConnection,
        user_id: Uuid,
    ) -> (Uuid, String) {
        let issued = issue_proxy_token_with_type(
            conn,
            app_state.jwt_secret.as_bytes(),
            user_id,
            TokenPersist::Create {
                name: "mobile-test",
            },
            Some(30),
            shared::TOKEN_TYPE_MOBILE,
        )
        .expect("issue mobile token");
        let claims = crate::jwt::verify_proxy_token(app_state.jwt_secret.as_bytes(), &issued.token)
            .expect("issued token verifies");
        assert_eq!(claims.token_type, shared::TOKEN_TYPE_MOBILE);
        assert!(claims.exp.is_some(), "mobile tokens must expire");
        (issued.row_id, issued.token)
    }

    #[test]
    fn extract_user_allows_valid_cookie() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = crate::test_support::test_app_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn, false);
        let cookies = signed_cookie(&app_state, user.id);

        let got = extract_user(&app_state, None, &cookies).expect("cookie auth");
        assert_eq!(got.id, user.id);
    }

    #[test]
    fn stale_account_binding_cannot_follow_a_changed_cookie() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let state = crate::test_support::test_app_state(pool);
        let mut conn = state.conn().unwrap();
        let old = insert_user(&mut conn, false);
        let new = insert_user(&mut conn, false);
        let mut headers = HeaderMap::new();
        headers.insert("X-Portal-Account", old.id.to_string().parse().unwrap());
        assert_eq!(
            extract_user(&state, Some(&headers), &signed_cookie(&state, old.id))
                .unwrap()
                .id,
            old.id
        );
        assert!(matches!(
            extract_user(&state, Some(&headers), &signed_cookie(&state, new.id)),
            Err(AppError::Forbidden)
        ));
        for invalid in ["", "not-an-account"] {
            headers.insert("X-Portal-Account", invalid.parse().unwrap());
            assert!(matches!(
                extract_user(&state, Some(&headers), &signed_cookie(&state, old.id)),
                Err(AppError::Forbidden)
            ));
        }
        // The binding is a ceiling, never a replacement for authentication.
        assert!(extract_user(&state, Some(&headers), &Cookies::default()).is_err());
    }

    #[test]
    fn extract_user_rejects_disabled_cookie_user() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = crate::test_support::test_app_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn, true);
        let cookies = signed_cookie(&app_state, user.id);

        assert!(matches!(
            extract_user(&app_state, None, &cookies),
            Err(AppError::Forbidden)
        ));
    }

    #[test]
    fn extract_user_rejects_missing_cookie() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = crate::test_support::test_app_state(pool);
        let cookies = Cookies::default();

        assert!(matches!(
            extract_user(&app_state, None, &cookies),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn extract_user_allows_valid_mobile_bearer() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = crate::test_support::test_app_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn, false);
        let (_row_id, token) = issue_mobile_token(&app_state, &mut conn, user.id);
        let headers = bearer_headers(&token);

        let got =
            extract_user(&app_state, Some(&headers), &Cookies::default()).expect("bearer auth");
        assert_eq!(got.id, user.id);
    }

    #[test]
    fn extract_user_rejects_expired_mobile_bearer() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = crate::test_support::test_app_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn, false);
        let (row_id, token) = issue_mobile_token(&app_state, &mut conn, user.id);
        diesel::update(proxy_auth_tokens::table.find(row_id))
            .set(proxy_auth_tokens::expires_at.eq(Some(
                (chrono::Utc::now() - chrono::Duration::minutes(1)).naive_utc(),
            )))
            .execute(&mut conn)
            .expect("expire token row");
        let headers = bearer_headers(&token);

        assert!(matches!(
            extract_user(&app_state, Some(&headers), &Cookies::default()),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn extract_user_rejects_revoked_mobile_bearer() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = crate::test_support::test_app_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn, false);
        let (row_id, token) = issue_mobile_token(&app_state, &mut conn, user.id);
        diesel::update(proxy_auth_tokens::table.find(row_id))
            .set(proxy_auth_tokens::revoked.eq(true))
            .execute(&mut conn)
            .expect("revoke token row");
        let headers = bearer_headers(&token);

        assert!(matches!(
            extract_user(&app_state, Some(&headers), &Cookies::default()),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn extract_user_rejects_disabled_mobile_bearer_user() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = crate::test_support::test_app_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn, true);
        let (_row_id, token) = issue_mobile_token(&app_state, &mut conn, user.id);
        let headers = bearer_headers(&token);

        assert!(matches!(
            extract_user(&app_state, Some(&headers), &Cookies::default()),
            Err(AppError::Forbidden)
        ));
    }

    #[test]
    fn extract_user_does_not_fall_back_from_bad_bearer_to_valid_cookie() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = crate::test_support::test_app_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn, false);
        let cookies = signed_cookie(&app_state, user.id);
        let headers = bearer_headers("not-a-jwt");

        assert!(matches!(
            extract_user(&app_state, Some(&headers), &cookies),
            Err(AppError::Unauthorized)
        ));
    }
}
