//! Owner-only import of native conversations from a connected launcher.
//! A paused session, its owner membership and scrollback commit atomically.
use crate::db::session_lifecycle::{create_desired_session, DesiredSessionDraft};
use crate::handlers::launcher_rpc::{launcher_rpc, require_launcher_owner};
use crate::schema::{messages, sessions};
use crate::{auth::CurrentUserId, errors::AppError, AppState};
use axum::{
    extract::{Path, Query, State},
    Json,
};
use diesel::prelude::*;
use shared::api::{
    imported_session_id, ExternalSessionList, ExternalSessionQuery, ImportSessionRequest,
    ImportSessionResponse, PreparedSessionImport, MAX_IMPORT_HISTORY_BYTES, MAX_IMPORT_MESSAGES,
};
use shared::{AgentType, LauncherToServer, ServerToLauncher};
use std::sync::Arc;
use uuid::Uuid;

fn require_import(
    state: &AppState,
    launcher: Uuid,
    user: Uuid,
    agent: &str,
) -> Result<AgentType, AppError> {
    require_launcher_owner(state, launcher, user)?;
    if agent != AgentType::Codex.as_str() {
        return Err(AppError::BadRequest(
            "Session import is currently supported for Codex only",
        ));
    }
    if !state
        .session_manager
        .launcher_supports_capability(launcher, shared::LAUNCHER_CAPABILITY_IMPORT_CODEX)
    {
        return Err(AppError::BadRequest(
            "Update agent-portal on this computer to import existing Codex sessions",
        ));
    }
    Ok(AgentType::Codex)
}

pub async fn list_external_sessions(
    State(state): State<Arc<AppState>>,
    CurrentUserId(user): CurrentUserId,
    Path((launcher, agent)): Path<(Uuid, String)>,
    Query(query): Query<ExternalSessionQuery>,
) -> Result<Json<ExternalSessionList>, AppError> {
    let agent_type = require_import(&state, launcher, user, &agent)?;
    let request_id = Uuid::new_v4();
    let reply = launcher_rpc(
        &state,
        launcher,
        request_id,
        ServerToLauncher::ListExternalSessions {
            request_id,
            agent_type,
            query,
        },
        50,
    )
    .await?;
    let LauncherToServer::ExternalSessionsResult { result, .. } = reply else {
        return Err(AppError::BadGateway(
            "Unexpected session discovery response",
        ));
    };
    let mut page = result.map_err(AppError::BadGatewayMessage)?;
    // A failed backend commit may leave a harmless native binding. Show Open
    // only for Portal sessions that actually exist and belong to this owner.
    let ids: Vec<_> = page
        .sessions
        .iter()
        .filter_map(|s| s.portal_session_id)
        .collect();
    let known = sessions::table
        .filter(sessions::id.eq_any(ids))
        .filter(sessions::user_id.eq(user))
        .select(sessions::id)
        .load::<Uuid>(&mut *state.conn()?)?;
    for session in &mut page.sessions {
        if session
            .portal_session_id
            .is_some_and(|id| !known.contains(&id))
        {
            session.portal_session_id = None;
        }
    }
    Ok(Json(page))
}

