use super::*;
use crate::schema::{secret_credentials, users};
use crate::secrets::{crypto::EnvelopeCrypto, SecretsService};
use tower::ServiceExt;
use zeroize::Zeroizing;
pub(crate) struct Database {
    pub(crate) pool: crate::db::DbPool,
    url: String,
    schema: String,
}
impl Database {
    pub(crate) fn new() -> Option<Self> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let schema = format!("vault_test_{}", Uuid::new_v4().simple());
        let mut conn = PgConnection::establish(&url).unwrap();
        diesel::sql_query(format!("CREATE SCHEMA {schema}"))
            .execute(&mut conn)
            .unwrap();
        let mut scoped = reqwest::Url::parse(&url).unwrap();
        scoped
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let manager = diesel::r2d2::ConnectionManager::<PgConnection>::new(scoped.to_string());
        let pool = diesel::r2d2::Pool::builder()
            .max_size(4)
            .min_idle(Some(0))
            .build(manager)
            .unwrap();
        crate::db::run_migrations(&pool).unwrap();
        Some(Self { pool, url, schema })
    }
}
impl Drop for Database {
    fn drop(&mut self) {
        if let Ok(mut conn) = PgConnection::establish(&self.url) {
            diesel::sql_query(format!("DROP SCHEMA {} CASCADE", self.schema))
                .execute(&mut conn)
                .unwrap();
        }
    }
}
pub(crate) fn credential(state: &AppState, user: Uuid, label: &str) -> String {
    let token = format!("aps_secret_{}", Uuid::new_v4());
    diesel::insert_into(secret_credentials::table)
        .values((
            secret_credentials::id.eq(Uuid::new_v4()),
            secret_credentials::user_id.eq(user),
            secret_credentials::token_hash.eq(crate::jwt::hash_token(&token)),
            secret_credentials::label.eq(label),
            secret_credentials::metadata_scope.eq(true),
            secret_credentials::mutation_scope.eq(true),
            secret_credentials::read_scope.eq(true),
            secret_credentials::expires_at.eq(Utc::now() + chrono::Duration::hours(1)),
        ))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    token
}
pub(crate) async fn request(
    router: &Router,
    token: &str,
    method: &str,
    path: &str,
    body: serde_json::Value,
    key: Uuid,
) -> (axum::http::StatusCode, serde_json::Value) {
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .header("idempotency-key", key.to_string())
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers().get("cache-control").unwrap(), "no-store");
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
        }),
    )
}
#[tokio::test]
async fn vault_rotation_delete_restore_acl_and_credentials_are_enforced() {
    let Some(db) = Database::new() else { return };
    let mut state = crate::test_support::test_app_state(db.pool.clone());
    state.secrets = Arc::new(SecretsService::new(Some(EnvelopeCrypto {
        key: Arc::new(Zeroizing::new(vec![19; 32])),
    })));
    let state = Arc::new(state);
    let owner = crate::test_support::insert_user(&mut state.conn().unwrap(), "owner").id;
    let other = crate::test_support::insert_user(&mut state.conn().unwrap(), "other").id;
    let admin = crate::test_support::insert_user(&mut state.conn().unwrap(), "admin").id;
    diesel::update(users::table.find(admin))
        .set(users::is_admin.eq(true))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let token = credential(&state, owner, "owner");
    let other_token = credential(&state, other, "recipient");
    let admin_token = credential(&state, admin, "curator");
    let router = router(state.clone())
        .with_state(state.clone())
        .layer(tower_cookies::CookieManagerLayer::new());
    // A valid authenticated caller with an incomplete body cannot reserve
    // the only write slot or an advisory-lock connection.
    let (body_polled, observed) = tokio::sync::oneshot::channel();
    let mut body_polled = Some(body_polled);
    let stream = futures_util::stream::poll_fn(move |_| {
        if let Some(sender) = body_polled.take() {
            let _ = sender.send(());
        }
        std::task::Poll::<Option<Result<axum::body::Bytes, std::io::Error>>>::Pending
    });
    let incomplete = axum::http::Request::builder()
        .method("POST")
        .uri("/api/secrets")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::new_v4().to_string())
        .body(axum::body::Body::from_stream(stream))
        .unwrap();
    let slow = tokio::spawn(router.clone().oneshot(incomplete));
    observed.await.unwrap();
    assert_eq!(state.secrets.mutation_gate.available_permits(), 1);
    assert_eq!(state.secrets.body_gate.available_permits(), 7);
    slow.abort();
    let _ = slow.await;
    assert_eq!(state.secrets.body_gate.available_permits(), 8);
    let body_capacity = state
        .secrets
        .body_gate
        .clone()
        .acquire_many_owned(8)
        .await
        .unwrap();
    let (busy, _) = request(
        &router,
        &token,
        "POST",
        "/api/secrets",
        serde_json::json!({"namespace":"personal","name":"busy","value":"never-written"}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(busy, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(state.secrets.mutation_gate.available_permits(), 1);
    drop(body_capacity);

    // A held mutation slot rejects another distinct key before a lock checkout;
    // it cannot consume the remaining pool capacity and deadlock the first.
    let permit = state
        .secrets
        .mutation_gate
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let idle = state.db_pool.state().idle_connections;
    let (busy, _) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        request(
            &router,
            &token,
            "POST",
            "/api/secrets",
            serde_json::json!({"namespace":"personal","name":"busy","value":"never-written"}),
            Uuid::new_v4(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(busy, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(state.db_pool.state().idle_connections, idle);
    drop(permit);
    let (bad, diagnostic) = request(
        &router,
        &token,
        "POST",
        "/api/secrets",
        serde_json::json!({"namespace":"personal","name":"bad","value":1234567890123u64}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(bad, axum::http::StatusCode::BAD_REQUEST);
    assert!(!diagnostic.to_string().contains("1234567890123"));
    let key = Uuid::new_v4();
    let create_body = serde_json::json!({"namespace":"personal","name":"DEPLOY_TOKEN","value":"first-value-with-newline\n"});
    let (status, created) = request(
        &router,
        &token,
        "POST",
        "/api/secrets",
        create_body.clone(),
        key,
    )
    .await;
    assert!(status.is_success(), "{created}");
    let id = created["id"].as_str().unwrap();
    let id_uuid = Uuid::parse_str(id).unwrap();
    assert!(!created.to_string().contains("first-value"));
    let (status, replayed) =
        request(&router, &token, "POST", "/api/secrets", create_body, key).await;
    assert!(status.is_success());
    assert_eq!(replayed["id"], created["id"]);
    for unauthorized in [&other_token, &admin_token, "old-session-jwt"] {
        let (status, _) = request(
            &router,
            unauthorized,
            "POST",
            &format!("/api/secrets/{id}/read"),
            serde_json::json!({"version":null}),
            Uuid::new_v4(),
        )
        .await;
        assert!(!status.is_success())
    }
    let (status, value) = request(
        &router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/read"),
        serde_json::json!({"version":null}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success(), "{value}");
    assert_eq!(value["value"], "first-value-with-newline\n");
    // Warm matcher sources never become an offline saved-value cache: a
    // provider/key failure still rejects a read, even with rules installed.
    let mut wrong_service = state.secrets.as_ref().clone();
    wrong_service.crypto = Some(EnvelopeCrypto {
        key: Arc::new(Zeroizing::new(vec![99; 32])),
    });
    let mut wrong_state = state.as_ref().clone();
    wrong_state.secrets = Arc::new(wrong_service);
    let wrong_state = Arc::new(wrong_state);
    let wrong_router = super::router(wrong_state.clone())
        .with_state(wrong_state)
        .layer(tower_cookies::CookieManagerLayer::new());
    let (unavailable, _) = request(
        &wrong_router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/read"),
        serde_json::json!({}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(unavailable, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    let (status, rotated) = request(
        &router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/versions"),
        serde_json::json!({"expected_generation":1,"value":"second-value"}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success(), "{rotated}");
    assert_eq!(rotated["current_version"], 2);
    let (_, old) = request(
        &router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/read"),
        serde_json::json!({"version":1}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(old["value"], "first-value-with-newline\n");
    let before =
        crate::secrets::redaction::snapshot_for_user(&mut state.conn().unwrap(), other).unwrap();
    assert_eq!(before.redact_str("second-value"), "second-value");
    let cookies = tower_cookies::Cookies::default();
    cookies
        .signed(&state.cookie_key)
        .add(tower_cookies::Cookie::new(
            shared::protocol::SESSION_COOKIE_NAME,
            owner.to_string(),
        ));
    let grant_body =
        serde_json::json!({"expected_generation":2,"user_id":other,"rights":["read","use"]});
    let browser_grant = |cookies: &tower_cookies::Cookies| {
        let header = cookies
            .list()
            .iter()
            .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
            .collect::<Vec<_>>()
            .join("; ");
        axum::http::Request::builder()
            .method("POST")
            .uri(format!("/api/secrets/{id}/grants"))
            .header("cookie", header)
            .header("origin", "http://localhost:3000")
            .header("x-portal-secrets", "1")
            .header("idempotency-key", Uuid::new_v4().to_string())
            .header("content-type", "application/json")
            .body(axum::body::Body::from(grant_body.to_string()))
            .unwrap()
    };
    let old_cookie = router
        .clone()
        .oneshot(browser_grant(&cookies))
        .await
        .unwrap();
    assert_eq!(
        old_cookie.status(),
        axum::http::StatusCode::FORBIDDEN,
        "Old browser cookies cannot delegate saved-value access"
    );
    auth::record_verification(&cookies, &state, owner);
    let response = router
        .clone()
        .oneshot(browser_grant(&cookies))
        .await
        .unwrap();
    let status = response.status();
    let granted: serde_json::Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(status.is_success(), "{granted}");
    let only_shared =
        crate::secrets::redaction::snapshot_for_user(&mut state.conn().unwrap(), other).unwrap();
    assert_eq!(only_shared.redact_str("second-value"), "second-value");
    let (status, _) = request(
        &router,
        &other_token,
        "POST",
        &format!("/api/secrets/{id}/read"),
        serde_json::json!({"for_execution":true}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    // An archive can hold the rule-generation publication fence while
    // revocation proceeds: ACL changes must not acquire that global row.
    {
        use diesel::connection::SimpleConnection;
        let mut fence = state.conn().unwrap();
        fence.batch_execute("BEGIN").unwrap();
        secrets_generation::table
            .find(1i16)
            .for_share()
            .select(secrets_generation::generation)
            .first::<i64>(&mut fence)
            .unwrap();
        let mut actor = state.conn().unwrap();
        actor
            .transaction::<_, AppError, _>(|conn| {
                conn.batch_execute("SET LOCAL lock_timeout = '500ms'")?;
                let row = store::load(conn, id_uuid)?;
                bump_acl(conn, &row, "test_fence", owner)?;
                // Roll back this probe, leaving the expected public generation.
                Err::<(), _>(AppError::Conflict("rollback fence probe"))
            })
            .err()
            .map(|error| assert!(matches!(error, AppError::Conflict("rollback fence probe"))))
            .unwrap();
        fence.batch_execute("ROLLBACK").unwrap();
    }
    let consented =
        crate::secrets::redaction::snapshot_for_user(&mut state.conn().unwrap(), other).unwrap();
    assert_ne!(consented.redact_str("second-value"), "second-value");
    let (status, deleted) = request(
        &router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/delete"),
        serde_json::json!({"expected_generation":3}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success(), "{deleted}");
    assert_eq!(deleted["tags"], serde_json::json!(["deleted"]));
    for version in [serde_json::Value::Null, serde_json::json!(1)] {
        let (status, _) = request(
            &router,
            &token,
            "POST",
            &format!("/api/secrets/{id}/read"),
            serde_json::json!({"version":version}),
            Uuid::new_v4(),
        )
        .await;
        assert!(!status.is_success())
    }
    let (_, active) = request(
        &router,
        &token,
        "GET",
        "/api/secrets",
        serde_json::Value::Null,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(active["secrets"].as_array().unwrap().len(), 0);
    let (_, deleted_view) = request(
        &router,
        &token,
        "GET",
        "/api/secrets?deleted=true",
        serde_json::Value::Null,
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(deleted_view["secrets"].as_array().unwrap().len(), 1);
    assert!(!deleted_view.to_string().contains("second-value"));
    let (status, _) = request(
        &router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/restore"),
        serde_json::json!({"expected_generation":4,"confirm_sharing":false}),
        Uuid::new_v4(),
    )
    .await;
    assert!(!status.is_success());
    let (status, restored) = request(
        &router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/restore"),
        serde_json::json!({"expected_generation":4,"confirm_sharing":true}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success(), "{restored}");
    let (status, _) = request(
        &router,
        &token,
        "DELETE",
        &format!("/api/secrets/{id}/grants/{other}?expected_generation=5"),
        serde_json::Value::Null,
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    let (status, _) = request(
        &router,
        &other_token,
        "POST",
        &format!("/api/secrets/{id}/read"),
        serde_json::json!({}),
        Uuid::new_v4(),
    )
    .await;
    assert!(!status.is_success());
    use crate::schema::secret_acl_history as history;
    let historical = history::table
        .filter(history::secret_id.eq(id_uuid))
        .order(history::created_at.asc())
        .select((
            history::id,
            history::action,
            history::grantee_user_id,
            history::can_read,
            history::can_use,
        ))
        .load::<(Uuid, String, Uuid, bool, bool)>(&mut state.conn().unwrap())
        .unwrap();
    assert_eq!(historical.len(), 2);
    assert_eq!(historical[0].1, "grant");
    assert_eq!(historical[1].1, "revoke");
    assert!(historical
        .iter()
        .all(|row| row.2 == other && row.3 && row.4));
    assert!(diesel::delete(history::table.find(historical[0].0))
        .execute(&mut state.conn().unwrap())
        .is_err());
    let retained =
        crate::secrets::redaction::snapshot_for_user(&mut state.conn().unwrap(), other).unwrap();
    assert_ne!(retained.redact_str("second-value"), "second-value");
    assert_eq!(
        secret_versions::table
            .filter(secret_versions::secret_id.eq(id_uuid))
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        2
    );
    let versions = secret_versions::table
        .load::<VersionRow>(&mut state.conn().unwrap())
        .unwrap();
    for row in versions {
        assert!(!row
            .encrypted_value
            .windows(12)
            .any(|v| v == b"second-value"));
        assert!(diesel::update(secret_versions::table.find(row.id))
            .set(secret_versions::ordinal.eq(99))
            .execute(&mut state.conn().unwrap())
            .is_err());
    }
    diesel::update(users::table.find(owner))
        .set(users::disabled.eq(true))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let (status, _) = request(
        &router,
        &token,
        "GET",
        "/api/secrets",
        serde_json::Value::Null,
        Uuid::new_v4(),
    )
    .await;
    assert!(!status.is_success());
    let (status,organization)=request(&router,&admin_token,"POST","/api/secrets",serde_json::json!({"namespace":"organization","name":"SHARED_TOKEN","value":"organization-value"}),Uuid::new_v4()).await;
    assert!(status.is_success(), "{organization}");
    let target_only = state
        .secrets
        .redactor_for_secret(&state.db_pool, id_uuid)
        .await
        .unwrap();
    assert_eq!(
        target_only.redact_str("organization-value"),
        "organization-value"
    );
    assert_ne!(target_only.redact_str("second-value"), "second-value");
    assert_ne!(
        target_only.redact_str("first-value-with-newline\n"),
        "first-value-with-newline\n"
    );
    let first_version = secret_versions::table
        .filter(secret_versions::secret_id.eq(id_uuid))
        .filter(secret_versions::ordinal.eq(1))
        .select(secret_versions::id)
        .first::<Uuid>(&mut state.conn().unwrap())
        .unwrap();
    let approved = [first_version];
    let frozen = state
        .secrets
        .redactor_for_secret_versions(&state.db_pool, id_uuid, Some(&approved))
        .await
        .unwrap();
    assert_ne!(
        frozen.redact_str("first-value-with-newline\n"),
        "first-value-with-newline\n"
    );
    assert_eq!(
        frozen.redact_str("second-value"),
        "second-value",
        "Later versions cannot expand curator-approved global cleanup"
    );
    assert!(state
        .secrets
        .redactor_for_secret_versions(&state.db_pool, id_uuid, Some(&[Uuid::new_v4()]))
        .await
        .is_err());
    let new_member =
        crate::test_support::insert_user(&mut state.conn().unwrap(), "future_member").id;
    let new_token = credential(&state, new_member, "future member");
    let org_id = organization["id"].as_str().unwrap();
    let (status, value) = request(
        &router,
        &new_token,
        "POST",
        &format!("/api/secrets/{org_id}/read"),
        serde_json::json!({"for_execution":true}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    assert_eq!(value["value"], "organization-value");
    let (status, _) = request(
        &router,
        &new_token,
        "POST",
        &format!("/api/secrets/{org_id}/versions"),
        serde_json::json!({"expected_generation":1,"value":"forbidden"}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn retained_source_quota_rejects_before_rule_generation_or_value_release() {
    let Some(db) = Database::new() else { return };
    let mut state = crate::test_support::test_app_state(db.pool.clone());
    state.secrets = Arc::new(SecretsService::new(Some(EnvelopeCrypto {
        key: Arc::new(Zeroizing::new(vec![44; 32])),
    })));
    let state = Arc::new(state);
    let owner = crate::test_support::insert_user(&mut state.conn().unwrap(), "quota_owner").id;
    let reader = crate::test_support::insert_user(&mut state.conn().unwrap(), "quota_reader").id;
    let token = credential(&state, owner, "owner");
    let recipient = credential(&state, reader, "reader");
    let router = super::router(state.clone())
        .with_state(state.clone())
        .layer(tower_cookies::CookieManagerLayer::new());
    let value = "x".repeat(65536);
    let (status, created) = request(
        &router,
        &token,
        "POST",
        "/api/secrets",
        serde_json::json!({"namespace":"personal","name":"CAPACITY","value":value}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success(), "{created}");
    let id = created["id"].as_str().unwrap();
    for expected in 1..16 {
        let (status, metadata) = request(
            &router,
            &token,
            "POST",
            &format!("/api/secrets/{id}/versions"),
            serde_json::json!({"expected_generation":expected,"value":value}),
            Uuid::new_v4(),
        )
        .await;
        assert!(status.is_success(), "{metadata}");
    }
    let generation = secrets_generation::table
        .find(1i16)
        .select(secrets_generation::generation)
        .first::<i64>(&mut state.conn().unwrap())
        .unwrap();
    let (status, _) = request(
        &router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/versions"),
        serde_json::json!({"expected_generation":16,"value":"one byte over capacity"}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert_eq!(
        secrets_generation::table
            .find(1i16)
            .select(secrets_generation::generation)
            .first::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        generation
    );
    assert_eq!(
        secret_versions::table
            .count()
            .get_result::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        16
    );
    let (status, _) = request(
        &router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/grants"),
        serde_json::json!({"expected_generation":16,"user_id":reader,"rights":["read","use"]}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    let (status, denied) = request(
        &router,
        &recipient,
        "POST",
        &format!("/api/secrets/{id}/read"),
        serde_json::json!({}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    assert!(!denied.to_string().contains(&value));
    assert_eq!(
        secrets_generation::table
            .find(1i16)
            .select(secrets_generation::generation)
            .first::<i64>(&mut state.conn().unwrap())
            .unwrap(),
        generation
    );
    let (status, saved) = request(
        &router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/read"),
        serde_json::json!({}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    assert_eq!(saved["value"], value);
    assert!(state
        .secrets
        .redaction
        .snapshot(&mut state.conn().unwrap())
        .is_ok());
}

#[tokio::test]
async fn clipboard_read_requires_verified_browser_csrf_and_live_read_grant() {
    let Some(db) = Database::new() else { return };
    let mut state = crate::test_support::test_app_state(db.pool.clone());
    state.secrets = Arc::new(SecretsService::new(Some(EnvelopeCrypto {
        key: Arc::new(Zeroizing::new(vec![23; 32])),
    })));
    let state = Arc::new(state);
    let owner = crate::test_support::insert_user(&mut state.conn().unwrap(), "copy-owner").id;
    let other = crate::test_support::insert_user(&mut state.conn().unwrap(), "copy-other").id;
    let token = credential(&state, owner, "owner");
    let router = router(state.clone())
        .with_state(state.clone())
        .layer(tower_cookies::CookieManagerLayer::new());
    let (status, created) = request(
        &router,
        &token,
        "POST",
        "/api/secrets",
        serde_json::json!({"namespace":"personal","name":"COPY_TEST","value":"clipboard-canary\n"}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    let id = created["id"].as_str().unwrap();
    let path = format!("/api/secrets/{id}/copy");
    let cookies = tower_cookies::Cookies::default();
    cookies
        .signed(&state.cookie_key)
        .add(tower_cookies::Cookie::new(
            shared::protocol::SESSION_COOKIE_NAME,
            owner.to_string(),
        ));
    let browser = |path: &str, cookies: &tower_cookies::Cookies, origin: &str, csrf: bool| {
        let cookie = cookies
            .list()
            .iter()
            .map(|c| format!("{}={}", c.name(), c.value()))
            .collect::<Vec<_>>()
            .join("; ");
        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri(path)
            .header("cookie", cookie)
            .header("origin", origin)
            .header("content-type", "application/json");
        if csrf {
            request = request.header("x-portal-secrets", "1");
        }
        request
            .body(axum::body::Body::from("{\"version\":1}"))
            .unwrap()
    };
    let denied = router
        .clone()
        .oneshot(browser(&path, &cookies, "http://localhost:3000", true))
        .await
        .unwrap();
    assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);
    auth::record_verification(&cookies, &state, owner);
    for (origin, csrf) in [
        ("https://attacker.invalid", true),
        ("http://localhost:3000", false),
    ] {
        let denied = router
            .clone()
            .oneshot(browser(&path, &cookies, origin, csrf))
            .await
            .unwrap();
        assert_eq!(denied.status(), axum::http::StatusCode::FORBIDDEN);
    }
    let (status, _) = request(
        &router,
        &token,
        "POST",
        &path,
        serde_json::json!({}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(
        status,
        axum::http::StatusCode::FORBIDDEN,
        "CLI credentials cannot use browser copy"
    );
    let response = router
        .clone()
        .oneshot(browser(&path, &cookies, "http://localhost:3000", true))
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let value: SecretReadResponse = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 100_000)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value.value.0, "clipboard-canary\n");
    let ordinary = router
        .clone()
        .oneshot(browser(
            &format!("/api/secrets/{id}/read"),
            &cookies,
            "http://localhost:3000",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(
        ordinary.status(),
        axum::http::StatusCode::FORBIDDEN,
        "Browser copy must not open CLI read"
    );
    let other_cookies = tower_cookies::Cookies::default();
    other_cookies
        .signed(&state.cookie_key)
        .add(tower_cookies::Cookie::new(
            shared::protocol::SESSION_COOKIE_NAME,
            other.to_string(),
        ));
    auth::record_verification(&other_cookies, &state, other);
    let denied = router
        .clone()
        .oneshot(browser(
            &path,
            &other_cookies,
            "http://localhost:3000",
            true,
        ))
        .await
        .unwrap();
    assert_eq!(
        denied.status(),
        axum::http::StatusCode::FORBIDDEN,
        "Verification does not grant read rights"
    );
    let (status, _) = request(
        &router,
        &token,
        "POST",
        &format!("/api/secrets/{id}/delete"),
        serde_json::json!({"expected_generation":1}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    let denied = router
        .clone()
        .oneshot(browser(&path, &cookies, "http://localhost:3000", true))
        .await
        .unwrap();
    assert!(
        !denied.status().is_success(),
        "Deleted secrets cannot be copied"
    );
    assert!(!String::from_utf8_lossy(
        &axum::body::to_bytes(denied.into_body(), 100_000)
            .await
            .unwrap()
    )
    .contains("clipboard-canary"));
}

#[tokio::test]
async fn inventory_requires_browser_verification_but_preserves_cli_access() {
    let Some(db) = Database::new() else { return };
    let mut state = crate::test_support::test_app_state(db.pool.clone());
    state.secrets = Arc::new(SecretsService::new(Some(EnvelopeCrypto {
        key: Arc::new(Zeroizing::new(vec![23; 32])),
    })));
    let state = Arc::new(state);
    let owner = crate::test_support::insert_user(&mut state.conn().unwrap(), "inventory-owner").id;
    let token = credential(&state, owner, "owner");
    let router = router(state.clone())
        .with_state(state.clone())
        .layer(tower_cookies::CookieManagerLayer::new());
    let (status, _) = request(
        &router,
        &token,
        "POST",
        "/api/secrets",
        serde_json::json!({"namespace":"personal","name":"INVENTORY_TEST","value":"synthetic"}),
        Uuid::new_v4(),
    )
    .await;
    assert!(status.is_success());
    let cookies = tower_cookies::Cookies::default();
    cookies
        .signed(&state.cookie_key)
        .add(tower_cookies::Cookie::new(
            shared::protocol::SESSION_COOKIE_NAME,
            owner.to_string(),
        ));
    for verified in [false, true] {
        if verified {
            auth::record_verification(&cookies, &state, owner);
        }
        let header = cookies
            .list()
            .iter()
            .map(|c| format!("{}={}", c.name(), c.value()))
            .collect::<Vec<_>>()
            .join("; ");
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/secrets")
                    .header("cookie", header)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            if verified {
                axum::http::StatusCode::OK
            } else {
                axum::http::StatusCode::FORBIDDEN
            }
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&body).contains("INVENTORY_TEST"),
            verified
        );
    }
    let (status, body) = request(
        &router,
        &token,
        "GET",
        "/api/secrets",
        serde_json::json!({}),
        Uuid::new_v4(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["secrets"][0]["name"], "INVENTORY_TEST");
}
