use diesel::prelude::*;
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::errors::AppError;
use crate::models::User;
use crate::AppState;

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
    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::users;

    if app_state.dev_mode {
        let user = users::table
            .filter(users::email.eq("testing@testing.local"))
            .first::<User>(&mut conn)
            .map_err(|e| AppError::DbQuery(e.to_string()))?;
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
