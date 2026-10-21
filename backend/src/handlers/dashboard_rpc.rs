//! User-socket adapters share the REST handlers' ownership/capability checks.
//! Keep REST available for CLI consumers and older browser bundles.
use super::launchers;
use crate::{auth::CurrentUserId, errors::AppError, AppState};
use axum::{
    extract::{Path, Query, State},
    response::IntoResponse,
    Json,
};
use diesel::prelude::*;
use shared::api::{DashboardError, DashboardRequest as Request, DashboardResponse as Response};
use std::sync::Arc;
use uuid::Uuid;

pub async fn execute(
    state: Arc<AppState>,
    user: Uuid,
    request: Request,
) -> Result<Response, DashboardError> {
    // A user socket outlives its HTTP upgrade: bans must apply to each action.
    let result = async {
        let mut conn = state.conn()?;
        let disabled = crate::schema::users::table
            .find(user)
            .select(crate::schema::users::disabled)
            .first::<bool>(&mut conn)
            .optional()?;
        if disabled != Some(false) {
            return Err(AppError::Forbidden);
        }
        drop(conn);
        execute_inner(state, user, request).await
    }
    .await;
    encode_result(result).await
}

async fn encode_result(result: Result<Response, AppError>) -> Result<Response, DashboardError> {
    match result {
        Ok(response) => Ok(response),
        Err(error) => {
            let response = error.into_response();
            let status = response.status().as_u16();
            let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap_or_default();
            Err(DashboardError {
                status,
                message: String::from_utf8_lossy(&bytes).into_owned(),
            })
        }
    }
}

