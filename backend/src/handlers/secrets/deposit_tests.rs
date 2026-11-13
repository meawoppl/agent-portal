use super::super::tests::{browser_session, request, Database};
use super::*;
use crate::handlers::websocket::{conn_channel, LauncherConnection};
use serde_json::json;
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

type Fixture = (
    Database,
    Arc<AppState>,
    Router,
    Uuid,
    Uuid,
    Uuid,
    String,
    String,
);
fn setup() -> Option<Fixture> {
    let db = Database::new()?;
    let mut state = crate::test_support::test_app_state(db.pool.clone());
    state.secrets = Arc::new(crate::secrets::SecretsService::new(Some(
        crate::secrets::crypto::EnvelopeCrypto {
            key: Arc::new(zeroize::Zeroizing::new(vec![4; 32])),
        },
    )));
    state.allowed_email_domain = Some("example.test".into());
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

    let human = browser_session(&state, owner, "human");
    let bound = proxy_token(&state, owner, Some(session.id));
    let router = super::super::router(state.clone())
        .with_state(state.clone())
        .layer(tower_cookies::CookieManagerLayer::new());
    use crate::schema::user_identities;
    for (user, email) in [
        (owner, "owner@example.test"),
        (other, "recipient@example.test"),
    ] {
        diesel::insert_into(user_identities::table)
            .values((
                user_identities::id.eq(Uuid::new_v4()),
                user_identities::user_id.eq(user),
                user_identities::provider.eq("google"),
                user_identities::subject.eq(user.to_string()),
                user_identities::email.eq(Some(email)),
                user_identities::email_verified.eq(true),
            ))
            .execute(&mut state.conn().unwrap())
            .unwrap();
    }
    Some((db, state, router, owner, other, session.id, human, bound))
}
fn batch(id: Uuid, share: bool) -> serde_json::Value {
    json!({"request_id":id,"items":[{"namespace":"personal","name":"FIRST","value":"canary-first-DO-NOT-LOG","share":if share {vec!["google:recipient@example.test"]} else {vec![]}}, {"namespace":"personal","name":"SECOND","description":"second credential","value":"canary-second-DO-NOT-LOG"}]})
}
async fn send(
    router: &Router,
    token: &str,
    path: &str,
    body: serde_json::Value,
) -> (axum::http::StatusCode, serde_json::Value) {
    request(router, token, "POST", path, body, Uuid::new_v4()).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batch_is_encrypted_write_only_then_atomic_with_owner_edits_and_sharing() {
    let Some((_db, state, router, owner, other, session, human, bound)) = setup() else {
        return;
    };
    let id = Uuid::new_v4();
    let body = batch(id, true);
    let (status, info) = send(&router, &bound, "/api/secrets/deposits", body.clone()).await;
    assert!(status.is_success(), "{info}");
    assert_eq!(info["items"].as_array().unwrap().len(), 2);
    assert!(!info.to_string().contains("DO-NOT-LOG"));
    assert_eq!(
        secrets::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        0
    );
    let row = load(&mut state.conn().unwrap(), id).unwrap();
    assert!(!row
        .envelopes
        .as_ref()
        .unwrap()
        .to_string()
        .contains("DO-NOT-LOG"));
    let transcript = crate::schema::messages::table
        .filter(crate::schema::messages::session_id.eq(session))
        .select(crate::schema::messages::content)
        .load::<String>(&mut state.conn().unwrap())
        .unwrap()
        .join(
            "
",
        );
    assert!(transcript.contains("secret_deposit_proposal"));
    assert!(!transcript.contains("DO-NOT-LOG"));
    assert!(!transcript.contains("FIRST"));
    assert!(send(&router, &bound, "/api/secrets/deposits", body)
        .await
        .0
        .is_success());
    assert_eq!(
        proposals::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        1
    );
    let other_browser = browser_session(&state, other, "other");
    let decision = format!("/api/secrets/deposits/{id}/decision");
    assert_eq!(
        send(&router, &bound, &decision, json!({"approve":true}))
            .await
            .0,
        403
    );
    assert_eq!(
        send(&router, &other_browser, &decision, json!({"approve":true}))
            .await
            .0,
        403
    );
    let path = format!("/api/secrets/deposits/{id}");
    assert_eq!(
        request(
            &router,
            &other_browser,
            "GET",
            &path,
            json!({}),
            Uuid::new_v4()
        )
        .await
        .0,
        403
    );
    let edits = json!({"approve":true,"items":[{"name":"RENAMED","description":"reviewed","recipients":[other]},{"name":"SECOND","description":"second","recipients":[]}]});
    let (status, result) = send(&router, &human, &decision, edits).await;
    assert!(status.is_success(), "{result}");
    assert_eq!(result["state"], "approved");
    assert!(load(&mut state.conn().unwrap(), id)
        .unwrap()
        .envelopes
        .is_none());
    let items: Vec<SecretDepositItem> = serde_json::from_value(result["items"].clone()).unwrap();
    assert_eq!(items[0].name, "RENAMED");
    for (index, item) in items.iter().enumerate() {
        let version = secret_versions::table
            .find(item.version_id)
            .first::<VersionRow>(&mut state.conn().unwrap())
            .unwrap();
        let value = state
            .secrets
            .crypto()
            .unwrap()
            .open(
                Identity {
                    organization: row.organization_id,
                    secret: item.secret_id,
                    version: item.version_id,
                },
                &version.envelope(),
            )
            .await
            .unwrap();
        assert_eq!(
            value.bytes(),
            if index == 0 {
                b"canary-first-DO-NOT-LOG".as_slice()
            } else {
                b"canary-second-DO-NOT-LOG".as_slice()
            }
        );
    }
    let key = store::load(&mut state.conn().unwrap(), items[0].secret_id).unwrap();
    let rights = store::rights(&mut state.conn().unwrap(), &key, other).unwrap();
    assert!(rights.read && rights.use_value);
    assert!(!rights.write && !rights.manage);
    assert_eq!(key.owner_user_id, owner);
    assert_eq!(
        crate::schema::secret_bindings::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        0
    );
    assert!(!request(
        &router,
        &bound,
        "GET",
        "/api/secrets",
        json!({}),
        Uuid::new_v4()
    )
    .await
    .0
    .is_success());
    assert!(send(&router, &human, &decision, json!({"approve":true}))
        .await
        .0
        .is_success());
    assert_eq!(
        secrets::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        2
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conflicts_roll_back_entire_batch_and_denial_or_expiry_erases_staging() {
    let Some((_db, state, router, _owner, _other, _session, human, bound)) = setup() else {
        return;
    };
    let (status, _) = send(
        &router,
        &human,
        "/api/secrets",
        json!({"namespace":"personal","name":"SECOND","value":"existing-value"}),
    )
    .await;
    assert!(status.is_success());
    let id = Uuid::new_v4();
    assert!(
        send(&router, &bound, "/api/secrets/deposits", batch(id, false))
            .await
            .0
            .is_success()
    );
    let path = format!("/api/secrets/deposits/{id}/decision");
    assert_eq!(
        send(&router, &human, &path, json!({"approve":true}))
            .await
            .0,
        409
    );
    assert_eq!(
        secrets::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        1
    );
    assert_eq!(
        load(&mut state.conn().unwrap(), id).unwrap().state,
        "pending"
    );
    assert!(send(&router, &human, &path, json!({"approve":false}))
        .await
        .0
        .is_success());
    assert!(load(&mut state.conn().unwrap(), id)
        .unwrap()
        .envelopes
        .is_none());
    let id = Uuid::new_v4();
    assert!(
        send(&router, &bound, "/api/secrets/deposits", batch(id, false))
            .await
            .0
            .is_success()
    );
    diesel::update(proposals::table.find(id))
        .set(proposals::expires_at.eq(Utc::now() - chrono::Duration::seconds(1)))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    expire(&mut state.conn().unwrap()).unwrap();
    let row = load(&mut state.conn().unwrap(), id).unwrap();
    assert_eq!(row.state, "expired");
    assert!(row.envelopes.is_none());
    let (status, result) = send(
        &router,
        &human,
        &format!("/api/secrets/deposits/{id}/decision"),
        json!({"approve":true}),
    )
    .await;
    assert!(status.is_success());
    assert_eq!(result["state"], "expired");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_process_and_unverified_browser_cannot_approve() {
    let Some((_db, state, router, owner, _, session, human, bound)) = setup() else {
        return;
    };
    let id = Uuid::new_v4();
    assert!(
        send(&router, &bound, "/api/secrets/deposits", batch(id, false))
            .await
            .0
            .is_success()
    );
    let path = format!("/api/secrets/deposits/{id}/decision");
    let cookies = tower_cookies::Cookies::default();
    cookies
        .signed(&state.cookie_key)
        .add(tower_cookies::Cookie::new(
            shared::protocol::SESSION_COOKIE_NAME,
            owner.to_string(),
        ));
    let unverified = format!(
        "browser:{}",
        cookies
            .list()
            .iter()
            .map(|c| format!("{}={}", c.name(), c.value()))
            .collect::<Vec<_>>()
            .join("; ")
    );
    assert_eq!(
        send(&router, &unverified, &path, json!({"approve":true}))
            .await
            .0,
        403
    );
    let replacement = proxy_token(&state, owner, Some(session));
    assert!(!send(
        &router,
        &replacement,
        "/api/secrets/deposits",
        batch(id, false)
    )
    .await
    .0
    .is_success());
    assert!(!send(&router, &human, &path, json!({"approve":true}))
        .await
        .0
        .is_success());
    assert_eq!(
        secrets::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_recipients_extra_grants_and_corrupt_staging_cannot_commit() {
    let Some((_db, state, router, _owner, other, _session, human, bound)) = setup() else {
        return;
    };
    let id = Uuid::new_v4();
    assert!(
        send(&router, &bound, "/api/secrets/deposits", batch(id, true))
            .await
            .0
            .is_success()
    );
    let path = format!("/api/secrets/deposits/{id}/decision");
    diesel::update(crate::schema::users::table.find(other))
        .set(crate::schema::users::disabled.eq(true))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    assert!(!send(&router, &human, &path, json!({"approve":true}))
        .await
        .0
        .is_success());
    assert_eq!(
        secrets::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        0
    );
    diesel::update(crate::schema::users::table.find(other))
        .set(crate::schema::users::disabled.eq(false))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let edits = json!({"approve":true,"items":[{"name":"FIRST","description":"","recipients":[Uuid::new_v4()]},{"name":"SECOND","description":"","recipients":[]}]});
    assert_eq!(send(&router, &human, &path, edits).await.0, 400);
    let row = load(&mut state.conn().unwrap(), id).unwrap();
    let mut envelopes: Vec<Envelope> = serde_json::from_value(row.envelopes.unwrap()).unwrap();
    envelopes[0].ciphertext[0] ^= 1;
    diesel::update(proposals::table.find(id))
        .set(proposals::envelopes.eq(Some(serde_json::to_value(envelopes).unwrap())))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    assert_eq!(
        send(&router, &human, &path, json!({"approve":true}))
            .await
            .0,
        503
    );
    assert_eq!(
        secrets::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        0
    );
    assert!(send(&router, &human, &path, json!({"approve":false}))
        .await
        .0
        .is_success());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_batch_and_failed_notice_leave_no_staged_values() {
    let Some((_db, state, router, _owner, _other, _session, human, bound)) = setup() else {
        return;
    };
    assert!(!send(
        &router,
        &human,
        "/api/secrets/deposits",
        batch(Uuid::new_v4(), false)
    )
    .await
    .0
    .is_success());
    for items in [
        json!([]),
        json!([{"namespace":"personal","name":"X","value":""}]),
        json!([{"namespace":"personal","name":"X","value":"one"},{"namespace":"personal","name":"X","value":"two"}]),
    ] {
        let (status, body) = send(
            &router,
            &bound,
            "/api/secrets/deposits",
            json!({"request_id":Uuid::new_v4(),"items":items}),
        )
        .await;
        assert_eq!(status, 400);
        assert!(!body.to_string().contains("canary"));
    }
    diesel::sql_query("CREATE FUNCTION reject_deposit_notice() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.role = 'portal' THEN RAISE EXCEPTION 'synthetic failure'; END IF; RETURN NEW; END $$").execute(&mut state.conn().unwrap()).unwrap();
    diesel::sql_query("CREATE TRIGGER reject_deposit_notice BEFORE INSERT ON messages FOR EACH ROW EXECUTE FUNCTION reject_deposit_notice()").execute(&mut state.conn().unwrap()).unwrap();
    assert!(!send(
        &router,
        &bound,
        "/api/secrets/deposits",
        batch(Uuid::new_v4(), false)
    )
    .await
    .0
    .is_success());
    assert_eq!(
        proposals::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        0
    );
}
