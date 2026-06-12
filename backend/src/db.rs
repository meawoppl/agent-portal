use anyhow::Result;
use bigdecimal::ToPrimitive;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::r2d2::{self, ConnectionManager, Pool, PooledConnection};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use std::env;
use uuid::Uuid;

use crate::schema;

pub type DbPool = Pool<ConnectionManager<PgConnection>>;
pub type DbConnection = PooledConnection<ConnectionManager<PgConnection>>;

/// Embedded database migrations - compiled into the binary
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

pub fn create_pool() -> Result<DbPool> {
    let database_url = env::var("DATABASE_URL").expect("DATABASE_URL must be set");

    let manager = ConnectionManager::<PgConnection>::new(database_url);
    let pool = r2d2::Pool::builder()
        .build(manager)
        .expect("Failed to create pool");

    Ok(pool)
}

/// Run pending database migrations
/// Returns the list of migrations that were applied
pub fn run_migrations(pool: &DbPool) -> Result<Vec<String>> {
    let mut conn = pool.get()?;

    let applied: Vec<String> = conn
        .run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?
        .iter()
        .map(|m| m.to_string())
        .collect();

    Ok(applied)
}

/// Run pending database migrations, logging each applied migration.
pub fn run_migrations_logged(pool: &DbPool) -> Result<()> {
    tracing::info!("Running database migrations...");
    match run_migrations(pool) {
        Ok(applied) => {
            if applied.is_empty() {
                tracing::info!("Database is up to date (no pending migrations)");
            } else {
                for migration in &applied {
                    tracing::info!("Applied migration: {}", migration);
                }
                tracing::info!("Successfully applied {} migration(s)", applied.len());
            }
            Ok(())
        }
        Err(e) => {
            tracing::error!("Failed to run database migrations: {}", e);
            Err(e)
        }
    }
}

/// Create the dev-mode test user if it does not already exist.
pub fn seed_dev_user(pool: &DbPool) -> Result<()> {
    use crate::models::{self, NewUser};
    use crate::schema::users::dsl::*;

    let mut conn = pool.get()?;
    let test_user = users
        .filter(email.eq("testing@testing.local"))
        .first::<models::User>(&mut conn)
        .optional()?;

    if test_user.is_none() {
        let new_user = NewUser {
            google_id: "dev_mode_test_user".to_string(),
            email: "testing@testing.local".to_string(),
            name: Some("Test User".to_string()),
            avatar_url: None,
        };

        diesel::insert_into(users)
            .values(&new_user)
            .execute(&mut conn)?;

        tracing::info!("✓ Created test user: testing@testing.local");
    }

    Ok(())
}

/// Aggregated usage data for a user (includes both active and deleted sessions)
#[derive(Debug, Default, Clone)]
pub struct UserUsage {
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
}

/// Fetch aggregated usage for a specific user (active sessions + deleted session costs)
pub fn get_user_usage(
    conn: &mut diesel::PgConnection,
    user_id: Uuid,
) -> std::result::Result<UserUsage, diesel::result::Error> {
    // Get cost and tokens from active sessions
    let active_cost: f64 = schema::sessions::table
        .filter(schema::sessions::user_id.eq(user_id))
        .select(diesel::dsl::sum(schema::sessions::total_cost_usd))
        .first::<Option<f64>>(conn)?
        .unwrap_or(0.0);

    let active_input: i64 = schema::sessions::table
        .filter(schema::sessions::user_id.eq(user_id))
        .select(diesel::dsl::sum(schema::sessions::input_tokens))
        .first::<Option<bigdecimal::BigDecimal>>(conn)
        .ok()
        .flatten()
        .and_then(|d| d.to_i64())
        .unwrap_or(0);

    let active_output: i64 = schema::sessions::table
        .filter(schema::sessions::user_id.eq(user_id))
        .select(diesel::dsl::sum(schema::sessions::output_tokens))
        .first::<Option<bigdecimal::BigDecimal>>(conn)
        .ok()
        .flatten()
        .and_then(|d| d.to_i64())
        .unwrap_or(0);

    let active_cache_creation: i64 = schema::sessions::table
        .filter(schema::sessions::user_id.eq(user_id))
        .select(diesel::dsl::sum(schema::sessions::cache_creation_tokens))
        .first::<Option<bigdecimal::BigDecimal>>(conn)
        .ok()
        .flatten()
        .and_then(|d| d.to_i64())
        .unwrap_or(0);

    let active_cache_read: i64 = schema::sessions::table
        .filter(schema::sessions::user_id.eq(user_id))
        .select(diesel::dsl::sum(schema::sessions::cache_read_tokens))
        .first::<Option<bigdecimal::BigDecimal>>(conn)
        .ok()
        .flatten()
        .and_then(|d| d.to_i64())
        .unwrap_or(0);

    // Get usage from deleted sessions for this user (single row per user)
    let (deleted_cost, deleted_input, deleted_output, deleted_cache_creation, deleted_cache_read): (
        f64,
        i64,
        i64,
        i64,
        i64,
    ) = schema::deleted_session_costs::table
        .filter(schema::deleted_session_costs::user_id.eq(user_id))
        .select((
            schema::deleted_session_costs::cost_usd,
            schema::deleted_session_costs::input_tokens,
            schema::deleted_session_costs::output_tokens,
            schema::deleted_session_costs::cache_creation_tokens,
            schema::deleted_session_costs::cache_read_tokens,
        ))
        .first(conn)
        .unwrap_or((0.0, 0, 0, 0, 0));

    Ok(UserUsage {
        cost_usd: active_cost + deleted_cost,
        input_tokens: active_input + deleted_input,
        output_tokens: active_output + deleted_output,
        cache_creation_tokens: active_cache_creation + deleted_cache_creation,
        cache_read_tokens: active_cache_read + deleted_cache_read,
    })
}
