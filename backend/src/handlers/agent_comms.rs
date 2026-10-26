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
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use diesel::prelude::*;
use tower_cookies::Cookies;
use tracing::{error, info};
use uuid::Uuid;

use shared::api::{
    AgentProgressRequest, AgentSessionInfo, AgentSessionsResponse, ProgressBar,
    SendAgentMessageRequest, SendAgentMessageResponse, SessionActivityState, ShowMediaResponse,
    MAX_PROGRESS_ID_LEN, MAX_PROGRESS_LABEL_LEN,
};
use shared::media::MediaKind;
use shared::{AgentType, PortalContent, PortalMessage, ServerToClient, SessionStatus};

use crate::errors::AppError;
use crate::handlers::helpers::request_content_type;
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

/// Raw rows a peek reads before condensing thinking bursts. The request counts
/// condensed entries, and one burst can be hundreds of markers, so the read is
/// a fixed budget rather than a multiple of the limit: a tool call followed by
/// a long burst still shows up. Older history is `message history`'s job.
const PEEK_RAW_ROWS: i64 = 400;

/// Look up a display name for `user_id`, keeping this path's `"portal"`
/// fallback for unknown users. Resolution itself lives in
/// [`crate::handlers::helpers::user_display_name`] (single query,
/// nickname-then-name-then-email).
fn user_display_name(conn: &mut crate::db::DbConnection, user_id: Uuid) -> String {
    crate::handlers::helpers::user_display_name(conn, user_id)
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

    let states =
        super::session_activity::session_activity_states(&app_state, &mut conn, &session_ids)?;

    let sessions = rows
        .into_iter()
        .map(|s| {
            let connected = app_state
                .session_manager
                .is_proxy_connected(s.id.to_string().as_str());
            let state = states
                .get(&s.id)
                .copied()
                .unwrap_or(SessionActivityState::Idle);
            AgentSessionInfo {
                connected: Some(connected),
                state: Some(state),
                busy: Some(state.is_active()),
                id: s.id,
                awaiting_permission: awaiting.contains(&s.id),
                last_activity: s.last_activity.and_utc().to_rfc3339(),
                session_name: s.session_name,
                working_directory: s.working_directory,
                agent_type: s.agent_type,
                status: s.status,
                hostname: s.hostname,
                model: s.last_model,
            }
        })
        .collect();

    Ok(Json(AgentSessionsResponse { sessions }))
}

/// Query for `GET /api/agent/sessions/{id}/messages`.
#[derive(serde::Deserialize)]
pub struct PeekQuery {
    /// Max messages to return; clamped to 1..=50, default 10.
    #[serde(default)]
    limit: Option<i64>,
    /// Only messages strictly newer than this RFC3339 timestamp.
    #[serde(default)]
    since: Option<String>,
}

