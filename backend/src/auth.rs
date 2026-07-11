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

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
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
    let (user_id, _email) =
        crate::handlers::proxy_tokens::verify_and_get_user(app_state, conn, token)?;

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
pub fn extract_user(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::images::ImageStore;
    use crate::handlers::proxy_tokens::{issue_proxy_token_with_type, TokenPersist};
    use crate::handlers::websocket::SessionManager;
    use crate::models::NewUser;
    use crate::schema::{proxy_auth_tokens, users};
    use axum::http::{header::AUTHORIZATION, HeaderValue};
    use std::time::Duration;
    use tower_cookies::cookie::Key;
    use tower_cookies::Cookie;

    static DB_SETUP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn user(disabled: bool) -> User {
        User {
            id: Uuid::new_v4(),
            google_id: "google-id".to_string(),
            email: "user@example.com".to_string(),
            name: Some("User".to_string()),
            avatar_url: None,
            created_at: chrono::Utc::now().naive_utc(),
            updated_at: chrono::Utc::now().naive_utc(),
            is_admin: false,
            disabled,
            ban_reason: None,
            sound_config: None,
            notification_prefs: None,
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

    fn test_pool() -> Option<crate::db::DbPool> {
        if std::env::var("DATABASE_URL").is_err() {
            eprintln!("DATABASE_URL not set, skipping DB-backed auth test");
            return None;
        }
        let _guard = DB_SETUP_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let pool = crate::db::create_pool().expect("create test DB pool");
        crate::db::run_migrations_logged(&pool).expect("run migrations");
        Some(pool)
    }

    fn test_state(pool: crate::db::DbPool) -> AppState {
        AppState {
            dev_mode: false,
            db_pool: pool,
            session_manager: SessionManager::new(),
            oauth_basic_client: None,
            device_flow_store: None,
            public_url: "http://localhost:3000".to_string(),
            cookie_key: Key::generate(),
            jwt_secret: "test-secret-key-at-least-32-bytes".to_string(),
            app_title: "Agent Portal Test".to_string(),
            splash_text: None,
            allowed_email_domain: None,
            allowed_emails: None,
            message_retention_count: 100,
            message_retention_days: 30,
            session_max_age_days: 14,
            max_image_mb: 10,
            image_store: ImageStore::new(1024 * 1024, Duration::from_secs(60)),
            forward_domain: None,
            archive: None,
            notifications: crate::push::channel().0,
        }
    }

    fn insert_user(conn: &mut PgConnection, disabled: bool) -> User {
        let id = Uuid::new_v4();
        let mut user: User = diesel::insert_into(users::table)
            .values(&NewUser {
                google_id: format!("google-{id}"),
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
        let Some(pool) = test_pool() else {
            return;
        };
        let app_state = test_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn, false);
        let cookies = signed_cookie(&app_state, user.id);

        let got = extract_user(&app_state, None, &cookies).expect("cookie auth");
        assert_eq!(got.id, user.id);
    }

    #[test]
    fn extract_user_rejects_disabled_cookie_user() {
        let Some(pool) = test_pool() else {
            return;
        };
        let app_state = test_state(pool);
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
        let Some(pool) = test_pool() else {
            return;
        };
        let app_state = test_state(pool);
        let cookies = Cookies::default();

        assert!(matches!(
            extract_user(&app_state, None, &cookies),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn extract_user_allows_valid_mobile_bearer() {
        let Some(pool) = test_pool() else {
            return;
        };
        let app_state = test_state(pool);
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
        let Some(pool) = test_pool() else {
            return;
        };
        let app_state = test_state(pool);
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
        let Some(pool) = test_pool() else {
            return;
        };
        let app_state = test_state(pool);
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
        let Some(pool) = test_pool() else {
            return;
        };
        let app_state = test_state(pool);
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
        let Some(pool) = test_pool() else {
            return;
        };
        let app_state = test_state(pool);
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
