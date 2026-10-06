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
    pub agent_type: Option<shared::AgentType>,
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
    let (launcher_id, working_directory, agent_type) = if let Some(session_id) = query.session_id {
        let session = verify_session_owner(&mut *state.conn()?, session_id, user_id)?;
        (
            session
                .launcher_id
                .ok_or(AppError::BadRequest("Session has no launcher"))?,
            Some(session.working_directory),
            Some(shared::AgentType::parse_or_default(&session.agent_type)),
        )
    } else {
        (
            query
                .launcher_id
                .ok_or(AppError::BadRequest("Select a computer to browse plugins"))?,
            query.working_directory,
            query.agent_type,
        )
    };
    let response = dispatch(
        &state,
        user_id,
        launcher_id,
        PluginRequest {
            agent_type,
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
        let session = verify_session_owner(&mut *state.conn()?, session_id, user_id)?;
        if session.launcher_id != Some(launcher_id) {
            return Err(AppError::BadRequest(
                "Session belongs to a different computer",
            ));
        }
        // Never let a session-scoped command quietly target a different cwd.
        request.working_directory = Some(session.working_directory);
        request.agent_type = Some(shared::AgentType::parse_or_default(&session.agent_type));
    }
    if matches!(request.action, PluginAction::RunCommand { .. }) && request.session_id.is_none() {
        return Err(AppError::BadRequest(
            "Run plugin commands from a session dock so approval and results are recorded",
        ));
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
    // The operation owns its audit lifecycle even if the browser navigates
    // away and drops the HTTP handler while the launcher is still executing.
    tokio::spawn(run_operation(state, user_id, launcher_id, request))
        .await
        .map_err(|error| AppError::Internal(format!("Plugin operation task failed: {error}")))?
        .map(Json)
}

async fn run_operation(
    state: Arc<AppState>,
    user_id: Uuid,
    launcher_id: Uuid,
    request: PluginRequest,
) -> Result<PluginResponse, AppError> {
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
    response
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
    struct PendingGuard<'a>(&'a crate::handlers::websocket::SessionManager, Uuid);
    impl Drop for PendingGuard<'_> {
        fn drop(&mut self) {
            self.0.cancel_plugin_request(self.1);
        }
    }
    let _pending = PendingGuard(&state.session_manager, request_id);

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::websocket::{conn_channel, LauncherConnection};
    use crate::schema::{messages, sessions, users};
    use crate::test_support::{insert_session, insert_user, shared_pool, test_app_state};
    use shared::api::PluginCommandRecord;
    use std::time::Duration;

    fn register_host(
        state: &AppState,
        owner: Uuid,
        capable: bool,
    ) -> (Uuid, tokio::sync::mpsc::Receiver<ServerToLauncher>) {
        let host = Uuid::new_v4();
        let (sender, receiver) = conn_channel(8);
        state
            .session_manager
            .try_register_launcher(
                host,
                LauncherConnection {
                    sender,
                    launcher_name: "plugin test host".into(),
                    hostname: host.to_string(),
                    user_id: owner,
                    running_sessions: Vec::new(),
                    working_directory: Some("/host/default".into()),
                    version: "test".into(),
                    capabilities: if capable {
                        vec![shared::LAUNCHER_CAPABILITY_PLUGINS.into()]
                    } else {
                        Vec::new()
                    },
                    cancel: tokio_util::sync::CancellationToken::new(),
                    gen: 0,
                    last_seen: std::sync::atomic::AtomicU64::new(0),
                },
            )
            .unwrap();
        (host, receiver)
    }

    fn command_request(session_id: Option<Uuid>, approved: bool) -> PluginRequest {
        PluginRequest {
            agent_type: None,
            session_id,
            // The handler must replace caller-supplied cwd with the session's.
            working_directory: Some("/wrong/client/directory".into()),
            action: PluginAction::RunCommand {
                name: "example".into(),
                command: "inspect".into(),
                expected_run: "inspect --summary".into(),
                args: vec!["file with spaces; $(literal)".into(), "--detail".into()],
                approved,
            },
        }
    }

    fn audit_records(state: &AppState, session_id: Uuid) -> Vec<PluginCommandRecord> {
        let rows = messages::table
            .filter(messages::session_id.eq(session_id))
            .select(messages::content)
            .load::<String>(&mut *state.conn().unwrap())
            .unwrap();
        rows.into_iter()
            .flat_map(|content| {
                let message: shared::PortalMessage = serde_json::from_str(&content).unwrap();
                message.content.into_iter().map(|content| match content {
                    shared::PortalContent::PluginCommand { record } => record,
                    other => panic!("unexpected audit content: {other:?}"),
                })
            })
            .collect()
    }

    fn cleanup(state: &AppState, user_id: Uuid) {
        let mut conn = state.conn().unwrap();
        diesel::delete(sessions::table.filter(sessions::user_id.eq(user_id)))
            .execute(&mut conn)
            .unwrap();
        diesel::delete(users::table.find(user_id))
            .execute(&mut conn)
            .unwrap();
    }

    #[tokio::test]
    async fn inventory_rejects_other_owners_and_launchers_without_plugin_capability() {
        let Some(pool) = shared_pool() else {
            return;
        };
        let state = Arc::new(test_app_state(pool));
        let owner = Uuid::new_v4();
        let (host, mut receiver) = register_host(&state, owner, true);
        let result = list_plugins(
            State(state.clone()),
            CurrentUserId(Uuid::new_v4()),
            Query(PluginInventoryQuery {
                agent_type: None,
                launcher_id: Some(host),
                ..Default::default()
            }),
        )
        .await;
        assert!(matches!(result, Err(AppError::Forbidden)));
        assert!(
            receiver.try_recv().is_err(),
            "unauthorized inventory reached host"
        );

        let (old_host, mut old_receiver) = register_host(&state, owner, false);
        let result = list_plugins(
            State(state.clone()),
            CurrentUserId(owner),
            Query(PluginInventoryQuery {
                agent_type: None,
                launcher_id: Some(old_host),
                ..Default::default()
            }),
        )
        .await;
        assert!(matches!(result, Err(AppError::BadRequest(message)) if message.contains("Update")));
        assert!(
            old_receiver.try_recv().is_err(),
            "unsupported inventory reached host"
        );
    }

    #[tokio::test]
    async fn commands_require_session_owner_matching_host_and_explicit_approval() {
        let Some(pool) = shared_pool() else {
            return;
        };
        let state = Arc::new(test_app_state(pool));
        let (user, session) = {
            let mut conn = state.conn().unwrap();
            let user = insert_user(&mut conn, "plugin-command-permission");
            let session = insert_session(&mut conn, user.id, "plugin command permissions");
            (user, session)
        };
        let (host, mut receiver) = register_host(&state, user.id, true);
        diesel::update(sessions::table.find(session.id))
            .set(sessions::launcher_id.eq(Some(host)))
            .execute(&mut *state.conn().unwrap())
            .unwrap();

        let result = manage_plugins(
            State(state.clone()),
            CurrentUserId(user.id),
            Path(host),
            Json(command_request(None, true)),
        )
        .await;
        assert!(
            matches!(result, Err(AppError::BadRequest(message)) if message.contains("session dock"))
        );
        let result = manage_plugins(
            State(state.clone()),
            CurrentUserId(user.id),
            Path(host),
            Json(command_request(Some(session.id), false)),
        )
        .await;
        assert!(
            matches!(result, Err(AppError::BadRequest(message)) if message.contains("Confirm"))
        );
        let result = manage_plugins(
            State(state.clone()),
            CurrentUserId(user.id),
            Path(Uuid::new_v4()),
            Json(command_request(Some(session.id), true)),
        )
        .await;
        assert!(
            matches!(result, Err(AppError::BadRequest(message)) if message.contains("different computer"))
        );
        let result = manage_plugins(
            State(state.clone()),
            CurrentUserId(Uuid::new_v4()),
            Path(host),
            Json(command_request(Some(session.id), true)),
        )
        .await;
        assert!(matches!(
            result,
            Err(AppError::NotFound("Session not found"))
        ));
        assert!(
            receiver.try_recv().is_err(),
            "rejected command reached host"
        );
        assert!(audit_records(&state, session.id).is_empty());
        cleanup(&state, user.id);
    }

    #[tokio::test]
    async fn approved_command_dispatches_typed_rpc_and_persists_exact_approval_and_result() {
        let Some(pool) = shared_pool() else {
            return;
        };
        let state = Arc::new(test_app_state(pool));
        let (user, session) = {
            let mut conn = state.conn().unwrap();
            let user = insert_user(&mut conn, "plugin-command-audit");
            let session = insert_session(&mut conn, user.id, "plugin command audit");
            (user, session)
        };
        let (host, mut receiver) = register_host(&state, user.id, true);
        diesel::update(sessions::table.find(session.id))
            .set((
                sessions::launcher_id.eq(Some(host)),
                sessions::working_directory.eq("/project/actual"),
            ))
            .execute(&mut *state.conn().unwrap())
            .unwrap();
        let request = command_request(Some(session.id), true);
        let mut expected_request = request.clone();
        expected_request.working_directory = Some("/project/actual".into());
        let operation = {
            let state = state.clone();
            tokio::spawn(async move {
                manage_plugins(
                    State(state),
                    CurrentUserId(user.id),
                    Path(host),
                    Json(request),
                )
                .await
            })
        };
        let rpc = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let ServerToLauncher::PluginRequest {
            request_id,
            request,
        } = rpc
        else {
            panic!("expected typed plugin RPC");
        };
        assert_eq!(request, expected_request);
        // Approval must be durable before the host receives any executable action.
        let approved = audit_records(&state, session.id);
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].status, "approved");
        assert!(approved[0].output.is_empty());
        assert_eq!(approved[0].exit_code, None);

        state.session_manager.complete_plugin_request(
            request_id,
            host,
            LauncherToServer::PluginResponse {
                request_id,
                response: PluginResponse {
                    success: true,
                    output: "inspection complete\n".into(),
                    exit_code: Some(0),
                    ..Default::default()
                },
            },
        );
        let response = tokio::time::timeout(Duration::from_secs(5), operation)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .0;
        assert!(response.success);
        assert_eq!(response.output, "inspection complete\n");
        let records = audit_records(&state, session.id);
        assert_eq!(records.len(), 2);
        for record in &records {
            assert_eq!(record.plugin, "example");
            assert_eq!(record.command, "inspect");
            assert_eq!(record.run, "inspect --summary");
            assert_eq!(record.args, ["file with spaces; $(literal)", "--detail"]);
            assert_eq!(record.launcher_id, host);
            assert_eq!(record.working_directory, "/project/actual");
            assert_eq!(record.requested_by, user.id);
        }
        let completed = records
            .iter()
            .find(|record| record.status == "completed")
            .unwrap();
        assert_eq!(completed.output, "inspection complete\n");
        assert_eq!(completed.exit_code, Some(0));
        cleanup(&state, user.id);
    }
}
