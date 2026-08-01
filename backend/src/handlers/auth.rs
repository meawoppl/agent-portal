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
    models::User,
    routes,
    schema::proxy_auth_tokens,
    AppState,
};

use shared::protocol::SESSION_COOKIE_NAME;

mod identity;
mod oauth;

pub use identity::{ProviderIdentity, PROVIDER_GITHUB, PROVIDER_GOOGLE};

const MOBILE_TOKEN_TTL_DAYS: u32 = 30;
const TOKEN_REFRESH_GRACE_MINUTES: i64 = 10;

/// Regular web login via Google.
pub async fn login(State(app_state): State<Arc<AppState>>, cookies: Cookies) -> impl IntoResponse {
    start_login(&oauth::GOOGLE, &app_state, &cookies)
}

/// Regular web login via GitHub.
pub async fn github_login(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> impl IntoResponse {
    start_login(&oauth::GITHUB, &app_state, &cookies)
}

/// Begin the authorization-code flow for one provider.
///
/// With no client configured this falls through to dev login, which is only
/// reachable in dev mode — outside it, boot fails unless a provider is
/// configured, so the fallback cannot silently become the production path.
fn start_login(
    provider: &oauth::Provider,
    app_state: &AppState,
    cookies: &Cookies,
) -> axum::response::Response {
    let client = app_state.oauth.client(provider.key);
    info!(
        target: "auth_audit",
        event = "web_login_start",
        provider = provider.key,
        oauth_configured = client.is_some(),
    );
    let Some(client) = client else {
        info!(
            target: "auth_audit",
            event = "web_login_dev_redirect",
            provider = provider.key,
        );
        return Redirect::temporary(routes::AUTH_DEV_LOGIN).into_response();
    };

    oauth::regular_authorization_redirect(provider, client, cookies, app_state).into_response()
}

/// Device flow login - separate endpoint that stores device_user_code in state
/// This is used when the user needs to authenticate before approving a device
#[derive(Debug, Deserialize)]
pub struct DeviceLoginQuery {
    pub device_user_code: String,
    /// Which provider to authenticate with. Absent (the CLI never sends it)
    /// means "whichever is configured first", so a deploy that enables only
    /// GitHub still has a working device flow.
    #[serde(default)]
    pub provider: Option<String>,
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
        oauth_configured = !app_state.oauth.enabled().is_empty(),
    );
    // In dev mode, auto-login and redirect to device approval page
    if app_state.oauth.enabled().is_empty() {
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

    let requested = query
        .provider
        .as_deref()
        .or_else(|| app_state.oauth.enabled().first().copied())
        .unwrap_or(PROVIDER_GOOGLE);
    let provider =
        oauth::by_key(requested).ok_or(AppError::ServiceUnavailable("Unknown login provider"))?;
    let client = app_state
        .oauth
        .client(provider.key)
        .ok_or(AppError::ServiceUnavailable("OAuth client not configured"))?;

    Ok(oauth::device_authorization_redirect(
        provider,
        client,
        &cookies,
        &app_state,
        &query.device_user_code,
    )
    .into_response())
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
    /// Google's own verification flag. Previously ignored; the account-linking
    /// rule (#1535) requires it, and an unverified Google address must not be
    /// able to claim an allow-listed domain. Defaults to `false` so a response
    /// missing the field fails closed.
    #[serde(default)]
    email_verified: bool,
    name: Option<String>,
    picture: Option<String>,
}

/// GitHub's `/user` response. `id` is numeric and immutable — the login handle
/// is renameable, so it must not be used as the subject.
///
/// `email` is deliberately ignored here even though the field exists: it is
/// null when the user keeps their address private, and GitHub attaches no
/// verification flag to it. The address comes from `/user/emails` instead.
#[derive(Debug, Deserialize)]
struct GitHubUserInfo {
    id: u64,
    name: Option<String>,
    avatar_url: Option<String>,
}

/// One entry of GitHub's `/user/emails` response.
#[derive(Debug, Deserialize)]
struct GitHubEmail {
    email: String,
    primary: bool,
    verified: bool,
}

pub async fn callback(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Query(query): Query<AuthCallbackQuery>,
) -> Result<impl IntoResponse, AppError> {
    provider_callback(&oauth::GOOGLE, app_state, cookies, query).await
}

pub async fn github_callback(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Query(query): Query<AuthCallbackQuery>,
) -> Result<impl IntoResponse, AppError> {
    provider_callback(&oauth::GITHUB, app_state, cookies, query).await
}

async fn provider_callback(
    provider: &oauth::Provider,
    app_state: Arc<AppState>,
    cookies: Cookies,
    query: AuthCallbackQuery,
) -> Result<axum::response::Response, AppError> {
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
        provider = provider.key,
        flow = callback_kind,
        state_present = query.state.is_some(),
    );

    let client = app_state
        .oauth
        .client(provider.key)
        .ok_or(AppError::ServiceUnavailable("OAuth client not configured"))?;
    let http_client = oauth2::reqwest::ClientBuilder::new()
        .redirect(oauth2::reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AppError::Internal(format!("Failed to build OAuth HTTP client: {e}")))?;

    let device_state =
        oauth::validate_callback_state(provider, &cookies, &app_state, query.state.as_deref())
            .inspect_err(|_| {
                warn!(
                    target: "auth_audit",
                    event = "oauth_callback_denied",
                    provider = provider.key,
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

    let resolved_identity = fetch_provider_identity(provider, token.access_token().secret())
        .await
        .inspect_err(|_| {
            warn!(
                target: "auth_audit",
                event = "oauth_callback_failed",
                provider = provider.key,
                flow = callback_kind,
                stage = "userinfo",
            );
        })?;

    // An address the provider has not vouched for must not satisfy the
    // allow-list — otherwise anyone could claim a permitted domain. Treat it as
    // absent here and let `resolve_user` refuse below.
    let allowlist_email = resolved_identity
        .email
        .as_deref()
        .filter(|_| resolved_identity.email_verified)
        .unwrap_or_default();
    if let Err(redirect) = check_email_allowed(&app_state, allowlist_email) {
        warn!(
            target: "auth_audit",
            event = "oauth_callback_denied",
            provider = provider.key,
            flow = callback_kind,
            reason = "email_not_allowed",
        );
        return Ok(redirect.into_response());
    }

    info!("User authenticated via {}", provider.key);

    // Resolve the login to a user: existing identity, link on a verified email,
    // or create. See `identity` for the rule and why linking is gated on
    // verification (#1535).
    let mut conn = app_state.conn()?;

    let resolved = identity::resolve_user(&mut conn, &resolved_identity)
        .map_err(|e| AppError::DbQuery(format!("Failed to resolve login identity: {e}")))?;

    let user = match resolved {
        Ok(user) => user,
        Err(identity::IdentityError::UnverifiedEmail) => {
            warn!(
                target: "auth_audit",
                event = "oauth_callback_denied",
                provider = provider.key,
                flow = callback_kind,
                reason = "unverified_email",
            );
            return Ok(unverified_email_redirect().into_response());
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
        return Ok(banned_redirect(reason).into_response());
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
        return Ok(device_approval_redirect(&device_state.user_code).into_response());
    }

    add_session_cookie(&cookies, &app_state, &user);
    info!(
        target: "auth_audit",
        event = "oauth_callback_success",
        flow = "web",
        user_id = %user.id,
        user_email = %user.email,
    );

    Ok(Redirect::temporary(routes::DASHBOARD).into_response())
}

/// GitHub rejects API requests without a `User-Agent`, so every call sends one.
const GITHUB_USER_AGENT: &str = "agent-portal";

/// Ask the provider who just authorized us, normalized to a [`ProviderIdentity`].
async fn fetch_provider_identity(
    provider: &oauth::Provider,
    access_token: &str,
) -> Result<ProviderIdentity, AppError> {
    let http = reqwest::Client::new();
    match provider.key {
        PROVIDER_GOOGLE => google_identity(&http, access_token).await,
        PROVIDER_GITHUB => github_identity(&http, access_token).await,
        other => Err(AppError::Internal(format!("Unknown provider: {other}"))),
    }
}

/// Must stay on the OIDC (`v3`) userinfo endpoint: it spells the verification
/// flag `email_verified`, whereas the legacy `v2` endpoint spells it
/// `verified_email`. Since the field fails closed, switching endpoints without
/// renaming it would reject every login rather than fail loudly.
async fn google_identity(
    http: &reqwest::Client,
    access_token: &str,
) -> Result<ProviderIdentity, AppError> {
    let info: GoogleUserInfo = http
        .get("https://www.googleapis.com/oauth2/v3/userinfo")
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to fetch Google user info: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to parse Google user info: {e}")))?;

    Ok(ProviderIdentity {
        provider: PROVIDER_GOOGLE,
        subject: info.sub,
        email: Some(info.email),
        email_verified: info.email_verified,
        name: info.name,
        avatar_url: info.picture,
    })
}

/// GitHub needs two calls: `/user` for the immutable numeric id and profile,
/// and `/user/emails` for an address we can trust. `/user` alone is not enough —
/// its `email` is null for users with a private address and carries no
/// verification flag, and an unverified address must never link an account.
async fn github_identity(
    http: &reqwest::Client,
    access_token: &str,
) -> Result<ProviderIdentity, AppError> {
    let info: GitHubUserInfo = http
        .get("https://api.github.com/user")
        .bearer_auth(access_token)
        .header(header::USER_AGENT, GITHUB_USER_AGENT)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to fetch GitHub user: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to parse GitHub user: {e}")))?;

    let emails: Vec<GitHubEmail> = http
        .get("https://api.github.com/user/emails")
        .bearer_auth(access_token)
        .header(header::USER_AGENT, GITHUB_USER_AGENT)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to fetch GitHub emails: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to parse GitHub emails: {e}")))?;

    let email = primary_verified_email(&emails);

    Ok(ProviderIdentity {
        provider: PROVIDER_GITHUB,
        subject: info.id.to_string(),
        email_verified: email.is_some(),
        email,
        name: info.name,
        avatar_url: info.avatar_url,
    })
}

/// Pick the address GitHub considers primary **and** verified.
///
/// Both conditions are required, and neither is inferable from the other: a
/// user may have several verified addresses, and the primary one may be
/// unverified. Returning `None` makes the caller refuse the login rather than
/// trust an address the user has not proven they control.
fn primary_verified_email(emails: &[GitHubEmail]) -> Option<String> {
    emails
        .iter()
        .find(|e| e.primary && e.verified)
        .map(|e| e.email.clone())
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

/// The provider gave us no verified email and we have never seen this identity,
/// so we refuse rather than silently forking a second account for the same
/// person (#1535). Reuses the access-denied page with an explanatory reason.
fn unverified_email_redirect() -> Redirect {
    Redirect::temporary(&format!(
        "{}?reason={}",
        routes::ACCESS_DENIED,
        encode_query_value(
            "Your identity provider did not supply a verified email address. \
             Verify your email with that provider, then sign in again."
        )
    ))
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

    fn gh_email(email: &str, primary: bool, verified: bool) -> GitHubEmail {
        GitHubEmail {
            email: email.to_string(),
            primary,
            verified,
        }
    }

    #[test]
    fn github_picks_the_primary_verified_address() {
        let emails = vec![
            gh_email("alt@example.invalid", false, true),
            gh_email("main@example.invalid", true, true),
        ];
        assert_eq!(
            primary_verified_email(&emails),
            Some("main@example.invalid".to_string())
        );
    }

    /// An unverified primary must not fall back to some other verified address:
    /// the account the person is signing into is the primary one, and silently
    /// substituting a different address would link the wrong portal account.
    #[test]
    fn github_refuses_when_the_primary_address_is_unverified() {
        let emails = vec![
            gh_email("main@example.invalid", true, false),
            gh_email("alt@example.invalid", false, true),
        ];
        assert_eq!(primary_verified_email(&emails), None);
    }

    #[test]
    fn github_refuses_when_no_address_is_verified() {
        let emails = vec![gh_email("main@example.invalid", true, false)];
        assert_eq!(primary_verified_email(&emails), None);
        assert_eq!(primary_verified_email(&[]), None);
    }

    /// GitHub ids are numeric; the subject must be the id and not the
    /// renameable login handle.
    #[test]
    fn github_user_info_parses_the_numeric_id() {
        let body = r#"{
            "id": 583231,
            "login": "octocat",
            "name": "The Octocat",
            "avatar_url": "https://example.invalid/a.png"
        }"#;
        let info: GitHubUserInfo = serde_json::from_str(body).expect("parses /user");
        assert_eq!(info.id.to_string(), "583231");
        assert_eq!(info.name.as_deref(), Some("The Octocat"));
    }

    /// Pins the field names of the OIDC (`v3`) userinfo response. The legacy
    /// `v2` endpoint spells the flag `verified_email`, and because the field
    /// fails closed, getting this wrong rejects every Google login rather than
    /// failing loudly — so assert the happy-path payload parses as verified.
    #[test]
    fn google_userinfo_parses_the_oidc_verification_flag() {
        let body = r#"{
            "sub": "1234567890",
            "email": "person@example.com",
            "email_verified": true,
            "name": "A Person",
            "picture": "https://example.invalid/p.png"
        }"#;
        let info: GoogleUserInfo = serde_json::from_str(body).expect("parses v3 userinfo");
        assert_eq!(info.sub, "1234567890");
        assert!(info.email_verified);

        // A response without the flag must fail closed, not default to trusted.
        let missing = r#"{"sub": "1", "email": "p@example.com"}"#;
        let info: GoogleUserInfo = serde_json::from_str(missing).expect("parses");
        assert!(
            !info.email_verified,
            "a missing verification flag must not be treated as verified"
        );
    }

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
