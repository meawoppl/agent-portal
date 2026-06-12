//! Background maintenance tasks: periodic cleanup loops and the one-shot
//! stale-session sweep that runs after the proxy reconnect grace period.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use crate::db::DbPool;
use crate::handlers;
use crate::handlers::websocket::SessionManager;
use crate::models;
use crate::schema;
use crate::AppState;

/// Spawn a tokio task that runs `f` on a fixed interval forever.
///
/// `name` is the human-readable task description used in the startup log
/// line (`"Started {name}"`).
pub fn spawn_periodic<F, Fut>(name: &str, period: Duration, state: Arc<AppState>, f: F)
where
    F: Fn(Arc<AppState>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send,
{
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(period);
        loop {
            interval.tick().await;
            f(state.clone()).await;
        }
    });
    tracing::info!("Started {}", name);
}

/// Deferred stale session cleanup: wait for proxies to reconnect before
/// marking unreconnected sessions as disconnected. Without this grace
/// period, a backend restart would immediately hide all sessions from the
/// frontend (which only shows status="active") and users would have to
/// restart launchers to get sessions back.
pub fn spawn_stale_session_cleanup(pool: DbPool, manager: SessionManager) {
    tokio::spawn(async move {
        const RECONNECT_GRACE_SECS: u64 = shared::protocol::MAX_RECONNECT_BACKOFF_SECS * 2;
        tracing::info!(
            "Waiting {}s for proxies to reconnect before cleaning stale sessions",
            RECONNECT_GRACE_SECS
        );
        tokio::time::sleep(std::time::Duration::from_secs(RECONNECT_GRACE_SECS)).await;

        let connected_keys: std::collections::HashSet<String> =
            manager.registered_session_keys().into_iter().collect();

        let Ok(mut conn) = pool.get() else {
            tracing::error!("Failed to get DB connection for stale session cleanup");
            return;
        };

        use diesel::prelude::*;
        use schema::sessions;

        let active_sessions: Vec<(uuid::Uuid,)> = match sessions::table
            .filter(sessions::status.eq(shared::SessionStatus::Active.as_str()))
            .select((sessions::id,))
            .load(&mut conn)
        {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to query active sessions for cleanup: {}", e);
                return;
            }
        };

        let stale_ids: Vec<uuid::Uuid> = active_sessions
            .into_iter()
            .map(|(id,)| id)
            .filter(|id| !connected_keys.contains(&id.to_string()))
            .collect();

        if stale_ids.is_empty() {
            tracing::info!("No stale sessions to clean up after reconnect grace period");
            return;
        }

        match diesel::update(sessions::table.filter(sessions::id.eq_any(&stale_ids)))
            .set(sessions::status.eq(shared::SessionStatus::Disconnected.as_str()))
            .execute(&mut conn)
        {
            Ok(updated) => {
                tracing::info!(
                    "Marked {} stale sessions as disconnected ({}s grace period elapsed)",
                    updated,
                    RECONNECT_GRACE_SECS
                );
            }
            Err(e) => {
                tracing::error!("Failed to mark stale sessions as disconnected: {}", e);
            }
        }
    });
}

/// Query user spend from DB and broadcast to all connected web clients
pub async fn broadcast_user_spend_updates(app_state: Arc<AppState>) {
    use diesel::prelude::*;
    use shared::{ServerToClient, SessionCost};

    if app_state.session_manager.user_clients.is_empty() {
        return;
    }

    let connected_user_ids = app_state.session_manager.get_all_user_ids();
    if connected_user_ids.is_empty() {
        return;
    }

    let Ok(mut conn) = app_state.db_pool.get() else {
        tracing::error!("Failed to get DB connection for spend broadcast");
        return;
    };

    // Single query: fetch all sessions with cost > 0 for all connected users
    type CostRow = (uuid::Uuid, uuid::Uuid, f64, i64, i64, i64, i64);
    let all_sessions: Vec<CostRow> = match schema::sessions::table
        .filter(schema::sessions::user_id.eq_any(&connected_user_ids))
        .filter(schema::sessions::total_cost_usd.gt(0.0))
        .select((
            schema::sessions::user_id,
            schema::sessions::id,
            schema::sessions::total_cost_usd,
            schema::sessions::input_tokens,
            schema::sessions::output_tokens,
            schema::sessions::cache_creation_tokens,
            schema::sessions::cache_read_tokens,
        ))
        .load(&mut conn)
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("Failed to query session costs for spend broadcast: {}", e);
            return;
        }
    };

    // Single query: fetch deleted session costs for all connected users
    let deleted_costs: Vec<(uuid::Uuid, f64)> = schema::deleted_session_costs::table
        .filter(schema::deleted_session_costs::user_id.eq_any(&connected_user_ids))
        .filter(schema::deleted_session_costs::cost_usd.gt(0.0))
        .select((
            schema::deleted_session_costs::user_id,
            schema::deleted_session_costs::cost_usd,
        ))
        .load(&mut conn)
        .unwrap_or_default();

    // Build a map of user_id -> deleted cost
    let deleted_cost_map: std::collections::HashMap<uuid::Uuid, f64> =
        deleted_costs.into_iter().collect();

    // Group sessions by user_id
    let mut user_sessions: std::collections::HashMap<uuid::Uuid, Vec<SessionCost>> =
        std::collections::HashMap::new();
    let mut user_active_cost: std::collections::HashMap<uuid::Uuid, f64> =
        std::collections::HashMap::new();

    for (uid, sid, cost, inp, outp, cache_create, cache_read) in all_sessions {
        *user_active_cost.entry(uid).or_default() += cost;
        user_sessions.entry(uid).or_default().push(SessionCost {
            session_id: sid,
            total_cost_usd: cost,
            input_tokens: inp,
            output_tokens: outp,
            cache_creation_tokens: cache_create,
            cache_read_tokens: cache_read,
        });
    }

    // Broadcast to each connected user
    for uid in &connected_user_ids {
        let active_cost = user_active_cost.get(uid).copied().unwrap_or(0.0);
        let deleted_cost = deleted_cost_map.get(uid).copied().unwrap_or(0.0);
        let total_spend = active_cost + deleted_cost;
        let session_costs = user_sessions.remove(uid).unwrap_or_default();

        if total_spend > 0.0 || !session_costs.is_empty() {
            app_state.session_manager.broadcast_to_user(
                uid,
                ServerToClient::UserSpendUpdate {
                    total_spend_usd: total_spend,
                    session_costs,
                },
            );
        }
    }
}

