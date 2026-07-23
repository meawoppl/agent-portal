//! Test-only DB support: one process-wide connection pool shared by every
//! DB-gated unit test.
//!
//! WHY a shared pool: each `#[cfg(test)]` module used to build its own r2d2
//! pool per test (via `create_pool()` / `Pool::builder()`). Under the full
//! parallel `cargo test -p backend` run, dozens of those pools exist at once
//! and each opens several Postgres connections, so the suite blew past
//! Postgres `max_connections` (~100 in `docker-compose.test.yml`). DB-gated
//! tests then intermittently panicked while checking out a connection (they
//! passed in isolation / with `--test-threads=1`). A single, small, shared
//! pool built once keeps total connections bounded regardless of test
//! parallelism.

use std::sync::OnceLock;
use std::time::Duration;

use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, Pool};
use tower_cookies::Key;

use crate::config::MobileAppLinksConfig;
use crate::db::DbPool;
use crate::handlers::images::ImageStore;
use crate::handlers::websocket::SessionManager;
use crate::AppState;

/// Build the canonical test [`AppState`] wired to `pool`.
///
/// WHY this exists: every DB-gated test module used to hand-write the full
/// ~20-field `AppState { .. }` literal (unit tests in `auth.rs` /
/// `handlers/auth.rs`, plus the integration harness in `tests/harness.rs`).
/// That made **every new `AppState` field a change to every one of those
/// literals** — during the push work three fields landed in a single day
/// (`notifications`, `vapid_public_key`, and the VAPID transport rework), and
/// each meant editing four identical literals plus eating the resulting rebase
/// conflicts across parallel PRs. Centralizing construction here means a new
/// field is one default in one place; call sites that don't care never change.
///
/// The defaults are the canonical *unit-test* configuration (dev mode off, no
/// OAuth, a tiny in-memory image store, every optional `None`). Call sites that
/// need something different mutate the returned struct before `Arc`-wrapping —
/// e.g. `let mut s = test_app_state(pool); s.dev_mode = true;`. That direct-
/// mutation override is deliberate: it needs no builder scaffolding and, unlike
/// a literal, names only the fields a given test actually cares about.
pub fn test_app_state(pool: DbPool) -> AppState {
    AppState {
        dev_mode: false,
        db_pool: pool,
        session_manager: SessionManager::new(),
        oauth_basic_client: None,
        device_flow_store: None,
        public_url: "http://localhost:3000".to_string(),
        cookie_key: Key::generate(),
        jwt_secret: "test-secret-key-at-least-32-bytes".to_string(),
        app_title: "Agent Portal Test".to_string(),
        splash_text: None,
        allowed_email_domain: None,
        allowed_emails: None,
        message_retention_count: 100,
        message_retention_days: 30,
        session_max_age_days: 14,
        max_image_mb: 10,
        image_store: ImageStore::new(1024 * 1024, Duration::from_secs(60)),
        max_video_mb: 100,
        media_store: crate::handlers::media_store::MediaStore::new(
            std::env::temp_dir().join(format!("agent-portal-media-test-{}", uuid::Uuid::new_v4())),
            1024 * 1024,
            Duration::from_secs(60),
        )
        .expect("create test media store"),
        forward_domain: None,
        archive: None,
        notifications: crate::push::channel().0,
        vapid_public_key: None,
        mobile_app_links: MobileAppLinksConfig::default(),
    }
}

/// Max connections the shared test pool may open.
///
/// Sized to sit in the sweet spot between two failure modes:
/// - **Too many** (the old per-test pools): dozens of pools × ~10 conns each
///   blew past Postgres `max_connections` (100 in `docker-compose.test.yml`).
/// - **Too few**: a single tiny pool starves the highly parallel suite. The
///   default `cargo test` parallelism is the CPU count (80 on CI-class hosts),
///   and several DB-gated tests hold one connection for the whole test body
///   while the handler under test checks out a *second* one — so each such
///   test needs 2 connections at once. Undersizing the pool deadlocks those
///   tests until r2d2's 30s checkout timeout fires, then they panic.
///
/// 48 comfortably covers every DB-gated test running its 2-connection pattern
/// concurrently while staying well under Postgres `max_connections`.
const TEST_POOL_MAX_SIZE: u32 = 48;

/// Returns a clone of the process-wide test pool, or `None` when `DATABASE_URL`
/// is unset (DB-gated tests skip in that case, keeping CI green without a DB).
///
/// The pool is built exactly once via [`OnceLock`] and migrations run once
/// against it; `OnceLock::get_or_init` serializes that init, subsuming the old
/// per-module `DB_SETUP_LOCK` migrate guard. `DbPool` is an r2d2 handle whose
/// clones share the underlying connection pool, so every caller draws from the
/// same bounded set of connections — see the module docs for why that matters.
pub fn shared_pool() -> Option<DbPool> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set, skipping DB-backed test");
        return None;
    }

    static POOL: OnceLock<DbPool> = OnceLock::new();
    let pool = POOL.get_or_init(|| {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL set");
        let manager = ConnectionManager::<PgConnection>::new(database_url);
        let pool = Pool::builder()
            .max_size(TEST_POOL_MAX_SIZE)
            .build(manager)
            .expect("build shared test DB pool");
        crate::db::run_migrations_logged(&pool).expect("run migrations for shared test pool");
        pool
    });
    Some(pool.clone())
}
