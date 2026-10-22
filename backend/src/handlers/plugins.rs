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
        PluginAction::Surface {
            action: shared::api::PluginSurfaceAction::Status,
            ..
        } => 20,
        // A start waits up to 30 s for the first health check.
        PluginAction::Surface { .. } => 90,
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
    Json(mut request): Json<PluginRequest>,
) -> Result<Json<PluginResponse>, AppError> {
    super::launcher_rpc::require_launcher_owner(&state, launcher_id, user_id)?;
    // An action for a session (a surface forwarded to it) must name one the
    // caller owns on this computer, and runs from that session's directory,
    // whatever the request said.
    let session = match request.session_id {
        Some(session_id) => {
            let session = {
                let mut conn = state.conn()?;
                super::session_access::verify_session_owner(&mut conn, session_id, user_id)?
            };
            if session.launcher_id != Some(launcher_id) {
                return Err(AppError::BadRequest(
                    "That session runs on another computer",
                ));
            }
            request.working_directory = Some(session.working_directory.clone());
            Some(session)
        }
        None => None,
    };
    if !state
        .session_manager
        .launcher_supports_capability(launcher_id, shared::LAUNCHER_CAPABILITY_PLUGINS)
    {
        return Err(AppError::BadRequest(
            "Update this computer's launcher to manage plugins",
        ));
    }
    let timeout = action_timeout_secs(&request.action);
    let ran_command = match &request.action {
        PluginAction::RunCommand {
            name,
            command,
            args,
            approved: true,
        } => Some((name.clone(), command.clone(), args.clone())),
        _ => None,
    };
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
    let response = match response {
        shared::LauncherToServer::PluginResult { result, .. } => {
            result.map_err(AppError::BadGatewayMessage)?
        }
        _ => {
            return Err(AppError::Internal(
                "Unexpected launcher plugin response".into(),
            ))
        }
    };
    if let (Some(session), Some((name, command, args))) = (&session, ran_command) {
        record_command(&state, session, &name, &command, &args, &response)?;
    }
    Ok(Json(response))
}

/// Output kept in a plugin command's transcript card; the tail carries the
/// result.
const COMMAND_CARD_OUTPUT: usize = 8 * 1024;

/// Put a plugin command's run in the session's transcript as a portal card:
/// which plugin command, on which computer and directory, how it ended, and
/// its output. A portal card, not an agent tool call, so it reads as a named
/// plugin command rather than arbitrary shell.
fn record_command(
    state: &AppState,
    session: &crate::models::Session,
    plugin: &str,
    command: &str,
    args: &[String],
    response: &PluginResponse,
) -> Result<(), AppError> {
    use diesel::prelude::*;
    // Quote an argument that holds spaces (or nothing), so the card shows the
    // arguments as they were given.
    let shown = |arg: &String| {
        if arg.is_empty() || arg.chars().any(char::is_whitespace) {
            format!("'{arg}'")
        } else {
            arg.clone()
        }
    };
    let invocation = std::iter::once(format!("{plugin}:{command}"))
        .chain(args.iter().map(shown))
        .collect::<Vec<_>>()
        .join(" ");
    let ended = match (response.success, response.exit_code, &response.error) {
        (true, _, _) => "succeeded".to_string(),
        (false, Some(code), _) => format!("failed (exit {code})"),
        (false, None, Some(error)) => format!("failed: {error}"),
        (false, None, None) => "failed".to_string(),
    };
    let output = response.output.trim_end();
    let tail = match output.len().checked_sub(COMMAND_CARD_OUTPUT) {
        Some(start) => {
            let start = (start..output.len())
                .find(|i| output.is_char_boundary(*i))
                .unwrap_or(output.len());
            format!("…{}", &output[start..])
        }
        None => output.to_string(),
    };
    let mut text = format!(
        "**Plugin command** `{invocation}` {ended}\n\nOn {} in `{}`",
        session.hostname, session.working_directory
    );
    if !tail.is_empty() {
        text.push_str(&format!("\n\n```\n{tail}\n```"));
    }
    let content = shared::PortalMessage::text(text).to_json();
    let inserted: crate::models::Message = diesel::insert_into(crate::schema::messages::table)
        .values(crate::models::NewMessage {
            session_id: session.id,
            role: shared::MessageRole::Portal.to_string(),
            content: content.to_string(),
            user_id: session.user_id,
            agent_type: session.agent_type.clone(),
            provenance_kind: None,
            provenance_session_id: None,
            provenance_agent_type: None,
        })
        .get_result(&mut state.conn()?)?;
    state.session_manager.broadcast_to_web_clients(
        &session.session_key,
        shared::ServerToClient::AgentOutput {
            content,
            agent_type: shared::AgentType::parse_or_default(&session.agent_type),
            meta: Some(inserted.portal_meta(None)),
        },
    );
    Ok(())
}
