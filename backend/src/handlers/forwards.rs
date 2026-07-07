//! Port-forward registration (docs/PORT_FORWARDING.md): agents declare local
//! ports with `agent-portal forward <port>`; the rows in `session_forwards`
//! are the authoritative allowlist the forward-origin reverse proxy checks.
//!
//! Mounted at both `/api/sessions/{id}/forwards` (browser) and
//! `/api/agent/sessions/{id}/forwards` (CLI) — [`resolve_user`] accepts a
//! session cookie or a `Bearer` proxy token, so the handlers are shared.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use tower_cookies::Cookies;
use tracing::info;
use uuid::Uuid;

use shared::api::{
    CreateForwardRequest, CreateForwardResponse, ForwardInfo, SessionForwardsResponse,
};
use shared::{ForwardPortFields, ServerToClient, ServerToProxy};

use crate::errors::AppError;
use crate::handlers::agent_comms::resolve_user;
use crate::models::{NewSessionForward, Session, SessionForward};
use crate::AppState;

/// How long to wait for the proxy's `ForwardStatus` probe reply before
/// reporting "unknown" (`listening: None`) — covers proxies that predate
/// forwarding support and never answer.
const FORWARD_STATUS_TIMEOUT: Duration = Duration::from_secs(3);

/// Public URL for a forward: `{scheme}://{port}--{session-simple}.{domain}/`.
/// Errors when `PORTAL_FORWARD_DOMAIN` is unset (forwarding disabled).
fn forward_url(app_state: &AppState, session_id: Uuid, port: u16) -> Result<String, AppError> {
    let domain = app_state
        .forward_domain
        .as_deref()
        .ok_or(AppError::ServiceUnavailable(
            "Forwarding is not configured on this server",
        ))?;
    let scheme = if app_state.public_url.starts_with("https://") {
        "https"
    } else {
        "http"
    };
    Ok(format!(
        "{scheme}://{port}--{}.{domain}/",
        session_id.simple()
    ))
}

fn to_forward_info(app_state: &AppState, row: &SessionForward) -> Result<ForwardInfo, AppError> {
    Ok(ForwardInfo {
        port: row.port as u16,
        url: forward_url(app_state, row.session_id, row.port as u16)?,
        created_at: DateTime::<Utc>::from_naive_utc_and_offset(row.created_at, Utc).to_rfc3339(),
    })
}

