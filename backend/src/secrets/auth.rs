//! Human vault access is browser-only; agent access uses explicit bound approvals.
use crate::{errors::AppError, AppState};
use axum::http::HeaderMap;
use uuid::Uuid;
#[derive(Clone, Copy)]
pub struct Principal {
    pub user: Uuid,
    pub browser: bool,
    pub metadata: bool,
    pub mutation: bool,
}
/// Ordinary, linked and retired vault tokens must never fall through to a
/// browser cookie or gain general vault authority. Bound agent routes have
/// their own process/session/approval checks.
pub fn require_browser_headers(headers: &HeaderMap) -> Result<(), AppError> {
    if headers.contains_key(axum::http::header::AUTHORIZATION) {
        Err(AppError::Forbidden)
    } else {
        Ok(())
    }
}
pub fn csrf(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    if headers
        .get("x-portal-secrets")
        .and_then(|v| v.to_str().ok())
        != Some("1")
    {
        return Err(AppError::Forbidden);
    };
    let origin = headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::Forbidden)?;
    let expected = reqwest::Url::parse(&state.public_url)
        .map_err(|_| AppError::Forbidden)?
        .origin()
        .ascii_serialization();
    if origin != expected {
        return Err(AppError::Forbidden);
    };
    Ok(())
}
const BROWSER_COOKIE: &str = "portal_secrets_browser";
const VERIFIED_COOKIE: &str = "portal_secrets_verified";
/// Explicit verification lasts for this browser session, not ten minutes from
/// login. Ordinary login clears it; the explicit browser-only session check sets it.
pub fn record_verification(cookies: &tower_cookies::Cookies, state: &AppState, user: Uuid) {
    let mut cookie = verification_cookie(state);
    let context =
        browser_context(cookies, state).unwrap_or_else(|| begin_verification(cookies, state));
    cookie.set_value(format!("{user}:{context}"));
    cookies.signed(&state.cookie_key).add(cookie);
}
fn verification_cookie(state: &AppState) -> tower_cookies::Cookie<'static> {
    let mut cookie = tower_cookies::Cookie::new(VERIFIED_COOKIE, "");
    cookie.set_path("/api/secrets");
    cookie.set_http_only(true);
    cookie.set_secure(!state.dev_mode);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Strict);
    cookie
}
pub fn browser_context(cookies: &tower_cookies::Cookies, state: &AppState) -> Option<String> {
    cookies
        .signed(&state.cookie_key)
        .get(BROWSER_COOKIE)
        .map(|c| c.value().to_owned())
}
fn browser_cookie(state: &AppState) -> tower_cookies::Cookie<'static> {
    let mut cookie = tower_cookies::Cookie::new(BROWSER_COOKIE, "");
    cookie.set_path("/api");
    cookie.set_http_only(true);
    cookie.set_secure(!state.dev_mode);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Lax);
    cookie
}
/// Bind each provider round-trip to this browser's verification attempt.
pub fn begin_verification(cookies: &tower_cookies::Cookies, state: &AppState) -> String {
    let context = Uuid::new_v4().to_string();
    let mut cookie = browser_cookie(state);
    cookie.set_value(context.clone());
    cookies.signed(&state.cookie_key).add(cookie);
    context
}
pub fn clear_verification(cookies: &tower_cookies::Cookies, state: &AppState) {
    let mut cookie = verification_cookie(state);
    cookie.make_removal();
    cookies.signed(&state.cookie_key).add(cookie);
    let mut context = browser_cookie(state);
    context.make_removal();
    cookies.signed(&state.cookie_key).add(context);
}
pub fn require_verification(
    cookies: &tower_cookies::Cookies,
    state: &AppState,
    user: Uuid,
) -> Result<(), AppError> {
    let cookie = cookies
        .signed(&state.cookie_key)
        .get(VERIFIED_COOKIE)
        .ok_or(AppError::Forbidden)?;
    let context = browser_context(cookies, state).ok_or(AppError::Forbidden)?;
    if cookie.value() != format!("{user}:{context}") {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_verification_has_browser_session_lifetime() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let state = crate::test_support::test_app_state(pool);
        let cookie = verification_cookie(&state);
        assert_eq!(cookie.path(), Some("/api/secrets"));
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(
            cookie.same_site(),
            Some(tower_cookies::cookie::SameSite::Strict)
        );
        assert_eq!(cookie.max_age(), None);
    }
    #[test]
    fn browser_csrf_requires_both_exact_origin_and_custom_header() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let state = crate::test_support::test_app_state(pool);
        let mut headers = HeaderMap::new();
        assert!(csrf(&state, &headers).is_err());
        headers.insert(
            "x-portal-secrets",
            axum::http::HeaderValue::from_static("1"),
        );
        headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("https://attacker.example"),
        );
        assert!(csrf(&state, &headers).is_err());
        headers.insert(
            "origin",
            axum::http::HeaderValue::from_static("http://localhost:3000"),
        );
        assert!(csrf(&state, &headers).is_ok());
        headers.insert(
            "authorization",
            axum::http::HeaderValue::from_static("Basic invalid"),
        );
        assert!(
            require_browser_headers(&headers).is_err(),
            "Malformed credentials must not fall back to a browser cookie"
        );
    }
    #[test]
    fn all_bearer_credentials_are_rejected_for_browser_vault_access() {
        for token in ["aps_secret_retired", "session-jwt", "linked-token"] {
            let mut headers = HeaderMap::new();
            headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
            assert!(require_browser_headers(&headers).is_err());
        }
        assert!(require_browser_headers(&HeaderMap::new()).is_ok());
    }
}
