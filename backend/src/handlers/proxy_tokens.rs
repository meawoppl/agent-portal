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
use tracing::{error, info};
use uuid::Uuid;

use crate::{
    auth::CurrentUserId,
    errors::AppError,
    handlers::responses::EmptyResponse,
    jwt::{create_proxy_token_with_type, hash_token},
    models::{NewProxyAuthToken, ProxyAuthToken, User},
    schema::proxy_auth_tokens,
    AppState,
};
use shared::TOKEN_TYPE_PROXY;

/// How [`issue_proxy_token`] persists the freshly minted token.
pub enum TokenPersist<'a> {
    /// Insert a new `proxy_auth_tokens` row with this display name.
    Create { name: &'a str },
    /// Re-point an existing row (keeping its id, name, and created_at) at the
    /// new JWT, refreshing its expiry.
    Renew { token_id: Uuid },
}

/// A freshly minted proxy token and the database row backing it.
pub struct IssuedProxyToken {
    /// `proxy_auth_tokens.id` of the inserted or renewed row.
    pub row_id: Uuid,
    /// The signed JWT handed to the proxy/launcher.
    pub token: String,
    /// Expiry matching the JWT claims; `None` for non-expiring tokens.
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Email of the token's owner, for logging.
    pub user_email: String,
}

/// Result of verifying a proxy token against both JWT claims and the backing
/// database row.
pub struct VerifiedProxyToken {
    pub user_id: Uuid,
    pub email: String,
    pub token: ProxyAuthToken,
}

/// Mint a proxy token for `user_id` and persist it: load the user, create the
/// JWT, hash it, and insert (or renew) the `proxy_auth_tokens` row.
///
/// `expires_in_days` is `Some(n)` for tokens with a fixed TTL or `None` for
/// credentials whose lifetime is governed by DB state. Live launcher
/// credentials are rotated to expiring replacements after registration.
pub fn issue_proxy_token(
    conn: &mut diesel::pg::PgConnection,
    jwt_secret: &[u8],
    user_id: Uuid,
    persist: TokenPersist<'_>,
    expires_in_days: Option<u32>,
) -> Result<IssuedProxyToken, AppError> {
    issue_proxy_token_with_type(
        conn,
        jwt_secret,
        user_id,
        persist,
        expires_in_days,
        TOKEN_TYPE_PROXY,
    )
}

pub fn issue_proxy_token_with_type(
    conn: &mut diesel::pg::PgConnection,
    jwt_secret: &[u8],
    user_id: Uuid,
    persist: TokenPersist<'_>,
    expires_in_days: Option<u32>,
    token_type: &str,
) -> Result<IssuedProxyToken, AppError> {
    use crate::schema::users;
    let user: User = users::table.find(user_id).first(conn)?;

    let expires_at =
        expires_in_days.map(|days| chrono::Utc::now() + chrono::Duration::days(days as i64));

    let token = create_proxy_token_with_type(
        jwt_secret,
        Uuid::new_v4(),
        user_id,
        &user.email,
        expires_in_days,
        token_type,
    )
    .map_err(|e| AppError::Internal(e.to_string()))?;

    let token_hash = hash_token(&token);

    let row_id = match persist {
        TokenPersist::Create { name } => {
            let new_token = NewProxyAuthToken {
                user_id,
                name: name.to_string(),
                token_hash,
                expires_at: expires_at.map(|t| t.naive_utc()),
            };
            let saved_token: ProxyAuthToken = diesel::insert_into(proxy_auth_tokens::table)
                .values(&new_token)
                .get_result(conn)?;
            saved_token.id
        }
        TokenPersist::Renew { token_id } => {
            diesel::update(
                proxy_auth_tokens::table
                    .filter(proxy_auth_tokens::id.eq(token_id))
                    .filter(proxy_auth_tokens::user_id.eq(user_id)),
            )
            .set((
                proxy_auth_tokens::token_hash.eq(&token_hash),
                proxy_auth_tokens::expires_at.eq(expires_at.map(|t| t.naive_utc())),
            ))
            .execute(conn)?;
            token_id
        }
    };

    Ok(IssuedProxyToken {
        row_id,
        token,
        expires_at,
        user_email: user.email,
    })
}

/// Build the `CreateProxyTokenResponse` (including the encoded `/p/` init URL)
/// for a freshly issued token. Shared by the create and renew handlers.
fn token_response(
    app_state: &AppState,
    issued: IssuedProxyToken,
) -> Result<CreateProxyTokenResponse, AppError> {
    let expires_at = issued
        .expires_at
        .ok_or_else(|| AppError::Internal("token issued without an expiry".to_string()))?;

    let config = ProxyInitConfig {
        token: issued.token.clone(),
        session_name_prefix: None,
    };
    let encoded_config = config
        .encode()
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let init_url = format!("{}/p/{}", app_state.public_url, encoded_config);

    Ok(CreateProxyTokenResponse {
        id: issued.row_id,
        token: issued.token,
        init_url,
        expires_at: expires_at.to_rfc3339(),
    })
}

