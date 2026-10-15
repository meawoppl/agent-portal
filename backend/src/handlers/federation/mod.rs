//! Linked credentials have a separate, opaque bearer namespace. They never
//! pass the legacy proxy verifier, which would discard their authorization
//! ceiling. The router gate is default-deny; handlers still enforce roles.
use crate::{
    errors::AppError,
    models::ProxyAuthToken,
    schema::{federation_policy, linked_client_grants, proxy_auth_tokens, users},
    AppState,
};
use axum::{
    extract::{MatchedPath, Request, State},
    http::{HeaderMap, Method},
    middleware::Next,
    response::{IntoResponse, Response},
};
use diesel::prelude::*;
use shared::api::{FederationPolicy, TokenScope};
use std::sync::Arc;
use uuid::Uuid;

pub const LINKED_TOKEN_PREFIX: &str = "ap_link_";

#[derive(Clone, Debug, Queryable, Selectable)]
#[diesel(table_name = linked_client_grants)]
pub struct Grant {
    pub id: Uuid,
    pub user_id: Uuid,
    pub client_origin: String,
    pub scope: String,
    pub device_label: Option<String>,
    pub created_at: chrono::NaiveDateTime,
    pub expires_at: chrono::NaiveDateTime,
    pub revoked_at: Option<chrono::NaiveDateTime>,
}

/// Retained on requests and sockets. No secret bearer is kept in this context.
/// The identity is immutable; every use reloads the token, grant and policy.
#[derive(Clone, Debug)]
pub struct LinkedAuth {
    pub token_id: Uuid,
    pub grant_id: Uuid,
    pub user_id: Uuid,
    pub scope: TokenScope,
    pub client_origin: String,
}

pub fn policy_allows(policy: &FederationPolicy, origin: &str, scope: TokenScope) -> bool {
    match policy {
        FederationPolicy::Off => false,
        FederationPolicy::Any { max_scope } => max_scope.permits(scope),
        FederationPolicy::Allowlist { origins, max_scope } => {
            max_scope.permits(scope) && origins.iter().any(|s| s == origin)
        }
    }
}

pub fn load_policy(conn: &mut PgConnection) -> Result<FederationPolicy, AppError> {
    let value: serde_json::Value = federation_policy::table
        .select(federation_policy::policy)
        .first(conn)?;
    serde_json::from_value(value).map_err(|_| AppError::Forbidden)
}

fn validate_row(conn: &mut PgConnection, token: &ProxyAuthToken) -> Result<LinkedAuth, AppError> {
    validate_row_for_refresh(conn, token, false)
}

// Expiry recovery is exclusive to atomic refresh; revoked/rotated credentials,
// expired grants, disabled users and current policy are always enforced.
fn validate_row_for_refresh(
    conn: &mut PgConnection,
    token: &ProxyAuthToken,
    refresh: bool,
) -> Result<LinkedAuth, AppError> {
    let now = chrono::Utc::now().naive_utc();
    let scope = token
        .scope
        .as_deref()
        .and_then(TokenScope::parse)
        .ok_or(AppError::Unauthorized)?;
    let grant_id = token.linked_grant_id.ok_or(AppError::Unauthorized)?;
    if token.revoked
        || token
            .expires_at
            .is_none_or(|expiry| !refresh && expiry <= now)
    {
        return Err(AppError::Unauthorized);
    }
    let grant = linked_client_grants::table
        .find(grant_id)
        .first::<Grant>(conn)?;
    let grant_scope = TokenScope::parse(&grant.scope).ok_or(AppError::Unauthorized)?;
    if grant.user_id != token.user_id
        || grant.revoked_at.is_some()
        || grant.expires_at <= now
        || !grant_scope.permits(scope)
    {
        return Err(AppError::Unauthorized);
    }
    let disabled: bool = users::table
        .find(token.user_id)
        .select(users::disabled)
        .first(conn)?;
    if disabled || !policy_allows(&load_policy(conn)?, &grant.client_origin, scope) {
        return Err(AppError::Forbidden);
    }
    Ok(LinkedAuth {
        token_id: token.id,
        grant_id,
        user_id: token.user_id,
        scope,
        client_origin: grant.client_origin,
    })
}

pub fn authenticate(
    state: &AppState,
    conn: &mut PgConnection,
    bearer: &str,
    headers: &HeaderMap,
) -> Result<LinkedAuth, AppError> {
    authenticate_for_refresh(state, conn, bearer, headers, false)
}

fn authenticate_for_refresh(
    _state: &AppState,
    conn: &mut PgConnection,
    bearer: &str,
    headers: &HeaderMap,
    refresh: bool,
) -> Result<LinkedAuth, AppError> {
    if !bearer.starts_with(LINKED_TOKEN_PREFIX) {
        return Err(AppError::Unauthorized);
    }
    let token = proxy_auth_tokens::table
        .filter(proxy_auth_tokens::token_hash.eq(crate::jwt::hash_token(bearer)))
        .first::<ProxyAuthToken>(conn)
        .optional()?
        .ok_or(AppError::Unauthorized)?;
    let auth = validate_row_for_refresh(conn, &token, refresh)?;
    // Origin constrains browsers, not native-client attestation. Requiring it
    // also prevents accidental credential use without an explicit connection.
    let origin = headers
        .get(axum::http::header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| shared::urls::canonical_origin(s).ok())
        .ok_or(AppError::Forbidden)?;
    if origin != auth.client_origin {
        return Err(AppError::Forbidden);
    }
    Ok(auth)
}

