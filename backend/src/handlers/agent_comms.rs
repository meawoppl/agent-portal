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

use base64::Engine as _;
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

    use crate::schema::{messages, pending_permission_requests, session_members, sessions};
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

    // One row per session: the newest event capable of changing turn state.
    // Portal/system chatter is deliberately excluded so a reconnect notice or
    // heartbeat cannot turn an otherwise-busy session idle. PostgreSQL's
    // DISTINCT ON keeps this a single batched query rather than N+1 lookups.
    let latest_signals: std::collections::HashMap<Uuid, (String, String)> = messages::table
        .filter(messages::session_id.eq_any(&session_ids))
        .filter(messages::role.eq_any(["user", "assistant", "result", "unknown", "error"]))
        .distinct_on(messages::session_id)
        .select((
            messages::session_id,
            messages::agent_type,
            messages::content,
        ))
        .order((messages::session_id, messages::created_at.desc()))
        .load::<(Uuid, String, String)>(&mut conn)?
        .into_iter()
        .map(|(id, agent_type, content)| (id, (agent_type, content)))
        .collect();

    let sessions = rows
        .into_iter()
        .map(|s| {
            let connected = app_state
                .session_manager
                .sessions
                .contains_key(s.id.to_string().as_str());
            let busy = connected
                && latest_signals
                    .get(&s.id)
                    .is_some_and(|(agent_type, content)| turn_signal_is_busy(agent_type, content));
            AgentSessionInfo {
                connected: Some(connected),
                busy: Some(busy),
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

/// Interpret the latest significant durable event as turn state. The wire
/// protocols have different terminal vocabulary, but all three expose typed
/// or stable top-level discriminators; malformed future frames fail safe to
/// "busy" while connected instead of advertising an agent as idle mid-turn.
fn turn_signal_is_busy(agent_type: &str, content: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(content) else {
        return true;
    };
    let kind = value.get("type").and_then(|value| value.as_str());
    match agent_type {
        "claude" => kind != Some("result") && kind != Some("error"),
        "codex" => !matches!(
            kind,
            Some("thread.started" | "turn.completed" | "turn.failed" | "error")
        ),
        "muse" => !value
            .get("payload_type")
            .and_then(|value| value.as_str())
            .is_some_and(|kind| kind.starts_with("run.terminal.")),
        _ => !matches!(
            kind,
            Some("result" | "turn.completed" | "turn.failed" | "error")
        ),
    }
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
    let mut content_type = headers
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
    let cap_bytes = match kind {
        MediaKind::Image => app_state.max_image_mb as usize * 1024 * 1024,
        MediaKind::Video => app_state.max_video_mb as usize * 1024 * 1024,
        MediaKind::Figure if content_type == shared::media::PORTABLE_FIGURE_HTML_TYPE => {
            shared::media::PORTABLE_FIGURE_HTML_MAX_BYTES
        }
        MediaKind::Figure => shared::media::PORTABLE_FIGURE_MAX_BYTES,
    };
    if body.len() > cap_bytes {
        return Err(AppError::PayloadTooLarge(format!(
            "{:.1} MB exceeds the {} MB transport limit for {}",
            body.len() as f64 / (1024.0 * 1024.0),
            cap_bytes / (1024 * 1024),
            match kind {
                MediaKind::Image => "images",
                MediaKind::Video => "videos",
                MediaKind::Figure => "portable figures",
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

    let mut filename = query
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut body = body;
    if content_type == shared::media::PORTABLE_FIGURE_HTML_TYPE {
        (body, filename) = unwrap_portable_figure_html(body, filename)?;
        content_type = shared::media::PORTABLE_FIGURE_TYPE.to_string();
    }
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
        MediaKind::Figure => {
            let limits = portable_figure_limits();
            let metadata = rizzma::portable::inspect(&body, &limits)
                .map_err(|_| AppError::BadRequest("invalid portable figure"))?;
            let meta = metadata.meta.as_ref().ok_or(AppError::BadRequest(
                "portable figure lacks display metadata",
            ))?;
            let poster_base64 = metadata
                .poster(&body)
                .map(|poster| base64::engine::general_purpose::STANDARD.encode(poster));
            let (controls, controls_unsupported) = portable_figure_controls(&metadata.controls);
            let id = app_state
                .media_store
                .store_bytes(&content_type, &body, user_id, Some(target_id))
                .map_err(|e| AppError::Internal(format!("store portable figure: {e}")))?;
            (
                PortalMessage::with_content(vec![PortalContent::Figure {
                    media_type: content_type.clone(),
                    data: format!("/api/media/{id}"),
                    file_path: filename.clone(),
                    file_size: Some(file_size),
                    schema: metadata.schema,
                    renderer_version: metadata.renderer.version.clone(),
                    width_px: meta.width_px,
                    height_px: meta.height_px,
                    title: meta.title.clone(),
                    alt: meta.alt.clone(),
                    poster_base64,
                    animated: meta.animated,
                    duration: meta.duration,
                    controls,
                    controls_unsupported,
                }]),
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

fn portable_figure_limits() -> rizzma::portable::Limits {
    let mut limits = rizzma::portable::Limits::new();
    limits.max_total_bytes = shared::media::PORTABLE_FIGURE_MAX_BYTES;
    // The poster is persisted in the transcript for durable fallback; keep
    // that row bounded independently of the canonical artifact cap.
    limits.max_poster_bytes = 1024 * 1024;
    // Keep Rizzma's finite parser-safety control bounds here. The tighter DOM
    // policy below intentionally runs after inspection so a figure with a
    // safe-but-too-large manifest can still degrade to its honest poster.
    limits
}

fn portable_figure_controls(
    controls: &[rizzma::portable::ControlRef],
) -> (Vec<shared::PortableFigureControl>, bool) {
    if controls.len() > shared::media::PORTABLE_FIGURE_MAX_CONTROLS {
        return (Vec::new(), true);
    }
    let mapped = controls
        .iter()
        .map(|control| {
            (control.label.len() <= shared::media::PORTABLE_FIGURE_MAX_CONTROL_LABEL_BYTES).then(
                || shared::PortableFigureControl {
                    label: control.label.clone(),
                    min: control.min,
                    max: control.max,
                    default: control.default,
                    step: control.step,
                },
            )
        })
        .collect::<Option<Vec<_>>>();
    mapped.map_or_else(|| (Vec::new(), true), |controls| (controls, false))
}

/// Strip the reversible HTML carrier at the trust boundary. Only canonical
/// raw artifact bytes continue to validation, storage, archive write-through,
/// and transcript persistence; wrapper HTML and embedded runtimes die here.
fn unwrap_portable_figure_html(
    body: Bytes,
    filename: Option<String>,
) -> Result<(Bytes, Option<String>), AppError> {
    let artifact = rizzma::portable::unwrap_html(&body, &portable_figure_limits())
        .map_err(|_| AppError::BadRequest("invalid portable-figure HTML wrapper"))?;
    Ok((
        Bytes::from(artifact),
        filename.map(canonical_figure_filename),
    ))
}

fn canonical_figure_filename(filename: String) -> String {
    if filename.to_ascii_lowercase().ends_with(".riz.html") {
        filename[..filename.len() - ".html".len()].to_string()
    } else {
        filename
    }
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

#[cfg(test)]
mod tests {
    use super::{portable_figure_controls, turn_signal_is_busy, unwrap_portable_figure_html};
    use axum::body::Bytes;

    #[test]
    fn riz_html_is_canonicalized_before_storage() {
        let marker = shared::media::RIZZMA_HTML_CARRIER_OPEN;
        let html = format!(
            "<!doctype html>{marker}UlpGRw==</script>\
             <script id=\"riz-rt-loader\">discard me</script>"
        );
        let (bytes, filename) =
            unwrap_portable_figure_html(Bytes::from(html), Some("Demo.RIZ.HTML".to_string()))
                .expect("valid carrier");
        assert_eq!(bytes.as_ref(), b"RZFG");
        assert_eq!(filename.as_deref(), Some("Demo.RIZ"));
    }

    #[test]
    fn portable_control_manifest_is_typed_bounded_and_ordered() {
        let controls = vec![
            rizzma::portable::ControlRef {
                label: "wavelength".to_string(),
                min: 0.6,
                max: 3.0,
                default: 1.5,
                step: Some(0.1),
            },
            rizzma::portable::ControlRef {
                label: "width".to_string(),
                min: 0.3,
                max: 2.5,
                default: 0.8,
                step: None,
            },
        ];
        let (mapped, unsupported) = portable_figure_controls(&controls);
        assert!(!unsupported);
        assert_eq!(mapped[0].label, "wavelength");
        assert_eq!(mapped[0].step, Some(0.1));
        assert_eq!(mapped[1].label, "width");

        let excessive = vec![controls[0].clone(); shared::media::PORTABLE_FIGURE_MAX_CONTROLS + 1];
        let (mapped, unsupported) = portable_figure_controls(&excessive);
        assert!(mapped.is_empty());
        assert!(unsupported);

        let mut overlong = controls;
        overlong[0].label = "x".repeat(shared::media::PORTABLE_FIGURE_MAX_CONTROL_LABEL_BYTES + 1);
        let (mapped, unsupported) = portable_figure_controls(&overlong);
        assert!(mapped.is_empty());
        assert!(unsupported);
    }

    #[test]
    fn turn_state_covers_all_agent_terminal_shapes() {
        assert!(turn_signal_is_busy("claude", r#"{"type":"assistant"}"#));
        assert!(!turn_signal_is_busy("claude", r#"{"type":"result"}"#));
        assert!(turn_signal_is_busy("codex", r#"{"type":"item.started"}"#));
        assert!(!turn_signal_is_busy(
            "codex",
            r#"{"type":"thread.started"}"#
        ));
        assert!(!turn_signal_is_busy(
            "codex",
            r#"{"type":"turn.completed"}"#
        ));
        assert!(turn_signal_is_busy(
            "muse",
            r#"{"type":"muse_record","payload_type":"tool.result"}"#
        ));
        assert!(!turn_signal_is_busy(
            "muse",
            r#"{"type":"muse_record","payload_type":"run.terminal.completed"}"#
        ));
    }

    #[test]
    fn malformed_in_progress_signal_fails_busy() {
        assert!(turn_signal_is_busy("codex", "not-json"));
    }
}
