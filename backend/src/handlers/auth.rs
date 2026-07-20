use axum::{
    extract::{Query, State},
    http::{header, HeaderMap},
    response::{IntoResponse, Redirect},
    Json,
};
use diesel::prelude::*;
use oauth2::{AuthorizationCode, TokenResponse};
use serde::Deserialize;
use shared::api::{MeResponse, TokenRefreshResponse};
use shared::TOKEN_TYPE_MOBILE;
use std::sync::Arc;
use tower_cookies::{cookie::SameSite, Cookie, Cookies};
use tracing::{info, warn};

use crate::{
    errors::AppError,
    handlers::proxy_tokens::{
        issue_proxy_token_with_type, verify_and_get_user_with_token, TokenPersist,
    },
    models::{NewUser, User},
    routes,
    schema::proxy_auth_tokens,
    AppState,
};

use shared::protocol::SESSION_COOKIE_NAME;

mod oauth;

const MOBILE_TOKEN_TTL_DAYS: u32 = 30;
const TOKEN_REFRESH_GRACE_MINUTES: i64 = 10;

/// Regular web login - redirects to Google OAuth
pub async fn login(State(app_state): State<Arc<AppState>>, cookies: Cookies) -> impl IntoResponse {
    info!(
        target: "auth_audit",
        event = "web_login_start",
        oauth_configured = app_state.oauth_basic_client.is_some(),
    );
    let client = match &app_state.oauth_basic_client {
        Some(c) => c,
        None => {
            info!(
                target: "auth_audit",
                event = "web_login_dev_redirect",
            );
            return Redirect::temporary(routes::AUTH_DEV_LOGIN).into_response();
        }
    };

    oauth::regular_authorization_redirect(client, &cookies, &app_state).into_response()
}

/// Device flow login - separate endpoint that stores device_user_code in state
/// This is used when the user needs to authenticate before approving a device
#[derive(Debug, Deserialize)]
pub struct DeviceLoginQuery {
    pub device_user_code: String,
}

pub async fn device_login(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Query(query): Query<DeviceLoginQuery>,
) -> Result<impl IntoResponse, AppError> {
    info!(
        target: "auth_audit",
        event = "device_login_start",
        user_code = %query.device_user_code,
        oauth_configured = app_state.oauth_basic_client.is_some(),
    );
    // In dev mode, auto-login and redirect to device approval page
    if app_state.oauth_basic_client.is_none() {
        let mut conn = app_state.conn()?;

        let user = crate::auth::dev_user(&mut conn).map_err(dev_user_lookup_error)?;

        if user.disabled {
            info!("Banned user {} attempted dev device login", user.email);
            warn!(
                target: "auth_audit",
                event = "device_login_denied",
                reason = "disabled_user",
                user_id = %user.id,
                user_email = %user.email,
                user_code = %query.device_user_code,
            );
            return Ok(Redirect::temporary(routes::BANNED).into_response());
        }

        info!("Dev mode: auto-logged in for device flow");
        info!(
            target: "auth_audit",
            event = "device_login_success",
            mode = "dev",
            user_id = %user.id,
            user_email = %user.email,
            user_code = %query.device_user_code,
        );

        add_session_cookie(&cookies, &app_state, &user);

        return Ok(device_approval_redirect(&query.device_user_code).into_response());
    }

    let client = app_state.oauth_basic_client.as_ref().unwrap();

    Ok(
        oauth::device_authorization_redirect(client, &cookies, &app_state, &query.device_user_code)
            .into_response(),
    )
}

