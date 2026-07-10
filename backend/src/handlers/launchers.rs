use axum::{
    extract::{Path, Query, State},
    Json,
};
use diesel::prelude::*;
use serde::Deserialize;
use shared::api::{DirectoryListingResponse, LaunchRequest, ProbeAgentsResponse};
use shared::{LauncherInfo, LauncherToServer, ServerToLauncher, SessionStatus};
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::handlers::responses::EmptyResponse;
use crate::handlers::websocket::SessionManager;
use crate::models::{NewSessionMember, NewSessionWithId};
use crate::AppState;

/// GET /api/launchers - List connected launchers for the current user
pub async fn list_launchers(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
) -> Result<Json<Vec<LauncherInfo>>, AppError> {
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
    CurrentUserId(user_id): CurrentUserId,
    Json(req): Json<LaunchRequest>,
) -> Result<Json<LaunchResponse>, AppError> {
    let launcher_id = resolve_launch_target(&app_state.session_manager, req.launcher_id, user_id)?;
    let (hostname, version) = {
        let launcher = app_state
            .session_manager
            .launchers
            .get(&launcher_id)
            .ok_or(AppError::NotFound("Launcher not found"))?;
        (launcher.hostname.clone(), launcher.version.clone())
    };

    // Create a fresh short-lived proxy token for the child process
    let auth_token = mint_launch_token(&app_state, user_id)?;

    let request_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    create_desired_session(
        &app_state,
        DesiredSessionDraft {
            session_id,
            user_id,
            working_directory: req.working_directory.clone(),
            hostname,
            launcher_id: Some(launcher_id),
            client_version: Some(version),
            agent_type: req.agent_type,
            claude_args: req.claude_args.clone(),
        },
    )?;
    app_state
        .session_manager
        .register_launch_session(request_id, session_id);

    let launch_msg = ServerToLauncher::LaunchSession {
        request_id,
        user_id,
        auth_token,
        working_directory: req.working_directory.clone(),
        session_name: Some(default_session_name(&req.working_directory)),
        claude_args: req.claude_args,
        agent_type: req.agent_type,
        scheduled_task_id: None,
        resume_session_id: Some(session_id),
    };

    if !app_state
        .session_manager
        .send_to_launcher(&launcher_id, launch_msg)
    {
        app_state.session_manager.cancel_launch_session(request_id);
        let mut conn = app_state.conn()?;
        use crate::schema::sessions;
        let _ = diesel::delete(sessions::table.find(session_id)).execute(&mut conn);
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

fn default_session_name(working_directory: &str) -> String {
    std::path::Path::new(working_directory)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(working_directory)
        .to_string()
}

pub(crate) struct DesiredSessionDraft {
    session_id: Uuid,
    user_id: Uuid,
    working_directory: String,
    hostname: String,
    launcher_id: Option<Uuid>,
    client_version: Option<String>,
    agent_type: shared::AgentType,
    claude_args: Vec<String>,
}

pub(crate) fn create_desired_session(
    app_state: &AppState,
    draft: DesiredSessionDraft,
) -> Result<(), AppError> {
    let mut conn = app_state.conn()?;

    use crate::schema::{session_members, sessions};
    use diesel::prelude::*;

    let session_name = default_session_name(&draft.working_directory);
    let new_session = NewSessionWithId {
        id: draft.session_id,
        user_id: draft.user_id,
        session_name,
        session_key: draft.session_id.to_string(),
        working_directory: draft.working_directory,
        status: SessionStatus::Disconnected.as_str().to_string(),
        git_branch: None,
        client_version: draft.client_version,
        hostname: draft.hostname,
        launcher_id: draft.launcher_id,
        agent_type: draft.agent_type.as_str().to_string(),
        repo_url: None,
        scheduled_task_id: None,
        paused: false,
        claude_args: serde_json::to_value(&draft.claude_args)
            .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
    };

    diesel::insert_into(sessions::table)
        .values(&new_session)
        .execute(&mut conn)?;

    diesel::insert_into(session_members::table)
        .values(NewSessionMember {
            session_id: draft.session_id,
            user_id: draft.user_id,
            role: "owner".to_string(),
        })
        .execute(&mut conn)?;

    Ok(())
}

fn resolve_launch_target(
    session_manager: &SessionManager,
    requested_launcher_id: Option<Uuid>,
    user_id: Uuid,
) -> Result<Uuid, AppError> {
    if let Some(launcher_id) = requested_launcher_id {
        let launcher = session_manager
            .launchers
            .get(&launcher_id)
            .ok_or(AppError::NotFound("Launcher not found"))?;
        if launcher.user_id != user_id {
            warn!(
                "User {} attempted to launch on launcher {} owned by {}",
                user_id, launcher_id, launcher.user_id
            );
            return Err(AppError::Forbidden);
        }
        return Ok(launcher_id);
    }

    let launchers = session_manager.get_launchers_for_user(&user_id);
    launchers.first().map(|l| l.launcher_id).ok_or_else(|| {
        error!("No connected launchers for user {}", user_id);
        AppError::NotFound("No connected launchers")
    })
}

#[derive(Deserialize)]
pub struct DirectoryQuery {
    pub path: String,
}

/// GET /api/launchers/:launcher_id/directories?path=/some/path
pub async fn list_directories(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(launcher_id): Path<Uuid>,
    Query(query): Query<DirectoryQuery>,
) -> Result<Json<DirectoryListingResponse>, AppError> {
    // Verify the launcher belongs to this user
    let launcher = app_state
        .session_manager
        .launchers
        .get(&launcher_id)
        .ok_or(AppError::NotFound("Launcher not found"))?;
    if launcher.user_id != user_id {
        return Err(AppError::Forbidden);
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
        app_state.session_manager.cancel_dir_request(request_id);
        error!("Failed to send ListDirectories to launcher {}", launcher_id);
        return Err(AppError::BadGateway(
            "Failed to send directory listing request",
        ));
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
                return Err(AppError::BadRequest("Directory listing failed"));
            }
            Ok(Json(DirectoryListingResponse {
                entries,
                resolved_path,
            }))
        }
        Ok(Ok(_)) => Err(AppError::Internal(
            "Unexpected launcher directory response".to_string(),
        )),
        Ok(Err(_)) => Err(AppError::Internal(
            "Directory listing response channel closed".to_string(),
        )),
        Err(_) => {
            app_state.session_manager.cancel_dir_request(request_id);
            warn!("Directory listing timed out for launcher {}", launcher_id);
            Err(AppError::GatewayTimeout("Directory listing timed out"))
        }
    }
}

