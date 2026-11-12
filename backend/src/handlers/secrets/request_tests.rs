use super::super::tests::{credential, request, Database};
use super::*;
use crate::handlers::websocket::{conn_channel, LauncherConnection};
use tower::ServiceExt;
fn proxy_token(state: &AppState, user: Uuid, session: Option<Uuid>) -> String {
    let issued = crate::handlers::proxy_tokens::issue_proxy_token(
        &mut state.conn().unwrap(),
        state.jwt_secret.as_bytes(),
        user,
        crate::handlers::proxy_tokens::TokenPersist::Create {
            name: crate::handlers::proxy_tokens::LAUNCH_TOKEN_NAME,
        },
        Some(1),
    )
    .unwrap();
    if let Some(session) = session {
        crate::handlers::proxy_tokens::link_token_to_session(
            &mut state.conn().unwrap(),
            &issued.token,
            session,
        )
    };
    issued.token
}

async fn browser(
    router: &Router,
    state: &AppState,
    user: Uuid,
    method: &str,
    path: &str,
    body: serde_json::Value,
    recent: bool,
) -> (axum::http::StatusCode, serde_json::Value) {
    let cookies = tower_cookies::Cookies::default();
    cookies
        .signed(&state.cookie_key)
        .add(tower_cookies::Cookie::new(
            shared::protocol::SESSION_COOKIE_NAME,
            user.to_string(),
        ));
    if recent {
        auth::record_verification(&cookies, state, user);
    }
    let cookie = cookies
        .list()
        .iter()
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect::<Vec<_>>()
        .join("; ");
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header("cookie", cookie)
                .header("origin", "http://localhost:3000")
                .header("x-portal-secrets", "1")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8192)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_requires_owning_browser_and_creates_one_pinned_binding() {
    let Some(db) = Database::new() else { return };
    let mut state = crate::test_support::test_app_state(db.pool.clone());
    state.secrets = Arc::new(crate::secrets::SecretsService::new(Some(
        crate::secrets::crypto::EnvelopeCrypto {
            key: Arc::new(zeroize::Zeroizing::new(vec![4; 32])),
        },
    )));
    let state = Arc::new(state);
    let owner = crate::test_support::insert_user(&mut state.conn().unwrap(), "binding_owner").id;
    let other = crate::test_support::insert_user(&mut state.conn().unwrap(), "binding_other").id;
    let launcher = Uuid::new_v4();
    let (sender, _launcher_rx) = conn_channel(8);
    state
        .session_manager
        .try_register_launcher(
            launcher,
            LauncherConnection {
                log_span: tracing::Span::none(),
                sender,
                launcher_name: "test".into(),
                hostname: "test".into(),
                user_id: owner,
                running_sessions: vec![],
                working_directory: None,
                version: "test".into(),
                capabilities: vec![],
                cancel: tokio_util::sync::CancellationToken::new(),
                gen: 0,
                last_seen: std::sync::atomic::AtomicU64::new(0),
                system: None,
            },
        )
        .unwrap();
    let session = crate::test_support::insert_session(&mut state.conn().unwrap(), owner, "binding");
    diesel::update(sessions::table.find(session.id))
        .set(sessions::launcher_id.eq(Some(launcher)))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let (sender, _proxy_rx) = conn_channel(8);
    state.session_manager.register_session(
        session.session_key.clone().into(),
        sender,
        tokio_util::sync::CancellationToken::new(),
    );

    let human = credential(&state, owner, "human");
    let bound = proxy_token(&state, owner, Some(session.id));
    let router = super::super::router(state.clone())
        .with_state(state.clone())
        .layer(tower_cookies::CookieManagerLayer::new());
    let (status, secret) = request(
        &router,
        &human,
        "POST",
        "/api/secrets",
        serde_json::json!({"namespace":"personal","name":"TOKEN","value":"never-in-transcript"}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success(), "{secret}");
    // The request and ID-only transcript notice commit atomically.
    diesel::sql_query("CREATE FUNCTION reject_notice() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.role = 'portal' THEN RAISE EXCEPTION 'test notice failure'; END IF; RETURN NEW; END $$").execute(&mut state.conn().unwrap()).unwrap();
    diesel::sql_query("CREATE TRIGGER reject_notice BEFORE INSERT ON messages FOR EACH ROW EXECUTE FUNCTION reject_notice()").execute(&mut state.conn().unwrap()).unwrap();
    let rollback_id = Uuid::new_v4();
    let rollback_body = serde_json::json!({"request_id":rollback_id,"reference":"personal/TOKEN","env_name":"APP_TOKEN","version":null,"expires_in_seconds":60});
    assert!(!request(
        &router,
        &bound,
        "POST",
        "/api/secrets/requests",
        rollback_body,
        Uuid::new_v4()
    )
    .await
    .0
    .is_success());
    assert_eq!(
        requests::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        0
    );
    diesel::sql_query("DROP TRIGGER reject_notice ON messages")
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let id = Uuid::new_v4();
    let body = serde_json::json!({"request_id":id,"reference":"personal/TOKEN","env_name":"APP_TOKEN","version":null,"expires_in_seconds":60});
    for _ in 0..2 {
        let (status, info) = request(
            &router,
            &bound,
            "POST",
            "/api/secrets/requests",
            body.clone(),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "{info}");
        assert_eq!(info["state"], "pending");
        assert_eq!(info["owner_user_id"], owner.to_string());
    }
    assert_eq!(
        requests::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        1
    );
    let notices = crate::schema::messages::table
        .filter(crate::schema::messages::role.eq("portal"))
        .select(crate::schema::messages::content)
        .load::<String>(&mut state.conn().unwrap())
        .unwrap();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains(&id.to_string()));
    assert!(!notices[0].contains("never-in-transcript"));
    assert!(!notices[0].contains("TOKEN"));
    let path = format!("/api/secrets/requests/{id}");
    let decide = format!("{path}/decision");
    let mut changed = body.clone();
    changed["env_name"] = serde_json::json!("OTHER");
    assert_eq!(
        request(
            &router,
            &bound,
            "POST",
            "/api/secrets/requests",
            changed,
            Uuid::new_v4()
        )
        .await
        .0,
        axum::http::StatusCode::CONFLICT
    );
    for token in [&bound, &human] {
        assert_eq!(
            request(
                &router,
                token,
                "POST",
                &decide,
                serde_json::json!({"approve":true}),
                Uuid::new_v4()
            )
            .await
            .0,
            axum::http::StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        browser(
            &router,
            &state,
            other,
            "GET",
            &path,
            serde_json::Value::Null,
            false
        )
        .await
        .0,
        axum::http::StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser(
            &router,
            &state,
            owner,
            "POST",
            &decide,
            serde_json::json!({"approve":true}),
            false
        )
        .await
        .0,
        axum::http::StatusCode::FORBIDDEN
    );
    let (status, approved) = browser(
        &router,
        &state,
        owner,
        "POST",
        &decide,
        serde_json::json!({"approve":true}),
        true,
    )
    .await;
    assert!(status.is_success(), "{approved}");
    assert_eq!(approved["state"], "approved");
    let (_, again) = browser(
        &router,
        &state,
        owner,
        "POST",
        &decide,
        serde_json::json!({"approve":true}),
        true,
    )
    .await;
    assert_eq!(again["binding_id"], approved["binding_id"]);
    assert_eq!(
        secret_bindings::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        1
    );
    // A different session of the same user has no authority over this request.
    let different =
        crate::test_support::insert_session(&mut state.conn().unwrap(), owner, "different");
    let wrong = proxy_token(&state, owner, Some(different.id));
    assert_eq!(
        request(
            &router,
            &wrong,
            "GET",
            &path,
            serde_json::Value::Null,
            Uuid::new_v4()
        )
        .await
        .0,
        axum::http::StatusCode::FORBIDDEN
    );
    // Expiry/denial/rotation cannot be turned into an approval by replay.
    for mode in ["deny", "expire", "rotate", "delete", "replace"] {
        let rid = Uuid::new_v4();
        let mut b = body.clone();
        b["request_id"] = serde_json::json!(rid);
        let (status, info) = request(
            &router,
            &bound,
            "POST",
            "/api/secrets/requests",
            b,
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "{info}");
        let p = format!("/api/secrets/requests/{rid}/decision");
        if mode == "deny" {
            assert_eq!(
                browser(
                    &router,
                    &state,
                    owner,
                    "POST",
                    &p,
                    serde_json::json!({"approve":false}),
                    false
                )
                .await
                .1["state"],
                "denied"
            );
        }
        if mode == "expire" {
            diesel::update(requests::table.find(rid))
                .set(requests::expires_at.eq(Utc::now() - chrono::Duration::seconds(1)))
                .execute(&mut state.conn().unwrap())
                .unwrap();
        }
        if mode == "rotate" {
            let (status, value) = request(
                &router,
                &human,
                "POST",
                &format!("/api/secrets/{}/versions", secret["id"].as_str().unwrap()),
                serde_json::json!({"value":"rotated-value","expected_generation":1}),
                Uuid::new_v4(),
            )
            .await;
            assert!(status.is_success(), "{value}");
        }
        if mode == "delete" {
            let (status, value) = request(
                &router,
                &human,
                "POST",
                &format!("/api/secrets/{}/delete", secret["id"].as_str().unwrap()),
                serde_json::json!({"expected_generation":2}),
                Uuid::new_v4(),
            )
            .await;
            assert!(status.is_success(), "{value}");
        }
        if mode == "replace" {
            let _replacement = proxy_token(&state, owner, Some(session.id));
        }
        let (status, info) = browser(
            &router,
            &state,
            owner,
            "POST",
            &p,
            serde_json::json!({"approve":true}),
            true,
        )
        .await;
        assert!(status.is_success(), "{info}");
        assert_ne!(info["state"], "approved");
        assert!(info["binding_id"].is_null());
        if mode == "delete" {
            let (status, value) = request(
                &router,
                &human,
                "POST",
                &format!("/api/secrets/{}/restore", secret["id"].as_str().unwrap()),
                serde_json::json!({"expected_generation":3,"confirm_sharing":true}),
                Uuid::new_v4(),
            )
            .await;
            assert!(status.is_success(), "{value}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_caps_include_denied_requests_and_gate_fails_before_checkout() {
    let Some(db) = Database::new() else { return };
    let mut state = crate::test_support::test_app_state(db.pool.clone());
    state.secrets = Arc::new(crate::secrets::SecretsService::new(Some(
        crate::secrets::crypto::EnvelopeCrypto {
            key: Arc::new(zeroize::Zeroizing::new(vec![4; 32])),
        },
    )));
    let state = Arc::new(state);
    let owner = crate::test_support::insert_user(&mut state.conn().unwrap(), "binding_owner").id;
    let _other = crate::test_support::insert_user(&mut state.conn().unwrap(), "binding_other").id;
    let launcher = Uuid::new_v4();
    let (sender, _launcher_rx) = conn_channel(8);
    state
        .session_manager
        .try_register_launcher(
            launcher,
            LauncherConnection {
                log_span: tracing::Span::none(),
                sender,
                launcher_name: "test".into(),
                hostname: "test".into(),
                user_id: owner,
                running_sessions: vec![],
                working_directory: None,
                version: "test".into(),
                capabilities: vec![],
                cancel: tokio_util::sync::CancellationToken::new(),
                gen: 0,
                last_seen: std::sync::atomic::AtomicU64::new(0),
                system: None,
            },
        )
        .unwrap();
    let session = crate::test_support::insert_session(&mut state.conn().unwrap(), owner, "binding");
    diesel::update(sessions::table.find(session.id))
        .set(sessions::launcher_id.eq(Some(launcher)))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let (sender, _proxy_rx) = conn_channel(8);
    state.session_manager.register_session(
        session.session_key.clone().into(),
        sender,
        tokio_util::sync::CancellationToken::new(),
    );

    let human = credential(&state, owner, "human");
    let bound = proxy_token(&state, owner, Some(session.id));
    let router = super::super::router(state.clone())
        .with_state(state.clone())
        .layer(tower_cookies::CookieManagerLayer::new());
    let (status, value) = request(
        &router,
        &human,
        "POST",
        "/api/secrets",
        serde_json::json!({"namespace":"personal","name":"TOKEN","value":"quota-test-value"}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success(), "{value}");
    let body = |id| serde_json::json!({"request_id":id,"reference":"personal/TOKEN","env_name":"APP_TOKEN","version":null,"expires_in_seconds":60});
    let permit = state
        .secrets
        .mutation_gate
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let (status, _) = request(
        &router,
        &bound,
        "POST",
        "/api/secrets/requests",
        body(Uuid::new_v4()),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    drop(permit);
    let mut ids = Vec::new();
    for _ in 0..4 {
        let id = Uuid::new_v4();
        let (status, value) = request(
            &router,
            &bound,
            "POST",
            "/api/secrets/requests",
            body(id),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "{value}");
        ids.push(id);
    }
    assert_eq!(
        request(
            &router,
            &bound,
            "POST",
            "/api/secrets/requests",
            body(Uuid::new_v4()),
            Uuid::new_v4()
        )
        .await
        .0,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    for id in ids {
        assert!(browser(
            &router,
            &state,
            owner,
            "POST",
            &format!("/api/secrets/requests/{id}/decision"),
            serde_json::json!({"approve":false}),
            false
        )
        .await
        .0
        .is_success());
    }
    for _ in 4..30 {
        let id = Uuid::new_v4();
        let (status, value) = request(
            &router,
            &bound,
            "POST",
            "/api/secrets/requests",
            body(id),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "{value}");
        assert!(browser(
            &router,
            &state,
            owner,
            "POST",
            &format!("/api/secrets/requests/{id}/decision"),
            serde_json::json!({"approve":false}),
            false
        )
        .await
        .0
        .is_success());
    }
    assert_eq!(
        request(
            &router,
            &bound,
            "POST",
            "/api/secrets/requests",
            body(Uuid::new_v4()),
            Uuid::new_v4()
        )
        .await
        .0,
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(
        requests::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        30
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grouped_requests_are_one_notice_and_atomic_owner_approval() {
    let Some(db) = Database::new() else { return };
    let mut state = crate::test_support::test_app_state(db.pool.clone());
    state.secrets = Arc::new(crate::secrets::SecretsService::new(Some(
        crate::secrets::crypto::EnvelopeCrypto {
            key: Arc::new(zeroize::Zeroizing::new(vec![4; 32])),
        },
    )));
    let state = Arc::new(state);
    let owner = crate::test_support::insert_user(&mut state.conn().unwrap(), "binding_owner").id;
    let other = crate::test_support::insert_user(&mut state.conn().unwrap(), "binding_other").id;
    let launcher = Uuid::new_v4();
    let (sender, _launcher_rx) = conn_channel(8);
    state
        .session_manager
        .try_register_launcher(
            launcher,
            LauncherConnection {
                log_span: tracing::Span::none(),
                sender,
                launcher_name: "test".into(),
                hostname: "test".into(),
                user_id: owner,
                running_sessions: vec![],
                working_directory: None,
                version: "test".into(),
                capabilities: vec![],
                cancel: tokio_util::sync::CancellationToken::new(),
                gen: 0,
                last_seen: std::sync::atomic::AtomicU64::new(0),
                system: None,
            },
        )
        .unwrap();
    let session = crate::test_support::insert_session(&mut state.conn().unwrap(), owner, "binding");
    diesel::update(sessions::table.find(session.id))
        .set(sessions::launcher_id.eq(Some(launcher)))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let (sender, _proxy_rx) = conn_channel(8);
    state.session_manager.register_session(
        session.session_key.clone().into(),
        sender,
        tokio_util::sync::CancellationToken::new(),
    );

    let human = credential(&state, owner, "human");
    let bound = proxy_token(&state, owner, Some(session.id));
    let router = super::super::router(state.clone())
        .with_state(state.clone())
        .layer(tower_cookies::CookieManagerLayer::new());
    let (status, secret) = request(
        &router,
        &human,
        "POST",
        "/api/secrets",
        serde_json::json!({"namespace":"personal","name":"TOKEN","value":"never-in-transcript"}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success(), "{secret}");

    let (status, _) = request(
        &router,
        &human,
        "POST",
        "/api/secrets",
        serde_json::json!({"namespace":"personal","name":"SECOND","value":"second-canary"}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    let id = Uuid::new_v4();
    let body = serde_json::json!({"request_id":id,"expires_in_seconds":900,"items":[
        {"reference":"personal/TOKEN","env_name":"TOKEN"},
        {"reference":"personal/SECOND","env_name":"SECOND"}
    ]});
    let (status, group) = request(
        &router,
        &bound,
        "POST",
        "/api/secrets/request-groups",
        body.clone(),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success(), "{status}: {group}");
    assert_eq!(group["items"].as_array().unwrap().len(), 2);
    let (status, replay) = request(
        &router,
        &bound,
        "POST",
        "/api/secrets/request-groups",
        body.clone(),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    assert_eq!(group, replay);
    let notices = crate::schema::messages::table
        .filter(crate::schema::messages::role.eq("portal"))
        .select(crate::schema::messages::content)
        .load::<String>(&mut state.conn().unwrap())
        .unwrap();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].contains("secret_access_group"));
    assert!(notices[0].contains(&id.to_string()));
    assert!(!notices[0].contains("canary"));
    assert!(!notices[0].contains("TOKEN"));
    let mut changed = body.clone();
    changed["items"][1]["env_name"] = serde_json::json!("OTHER");
    assert_eq!(
        request(
            &router,
            &bound,
            "POST",
            "/api/secrets/request-groups",
            changed,
            Uuid::new_v4()
        )
        .await
        .0,
        axum::http::StatusCode::CONFLICT
    );
    let mut bad = body.clone();
    bad["request_id"] = serde_json::json!(Uuid::new_v4());
    bad["items"][1]["reference"] = serde_json::json!("personal/MISSING");
    assert!(!request(
        &router,
        &bound,
        "POST",
        "/api/secrets/request-groups",
        bad,
        Uuid::new_v4()
    )
    .await
    .0
    .is_success());
    assert_eq!(
        requests::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        2
    );
    let member = group["items"][0]["id"].as_str().unwrap();
    let member_decision = format!("/api/secrets/requests/{member}/decision");
    assert_eq!(
        browser(
            &router,
            &state,
            owner,
            "POST",
            &member_decision,
            serde_json::json!({"approve":true}),
            true
        )
        .await
        .0,
        axum::http::StatusCode::CONFLICT
    );
    let decision = format!("/api/secrets/request-groups/{id}/decision");
    for (user, proof) in [(other, true), (owner, false)] {
        assert_eq!(
            browser(
                &router,
                &state,
                user,
                "POST",
                &decision,
                serde_json::json!({"approve":true}),
                proof
            )
            .await
            .0,
            axum::http::StatusCode::FORBIDDEN
        );
    }
    // Failure while approving the second member must roll back the first binding.
    diesel::sql_query("CREATE FUNCTION reject_second_binding() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.env_name = 'SECOND' THEN RAISE EXCEPTION 'test binding failure'; END IF; RETURN NEW; END $$").execute(&mut state.conn().unwrap()).unwrap();
    diesel::sql_query("CREATE TRIGGER reject_second_binding BEFORE INSERT ON secret_bindings FOR EACH ROW EXECUTE FUNCTION reject_second_binding()").execute(&mut state.conn().unwrap()).unwrap();
    assert!(!browser(
        &router,
        &state,
        owner,
        "POST",
        &decision,
        serde_json::json!({"approve":true}),
        true
    )
    .await
    .0
    .is_success());
    assert_eq!(
        secret_bindings::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        0
    );
    assert_eq!(
        requests::table
            .filter(requests::state.eq("approved"))
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        0
    );
    diesel::sql_query("DROP TRIGGER reject_second_binding ON secret_bindings")
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let (status, approved) = browser(
        &router,
        &state,
        owner,
        "POST",
        &decision,
        serde_json::json!({"approve":true}),
        true,
    )
    .await;
    assert!(status.is_success(), "{status}: {approved}");
    assert_eq!(approved["state"], "approved");
    for item in approved["items"].as_array().unwrap() {
        assert_eq!(item["state"], "approved");
        let binding = item["binding_id"].as_str().unwrap();
        let (status, ticket) = request(
            &router,
            &bound,
            "POST",
            &format!("/api/secrets/bindings/{binding}/ticket"),
            serde_json::Value::Null,
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success());
        let (status, value) = request(
            &router,
            &bound,
            "POST",
            &format!("/api/secrets/bindings/{binding}/redeem"),
            serde_json::json!({"ticket":ticket["ticket"]}),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success());
        assert_eq!(value["env_name"], item["env_name"]);
        assert!(value["value"].as_str().is_some_and(|v| !v.is_empty()));
    }
    let (status, replay) = browser(
        &router,
        &state,
        owner,
        "POST",
        &decision,
        serde_json::json!({"approve":true}),
        true,
    )
    .await;
    assert!(status.is_success());
    assert_eq!(approved, replay);
    assert_eq!(
        secret_bindings::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        2
    );
    // Removing one member must not turn the original set into a smaller approval.
    let member_id = Uuid::parse_str(member).unwrap();
    diesel::delete(requests::table.find(member_id))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let (status, _) = request(
        &router,
        &bound,
        "GET",
        &format!("/api/secrets/request-groups/{id}"),
        serde_json::Value::Null,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
}
