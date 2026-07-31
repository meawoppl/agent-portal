//! Authenticated session-history endpoints over the long-term archive.
//!
//! The archive outlives the hot DB rows (retention deletes sessions after
//! `SESSION_MAX_AGE_DAYS`), so this is the portal's durable "history browser"
//! backend. Visibility is enforced server-side on every endpoint:
//!
//! * **admins** see every archived session,
//! * **owners** see sessions archived under their own user id,
//! * **shared-with-me** comes from the manifest's `members` snapshot
//!   (captured from `session_members` at archive time), unioned with any
//!   still-live `session_members` row so a share granted after the final
//!   archive still works while the hot row exists.
//!
//! Non-visible sessions 404 rather than 403, mirroring
//! [`verify_session_reader`](crate::handlers::session_access::verify_session_reader)'s
//! no-existence-leak policy.
//!
//! The archive store's read methods are synchronous (the S3 backend blocks on
//! a captured runtime handle), so every store touch runs on the blocking pool.
//! List requests share the [`ArchiveRuntime::scan_rows`] cache; per-session
//! reads always hit the store for the freshest bytes.

use std::collections::HashSet;
use std::io::Cursor;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use diesel::prelude::*;
use serde::Deserialize;
use tokio_util::io::ReaderStream;
use tower_cookies::Cookies;
use uuid::Uuid;

use shared::api::{HistorySessionSummary, HistorySessionsResponse};

use crate::archive::{
    decode_transcript, manifest_key, scan, transcript_key, ArchiveRuntime, SessionArchiveManifest,
    TRANSCRIPT_COMPRESSION,
};
use crate::auth::extract_user;
use crate::errors::AppError;
use crate::handlers::media_store::parse_range;
use crate::models::User;
use crate::AppState;

fn archive_runtime(app_state: &AppState) -> Result<Arc<ArchiveRuntime>, AppError> {
    app_state
        .archive
        .clone()
        .ok_or(AppError::NotFound("session archive is not enabled"))
}

/// Run a blocking archive-store closure on the blocking pool, mapping both
/// the join failure and the store's io::Error to [`AppError`].
async fn on_blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, AppError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| AppError::Internal(format!("archive task failed: {e}")))?
        .map_err(|e| AppError::Internal(format!("archive read failed: {e}")))
}

/// Session ids the user can read via a still-live `session_members` row.
fn live_member_session_ids(app_state: &AppState, user_id: Uuid) -> Result<HashSet<Uuid>, AppError> {
    use crate::schema::session_members;
    let mut conn = app_state.conn()?;
    let ids: Vec<Uuid> = session_members::table
        .filter(session_members::user_id.eq(user_id))
        .select(session_members::session_id)
        .load(&mut conn)?;
    Ok(ids.into_iter().collect())
}

fn row_visible(row: &scan::FlatRow, user: &User, live_member_ids: &HashSet<Uuid>) -> bool {
    user.is_admin
        || row.manifest.user_id == user.id
        || manifest_has_member(&row.manifest, user.id)
        || live_member_ids.contains(&row.manifest.session_id)
}

fn manifest_has_member(manifest: &SessionArchiveManifest, user_id: Uuid) -> bool {
    manifest
        .members
        .as_ref()
        .is_some_and(|members| members.iter().any(|m| m.user_id == user_id))
}

/// Per-session read check for the manifest/messages/media endpoints. Cheap
/// paths first (admin, owner-by-path, live membership); the manifest is only
/// fetched for the archived-share case. Every failure is a 404.
async fn verify_history_reader(
    app_state: &AppState,
    runtime: &Arc<ArchiveRuntime>,
    user: &User,
    owner_id: Uuid,
    session_id: Uuid,
) -> Result<(), AppError> {
    if user.is_admin || owner_id == user.id {
        return Ok(());
    }
    {
        use crate::schema::session_members;
        let mut conn = app_state.conn()?;
        let live: i64 = session_members::table
            .filter(session_members::session_id.eq(session_id))
            .filter(session_members::user_id.eq(user.id))
            .count()
            .get_result(&mut conn)?;
        if live > 0 {
            return Ok(());
        }
    }
    let store_runtime = runtime.clone();
    let manifest_bytes = on_blocking(move || {
        store_runtime
            .store
            .get_object(&manifest_key(owner_id, session_id))
    })
    .await?
    .ok_or(AppError::NotFound("Session not found"))?;
    let manifest: SessionArchiveManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| AppError::Internal(format!("corrupt archive manifest: {e}")))?;
    if manifest.user_id == owner_id && manifest_has_member(&manifest, user.id) {
        Ok(())
    } else {
        Err(AppError::NotFound("Session not found"))
    }
}

