//! Port-forward registration (docs/PORT_FORWARDING.md): a session has at most
//! one forwarded port. `session_forwards` holds it; `forward_subdomains` is the
//! stable label ↔ session lookup the reverse proxy routes by. An agent that
//! needs several services fronts them behind its own reverse proxy on the one
//! forwarded port.
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
use sha2::{Digest, Sha256};
use tower_cookies::Cookies;
use tracing::info;
use uuid::Uuid;

use shared::api::{
    CreateForwardRequest, CreateForwardResponse, ForwardInfo, SessionForwardsResponse,
};
use shared::{ForwardPortFields, ServerToClient, ServerToProxy};

use crate::errors::AppError;
use crate::handlers::agent_comms::resolve_user;
use crate::models::{NewForwardSubdomain, NewSessionForward, Session, SessionForward};
use crate::AppState;

/// How long to wait for the proxy's `ForwardStatus` probe reply before
/// reporting "unknown" (`listening: None`) — covers proxies that predate
/// forwarding support and never answer.
const FORWARD_STATUS_TIMEOUT: Duration = Duration::from_secs(3);

/// Length of a subdomain label in hex chars (32 bits). Short by design; the
/// LUT + collision-retry in [`ensure_subdomain_label`] keeps it unambiguous.
const LABEL_HEX_LEN: usize = 8;

/// Public URL for a forward: `{scheme}://{label}.{domain}/`. Errors when
/// `PORTAL_FORWARD_DOMAIN` is unset (forwarding disabled).
fn forward_url(app_state: &AppState, label: &str) -> Result<String, AppError> {
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
    Ok(format!("{scheme}://{label}.{domain}/"))
}

fn to_forward_info(
    app_state: &AppState,
    row: &SessionForward,
    label: &str,
) -> Result<ForwardInfo, AppError> {
    Ok(ForwardInfo {
        port: row.port as u16,
        url: forward_url(app_state, label)?,
        created_at: DateTime::<Utc>::from_naive_utc_and_offset(row.created_at, Utc).to_rfc3339(),
        public: row.public,
        // Latest probe verdict from the proxy's background health check;
        // `None` until a probe has reported (drives the chip tint).
        listening: app_state
            .session_manager
            .forward_health(row.session_id, row.port as u16),
    })
}

/// The `attempt`-th candidate label for a session: the first [`LABEL_HEX_LEN`]
/// hex chars of `sha256(session_id || attempt)`. Deterministic, so attempt 0 is
/// stable across calls; later attempts only come into play on a collision.
fn label_candidate(session_id: Uuid, attempt: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    hasher.update(attempt.to_le_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)[..LABEL_HEX_LEN].to_string()
}

/// Return the session's stable subdomain label, allocating one on first use.
/// Reuses the existing row if present (so the URL is stable across
/// close/reopen); otherwise inserts the first candidate label that doesn't
/// collide with another session's, re-deriving with a counter on conflict.
pub(crate) fn ensure_subdomain_label(
    conn: &mut crate::db::DbConnection,
    session_id: Uuid,
) -> Result<String, AppError> {
    use crate::schema::forward_subdomains as fs;

    if let Some(existing) = fs::table
        .filter(fs::session_id.eq(session_id))
        .select(fs::label)
        .first::<String>(conn)
        .optional()?
    {
        return Ok(existing);
    }

    for attempt in 0u32..256 {
        let candidate = label_candidate(session_id, attempt);
        let inserted = diesel::insert_into(fs::table)
            .values(&NewForwardSubdomain {
                label: candidate.clone(),
                session_id,
            })
            .on_conflict_do_nothing()
            .execute(conn)?;
        if inserted == 1 {
            return Ok(candidate);
        }
        // Nothing inserted: the label collides with another session's, or this
        // session got a row concurrently. Re-check the session before trying
        // the next candidate.
        if let Some(existing) = fs::table
            .filter(fs::session_id.eq(session_id))
            .select(fs::label)
            .first::<String>(conn)
            .optional()?
        {
            return Ok(existing);
        }
    }
    Err(AppError::Internal(
        "could not allocate a forward subdomain".to_string(),
    ))
}

/// Map a subdomain label back to its session (the reverse-proxy Host route).
/// A label resolves via the admin custom-subdomain table first, then the auto
/// `forward_subdomains` LUT — both route to the same session. The two
/// namespaces can't collide (auto labels are always 8-hex; custom labels are
/// rejected if 8-hex), so order only matters for the lookup, not correctness.
pub(crate) fn session_for_label(
    conn: &mut crate::db::DbConnection,
    label: &str,
) -> Result<Uuid, AppError> {
    use crate::schema::{custom_subdomains as cs, forward_subdomains as fs};
    if let Some(session_id) = cs::table
        .filter(cs::label.eq(label))
        .select(cs::session_id)
        .first::<Uuid>(conn)
        .optional()?
    {
        return Ok(session_id);
    }
    fs::table
        .filter(fs::label.eq(label))
        .select(fs::session_id)
        .first::<Uuid>(conn)
        .optional()?
        .ok_or(AppError::NotFound("forward"))
}