pub async fn import_session(
    State(state): State<Arc<AppState>>,
    CurrentUserId(user): CurrentUserId,
    headers: axum::http::HeaderMap,
    Path((launcher, agent)): Path<(Uuid, String)>,
    Json(request): Json<ImportSessionRequest>,
) -> Result<Json<ImportSessionResponse>, AppError> {
    crate::auth::require_manual_launch_credential(&state, &headers)?;
    let agent_type = require_import(&state, launcher, user, &agent)?;
    let source = Uuid::parse_str(&request.source_id)
        .map_err(|_| AppError::BadRequest("Invalid Codex thread ID"))?
        .to_string();
    let session_id = imported_session_id(user, launcher, agent_type, &source);
    if imported_exists(&mut *state.conn()?, session_id, user)? {
        return Ok(Json(ImportSessionResponse {
            session_id,
            already_imported: true,
        }));
    }
    let include_history = request.include_history;
    let request_id = Uuid::new_v4();
    let reply = launcher_rpc(
        &state,
        launcher,
        request_id,
        ServerToLauncher::PrepareSessionImport {
            request_id,
            agent_type,
            session_id,
            request: ImportSessionRequest {
                source_id: source.clone(),
                include_history,
            },
        },
        95,
    )
    .await?;
    let LauncherToServer::PrepareSessionImportResult { result, .. } = reply else {
        return Err(AppError::BadGateway("Unexpected import response"));
    };
    let prepared = result.map_err(AppError::BadGatewayMessage)?;
    validate_prepared(&prepared, &source, agent_type)?;
    let (hostname, version) = state
        .session_manager
        .launcher_host_version(launcher)
        .ok_or(AppError::NotFound("Launcher disconnected during import"))?;
    let draft = DesiredSessionDraft {
        session_id,
        user_id: user,
        working_directory: prepared.session.working_directory.clone(),
        session_name: prepared.session.name.clone(),
        hostname,
        launcher_id: Some(launcher),
        client_version: Some(version),
        launcher_version: state.session_manager.launcher_version(Some(launcher)),
        agent_type,
        claude_args: vec![],
        forked_from_session_id: None,
        fork_point_turn_id: None,
        fork_create_worktree: false,
    };
    let mut conn = state.conn()?;
    let result = store_import(&mut conn, draft, prepared, include_history);
    let already_imported = match result {
        Ok(()) => false,
        // The unique session ID serializes concurrent requests. The losing
        // transaction rolls back, including all messages, before this lookup.
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) if imported_exists(&mut conn, session_id, user)? => true,
        Err(e) => return Err(e.into()),
    };
    Ok(Json(ImportSessionResponse {
        session_id,
        already_imported,
    }))
}

fn imported_exists(conn: &mut PgConnection, id: Uuid, user: Uuid) -> QueryResult<bool> {
    diesel::select(diesel::dsl::exists(
        sessions::table
            .filter(sessions::id.eq(id))
            .filter(sessions::user_id.eq(user)),
    ))
    .get_result(conn)
}

fn validate_prepared(
    prepared: &PreparedSessionImport,
    source: &str,
    agent: AgentType,
) -> Result<(), AppError> {
    if prepared.session.source_id != source || prepared.session.agent_type != agent {
        return Err(AppError::BadGateway(
            "Launcher returned a different conversation",
        ));
    }
    let bytes: usize = prepared
        .messages
        .iter()
        .map(|m| m.content.to_string().len())
        .sum();
    if bytes > MAX_IMPORT_HISTORY_BYTES || prepared.messages.len() > MAX_IMPORT_MESSAGES {
        return Err(AppError::PayloadTooLarge(
            "Imported history exceeds the limit; retry without copying history".into(),
        ));
    }
    if prepared
        .messages
        .iter()
        .any(|m| m.role != "user" && m.role != "assistant")
    {
        return Err(AppError::BadGateway("Invalid imported message role"));
    }
    Ok(())
}

