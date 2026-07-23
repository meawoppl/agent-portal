//! Inter-agent messaging: list the caller's sessions and post a message into
//! one, delivered as an input turn to that session's agent.
//!
//! Auth accepts either a browser session cookie (the web page) or a `Bearer`
//! proxy token (programmatic/agent callers), and is scoped to a single user —
//! you can only see and message your own sessions.

use std::str::FromStr;
use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap},
    Json,
};
use diesel::prelude::*;
use tower_cookies::Cookies;
use tracing::{error, info};
use uuid::Uuid;

use shared::api::{
    AgentSessionInfo, AgentSessionsResponse, SendAgentMessageRequest, SendAgentMessageResponse,
    ShowMediaResponse,
};
use shared::media::MediaKind;
use shared::{AgentType, PortalContent, PortalMessage, ServerToClient, SessionStatus};

use crate::errors::AppError;
use crate::models::Session;
use crate::AppState;

/// Resolve the calling user from a `Bearer` proxy token if present, otherwise
/// from the browser session cookie.
pub(crate) fn resolve_user(
    app_state: &AppState,
    headers: &HeaderMap,
    cookies: &Cookies,
) -> Result<Uuid, AppError> {
    crate::auth::extract_user_id(app_state, Some(headers), cookies)
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

    use crate::schema::{pending_permission_requests, session_members, sessions};
    let rows: Vec<Session> = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(session_members::user_id.eq(user_id))
        .filter(sessions::status.ne(SessionStatus::Replaced.as_str()))
        .filter(sessions::scheduled_task_id.is_null())
        .select(Session::as_select())
        .order(sessions::last_activity.desc())
        .load(&mut conn)?;

    // One batched lookup for the "blocked on you" flag, not one per session.
    let session_ids: Vec<Uuid> = rows.iter().map(|s| s.id).collect();
    let awaiting: std::collections::HashSet<Uuid> = pending_permission_requests::table
        .filter(pending_permission_requests::session_id.eq_any(&session_ids))
        .select(pending_permission_requests::session_id)
        .distinct()
        .load::<Uuid>(&mut conn)?
        .into_iter()
        .collect();

    let sessions = rows
        .into_iter()
        .map(|s| AgentSessionInfo {
            id: s.id,
            awaiting_permission: awaiting.contains(&s.id),
            last_activity: s.last_activity.and_utc().to_rfc3339(),
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
    use crate::schema::{session_members, sessions};

    // Authorize: the caller must be a member of the target session.
    let session: Session = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(target_id))
        .filter(session_members::user_id.eq(user_id))
        .select(Session::as_select())
        .first(&mut conn)
        .map_err(|_| AppError::NotFound("session"))?;

    // Attribute the message so the recipient knows where it came from. Agent
    // senders get an explicit portal event payload; the proxy converts it to
    // agent-facing text, and the frontend renders the typed event directly.
    // The human web page sends no `from`, so fall back to a plain text portal
    // message with the sender display name in the prompt text.
    let content = match req.from.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(from) => {
            let sender_agent = from
                .parse::<Uuid>()
                .ok()
                .and_then(|id| {
                    sessions::table
                        .find(id)
                        .select(sessions::agent_type)
                        .first::<String>(&mut conn)
                        .ok()
                })
                .unwrap_or_else(|| "agent".to_string());
            shared::PortalMessage::agent_message(
                sender_agent,
                from.to_string(),
                message.to_string(),
            )
            .to_json()
        }
        None => serde_json::Value::String(format!(
            "[portal message from {}]\n{}",
            user_display_name(&mut conn, user_id),
            message
        )),
    };

    // Seq bump + best-effort persist + live delivery, shared with the web
    // input path (see SessionManager::enqueue_input). DB write faults are
    // logged, not fatal — the message still reaches a live agent.
    let outcome = app_state.session_manager.enqueue_input(
        &app_state.db_pool,
        &session.session_key,
        target_id,
        content,
        None,
        // Inter-agent sends have no browser to track delivery for.
        None,
    );

    info!(
        "Agent message: user {} -> session {} (seq {}, delivered={}, persisted={})",
        user_id, target_id, outcome.seq, outcome.delivered, outcome.persisted
    );

    let pending_inputs = pending_input_count(&mut conn, target_id).unwrap_or(0);

    Ok(Json(SendAgentMessageResponse {
        delivered: outcome.delivered,
        persisted: outcome.persisted,
        seq: outcome.seq,
        pending_inputs,
    }))
}

/// Query for `POST /api/agent/sessions/{id}/media`.
#[derive(serde::Deserialize)]
pub struct ShowMediaQuery {
    /// Original filename, shown in the transcript entry (e.g. `plot.png`).
    #[serde(default)]
    filename: Option<String>,
}

