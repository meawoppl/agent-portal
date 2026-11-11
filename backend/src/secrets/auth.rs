//! Dedicated human vault credentials are deliberately unrelated to proxy JWTs.
use crate::{errors::AppError, schema::secret_credentials, AppState};
use axum::http::HeaderMap;
use chrono::Utc;
use diesel::prelude::*;
use uuid::Uuid;
#[derive(Clone, Copy)]
pub struct Principal {
    pub user: Uuid,
    pub browser: bool,
    pub metadata: bool,
    pub mutation: bool,
    pub read: bool,
}
pub fn credential(state: &AppState, headers: &HeaderMap) -> Result<Option<Principal>, AppError> {
    let Some(token) = crate::auth::bearer_token(headers) else {
        if headers.contains_key(axum::http::header::AUTHORIZATION) {
            return Err(AppError::Unauthorized);
        }
        return Ok(None);
    };
    if !token.starts_with("aps_secret_") {
        return Err(AppError::Forbidden);
    };
    let mut conn = state.conn()?;
    let (user, metadata, mutation, read) = secret_credentials::table
        .filter(secret_credentials::token_hash.eq(crate::jwt::hash_token(token)))
        .filter(secret_credentials::revoked_at.is_null())
        .filter(secret_credentials::expires_at.gt(Utc::now()))
        .select((
            secret_credentials::user_id,
            secret_credentials::metadata_scope,
            secret_credentials::mutation_scope,
            secret_credentials::read_scope,
        ))
        .first::<(Uuid, bool, bool, bool)>(&mut conn)
        .map_err(|_| AppError::Unauthorized)?;
    super::store::active_user(&mut conn, user)?;
    Ok(Some(Principal {
        user,
        browser: false,
        metadata,
        mutation,
        read,
    }))
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
pub fn require_read(principal: Principal) -> Result<(), AppError> {
    if principal.browser || !principal.read {
        Err(AppError::Forbidden)
    } else {
        Ok(())
    }
}

const RECENT_COOKIE: &str = "portal_secrets_recent_auth";
pub fn record_recent_auth(cookies: &tower_cookies::Cookies, state: &AppState, user: Uuid) {
    let mut cookie = tower_cookies::Cookie::new(
        RECENT_COOKIE,
        format!("{}:{}", user, Utc::now().timestamp()),
    );
    cookie.set_path("/api/secrets");
    cookie.set_http_only(true);
    cookie.set_secure(!state.dev_mode);
    cookie.set_same_site(tower_cookies::cookie::SameSite::Strict);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::minutes(10));
    cookies.signed(&state.cookie_key).add(cookie);
}
pub fn require_recent_auth(
    cookies: &tower_cookies::Cookies,
    state: &AppState,
    user: Uuid,
) -> Result<(), AppError> {
    let cookie = cookies
        .signed(&state.cookie_key)
        .get(RECENT_COOKIE)
        .ok_or(AppError::Forbidden)?;
    let (owner, time) = cookie.value().rsplit_once(':').ok_or(AppError::Forbidden)?;
    let time = time.parse::<i64>().map_err(|_| AppError::Forbidden)?;
    let age = Utc::now().timestamp() - time;
    if owner != user.to_string() || !(0..=600).contains(&age) {
        return Err(AppError::Forbidden);
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
            credential(&state, &headers).is_err(),
            "Malformed credentials must not fall back to a browser cookie"
        );
    }
    #[test]
    fn ordinary_credentials_never_gain_value_read_scope() {
        assert!(require_read(Principal {
            user: Uuid::new_v4(),
            browser: true,
            metadata: true,
            mutation: true,
            read: true
        })
        .is_err());
        assert!(require_read(Principal {
            user: Uuid::new_v4(),
            browser: false,
            metadata: true,
            mutation: true,
            read: false
        })
        .is_err());
    }
}