fn store_import(
    conn: &mut PgConnection,
    draft: DesiredSessionDraft,
    prepared: PreparedSessionImport,
    include_history: bool,
) -> QueryResult<()> {
    let id = draft.session_id;
    let user = draft.user_id;
    conn.transaction(|conn| {
        create_desired_session(conn, draft)?;
        crate::db::session_lifecycle::pause(conn, id)?;
        diesel::update(sessions::table.find(id)).set(sessions::last_model.eq(prepared.session.model)).execute(conn)?;
        let mut rows: Vec<_> = prepared.messages.into_iter().map(|m| crate::models::NewMessage {
            session_id: id, user_id: user, role: m.role, content: m.content.to_string(), agent_type: "codex".into(),
            provenance_kind: None, provenance_session_id: None, provenance_agent_type: None,
        }).collect();
        let history = if include_history { "Saved conversation history was copied. Attachments remain in Codex; local media is shown as a reference." }
            else { "Earlier messages were not copied into Portal. The complete saved context remains in Codex." };
        rows.push(crate::models::NewMessage {
            session_id: id, user_id: user, role: "assistant".into(), agent_type: "codex".into(),
            content: shared::PortalMessage::text(format!("**Imported Codex conversation**\n\n{history}\n\nThis session is paused. Finish any work in the original client and close that conversation, then use **Resume** here. Running commands are not transferred.")).to_json().to_string(),
            provenance_kind: None, provenance_session_id: None, provenance_agent_type: None,
        });
        // Stay below PostgreSQL's bind-parameter limit on large conversations.
        for chunk in rows.chunks(500) { diesel::insert_into(messages::table).values(chunk).execute(conn)?; }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{insert_user, shared_pool};
    use shared::api::{ExternalSession, ImportedMessage};
    fn prepared() -> PreparedSessionImport {
        PreparedSessionImport {
            session: ExternalSession {
                source_id: "native-thread".into(),
                agent_type: AgentType::Codex,
                name: "Imported work".into(),
                working_directory: "/home/test/repo".into(),
                updated_at: 1,
                model: Some("saved-model".into()),
                portal_session_id: None,
            },
            messages: vec![ImportedMessage {
                role: "user".into(),
                content: serde_json::Value::String("saved message".into()),
            }],
        }
    }
    #[test]
    fn wrong_source_agent_and_roles_are_rejected() {
        assert!(validate_prepared(&prepared(), "different", AgentType::Codex).is_err());
        assert!(validate_prepared(&prepared(), "native-thread", AgentType::Claude).is_err());
        let mut input = prepared();
        input.messages[0].role = "system".into();
        assert!(validate_prepared(&input, "native-thread", AgentType::Codex).is_err());
        assert!(validate_prepared(&prepared(), "native-thread", AgentType::Codex).is_ok());
    }
    #[test]
    fn import_commits_paused_with_owner_history_and_no_queued_input() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        conn.test_transaction::<_, diesel::result::Error, _>(|conn| {
            let user = insert_user(conn, "import");
            let id = Uuid::new_v4();
            let draft = || DesiredSessionDraft {
                session_id: id,
                user_id: user.id,
                working_directory: "/home/test/repo".into(),
                session_name: "Imported work".into(),
                hostname: "test-host".into(),
                launcher_id: Some(Uuid::new_v4()),
                client_version: None,
                launcher_version: None,
                agent_type: AgentType::Codex,
                claude_args: vec![],
                forked_from_session_id: None,
                fork_point_turn_id: None,
                fork_create_worktree: false,
            };
            store_import(conn, draft(), prepared(), true)?;
            let session = sessions::table
                .find(id)
                .first::<crate::models::Session>(conn)?;
            assert!(session.paused);
            assert_eq!(session.last_model.as_deref(), Some("saved-model"));
            assert!(imported_exists(conn, id, user.id)?);
            assert!(!imported_exists(conn, id, Uuid::new_v4())?);
            assert_eq!(
                messages::table
                    .filter(messages::session_id.eq(id))
                    .count()
                    .get_result::<i64>(conn)?,
                2
            );
            use crate::schema::{pending_inputs, session_members};
            assert_eq!(
                session_members::table
                    .filter(session_members::session_id.eq(id))
                    .count()
                    .get_result::<i64>(conn)?,
                1
            );
            assert_eq!(
                pending_inputs::table
                    .filter(pending_inputs::session_id.eq(id))
                    .count()
                    .get_result::<i64>(conn)?,
                0
            );
            assert!(store_import(conn, draft(), prepared(), true).is_err());
            assert_eq!(
                messages::table
                    .filter(messages::session_id.eq(id))
                    .count()
                    .get_result::<i64>(conn)?,
                2
            );
            Ok(())
        });
    }
    fn connected_test_state(
        pool: crate::db::DbPool,
        owner: Uuid,
        capable: bool,
    ) -> (
        Arc<AppState>,
        Uuid,
        tokio::sync::mpsc::Receiver<ServerToLauncher>,
    ) {
        use crate::handlers::websocket::{conn_channel, LauncherConnection};
        let state = Arc::new(crate::test_support::test_app_state(pool));
        let launcher = Uuid::new_v4();
        let (sender, receiver) = conn_channel(8);
        state
            .session_manager
            .try_register_launcher(
                launcher,
                LauncherConnection {
                    sender,
                    launcher_name: "import test".into(),
                    hostname: "import-host".into(),
                    user_id: owner,
                    running_sessions: vec![],
                    working_directory: None,
                    version: "test".into(),
                    capabilities: if capable {
                        vec![shared::LAUNCHER_CAPABILITY_IMPORT_CODEX.into()]
                    } else {
                        vec![]
                    },
                    cancel: tokio_util::sync::CancellationToken::new(),
                    gen: 0,
                    last_seen: std::sync::atomic::AtomicU64::new(0),
                    system: None,
                },
            )
            .unwrap();
        (state, launcher, receiver)
    }

    #[tokio::test]
    async fn owner_and_capability_gates_run_before_native_discovery() {
        let Some(pool) = shared_pool() else { return };
        let owner = Uuid::new_v4();
        let (state, launcher, mut receiver) = connected_test_state(pool, owner, false);
        let result = list_external_sessions(
            State(state.clone()),
            CurrentUserId(Uuid::new_v4()),
            Path((launcher, "codex".into())),
            Query(ExternalSessionQuery::default()),
        )
        .await;
        assert!(matches!(result, Err(AppError::Forbidden)));
        let result = list_external_sessions(
            State(state),
            CurrentUserId(owner),
            axum::http::HeaderMap::new(),
            Path((launcher, "codex".into())),
            Query(ExternalSessionQuery::default()),
        )
        .await;
        assert!(matches!(result, Err(AppError::BadRequest(_))));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn concurrent_imports_commit_one_session_and_retry_needs_no_native_read() {
        let Some(pool) = shared_pool() else { return };
        let owner = insert_user(&mut pool.get().unwrap(), "concurrent-import").id;
        let (state, launcher, mut receiver) = connected_test_state(pool.clone(), owner, true);
        let source = Uuid::new_v4().to_string();
        let mut tasks = vec![];
        for _ in 0..2 {
            let state = state.clone();
            let source = source.clone();
            tasks.push(tokio::spawn(async move {
                import_session(
                    State(state),
                    CurrentUserId(owner),
                    axum::http::HeaderMap::new(),
                    Path((launcher, "codex".into())),
                    Json(ImportSessionRequest {
                        source_id: source,
                        include_history: true,
                    }),
                )
                .await
                .unwrap()
                .0
            }));
        }
        // Wait for both requests before replying: both handlers pass their
        // initial existence check, exercising the unique-ID race at commit.
        let mut requests = vec![];
        for _ in 0..2 {
            requests.push(receiver.recv().await.unwrap());
        }
        for request in requests {
            let ServerToLauncher::PrepareSessionImport { request_id, .. } = request else {
                panic!("unexpected RPC");
            };
            let mut snapshot = prepared();
            snapshot.session.source_id = source.clone();
            state.session_manager.complete_launcher_request(
                crate::handlers::websocket::LauncherRpcKind::Agent,
                request_id,
                LauncherToServer::PrepareSessionImportResult {
                    request_id,
                    result: Ok(snapshot),
                },
            );
        }
        let mut results = vec![];
        for task in tasks {
            results.push(task.await.unwrap());
        }
        assert_eq!(results[0].session_id, results[1].session_id);
        assert_ne!(results[0].already_imported, results[1].already_imported);
        let retry = import_session(
            State(state),
            CurrentUserId(owner),
            axum::http::HeaderMap::new(),
            Path((launcher, "codex".into())),
            Json(ImportSessionRequest {
                source_id: source,
                include_history: true,
            }),
        )
        .await
        .unwrap()
        .0;
        assert!(retry.already_imported);
        assert!(receiver.try_recv().is_err());
        let mut conn = pool.get().unwrap();
        assert_eq!(
            messages::table
                .filter(messages::session_id.eq(retry.session_id))
                .count()
                .get_result::<i64>(&mut conn)
                .unwrap(),
            2
        );
        diesel::delete(sessions::table.find(retry.session_id))
            .execute(&mut conn)
            .unwrap();
        diesel::delete(crate::schema::users::table.find(owner))
            .execute(&mut conn)
            .unwrap();
    }
}
