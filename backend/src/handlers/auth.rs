use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect},
    Json,
};
use diesel::prelude::*;
use oauth2::{AuthorizationCode, CsrfToken, Scope, TokenResponse};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_cookies::{cookie::SameSite, Cookie, Cookies};
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    models::{NewUser, User},
    AppState,
};

use shared::protocol::SESSION_COOKIE_NAME;

const OAUTH_CSRF_COOKIE: &str = "oauth_csrf";

/// Regular web login - redirects to Google OAuth
pub async fn login(State(app_state): State<Arc<AppState>>, cookies: Cookies) -> impl IntoResponse {
    let client = match &app_state.oauth_basic_client {
        Some(c) => c,
        None => return Redirect::temporary("/api/auth/dev-login").into_response(),
    };

    let auth_request = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("openid".to_string()))
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()));

    let (auth_url, csrf_token) = auth_request.url();

    // Store the CSRF token in a short-lived signed cookie so we can verify it on callback.
    let mut csrf_cookie = Cookie::new(OAUTH_CSRF_COOKIE, csrf_token.secret().clone());
    csrf_cookie.set_path("/api/auth/google/callback");
    csrf_cookie.set_http_only(true);
    csrf_cookie.set_secure(!app_state.dev_mode);
    csrf_cookie.set_same_site(SameSite::Lax);
    csrf_cookie.set_max_age(tower_cookies::cookie::time::Duration::minutes(10));
    cookies.signed(&app_state.cookie_key).add(csrf_cookie);

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
            return Ok(Redirect::temporary("/banned").into_response());
        }

        info!("Dev mode: auto-logged in for device flow");

        let mut cookie = Cookie::new(SESSION_COOKIE_NAME, user.id.to_string());
        cookie.set_path("/");
        cookie.set_http_only(true);
        cookie.set_secure(false);
        cookie.set_same_site(SameSite::Lax);
        cookies.signed(&app_state.cookie_key).add(cookie);

        return Ok(Redirect::temporary(&format!(
            "/api/auth/device?user_code={}",
            query.device_user_code
        ))
        .into_response());
    }

    let client = app_state.oauth_basic_client.as_ref().unwrap();

    // Use a prefixed state to identify this as a device flow callback
    let state_value = format!("device:{}", query.device_user_code);

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

    // Verify CSRF state token for regular web logins.
    // Device flow uses a deterministic "device:{code}" state so we skip it there.
    let is_device_flow = query
        .state
        .as_deref()
        .is_some_and(|s| s.starts_with("device:"));

    if !is_device_flow {
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
        let mut remove_csrf = Cookie::new(OAUTH_CSRF_COOKIE, "");
        remove_csrf.set_path("/api/auth/google/callback");
        remove_csrf.set_max_age(tower_cookies::cookie::time::Duration::ZERO);
        cookies.signed(&app_state.cookie_key).add(remove_csrf);
    }

    // Exchange code for token
    let token: oauth2::StandardTokenResponse<
        oauth2::EmptyExtraTokenFields,
        oauth2::basic::BasicTokenType,
    > = client
        .exchange_code(AuthorizationCode::new(query.code))
        .request_async(oauth2::reqwest::async_http_client)
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
        // URL encode the reason for the query parameter
        let encoded_reason: String = reason
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
            .collect();
        info!("Banned user {} attempted login", user.email);
        return Ok(Redirect::temporary(&format!(
            "/banned?reason={}",
            encoded_reason
        )));
    }

    // Check if this is part of a device flow (state starts with "device:")
    if let Some(ref state) = query.state {
        if let Some(device_user_code) = state.strip_prefix("device:") {
            // Set session cookie first so user is logged in
            let mut cookie = Cookie::new(SESSION_COOKIE_NAME, user.id.to_string());
            cookie.set_path("/");
            cookie.set_http_only(true);
            cookie.set_secure(!app_state.dev_mode);
            cookie.set_same_site(SameSite::Lax);
            cookies.signed(&app_state.cookie_key).add(cookie);

            // Redirect back to device verify page to show approval UI
            info!(
                "OAuth complete for device flow, redirecting to approval page for user: {}",
                user.email
            );
            return Ok(Redirect::temporary(&format!(
                "/api/auth/device?user_code={}",
                device_user_code
            )));
        }
    }

    // Set session cookie with user ID
    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, user.id.to_string());
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_secure(!app_state.dev_mode); // Don't require HTTPS in dev mode
    cookie.set_same_site(SameSite::Lax);
    cookies.signed(&app_state.cookie_key).add(cookie);

    Ok(Redirect::temporary("/dashboard"))
}

#[derive(Debug, Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub is_admin: bool,
    pub voice_enabled: bool,
}

pub async fn me(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<UserResponse>, StatusCode> {
    // Extract user ID from signed session cookie
    let cookie = cookies
        .signed(&app_state.cookie_key)
        .get(SESSION_COOKIE_NAME)
        .ok_or(StatusCode::UNAUTHORIZED)?;

    let user_id: Uuid = cookie
        .value()
        .parse()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // Fetch user from database
    use crate::schema::users::dsl::*;
    let mut conn = app_state
        .db_pool
        .get()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let user = users
        .find(user_id)
        .first::<User>(&mut conn)
        .map_err(|_| StatusCode::NOT_FOUND)?;

    Ok(Json(UserResponse {
        id: user.id,
        email: user.email,
        name: user.name,
        avatar_url: user.avatar_url,
        is_admin: user.is_admin,
        voice_enabled: user.voice_enabled,
    }))
}

pub async fn logout(State(app_state): State<Arc<AppState>>, cookies: Cookies) -> impl IntoResponse {
    // Remove session cookie by setting it with empty value and immediate expiry
    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, "");
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_secure(true);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::ZERO);
    cookies.signed(&app_state.cookie_key).add(cookie);

    info!("User logged out");
    Redirect::temporary("/")
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
        let encoded_reason: String = reason
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
            .collect();
        info!("Banned user {} attempted dev login", user.email);
        return Ok(Redirect::temporary(&format!(
            "/banned?reason={}",
            encoded_reason
        )));
    }

    info!("Dev mode: auto-logged in as testing@testing.local");

    // Set session cookie with user ID
    let mut cookie = Cookie::new(SESSION_COOKIE_NAME, user.id.to_string());
    cookie.set_path("/");
    cookie.set_http_only(true);
    cookie.set_secure(!app_state.dev_mode); // Don't require HTTPS in dev mode
    cookie.set_same_site(SameSite::Lax);
    cookies.signed(&app_state.cookie_key).add(cookie);

    // Redirect to dashboard
    Ok(Redirect::temporary("/dashboard"))
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
    Err(Redirect::temporary("/access-denied"))
}