async fn execute_inner(
    state: Arc<AppState>,
    user: Uuid,
    request: Request,
) -> Result<Response, AppError> {
    // The dashboard socket upgrade rejects session-bound credentials before
    // this owner-scoped RPC dispatcher is reachable. HTTP handlers still apply
    // their own credential gate for direct callers.
    let state = State(state);
    let user = CurrentUserId(user);
    Ok(match request {
        Request::ListExternalSessions {
            launcher_id,
            agent_type,
            query,
        } => Response::ExternalSessions(
            super::session_imports::list_external_sessions(
                state,
                user,
                Path((launcher_id, agent_type.as_str().to_owned())),
                Query(query),
            )
            .await?
            .0,
        ),
        Request::ImportSession {
            launcher_id,
            agent_type,
            request,
        } => Response::Imported(
            super::session_imports::import_session(
                state,
                user,
                axum::http::HeaderMap::new(),
                Path((launcher_id, agent_type.as_str().to_owned())),
                Json(request),
            )
            .await?
            .0,
        ),
        Request::ListLaunchers => {
            Response::Launchers(launchers::list_launchers(state, user).await?.0)
        }
        Request::ListDirectories { launcher_id, path } => Response::Directories(
            launchers::list_directories(
                state,
                user,
                Path(launcher_id),
                Query(launchers::DirectoryQuery { path }),
            )
            .await?
            .0,
        ),
        Request::ProbeAgents { launcher_id } => Response::Agents(
            launchers::probe_agents(state, user, Path(launcher_id))
                .await?
                .0,
        ),
        Request::Launch(request) => Response::Launched(
            launchers::launch_session(state, user, axum::http::HeaderMap::new(), Json(request))
                .await?
                .0,
        ),
        Request::UpdateLauncher { launcher_id } => {
            launchers::update_launcher(state, user, Path(launcher_id)).await?;
            Response::Complete
        }
        Request::RestartLauncher { launcher_id } => {
            launchers::restart_launcher(state, user, Path(launcher_id)).await?;
            Response::Complete
        }
        Request::StartAgentLogin {
            launcher_id,
            request,
        } => Response::LoginStarted(
            launchers::start_agent_login(state, user, Path(launcher_id), Json(request))
                .await?
                .0,
        ),
        Request::SubmitAgentLoginCode {
            launcher_id,
            flow_id,
            request,
        } => Response::LoginOutcome(
            launchers::submit_agent_login_code(
                state,
                user,
                Path((launcher_id, flow_id)),
                Json(request),
            )
            .await?
            .0,
        ),
        Request::PollAgentLogin {
            launcher_id,
            flow_id,
        } => Response::LoginOutcome(
            launchers::poll_agent_login(state, user, Path((launcher_id, flow_id)))
                .await?
                .0,
        ),
        Request::CancelAgentLogin {
            launcher_id,
            flow_id,
        } => {
            launchers::cancel_agent_login(state, user, Path((launcher_id, flow_id))).await?;
            Response::Complete
        }
        Request::InstallAgent {
            launcher_id,
            agent_type,
        } => Response::AgentInstalled(
            launchers::install_agent(state, user, Path((launcher_id, agent_type)))
                .await?
                .0,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::websocket::{conn_channel, LauncherConnection, LauncherRpcKind};
    use shared::{LauncherToServer, ServerToLauncher};

    fn fixture() -> (
        Arc<AppState>,
        Uuid,
        Uuid,
        tokio::sync::mpsc::Receiver<ServerToLauncher>,
    ) {
        // These owner/correlation paths do not need database access.
        let pool = diesel::r2d2::Pool::builder()
            .max_size(1)
            .min_idle(Some(0))
            .build_unchecked(
                diesel::r2d2::ConnectionManager::<diesel::PgConnection>::new(
                    "postgresql://unused@127.0.0.1:1/unused",
                ),
            );
        let state = Arc::new(crate::test_support::test_app_state(pool));
        let owner = Uuid::new_v4();
        let host = Uuid::new_v4();
        let (sender, receiver) = conn_channel(8);
        state
            .session_manager
            .try_register_launcher(
                host,
                LauncherConnection {
                    system: None,
                    sender,
                    launcher_name: "rpc-test".into(),
                    hostname: "rpc-host".into(),
                    user_id: owner,
                    running_sessions: vec![],
                    working_directory: None,
                    version: "test".into(),
                    capabilities: vec![shared::LAUNCHER_CAPABILITY_RESTART.into()],
                    cancel: tokio_util::sync::CancellationToken::new(),
                    gen: 0,
                    last_seen: std::sync::atomic::AtomicU64::new(0),
                },
            )
            .unwrap();
        (state, owner, host, receiver)
    }

    async fn execute_adapter(
        state: Arc<AppState>,
        user: Uuid,
        request: Request,
    ) -> Result<Response, DashboardError> {
        encode_result(execute_inner(state, user, request).await).await
    }

    #[tokio::test]
    async fn disabled_account_cannot_act_through_an_existing_socket() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let user = {
            let mut conn = pool.get().unwrap();
            let user = crate::test_support::insert_user(&mut conn, "dashboard-disabled");
            diesel::update(crate::schema::users::table.find(user.id))
                .set(crate::schema::users::disabled.eq(true))
                .execute(&mut conn)
                .unwrap();
            user
        };
        let state = Arc::new(crate::test_support::test_app_state(pool));
        let result = execute(state, user.id, Request::ListLaunchers)
            .await
            .unwrap_err();
        assert_eq!(result.status, 403);
    }

    #[tokio::test]
    async fn websocket_adapters_keep_host_ownership_for_reads_and_writes() {
        let (state, _owner, host, mut receiver) = fixture();
        let attacker = Uuid::new_v4();
        for request in [
            Request::ProbeAgents { launcher_id: host },
            Request::RestartLauncher { launcher_id: host },
            Request::ListDirectories {
                launcher_id: host,
                path: "~".into(),
            },
        ] {
            let denied = execute_adapter(state.clone(), attacker, request)
                .await
                .unwrap_err();
            assert_eq!(denied.status, 403);
            assert_eq!(denied.message, "Forbidden");
        }
        assert!(
            receiver.try_recv().is_err(),
            "denied request must never reach the host"
        );
        let Response::Launchers(rows) = execute_adapter(state, attacker, Request::ListLaunchers)
            .await
            .unwrap()
        else {
            panic!("wrong result")
        };
        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn slow_probe_does_not_block_independent_restart() {
        let (state, owner, host, mut receiver) = fixture();
        let probe = tokio::spawn({
            let state = state.clone();
            async move {
                execute_adapter(state, owner, Request::ProbeAgents { launcher_id: host }).await
            }
        });
        let Some(ServerToLauncher::ProbeAgents { request_id }) = receiver.recv().await else {
            panic!("expected probe")
        };
        assert!(matches!(
            execute_adapter(
                state.clone(),
                owner,
                Request::RestartLauncher { launcher_id: host }
            )
            .await,
            Ok(Response::Complete)
        ));
        assert!(matches!(
            receiver.recv().await,
            Some(ServerToLauncher::Restart)
        ));
        state.session_manager.complete_launcher_request(
            LauncherRpcKind::Agent,
            request_id,
            LauncherToServer::ProbeAgentsResult {
                request_id,
                agents: vec![],
            },
        );
        assert!(matches!(probe.await.unwrap(), Ok(Response::Agents(_))));
    }
}
