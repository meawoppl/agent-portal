use axum::{
    extract::{Query, State},
    response::{IntoResponse, Redirect},
    Json,
};
use diesel::prelude::*;
use oauth2::{AuthorizationCode, TokenResponse};
use serde::Deserialize;
use shared::api::MeResponse;
use std::sync::Arc;
use tower_cookies::{cookie::SameSite, Cookie, Cookies};
use tracing::info;

use crate::{
    errors::AppError,
    models::{NewUser, User},
    routes, AppState,
};

use shared::protocol::SESSION_COOKIE_NAME;

mod oauth;

/// Regular web login - redirects to Google OAuth
pub async fn login(State(app_state): State<Arc<AppState>>, cookies: Cookies) -> impl IntoResponse {
    let client = match &app_state.oauth_basic_client {
        Some(c) => c,
        None => return Redirect::temporary(routes::AUTH_DEV_LOGIN).into_response(),
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
    // In dev mode, auto-login and redirect to device approval page
    if app_state.oauth_basic_client.is_none() {
        use crate::schema::users::dsl::*;

        let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

        let user = users
            .filter(email.eq("testing@testing.local"))
            .first::<User>(&mut conn)
            .map_err(dev_user_lookup_error)?;

        if user.disabled {
            info!("Banned user {} attempted dev device login", user.email);
            return Ok(Redirect::temporary(routes::BANNED).into_response());
        }

        info!("Dev mode: auto-logged in for device flow");

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
    let client = app_state
        .oauth_basic_client
        .as_ref()
        .ok_or(AppError::ServiceUnavailable("OAuth client not configured"))?;
    let http_client = oauth2::reqwest::ClientBuilder::new()
        .redirect(oauth2::reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AppError::Internal(format!("Failed to build OAuth HTTP client: {e}")))?;

    let device_state =
        oauth::validate_callback_state(&cookies, &app_state, query.state.as_deref())?;

    // Exchange code for token
    let token: oauth2::StandardTokenResponse<
        oauth2::EmptyExtraTokenFields,
        oauth2::basic::BasicTokenType,
    > = client
        .exchange_code(AuthorizationCode::new(query.code))
        .request_async(&http_client)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to exchange OAuth code: {e}")))?;

    // Fetch user info from Google
    let client = reqwest::Client::new();
    let user_info: GoogleUserInfo = client
        .get("https://www.googleapis.com/oauth2/v3/userinfo")
        .bearer_auth(token.access_token().secret())
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to fetch Google user info: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to parse Google user info: {e}")))?;

    info!("User authenticated: {}", user_info.email);

    // Check email access control
    if let Err(redirect) = check_email_allowed(&app_state, &user_info.email) {
        return Ok(redirect);
    }

    // Save or update user in database
    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

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
) -> Result<Json<MeResponse>, AppError> {
    let user = crate::auth::extract_user(&app_state, &cookies).map_err(auth_user_error)?;

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
) -> Result<impl IntoResponse, AppError> {
    use crate::schema::users::dsl::*;

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    let user = users
        .filter(email.eq("testing@testing.local"))
        .first::<User>(&mut conn)
        .map_err(dev_user_lookup_error)?;

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
}
