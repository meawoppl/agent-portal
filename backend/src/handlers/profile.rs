//! User profile settings (#1485).
//!
//! Currently just the display nickname. Kept separate from the OAuth `auth`
//! handlers because this is a self-service profile edit, not part of any login
//! flow.

use axum::{extract::State, Json};
use diesel::prelude::*;
use shared::api::UpdateProfileRequest;
use std::sync::Arc;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::handlers::responses::EmptyResponse;
use crate::AppState;

/// Longest nickname we store. Matches the `VARCHAR(64)` column; enforced here
/// too so an over-long value is a clean 400 rather than a database error.
const MAX_NICKNAME_LEN: usize = 64;

/// `PUT /api/settings/profile` — set or clear the caller's display nickname.
pub async fn update_profile(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Json(body): Json<UpdateProfileRequest>,
) -> Result<EmptyResponse, AppError> {
    // Trim and treat blank as "clear" — a whitespace-only nickname would render
    // as an invisible label, which is worse than falling back to the name.
    let nickname = body
        .nickname
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    if let Some(ref n) = nickname {
        if n.chars().count() > MAX_NICKNAME_LEN {
            return Err(AppError::BadRequest("Nickname is too long"));
        }
    }

    let mut conn = app_state.conn()?;
    use crate::schema::users;
    diesel::update(users::table.find(user_id))
        .set(users::nickname.eq(nickname))
        .execute(&mut conn)?;

    Ok(EmptyResponse::OK)
}