/// POST /api/proxy-tokens - Create a new proxy token
pub async fn create_token_handler(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Json(req): Json<CreateProxyTokenRequest>,
) -> Result<Json<CreateProxyTokenResponse>, AppError> {
    let mut conn = app_state.conn()?;

    let issued = issue_proxy_token(
        &mut conn,
        app_state.jwt_secret.as_bytes(),
        user_id,
        TokenPersist::Create { name: &req.name },
        Some(req.expires_in_days),
    )?;

    let user_email = issued.user_email.clone();
    let response = token_response(&app_state, issued)?;

    info!("Created proxy token '{}' for user {}", req.name, user_email);

    Ok(Json(response))
}

/// GET /api/proxy-tokens - List all tokens for the current user
pub async fn list_tokens_handler(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
) -> Result<Json<ProxyTokenListResponse>, AppError> {
    let mut conn = app_state.conn()?;

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
    CurrentUserId(user_id): CurrentUserId,
    Path(token_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;

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
    CurrentUserId(user_id): CurrentUserId,
    Path(token_id): Path<Uuid>,
    Json(req): Json<RenewProxyTokenRequest>,
) -> Result<Json<CreateProxyTokenResponse>, AppError> {
    let mut conn = app_state.conn()?;

    let existing: ProxyAuthToken = proxy_auth_tokens::table
        .filter(proxy_auth_tokens::id.eq(token_id))
        .filter(proxy_auth_tokens::user_id.eq(user_id))
        .first(&mut conn)
        .map_err(|_| AppError::NotFound("proxy token"))?;

    if existing.revoked {
        return Err(AppError::BadRequest("Cannot renew a revoked token"));
    }

    let issued = issue_proxy_token(
        &mut conn,
        app_state.jwt_secret.as_bytes(),
        user_id,
        TokenPersist::Renew { token_id },
        Some(req.expires_in_days),
    )?;

    let user_email = issued.user_email.clone();
    let response = token_response(&app_state, issued)?;

    info!(
        "Renewed proxy token '{}' for user {}",
        existing.name, user_email
    );

    Ok(Json(response))
}

/// Verify a proxy token and return its user plus backing token row if valid.
///
/// The row metadata lets long-lived websocket clients rotate credentials after
/// registration without changing the existing verifier's public shape.
pub fn verify_and_get_user_with_token(
    app_state: &AppState,
    conn: &mut diesel::pg::PgConnection,
    token: &str,
) -> Result<VerifiedProxyToken, AppError> {
    // First verify JWT signature and expiration
    let claims =
        crate::jwt::verify_proxy_token(app_state.jwt_secret.as_bytes(), token).map_err(|e| {
            error!("JWT verification failed: {}", e);
            AppError::Unauthorized
        })?;

    // Then check database for revocation.
    //
    // Only `NotFound` means the token is bad. Any other Diesel error is a
    // transient infrastructure failure (connection reset, pool timeout,
    // statement timeout — common during deploys), and MUST NOT be reported
    // as Unauthorized: clients treat auth rejection as fatal (launchers
    // park, proxies stop reconnecting), so conflating the two turns a DB
    // blip into a fleet-wide manual-restart event. See #1264.
    let token_hash = hash_token(token);
    let db_token: ProxyAuthToken = proxy_auth_tokens::table
        .filter(proxy_auth_tokens::token_hash.eq(&token_hash))
        .first(conn)
        .map_err(|e| match e {
            diesel::result::Error::NotFound => {
                error!("Token not found in database");
                AppError::Unauthorized
            }
            other => {
                error!("Transient DB error verifying token: {}", other);
                AppError::ServiceUnavailable("token verification unavailable")
            }
        })?;

    // Check if revoked
    if db_token.revoked {
        error!("Token has been revoked");
        return Err(AppError::Unauthorized);
    }

    // Check if expired (belt and suspenders - JWT already checked this).
    // A NULL `expires_at` means the row has no fixed expiry; its lifetime is
    // governed by revocation/session binding. Live launcher credentials rotate
    // to expiring replacements after registration (#1237).
    if let Some(expires_at) = db_token.expires_at {
        let now = chrono::Utc::now().naive_utc();
        if expires_at < now {
            error!("Token has expired");
            return Err(AppError::Unauthorized);
        }
    }

    // Check if user is banned
    use crate::schema::users;
    let user: crate::models::User =
        users::table
            .find(claims.sub)
            .first(conn)
            .map_err(|e| match e {
                diesel::result::Error::NotFound => {
                    error!("User not found for token");
                    AppError::Unauthorized
                }
                other => {
                    error!("Transient DB error loading token user: {}", other);
                    AppError::ServiceUnavailable("token verification unavailable")
                }
            })?;

    if user.disabled {
        error!("Token belongs to banned user: {}", user.email);
        return Err(AppError::Forbidden);
    }

    // Update last_used_at
    let _ = diesel::update(proxy_auth_tokens::table.find(db_token.id))
        .set(proxy_auth_tokens::last_used_at.eq(diesel::dsl::now))
        .execute(conn);

    Ok(VerifiedProxyToken {
        user_id: claims.sub,
        email: claims.email,
        token: db_token,
    })
}

/// Verify a proxy token and return the user_id if valid.
/// This is called from the websocket handler.
pub fn verify_and_get_user(
    app_state: &AppState,
    conn: &mut diesel::pg::PgConnection,
    token: &str,
) -> Result<(Uuid, String), AppError> {
    let verified = verify_and_get_user_with_token(app_state, conn, token)?;
    Ok((verified.user_id, verified.email))
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
