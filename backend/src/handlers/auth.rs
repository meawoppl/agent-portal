use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect},
    Json,
};
use diesel::prelude::*;
use oauth2::{AuthorizationCode, CsrfToken, Scope, TokenResponse};
use serde::Deserialize;
use shared::api::MeResponse;
use std::sync::Arc;
use tower_cookies::{cookie::SameSite, Cookie, Cookies};
use tracing::{error, info};

use crate::{
    models::{NewUser, User},
    routes, AppState,
};

use shared::protocol::SESSION_COOKIE_NAME;

const OAUTH_CSRF_COOKIE: &str = "oauth_csrf";
const OAUTH_DEVICE_CSRF_COOKIE: &str = "oauth_device_csrf";
/// Regular web login - redirects to Google OAuth
pub async fn login(State(app_state): State<Arc<AppState>>, cookies: Cookies) -> impl IntoResponse {
    let client = match &app_state.oauth_basic_client {
        Some(c) => c,
        None => return Redirect::temporary(routes::AUTH_DEV_LOGIN).into_response(),
    };

    let auth_request = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("openid".to_string()))
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()));

    let (auth_url, csrf_token) = auth_request.url();

    // Store the CSRF token in a short-lived signed cookie so we can verify it on callback.
    cookies.signed(&app_state.cookie_key).add(oauth_csrf_cookie(
        OAUTH_CSRF_COOKIE,
        csrf_token.secret(),
        !app_state.dev_mode,
    ));

    Redirect::temporary(auth_url.as_str()).into_response()
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
) -> Result<impl IntoResponse, StatusCode> {
    // In dev mode, auto-login and redirect to device approval page
    if app_state.oauth_basic_client.is_none() {
        use crate::schema::users::dsl::*;

        let mut conn = app_state
            .db_pool
            .get()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        let user = users
            .filter(email.eq("testing@testing.local"))
            .first::<User>(&mut conn)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        if user.disabled {
            info!("Banned user {} attempted dev device login", user.email);
            return Ok(Redirect::temporary(routes::BANNED).into_response());
        }

        info!("Dev mode: auto-logged in for device flow");

        add_session_cookie(&cookies, &app_state, &user);

        return Ok(device_approval_redirect(&query.device_user_code).into_response());
    }

    let client = app_state.oauth_basic_client.as_ref().unwrap();

    let device_csrf = CsrfToken::new_random();
    let state_value = build_device_oauth_state(&query.device_user_code, device_csrf.secret());
    cookies.signed(&app_state.cookie_key).add(oauth_csrf_cookie(
        OAUTH_DEVICE_CSRF_COOKIE,
        device_csrf.secret(),
        !app_state.dev_mode,
    ));

    let auth_request = client
        .authorize_url(|| CsrfToken::new(state_value))
        .add_scope(Scope::new("openid".to_string()))
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()));

    let (auth_url, _csrf_token) = auth_request.url();

    Ok(Redirect::temporary(auth_url.as_str()).into_response())
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
) -> Result<impl IntoResponse, StatusCode> {
    let client = app_state
        .oauth_basic_client
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let http_client = oauth2::reqwest::ClientBuilder::new()
        .redirect(oauth2::reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| {
            error!("Failed to build OAuth HTTP client: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Verify CSRF state token before exchanging the OAuth code.
    let device_state = query.state.as_deref().and_then(parse_device_oauth_state);
    if query
        .state
        .as_deref()
        .is_some_and(|state| state.starts_with("device:"))
        && device_state.is_none()
    {
        error!("Device OAuth callback: malformed state");
        return Err(StatusCode::FORBIDDEN);
    }

    if let Some(ref device_state) = device_state {
        let csrf_cookie = cookies
            .signed(&app_state.cookie_key)
            .get(OAUTH_DEVICE_CSRF_COOKIE)
            .ok_or_else(|| {
                error!("Device OAuth callback: missing CSRF cookie");
                StatusCode::FORBIDDEN
            })?;

        if csrf_cookie.value() != device_state.csrf_nonce {
            error!("Device OAuth callback: CSRF token mismatch");
            return Err(StatusCode::FORBIDDEN);
        }

        cookies
            .signed(&app_state.cookie_key)
            .add(remove_oauth_csrf_cookie(OAUTH_DEVICE_CSRF_COOKIE));
    } else {
        let csrf_cookie = cookies
            .signed(&app_state.cookie_key)
            .get(OAUTH_CSRF_COOKIE)
            .ok_or_else(|| {
                error!("OAuth callback: missing CSRF cookie");
                StatusCode::FORBIDDEN
            })?;

        let state_value = query.state.as_deref().unwrap_or("");
        if csrf_cookie.value() != state_value {
            error!("OAuth callback: CSRF token mismatch");
            return Err(StatusCode::FORBIDDEN);
        }

        // Consume the CSRF cookie
        cookies
            .signed(&app_state.cookie_key)
            .add(remove_oauth_csrf_cookie(OAUTH_CSRF_COOKIE));
    }

    // Exchange code for token
    let token: oauth2::StandardTokenResponse<
        oauth2::EmptyExtraTokenFields,
        oauth2::basic::BasicTokenType,
    > = client
        .exchange_code(AuthorizationCode::new(query.code))
        .request_async(&http_client)
        .await
        .map_err(|e| {
            error!("Failed to exchange code: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    // Fetch user info from Google
    let client = reqwest::Client::new();
    let user_info: GoogleUserInfo = client
        .get("https://www.googleapis.com/oauth2/v3/userinfo")
        .bearer_auth(token.access_token().secret())
        .send()
        .await
        .map_err(|e| {
            error!("Failed to fetch user info: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .json()
        .await
        .map_err(|e| {
            error!("Failed to parse user info: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    info!("User authenticated: {}", user_info.email);

    // Check email access control
    if let Err(redirect) = check_email_allowed(&app_state, &user_info.email) {
        return Ok(redirect);
    }

    // Save or update user in database
    let mut conn = app_state.db_pool.get().map_err(|e| {
        error!("Failed to get db connection: {}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    use crate::schema::users::dsl::*;

    let user = users
        .filter(google_id.eq(&user_info.sub))
        .first::<User>(&mut conn)
        .optional()
        .map_err(|e| {
            error!("Failed to look up user by google_id: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

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
                .map_err(|e| {
                    error!("Failed to create user: {}", e);
                    StatusCode::INTERNAL_SERVER_ERROR
                })?
        }
    };

    // Check if user is banned
    if user.disabled {
        let reason = user.ban_reason.as_deref().unwrap_or("No reason provided");
        info!("Banned user {} attempted login", user.email);
        return Ok(banned_redirect(reason));
    }

    if let Some(device_state) = device_state {
        add_session_cookie(&cookies, &app_state, &user);

        // Redirect back to device verify page to show approval UI
        info!(
            "OAuth complete for device flow, redirecting to approval page for user: {}",
            user.email
        );
        return Ok(device_approval_redirect(&device_state.user_code));
    }

    add_session_cookie(&cookies, &app_state, &user);

    Ok(Redirect::temporary(routes::DASHBOARD))
}

pub async fn me(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<MeResponse>, StatusCode> {
    let user = crate::auth::extract_user(&app_state, &cookies).map_err(|e| match e {
        crate::errors::AppError::Forbidden => StatusCode::FORBIDDEN,
        crate::errors::AppError::DbPool | crate::errors::AppError::DbQuery(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        _ => StatusCode::UNAUTHORIZED,
    })?;

    Ok(Json(MeResponse {
        id: user.id,
        email: user.email,
        name: user.name,
        avatar_url: user.avatar_url,
        is_admin: user.is_admin,
    }))
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
) -> Result<impl IntoResponse, StatusCode> {
    use crate::schema::users::dsl::*;

    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let user = users
        .filter(email.eq("testing@testing.local"))
        .first::<User>(&mut conn)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    // Check if user is banned
    if user.disabled {
        let reason = user.ban_reason.as_deref().unwrap_or("No reason provided");
        info!("Banned user {} attempted dev login", user.email);
        return Ok(banned_redirect(reason));
    }

    info!("Dev mode: auto-logged in as testing@testing.local");

    add_session_cookie(&cookies, &app_state, &user);

    // Redirect to dashboard
    Ok(Redirect::temporary(routes::DASHBOARD))
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

fn oauth_csrf_cookie(name: &'static str, value: &str, secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::new(name, value.to_owned());
    cookie.set_path(routes::AUTH_GOOGLE_CALLBACK);
    cookie.set_http_only(true);
    cookie.set_secure(secure);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::minutes(10));
    cookie
}

fn remove_oauth_csrf_cookie(name: &'static str) -> Cookie<'static> {
    let mut cookie = Cookie::new(name, "");
    cookie.set_path(routes::AUTH_GOOGLE_CALLBACK);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::ZERO);
    cookie
}

fn build_device_oauth_state(user_code: &str, csrf_nonce: &str) -> String {
    format!("device:{user_code}:{csrf_nonce}")
}

#[derive(Debug, PartialEq, Eq)]
struct DeviceOAuthState {
    user_code: String,
    csrf_nonce: String,
}

fn parse_device_oauth_state(state: &str) -> Option<DeviceOAuthState> {
    let state = state.strip_prefix("device:")?;
    let (user_code, csrf_nonce) = state.rsplit_once(':')?;
    if user_code.is_empty() || csrf_nonce.is_empty() {
        return None;
    }

    Some(DeviceOAuthState {
        user_code: user_code.to_owned(),
        csrf_nonce: csrf_nonce.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_oauth_state_round_trips_user_code_and_nonce() {
        let state = build_device_oauth_state("ABC-123", "nonce-value");

        assert_eq!(
            parse_device_oauth_state(&state),
            Some(DeviceOAuthState {
                user_code: "ABC-123".to_string(),
                csrf_nonce: "nonce-value".to_string(),
            })
        );
    }

    #[test]
    fn device_oauth_state_rejects_legacy_or_malformed_state() {
        assert_eq!(parse_device_oauth_state("device:ABC-123"), None);
        assert_eq!(parse_device_oauth_state("device::nonce"), None);
        assert_eq!(parse_device_oauth_state("device:ABC-123:"), None);
        assert_eq!(parse_device_oauth_state("regular-state"), None);
    }

    #[test]
    fn encode_query_value_keeps_unreserved_and_encodes_reserved_chars() {
        assert_eq!(
            encode_query_value("ABC-123 reason?/"),
            "ABC-123+reason%3F%2F"
        );
    }
}