/// Authorize `user_id` as a member of `session_id` and return the session.
/// Read access — sufficient to *see* forwards, never to change them.
fn member_session(
    conn: &mut crate::db::DbConnection,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<Session, AppError> {
    use crate::schema::{session_members, sessions};
    sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .select(Session::as_select())
        .first(conn)
        .map_err(|_| AppError::NotFound("session"))
}

/// Authorize `user_id` as the *owner* of `session_id` and return the session.
/// Registering a forward exposes a loopback port on the proxy host and
/// revoking one tears it down — both are owner-only, a strictly tighter gate
/// than transcript read access (a viewer member must not be able to
/// open/probe ports on the machine running the agent). The CLI path clears
/// this too: the launcher's bearer token resolves to the session owner.
fn owner_session(
    conn: &mut crate::db::DbConnection,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<Session, AppError> {
    use crate::schema::sessions;
    sessions::table
        .filter(sessions::id.eq(session_id))
        .filter(sessions::user_id.eq(user_id))
        .select(Session::as_select())
        .first(conn)
        .map_err(|_| AppError::NotFound("session"))
}

/// POST …/sessions/{id}/forwards — declare a port (idempotent), sync the
/// proxy's allowlist, and report the probe-dial result.
pub async fn create_forward(
    State(app_state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    cookies: Cookies,
    Json(req): Json<CreateForwardRequest>,
) -> Result<Json<CreateForwardResponse>, AppError> {
    if req.port == 0 {
        return Err(AppError::BadRequest("port must be 1-65535"));
    }
    let user_id = resolve_user(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;
    let session = owner_session(&mut conn, session_id, user_id)?;
    // Fail before touching state when forwarding is disabled.
    forward_url(&app_state, session_id, req.port)?;

    use crate::schema::session_forwards;
    diesel::insert_into(session_forwards::table)
        .values(&NewSessionForward {
            session_id,
            port: req.port as i32,
        })
        .on_conflict((session_forwards::session_id, session_forwards::port))
        .do_nothing()
        .execute(&mut conn)?;
    let row: SessionForward = session_forwards::table
        .filter(session_forwards::session_id.eq(session_id))
        .filter(session_forwards::port.eq(req.port as i32))
        .select(SessionForward::as_select())
        .first(&mut conn)?;

    // Sync the proxy's allowlist and wait (briefly) for its probe verdict.
    let status_rx = app_state
        .session_manager
        .register_forward_status(session_id, req.port);
    let sent = app_state.session_manager.send_to_connected_session(
        &session.session_key,
        ServerToProxy::ForwardOpen(ForwardPortFields { port: req.port }),
    );
    let (listening, probe_error) = if sent {
        match tokio::time::timeout(FORWARD_STATUS_TIMEOUT, status_rx).await {
            Ok(Ok(status)) => (Some(status.listening), status.error),
            _ => {
                app_state
                    .session_manager
                    .cancel_forward_status(session_id, req.port);
                (None, None)
            }
        }
    } else {
        app_state
            .session_manager
            .cancel_forward_status(session_id, req.port);
        (None, None)
    };

    app_state.session_manager.broadcast_to_web_clients(
        &session.session_key,
        ServerToClient::ForwardsChanged { session_id },
    );
    info!(
        "Forward registered: session {} port {} (listening: {:?})",
        session_id, req.port, listening
    );

    Ok(Json(CreateForwardResponse {
        forward: to_forward_info(&app_state, &row)?,
        listening,
        probe_error,
    }))
}

/// GET …/sessions/{id}/forwards — active forwards. Empty (not an error) when
/// forwarding is disabled, so session views don't 503 on every load.
pub async fn list_forwards(
    State(app_state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<SessionForwardsResponse>, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;
    member_session(&mut conn, session_id, user_id)?;

    if app_state.forward_domain.is_none() {
        return Ok(Json(SessionForwardsResponse { forwards: vec![] }));
    }

    use crate::schema::session_forwards;
    let rows: Vec<SessionForward> = session_forwards::table
        .filter(session_forwards::session_id.eq(session_id))
        .order(session_forwards::port.asc())
        .select(SessionForward::as_select())
        .load(&mut conn)?;

    let forwards = rows
        .iter()
        .map(|row| to_forward_info(&app_state, row))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(SessionForwardsResponse { forwards }))
}

/// DELETE …/sessions/{id}/forwards/{port} — revoke (owner only). Drops the
/// allowlist row and tells the proxy to close the port and its live streams.
pub async fn delete_forward(
    State(app_state): State<Arc<AppState>>,
    Path((session_id, port)): Path<(Uuid, u16)>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<StatusCode, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;
    let session = owner_session(&mut conn, session_id, user_id)?;

    use crate::schema::session_forwards;
    let deleted = diesel::delete(
        session_forwards::table
            .filter(session_forwards::session_id.eq(session_id))
            .filter(session_forwards::port.eq(port as i32)),
    )
    .execute(&mut conn)?;
    if deleted == 0 {
        return Err(AppError::NotFound("forward"));
    }

    // Best effort — a disconnected proxy re-syncs from the DB on reconnect.
    app_state.session_manager.send_to_connected_session(
        &session.session_key,
        ServerToProxy::ForwardClose(ForwardPortFields { port }),
    );
    app_state.session_manager.broadcast_to_web_clients(
        &session.session_key,
        ServerToClient::ForwardsChanged { session_id },
    );
    info!("Forward revoked: session {} port {}", session_id, port);

    Ok(StatusCode::NO_CONTENT)
}