/// The session's subdomain label, if one has been allocated.
fn existing_label(
    conn: &mut crate::db::DbConnection,
    session_id: Uuid,
) -> Result<Option<String>, AppError> {
    use crate::schema::forward_subdomains as fs;
    Ok(fs::table
        .filter(fs::session_id.eq(session_id))
        .select(fs::label)
        .first::<String>(conn)
        .optional()?)
}

/// The session's forwarded port, if a forward is active.
pub(crate) fn active_forward_port(
    conn: &mut crate::db::DbConnection,
    session_id: Uuid,
) -> Result<Option<u16>, AppError> {
    use crate::schema::session_forwards as sf;
    Ok(sf::table
        .filter(sf::session_id.eq(session_id))
        .select(sf::port)
        .first::<i32>(conn)
        .optional()?
        .map(|p| p as u16))
}

/// The session's forwarded `(port, public)`, if a forward is active. The
/// reverse proxy needs both: `public` decides whether to require auth.
pub(crate) fn active_forward(
    conn: &mut crate::db::DbConnection,
    session_id: Uuid,
) -> Result<Option<(u16, bool)>, AppError> {
    use crate::schema::session_forwards as sf;
    Ok(sf::table
        .filter(sf::session_id.eq(session_id))
        .select((sf::port, sf::public))
        .first::<(i32, bool)>(conn)
        .optional()?
        .map(|(port, public)| (port as u16, public)))
}

/// Authorize `user_id` as a member of `session_id` and return the session.
/// Read access — sufficient to *see* and *use* the forward, never to change it.
pub(crate) fn member_session(
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

/// Authorize the *owner* of `session_id` (registering/moving/revoking a forward
/// exposes or tears down a loopback port on the proxy host — a strictly tighter
/// gate than transcript read access; the CLI clears it because the launcher's
/// bearer token resolves to the owner) **and** take a `FOR UPDATE` row lock on
/// the session, so concurrent forward mutations for the same session serialize
/// (the DB row is the per-session lock). Must run inside a transaction.
fn lock_owned_session(
    conn: &mut crate::db::DbConnection,
    session_id: Uuid,
    user_id: Uuid,
) -> Result<Session, AppError> {
    use crate::schema::sessions;
    sessions::table
        .filter(sessions::id.eq(session_id))
        .filter(sessions::user_id.eq(user_id))
        .select(Session::as_select())
        .for_update()
        .first(conn)
        .optional()?
        .ok_or(AppError::NotFound("session"))
}

/// POST …/sessions/{id}/forwards — set the session's single forwarded port
/// (replacing any current one), sync the proxy's allowlist, and report the
/// probe-dial result plus any port that was replaced.
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
    if app_state.forward_domain.is_none() {
        return Err(AppError::ServiceUnavailable(
            "Forwarding is not configured on this server",
        ));
    }

    // All DB work in one transaction under a per-session row lock, so a
    // concurrent `forward`/`forward`/`close` for the same session serializes —
    // otherwise two racing calls could each read `previous_port` before the
    // other commits and skip the `ForwardClose(old)` that keeps the proxy
    // allowlist in step with the DB. Commit before any proxy I/O so the 3s
    // probe wait never holds the lock.
    let (session, row, label, replaced_port) = conn.transaction::<_, AppError, _>(|conn| {
        use crate::schema::session_forwards;
        let session = lock_owned_session(conn, session_id, user_id)?;
        let previous_port = active_forward_port(conn, session_id)?;
        let replaced_port = previous_port.filter(|p| *p != req.port);

        // At most one row per session: insert, or move the existing port.
        diesel::insert_into(session_forwards::table)
            .values(&NewSessionForward {
                session_id,
                port: req.port as i32,
            })
            .on_conflict(session_forwards::session_id)
            .do_update()
            .set(session_forwards::port.eq(req.port as i32))
            .execute(conn)?;
        // A port change points the forward at a *different* local service, so
        // any prior public opt-in must not transfer to it — reset to private.
        // (The agent can move the port with the owner-resolved token; public
        // exposure requires a fresh, explicit owner toggle.) Idempotent
        // same-port re-registration keeps the flag.
        if replaced_port.is_some() {
            diesel::update(
                session_forwards::table.filter(session_forwards::session_id.eq(session_id)),
            )
            .set(session_forwards::public.eq(false))
            .execute(conn)?;
        }
        let row: SessionForward = session_forwards::table
            .filter(session_forwards::session_id.eq(session_id))
            .select(SessionForward::as_select())
            .first(conn)?;
        let label = ensure_subdomain_label(conn, session_id)?;
        Ok((session, row, label, replaced_port))
    })?;

    // Drop the replaced port from the proxy's allowlist (and its live streams).
    if let Some(old) = replaced_port {
        app_state.session_manager.send_to_connected_session(
            &session.session_key,
            ServerToProxy::ForwardClose(ForwardPortFields { port: old }),
        );
    }

    // Sync the proxy's allowlist for the new port and wait (briefly) for its
    // probe verdict.
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
        "Forward set: session {} port {} (replaced: {:?}, listening: {:?})",
        session_id, req.port, replaced_port, listening
    );

    Ok(Json(CreateForwardResponse {
        forward: to_forward_info(&app_state, &row, &label)?,
        replaced_port,
        listening,
        probe_error,
    }))
}