#[derive(Debug, Deserialize)]
pub struct AuthCallbackQuery {
    code: String,
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GoogleUserInfo {
    sub: String,
    email: String,
    name: Option<String>,
    picture: Option<String>,
}

pub async fn callback(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Query(query): Query<AuthCallbackQuery>,
) -> Result<impl IntoResponse, AppError> {
    let callback_kind = if query
        .state
        .as_deref()
        .is_some_and(|state| state.starts_with("device:"))
    {
        "device"
    } else {
        "web"
    };
    info!(
        target: "auth_audit",
        event = "oauth_callback_start",
        flow = callback_kind,
        state_present = query.state.is_some(),
    );

    let client = app_state
        .oauth_basic_client
        .as_ref()
        .ok_or(AppError::ServiceUnavailable("OAuth client not configured"))?;
    let http_client = oauth2::reqwest::ClientBuilder::new()
        .redirect(oauth2::reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AppError::Internal(format!("Failed to build OAuth HTTP client: {e}")))?;

    let device_state = oauth::validate_callback_state(&cookies, &app_state, query.state.as_deref())
        .inspect_err(|_| {
            warn!(
                target: "auth_audit",
                event = "oauth_callback_denied",
                flow = callback_kind,
                reason = "invalid_state",
            );
        })?;

    // Exchange code for token
    let token: oauth2::StandardTokenResponse<
        oauth2::EmptyExtraTokenFields,
        oauth2::basic::BasicTokenType,
    > = client
        .exchange_code(AuthorizationCode::new(query.code))
        .request_async(&http_client)
        .await
        .map_err(|e| {
            warn!(
                target: "auth_audit",
                event = "oauth_callback_failed",
                flow = callback_kind,
                stage = "token_exchange",
            );
            AppError::Internal(format!("Failed to exchange OAuth code: {e}"))
        })?;

    // Fetch user info from Google
    let client = reqwest::Client::new();
    let user_info: GoogleUserInfo = client
        .get("https://www.googleapis.com/oauth2/v3/userinfo")
        .bearer_auth(token.access_token().secret())
        .send()
        .await
        .map_err(|e| {
            warn!(
                target: "auth_audit",
                event = "oauth_callback_failed",
                flow = callback_kind,
                stage = "userinfo_fetch",
            );
            AppError::Internal(format!("Failed to fetch Google user info: {e}"))
        })?
        .json()
        .await
        .map_err(|e| {
            warn!(
                target: "auth_audit",
                event = "oauth_callback_failed",
                flow = callback_kind,
                stage = "userinfo_parse",
            );
            AppError::Internal(format!("Failed to parse Google user info: {e}"))
        })?;

    info!("User authenticated: {}", user_info.email);

    // Check email access control
    if let Err(redirect) = check_email_allowed(&app_state, &user_info.email) {
        warn!(
            target: "auth_audit",
            event = "oauth_callback_denied",
            flow = callback_kind,
            reason = "email_not_allowed",
            user_email = %user_info.email,
        );
        return Ok(redirect);
    }

    // Save or update user in database
    let mut conn = app_state.conn()?;

    use crate::schema::users::dsl::*;

    let user = users
        .filter(google_id.eq(&user_info.sub))
        .first::<User>(&mut conn)
        .optional()
        .map_err(|e| AppError::DbQuery(format!("Failed to look up user by google_id: {e}")))?;

    let user = match user {
        Some(user) => user,
        None => {
            let new_user = NewUser {
                google_id: user_info.sub,
                email: user_info.email,
                name: user_info.name,
                avatar_url: user_info.picture,
            };

            diesel::insert_into(users)
                .values(&new_user)
                .get_result::<User>(&mut conn)
                .map_err(|e| AppError::DbQuery(format!("Failed to create user: {e}")))?
        }
    };

    // Check if user is banned
    if user.disabled {
        let reason = user.ban_reason.as_deref().unwrap_or("No reason provided");
        info!("Banned user {} attempted login", user.email);
        warn!(
            target: "auth_audit",
            event = "oauth_callback_denied",
            flow = callback_kind,
            reason = "disabled_user",
            user_id = %user.id,
            user_email = %user.email,
        );
        return Ok(banned_redirect(reason));
    }

    if let Some(device_state) = device_state {
        add_session_cookie(&cookies, &app_state, &user);

        // Redirect back to device verify page to show approval UI
        info!(
            "OAuth complete for device flow, redirecting to approval page for user: {}",
            user.email
        );
        info!(
            target: "auth_audit",
            event = "oauth_callback_success",
            flow = "device",
            user_id = %user.id,
            user_email = %user.email,
            user_code = %device_state.user_code,
        );
        return Ok(device_approval_redirect(&device_state.user_code));
    }

    add_session_cookie(&cookies, &app_state, &user);
    info!(
        target: "auth_audit",
        event = "oauth_callback_success",
        flow = "web",
        user_id = %user.id,
        user_email = %user.email,
    );

    Ok(Redirect::temporary(routes::DASHBOARD))
}

pub async fn me(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<MeResponse>, AppError> {
    let user =
        crate::auth::extract_user(&app_state, Some(&headers), &cookies).map_err(auth_user_error)?;

    Ok(Json(me_response(user)))
}

pub async fn refresh_token(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<TokenRefreshResponse>, AppError> {
    let token = bearer_token(&headers).ok_or(AppError::Unauthorized)?;
    let mut conn = app_state.conn()?;
    let verified = verify_and_get_user_with_token(&app_state, &mut conn, token)?;
    require_mobile_token(&verified.token_type)?;

    let now = chrono::Utc::now().naive_utc();
    if !mobile_token_needs_refresh(verified.token.created_at, verified.token.expires_at, now) {
        return Ok(Json(TokenRefreshResponse {
            refreshed: false,
            auth_token: None,
            expires_at: verified
                .token
                .expires_at
                .map(|dt| dt.and_utc().to_rfc3339()),
        }));
    }

    let issued = issue_proxy_token_with_type(
        &mut conn,
        app_state.jwt_secret.as_bytes(),
        verified.user_id,
        TokenPersist::Create {
            name: &verified.token.name,
        },
        Some(MOBILE_TOKEN_TTL_DAYS),
        TOKEN_TYPE_MOBILE,
    )?;

    let grace_expires_at = mobile_token_grace_expires_at(now);
    diesel::update(
        proxy_auth_tokens::table
            .filter(proxy_auth_tokens::id.eq(verified.token.id))
            .filter(proxy_auth_tokens::revoked.eq(false)),
    )
    .set(proxy_auth_tokens::expires_at.eq(Some(grace_expires_at)))
    .execute(&mut conn)?;

    info!(
        target: "auth_audit",
        event = "mobile_token_refreshed",
        user_id = %verified.user_id,
        user_email = %verified.email,
        old_token_id = %verified.token.id,
        new_token_id = %issued.row_id,
    );

    Ok(Json(TokenRefreshResponse {
        refreshed: true,
        auth_token: Some(issued.token),
        expires_at: issued.expires_at.map(|dt| dt.to_rfc3339()),
    }))
}

pub async fn token_login(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<MeResponse>, AppError> {
    let token = bearer_token(&headers).ok_or(AppError::Unauthorized)?;
    let mut conn = app_state.conn()?;
    let verified = verify_and_get_user_with_token(&app_state, &mut conn, token)?;
    require_mobile_token(&verified.token_type)?;

    use crate::schema::users;
    let user = users::table
        .find(verified.user_id)
        .first::<User>(&mut conn)
        .map_err(|_| AppError::Unauthorized)?;
    if user.disabled {
        return Err(AppError::Forbidden);
    }

    add_session_cookie(&cookies, &app_state, &user);
    info!(
        target: "auth_audit",
        event = "mobile_token_login",
        user_id = %user.id,
        user_email = %user.email,
    );

    Ok(Json(me_response(user)))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
}

fn require_mobile_token(token_type: &str) -> Result<(), AppError> {
    if token_type == TOKEN_TYPE_MOBILE {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}

fn mobile_token_needs_refresh(
    created_at: chrono::NaiveDateTime,
    expires_at: Option<chrono::NaiveDateTime>,
    now: chrono::NaiveDateTime,
) -> bool {
    let Some(expires_at) = expires_at else {
        return true;
    };

    let lifetime_secs = expires_at.signed_duration_since(created_at).num_seconds();
    if lifetime_secs <= 0 {
        return true;
    }

    let age_secs = now.signed_duration_since(created_at).num_seconds().max(0);
    age_secs >= lifetime_secs / 2
}

fn mobile_token_grace_expires_at(now: chrono::NaiveDateTime) -> chrono::NaiveDateTime {
    now + chrono::Duration::minutes(TOKEN_REFRESH_GRACE_MINUTES)
}

fn me_response(user: User) -> MeResponse {
    MeResponse {
        id: user.id,
        email: user.email,
        name: user.name,
        avatar_url: user.avatar_url,
        is_admin: user.is_admin,
    }
}

pub async fn logout(State(app_state): State<Arc<AppState>>, cookies: Cookies) -> impl IntoResponse {
    cookies
        .signed(&app_state.cookie_key)
        .add(remove_session_cookie());

    info!("User logged out");
    Redirect::temporary(routes::ROOT)
}

// Development mode handlers (bypass OAuth)
pub async fn dev_login(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<impl IntoResponse, AppError> {
    let mut conn = app_state.conn()?;

    let user = crate::auth::dev_user(&mut conn).map_err(dev_user_lookup_error)?;

    // Check if user is banned
    if user.disabled {
        let reason = user.ban_reason.as_deref().unwrap_or("No reason provided");
        info!("Banned user {} attempted dev login", user.email);
        warn!(
            target: "auth_audit",
            event = "dev_login_denied",
            reason = "disabled_user",
            user_id = %user.id,
            user_email = %user.email,
        );
        return Ok(banned_redirect(reason));
    }

    info!(
        "Dev mode: auto-logged in as {}",
        crate::auth::DEV_USER_EMAIL
    );
    info!(
        target: "auth_audit",
        event = "dev_login_success",
        user_id = %user.id,
        user_email = %user.email,
    );

    add_session_cookie(&cookies, &app_state, &user);

    // Redirect to dashboard
    Ok(Redirect::temporary(routes::DASHBOARD))
}

fn dev_user_lookup_error(error: diesel::result::Error) -> AppError {
    AppError::Internal(format!("Failed to look up dev user: {error}"))
}

fn auth_user_error(error: AppError) -> AppError {
    match error {
        AppError::Forbidden => AppError::Forbidden,
        AppError::DbPool | AppError::DbQuery(_) => error,
        _ => AppError::Unauthorized,
    }
}

/// Check if an email is allowed based on ALLOWED_EMAIL_DOMAIN and ALLOWED_EMAILS
///
/// Returns Ok(()) if allowed, or Err(Redirect) to the access denied page
fn check_email_allowed(app_state: &AppState, email: &str) -> Result<(), Redirect> {
    let email_lower = email.to_lowercase();

    // If no restrictions are set, allow all
    if app_state.allowed_email_domain.is_none() && app_state.allowed_emails.is_none() {
        return Ok(());
    }

    // Check domain allowlist
    if let Some(ref domain) = app_state.allowed_email_domain {
        let domain_lower = domain.to_lowercase();
        if email_lower.ends_with(&format!("@{}", domain_lower)) {
            return Ok(());
        }
    }

    // Check specific email allowlist
    if let Some(ref emails) = app_state.allowed_emails {
        if emails.contains(&email_lower) {
            return Ok(());
        }
    }

    // Access denied
    info!("Access denied for email: {} (not in allowlist)", email);
    Err(Redirect::temporary(routes::ACCESS_DENIED))
}

fn add_session_cookie(cookies: &Cookies, app_state: &AppState, user: &User) {
    cookies
        .signed(&app_state.cookie_key)
        .add(session_cookie(user, !app_state.dev_mode));
}

fn session_cookie(user: &User, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, user.id.to_string());
    cookie.set_path(routes::ROOT);
    cookie.set_http_only(true);
    cookie.set_secure(secure);
    cookie.set_same_site(SameSite::Lax);
    cookie
}

fn remove_session_cookie() -> Cookie<'static> {
    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, "");
    cookie.set_path(routes::ROOT);
    cookie.set_http_only(true);
    cookie.set_secure(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::ZERO);
    cookie
}

fn banned_redirect(reason: &str) -> Redirect {
    Redirect::temporary(&format!(
        "{}?reason={}",
        routes::BANNED,
        encode_query_value(reason)
    ))
}

fn device_approval_redirect(user_code: &str) -> Redirect {
    Redirect::temporary(&format!(
        "{}?user_code={}",
        routes::AUTH_DEVICE,
        encode_query_value(user_code)
    ))
}

fn encode_query_value(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' || c == '~' {
                c.to_string()
            } else if c == ' ' {
                "+".to_string()
            } else {
                format!("%{:02X}", c as u8)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{NewUser, ProxyAuthToken};
    use crate::schema::{proxy_auth_tokens, users};
    use axum::http::{header::AUTHORIZATION, HeaderValue};

    #[test]
    fn auth_user_error_preserves_forbidden_and_database_errors() {
        assert!(matches!(
            auth_user_error(AppError::Forbidden),
            AppError::Forbidden
        ));
        assert!(matches!(
            auth_user_error(AppError::DbPool),
            AppError::DbPool
        ));
        assert!(matches!(
            auth_user_error(AppError::DbQuery("session lookup failed".to_string())),
            AppError::DbQuery(message) if message == "session lookup failed"
        ));
    }

    #[test]
    fn auth_user_error_collapses_other_failures_to_unauthorized() {
        assert!(matches!(
            auth_user_error(AppError::BadRequest("bad cookie")),
            AppError::Unauthorized
        ));
        assert!(matches!(
            auth_user_error(AppError::NotFound("user")),
            AppError::Unauthorized
        ));
        assert!(matches!(
            auth_user_error(AppError::Internal("decode failed".to_string())),
            AppError::Unauthorized
        ));
        assert!(matches!(
            auth_user_error(AppError::ServiceUnavailable("auth unavailable")),
            AppError::Unauthorized
        ));
    }

    #[test]
    fn encode_query_value_keeps_unreserved_and_encodes_reserved_chars() {
        assert_eq!(
            encode_query_value("ABC-123 reason?/"),
            "ABC-123+reason%3F%2F"
        );
    }

    #[test]
    fn mobile_token_needs_refresh_at_half_life() {
        let now = chrono::Utc::now().naive_utc();
        let created = now - chrono::Duration::days(16);
        let expires = created + chrono::Duration::days(30);
        assert!(mobile_token_needs_refresh(created, Some(expires), now));

        let created = now - chrono::Duration::days(10);
        let expires = created + chrono::Duration::days(30);
        assert!(!mobile_token_needs_refresh(created, Some(expires), now));
        assert!(mobile_token_needs_refresh(created, None, now));
    }

    fn test_state(pool: crate::db::DbPool) -> Arc<AppState> {
        Arc::new(crate::test_support::test_app_state(pool))
    }

    fn insert_user(conn: &mut diesel::PgConnection) -> User {
        let id = uuid::Uuid::new_v4();
        diesel::insert_into(users::table)
            .values(&NewUser {
                google_id: format!("google-{id}"),
                email: format!("mobile-{id}@example.com"),
                name: Some("Mobile Test".to_string()),
                avatar_url: None,
            })
            .get_result(conn)
            .expect("insert user")
    }

    fn issue_mobile_token(
        app_state: &AppState,
        conn: &mut diesel::PgConnection,
        user_id: uuid::Uuid,
    ) -> (uuid::Uuid, String) {
        let issued = issue_proxy_token_with_type(
            conn,
            app_state.jwt_secret.as_bytes(),
            user_id,
            TokenPersist::Create {
                name: "mobile-refresh-test",
            },
            Some(MOBILE_TOKEN_TTL_DAYS),
            TOKEN_TYPE_MOBILE,
        )
        .expect("issue mobile token");
        (issued.row_id, issued.token)
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).expect("valid bearer header"),
        );
        headers
    }

    fn set_token_window(
        conn: &mut diesel::PgConnection,
        token_id: uuid::Uuid,
        created_at: chrono::NaiveDateTime,
        expires_at: chrono::NaiveDateTime,
    ) {
        diesel::update(proxy_auth_tokens::table.find(token_id))
            .set((
                proxy_auth_tokens::created_at.eq(created_at),
                proxy_auth_tokens::expires_at.eq(Some(expires_at)),
            ))
            .execute(conn)
            .expect("set token window");
    }

    #[tokio::test]
    async fn refresh_token_waits_until_half_life() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = test_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn);
        let (token_id, token) = issue_mobile_token(&app_state, &mut conn, user.id);
        let now = chrono::Utc::now().naive_utc();
        set_token_window(
            &mut conn,
            token_id,
            now - chrono::Duration::days(10),
            now + chrono::Duration::days(20),
        );

