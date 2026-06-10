use axum::{extract::State, Json};
use diesel::prelude::*;
use shared::api::SoundSettingsResponse;
use std::sync::Arc;
use tower_cookies::Cookies;

use crate::auth::extract_user_id;
use crate::errors::AppError;
use crate::handlers::responses::EmptyResponse;
use crate::AppState;

pub async fn get_sound_settings(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<SoundSettingsResponse>, AppError> {
    let user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get()?;

    use crate::schema::users;
    let sound_config: Option<serde_json::Value> = users::table
        .find(user_id)
        .select(users::sound_config)
        .first(&mut conn)?;

    Ok(Json(SoundSettingsResponse { sound_config }))
}

pub async fn save_sound_settings(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Json(config): Json<serde_json::Value>,
) -> Result<EmptyResponse, AppError> {
    let user_id = extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get()?;

    use crate::schema::users;
    diesel::update(users::table.find(user_id))
        .set(users::sound_config.eq(Some(config)))
        .execute(&mut conn)?;

    Ok(EmptyResponse::OK)
}
