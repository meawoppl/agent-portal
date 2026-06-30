#![allow(clippy::expect_used, clippy::unwrap_used)]
//! E2E WS/protocol test harness (#1209 item 1, slice 1).
//!
//! Boots the real backend (dev mode) on an ephemeral port against a test
//! Postgres, connects a fake proxy over the real `/ws/session` endpoint using
//! the same `ws-bridge` client the proxy uses, and asserts the register
//! handshake — the first cross-crate protocol test (proxy ↔ backend) that
//! exercises the actual typed endpoints rather than a unit-level stub.
//!
//! Requires `DATABASE_URL` to point at a test Postgres. CI provides one; locally:
//! `docker compose -f docker-compose.test.yml up -d db` then
//! `DATABASE_URL=postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal cargo test -p backend --test harness`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use backend::config::ServerConfig;
use backend::handlers::device_flow::DeviceFlowStore;
use backend::handlers::images::ImageStore;
use backend::handlers::websocket::SessionManager;
use backend::AppState;
use shared::endpoints::SessionEndpoint;
use shared::{AgentType, ProxyToServer, RegisterFields, ServerToProxy};
use tokio::net::TcpListener;

/// Boot the backend (dev mode) on an ephemeral port; returns its address.
/// Mirrors `backend::run`'s state construction, minus the background tasks.
async fn spawn_test_app() -> SocketAddr {
    let pool = backend::db::create_pool()
        .expect("DATABASE_URL must point to a test Postgres (docker-compose.test.yml)");
    backend::db::run_migrations_logged(&pool).expect("run migrations");
    backend::db::seed_dev_user(&pool).expect("seed dev user");

    let config = ServerConfig::from_env(true).expect("parse server config");
    let oauth = backend::config::build_google_oauth_client(true).expect("oauth client");

    let state = Arc::new(AppState {
        dev_mode: true,
        db_pool: pool,
        session_manager: SessionManager::new(),
        oauth_basic_client: oauth,
        device_flow_store: Some(DeviceFlowStore::default()),
        public_url: config.public_url,
        cookie_key: config.cookie_key,
        jwt_secret: config.jwt_secret,
        app_title: config.app_title,
        splash_text: config.splash_text,
        allowed_email_domain: config.allowed_email_domain,
        allowed_emails: config.allowed_emails,
        message_retention_count: config.message_retention_count,
        message_retention_days: config.message_retention_days,
        session_max_age_days: config.session_max_age_days,
        max_image_mb: config.max_image_mb,
        image_store: ImageStore::new(config.image_store_max_bytes, config.image_store_ttl),
    });

    let app = backend::routes::build_router(state);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind ephemeral port");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .expect("serve");
    });
    addr
}

fn test_register_fields() -> RegisterFields {
    RegisterFields {
        session_id: uuid::Uuid::new_v4(),
        session_name: "harness".to_string(),
        // dev mode resolves the seeded dev user, so no real token is needed.
        auth_token: None,
        working_directory: "/tmp/harness".to_string(),
        resuming: false,
        git_branch: None,
        replay_after: None,
        client_version: Some("harness-test".to_string()),
        replaces_session_id: None,
        hostname: Some("harness".to_string()),
        launcher_id: None,
        agent_type: AgentType::Claude,
        repo_url: None,
        scheduled_task_id: None,
        claude_args: vec![],
    }
}

/// The proxy ↔ backend register handshake: a fresh dev-mode session registers
/// and the backend acknowledges success over the real `/ws/session` endpoint.
#[tokio::test]
async fn proxy_register_returns_ack_success() {
    // Gate on a real DB like the other DB-backed tests, so CI stays green until
    // the Postgres-service + migration step lands (#1209 item 1, slice 1c).
    // Locally: `docker compose -f docker-compose.test.yml up -d db` + DATABASE_URL.
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set, skipping E2E harness test");
        return;
    }

    let addr = spawn_test_app().await;
    let url = format!("ws://{addr}");

    let mut conn = ws_bridge::native_client::connect::<SessionEndpoint>(&url)
        .await
        .expect("connect to /ws/session");

    conn.send(ProxyToServer::Register(test_register_fields()))
        .await
        .expect("send Register");

    let success = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(result) = conn.recv().await {
            if let Ok(ServerToProxy::RegisterAck { success, .. }) = result {
                return Some(success);
            }
        }
        None
    })
    .await
    .expect("RegisterAck within timeout")
    .expect("connection closed before RegisterAck");

    assert!(success, "dev-mode register should succeed");
}
