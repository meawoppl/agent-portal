//! Explicit provider verification, separate from login and account linking.
use super::*;
use serde::{Deserialize, Serialize};
use shared::api::{SecretVerificationRedirect, SecretVerificationStart, SecretVerificationStatus};

const CHALLENGE: &str = "portal_secret_verification_challenge";
#[derive(Serialize, Deserialize)]
pub(super) struct Challenge {
    user: uuid::Uuid,
    provider: String,
    nonce: String,
    browser: String,
    destination: String,
    created_at: i64,
}
fn destination(value: &str) -> Result<String, AppError> {
    // Only local application pages, never a scheme-relative or external URL.
    let parsed = reqwest::Url::parse("https://portal.invalid")
        .and_then(|base| base.join(value))
        .map_err(|_| AppError::Forbidden)?;
    if !value.starts_with('/')
        || value.starts_with("//")
        || value.contains('\\')
        || parsed.origin().ascii_serialization() != "https://portal.invalid"
        || !matches!(
            parsed.path(),
            "/dashboard" | "/settings" | "/secrets/authorize"
        )
        || value.len() > 2048
    {
        return Err(AppError::Forbidden);
    }
    Ok(value.into())
}
fn challenge_cookie(value: String, state: &AppState) -> Cookie<'static> {
    let mut cookie = Cookie::new(CHALLENGE, value);
    cookie.set_path("/api/auth");
    cookie.set_http_only(true);
    cookie.set_secure(!state.dev_mode);
    cookie.set_same_site(SameSite::Lax);
    cookie.set_max_age(tower_cookies::cookie::time::Duration::minutes(10));
    cookie
}
pub async fn status(
    State(state): State<Arc<AppState>>,
    crate::auth::BrowserUserId(user): crate::auth::BrowserUserId,
    cookies: Cookies,
) -> Result<Json<SecretVerificationStatus>, AppError> {
    use crate::schema::user_identities;
    let providers = user_identities::table
        .filter(user_identities::user_id.eq(user))
        .select(user_identities::provider)
        .load::<String>(&mut *state.conn()?)?
        .into_iter()
        .filter(|p| state.oauth.client(p).is_some())
        .collect();
    Ok(Json(SecretVerificationStatus {
        verified: crate::secrets::auth::require_verification(&cookies, &state, user).is_ok(),
        providers,
    }))
}
pub async fn start(
    State(state): State<Arc<AppState>>,
    crate::auth::BrowserUserId(user): crate::auth::BrowserUserId,
    cookies: Cookies,
    headers: HeaderMap,
    Json(request): Json<SecretVerificationStart>,
) -> Result<Json<SecretVerificationRedirect>, AppError> {
    crate::secrets::auth::csrf(&state, &headers)?;
    let destination = destination(&request.return_to)?;
    let provider = oauth::by_key(&request.provider).ok_or(AppError::Forbidden)?;
    let client = state
        .oauth
        .client(provider.key)
        .ok_or(AppError::Forbidden)?;
    use crate::schema::user_identities;
    if user_identities::table
        .filter(user_identities::user_id.eq(user))
        .filter(user_identities::provider.eq(provider.key))
        .count()
        .get_result::<i64>(&mut *state.conn()?)?
        == 0
    {
        return Err(AppError::Forbidden);
    }
    let nonce = format!("secret-verify:{}", oauth2::CsrfToken::new_random().secret());
    let challenge = Challenge {
        user,
        provider: provider.key.into(),
        nonce: nonce.clone(),
        browser: crate::secrets::auth::begin_verification(&cookies, &state),
        destination,
        created_at: chrono::Utc::now().timestamp(),
    };
    cookies.signed(&state.cookie_key).add(challenge_cookie(
        serde_json::to_string(&challenge)
            .map_err(|_| AppError::Internal("Could not start verification".into()))?,
        &state,
    ));
    let (url, _) = oauth::add_scopes(
        provider,
        client.authorize_url(|| oauth2::CsrfToken::new(nonce)),
    )
    .url();
    Ok(Json(SecretVerificationRedirect {
        url: url.to_string(),
    }))
}
pub(super) fn take(
    provider: &oauth::Provider,
    state: &AppState,
    cookies: &Cookies,
    nonce: Option<&str>,
) -> Result<Option<Challenge>, AppError> {
    let Some(nonce) = nonce.filter(|v| v.starts_with("secret-verify:")) else {
        return Ok(None);
    };
    let cookie = cookies
        .signed(&state.cookie_key)
        .get(CHALLENGE)
        .ok_or(AppError::Forbidden)?;
    let challenge: Challenge =
        serde_json::from_str(cookie.value()).map_err(|_| AppError::Forbidden)?;
    let age = chrono::Utc::now().timestamp() - challenge.created_at;
    let current = cookies
        .signed(&state.cookie_key)
        .get(SESSION_COOKIE_NAME)
        .ok_or(AppError::Forbidden)?;
    if crate::secrets::auth::browser_context(cookies, state).as_deref()
        != Some(challenge.browser.as_str())
        || challenge.nonce != nonce
        || challenge.provider != provider.key
        || current.value() != challenge.user.to_string()
        || !(0..=600).contains(&age)
    {
        return Err(AppError::Forbidden);
    }
    let mut removed = challenge_cookie(String::new(), state);
    removed.make_removal();
    cookies.signed(&state.cookie_key).add(removed);
    Ok(Some(challenge))
}
pub(super) fn cancel(challenge: Challenge) -> Result<axum::response::Response, AppError> {
    let path = destination(&challenge.destination)?;
    let mut url = reqwest::Url::parse("https://portal.invalid")
        .and_then(|base| base.join(&path))
        .map_err(|_| AppError::Forbidden)?;
    url.query_pairs_mut()
        .append_pair("secret_verification", "failed");
    let local = format!(
        "{}{}{}",
        url.path(),
        url.query().map(|q| format!("?{q}")).unwrap_or_default(),
        url.fragment().map(|f| format!("#{f}")).unwrap_or_default()
    );
    Ok(Redirect::to(&local).into_response())
}
pub(super) fn finish(
    challenge: Challenge,
    identity: &ProviderIdentity,
    state: &AppState,
    cookies: &Cookies,
) -> Result<axum::response::Response, AppError> {
    use crate::schema::user_identities;
    let mut conn = state.conn()?;
    crate::secrets::store::active_user(&mut conn, challenge.user)?;
    // Never resolve/link/create accounts in a verification flow.
    let user = user_identities::table
        .filter(user_identities::provider.eq(identity.provider))
        .filter(user_identities::subject.eq(&identity.subject))
        .select(user_identities::user_id)
        .first::<uuid::Uuid>(&mut conn)
        .optional()?;
    if user != Some(challenge.user) {
        return cancel(challenge);
    }
    crate::secrets::auth::record_verification(cookies, state, challenge.user);
    Ok(Redirect::to(&destination(&challenge.destination)?).into_response())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn challenge(user: uuid::Uuid) -> Challenge {
        Challenge {
            user,
            provider: "google".into(),
            nonce: "secret-verify:test".into(),
            browser: String::new(),
            destination: "/secrets/authorize?code=ABC".into(),
            created_at: chrono::Utc::now().timestamp(),
        }
    }
    fn install(state: &AppState, cookies: &Cookies, value: &Challenge) {
        cookies.signed(&state.cookie_key).add(challenge_cookie(
            serde_json::to_string(value).unwrap(),
            state,
        ));
    }
    #[test]
    fn verification_is_bound_to_browser_account_provider_and_challenge() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let state = crate::test_support::test_app_state(pool);
        let user = uuid::Uuid::new_v4();
        let cookies = Cookies::default();
        let mut value = challenge(user);
        value.browser = crate::secrets::auth::begin_verification(&cookies, &state);
        install(&state, &cookies, &value);
        assert!(take(&oauth::GOOGLE, &state, &cookies, Some(&value.nonce)).is_err());
        cookies
            .signed(&state.cookie_key)
            .add(Cookie::new(SESSION_COOKIE_NAME, user.to_string()));
        assert!(take(&oauth::GITHUB, &state, &cookies, Some(&value.nonce)).is_err());
        assert!(take(
            &oauth::GOOGLE,
            &state,
            &cookies,
            Some("secret-verify:wrong")
        )
        .is_err());
        value.created_at -= 601;
        install(&state, &cookies, &value);
        assert!(take(&oauth::GOOGLE, &state, &cookies, Some(&value.nonce)).is_err());
        value.created_at = chrono::Utc::now().timestamp();
        install(&state, &cookies, &value);
        let accepted = take(&oauth::GOOGLE, &state, &cookies, Some(&value.nonce))
            .unwrap()
            .unwrap();
        assert_eq!(accepted.user, user);
        assert!(take(&oauth::GOOGLE, &state, &cookies, Some(&value.nonce)).is_err());
        assert!(take(&oauth::GOOGLE, &state, &cookies, Some("normal-login"))
            .unwrap()
            .is_none());
    }
    #[test]
    fn existing_identity_required_and_login_clears_verification() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let state = crate::test_support::test_app_state(pool);
        let mut conn = state.conn().unwrap();
        let user = crate::test_support::insert_user(&mut conn, "verify-owner");
        let other = crate::test_support::insert_user(&mut conn, "verify-other");
        let subject = uuid::Uuid::new_v4().to_string();
        diesel::insert_into(crate::schema::user_identities::table)
            .values(crate::models::NewUserIdentity {
                user_id: user.id,
                provider: "google".into(),
                subject: subject.clone(),
                email: Some(user.email.clone()),
                handle: None,
                email_verified: true,
                last_seen_at: None,
            })
            .execute(&mut conn)
            .unwrap();
        drop(conn);
        let cookies = Cookies::default();
        let mut identity = ProviderIdentity {
            provider: "google",
            subject,
            handle: None,
            email: Some(user.email.clone()),
            email_verified: true,
            name: None,
            avatar_url: None,
        };
        let rejected = finish(challenge(other.id), &identity, &state, &cookies).unwrap();
        assert!(rejected
            .headers()
            .get("location")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("secret_verification=failed"));
        assert!(crate::secrets::auth::require_verification(&cookies, &state, other.id).is_err());
        let accepted = finish(challenge(user.id), &identity, &state, &cookies).unwrap();
        assert_eq!(
            accepted.headers().get("location").unwrap(),
            "/secrets/authorize?code=ABC"
        );
        assert!(crate::secrets::auth::require_verification(&cookies, &state, user.id).is_ok());
        assert!(crate::secrets::auth::require_verification(&cookies, &state, other.id).is_err());
        super::super::add_session_cookie(&cookies, &state, &user);
        assert!(crate::secrets::auth::require_verification(&cookies, &state, user.id).is_err());
        // Even a verified email matching the owner cannot link a new subject.
        identity.subject = uuid::Uuid::new_v4().to_string();
        finish(challenge(user.id), &identity, &state, &cookies).unwrap();
        assert!(crate::secrets::auth::require_verification(&cookies, &state, user.id).is_err());
        assert_eq!(
            crate::schema::user_identities::table
                .filter(crate::schema::user_identities::user_id.eq(user.id))
                .count()
                .get_result::<i64>(&mut *state.conn().unwrap())
                .unwrap(),
            1
        );
    }
    #[test]
    fn return_path_keeps_pending_context_without_open_redirects() {
        for p in [
            "/dashboard?session=abc",
            "/settings?tab=secrets&secret_job=123",
            "/secrets/authorize?code=ABC",
        ] {
            assert_eq!(destination(p).unwrap(), p);
        }
        for p in [
            "//evil.test/dashboard",
            "https://evil.test/dashboard",
            "/\\evil.test/dashboard",
            "/api/auth/logout",
            "/settings/../api/auth/logout",
        ] {
            assert!(destination(p).is_err(), "{p}");
        }
    }
}
