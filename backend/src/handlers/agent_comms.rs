//! Inter-agent messaging: list the caller's sessions and post a message into
//! one, delivered as an input turn to that session's agent.
//!
//! Auth accepts either a browser session cookie (the web page) or a `Bearer`
//! proxy token (programmatic/agent callers), and is scoped to a single user —
//! you can only see and message your own sessions.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    Json,
};
use diesel::prelude::*;
use tower_cookies::Cookies;
use tracing::{error, info};
use uuid::Uuid;

use shared::api::{
    AgentSessionInfo, AgentSessionsResponse, SendAgentMessageRequest, SendAgentMessageResponse,
};
use shared::ServerToProxy;

use crate::errors::AppError;
use crate::models::{NewPendingInput, Session};
use crate::AppState;

/// Resolve the calling user from a `Bearer` proxy token if present, otherwise
/// from the browser session cookie.
fn resolve_user(
    app_state: &AppState,
    headers: &HeaderMap,
    cookies: &Cookies,
) -> Result<Uuid, AppError> {
    if let Some(token) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
    {
        let mut conn = app_state.conn()?;
        let (user_id, _email) =
            crate::handlers::proxy_tokens::verify_and_get_user(app_state, &mut conn, token)?;
        return Ok(user_id);
    }
    crate::auth::extract_user_id(app_state, cookies)
}

/// Look up a display name for `user_id` (name, falling back to email).
fn user_display_name(conn: &mut crate::db::DbConnection, user_id: Uuid) -> String {
    use crate::schema::users;
    users::table
        .find(user_id)
        .select(users::name)
        .first::<Option<String>>(conn)
        .ok()
        .flatten()
        .or_else(|| {
            users::table
                .find(user_id)
                .select(users::email)
                .first::<String>(conn)
                .ok()
        })
        .unwrap_or_else(|| "portal".to_string())
}

/// GET /api/agent/sessions — the caller's sessions, for picking a recipient.
/// Excludes replaced rows and scheduled-task sessions.
pub async fn list_agent_sessions(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<AgentSessionsResponse>, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;

    use crate::schema::{session_members, sessions};
    let rows: Vec<Session> = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(session_members::user_id.eq(user_id))
        .filter(sessions::status.ne("replaced"))
        .filter(sessions::scheduled_task_id.is_null())
        .select(Session::as_select())
        .order(sessions::last_activity.desc())
        .load(&mut conn)?;

    let sessions = rows
        .into_iter()
        .map(|s| AgentSessionInfo {
            id: s.id,
            session_name: s.session_name,
            working_directory: s.working_directory,
            agent_type: s.agent_type,
            status: s.status,
            hostname: s.hostname,
        })
        .collect();

    Ok(Json(AgentSessionsResponse { sessions }))
}

/// POST /api/agent/sessions/{id}/message — inject a message into a session as
/// an input turn (same pipeline as a user typing in the web client).
pub async fn send_agent_message(
    State(app_state): State<Arc<AppState>>,
    Path(target_id): Path<Uuid>,
    headers: HeaderMap,
    cookies: Cookies,
    Json(req): Json<SendAgentMessageRequest>,
) -> Result<Json<SendAgentMessageResponse>, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;
    let message = req.message.trim();
    if message.is_empty() {
        return Err(AppError::BadRequest("message is empty"));
    }

    let mut conn = app_state.conn()?;
    use crate::schema::{pending_inputs, session_members, sessions};

    // Authorize: the caller must be a member of the target session.
    let session: Session = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(target_id))
        .filter(session_members::user_id.eq(user_id))
        .select(Session::as_select())
        .first(&mut conn)
        .map_err(|_| AppError::NotFound("session"))?;

    // Attribute the message so the recipient knows where it came from. An
    // agent sender supplies its own session id (`from`); the human web page
    // doesn't, so fall back to the sending user's display name.
    let bumper = match req.from.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(from) => format!("[message from agent {from}]"),
        None => format!(
            "[portal message from {}]",
            user_display_name(&mut conn, user_id)
        ),
    };
    let content = serde_json::Value::String(format!("{bumper}\n{message}"));

    // Mirror the WS input path's *tolerant* handling (see `handle_web_input`):
    // the seq bump and the pending-input row are best-effort — that path
    // `.unwrap_or`s the update and only logs an INSERT failure, then still
    // delivers live. Propagating these as `?` is what turned a latent input-
    // pipeline write error (one the WS path silently swallows) into a 500 on
    // every send. Delivering to the live proxy is what actually matters.
    let seq: i64 = match diesel::update(sessions::table.find(target_id))
        .set(sessions::input_seq.eq(sessions::input_seq + 1))
        .returning(sessions::input_seq)
        .get_result(&mut conn)
    {
        Ok(seq) => seq,
        Err(e) => {
            error!("INPUT_SEQ_BUMP_FAILED session={}: {}", target_id, e);
            1
        }
    };

    let new_input = NewPendingInput {
        session_id: target_id,
        seq_num: seq,
        content: serde_json::to_string(&content).unwrap_or_default(),
        send_mode: None,
    };
    if let Err(e) = diesel::insert_into(pending_inputs::table)
        .values(&new_input)
        .execute(&mut conn)
    {
        // Same stable marker as the WS input path so one alert rule catches
        // both. See CLAUDE.md "Schema-drift alerting".
        error!(
            "PENDING_INPUT_PERSIST_FAILED session={} (delivered live): {}",
            target_id, e
        );
    }

    let delivered = app_state.session_manager.send_to_session(
        &session.session_key,
        ServerToProxy::SequencedInput {
            session_id: target_id,
            seq,
            content,
            send_mode: None,
        },
    );

    info!(
        "Agent message: user {} -> session {} (seq {}, live={})",
        user_id, target_id, seq, delivered
    );

    Ok(Json(SendAgentMessageResponse { delivered, seq }))
}
