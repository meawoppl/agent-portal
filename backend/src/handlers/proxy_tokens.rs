//! Proxy Token Management Handlers
//!
//! CRUD endpoints for managing proxy authentication tokens.
//! These allow users to create, list, and revoke tokens for CLI access.

use axum::{
    extract::{Path, State},
    Json,
};
use diesel::prelude::*;
use shared::{
    CreateProxyTokenRequest, CreateProxyTokenResponse, ProxyInitConfig, ProxyTokenInfo,
    ProxyTokenListResponse, RenewProxyTokenRequest,
};
use std::sync::Arc;
use tower_cookies::Cookies;
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    errors::AppError,
    handlers::responses::EmptyResponse,
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

    let mut conn = app_state.db_pool.get()?;

    // Get user email for JWT claims
    use crate::schema::users;
    let user: User = users::table.find(user_id).first(&mut conn)?;

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
        Some(req.expires_in_days),
    )
    .map_err(|e| AppError::Internal(e.to_string()))?;

    // Hash token for storage
    let token_hash = hash_token(&token);

    // Store in database
    let new_token = NewProxyAuthToken {
        user_id,
        name: req.name.clone(),
        token_hash,
        expires_at: Some(expires_at.naive_utc()),
    };

    let saved_token: ProxyAuthToken = diesel::insert_into(proxy_auth_tokens::table)
        .values(&new_token)
        .get_result(&mut conn)?;

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

    let mut conn = app_state.db_pool.get()?;

    let tokens: Vec<ProxyAuthToken> = proxy_auth_tokens::table
        .filter(proxy_auth_tokens::user_id.eq(user_id))
        .order(proxy_auth_tokens::created_at.desc())
        .load(&mut conn)?;

    let token_infos: Vec<ProxyTokenInfo> = tokens
        .into_iter()
        .map(|t| ProxyTokenInfo {
            id: t.id,
            name: t.name,
            created_at: t.created_at.and_utc().to_rfc3339(),
            last_used_at: t.last_used_at.map(|dt| dt.and_utc().to_rfc3339()),
            expires_at: t.expires_at.map(|dt| dt.and_utc().to_rfc3339()),
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
) -> Result<EmptyResponse, AppError> {
    let user_id = crate::auth::extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get()?;

    // Update token to revoked (only if owned by user)
    let updated = diesel::update(
        proxy_auth_tokens::table
            .filter(proxy_auth_tokens::id.eq(token_id))
            .filter(proxy_auth_tokens::user_id.eq(user_id)),
    )
    .set(proxy_auth_tokens::revoked.eq(true))
    .execute(&mut conn)?;

    if updated == 0 {
        return Err(AppError::NotFound("proxy token"));
    }

    info!("Revoked proxy token {}", token_id);
    Ok(EmptyResponse::NO_CONTENT)
}

/// POST /api/proxy-tokens/:id/renew - Renew a token with a new expiration
pub async fn renew_token_handler(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(token_id): Path<Uuid>,
    Json(req): Json<RenewProxyTokenRequest>,
) -> Result<Json<CreateProxyTokenResponse>, AppError> {
    let user_id = crate::auth::extract_user_id(&app_state, &cookies)?;

    let mut conn = app_state.db_pool.get()?;

    let existing: ProxyAuthToken = proxy_auth_tokens::table
        .filter(proxy_auth_tokens::id.eq(token_id))
        .filter(proxy_auth_tokens::user_id.eq(user_id))
        .first(&mut conn)
        .map_err(|_| AppError::NotFound("proxy token"))?;

    if existing.revoked {
        return Err(AppError::BadRequest("Cannot renew a revoked token"));
    }

    use crate::schema::users;
    let user: User = users::table.find(user_id).first(&mut conn)?;

    let new_token_id = Uuid::new_v4();
    let expires_at = chrono::Utc::now() + chrono::Duration::days(req.expires_in_days as i64);

    let jwt_secret = app_state.jwt_secret.as_bytes();
    let token = create_proxy_token(
        jwt_secret,
        new_token_id,
        user_id,
        &user.email,
        Some(req.expires_in_days),
    )
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let token_hash = hash_token(&token);

    diesel::update(
        proxy_auth_tokens::table
            .filter(proxy_auth_tokens::id.eq(token_id))
            .filter(proxy_auth_tokens::user_id.eq(user_id)),
    )
    .set((
        proxy_auth_tokens::token_hash.eq(&token_hash),
        proxy_auth_tokens::expires_at.eq(Some(expires_at.naive_utc())),
    ))
    .execute(&mut conn)?;

    let config = ProxyInitConfig {
        token: token.clone(),
        session_name_prefix: None,
    };
    let encoded_config = config
        .encode()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let init_url = format!("{}/p/{}", app_state.public_url, encoded_config);

    info!(
        "Renewed proxy token '{}' for user {}",
        existing.name, user.email
    );

    Ok(Json(CreateProxyTokenResponse {
        id: token_id,
        token,
        init_url,
        expires_at: expires_at.to_rfc3339(),
    }))
}

/// Verify a proxy token and return the user_id if valid
/// This is called from the websocket handler
pub fn verify_and_get_user(
    app_state: &AppState,
    conn: &mut diesel::pg::PgConnection,
    token: &str,
) -> Result<(Uuid, String), AppError> {
    // First verify JWT signature and expiration
    let claims =
        crate::jwt::verify_proxy_token(app_state.jwt_secret.as_bytes(), token).map_err(|e| {
            error!("JWT verification failed: {}", e);
            AppError::Unauthorized
        })?;

    // Then check database for revocation
    let token_hash = hash_token(token);
    let db_token: ProxyAuthToken = proxy_auth_tokens::table
        .filter(proxy_auth_tokens::token_hash.eq(&token_hash))
        .first(conn)
        .map_err(|_| {
            error!("Token not found in database");
            AppError::Unauthorized
        })?;

    // Check if revoked
    if db_token.revoked {
        error!("Token has been revoked");
        return Err(AppError::Unauthorized);
    }

    // Check if expired (belt and suspenders - JWT already checked this).
    // A NULL `expires_at` means the token never expires (launch/launcher
    // tokens); its lifetime is governed by revocation instead. See #932.
    if let Some(expires_at) = db_token.expires_at {
        let now = chrono::Utc::now().naive_utc();
        if expires_at < now {
            error!("Token has expired");
            return Err(AppError::Unauthorized);
        }
    }

    // Check if user is banned
    use crate::schema::users;
    let user: crate::models::User = users::table.find(claims.sub).first(conn).map_err(|_| {
        error!("User not found for token");
        AppError::Unauthorized
    })?;

    if user.disabled {
        error!("Token belongs to banned user: {}", user.email);
        return Err(AppError::Forbidden);
    }

    // Update last_used_at
    let _ = diesel::update(proxy_auth_tokens::table.find(db_token.id))
        .set(proxy_auth_tokens::last_used_at.eq(diesel::dsl::now))
        .execute(conn);

    Ok((claims.sub, claims.email))
}

/// Name stamped on tokens minted per launched session (`mint_launch_token`).
///
/// Only tokens with this name are bound to a session and revoked when the
/// session ends. Durable credentials — user dashboard tokens and device-flow
/// tokens (which a direct `claude-portal` CLI may reuse across sessions) — must
/// never be revoked just because one session they registered terminated.
pub const LAUNCH_TOKEN_NAME: &str = "launcher-spawned";

/// Bind a launch token to the session whose proxy registered with it, and
/// revoke any older tokens previously bound to the same session.
///
/// Launch tokens never expire (see #932); instead their lifetime tracks the
/// session. The proxy currently attached to a session is whichever one
/// registered most recently, so binding the active token and revoking the
/// rest keeps exactly one live token per session. A reconnect re-binds the
/// *same* token (no-op); a relaunch binds a fresh token and revokes the dead
/// proxy's old one.
///
/// Only `LAUNCH_TOKEN_NAME` tokens are bound — the filter makes binding a no-op
/// for durable credentials, so they are never revoked on session termination.
pub fn link_token_to_session(conn: &mut diesel::pg::PgConnection, token: &str, session: Uuid) {
    let token_hash = hash_token(token);

    let bound = match diesel::update(
        proxy_auth_tokens::table
            .filter(proxy_auth_tokens::token_hash.eq(&token_hash))
            .filter(proxy_auth_tokens::name.eq(LAUNCH_TOKEN_NAME)),
    )
    .set(proxy_auth_tokens::session_id.eq(Some(session)))
    .execute(conn)
    {
        Ok(n) => n,
        Err(e) => {
            error!("Failed to bind token to session {}: {}", session, e);
            return;
        }
    };

    // Not a launch token (durable credential): leave it and every other token
    // for this session alone.
    if bound == 0 {
        return;
    }

    // Revoke superseded tokens for this session (any token bound to it that
    // isn't the one that just registered).
    let _ = diesel::update(
        proxy_auth_tokens::table
            .filter(proxy_auth_tokens::session_id.eq(session))
            .filter(proxy_auth_tokens::token_hash.ne(&token_hash)),
    )
    .set(proxy_auth_tokens::revoked.eq(true))
    .execute(conn);
}

/// Revoke every token bound to a session. Called when the session terminates
/// (proxy exit) or is deleted, so a session's tokens never outlive it.
pub fn revoke_tokens_for_session(conn: &mut diesel::pg::PgConnection, session: Uuid) {
    if let Err(e) =
        diesel::update(proxy_auth_tokens::table.filter(proxy_auth_tokens::session_id.eq(session)))
            .set(proxy_auth_tokens::revoked.eq(true))
            .execute(conn)
    {
        error!("Failed to revoke tokens for session {}: {}", session, e);
    }
}
