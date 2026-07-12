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

/// One process-wide pool for the whole harness binary.
///
/// WHY: harness tests run concurrently against ONE shared Postgres. Building a
/// fresh pool per test multiplies open connections and, together with the
/// other DB-gated test binaries, can exhaust Postgres `max_connections` (~100
/// in `docker-compose.test.yml`) — the parallel-suite flake this change fixes.
/// The pool is built (and migrated+seeded) exactly once via `OnceLock`;
/// `get_or_init` serializes that setup (Diesel's migration runner is not safe
/// to race with itself), and `DbPool` clones share the underlying pool.
///
/// This mirrors `backend::test_support::shared_pool`, duplicated here because
/// that module is `#[cfg(test)]`-gated in the lib and so isn't visible to this
/// separate integration-test binary.
fn test_pool() -> backend::db::DbPool {
    static POOL: std::sync::OnceLock<backend::db::DbPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let pool = backend::db::create_pool()
            .expect("DATABASE_URL must point to a test Postgres (docker-compose.test.yml)");
        backend::db::run_migrations_logged(&pool).expect("run migrations");
        backend::db::seed_dev_user(&pool).expect("seed dev user");
        pool
    })
    .clone()
}

/// Boot the backend (dev mode) on an ephemeral port; returns its address.
/// Mirrors `backend::run`'s state construction, minus the background tasks.
async fn spawn_test_app() -> SocketAddr {
    let pool = test_pool();

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
        forward_domain: config.forward_domain,
        archive: None,
        // No dispatcher is spawned in the harness; the receiver drops
        // immediately, so emits are silently no-ops (see NotificationSender).
        notifications: backend::push::channel().0,
        vapid_public_key: config.vapid_public_key,
        mobile_app_links: config.mobile_app_links,
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

/// The archive tests each run a global sweep against the SHARED test
/// Postgres — two sweeps racing can re-mark each other's sessions and
/// break idempotency assertions. Serialize them.
static ARCHIVE_DB_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// #1258 phase 1: the archive sweep persists a manifest + compressed
/// transcript for a terminal session, marks it archived (idempotent — a
/// second sweep is a no-op), and re-archives after new activity.
#[tokio::test]
async fn archive_sweep_persists_and_is_idempotent() {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    }
    let _guard = ARCHIVE_DB_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    use backend::archive::{read_transcript, ArchiveBackendConfig, ArchiveConfig, ArchiveRuntime};
    use backend::models::{NewMessage, NewSessionWithId};
    use backend::schema::{messages, sessions};
    use diesel::prelude::*;

    let pool = test_pool();
    let mut conn = pool.get().expect("conn");

    let user_id: uuid::Uuid = backend::schema::users::table
        .select(backend::schema::users::id)
        .order(backend::schema::users::created_at.asc())
        .first(&mut conn)
        .expect("seeded user");

    // A terminal session, idle for two hours.
    let session_id = uuid::Uuid::new_v4();
    let stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(2);
    diesel::insert_into(sessions::table)
        .values(&NewSessionWithId {
            id: session_id,
            user_id,
            session_name: "archive-harness".to_string(),
            session_key: session_id.to_string(),
            working_directory: "/tmp/archive-harness".to_string(),
            status: shared::SessionStatus::Disconnected.as_str().to_string(),
            git_branch: Some("main".to_string()),
            client_version: None,
            hostname: "harness".to_string(),
            launcher_id: None,
            agent_type: "claude".to_string(),
            repo_url: None,
            scheduled_task_id: None,
            paused: false,
            claude_args: serde_json::Value::Array(vec![]),
        })
        .execute(&mut conn)
        .expect("insert session");
    diesel::update(sessions::table.find(session_id))
        .set((
            sessions::last_activity.eq(stale),
            sessions::created_at.eq(stale),
        ))
        .execute(&mut conn)
        .expect("backdate session");

    for (role, content) in [
        ("user", r#"{"type":"user","text":"hello"}"#),
        ("assistant", r#"{"type":"assistant","text":"hi"}"#),
    ] {
        diesel::insert_into(messages::table)
            .values(&NewMessage {
                session_id,
                role: role.to_string(),
                content: content.to_string(),
                user_id,
                agent_type: "claude".to_string(),
                provenance_kind: None,
                provenance_session_id: None,
                provenance_agent_type: None,
            })
            .execute(&mut conn)
            .expect("insert message");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let runtime = ArchiveRuntime::new(ArchiveConfig {
        backend: ArchiveBackendConfig::Local {
            root: tmp.path().to_path_buf(),
        },
        transcripts: true,
    })
    .expect("local archive runtime");

    let (archived, failed) =
        backend::background::archive_pending_sessions(&pool, &runtime).expect("sweep");
    assert!(failed == 0, "no failures expected");
    assert!(archived >= 1, "our session must be archived");

    let manifest = runtime
        .store
        .get_session_manifest(user_id, session_id)
        .expect("read manifest")
        .expect("manifest exists");
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.session_name, "archive-harness");
    assert_eq!(manifest.message_counts.get("user"), Some(&1));
    assert_eq!(manifest.message_counts.get("assistant"), Some(&1));
    let transcript = manifest.transcript.as_ref().expect("transcript info");
    assert_eq!(transcript.message_count, 2);

    let lines = read_transcript(tmp.path(), user_id, session_id).expect("read transcript");
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].content["text"], "hello");

    // Idempotent: our session must not be picked up again unchanged.
    // (Other stale sessions in a shared dev DB may legitimately archive.)
    let archived_at: Option<chrono::NaiveDateTime> = sessions::table
        .find(session_id)
        .select(sessions::archived_at)
        .first(&mut conn)
        .expect("read archived_at");
    let first_mark = archived_at.expect("archived_at set");
    backend::background::archive_pending_sessions(&pool, &runtime).expect("second sweep");
    let second_mark: Option<chrono::NaiveDateTime> = sessions::table
        .find(session_id)
        .select(sessions::archived_at)
        .first(&mut conn)
        .expect("re-read archived_at");
    assert_eq!(
        second_mark,
        Some(first_mark),
        "unchanged session not re-archived"
    );

    // New activity past archived_at → eligible again.
    diesel::update(sessions::table.find(session_id))
        .set(sessions::last_activity.eq(chrono::Utc::now().naive_utc()
            - chrono::Duration::seconds(backend::archive::ARCHIVE_IDLE_SECS + 60)))
        .execute(&mut conn)
        .expect("bump activity");
    // Guard: only when the bumped activity is later than the mark.
    diesel::update(sessions::table.find(session_id))
        .set(sessions::archived_at.eq(stale))
        .execute(&mut conn)
        .expect("backdate mark");
    let (rearchived, _) =
        backend::background::archive_pending_sessions(&pool, &runtime).expect("third sweep");
    assert!(rearchived >= 1, "reactivated session re-archives");

    // Cleanup.
    diesel::delete(messages::table.filter(messages::session_id.eq(session_id)))
        .execute(&mut conn)
        .ok();
    diesel::delete(sessions::table.find(session_id))
        .execute(&mut conn)
        .ok();
}

