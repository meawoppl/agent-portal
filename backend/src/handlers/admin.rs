//! Admin dashboard API handlers
//!
//! These endpoints are restricted to users with is_admin=true.

use axum::{
    extract::{Path, State},
    Json,
};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Double};
use shared::api::{
    AdminSessionInfo, AdminSessionsResponse, AdminStats, AdminUserEntry, AdminUsersResponse,
    UpdateUserRequest,
};
use std::sync::Arc;
use tower_cookies::Cookies;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    db::get_user_usage, errors::AppError, handlers::responses::EmptyResponse, models::User, schema,
    AppState,
};

use shared::protocol::SESSION_COOKIE_NAME;

// ============================================================================
// Admin Guard - extracts and validates admin user from cookies
// ============================================================================

/// Extract the current user from cookies and verify they are an admin.
/// Returns the User if they are an admin, or an appropriate application error.
pub async fn require_admin(app_state: &Arc<AppState>, cookies: &Cookies) -> Result<User, AppError> {
    // Extract user ID from signed session cookie
    let cookie = cookies
        .signed(&app_state.cookie_key)
        .get(SESSION_COOKIE_NAME)
        .ok_or(AppError::Unauthorized)?;

    let user_id: Uuid = cookie.value().parse().map_err(|_| AppError::Unauthorized)?;

    // Fetch user from database
    let mut conn = app_state.db_pool.get()?;

    let user = schema::users::table
        .find(user_id)
        .first::<User>(&mut conn)
        .map_err(|_| AppError::NotFound("admin user"))?;

    // Check if user is disabled
    if user.disabled {
        warn!("Disabled user {} attempted admin access", user.email);
        return Err(AppError::Forbidden);
    }

    // Check if user is admin
    if !user.is_admin {
        warn!("Non-admin user {} attempted admin access", user.email);
        return Err(AppError::Forbidden);
    }

    Ok(user)
}

// ============================================================================
// Stats Endpoint - System overview statistics
// ============================================================================

/// Aggregated user counts from a single query.
#[derive(QueryableByName)]
struct UserStats {
    #[diesel(sql_type = BigInt)]
    total: i64,
    #[diesel(sql_type = BigInt)]
    admin_count: i64,
    #[diesel(sql_type = BigInt)]
    disabled_count: i64,
}

/// Aggregated session counts and cost/token sums from a single query.
#[derive(QueryableByName)]
struct SessionStats {
    #[diesel(sql_type = BigInt)]
    total: i64,
    #[diesel(sql_type = BigInt)]
    active_count: i64,
    #[diesel(sql_type = Double)]
    spend_usd: f64,
    #[diesel(sql_type = BigInt)]
    sum_input_tokens: i64,
    #[diesel(sql_type = BigInt)]
    sum_output_tokens: i64,
    #[diesel(sql_type = BigInt)]
    sum_cache_creation_tokens: i64,
    #[diesel(sql_type = BigInt)]
    sum_cache_read_tokens: i64,
}

/// Aggregated deleted-session cost/token sums from a single query.
#[derive(QueryableByName)]
struct DeletedCostStats {
    #[diesel(sql_type = Double)]
    spend_usd: f64,
    #[diesel(sql_type = BigInt)]
    sum_input_tokens: i64,
    #[diesel(sql_type = BigInt)]
    sum_output_tokens: i64,
    #[diesel(sql_type = BigInt)]
    sum_cache_creation_tokens: i64,
    #[diesel(sql_type = BigInt)]
    sum_cache_read_tokens: i64,
}

