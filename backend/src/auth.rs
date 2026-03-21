use diesel::prelude::*;
use tower_cookies::Cookies;
use uuid::Uuid;

use crate::errors::AppError;
use crate::AppState;

/// Extract the authenticated user's ID from the session cookie.
///
/// In dev mode, returns the test user's ID without checking cookies.
/// In production, reads the signed session cookie and parses the user ID.
pub fn extract_user_id(app_state: &AppState, cookies: &Cookies) -> Result<Uuid, AppError> {
    if app_state.dev_mode {
        let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

        use crate::schema::users;
        return users::table
            .filter(users::email.eq("testing@testing.local"))
            .select(users::id)
            .first::<Uuid>(&mut conn)
            .map_err(|e| AppError::DbQuery(e.to_string()));
    }

    let cookie = cookies
        .signed(&app_state.cookie_key)
        .get(shared::protocol::SESSION_COOKIE_NAME)
        .ok_or(AppError::Unauthorized)?;

    cookie.value().parse().map_err(|_| AppError::Unauthorized)
}