/// Purge expired device flow codes from the in-memory store
pub async fn purge_expired_device_codes(app_state: Arc<AppState>) {
    let Some(store) = &app_state.device_flow_store else {
        return;
    };
    let mut map = store.write().await;
    let before = map.len();
    map.retain(|_, state| state.expires_at > std::time::SystemTime::now());
    let removed = before - map.len();
    if removed > 0 {
        tracing::debug!("Purged {} expired device flow codes", removed);
    }
}

/// Run retention cleanup: delete old messages and truncate per-session counts
pub async fn run_retention_cleanup(app_state: Arc<AppState>) {
    use handlers::retention::{run_retention_cleanup, RetentionConfig};

    let session_ids = app_state.session_manager.drain_pending_truncations();

    let Ok(mut conn) = app_state.db_pool.get() else {
        tracing::error!("Failed to get DB connection for retention cleanup");
        return;
    };

    let config = RetentionConfig::new(
        app_state.message_retention_count,
        app_state.message_retention_days,
    );

    let (age_deleted, count_deleted) = run_retention_cleanup(&mut conn, session_ids, config);

    if age_deleted > 0 || count_deleted > 0 {
        tracing::info!(
            "Retention cleanup complete: {} old, {} over-limit",
            age_deleted,
            count_deleted
        );
    }
}

/// Delete sessions whose last_activity is older than SESSION_MAX_AGE_DAYS
pub async fn run_session_age_cleanup(app_state: Arc<AppState>) {
    use diesel::prelude::*;
    use handlers::helpers::delete_session_with_data;

    let max_days = app_state.session_max_age_days;
    if max_days == 0 {
        return;
    }

    let Ok(mut conn) = app_state.db_pool.get() else {
        tracing::error!("Failed to get DB connection for session age cleanup");
        return;
    };

    // Set a 5-second timeout for cleanup queries
    if let Err(e) = diesel::sql_query("SET LOCAL statement_timeout = '5000'").execute(&mut conn) {
        tracing::warn!(
            "Failed to set statement_timeout for session age cleanup: {}",
            e
        );
    }

    let cutoff = chrono::Utc::now().naive_utc() - chrono::Duration::days(i64::from(max_days));

    let old_sessions: Vec<models::Session> = match schema::sessions::table
        .filter(schema::sessions::last_activity.lt(cutoff))
        .load(&mut conn)
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("Failed to query old sessions: {}", e);
            return;
        }
    };

    if old_sessions.is_empty() {
        return;
    }

    let mut deleted = 0;
    for session in &old_sessions {
        match delete_session_with_data(&mut conn, session, true) {
            Ok(_) => deleted += 1,
            Err(e) => tracing::error!("Failed to delete old session {}: {:?}", session.id, e),
        }
    }

    tracing::info!(
        "Session age cleanup: deleted {} sessions older than {} days",
        deleted,
        max_days
    );
}

/// Delete proxy auth tokens whose expiration is more than 7 days in the past.
pub async fn run_expired_token_cleanup(app_state: Arc<AppState>) {
    use diesel::prelude::*;

    let Ok(mut conn) = app_state.db_pool.get() else {
        tracing::error!("Failed to get DB connection for expired token cleanup");
        return;
    };

    if let Err(e) = diesel::sql_query("SET LOCAL statement_timeout = '5000'").execute(&mut conn) {
        tracing::warn!(
            "Failed to set statement_timeout for expired token cleanup: {}",
            e
        );
    }

    let token_cutoff = chrono::Utc::now().naive_utc() - chrono::Duration::days(7);
    match diesel::delete(
        schema::proxy_auth_tokens::table
            .filter(schema::proxy_auth_tokens::expires_at.lt(token_cutoff)),
    )
    .execute(&mut conn)
    {
        Ok(0) => {}
        Ok(count) => {
            tracing::info!("Expired token cleanup: deleted {} tokens", count);
        }
        Err(e) => {
            tracing::error!("Failed to delete expired tokens: {}", e);
        }
    }
}