        let Json(response) = refresh_token(State(app_state), bearer_headers(&token))
            .await
            .expect("refresh response");

        assert!(!response.refreshed);
        assert!(response.auth_token.is_none());
        assert!(response.expires_at.is_some());
    }

    #[tokio::test]
    async fn refresh_token_rotates_after_half_life_and_graces_old_row() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = test_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn);
        let (token_id, token) = issue_mobile_token(&app_state, &mut conn, user.id);
        let now = chrono::Utc::now().naive_utc();
        set_token_window(
            &mut conn,
            token_id,
            now - chrono::Duration::days(20),
            now + chrono::Duration::days(10),
        );

        let Json(response) = refresh_token(State(app_state.clone()), bearer_headers(&token))
            .await
            .expect("refresh response");

        assert!(response.refreshed);
        let new_token = response.auth_token.expect("new token");
        let claims = crate::jwt::verify_proxy_token(app_state.jwt_secret.as_bytes(), &new_token)
            .expect("new token verifies");
        assert_eq!(claims.token_type, TOKEN_TYPE_MOBILE);
        assert!(claims.exp.is_some());

        let old_row: ProxyAuthToken = proxy_auth_tokens::table
            .find(token_id)
            .first(&mut conn)
            .expect("old row");
        let grace = old_row.expires_at.expect("old row grace expiry");
        let remaining = grace.signed_duration_since(chrono::Utc::now().naive_utc());
        assert!(remaining.num_minutes() <= TOKEN_REFRESH_GRACE_MINUTES);
        assert!(remaining.num_minutes() >= TOKEN_REFRESH_GRACE_MINUTES - 1);
    }

    #[tokio::test]
    async fn token_login_sets_session_cookie() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let app_state = test_state(pool);
        let mut conn = app_state.conn().expect("conn");
        let user = insert_user(&mut conn);
        let (_token_id, token) = issue_mobile_token(&app_state, &mut conn, user.id);
        let cookies = Cookies::default();

        let Json(response) = token_login(
            State(app_state.clone()),
            bearer_headers(&token),
            cookies.clone(),
        )
        .await
        .expect("token login");

        assert_eq!(response.id, user.id);
        let cookie_user =
            crate::auth::extract_user(&app_state, None, &cookies).expect("session cookie auth");
        assert_eq!(cookie_user.id, user.id);
    }
}