#[derive(Deserialize)]
pub struct HistoryListQuery {
    /// Admin-only: email substring or user/session UUID prefix. Ignored for
    /// non-admins (their scope is already just their own + shared sessions).
    pub user: Option<String>,
    pub agent: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    /// Session-name substring (case-insensitive).
    pub q: Option<String>,
}

/// GET /api/history/sessions — archived sessions visible to the caller,
/// filtered and sorted most-recently-active first.
pub async fn list_history_sessions(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Query(q): Query<HistoryListQuery>,
) -> Result<Json<HistorySessionsResponse>, AppError> {
    let user = extract_user(&app_state, Some(&headers), &cookies)?;
    let runtime = archive_runtime(&app_state)?;

    let filters = scan::Filters {
        user: if user.is_admin { q.user } else { None },
        agent: q.agent,
        name: q.q,
        from: parse_date_param(q.from.as_deref(), false)?,
        to: parse_date_param(q.to.as_deref(), true)?,
    };

    let scan_runtime = runtime.clone();
    let rows = on_blocking(move || scan_runtime.scan_rows()).await?;
    let live_member_ids = live_member_session_ids(&app_state, user.id)?;

    let mut kept: Vec<&scan::FlatRow> = rows
        .iter()
        .filter(|r| row_visible(r, &user, &live_member_ids) && filters.matches(r))
        .collect();
    kept.sort_by_key(|r| std::cmp::Reverse(r.manifest.last_activity));

    Ok(Json(HistorySessionsResponse {
        sessions: kept.into_iter().map(session_summary).collect(),
        is_admin: user.is_admin,
    }))
}

fn parse_date_param(
    input: Option<&str>,
    end_of_day: bool,
) -> Result<Option<chrono::NaiveDateTime>, AppError> {
    input
        .map(|s| scan::parse_date_arg(s, end_of_day))
        .transpose()
        .map_err(|_| AppError::BadRequest("invalid date: expected RFC3339 or YYYY-MM-DD"))
}

fn session_summary(row: &scan::FlatRow) -> HistorySessionSummary {
    let m = &row.manifest;
    HistorySessionSummary {
        session_id: m.session_id.to_string(),
        user_id: m.user_id.to_string(),
        owner_email: m.owner_email.clone(),
        owner_name: m.owner_name.clone(),
        session_name: m.session_name.clone(),
        agent_type: m.agent_type.clone(),
        status: m.status.clone(),
        hostname: m.hostname.clone(),
        created_at: fmt_dt(&m.created_at),
        last_activity: fmt_dt(&m.last_activity),
        total_cost_usd: m.total_cost_usd,
        message_count: row.message_count(),
        media_count: row.media_count() as i64,
        models: m.turns.models.clone(),
    }
}