/// GET …/sessions/{id}/forwards — the session's forward (0 or 1). Empty (not an
/// error) when forwarding is disabled, so session views don't 503 on load.
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
    let row = session_forwards::table
        .filter(session_forwards::session_id.eq(session_id))
        .select(SessionForward::as_select())
        .first::<SessionForward>(&mut conn)
        .optional()?;
    let forwards = match (row, existing_label(&mut conn, session_id)?) {
        (Some(row), Some(label)) => vec![to_forward_info(&app_state, &row, &label)?],
        _ => vec![],
    };
    Ok(Json(SessionForwardsResponse { forwards }))
}

/// DELETE …/sessions/{id}/forwards — revoke the session's forward (owner only).
/// Drops the row and tells the proxy to close the port and its live streams.
/// The subdomain label is kept so a re-forward reuses the same URL.
pub async fn delete_forward(
    State(app_state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<StatusCode, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;

    // Same per-session serialization as create_forward: lock the session row,
    // read + delete the forward atomically, commit, then do proxy I/O.
    let (session, port) = conn.transaction::<_, AppError, _>(|conn| {
        use crate::schema::session_forwards;
        let session = lock_owned_session(conn, session_id, user_id)?;
        let Some(port) = active_forward_port(conn, session_id)? else {
            return Err(AppError::NotFound("forward"));
        };
        diesel::delete(session_forwards::table.filter(session_forwards::session_id.eq(session_id)))
            .execute(conn)?;
        Ok((session, port))
    })?;

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

/// GET /api/forwards — the caller's active forwards across the sessions they
/// own, for the Settings ▸ Forwarding tab. Owner-scoped, since only the owner
/// can toggle a forward public. Empty (not an error) when forwarding is
/// disabled.
pub async fn list_user_forwards(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<shared::api::UserForwardsResponse>, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;

    if app_state.forward_domain.is_none() {
        return Ok(Json(shared::api::UserForwardsResponse { forwards: vec![] }));
    }

    use crate::schema::{forward_subdomains, session_forwards, sessions};
    let rows: Vec<(SessionForward, String, String)> = session_forwards::table
        .inner_join(sessions::table.on(sessions::id.eq(session_forwards::session_id)))
        .inner_join(
            forward_subdomains::table
                .on(forward_subdomains::session_id.eq(session_forwards::session_id)),
        )
        .filter(sessions::user_id.eq(user_id))
        .select((
            SessionForward::as_select(),
            sessions::session_name,
            forward_subdomains::label,
        ))
        .order(session_forwards::created_at.desc())
        .load(&mut conn)?;

    let forwards = rows
        .into_iter()
        .map(|(row, session_name, label)| {
            Ok(shared::api::UserForwardInfo {
                session_id: row.session_id,
                session_name,
                port: row.port as u16,
                url: forward_url(&app_state, &label)?,
                public: row.public,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(shared::api::UserForwardsResponse { forwards }))
}

/// PATCH /api/sessions/{id}/forwards/public — set the forward's public flag
/// (owner only). Public forwards serve without the token-handoff auth.
pub async fn set_forward_public(
    State(app_state): State<Arc<AppState>>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    cookies: Cookies,
    Json(req): Json<shared::api::SetForwardPublicRequest>,
) -> Result<StatusCode, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;

    // Serialize with create/delete via the same per-session row lock.
    let session = conn.transaction::<_, AppError, _>(|conn| {
        use crate::schema::session_forwards;
        let session = lock_owned_session(conn, session_id, user_id)?;
        let updated = diesel::update(
            session_forwards::table.filter(session_forwards::session_id.eq(session_id)),
        )
        .set(session_forwards::public.eq(req.public))
        .execute(conn)?;
        if updated == 0 {
            return Err(AppError::NotFound("forward"));
        }
        Ok(session)
    })?;

    app_state.session_manager.broadcast_to_web_clients(
        &session.session_key,
        ServerToClient::ForwardsChanged { session_id },
    );
    info!(
        "Forward visibility: session {} public={}",
        session_id, req.public
    );

    Ok(StatusCode::NO_CONTENT)
}
