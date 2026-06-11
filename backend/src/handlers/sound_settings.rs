use axum::{extract::State, Json};
use diesel::prelude::*;
use shared::api::SoundSettingsResponse;
use std::sync::Arc;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::handlers::responses::EmptyResponse;
use crate::AppState;

pub async fn get_sound_settings(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
) -> Result<Json<SoundSettingsResponse>, AppError> {
    let mut conn = app_state.conn()?;

    use crate::schema::users;
    let sound_config: Option<serde_json::Value> = users::table
        .find(user_id)
        .select(users::sound_config)
        .first(&mut conn)?;

    Ok(Json(SoundSettingsResponse { sound_config }))
}

pub async fn save_sound_settings(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Json(config): Json<serde_json::Value>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;

    use crate::schema::users;
    diesel::update(users::table.find(user_id))
        .set(users::sound_config.eq(Some(config)))
        .execute(&mut conn)?;

    Ok(EmptyResponse::OK)
}