pub(crate) fn mint_launch_token(app_state: &AppState, user_id: Uuid) -> Result<String, AppError> {
    use crate::handlers::proxy_tokens::{issue_proxy_token, TokenPersist, LAUNCH_TOKEN_NAME};

    let mut conn = app_state.conn()?;

    // Launch tokens never expire. The token is bound to its session at proxy
    // registration and revoked when the session terminates, so its lifetime
    // tracks the session rather than a fixed TTL. See #932.
    let issued = issue_proxy_token(
        &mut conn,
        app_state.jwt_secret.as_bytes(),
        user_id,
        TokenPersist::Create {
            name: LAUNCH_TOKEN_NAME,
        },
        None,
    )?;

    Ok(issued.token)
}

/// POST /api/launchers/:launcher_id/update - Tell the launcher to fetch the
/// latest release, install it, and restart itself.
pub async fn update_launcher(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(launcher_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let sender = {
        let launcher = app_state
            .session_manager
            .launchers
            .get(&launcher_id)
            .ok_or(AppError::NotFound("Launcher not found"))?;
        if launcher.user_id != user_id {
            return Err(AppError::Forbidden);
        }
        launcher.sender.clone()
    };

    if sender.send(ServerToLauncher::UpdateAndRestart).is_err() {
        warn!("Launcher {} disconnected while sending update", launcher_id);
        return Err(AppError::Internal("Launcher disconnected".to_string()));
    }

    info!("Sent UpdateAndRestart to launcher {}", launcher_id);
    Ok(EmptyResponse::OK)
}

/// GET /api/launchers/:launcher_id/probe-agents - Ask the launcher to (re-)scan
/// its agent CLIs (`claude`, `codex`) and return install state. The frontend
/// calls this when the launch dialog opens.
pub async fn probe_agents(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(launcher_id): Path<Uuid>,
) -> Result<Json<ProbeAgentsResponse>, AppError> {
    let sender = {
        let launcher = app_state
            .session_manager
            .launchers
            .get(&launcher_id)
            .ok_or(AppError::NotFound("Launcher not found"))?;
        if launcher.user_id != user_id {
            return Err(AppError::Forbidden);
        }
        launcher.sender.clone()
    };

    let request_id = Uuid::new_v4();
    let rx = app_state.session_manager.register_probe_request(request_id);

    if sender
        .send(ServerToLauncher::ProbeAgents { request_id })
        .is_err()
    {
        app_state.session_manager.cancel_probe_request(request_id);
        warn!("Launcher {} disconnected while probing agents", launcher_id);
        return Err(AppError::BadGateway("Failed to send agent probe request"));
    }

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(LauncherToServer::ProbeAgentsResult { agents, .. })) => {
            Ok(Json(ProbeAgentsResponse { agents }))
        }
        Ok(Ok(_)) => Err(AppError::Internal(
            "Unexpected launcher probe response".to_string(),
        )),
        Ok(Err(_)) => Err(AppError::Internal(
            "Agent probe response channel closed".to_string(),
        )),
        Err(_) => {
            app_state.session_manager.cancel_probe_request(request_id);
            warn!("Probe agents timed out for launcher {}", launcher_id);
            Err(AppError::GatewayTimeout("Agent probe timed out"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::websocket::LauncherConnection;
    use tokio::sync::mpsc;

    fn launcher_for(user_id: Uuid, hostname: &str) -> LauncherConnection {
        let (sender, _rx) = mpsc::unbounded_channel();
        LauncherConnection {
            sender,
            launcher_name: format!("launcher-{}", hostname),
            hostname: hostname.to_string(),
            user_id,
            running_sessions: Vec::new(),
            working_directory: None,
            version: "test".to_string(),
            cancel: tokio_util::sync::CancellationToken::new(),
            gen: 0,
        }
    }

    #[test]
    fn explicit_launcher_must_belong_to_user() {
        let manager = SessionManager::new();
        let owner = Uuid::new_v4();
        let other_user = Uuid::new_v4();
        let launcher_id = Uuid::new_v4();
        manager
            .try_register_launcher(launcher_id, launcher_for(owner, "host-a"))
            .unwrap();

        assert!(matches!(
            resolve_launch_target(&manager, Some(launcher_id), other_user),
            Err(AppError::Forbidden)
        ));
    }

    #[test]
    fn explicit_launcher_owner_is_allowed() {
        let manager = SessionManager::new();
        let owner = Uuid::new_v4();
        let launcher_id = Uuid::new_v4();
        manager
            .try_register_launcher(launcher_id, launcher_for(owner, "host-a"))
            .unwrap();

        assert_eq!(
            resolve_launch_target(&manager, Some(launcher_id), owner).unwrap(),
            launcher_id
        );
    }

    #[test]
    fn missing_explicit_launcher_is_not_found() {
        let manager = SessionManager::new();

        assert!(matches!(
            resolve_launch_target(&manager, Some(Uuid::new_v4()), Uuid::new_v4()),
            Err(AppError::NotFound("Launcher not found"))
        ));
    }

    #[test]
    fn default_launcher_requires_connected_launcher() {
        let manager = SessionManager::new();

        assert!(matches!(
            resolve_launch_target(&manager, None, Uuid::new_v4()),
            Err(AppError::NotFound("No connected launchers"))
        ));
    }
}
