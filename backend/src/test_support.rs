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

use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, Pool};

use crate::db::DbPool;

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
