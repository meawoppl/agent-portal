use axum::response::Redirect;
use oauth2::{CsrfToken, Scope};
use tower_cookies::{cookie::SameSite, Cookie, Cookies};
use tracing::error;

use super::identity::{PROVIDER_GITHUB, PROVIDER_GOOGLE};
use crate::{errors::AppError, routes, AppState, OAuthClient};

const OAUTH_CSRF_COOKIE: &str = "oauth_csrf";
const OAUTH_DEVICE_CSRF_COOKIE: &str = "oauth_device_csrf";

/// The per-provider knobs of an otherwise identical authorization-code flow.
pub(super) struct Provider {
    /// Value stored in `user_identities.provider`.
    pub(super) key: &'static str,
    /// Where this provider sends the authorization code back to. Doubles as the
    /// CSRF cookies' `Path`: two providers use the same cookie *names*, and it
    /// is the differing path that keeps concurrent logins from clobbering each
    /// other — a cookie is identified by name **and** path, and each callback
    /// only receives the one scoped to it.
    pub(super) callback_path: &'static str,
    pub(super) scopes: &'static [&'static str],
}

pub(super) const GOOGLE: Provider = Provider {
    key: PROVIDER_GOOGLE,
    callback_path: routes::AUTH_GOOGLE_CALLBACK,
    scopes: &["openid", "email", "profile"],
};

/// `user:email` is required on top of `read:user`: GitHub's `/user` endpoint
/// omits the address entirely when the user has it set to private, and carries
/// no verification flag either way — see `github_user_info`.
pub(super) const GITHUB: Provider = Provider {
    key: PROVIDER_GITHUB,
    callback_path: routes::AUTH_GITHUB_CALLBACK,
    scopes: &["read:user", "user:email"],
};

/// Look up a provider by its `user_identities.provider` key.
pub(super) fn by_key(key: &str) -> Option<&'static Provider> {
    match key {
        PROVIDER_GOOGLE => Some(&GOOGLE),
        PROVIDER_GITHUB => Some(&GITHUB),
        _ => None,
    }
}

pub(super) fn regular_authorization_redirect(
    provider: &Provider,
    client: &OAuthClient,
    cookies: &Cookies,
    app_state: &AppState,
) -> Redirect {
    let auth_request = add_scopes(provider, client.authorize_url(CsrfToken::new_random));
    let (auth_url, csrf_token) = auth_request.url();

    // Store the CSRF token in a short-lived signed cookie so we can verify it on callback.
    cookies.signed(&app_state.cookie_key).add(oauth_csrf_cookie(
        OAUTH_CSRF_COOKIE,
        csrf_token.secret(),
        provider.callback_path,
        !app_state.dev_mode,
    ));

    Redirect::temporary(auth_url.as_str())
}

pub(super) fn device_authorization_redirect(
    provider: &Provider,
    client: &OAuthClient,
    cookies: &Cookies,
    app_state: &AppState,
    device_user_code: &str,
) -> Redirect {
    let device_csrf = CsrfToken::new_random();
    let state_value = build_device_oauth_state(device_user_code, device_csrf.secret());

    cookies.signed(&app_state.cookie_key).add(oauth_csrf_cookie(
        OAUTH_DEVICE_CSRF_COOKIE,
        device_csrf.secret(),
        provider.callback_path,
        !app_state.dev_mode,
    ));

    let auth_request = add_scopes(
        provider,
        client.authorize_url(|| CsrfToken::new(state_value)),
    );
    let (auth_url, _csrf_token) = auth_request.url();

    Redirect::temporary(auth_url.as_str())
}

pub(super) fn validate_callback_state(
    provider: &Provider,
    cookies: &Cookies,
    app_state: &AppState,
    state: Option<&str>,
) -> Result<Option<DeviceOAuthState>, AppError> {
    let device_state = state.and_then(parse_device_oauth_state);
    if state.is_some_and(|state| state.starts_with("device:")) && device_state.is_none() {
        error!("Device OAuth callback: malformed state");
        return Err(AppError::Forbidden);
    }

    if let Some(ref device_state) = device_state {
        let csrf_cookie = cookies
            .signed(&app_state.cookie_key)
            .get(OAUTH_DEVICE_CSRF_COOKIE)
            .ok_or_else(|| {
                error!("Device OAuth callback: missing CSRF cookie");
                AppError::Forbidden
            })?;

        if csrf_cookie.value() != device_state.csrf_nonce {
            error!("Device OAuth callback: CSRF token mismatch");
            return Err(AppError::Forbidden);
        }

        cookies
            .signed(&app_state.cookie_key)
            .add(remove_oauth_csrf_cookie(
                OAUTH_DEVICE_CSRF_COOKIE,
                provider.callback_path,
            ));
    } else {
        let csrf_cookie = cookies
            .signed(&app_state.cookie_key)
            .get(OAUTH_CSRF_COOKIE)
            .ok_or_else(|| {
                error!("OAuth callback: missing CSRF cookie");
                AppError::Forbidden
            })?;

        let state_value = state.unwrap_or("");
        if csrf_cookie.value() != state_value {
            error!("OAuth callback: CSRF token mismatch");
            return Err(AppError::Forbidden);
        }

        cookies
            .signed(&app_state.cookie_key)
            .add(remove_oauth_csrf_cookie(
                OAUTH_CSRF_COOKIE,
                provider.callback_path,
            ));
    }

    Ok(device_state)
}

fn add_scopes<'a>(
    provider: &Provider,
    auth_request: oauth2::AuthorizationRequest<'a>,
) -> oauth2::AuthorizationRequest<'a> {
    provider.scopes.iter().fold(auth_request, |req, scope| {
        req.add_scope(Scope::new((*scope).to_string()))
    })
}

fn oauth_csrf_cookie(
    name: &'static str,
    value: &str,
    path: &'static str,
    secure: bool,
) -> Cookie<'static> {
    let mut cookie = Cookie::new(name, value.to_owned());
    cookie.set_path(path);
    cookie.set_http_only(true);
    cookie.set_secure(secure);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::minutes(10));
    cookie
}

fn remove_oauth_csrf_cookie(name: &'static str, path: &'static str) -> Cookie<'static> {
    let mut cookie = Cookie::new(name, "");
    cookie.set_path(path);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::ZERO);
    cookie
}

fn build_device_oauth_state(user_code: &str, csrf_nonce: &str) -> String {
    format!("device:{user_code}:{csrf_nonce}")
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct DeviceOAuthState {
    pub(super) user_code: String,
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
}