impl LinkedAuth {
    pub fn authorize(&self, state: &AppState, required: TokenScope) -> Result<(), AppError> {
        let mut conn = state.conn()?;
        let token = proxy_auth_tokens::table
            .find(self.token_id)
            .first::<ProxyAuthToken>(&mut conn)?;
        let live = validate_row(&mut conn, &token)?;
        if live.grant_id != self.grant_id
            || live.user_id != self.user_id
            || live.client_origin != self.client_origin
            || !self.scope.permits(required)
            || !live.scope.permits(required)
        {
            return Err(AppError::Forbidden);
        }
        Ok(())
    }
}

/// Explicit route classification, never "all GETs are reads": forward-open
/// mints an unscoped cookie, and credential/agent/admin APIs confer authority.
fn route_scope(method: &Method, route: &str) -> Option<TokenScope> {
    use TokenScope::{Read, Write};
    match (method.as_str(), route) {
        (
            "GET",
            "/api/sessions"
            | "/api/sessions/{id}"
            | "/api/agent/sessions"
            | "/api/sessions/{id}/messages"
            | "/api/sessions/{id}/turn-metrics"
            | "/api/sessions/{id}/edit-stack"
            | "/api/sessions/{id}/edit-stack/{item_id}/image"
            | "/api/sessions/{id}/members"
            | "/api/sessions/{id}/forwards"
            | "/api/plugins"
            | "/api/metrics/recent"
            | "/api/config"
            | "/api/sessions/{id}/files/pull"
            | "/api/images/{id}"
            | "/api/media/{id}"
            | "/api/scheduled-tasks"
            | "/api/scheduled-tasks/{id}",
        ) => Some(Read),
        (
            "POST",
            "/api/sessions/{id}/fork"
            | "/api/sessions/{id}/stop"
            | "/api/sessions/{id}/pause"
            | "/api/sessions/{id}/resume"
            | "/api/stt/transcribe"
            | "/api/scheduled-tasks",
        ) => Some(Write),
        (
            "PATCH" | "DELETE",
            "/api/sessions/{id}/edit-stack/{item_id}" | "/api/scheduled-tasks/{id}",
        ) => Some(Write),
        ("POST", "/api/linked-grants/refresh") | ("DELETE", "/api/linked-grants/{id}") => {
            Some(Read)
        }
        ("DELETE", "/api/sessions/{id}" | "/api/sessions/{id}/forwards") => Some(Write),
        // WS bearer upgrades deliberately remain disabled until one-use tickets
        // land. No enrollment or credential-minting routes are enabled here.
        _ => None,
    }
}

/// Exhaustive so new message types require an explicit scope decision.
pub fn message_scope(message: &shared::ClientToServer) -> TokenScope {
    match message {
        shared::ClientToServer::Register(_) => TokenScope::Read,
        shared::ClientToServer::AgentInput { .. }
        | shared::ClientToServer::PermissionResponse(_)
        | shared::ClientToServer::FileUploadStart(_)
        | shared::ClientToServer::FileUploadChunk(_)
        | shared::ClientToServer::Interrupt
        | shared::ClientToServer::ScheduleLimitContinuation { .. } => TokenScope::Write,
    }
}

pub async fn linked_scope_gate(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(bearer) = crate::auth::bearer_token(req.headers()) else {
        return next.run(req).await;
    };
    if !bearer.starts_with(LINKED_TOKEN_PREFIX) {
        return next.run(req).await;
    }
    let result = (|| {
        let host = req
            .headers()
            .get(axum::http::header::HOST)
            .and_then(|h| h.to_str().ok())
            .or_else(|| req.uri().authority().map(|a| a.as_str()));
        if state
            .forward_domain
            .as_deref()
            .zip(host)
            .is_some_and(|(domain, host)| {
                super::forward_proxy::parse_forward_host(host, domain).is_some()
            })
        {
            return Err(AppError::Forbidden);
        }
        let route = req
            .extensions()
            .get::<MatchedPath>()
            .map(|p| p.as_str())
            .unwrap_or("");
        let required = route_scope(req.method(), route).ok_or(AppError::Forbidden)?;
        let refresh = req.method() == Method::POST && route == "/api/linked-grants/refresh";
        let auth =
            authenticate_for_refresh(&state, &mut *state.conn()?, bearer, req.headers(), refresh)?;
        if !auth.scope.permits(required) {
            return Err(AppError::Forbidden);
        }
        Ok(auth)
    })();
    match result {
        Ok(auth) => {
            req.extensions_mut().insert(auth);
            let mut response = next.run(req).await;
            // Even a private browser cache is shared by accounts using the
            // same URL. Every linked read must reach the live grant gate.
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("private, no-store"),
            );
            response
        }
        Err(error) => {
            let mut response = error.into_response();
            // This gate wraps CORS (and forwarded hosts). Its public error
            // responses must remain readable cross-origin, so a revoked
            // credential appears as 401/403 rather than a network failure.
            response.headers_mut().insert(
                axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
                axum::http::HeaderValue::from_static("*"),
            );
            response.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            response
        }
    }
}

#[cfg(test)]
mod tests;

pub mod policy;

#[cfg(test)]
static TEST_POLICY_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub mod credentials;
pub mod enrollment;
