use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde::Deserialize;
use shared::api::LaunchRequest;
use shared::{DirectoryEntry, LauncherInfo, LauncherToServer, ServerToLauncher};
use std::sync::Arc;
use tower_cookies::Cookies;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::auth::extract_user_id;
use crate::errors::AppError;
use crate::AppState;

/// GET /api/launchers - List connected launchers for the current user
pub async fn list_launchers(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<Vec<LauncherInfo>>, AppError> {
    let user_id = extract_user_id(&app_state, &cookies)?;
    let launchers = app_state.session_manager.get_launchers_for_user(&user_id);
    Ok(Json(launchers))
}

#[derive(serde::Serialize)]
pub struct LaunchResponse {
    pub request_id: Uuid,
}

/// POST /api/launch - Request launching a new session
pub async fn launch_session(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Json(req): Json<LaunchRequest>,
) -> Result<Json<LaunchResponse>, AppError> {
    let user_id = extract_user_id(&app_state, &cookies)?;

    // Find the right launcher
    let launcher_id = if let Some(id) = req.launcher_id {
        id
    } else {
        // Auto-select: pick the first connected launcher for this user
        let launchers = app_state.session_manager.get_launchers_for_user(&user_id);
        launchers.first().map(|l| l.launcher_id).ok_or_else(|| {
            error!("No connected launchers for user {}", user_id);
            AppError::NotFound("No connected launchers")
        })?
    };

    // Create a fresh short-lived proxy token for the child process
    let auth_token = mint_launch_token(&app_state, user_id)?;

    let request_id = Uuid::new_v4();
    let launch_msg = ServerToLauncher::LaunchSession {
        request_id,
        user_id,
        auth_token,
        working_directory: req.working_directory.clone(),
        session_name: None,
        claude_args: req.claude_args,
        agent_type: req.agent_type,
        scheduled_task_id: None,
    };

    if !app_state
        .session_manager
        .send_to_launcher(&launcher_id, launch_msg)
    {
        error!("Failed to send launch request to launcher {}", launcher_id);
        return Err(AppError::Internal(
            "Failed to send launch request".to_string(),
        ));
    }

    info!(
        "Launch request sent: request_id={}, launcher={}, dir={}",
        request_id, launcher_id, req.working_directory
    );

    Ok(Json(LaunchResponse { request_id }))
}

#[derive(Deserialize)]
pub struct DirectoryQuery {
    pub path: String,
}

#[derive(serde::Serialize)]
pub struct DirectoryListingResponse {
    pub entries: Vec<DirectoryEntry>,
    pub resolved_path: Option<String>,
}

/// GET /api/launchers/:launcher_id/directories?path=/some/path
pub async fn list_directories(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(launcher_id): Path<Uuid>,
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<DirectoryListingResponse>, StatusCode> {
    let user_id = extract_user_id(&app_state, &cookies).map_err(|_| StatusCode::UNAUTHORIZED)?;

    // Verify the launcher belongs to this user
    let launcher = app_state
        .session_manager
        .launchers
        .get(&launcher_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    if launcher.user_id != user_id {
        return Err(StatusCode::FORBIDDEN);
    }
    drop(launcher);

    let request_id = Uuid::new_v4();
    let rx = app_state.session_manager.register_dir_request(request_id);

    let sent = app_state.session_manager.send_to_launcher(
        &launcher_id,
        ServerToLauncher::ListDirectories {
            request_id,
            path: query.path.clone(),
        },
    );

    if !sent {
        app_state
            .session_manager
            .pending_dir_requests
            .remove(&request_id);
        error!("Failed to send ListDirectories to launcher {}", launcher_id);
        return Err(StatusCode::BAD_GATEWAY);
    }

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(LauncherToServer::ListDirectoriesResult {
            entries,
            error,
            resolved_path,
            ..
        })) => {
            if let Some(err) = error {
                warn!("Directory listing error: {}", err);
                return Err(StatusCode::BAD_REQUEST);
            }
            Ok(Json(DirectoryListingResponse {
                entries,
                resolved_path,
            }))
        }
        Ok(Ok(_)) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        Ok(Err(_)) => Err(StatusCode::INTERNAL_SERVER_ERROR),
        Err(_) => {
            app_state
                .session_manager
                .pending_dir_requests
                .remove(&request_id);
            warn!("Directory listing timed out for launcher {}", launcher_id);
            Err(StatusCode::GATEWAY_TIMEOUT)
        }
    }
}

pub(crate) fn mint_launch_token(app_state: &AppState, user_id: Uuid) -> Result<String, AppError> {
    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::users;
    use diesel::prelude::*;

    let user: crate::models::User = users::table
        .find(user_id)
        .first(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    let token_id = Uuid::new_v4();
    let token = crate::jwt::create_proxy_token(
        app_state.jwt_secret.as_bytes(),
        token_id,
        user_id,
        &user.email,
        1, // 1 day expiration for launched sessions
    )
    .map_err(|e| AppError::Internal(format!("Failed to create launch token: {}", e)))?;

    // Store token hash in DB
    let token_hash = crate::jwt::hash_token(&token);
    let new_token = crate::models::NewProxyAuthToken {
        user_id,
        name: "launcher-spawned".to_string(),
        token_hash,
        expires_at: (chrono::Utc::now() + chrono::Duration::days(1)).naive_utc(),
    };

    use crate::schema::proxy_auth_tokens;
    diesel::insert_into(proxy_auth_tokens::table)
        .values(&new_token)
        .execute(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    Ok(token)
}

/// POST /api/launchers/:launcher_id/renew-token - Manually renew a launcher's auth token
pub async fn renew_launcher_token(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(launcher_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let user_id = extract_user_id(&app_state, &cookies)?;

    // Verify the launcher belongs to this user and get its current token info
    let (old_token_hash, sender) = {
        let launcher = app_state
            .session_manager
            .launchers
            .get(&launcher_id)
            .ok_or(AppError::NotFound("Launcher not found"))?;
        if launcher.user_id != user_id {
            return Err(AppError::Forbidden);
        }
        (launcher.token_hash.clone(), launcher.sender.clone())
    };

    crate::handlers::websocket::launcher_socket::renew_launcher_token_for(
        &app_state,
        launcher_id,
        user_id,
        old_token_hash,
        sender,
    )
    .map_err(|e| AppError::Internal(format!("{:?}", e)))?;

    info!("Manually renewed token for launcher {}", launcher_id);
    Ok(StatusCode::OK)
}