pub async fn get_stats(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<AdminStats>, AppError> {
    let admin = require_admin(&app_state, &cookies).await?;
    info!("Admin {} requested system stats", admin.email);

    let mut conn = app_state.db_pool.get()?;

    // Query 1: All user counts in one pass
    let user_stats: UserStats = diesel::sql_query(
        "SELECT COUNT(*) as total, \
         COUNT(*) FILTER (WHERE is_admin) as admin_count, \
         COUNT(*) FILTER (WHERE disabled) as disabled_count \
         FROM users",
    )
    .get_result(&mut conn)
    .map_err(|e| admin_db_query("Failed to query user stats", e))?;

    // Query 2: All session counts + cost/token sums in one pass
    let session_stats: SessionStats = diesel::sql_query(
        "SELECT COUNT(*) as total, \
         COUNT(*) FILTER (WHERE status = $1) as active_count, \
         COALESCE(SUM(total_cost_usd), 0.0)::float8 as spend_usd, \
         COALESCE(SUM(input_tokens), 0)::bigint as sum_input_tokens, \
         COALESCE(SUM(output_tokens), 0)::bigint as sum_output_tokens, \
         COALESCE(SUM(cache_creation_tokens), 0)::bigint as sum_cache_creation_tokens, \
         COALESCE(SUM(cache_read_tokens), 0)::bigint as sum_cache_read_tokens \
         FROM sessions",
    )
    .bind::<diesel::sql_types::Text, _>(shared::SessionStatus::Active.as_str())
    .get_result(&mut conn)
    .map_err(|e| admin_db_query("Failed to query session stats", e))?;

    // Query 3: Deleted session cost/token sums in one pass
    let deleted_stats: DeletedCostStats = diesel::sql_query(
        "SELECT \
         COALESCE(SUM(cost_usd), 0.0)::float8 as spend_usd, \
         COALESCE(SUM(input_tokens), 0)::bigint as sum_input_tokens, \
         COALESCE(SUM(output_tokens), 0)::bigint as sum_output_tokens, \
         COALESCE(SUM(cache_creation_tokens), 0)::bigint as sum_cache_creation_tokens, \
         COALESCE(SUM(cache_read_tokens), 0)::bigint as sum_cache_read_tokens \
         FROM deleted_session_costs",
    )
    .get_result(&mut conn)
    .map_err(|e| admin_db_query("Failed to query deleted session stats", e))?;

    // Get connected client counts from session manager (no DB query needed)
    let connected_proxy_clients = app_state.session_manager.sessions.len();
    let connected_web_clients: usize = app_state
        .session_manager
        .user_clients
        .iter()
        .map(|r| r.value().len())
        .sum();

    Ok(Json(AdminStats {
        total_users: user_stats.total,
        admin_users: user_stats.admin_count,
        disabled_users: user_stats.disabled_count,
        total_sessions: session_stats.total,
        active_sessions: session_stats.active_count,
        connected_proxy_clients,
        connected_web_clients,
        total_spend_usd: session_stats.spend_usd + deleted_stats.spend_usd,
        total_input_tokens: session_stats.sum_input_tokens + deleted_stats.sum_input_tokens,
        total_output_tokens: session_stats.sum_output_tokens + deleted_stats.sum_output_tokens,
        total_cache_creation_tokens: session_stats.sum_cache_creation_tokens
            + deleted_stats.sum_cache_creation_tokens,
        total_cache_read_tokens: session_stats.sum_cache_read_tokens
            + deleted_stats.sum_cache_read_tokens,
    }))
}

// ============================================================================
// Users Endpoint - List and manage users
// ============================================================================

pub async fn list_users(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<AdminUsersResponse>, AppError> {
    let admin = require_admin(&app_state, &cookies).await?;
    info!("Admin {} requested user list", admin.email);

    let mut conn = app_state.db_pool.get()?;

    // Get all users
    let users: Vec<User> = schema::users::table
        .order(schema::users::created_at.desc())
        .load(&mut conn)
        .map_err(|e| admin_db_query("Failed to load users", e))?;

    // Get session counts and usage per user
    let mut user_infos = Vec::with_capacity(users.len());
    for user in users {
        // Get session count
        let session_count: i64 = schema::sessions::table
            .filter(schema::sessions::user_id.eq(user.id))
            .count()
            .get_result(&mut conn)
            .unwrap_or(0);

        // Get aggregated usage via helper
        let usage = get_user_usage(&mut conn, user.id).unwrap_or_default();

        user_infos.push(AdminUserEntry {
            id: user.id,
            email: user.email,
            name: user.name,
            avatar_url: user.avatar_url,
            is_admin: user.is_admin,
            disabled: user.disabled,
            created_at: user.created_at.to_string(),
            session_count,
            total_spend_usd: usage.cost_usd,
            total_input_tokens: usage.input_tokens,
            total_output_tokens: usage.output_tokens,
            total_cache_creation_tokens: usage.cache_creation_tokens,
            total_cache_read_tokens: usage.cache_read_tokens,
        });
    }

    Ok(Json(AdminUsersResponse { users: user_infos }))
}

pub async fn update_user(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(user_id): Path<Uuid>,
    Json(update): Json<UpdateUserRequest>,
) -> Result<EmptyResponse, AppError> {
    let admin = require_admin(&app_state, &cookies).await?;

    // Prevent admin from demoting themselves
    if user_id == admin.id && update.is_admin == Some(false) {
        warn!(
            "Admin {} attempted to remove their own admin status",
            admin.email
        );
        return Err(AppError::BadRequest(
            "admin cannot remove their own admin status",
        ));
    }

    // Prevent admin from disabling themselves
    if user_id == admin.id && update.disabled == Some(true) {
        warn!(
            "Admin {} attempted to disable their own account",
            admin.email
        );
        return Err(AppError::BadRequest(
            "admin cannot disable their own account",
        ));
    }

    let mut conn = app_state.db_pool.get()?;

    // Get target user for logging
    let target_user: User = schema::users::table
        .find(user_id)
        .first(&mut conn)
        .map_err(|_| AppError::NotFound("user"))?;

    // Build update query
    if let Some(is_admin_val) = update.is_admin {
        diesel::update(schema::users::table.find(user_id))
            .set(schema::users::is_admin.eq(is_admin_val))
            .execute(&mut conn)
            .map_err(|e| admin_db_query("Failed to update user admin status", e))?;
        info!(
            "Admin {} set is_admin={} for user {}",
            admin.email, is_admin_val, target_user.email
        );
    }

    if let Some(disabled_val) = update.disabled {
        diesel::update(schema::users::table.find(user_id))
            .set(schema::users::disabled.eq(disabled_val))
            .execute(&mut conn)
            .map_err(|e| admin_db_query("Failed to update user disabled status", e))?;
        info!(
            "Admin {} set disabled={} for user {}",
            admin.email, disabled_val, target_user.email
        );

        // If banning, revoke all tokens and delete all sessions/data
        if disabled_val {
            use crate::schema::proxy_auth_tokens;

            // Revoke all proxy tokens
            let revoked_count = diesel::update(
                proxy_auth_tokens::table.filter(proxy_auth_tokens::user_id.eq(user_id)),
            )
            .set(proxy_auth_tokens::revoked.eq(true))
            .execute(&mut conn)
            .map_err(|e| admin_db_query("Failed to revoke user tokens", e))?;
            if revoked_count > 0 {
                info!(
                    "Revoked {} proxy tokens for banned user {}",
                    revoked_count, target_user.email
                );
            }

            // Delete all user's sessions and associated data
            let (deleted_sessions, deleted_messages, deleted_members) =
                super::helpers::delete_user_sessions(&mut conn, user_id).map_err(|e| {
                    AppError::Internal(format!("Failed to delete user sessions: {e:?}"))
                })?;

            if deleted_sessions > 0 {
                info!(
                    "Deleted {} sessions, {} messages, {} members for banned user {}",
                    deleted_sessions, deleted_messages, deleted_members, target_user.email
                );
            }
        }
    }

    // Handle ban_reason update
    if let Some(reason) = update.ban_reason {
        diesel::update(schema::users::table.find(user_id))
            .set(schema::users::ban_reason.eq(reason.as_ref()))
            .execute(&mut conn)
            .map_err(|e| admin_db_query("Failed to update ban reason", e))?;
        info!(
            "Admin {} set ban_reason for user {}",
            admin.email, target_user.email
        );
    }

    Ok(EmptyResponse::NO_CONTENT)
}

// ============================================================================
// Sessions Endpoint - List and manage all sessions
// ============================================================================

pub async fn list_sessions(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<AdminSessionsResponse>, AppError> {
    let admin = require_admin(&app_state, &cookies).await?;
    info!("Admin {} requested sessions list", admin.email);

    let mut conn = app_state.db_pool.get()?;

    // Get all sessions with user email
    let results: Vec<(crate::models::Session, String)> = schema::sessions::table
        .inner_join(schema::users::table)
        .select((schema::sessions::all_columns, schema::users::email))
        .order(schema::sessions::last_activity.desc())
        .load(&mut conn)
        .map_err(|e| admin_db_query("Failed to load sessions", e))?;

    let session_infos: Vec<AdminSessionInfo> = results
        .into_iter()
        .map(|(session, user_email)| {
            let is_connected = app_state
                .session_manager
                .sessions
                .contains_key(&session.id.to_string());

            AdminSessionInfo {
                id: session.id,
                user_id: session.user_id,
                user_email,
                session_name: session.session_name,
                working_directory: session.working_directory,
                git_branch: session.git_branch,
                status: session.status,
                total_cost_usd: session.total_cost_usd,
                created_at: session.created_at.to_string(),
                last_activity: session.last_activity.to_string(),
                is_connected,
                hostname: session.hostname,
            }
        })
        .collect();

    Ok(Json(AdminSessionsResponse {
        sessions: session_infos,
    }))
}

pub async fn delete_session(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(session_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let admin = require_admin(&app_state, &cookies).await?;

    let mut conn = app_state.db_pool.get()?;

    // Get session info for logging and cost tracking
    let session: crate::models::Session = schema::sessions::table
        .find(session_id)
        .first(&mut conn)
        .map_err(|_| AppError::NotFound("session"))?;

    // Remove from session manager (disconnect if connected)
    let session_key = session_id.to_string();
    app_state
        .session_manager
        .unregister_session(&session_key, None);

    // Delete session and all associated data, recording costs
    super::helpers::delete_session_with_data(&mut conn, &session, true)
        .map_err(|e| AppError::Internal(format!("Failed to delete session data: {e:?}")))?;

    info!(
        "Admin {} deleted session {} ({}) - cost ${:.4} recorded",
        admin.email, session_id, session.session_name, session.total_cost_usd
    );

    Ok(EmptyResponse::NO_CONTENT)
}

fn admin_db_query(context: &str, error: diesel::result::Error) -> AppError {
    AppError::DbQuery(format!("{context}: {error}"))
}
