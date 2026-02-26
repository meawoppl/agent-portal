//! App configuration endpoint
//!
//! Returns public application configuration to the frontend.

use crate::AppState;
use axum::{extract::State, Json};
use shared::AppConfig;
use std::sync::Arc;

/// GET /api/config - Returns application configuration
pub async fn get_config(State(app_state): State<Arc<AppState>>) -> Json<AppConfig> {
    Json(AppConfig {
        app_title: app_state.app_title.clone(),
        server_version: env!("CARGO_PKG_VERSION").to_string(),
    })
}
