//! Grant-preserving rotation. Opaque secrets are returned once and only their
//! hashes are stored. Never route these through legacy JWT mint/renew helpers.
use super::{Grant, LinkedAuth, LINKED_TOKEN_PREFIX};
use crate::{
    auth::CurrentUserId,
    errors::AppError,
    models::ProxyAuthToken,
    schema::{linked_client_grants, proxy_auth_tokens, users},
    AppState,
};
use axum::{
    extract::{Path, State},
    Extension, Json,
};
use diesel::prelude::*;
use rand::RngCore;
use shared::api::{
    LinkedClientGrant, LinkedClientGrantsResponse, LinkedClientRefreshResponse, TokenScope,
};
use std::sync::Arc;
use uuid::Uuid;

pub(super) fn random_secret() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub(super) fn mint(
    conn: &mut PgConnection,
    grant: &Grant,
) -> Result<LinkedClientRefreshResponse, AppError> {
    let now = chrono::Utc::now().naive_utc();
    if grant.revoked_at.is_some() || grant.expires_at <= now {
        return Err(AppError::Unauthorized);
    }
    let scope = TokenScope::parse(&grant.scope).ok_or(AppError::Forbidden)?;
    let expires_at = grant.expires_at.min(now + chrono::Duration::hours(1));
    let token = format!("{LINKED_TOKEN_PREFIX}{}", random_secret());
    diesel::insert_into(proxy_auth_tokens::table)
        .values((
            proxy_auth_tokens::user_id.eq(grant.user_id),
            proxy_auth_tokens::name.eq("linked portal"),
            proxy_auth_tokens::token_hash.eq(crate::jwt::hash_token(&token)),
            proxy_auth_tokens::scope.eq(scope.as_str()),
            proxy_auth_tokens::linked_grant_id.eq(grant.id),
            proxy_auth_tokens::expires_at.eq(expires_at),
        ))
        .execute(conn)?;
    Ok(LinkedClientRefreshResponse {
        access_token: token,
        expires_at: expires_at.and_utc().to_rfc3339(),
    })
}

pub(super) fn wire_grant(
    conn: &mut PgConnection,
    grant: Grant,
) -> Result<LinkedClientGrant, AppError> {
    let email = users::table
        .find(grant.user_id)
        .select(users::email)
        .first(conn)?;
    Ok(LinkedClientGrant {
        id: grant.id,
        user_id: grant.user_id,
        user_email: email,
        device_label: grant.device_label,
        client_origin: grant.client_origin,
        scope: TokenScope::parse(&grant.scope).ok_or(AppError::Forbidden)?,
        created_at: grant.created_at.and_utc().to_rfc3339(),
        expires_at: grant.expires_at.and_utc().to_rfc3339(),
        revoked_at: grant.revoked_at.map(|t| t.and_utc().to_rfc3339()),
    })
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
) -> Result<Json<LinkedClientGrantsResponse>, AppError> {
    // The router deliberately denies this metadata list to linked bearers.
    let mut conn = state.conn()?;
    let rows = linked_client_grants::table
        .filter(linked_client_grants::user_id.eq(user_id))
        .order(linked_client_grants::created_at.desc())
        .load::<Grant>(&mut conn)?;
    let grants = rows
        .into_iter()
        .map(|g| wire_grant(&mut conn, g))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(LinkedClientGrantsResponse { grants }))
}

pub async fn refresh(
    State(state): State<Arc<AppState>>,
    auth: Option<Extension<LinkedAuth>>,
) -> Result<Json<LinkedClientRefreshResponse>, AppError> {
    let Extension(auth) = auth.ok_or(AppError::Forbidden)?;
    let mut conn = state.conn()?;
    let response = conn.transaction::<_, AppError, _>(|conn| {
        // Serializes concurrent rotation: only one caller can consume this token.
        let old = proxy_auth_tokens::table
            .find(auth.token_id)
            .for_update()
            .first::<ProxyAuthToken>(conn)?;
        let live = super::validate_row_for_refresh(conn, &old, true)?;
        if live.grant_id != auth.grant_id || live.user_id != auth.user_id {
            return Err(AppError::Forbidden);
        }
        let mut grant = linked_client_grants::table
            .find(auth.grant_id)
            .first::<Grant>(conn)?;
        // Rotation can never increase even a narrower token's scope.
        grant.scope = old.scope.ok_or(AppError::Forbidden)?;
        let response = mint(conn, &grant)?;
        diesel::update(proxy_auth_tokens::table.find(old.id))
            .set(proxy_auth_tokens::revoked.eq(true))
            .execute(conn)?;
        Ok(response)
    })?;
    Ok(Json(response))
}

pub async fn revoke(
    State(state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    auth: Option<Extension<LinkedAuth>>,
    Path(id): Path<Uuid>,
) -> Result<axum::http::StatusCode, AppError> {
    if auth.is_some_and(|Extension(auth)| auth.grant_id != id) {
        return Err(AppError::Forbidden);
    }
    let count = diesel::update(
        linked_client_grants::table
            .filter(linked_client_grants::id.eq(id))
            .filter(linked_client_grants::user_id.eq(user_id)),
    )
    .set(linked_client_grants::revoked_at.eq(chrono::Utc::now().naive_utc()))
    .execute(&mut *state.conn()?)?;
    if count == 0 {
        return Err(AppError::NotFound("Grant not found"));
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}
