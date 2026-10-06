//! Owner-authorized plugin operations execute on the selected launcher.

use crate::handlers::session_access::verify_session_owner;
use crate::{auth::CurrentUserId, errors::AppError, AppState};
use axum::{
    extract::{Path, Query, State},
    Json,
};
use diesel::prelude::*;
use serde::Deserialize;
use shared::api::{PluginAction, PluginInventoryResponse, PluginRequest, PluginResponse};
use shared::{LauncherToServer, ServerToLauncher};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInventoryQuery {
    pub session_id: Option<Uuid>,
    pub launcher_id: Option<Uuid>,
    pub working_directory: Option<String>,
}

/// Inventory is host-private: session sharing does not grant filesystem access
/// or authority to execute a plugin on the session owner's computer.
pub async fn list_plugins(
    State(state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Query(query): Query<PluginInventoryQuery>,
) -> Result<Json<PluginInventoryResponse>, AppError> {
    let (launcher_id, working_directory) = if let Some(session_id) = query.session_id {
        let session = verify_session_owner(&mut state.conn()?, session_id, user_id)?;
        (
            session
                .launcher_id
                .ok_or(AppError::BadRequest("Session has no launcher"))?,
            Some(session.working_directory),
        )
    } else {
        (
            query
                .launcher_id
                .ok_or(AppError::BadRequest("Select a computer to browse plugins"))?,
            query.working_directory,
        )
    };
    let response = dispatch(
        &state,
        user_id,
        launcher_id,
        PluginRequest {
            working_directory,
            session_id: query.session_id,
            action: PluginAction::Inventory,
        },
    )
    .await?;
    if !response.success {
        return Err(AppError::BadGatewayMessage(
            response
                .error
                .unwrap_or_else(|| "Plugin inventory failed".into()),
        ));
    }
    response
        .inventory
        .map(Json)
        .ok_or_else(|| AppError::BadGateway("Launcher returned no plugin inventory"))
}

pub async fn manage_plugins(
    State(state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(launcher_id): Path<Uuid>,
    Json(mut request): Json<PluginRequest>,
) -> Result<Json<PluginResponse>, AppError> {
    if let Some(session_id) = request.session_id {
        let session = verify_session_owner(&mut state.conn()?, session_id, user_id)?;
        if session.launcher_id != Some(launcher_id) {
            return Err(AppError::BadRequest(
                "Session belongs to a different computer",
            ));
        }
        // Never let a session-scoped command quietly target a different cwd.
        request.working_directory = Some(session.working_directory);
    }
    if matches!(
        request.action,
        PluginAction::RunCommand {
            approved: false,
            ..
        }
    ) {
        return Err(AppError::BadRequest(
            "Confirm the plugin command before running it",
        ));
    }
    authorize(&state, user_id, launcher_id)?;
    let command = if let PluginAction::RunCommand {
        name,
        command,
        args,
        expected_run,
        ..
    } = &request.action
    {
        Some(shared::api::PluginCommandRecord {
            plugin: name.clone(),
            command: command.clone(),
            run: expected_run.clone(),
            args: args.clone(),
            launcher_id,
            working_directory: request.working_directory.clone().unwrap_or_default(),
            requested_by: user_id,
            status: "approved".into(),
            exit_code: None,
            output: String::new(),
        })
    } else {
        None
    };
    if let Some(record) = &command {
        // Persist approval before dispatch: failed audit storage cannot result
        // in an unrecorded session command executing on the launcher.
        record_command(&state, request.session_id, user_id, record.clone())?;
    }
    let session_id = request.session_id;
    let response = dispatch(&state, user_id, launcher_id, request).await;
    if let Some(mut record) = command {
        match &response {
            Ok(result) => {
                record.status = if result.success {
                    "completed"
                } else {
                    "failed"
                }
                .into();
                record.exit_code = result.exit_code;
                record.output = result.output.clone();
                if let Some(error) = &result.error {
                    record.output.push_str(&format!("\n{error}"));
                }
            }
            Err(error) => {
                record.status = "outcome unknown".into();
                record.output = format!("{error:?}");
            }
        }
        record_command(&state, session_id, user_id, record)?;
    }
    response.map(Json)
}

fn authorize(state: &AppState, user_id: Uuid, launcher_id: Uuid) -> Result<(), AppError> {
    if state.session_manager.launcher_owner(launcher_id) != Some(user_id) {
        return Err(AppError::Forbidden);
    }
    if !state
        .session_manager
        .launcher_supports_capability(launcher_id, shared::LAUNCHER_CAPABILITY_PLUGINS)
    {
        return Err(AppError::BadRequest(
            "Update this computer's launcher to manage plugins",
        ));
    }
    Ok(())
}

async fn dispatch(
    state: &AppState,
    user_id: Uuid,
    launcher_id: Uuid,
    request: PluginRequest,
) -> Result<PluginResponse, AppError> {
    authorize(state, user_id, launcher_id)?;
    let timeout = match &request.action {
        PluginAction::Install { .. } | PluginAction::Update { .. } => 900,
        PluginAction::Inventory => 30,
        _ => 180,
    };
    let request_id = Uuid::new_v4();
    let rx = state
        .session_manager
        .register_plugin_request(request_id, launcher_id);
    if !state.session_manager.send_to_launcher(
        &launcher_id,
        ServerToLauncher::PluginRequest {
            request_id,
            request,
        },
    ) {
        state.session_manager.cancel_plugin_request(request_id);
        return Err(AppError::BadGateway("Computer disconnected"));
    }
    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout), rx).await;
    state.session_manager.cancel_plugin_request(request_id);
    match result {
        Ok(Ok(LauncherToServer::PluginResponse { response, .. })) => Ok(response),
        Ok(_) => Err(AppError::BadGateway(
            "Computer disconnected during plugin operation",
        )),
        Err(_) => Err(AppError::GatewayTimeout(
            "Plugin operation timed out; refresh status before retrying",
        )),
    }
}

fn record_command(
    state: &AppState,
    session_id: Option<Uuid>,
    user_id: Uuid,
    record: shared::api::PluginCommandRecord,
) -> Result<(), AppError> {
    tracing::info!(plugin = %record.plugin, command = %record.command, launcher = %record.launcher_id,
        cwd = %record.working_directory, user = %user_id, status = %record.status, "Portal plugin command");
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let mut conn = state.conn()?;
    let session = verify_session_owner(&mut conn, session_id, user_id)?;
    let portal =
        shared::PortalMessage::with_content(vec![shared::PortalContent::PluginCommand { record }]);
    let content = portal.to_json();
    let inserted = diesel::insert_into(crate::schema::messages::table)
        .values(crate::models::NewMessage {
            session_id,
            role: shared::MessageRole::Portal.to_string(),
            content: content.to_string(),
            user_id,
            agent_type: session.agent_type.clone(),
            provenance_kind: None,
            provenance_session_id: None,
            provenance_agent_type: None,
        })
        .get_result::<crate::models::Message>(&mut conn)?;
    state.session_manager.broadcast_to_web_clients(
        &session.session_key,
        shared::ServerToClient::AgentOutput {
            content,
            agent_type: session.agent_type.parse().unwrap_or_default(),
            meta: Some(inserted.portal_meta(None)),
        },
    );
    Ok(())
}