/// #1258 phase 2: a re-archive after hot-DB messages were trimmed must
/// MERGE with the archived transcript, never shrink it.
#[tokio::test]
async fn rearchive_after_trim_merges_transcript() {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("skipping: DATABASE_URL not set");
        return;
    }
    let _guard = ARCHIVE_DB_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    use backend::archive::{read_transcript, ArchiveBackendConfig, ArchiveConfig, ArchiveRuntime};
    use backend::models::{NewMessage, NewSessionWithId};
    use backend::schema::{messages, sessions};
    use diesel::prelude::*;

    let pool = test_pool();
    let mut conn = pool.get().expect("conn");
    let user_id: uuid::Uuid = backend::schema::users::table
        .select(backend::schema::users::id)
        .order(backend::schema::users::created_at.asc())
        .first(&mut conn)
        .expect("seeded user");

    let session_id = uuid::Uuid::new_v4();
    let stale = chrono::Utc::now().naive_utc() - chrono::Duration::hours(2);
    diesel::insert_into(sessions::table)
        .values(&NewSessionWithId {
            id: session_id,
            user_id,
            session_name: "merge-harness".to_string(),
            session_key: session_id.to_string(),
            working_directory: "/tmp/merge-harness".to_string(),
            status: shared::SessionStatus::Disconnected.as_str().to_string(),
            git_branch: None,
            client_version: None,
            hostname: "harness".to_string(),
            launcher_id: None,
            agent_type: "claude".to_string(),
            repo_url: None,
            scheduled_task_id: None,
            paused: false,
            claude_args: serde_json::Value::Array(vec![]),
        })
        .execute(&mut conn)
        .expect("insert session");
    diesel::update(sessions::table.find(session_id))
        .set(sessions::last_activity.eq(stale))
        .execute(&mut conn)
        .expect("backdate");

    let insert_msg = |conn: &mut diesel::PgConnection, text: &str| {
        diesel::insert_into(messages::table)
            .values(&NewMessage {
                session_id,
                role: "user".to_string(),
                content: format!(r#"{{"text":"{text}"}}"#),
                user_id,
                agent_type: "claude".to_string(),
                provenance_kind: None,
                provenance_session_id: None,
                provenance_agent_type: None,
            })
            .execute(conn)
            .expect("insert message");
    };
    insert_msg(&mut conn, "first");
    insert_msg(&mut conn, "second");

    let tmp = tempfile::tempdir().expect("tempdir");
    let runtime = ArchiveRuntime::new(ArchiveConfig {
        backend: ArchiveBackendConfig::Local {
            root: tmp.path().to_path_buf(),
        },
        transcripts: true,
    })
    .expect("local archive runtime");

    // First archive captures both messages.
    backend::background::archive_pending_sessions(&pool, &runtime).expect("first sweep");
    assert_eq!(
        read_transcript(tmp.path(), user_id, session_id)
            .expect("read")
            .len(),
        2
    );

    // Retention trims one hot row; a third message arrives; re-archive.
    diesel::delete(
        messages::table
            .filter(messages::session_id.eq(session_id))
            .filter(messages::content.like("%first%")),
    )
    .execute(&mut conn)
    .expect("trim");
    insert_msg(&mut conn, "third");
    diesel::update(sessions::table.find(session_id))
        .set((
            sessions::last_activity.eq(chrono::Utc::now().naive_utc()
                - chrono::Duration::seconds(backend::archive::ARCHIVE_IDLE_SECS + 60)),
            sessions::archived_at.eq(stale),
        ))
        .execute(&mut conn)
        .expect("make stale again");
    backend::background::archive_pending_sessions(&pool, &runtime).expect("second sweep");

    let lines = read_transcript(tmp.path(), user_id, session_id).expect("read merged");
    let texts: Vec<String> = lines
        .iter()
        .map(|l| l.content["text"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        lines.len(),
        3,
        "merge must keep the trimmed message: {texts:?}"
    );
    assert!(
        texts.contains(&"first".to_string()),
        "trimmed message survives"
    );
    assert!(texts.contains(&"third".to_string()), "new message included");

    let manifest = runtime
        .store
        .get_session_manifest(user_id, session_id)
        .expect("manifest read")
        .expect("manifest");
    assert_eq!(manifest.message_counts.get("user"), Some(&3));

    diesel::delete(messages::table.filter(messages::session_id.eq(session_id)))
        .execute(&mut conn)
        .ok();
    diesel::delete(sessions::table.find(session_id))
        .execute(&mut conn)
        .ok();
}
