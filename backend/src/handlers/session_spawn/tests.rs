use super::*;
use crate::handlers::proxy_tokens::{issue_proxy_token, TokenPersist, LAUNCH_TOKEN_NAME};
use crate::test_support::{insert_session, insert_user, shared_pool, test_app_state};

struct Fixture {
    state: Arc<AppState>,
    parent: Session,
    headers: HeaderMap,
    receiver: tokio::sync::mpsc::Receiver<shared::ServerToLauncher>,
}
fn fixture() -> Option<Fixture> {
    let pool = shared_pool()?;
    let state = Arc::new(test_app_state(pool));
    let mut conn = state.conn().unwrap();
    let user = insert_user(&mut conn, "spawn");
    let mut parent = insert_session(&mut conn, user.id, "parent");
    let host = Uuid::new_v4();
    parent.launcher_id = Some(host);
    diesel::update(sessions::table.find(parent.id))
        .set(sessions::launcher_id.eq(host))
        .execute(&mut conn)
        .unwrap();
    let (sender, receiver) = crate::handlers::websocket::conn_channel(32);
    state
        .session_manager
        .try_register_launcher(
            host,
            crate::handlers::websocket::LauncherConnection {
                sender,
                launcher_name: "test".into(),
                hostname: host.to_string(),
                user_id: user.id,
                running_sessions: vec![],
                working_directory: None,
                version: "test".into(),
                capabilities: vec![
                    shared::LAUNCHER_CAPABILITY_CLI_SPAWN.into(),
                    shared::LAUNCHER_CAPABILITY_CREATE_WORKTREE.into(),
                ],
                cancel: tokio_util::sync::CancellationToken::new(),
                gen: 0,
                last_seen: std::sync::atomic::AtomicU64::new(0),
                system: None,
            },
        )
        .unwrap();
    let issued = issue_proxy_token(
        &mut conn,
        state.jwt_secret.as_bytes(),
        user.id,
        TokenPersist::Create {
            name: LAUNCH_TOKEN_NAME,
        },
        None,
    )
    .unwrap();
    diesel::update(crate::schema::proxy_auth_tokens::table.find(issued.row_id))
        .set(crate::schema::proxy_auth_tokens::session_id.eq(parent.id))
        .execute(&mut conn)
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {}", issued.token).parse().unwrap(),
    );
    drop(conn);
    Some(Fixture {
        state,
        parent,
        headers,
        receiver,
    })
}
fn request() -> SpawnSessionRequest {
    SpawnSessionRequest {
        request_id: Uuid::new_v4(),
        launcher_id: None,
        prompt: "Review the parser".into(),
        plugins: Some([("example".into(), true)].into()),
        launch: shared::LaunchSpec {
            working_directory: "/tmp".into(),
            session_name: Some("reviewer".into()),
            claude_args: vec![],
            agent_type: shared::AgentType::Claude,
            worktree: shared::WorktreeMode::Scratch {
                branch: Some("review".into()),
            },
        },
    }
}
async fn submit(f: &Fixture, req: SpawnSessionRequest) -> Result<SpawnRequestInfo, SpawnFailure> {
    start(
        State(f.state.clone()),
        Path(f.parent.id),
        f.headers.clone(),
        Json(req),
    )
    .await
    .map(|j| j.0)
}
#[tokio::test]
async fn approval_is_durable_idempotent_and_replays_the_launch_recipe() {
    let Some(mut f) = fixture() else { return };
    let req = request();
    let pending = submit(&f, req.clone()).await.unwrap();
    assert_eq!(pending.state, SpawnRequestState::Pending);
    assert!(f.receiver.try_recv().is_err());
    assert_eq!(
        submit(&f, req.clone()).await.unwrap().request_id,
        pending.request_id
    );
    let approved = approve(
        State(f.state.clone()),
        BrowserUserId(f.parent.user_id),
        Path((f.parent.id, req.request_id)),
    )
    .await
    .unwrap()
    .0;
    let child = approved.child_session_id.unwrap();
    let launch = f.receiver.try_recv().unwrap();
    assert!(
        matches!(launch,shared::ServerToLauncher::LaunchSession{resume:Some(false),scratch_worktree:true,plugin_selection:Some(p),..} if p.get("example")==Some(&true))
    );
    let _ = approve(
        State(f.state.clone()),
        BrowserUserId(f.parent.user_id),
        Path((f.parent.id, req.request_id)),
    )
    .await
    .unwrap();
    assert!(f.receiver.try_recv().is_err());
    let mut conn = f.state.conn().unwrap();
    let actual = owned(&mut conn, f.parent.user_id, child).unwrap();
    assert_eq!(actual.spawned_by, Some(f.parent.id));
    assert_eq!(actual.spawn_depth, 2);
    let content = crate::schema::pending_inputs::table
        .filter(crate::schema::pending_inputs::session_id.eq(child))
        .select(crate::schema::pending_inputs::content)
        .first::<String>(&mut conn)
        .unwrap();
    assert!(content.contains("Review the parser"));
    assert!(content.contains(&f.parent.id.to_string()));
    assert!(launch_recipe(&mut conn, child).unwrap().unwrap().1);
    registered(&mut conn, child).unwrap();
    assert!(!launch_recipe(&mut conn, child).unwrap().unwrap().1);
    assert_eq!(
        requests::table
            .find(req.request_id)
            .select(requests::approved_by)
            .first::<Option<Uuid>>(&mut conn)
            .unwrap(),
        Some(f.parent.user_id)
    );
    // First browser approval enables subsequent requests without a second dialog.
    drop(conn);
    assert_eq!(
        submit(&f, request()).await.unwrap().state,
        SpawnRequestState::Launching
    );
}
#[tokio::test]
async fn reservations_count_limits_and_cannot_forge_parent_or_host() {
    let Some(f) = fixture() else { return };
    let mut conn = f.state.conn().unwrap();
    save_policy(
        &mut conn,
        f.parent.id,
        &SpawnPolicy {
            max_children: 1,
            ..Default::default()
        },
    )
    .unwrap();
    drop(conn);
    let first = request();
    submit(&f, first.clone()).await.unwrap();
    assert_eq!(submit(&f, request()).await.unwrap_err().code, "child_limit");
    let mut conflict = first.clone();
    conflict.prompt = "different".into();
    assert_eq!(
        submit(&f, conflict).await.unwrap_err().code,
        "request_conflict"
    );
    let mut conn = f.state.conn().unwrap();
    let other = insert_session(&mut conn, f.parent.user_id, "other");
    drop(conn);
    assert_eq!(
        agent_parent(&f.state, &f.headers, other.id)
            .unwrap_err()
            .code,
        "session_credential_required"
    );
    assert!(validate_launch(&f.state, &f.parent, Uuid::new_v4(), &SpawnPolicy::default()).is_err());
    let _ = deny(
        State(f.state.clone()),
        BrowserUserId(f.parent.user_id),
        Path((f.parent.id, first.request_id)),
    )
    .await
    .unwrap();
    let mut conn = f.state.conn().unwrap();
    diesel::update(sessions::table.find(f.parent.id))
        .set(sessions::spawn_depth.eq(2))
        .execute(&mut conn)
        .unwrap();
    drop(conn);
    assert_eq!(submit(&f, request()).await.unwrap_err().code, "depth_limit");
}
#[tokio::test]
async fn child_stop_and_parent_end_idle_failure_policies() {
    let Some(f) = fixture() else { return };
    let mut conn = f.state.conn().unwrap();
    save_policy(
        &mut conn,
        f.parent.id,
        &SpawnPolicy {
            approval_mode: SpawnApprovalMode::Auto,
            stop_on_parent_end: true,
            ..Default::default()
        },
    )
    .unwrap();
    drop(conn);
    let info = submit(&f, request()).await.unwrap();
    let child = info.child_session_id.unwrap();
    let mut conn = f.state.conn().unwrap();
    let unrelated = insert_session(&mut conn, f.parent.user_id, "unrelated");
    drop(conn);
    assert_eq!(
        stop_child(
            State(f.state.clone()),
            Path((f.parent.id, unrelated.id)),
            f.headers.clone()
        )
        .await
        .unwrap_err()
        .code,
        "not_own_child"
    );
    let mut conn = f.state.conn().unwrap();
    crate::db::session_lifecycle::pause(&mut conn, f.parent.id).unwrap();
    drop(conn);
    sweep(f.state.clone()).await.unwrap();
    let mut conn = f.state.conn().unwrap();
    assert!(owned(&mut conn, f.parent.user_id, child).unwrap().paused);
    assert!(crate::auth::require_session_lifecycle_scope(&f.state, &f.headers, child).is_ok());
    assert!(
        crate::auth::require_session_lifecycle_scope(&f.state, &f.headers, f.parent.id).is_ok()
    );
    assert!(
        crate::auth::require_session_lifecycle_scope(&f.state, &f.headers, unrelated.id).is_err()
    );
    // Failure pauses only initial children; ordinary sessions still reconcile.
    launch_failed(&mut conn, unrelated.id).unwrap();
    assert!(
        !owned(&mut conn, f.parent.user_id, unrelated.id)
            .unwrap()
            .paused
    );
    // Parent deletion cannot reset the immutable depth or delete the audit.
    diesel::delete(sessions::table.find(f.parent.id))
        .execute(&mut conn)
        .unwrap();
    let actual = owned(&mut conn, f.parent.user_id, child).unwrap();
    assert_eq!(actual.spawned_by, None);
    assert_eq!(actual.spawn_depth, 2);
    assert!(requests::table
        .find(info.request_id)
        .select(requests::id)
        .first::<Uuid>(&mut conn)
        .is_ok());
}
#[tokio::test]
async fn owner_rate_limit_applies_across_parents() {
    let Some(f) = fixture() else { return };
    let mut conn = f.state.conn().unwrap();
    save_policy(
        &mut conn,
        f.parent.id,
        &SpawnPolicy {
            max_children: 32,
            ..Default::default()
        },
    )
    .unwrap();
    drop(conn);
    for _ in 0..12 {
        submit(&f, request()).await.unwrap();
    }
    assert_eq!(submit(&f, request()).await.unwrap_err().code, "rate_limit");
}
#[test]
fn policy_bounds_and_default_consent() {
    let p = SpawnPolicy::default();
    assert_eq!(p.approval_mode, SpawnApprovalMode::FirstSpawn);
    assert_eq!((p.max_children, p.max_depth), (4, 2));
    assert!(validate_policy(&SpawnPolicy {
        max_children: 0,
        ..p.clone()
    })
    .is_err());
    assert!(validate_policy(&SpawnPolicy {
        max_depth: 9,
        ..p.clone()
    })
    .is_err());
    assert!(validate_policy(&SpawnPolicy {
        idle_timeout_seconds: 1,
        ..p
    })
    .is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_cannot_overbook_one_remaining_slot() {
    let Some(f) = fixture() else { return };
    save_policy(
        &mut *f.state.conn().unwrap(),
        f.parent.id,
        &SpawnPolicy {
            max_children: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(4));
    let mut tasks = Vec::new();
    for _ in 0..4 {
        let state = f.state.clone();
        let headers = f.headers.clone();
        let parent = f.parent.id;
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            start(State(state), Path(parent), headers, Json(request())).await
        }));
    }
    let mut allowed = 0;
    for task in tasks {
        match task.await.unwrap() {
            Ok(_) => allowed += 1,
            Err(e) => assert_eq!(e.code, "child_limit"),
        }
    }
    assert_eq!(allowed, 1);
}
#[tokio::test]
async fn idle_children_pause_without_stopping_unrelated_sessions() {
    let Some(f) = fixture() else { return };
    save_policy(
        &mut *f.state.conn().unwrap(),
        f.parent.id,
        &SpawnPolicy {
            approval_mode: SpawnApprovalMode::Auto,
            idle_timeout_seconds: 60,
            ..Default::default()
        },
    )
    .unwrap();
    let info = submit(&f, request()).await.unwrap();
    let child = info.child_session_id.unwrap();
    let mut conn = f.state.conn().unwrap();
    let old = chrono::Utc::now().naive_utc() - chrono::Duration::minutes(2);
    diesel::update(sessions::table.find(child))
        .set((
            sessions::last_activity.eq(old),
            sessions::last_messaged_at.eq(old),
        ))
        .execute(&mut conn)
        .unwrap();
    drop(conn);
    sweep(f.state.clone()).await.unwrap();
    let mut conn = f.state.conn().unwrap();
    assert!(owned(&mut conn, f.parent.user_id, child).unwrap().paused);
    assert!(
        !owned(&mut conn, f.parent.user_id, f.parent.id)
            .unwrap()
            .paused
    );
}
