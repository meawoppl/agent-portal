use axum::{
    extract::{Path, Query, State},
    Json,
};
use diesel::prelude::*;
use serde::Deserialize;
use shared::api::{
    DirectoryListingResponse, ForkDirectoryMode, ForkSessionRequest, ForkSessionResponse,
    InstallAgentResponse, LaunchRequest, ProbeAgentsResponse, StartAgentLoginRequest,
    StartAgentLoginResponse, SubmitAgentLoginCodeRequest,
};
use shared::{
    AgentLoginOutcome, AgentType, LauncherInfo, LauncherToServer, ServerToLauncher, SessionRole,
    SessionStatus,
};
use std::sync::Arc;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::handlers::responses::EmptyResponse;
use crate::handlers::websocket::SessionManager;
use crate::models::{jsonb_string_vec, NewSessionMember, NewSessionWithId};
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
    pub session_id: Uuid,
}

/// POST /api/launch - Request launching a new session
pub async fn launch_session(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Json(req): Json<LaunchRequest>,
) -> Result<Json<LaunchResponse>, AppError> {
    let launcher_id = resolve_launch_target(&app_state.session_manager, req.launcher_id, user_id)?;
    let (hostname, version) = app_state
        .session_manager
        .launcher_host_version(launcher_id)
        .ok_or(AppError::NotFound("Launcher not found"))?;
    if req.create_worktree
        && !app_state
            .session_manager
            .launcher_supports_capability(launcher_id, shared::LAUNCHER_CAPABILITY_CREATE_WORKTREE)
    {
        return Err(AppError::BadRequest(
            "Selected launcher is too old for git worktree launches. Update agent-portal on that machine and try again.",
        ));
    }

    // Create a fresh short-lived proxy token for the child process
    let auth_token = mint_launch_token(&app_state, user_id)?;

    // A human-chosen name (when supplied) drives both the display name and,
    // for worktree launches, the worktree branch. With no name we fall back to
    // the working directory's basename — except for a worktree launch, where we
    // mint a `session-<timestamp>` branch and use it as the display name too, so
    // several unnamed worktree sessions of the same repo stay distinguishable in
    // the rail instead of all collapsing onto the shared repo basename.
    let (session_name, worktree_branch) = match (
        normalize_custom_name(req.name.as_deref()),
        req.create_worktree,
    ) {
        (Some(name), true) => (name.clone(), Some(name)),
        (Some(name), false) => (name, None),
        (None, true) => {
            let branch = default_worktree_branch();
            (branch.clone(), Some(branch))
        }
        (None, false) => (default_session_name(&req.working_directory), None),
    };

    let request_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    create_desired_session(
        &app_state,
        DesiredSessionDraft {
            session_id,
            user_id,
            working_directory: req.working_directory.clone(),
            session_name: session_name.clone(),
            hostname,
            launcher_id: Some(launcher_id),
            client_version: Some(version),
            agent_type: req.agent_type,
            claude_args: req.claude_args.clone(),
            forked_from_session_id: None,
            fork_point_turn_id: None,
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
        session_name: Some(session_name),
        claude_args: req.claude_args,
        agent_type: req.agent_type,
        scheduled_task_id: None,
        resume_session_id: Some(session_id),
        // Brand-new session: the id above was just minted, so the launcher must
        // create it under that id, not `--resume` (and rotate) it (#1405).
        resume: Some(false),
        create_worktree: req.create_worktree,
        worktree_branch,
        fork_from_session_id: None,
        fork_point_turn_id: None,
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

    Ok(Json(LaunchResponse {
        request_id,
        session_id,
    }))
}

/// Trim a caller-supplied session name, returning `None` when it is absent or
/// blank so callers fall back to the directory-basename default.
fn normalize_custom_name(name: Option<&str>) -> Option<String> {
    name.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Timestamped default branch/name for an unnamed worktree launch. Built from
/// the shared [`shared::WORKTREE_BRANCH_PREFIX`] shape (the launcher's fallback
/// uses the same consts) so the display name always matches the worktree branch
/// even when the caller supplies no name; generating it here lets the two agree
/// before the launcher runs.
fn default_worktree_branch() -> String {
    format!(
        "{}{}",
        shared::WORKTREE_BRANCH_PREFIX,
        chrono::Local::now().format(shared::WORKTREE_BRANCH_TIME_FORMAT)
    )
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
    session_name: String,
    hostname: String,
    launcher_id: Option<Uuid>,
    client_version: Option<String>,
    agent_type: shared::AgentType,
    claude_args: Vec<String>,
    forked_from_session_id: Option<Uuid>,
    fork_point_turn_id: Option<String>,
}

pub(crate) fn create_desired_session(
    app_state: &AppState,
    draft: DesiredSessionDraft,
) -> Result<(), AppError> {
    let mut conn = app_state.conn()?;

    use crate::schema::{session_members, sessions};
    use diesel::prelude::*;

    let new_session = NewSessionWithId {
        id: draft.session_id,
        user_id: draft.user_id,
        session_name: draft.session_name,
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
        // Stamp the launcher's live version now — the registry entry that
        // holds it is gone by archive time (see
        // `NewSessionWithId::launcher_version`).
        launcher_version: app_state
            .session_manager
            .launcher_version(draft.launcher_id),
    };

    diesel::insert_into(sessions::table)
        .values(&new_session)
        .execute(&mut conn)?;

    if draft.forked_from_session_id.is_some() || draft.fork_point_turn_id.is_some() {
        diesel::update(sessions::table.find(draft.session_id))
            .set((
                sessions::forked_from_session_id.eq(draft.forked_from_session_id),
                sessions::fork_point_turn_id.eq(draft.fork_point_turn_id),
                sessions::fork_launch_pending.eq(true),
                sessions::fork_create_worktree.eq(false),
            ))
            .execute(&mut conn)?;
    }

    diesel::insert_into(session_members::table)
        .values(NewSessionMember {
            session_id: draft.session_id,
            user_id: draft.user_id,
            role: SessionRole::Owner.as_str().to_string(),
        })
        .execute(&mut conn)?;

    Ok(())
}

/// POST /api/sessions/:session_id/fork — create a divergent session on the
/// source session's launcher, where the agent-native conversation state lives.
pub async fn fork_session(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(source_id): Path<Uuid>,
    Json(req): Json<ForkSessionRequest>,
) -> Result<Json<ForkSessionResponse>, AppError> {
    use crate::schema::sessions;

    let mut conn = app_state.conn()?;
    let source = sessions::table
        .find(source_id)
        .filter(sessions::user_id.eq(user_id))
        .first::<crate::models::Session>(&mut conn)
        .optional()?
        .ok_or(AppError::NotFound("Source session not found"))?;

    let launcher_id = source.launcher_id.ok_or(AppError::BadRequest(
        "Proxy-direct sessions cannot be forked",
    ))?;
    if !app_state
        .session_manager
        .launcher_supports_capability(launcher_id, shared::LAUNCHER_CAPABILITY_FORK_SESSION)
    {
        return Err(AppError::BadRequest(
            "Source launcher is offline or does not support session forking",
        ));
    }
    let agent_type = AgentType::parse_or_default(&source.agent_type);
    if agent_type == AgentType::Muse {
        return Err(AppError::BadRequest("Muse sessions cannot be forked"));
    }
    if agent_type != AgentType::Codex && req.fork_point_turn_id.is_some() {
        return Err(AppError::BadRequest(
            "Fork-at-turn is only supported for Codex sessions",
        ));
    }

    let create_worktree = req.directory_mode == ForkDirectoryMode::Worktree;
    if create_worktree
        && !app_state
            .session_manager
            .launcher_supports_capability(launcher_id, shared::LAUNCHER_CAPABILITY_CREATE_WORKTREE)
    {
        return Err(AppError::BadRequest(
            "Source launcher does not support git worktree launches",
        ));
    }
    let working_directory = match req.directory_mode {
        ForkDirectoryMode::Worktree | ForkDirectoryMode::Same => source.working_directory.clone(),
        ForkDirectoryMode::Other => req
            .working_directory
            .filter(|path| !path.trim().is_empty())
            .ok_or(AppError::BadRequest(
                "working_directory is required for other-directory forks",
            ))?,
    };
    let name = normalize_custom_name(Some(&req.name))
        .unwrap_or_else(|| format!("{} (fork)", source.session_name));
    let mut claude_args: Vec<String> = jsonb_string_vec(&source.claude_args);
    if let Some(model) = req.model.filter(|model| !model.trim().is_empty()) {
        claude_args = apply_model_override(claude_args, agent_type, model);
    }

    let (hostname, version) = app_state
        .session_manager
        .launcher_host_version(launcher_id)
        .ok_or(AppError::BadRequest("Source launcher is offline"))?;
    // Mint before persisting the desired row so an auth failure cannot leave
    // behind an orphaned, never-launchable fork.
    let auth_token = mint_launch_token(&app_state, user_id)?;
    let request_id = Uuid::new_v4();
    let session_id = Uuid::new_v4();
    create_desired_session(
        &app_state,
        DesiredSessionDraft {
            session_id,
            user_id,
            working_directory: working_directory.clone(),
            session_name: name.clone(),
            hostname,
            launcher_id: Some(launcher_id),
            client_version: Some(version),
            agent_type,
            claude_args: claude_args.clone(),
            forked_from_session_id: Some(source_id),
            fork_point_turn_id: req.fork_point_turn_id.clone(),
        },
    )?;
    if create_worktree {
        diesel::update(sessions::table.find(session_id))
            .set(sessions::fork_create_worktree.eq(true))
            .execute(&mut conn)?;
    }
    let fork_notice = fork_child_notice(
        &source.session_name,
        source_id,
        req.divergence_prompt.as_deref(),
    );
    app_state
        .session_manager
        .set_last_input_sender(session_id, user_id, "Fork notice".to_string());
    app_state.session_manager.enqueue_input(
        &app_state.db_pool,
        &session_id.to_string(),
        session_id,
        serde_json::Value::String(fork_notice),
        None,
        None,
    );
    app_state
        .session_manager
        .register_launch_session(request_id, session_id);
    let launch = ServerToLauncher::LaunchSession {
        request_id,
        user_id,
        auth_token,
        working_directory,
        session_name: Some(name.clone()),
        claude_args,
        agent_type,
        scheduled_task_id: None,
        resume_session_id: Some(session_id),
        resume: Some(false),
        create_worktree,
        worktree_branch: create_worktree.then_some(name),
        fork_from_session_id: Some(source_id),
        fork_point_turn_id: req.fork_point_turn_id,
    };
    if !app_state
        .session_manager
        .send_to_launcher(&launcher_id, launch)
    {
        app_state.session_manager.cancel_launch_session(request_id);
        diesel::delete(sessions::table.find(session_id)).execute(&mut conn)?;
        return Err(AppError::Internal(
            "Failed to send fork request to launcher".to_string(),
        ));
    }
    Ok(Json(ForkSessionResponse {
        request_id,
        session_id,
    }))
}

fn apply_model_override(args: Vec<String>, agent_type: AgentType, model: String) -> Vec<String> {
    let mut next = Vec::with_capacity(args.len() + 2);
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        if arg == "--model"
            || (arg == "-c"
                && iter
                    .as_slice()
                    .first()
                    .is_some_and(|value| value.starts_with("model=")))
        {
            let _ = iter.next();
        } else {
            next.push(arg);
        }
    }
    match agent_type {
        AgentType::Claude => next.extend(["--model".to_string(), model]),
        AgentType::Codex => next.extend(["-c".to_string(), format!("model={model}")]),
        AgentType::Muse => {}
    }
    next
}

fn fork_child_notice(
    source_name: &str,
    source_id: Uuid,
    divergence_prompt: Option<&str>,
) -> String {
    let direction = divergence_prompt
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
        .unwrap_or("Please await new directions from the user.");
    format!(
        "This session was forked from Agent Portal session \"{source_name}\" ({source_id}). \
         You are the child session, and the source thread is continuing independently. {direction}"
    )
}

fn resolve_launch_target(
    session_manager: &SessionManager,
    requested_launcher_id: Option<Uuid>,
    user_id: Uuid,
) -> Result<Uuid, AppError> {
    if let Some(launcher_id) = requested_launcher_id {
        let owner = session_manager
            .launcher_owner(launcher_id)
            .ok_or(AppError::NotFound("Launcher not found"))?;
        if owner != user_id {
            warn!(
                "User {} attempted to launch on launcher {} owned by {}",
                user_id, launcher_id, owner
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
    require_launcher_owner(&app_state, launcher_id, user_id)?;

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
    require_launcher_owner(&app_state, launcher_id, user_id)?;

    // Route through the evicting sender (not a cloned raw sender) so a dead
    // channel tears the stale connection down instead of lingering.
    if !app_state
        .session_manager
        .send_to_launcher(&launcher_id, ServerToLauncher::UpdateAndRestart)
    {
        warn!("Launcher {} disconnected while sending update", launcher_id);
        return Err(AppError::Internal("Launcher disconnected".to_string()));
    }

    info!("Sent UpdateAndRestart to launcher {}", launcher_id);
    Ok(EmptyResponse::OK)
}

/// POST /api/launchers/:launcher_id/restart - Tell the launcher to restart its
/// process *without* updating the binary. Gated on
/// `LAUNCHER_CAPABILITY_RESTART`: launchers too old to decode
/// `ServerToLauncher::Restart` get a clear 400 instead of a silently-dropped
/// frame.
pub async fn restart_launcher(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(launcher_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    require_launcher_owner(&app_state, launcher_id, user_id)?;

    if !app_state
        .session_manager
        .launcher_supports_capability(launcher_id, shared::LAUNCHER_CAPABILITY_RESTART)
    {
        return Err(AppError::BadRequest(
            "Launcher too old — update it first, then restart becomes available.",
        ));
    }

    // Route through the evicting sender (not a cloned raw sender) so a dead
    // channel tears the stale connection down instead of lingering.
    if !app_state
        .session_manager
        .send_to_launcher(&launcher_id, ServerToLauncher::Restart)
    {
        warn!(
            "Launcher {} disconnected while sending restart",
            launcher_id
        );
        return Err(AppError::Internal("Launcher disconnected".to_string()));
    }

    info!("Sent Restart to launcher {}", launcher_id);
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
    require_launcher_owner(&app_state, launcher_id, user_id)?;

    let request_id = Uuid::new_v4();
    let rx = app_state.session_manager.register_probe_request(request_id);

    // Evicting send (not a cloned raw sender): a dead channel tears the
    // stale connection down instead of lingering.
    if !app_state
        .session_manager
        .send_to_launcher(&launcher_id, ServerToLauncher::ProbeAgents { request_id })
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

/// Confirm the caller owns `launcher_id` (agent logins run credentials on that
/// host, so only its owner may drive them). 404 on unknown so we don't leak
/// launcher existence to non-owners.
fn require_launcher_owner(
    app_state: &AppState,
    launcher_id: Uuid,
    user_id: Uuid,
) -> Result<(), AppError> {
    let owner = app_state
        .session_manager
        .launcher_owner(launcher_id)
        .ok_or(AppError::NotFound("Launcher not found"))?;
    if owner != user_id {
        return Err(AppError::Forbidden);
    }
    Ok(())
}

/// Relay one request/response RPC to a launcher, reusing the probe correlation.
async fn launcher_rpc(
    app_state: &AppState,
    launcher_id: Uuid,
    request_id: Uuid,
    message: ServerToLauncher,
    timeout_secs: u64,
) -> Result<LauncherToServer, AppError> {
    let rx = app_state.session_manager.register_probe_request(request_id);
    if !app_state
        .session_manager
        .send_to_launcher(&launcher_id, message)
    {
        app_state.session_manager.cancel_probe_request(request_id);
        return Err(AppError::BadGateway("Launcher is not connected"));
    }
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx).await {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(_)) => Err(AppError::Internal(
            "Launcher response channel closed".into(),
        )),
        Err(_) => {
            app_state.session_manager.cancel_probe_request(request_id);
            Err(AppError::GatewayTimeout("Launcher did not respond in time"))
        }
    }
}

/// POST /api/launchers/:id/agent-login/start — begin an interactive login for
/// an agent on that host. Returns the URL/code for the user to act on.
pub async fn start_agent_login(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(launcher_id): Path<Uuid>,
    Json(req): Json<StartAgentLoginRequest>,
) -> Result<Json<StartAgentLoginResponse>, AppError> {
    require_launcher_owner(&app_state, launcher_id, user_id)?;

    let request_id = Uuid::new_v4();
    let flow_id = Uuid::new_v4();
    // Starting can wait on the CLI printing its URL (claude ~30s); allow slack.
    let reply = launcher_rpc(
        &app_state,
        launcher_id,
        request_id,
        ServerToLauncher::StartAgentLogin {
            request_id,
            flow_id,
            agent_type: req.agent_type,
        },
        45,
    )
    .await?;

    match reply {
        LauncherToServer::AgentLoginStartResult {
            presentable: Some(presentable),
            interaction: Some(interaction),
            ..
        } => Ok(Json(StartAgentLoginResponse {
            flow_id,
            presentable,
            interaction,
        })),
        LauncherToServer::AgentLoginStartResult {
            error: Some(error), ..
        } => Err(AppError::BadGatewayMessage(error)),
        _ => Err(AppError::Internal(
            "Unexpected launcher login response".into(),
        )),
    }
}

/// POST /api/launchers/:id/agent-login/:flow_id/code — submit the pasted code
/// (claude). Blocks until the flow settles.
pub async fn submit_agent_login_code(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path((launcher_id, flow_id)): Path<(Uuid, Uuid)>,
    Json(req): Json<SubmitAgentLoginCodeRequest>,
) -> Result<Json<AgentLoginOutcome>, AppError> {
    require_launcher_owner(&app_state, launcher_id, user_id)?;
    let request_id = Uuid::new_v4();
    // The CLI can take up to ~a minute to settle after the code is submitted.
    let reply = launcher_rpc(
        &app_state,
        launcher_id,
        request_id,
        ServerToLauncher::SubmitAgentLoginCode {
            request_id,
            flow_id,
            code: req.code,
        },
        90,
    )
    .await?;
    login_outcome(reply)
}

/// GET /api/launchers/:id/agent-login/:flow_id — poll an in-browser login
/// (codex) for completion. `outcome.done == false` = keep polling.
pub async fn poll_agent_login(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path((launcher_id, flow_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<AgentLoginOutcome>, AppError> {
    require_launcher_owner(&app_state, launcher_id, user_id)?;
    let request_id = Uuid::new_v4();
    let reply = launcher_rpc(
        &app_state,
        launcher_id,
        request_id,
        ServerToLauncher::PollAgentLogin {
            request_id,
            flow_id,
        },
        8,
    )
    .await?;
    login_outcome(reply)
}

/// POST /api/launchers/:id/agent-login/:flow_id/cancel — abandon a flow
/// (browser closed). Fire-and-forget; the launcher drops the flow.
pub async fn cancel_agent_login(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path((launcher_id, flow_id)): Path<(Uuid, Uuid)>,
) -> Result<crate::handlers::responses::EmptyResponse, AppError> {
    require_launcher_owner(&app_state, launcher_id, user_id)?;
    app_state
        .session_manager
        .send_to_launcher(&launcher_id, ServerToLauncher::CancelAgentLogin { flow_id });
    Ok(crate::handlers::responses::EmptyResponse::OK)
}

fn login_outcome(reply: LauncherToServer) -> Result<Json<AgentLoginOutcome>, AppError> {
    match reply {
        LauncherToServer::AgentLoginOutcomeResult { outcome, .. } => Ok(Json(outcome)),
        _ => Err(AppError::Internal(
            "Unexpected launcher login response".into(),
        )),
    }
}

/// POST /api/launchers/:id/agents/:agent_type/install — run the agent's install
/// command on that host and report the outcome. Owner-gated: installing on a
/// host is a privileged action, so only the launcher's owner may trigger it.
pub async fn install_agent(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path((launcher_id, agent_type)): Path<(Uuid, AgentType)>,
) -> Result<Json<InstallAgentResponse>, AppError> {
    require_launcher_owner(&app_state, launcher_id, user_id)?;
    let request_id = Uuid::new_v4();
    // A global npm install can run for tens of seconds — allow generous slack.
    let reply = launcher_rpc(
        &app_state,
        launcher_id,
        request_id,
        ServerToLauncher::InstallAgent {
            request_id,
            agent_type,
        },
        180,
    )
    .await?;
    match reply {
        LauncherToServer::InstallAgentResult {
            success, message, ..
        } => Ok(Json(InstallAgentResponse { success, message })),
        _ => Err(AppError::Internal(
            "Unexpected launcher install response".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::websocket::LauncherConnection;

    fn launcher_for(user_id: Uuid, hostname: &str) -> LauncherConnection {
        let (sender, _rx) = crate::handlers::websocket::conn_channel(64);
        LauncherConnection {
            sender,
            launcher_name: format!("launcher-{}", hostname),
            hostname: hostname.to_string(),
            user_id,
            running_sessions: Vec::new(),
            working_directory: None,
            version: "test".to_string(),
            capabilities: vec![shared::LAUNCHER_CAPABILITY_CREATE_WORKTREE.to_string()],
            cancel: tokio_util::sync::CancellationToken::new(),
            gen: 0,
            last_seen: std::sync::atomic::AtomicU64::new(0),
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

    #[test]
    fn custom_name_is_trimmed_and_blanks_fall_back() {
        assert_eq!(
            normalize_custom_name(Some("  api-refactor  ")),
            Some("api-refactor".to_string())
        );
        assert_eq!(normalize_custom_name(Some("   ")), None);
        assert_eq!(normalize_custom_name(Some("")), None);
        assert_eq!(normalize_custom_name(None), None);
    }

    #[test]
    fn default_session_name_uses_directory_basename() {
        assert_eq!(
            default_session_name("/home/ashley/agent-portal"),
            "agent-portal"
        );
        assert_eq!(
            default_session_name("/home/ashley/agent-portal/"),
            "agent-portal"
        );
    }

    #[test]
    fn default_worktree_branch_is_timestamped() {
        let branch = default_worktree_branch();
        // `session-YYYYMMDD-HHMMSS` — prefix plus a 15-char timestamp.
        assert!(branch.starts_with("session-"), "got {branch}");
        let ts = branch.trim_start_matches("session-");
        assert_eq!(ts.len(), 15, "unexpected timestamp in {branch}");
        assert!(ts.chars().all(|c| c.is_ascii_digit() || c == '-'));
    }

    #[test]
    fn fork_model_override_replaces_agent_specific_model_only() {
        let claude = apply_model_override(
            vec!["--model".into(), "old".into(), "--verbose".into()],
            AgentType::Claude,
            "new".into(),
        );
        assert_eq!(claude, ["--verbose", "--model", "new"]);

        let codex = apply_model_override(
            vec!["-c".into(), "model=old".into(), "--yolo".into()],
            AgentType::Codex,
            "new".into(),
        );
        assert_eq!(codex, ["--yolo", "-c", "model=new"]);
    }

    #[test]
    fn fork_notice_identifies_child_and_defaults_to_waiting() {
        let source_id = Uuid::from_u128(0x11111111222233334444555555555555);
        assert_eq!(
            fork_child_notice("research", source_id, None),
            "This session was forked from Agent Portal session \"research\" \
             (11111111-2222-3333-4444-555555555555). You are the child session, and the \
             source thread is continuing independently. Please await new directions from the user."
        );
    }

    #[test]
    fn divergence_prompt_replaces_only_default_direction() {
        let source_id = Uuid::nil();
        let notice = fork_child_notice(
            "implementation",
            source_id,
            Some("  Explore the alternate storage design.  "),
        );
        assert!(notice.contains("You are the child session"));
        assert!(notice.contains("source thread is continuing independently"));
        assert!(notice.ends_with("Explore the alternate storage design."));
        assert!(!notice.contains("await new directions"));
    }
}