fn fmt_dt(dt: &chrono::NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn parse_ids(user: &str, session: &str) -> Result<(Uuid, Uuid), AppError> {
    let user_id =
        Uuid::parse_str(user.trim()).map_err(|_| AppError::NotFound("Session not found"))?;
    let session_id =
        Uuid::parse_str(session.trim()).map_err(|_| AppError::NotFound("Session not found"))?;
    Ok((user_id, session_id))
}

/// GET /api/history/sessions/{user}/{session}/manifest — the archived
/// manifest JSON, verbatim.
pub async fn get_history_manifest(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Path((user, session)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let caller = extract_user(&app_state, Some(&headers), &cookies)?;
    let runtime = archive_runtime(&app_state)?;
    let (owner_id, session_id) = parse_ids(&user, &session)?;
    verify_history_reader(&app_state, &runtime, &caller, owner_id, session_id).await?;

    let bytes = on_blocking(move || {
        runtime
            .store
            .get_object(&manifest_key(owner_id, session_id))
    })
    .await?
    .ok_or(AppError::NotFound("Session not found"))?;
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}

/// GET /api/history/sessions/{user}/{session}/messages — the archived
/// transcript as NDJSON (zstd-decoded server-side). A session archived in
/// metadata-only mode streams an empty 200 body.
pub async fn get_history_messages(
    State(app_state): State<Arc<AppState>>,
    headers: HeaderMap,
    cookies: Cookies,
    Path((user, session)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let caller = extract_user(&app_state, Some(&headers), &cookies)?;
    let runtime = archive_runtime(&app_state)?;
    let (owner_id, session_id) = parse_ids(&user, &session)?;
    verify_history_reader(&app_state, &runtime, &caller, owner_id, session_id).await?;

    let ndjson = on_blocking(move || {
        match runtime
            .store
            .get_object(&transcript_key(owner_id, session_id))?
        {
            // Decode per the manifest's declared codec, not a hardcoded zstd
            // (#1466). Manifest-last write order means an absent manifest is a
            // mid-write read; fall back to the write-side default.
            Some(raw) => {
                let compression = runtime
                    .store
                    .get_session_manifest(owner_id, session_id)?
                    .and_then(|m| m.transcript)
                    .map(|t| t.compression)
                    .unwrap_or_else(|| TRANSCRIPT_COMPRESSION.to_string());
                decode_transcript(&compression, &raw).map(Some)
            }
            None => Ok(None),
        }
    })
    .await?
    .unwrap_or_default();

    let stream = ReaderStream::new(Cursor::new(ndjson));
    Ok((
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(stream),
    )
        .into_response())
}

/// GET /api/history/media/{user}/{session}/{media_id} — archived media bytes
/// with single/suffix HTTP Range support (`206`/`416`, `Accept-Ranges`).
pub async fn get_history_media(
    State(app_state): State<Arc<AppState>>,
    request_headers: HeaderMap,
    cookies: Cookies,
    Path((user, session, media)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let caller = extract_user(&app_state, Some(&request_headers), &cookies)?;
    let runtime = archive_runtime(&app_state)?;
    let (owner_id, session_id) = parse_ids(&user, &session)?;
    let media_id =
        Uuid::parse_str(media.trim()).map_err(|_| AppError::NotFound("Media not found"))?;
    verify_history_reader(&app_state, &runtime, &caller, owner_id, session_id).await?;

    let (meta, bytes) = on_blocking(move || {
        let meta = runtime
            .store
            .get_media_meta(owner_id, session_id, media_id)?;
        let bytes = runtime
            .store
            .get_media_bytes(owner_id, session_id, media_id)?;
        Ok((meta, bytes))
    })
    .await?;

    let bytes = bytes.ok_or(AppError::NotFound("Media not found"))?;
    // The sidecar carries the content type; fall back to octet-stream if a
    // blob somehow lacks one.
    let content_type = meta
        .map(|m| m.content_type)
        .unwrap_or_else(|| "application/octet-stream".to_string());

    Ok(bytes_response(&bytes, &content_type, &request_headers))
}

/// Build a media response honoring a single (or suffix) HTTP Range. Bytes are
/// already in memory (`get_media_bytes`), so ranging is a slice.
fn bytes_response(bytes: &[u8], content_type: &str, headers: &HeaderMap) -> Response {
    let total = bytes.len() as u64;
    let mut response = match parse_range(headers, total) {
        Some(Err(())) => Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_RANGE, format!("bytes */{total}"))
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Some(Ok((start, end))) => {
            let slice = bytes[start as usize..=end as usize].to_vec();
            let len = end - start + 1;
            Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, content_type)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, len)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total}"),
                )
                .body(Body::from(slice))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        None => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_LENGTH, total)
            .body(Body::from(bytes.to_vec()))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
    };
    // Archived media is the same attacker-influenced upload as the live copy, so
    // it carries the same hardening (see `media_security`).
    response
        .headers_mut()
        .extend(crate::handlers::media_security::media_security_headers(
            content_type,
        ));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_response_range_sets_206_and_content_range() {
        let bytes = (0u8..100).collect::<Vec<u8>>();
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=10-19".parse().unwrap());
        let resp = bytes_response(&bytes, "image/png", &headers);
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            resp.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 10-19/100"
        );
        assert_eq!(resp.headers().get(header::CONTENT_LENGTH).unwrap(), "10");
        assert_eq!(resp.headers().get(header::ACCEPT_RANGES).unwrap(), "bytes");
    }

    #[test]
    fn bytes_response_unsatisfiable_is_416() {
        let bytes = vec![0u8; 10];
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=50-60".parse().unwrap());
        let resp = bytes_response(&bytes, "image/png", &headers);
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            resp.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes */10"
        );
    }

    #[test]
    fn bytes_response_no_range_is_200_full() {
        let bytes = vec![7u8; 42];
        let headers = HeaderMap::new();
        let resp = bytes_response(&bytes, "video/mp4", &headers);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(header::CONTENT_LENGTH).unwrap(), "42");
        assert_eq!(resp.headers().get(header::ACCEPT_RANGES).unwrap(), "bytes");
    }
}
