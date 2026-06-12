use anyhow::Result;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::r2d2::{self, ConnectionManager, Pool, PooledConnection};
use diesel::sql_types::{BigInt, Double};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use std::collections::HashMap;
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

/// Aggregated usage data for a user (includes both active and deleted sessions)
#[derive(Debug, Default, Clone)]
pub struct UserUsage {
    pub session_count: i64,
    pub cost_usd: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
}

/// Per-user aggregates over the sessions table from a single grouped query.
#[derive(QueryableByName)]
struct UserSessionAgg {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    user_id: Uuid,
    #[diesel(sql_type = BigInt)]
    session_count: i64,
    #[diesel(sql_type = Double)]
    cost_usd: f64,
    #[diesel(sql_type = BigInt)]
    input_tokens: i64,
    #[diesel(sql_type = BigInt)]
    output_tokens: i64,
    #[diesel(sql_type = BigInt)]
    cache_creation_tokens: i64,
    #[diesel(sql_type = BigInt)]
    cache_read_tokens: i64,
}

/// Fetch aggregated usage for all users (active sessions + deleted session costs)
/// in two queries: one grouped over sessions, one over deleted_session_costs.
pub fn get_all_user_usage(
    conn: &mut diesel::PgConnection,
) -> std::result::Result<HashMap<Uuid, UserUsage>, diesel::result::Error> {
    let mut usage_by_user: HashMap<Uuid, UserUsage> = HashMap::new();

    // Cost, tokens, and session counts from active sessions, grouped per user
    let session_aggs: Vec<UserSessionAgg> = diesel::sql_query(
        "SELECT user_id, \
         COUNT(*) as session_count, \
         COALESCE(SUM(total_cost_usd), 0.0)::float8 as cost_usd, \
         COALESCE(SUM(input_tokens), 0)::bigint as input_tokens, \
         COALESCE(SUM(output_tokens), 0)::bigint as output_tokens, \
         COALESCE(SUM(cache_creation_tokens), 0)::bigint as cache_creation_tokens, \
         COALESCE(SUM(cache_read_tokens), 0)::bigint as cache_read_tokens \
         FROM sessions GROUP BY user_id",
    )
    .load(conn)?;

    for agg in session_aggs {
        usage_by_user.insert(
            agg.user_id,
            UserUsage {
                session_count: agg.session_count,
                cost_usd: agg.cost_usd,
                input_tokens: agg.input_tokens,
                output_tokens: agg.output_tokens,
                cache_creation_tokens: agg.cache_creation_tokens,
                cache_read_tokens: agg.cache_read_tokens,
            },
        );
    }

    // Usage from deleted sessions (single row per user)
    let deleted_rows: Vec<(Uuid, f64, i64, i64, i64, i64)> = schema::deleted_session_costs::table
        .select((
            schema::deleted_session_costs::user_id,
            schema::deleted_session_costs::cost_usd,
            schema::deleted_session_costs::input_tokens,
            schema::deleted_session_costs::output_tokens,
            schema::deleted_session_costs::cache_creation_tokens,
            schema::deleted_session_costs::cache_read_tokens,
        ))
        .load(conn)?;

    for (user_id, cost_usd, input, output, cache_creation, cache_read) in deleted_rows {
        let entry = usage_by_user.entry(user_id).or_default();
        entry.cost_usd += cost_usd;
        entry.input_tokens += input;
        entry.output_tokens += output;
        entry.cache_creation_tokens += cache_creation;
        entry.cache_read_tokens += cache_read;
    }

    Ok(usage_by_user)
}
