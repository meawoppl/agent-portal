//! Proxy Token Management Handlers
//!
//! CRUD endpoints for managing proxy authentication tokens.
//! These allow users to create, list, and revoke tokens for CLI access.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use diesel::prelude::*;
use shared::{
    CreateProxyTokenRequest, CreateProxyTokenResponse, ProxyInitConfig, ProxyTokenInfo,
    ProxyTokenListResponse,
};
use std::sync::Arc;
use tower_cookies::Cookies;
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    errors::AppError,
    jwt::{create_proxy_token, hash_token},
    models::{NewProxyAuthToken, ProxyAuthToken, User},
    schema::proxy_auth_tokens,
    AppState,
};

/// POST /api/proxy-tokens - Create a new proxy token
pub async fn create_token_handler(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Json(req): Json<CreateProxyTokenRequest>,
) -> Result<Json<CreateProxyTokenResponse>, AppError> {
    let user_id = crate::auth::extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    // Get user email for JWT claims
    use crate::schema::users;
    let user: User = users::table
        .find(user_id)
        .first(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    // Generate token ID
    let token_id = Uuid::new_v4();

    // Calculate expiration
    let expires_at = chrono::Utc::now() + chrono::Duration::days(req.expires_in_days as i64);

    // Create JWT
    let jwt_secret = app_state.jwt_secret.as_bytes();
    let token = create_proxy_token(
        jwt_secret,
        token_id,
        user_id,
        &user.email,
        req.expires_in_days,
    )
    .map_err(|e| AppError::Internal(e.to_string()))?;

    // Hash token for storage
    let token_hash = hash_token(&token);

    // Store in database
    let new_token = NewProxyAuthToken {
        user_id,
        name: req.name.clone(),
        token_hash,
        expires_at: expires_at.naive_utc(),
    };

    let saved_token: ProxyAuthToken = diesel::insert_into(proxy_auth_tokens::table)
        .values(&new_token)
        .get_result(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    // Build init URL
    let config = ProxyInitConfig {
        token: token.clone(),
        session_name_prefix: None,
    };
    let encoded_config = config
        .encode()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let init_url = format!("{}/p/{}", app_state.public_url, encoded_config);

    info!("Created proxy token '{}' for user {}", req.name, user.email);

    Ok(Json(CreateProxyTokenResponse {
        id: saved_token.id,
        token,
        init_url,
        expires_at: expires_at.to_rfc3339(),
    }))
}

/// GET /api/proxy-tokens - List all tokens for the current user
pub async fn list_tokens_handler(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<ProxyTokenListResponse>, AppError> {
    let user_id = crate::auth::extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    let tokens: Vec<ProxyAuthToken> = proxy_auth_tokens::table
        .filter(proxy_auth_tokens::user_id.eq(user_id))
        .order(proxy_auth_tokens::created_at.desc())
        .load(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    let token_infos: Vec<ProxyTokenInfo> = tokens
        .into_iter()
        .map(|t| ProxyTokenInfo {
            id: t.id,
            name: t.name,
            created_at: t.created_at.and_utc().to_rfc3339(),
            last_used_at: t.last_used_at.map(|dt| dt.and_utc().to_rfc3339()),
            expires_at: t.expires_at.and_utc().to_rfc3339(),
            revoked: t.revoked,
        })
        .collect();

    Ok(Json(ProxyTokenListResponse {
        tokens: token_infos,
    }))
}

/// DELETE /api/proxy-tokens/:id - Revoke a token
pub async fn revoke_token_handler(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(token_id): Path<Uuid>,
) -> Result<StatusCode, AppError> {
    let user_id = crate::auth::extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    // Update token to revoked (only if owned by user)
    let updated = diesel::update(
        proxy_auth_tokens::table
            .filter(proxy_auth_tokens::id.eq(token_id))
            .filter(proxy_auth_tokens::user_id.eq(user_id)),
    )
    .set(proxy_auth_tokens::revoked.eq(true))
    .execute(&mut conn)
    .map_err(|e| AppError::DbQuery(e.to_string()))?;

    if updated == 0 {
        return Err(AppError::NotFound("proxy token"));
    }

    info!("Revoked proxy token {}", token_id);
    Ok(StatusCode::NO_CONTENT)
}

/// Verify a proxy token and return the user_id if valid
/// This is called from the websocket handler
pub fn verify_and_get_user(
    app_state: &AppState,
    conn: &mut diesel::pg::PgConnection,
    token: &str,
) -> Result<(Uuid, String), StatusCode> {
    // First verify JWT signature and expiration
    let claims =
        crate::jwt::verify_proxy_token(app_state.jwt_secret.as_bytes(), token).map_err(|e| {
            error!("JWT verification failed: {}", e);
            StatusCode::UNAUTHORIZED
        })?;

    // Then check database for revocation
    let token_hash = hash_token(token);
    let db_token: ProxyAuthToken = proxy_auth_tokens::table
        .filter(proxy_auth_tokens::token_hash.eq(&token_hash))
        .first(conn)
        .map_err(|_| {
            error!("Token not found in database");
            StatusCode::UNAUTHORIZED
        })?;

    // Check if revoked
    if db_token.revoked {
        error!("Token has been revoked");
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Check if expired (belt and suspenders - JWT already checked this)
    let now = chrono::Utc::now().naive_utc();
    if db_token.expires_at < now {
        error!("Token has expired");
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Check if user is banned
    use crate::schema::users;
    let user: crate::models::User = users::table.find(claims.sub).first(conn).map_err(|_| {
        error!("User not found for token");
        StatusCode::UNAUTHORIZED
    })?;

    if user.disabled {
        error!("Token belongs to banned user: {}", user.email);
        return Err(StatusCode::FORBIDDEN);
    }

    // Update last_used_at
    let _ = diesel::update(proxy_auth_tokens::table.find(db_token.id))
        .set(proxy_auth_tokens::last_used_at.eq(diesel::dsl::now))
        .execute(conn);

    Ok((claims.sub, claims.email))
}