/// POST /api/agent/sessions/{id}/media — display an image or video in a
/// session's transcript (`agent-portal show <file>`). The raw file bytes are
/// the request body; the declared content type rides in the `Content-Type`
/// header and the original name in `?filename=`. Images go to the in-memory
/// [`ImageStore`](crate::handlers::images::ImageStore); videos go to the
/// on-disk [`MediaStore`](crate::handlers::media_store::MediaStore). A typed
/// `portal` message is persisted (so it replays on reconnect) and broadcast to
/// any live web clients.
///
/// Auth mirrors `send_agent_message`: dual cookie/Bearer, same-user, and the
/// caller must be a member of the target session.
pub async fn show_media(
    State(app_state): State<Arc<AppState>>,
    Path(target_id): Path<Uuid>,
    Query(query): Query<ShowMediaQuery>,
    headers: HeaderMap,
    cookies: Cookies,
    body: Bytes,
) -> Result<Json<ShowMediaResponse>, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;

    // Declared content type, minus any `; charset=` suffix.
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or(AppError::BadRequest("missing Content-Type header"))?;

    let kind = shared::media::media_kind(&content_type)
        .ok_or(AppError::BadRequest("unsupported media type"))?;

    if body.is_empty() {
        return Err(AppError::BadRequest("empty media body"));
    }

    // Per-kind size cap.
    let cap_mb = match kind {
        MediaKind::Image => app_state.max_image_mb,
        MediaKind::Video => app_state.max_video_mb,
    };
    if body.len() as u64 > cap_mb as u64 * 1024 * 1024 {
        return Err(AppError::PayloadTooLarge(format!(
            "{:.1} MB exceeds the {} MB limit for {}",
            body.len() as f64 / (1024.0 * 1024.0),
            cap_mb,
            match kind {
                MediaKind::Image => "images",
                MediaKind::Video => "videos",
            },
        )));
    }

    let mut conn = app_state.conn()?;
    use crate::schema::{session_members, sessions};

    // Authorize: the caller must be a member of the target session.
    let session: Session = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(target_id))
        .filter(session_members::user_id.eq(user_id))
        .select(Session::as_select())
        .first(&mut conn)
        .map_err(|_| AppError::NotFound("session"))?;

    let filename = query
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let file_size = body.len() as u64;

    // Store bytes; build the typed portal content referencing the served URL.
    // The media is bound to the caller + target session so `serve_image` /
    // `serve_media` gate fetches by ownership/membership (#786 pattern).
    let portal = match kind {
        MediaKind::Image => {
            let id = app_state.image_store.store_bytes(
                &content_type,
                body.to_vec(),
                user_id,
                Some(target_id),
            );
            PortalMessage::with_content(vec![PortalContent::Image {
                media_type: content_type.clone(),
                data: format!("/api/images/{id}"),
                file_path: filename.clone(),
                file_size: Some(file_size),
                source_type: Some("url".to_string()),
            }])
        }
        MediaKind::Video => {
            let id = app_state
                .media_store
                .store_bytes(&content_type, &body, user_id, Some(target_id))
                .map_err(|e| AppError::Internal(format!("store video: {e}")))?;
            PortalMessage::video_with_info(
                content_type.clone(),
                format!("/api/media/{id}"),
                filename.clone(),
                Some(file_size),
            )
        }
    };

    let content_json = portal.to_json();
    let agent_type = AgentType::from_str(&session.agent_type).unwrap_or_default();

    // Persist the transcript row (durability + reconnect replay). Broadcast is
    // best-effort; persistence is the guarantee.
    let mut persisted = false;
    let mut meta: Option<shared::PortalMeta> = None;
    {
        use crate::schema::messages;
        let new_message = crate::models::NewMessage {
            session_id: target_id,
            role: shared::MessageRole::Portal.to_string(),
            content: content_json.to_string(),
            user_id: session.user_id,
            agent_type: session.agent_type.clone(),
            provenance_kind: None,
            provenance_session_id: None,
            provenance_agent_type: None,
        };
        match diesel::insert_into(messages::table)
            .values(&new_message)
            .get_result::<crate::models::Message>(&mut conn)
        {
            Ok(inserted) => {
                persisted = true;
                meta = Some(inserted.portal_meta(None));
            }
            Err(e) => error!("Failed to persist show-media message: {}", e),
        }
    }

    app_state.session_manager.broadcast_to_web_clients(
        &session.session_key,
        ServerToClient::AgentOutput {
            content: content_json,
            agent_type,
            meta,
        },
    );

    info!(
        "show_media: user {} -> session {} ({}, {} bytes, persisted={})",
        user_id, target_id, content_type, file_size, persisted
    );

    Ok(Json(ShowMediaResponse {
        session_name: session.session_name,
        content_type,
        persisted,
    }))
}

fn pending_input_count(
    conn: &mut crate::db::DbConnection,
    session_id: Uuid,
) -> Result<usize, diesel::result::Error> {
    use crate::schema::pending_inputs;
    let count: i64 = pending_inputs::table
        .filter(pending_inputs::session_id.eq(session_id))
        .count()
        .get_result(conn)?;
    Ok(count.max(0) as usize)
}