/// GET /api/agent/sessions/{id}/messages — a read-only glance at a peer
/// session's recent activity (#1406, `agent-portal message peek`).
///
/// Summarization happens server-side ([`super::peek_summary`]) because the
/// consumer is an *agent context window*: each message becomes one capped
/// line, so the worst case is ~50 short lines, never a raw transcript dump.
pub async fn peek_agent_messages(
    State(app_state): State<Arc<AppState>>,
    Path(target_id): Path<Uuid>,
    Query(query): Query<PeekQuery>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<shared::api::PeekMessagesResponse>, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;
    use crate::schema::{messages, pending_permission_requests, session_members, sessions};

    // Authorize: the caller must be a member of the target session.
    let session: Session = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(target_id))
        .filter(session_members::user_id.eq(user_id))
        .select(Session::as_select())
        .first(&mut conn)
        .map_err(|_| AppError::NotFound("session"))?;

    let limit = query.limit.unwrap_or(10).clamp(1, 50);
    let since = query
        .since
        .as_deref()
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.naive_utc())
                .map_err(|_| AppError::BadRequest("since must be an RFC3339 timestamp"))
        })
        .transpose()?;

    let mut rows_query = messages::table
        .filter(messages::session_id.eq(target_id))
        .into_boxed();
    if let Some(since) = since {
        rows_query = rows_query.filter(messages::created_at.gt(since));
    }
    // `limit` counts condensed entries, so read the fixed raw budget.
    let rows: Vec<crate::models::Message> = rows_query
        .order(messages::created_at.desc())
        .limit(PEEK_RAW_ROWS)
        .load(&mut conn)?;

    let total_messages: i64 = messages::table
        .filter(messages::session_id.eq(target_id))
        .count()
        .get_result(&mut conn)?;

    // Newest-first from the query; the wire contract is oldest → newest.
    let summarized = rows
        .into_iter()
        .rev()
        .map(|m| {
            let estimate = super::peek_summary::thinking_estimate(&m.agent_type, &m.content);
            let message = super::peek_summary::summarize_message(
                m.id,
                &m.agent_type,
                &m.role,
                &m.content,
                m.created_at,
            );
            (message, estimate)
        })
        .collect();
    let peek_messages = super::peek_summary::newest_entries(summarized, limit as usize);

    let connected = app_state
        .session_manager
        .is_proxy_connected(&target_id.to_string());
    // Status header uses the same meaningful signals as Computers and list.
    let state =
        super::session_activity::session_activity_states(&app_state, &mut conn, &[target_id])?
            .get(&target_id)
            .copied()
            .unwrap_or(SessionActivityState::Idle);
    let pending_tool_name = pending_permission_requests::table
        .filter(pending_permission_requests::session_id.eq(target_id))
        .order(pending_permission_requests::created_at.desc())
        .select(pending_permission_requests::tool_name)
        .first::<String>(&mut conn)
        .optional()?;
    let awaiting_permission = pending_tool_name.is_some();

    Ok(Json(shared::api::PeekMessagesResponse {
        session: AgentSessionInfo {
            connected: Some(connected),
            state: Some(state),
            busy: Some(state.is_active()),
            id: session.id,
            awaiting_permission,
            last_activity: session.last_activity.and_utc().to_rfc3339(),
            session_name: session.session_name,
            working_directory: session.working_directory,
            agent_type: session.agent_type,
            status: session.status,
            hostname: session.hostname,
            model: session.last_model,
        },
        pending_tool_name,
        messages: peek_messages,
        total_messages,
    }))
}

/// GET /api/agent/sessions/{id}/history — the caller-visible session's full
/// known transcript as canonical NDJSON.
///
/// Archived rows and newer hot-DB rows are unioned by message UUID using the
/// same merge primitive as the archive sweep. That makes a download useful
/// while a session is still active and prevents retention from erasing rows
/// that already reached the archive. `X-Portal-History-Complete` is true only
/// when a durable transcript archive exists; without one we return the
/// retained DB rows but cannot prove that an older retention trim never ran.
#[derive(serde::Deserialize, Default)]
pub struct HistoryQuery {
    pub since: Option<String>,
    pub tail: Option<usize>,
}

