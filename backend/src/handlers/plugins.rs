//! Plugin inventory and actions, all answered by the launcher that hosts the
//! plugins: a session's own launcher, or the computer picked in the UI.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::Deserialize;
use shared::api::{PluginAction, PluginInventoryResponse, PluginRequest, PluginResponse};
use uuid::Uuid;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::handlers::session_access::verify_session_reader;
use crate::AppState;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPluginsQuery {
    pub session_id: Uuid,
}

/// GET /api/plugins?sessionId=: the plugins a session was launched with.
pub async fn list_plugins(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Query(query): Query<SessionPluginsQuery>,
) -> Result<Json<PluginInventoryResponse>, AppError> {
    let session = {
        let mut conn = app_state.conn()?;
        verify_session_reader(&mut conn, query.session_id, current_user_id)?
    };
    session_inventory(&app_state, &session, None)
        .await
        .map(Json)
}

/// PATCH /api/plugins. A session choice never changes the host installation.
pub async fn set_session_plugin(
    State(state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Json(request): Json<shared::api::SetSessionPluginRequest>,
) -> Result<Json<PluginInventoryResponse>, AppError> {
    let session = {
        let mut conn = state.conn()?;
        super::session_access::verify_session_owner(&mut conn, request.session_id, user_id)?
    };
    let launcher = session
        .launcher_id
        .ok_or(AppError::BadRequest("Session has no launcher"))?;
    super::launcher_rpc::require_launcher_owner(&state, launcher, user_id)?;
    session_inventory(
        &state,
        &session,
        Some(shared::api::PluginOverride {
            name: request.name,
            enabled: request.enabled,
        }),
    )
    .await
    .map(Json)
}

async fn session_inventory(
    state: &AppState,
    session: &crate::models::Session,
    selection: Option<shared::api::PluginOverride>,
) -> Result<PluginInventoryResponse, AppError> {
    let launcher = session
        .launcher_id
        .ok_or(AppError::BadRequest("Session has no launcher"))?;
    if !state
        .session_manager
        .launcher_supports_capability(launcher, shared::LAUNCHER_CAPABILITY_SESSION_PLUGINS)
    {
        return Err(AppError::BadRequest(
            "Update the session launcher to manage plugins",
        ));
    }
    let request_id = Uuid::new_v4();
    let response = super::launcher_rpc::launcher_rpc(
        state,
        launcher,
        request_id,
        shared::ServerToLauncher::SessionPlugins {
            request_id,
            session_id: session.id,
            working_directory: session.working_directory.clone(),
            selection,
        },
        20,
    )
    .await?;
    match response {
        shared::LauncherToServer::SessionPluginsResult { result, .. } => {
            result.map_err(AppError::BadGatewayMessage)
        }
        _ => Err(AppError::Internal(
            "Unexpected launcher plugin response".into(),
        )),
    }
}

/// How long one action may run on the launcher.
fn action_timeout_secs(action: &PluginAction) -> u64 {
    match action {
        PluginAction::Inventory => 20,
        _ => 600,
    }
}

/// POST /api/launchers/{launcher_id}/plugins: run one named plugin action on
/// that computer. Only the launcher's owner may, since every action reads or
/// changes that host.
pub async fn run_plugin_action(
    State(state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(launcher_id): Path<Uuid>,
    Json(request): Json<PluginRequest>,
) -> Result<Json<PluginResponse>, AppError> {
    super::launcher_rpc::require_launcher_owner(&state, launcher_id, user_id)?;
    if !state
        .session_manager
        .launcher_supports_capability(launcher_id, shared::LAUNCHER_CAPABILITY_PLUGINS)
    {
        return Err(AppError::BadRequest(
            "Update this computer's launcher to manage plugins",
        ));
    }
    let timeout = action_timeout_secs(&request.action);
    let request_id = Uuid::new_v4();
    let response = super::launcher_rpc::launcher_rpc(
        &state,
        launcher_id,
        request_id,
        shared::ServerToLauncher::Plugin {
            request_id,
            request,
        },
        timeout,
    )
    .await?;
    match response {
        shared::LauncherToServer::PluginResult { result, .. } => {
            result.map(Json).map_err(AppError::BadGatewayMessage)
        }
        _ => Err(AppError::Internal(
            "Unexpected launcher plugin response".into(),
        )),
    }
}
