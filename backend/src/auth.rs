use axum::extract::FromRequestParts;
use axum::http::request::Parts;
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

/// Extract and load the authenticated user from the session cookie.
///
/// In dev mode, returns the test user without checking cookies. In production,
/// reads the signed session cookie, parses the user ID, and loads the user row.
/// Disabled users are rejected in both modes so a ban takes effect for existing
/// browser sessions immediately.
pub fn extract_user(app_state: &AppState, cookies: &Cookies) -> Result<User, AppError> {
    let mut conn = app_state.conn()?;

    use crate::schema::users;

    if app_state.dev_mode {
        let user = dev_user(&mut conn)?;
        return reject_disabled_user(user);
    }

    let cookie = cookies
        .signed(&app_state.cookie_key)
        .get(shared::protocol::SESSION_COOKIE_NAME)
        .ok_or(AppError::Unauthorized)?;

    let user_id: Uuid = cookie.value().parse().map_err(|_| AppError::Unauthorized)?;
    let user = users::table
        .find(user_id)
        .first::<User>(&mut conn)
        .map_err(|_| AppError::Unauthorized)?;

    reject_disabled_user(user)
}

/// Extract the authenticated user's ID from the session cookie.
///
/// In dev mode, returns the test user's ID without checking cookies.
/// In production, reads the signed session cookie and parses the user ID.
/// Disabled users are rejected in both modes.
pub fn extract_user_id(app_state: &AppState, cookies: &Cookies) -> Result<Uuid, AppError> {
    extract_user(app_state, cookies).map(|user| user.id)
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
        extract_user_id(state, &cookies)
            .map(CurrentUserId)
            .map_err(|e| e.into_response())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