pub async fn download_agent_history(
    State(app_state): State<Arc<AppState>>,
    Path(target_id): Path<Uuid>,
    Query(query): Query<HistoryQuery>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Response, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;
    let mut conn = app_state.conn()?;
    use crate::schema::{messages, session_members, sessions};

    let session: Session = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(target_id))
        .filter(session_members::user_id.eq(user_id))
        .select(Session::as_select())
        .first(&mut conn)
        .map_err(|_| AppError::NotFound("session"))?;

    let history_window = if let Some(since) = query.since {
        use crate::schema::{session_follow_heads, session_follow_messages};
        let after = shared::api::parse_follow_cursor(&since, target_id).ok_or(
            AppError::BadRequest("invalid or cross-session follow cursor"),
        )?;
        let head = session_follow_heads::table
            .find(target_id)
            .select(session_follow_heads::sequence)
            .first::<i64>(&mut conn)
            .optional()?
            .unwrap_or(0);
        if after > head {
            return Err(AppError::Conflict("follow cursor is ahead of this session"));
        }
        let ids: Vec<Uuid> = session_follow_messages::table
            .filter(session_follow_messages::session_id.eq(target_id))
            .filter(session_follow_messages::sequence.gt(after))
            .filter(session_follow_messages::sequence.le(head))
            .order(session_follow_messages::sequence.asc())
            .select(session_follow_messages::message_id)
            .load(&mut conn)?;
        Some((head, ids))
    } else {
        None
    };

    type MessageRow = (Uuid, String, String, chrono::NaiveDateTime, String);
    let rows: Vec<MessageRow> = messages::table
        .filter(messages::session_id.eq(target_id))
        .order(messages::created_at.asc())
        .select((
            messages::id,
            messages::role,
            messages::content,
            messages::created_at,
            messages::agent_type,
        ))
        .load(&mut conn)?;
    let current = rows
        .into_iter()
        .map(
            |(id, role, content, created_at, agent_type)| archive_format::ArchiveMessageLine {
                id,
                role,
                created_at,
                agent_type,
                content: serde_json::from_str(&content)
                    .unwrap_or(serde_json::Value::String(content)),
            },
        )
        .collect();

    let (archived, complete) = if let Some(runtime) = app_state.archive.clone() {
        let owner_id = session.user_id;
        let archived = super::history::on_blocking(move || {
            runtime.store.read_transcript_lines(owner_id, target_id)
        })
        .await?;
        let complete = archived.is_some();
        (archived.unwrap_or_default(), complete)
    } else {
        (Vec::new(), false)
    };
    let mut merged = archive_format::merge_transcript_lines(archived, current);
    if let Some((_, ids)) = &history_window {
        let mut by_id: std::collections::HashMap<_, _> =
            merged.into_iter().map(|line| (line.id, line)).collect();
        merged = ids
            .iter()
            .map(|id| {
                by_id.remove(id).ok_or(AppError::Conflict(
                    "history gap: a body is absent from both retained transcript and archive",
                ))
            })
            .collect::<Result<_, _>>()?;
    }
    if let Some(tail) = query.tail {
        merged.drain(..merged.len().saturating_sub(tail));
    }

    let mut body = Vec::new();
    for line in &merged {
        serde_json::to_writer(&mut body, line)
            .map_err(|e| AppError::Internal(format!("could not serialize transcript: {e}")))?;
        body.push(b'\n');
    }

    let mut response = body.into_response();
    let response_headers = response.headers_mut();
    if let Some((head, _)) = history_window {
        response_headers.insert(
            "x-portal-history-cursor",
            HeaderValue::from_str(&shared::api::follow_cursor(target_id, head))
                .map_err(|e| AppError::Internal(e.to_string()))?,
        );
    }

    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    response_headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "attachment; filename=\"{target_id}.history.ndjson\""
        ))
        .map_err(|e| AppError::Internal(format!("invalid history filename header: {e}")))?,
    );
    response_headers.insert(
        "x-portal-history-complete",
        HeaderValue::from_static(if complete { "true" } else { "false" }),
    );
    response_headers.insert(
        "x-portal-history-messages",
        HeaderValue::from_str(&merged.len().to_string())
            .map_err(|e| AppError::Internal(format!("invalid history count header: {e}")))?,
    );
    Ok(response)
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
    let content = match shared::strings::trimmed_non_blank(req.from.as_deref()) {
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
        crate::handlers::websocket::EnqueueInput {
            content,
            send_mode: None,
            reasoning_effort: None,
            // Inter-agent sends have no browser to track delivery for.
            client_msg_id: None,
        },
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

/// POST /api/agent/sessions/{id}/media — display media in a
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
    let content_type = request_content_type(&headers)?.to_string();

    let kind = shared::media::media_kind(&content_type)
        .ok_or(AppError::BadRequest("unsupported media type"))?;

    if body.is_empty() {
        return Err(AppError::BadRequest("empty media body"));
    }

    // Per-kind size cap.
    let cap_bytes = match kind {
        MediaKind::Image => app_state.max_image_mb as usize * 1024 * 1024,
        MediaKind::Video => app_state.max_video_mb as usize * 1024 * 1024,
    };
    if body.len() > cap_bytes {
        return Err(AppError::PayloadTooLarge(format!(
            "{:.1} MB exceeds the {} MB transport limit for {}",
            body.len() as f64 / (1024.0 * 1024.0),
            cap_bytes / (1024 * 1024),
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

    let filename =
        shared::strings::trimmed_non_blank(query.filename.as_deref()).map(str::to_string);
    let file_size = body.len() as u64;

    // Store bytes; build the typed portal content referencing the served URL.
    // The media is bound to the caller + target session so `serve_image` /
    // `serve_media` gate fetches by ownership/membership (#786 pattern).
    let (portal, media_id) = match kind {
        MediaKind::Image => {
            let id = app_state.image_store.store_bytes(
                &content_type,
                body.to_vec(),
                user_id,
                Some(target_id),
            );
            (
                PortalMessage::with_content(vec![PortalContent::Image {
                    media_type: content_type.clone(),
                    data: format!("/api/images/{id}"),
                    file_path: filename.clone(),
                    file_size: Some(file_size),
                    source_type: Some("url".to_string()),
                }]),
                id,
            )
        }
        MediaKind::Video => {
            let id = app_state
                .media_store
                .store_bytes(&content_type, &body, user_id, Some(target_id))
                .map_err(|e| AppError::Internal(format!("store video: {e}")))?;
            (
                PortalMessage::video_with_info(
                    content_type.clone(),
                    format!("/api/media/{id}"),
                    filename.clone(),
                    Some(file_size),
                ),
                id,
            )
        }
    };

    // Write-through to the durable archive (best-effort, never fails the
    // upload). The served stores above are TTL/size-bounded, so without this
    // the archived transcript would show only a "media expired" placeholder
    // once the blob is evicted. Media is keyed under the session *owner* to
    // match the manifest/transcript layout. Gated by PORTAL_SESSION_ARCHIVE_MEDIA.
    if let Some(runtime) = &app_state.archive {
        if runtime.config.media {
            let runtime = runtime.clone();
            let media = crate::handlers::media_archive::MediaWriteThrough {
                owner_user_id: session.user_id,
                session_id: target_id,
                media_id,
                kind,
                content_type: content_type.clone(),
                filename: filename.clone(),
                bytes: body.to_vec(),
            };
            tokio::task::spawn_blocking(move || {
                crate::handlers::media_archive::write_through(&runtime, media);
            });
        }
    }

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

/// `POST /api/agent/sessions/{id}/progress` — `agent-portal progress`. Creates,
/// updates, or clears one of the session's live progress bars and pushes the
/// resulting set to its web clients. Restricted to owners/editors: a viewer
/// must not be able to forge or clear another user's session UI. Nothing is persisted: bars are live
/// status, held in memory by the session manager.
pub async fn post_agent_progress(
    State(app_state): State<Arc<AppState>>,
    Path(target_id): Path<Uuid>,
    headers: HeaderMap,
    cookies: Cookies,
    Json(req): Json<AgentProgressRequest>,
) -> Result<StatusCode, AppError> {
    let user_id = resolve_user(&app_state, &headers, &cookies)?;

    let id = shared::strings::trimmed_non_blank(Some(&req.id))
        .ok_or(AppError::BadRequest("progress id must not be blank"))?
        .to_string();
    if id.len() > MAX_PROGRESS_ID_LEN {
        return Err(AppError::BadRequest("progress id is too long"));
    }
    let label = shared::strings::trimmed_non_blank(req.label.as_deref()).map(str::to_string);
    if label
        .as_ref()
        .is_some_and(|l| l.len() > MAX_PROGRESS_LABEL_LEN)
    {
        return Err(AppError::BadRequest("progress label is too long"));
    }
    if req.fraction.is_some_and(|f| !f.is_finite()) {
        return Err(AppError::BadRequest("progress fraction must be finite"));
    }

    let mut conn = app_state.conn()?;
    crate::handlers::session_access::verify_session_mutator(&mut conn, target_id, user_id)?;
    // The store remembers who to tell about the rail pill; it has no DB of its
    // own for the sweep to look members up with.
    let members: Vec<Uuid> = crate::schema::session_members::table
        .filter(crate::schema::session_members::session_id.eq(target_id))
        .select(crate::schema::session_members::user_id)
        .load(&mut conn)?;
    drop(conn);

    let manager = &app_state.session_manager;
    if req.clear {
        manager.clear_agent_progress(target_id, members, &id);
    } else {
        let bar = ProgressBar {
            id,
            label,
            fraction: req.fraction.map(|f| f.clamp(0.0, 1.0)),
        };
        manager
            .set_agent_progress(target_id, members, bar)
            .map_err(|_| AppError::Conflict("too many progress bars; clear one first"))?;
    }
    Ok(StatusCode::NO_CONTENT)
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
