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

use std::collections::{BTreeMap, HashSet};
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

use shared::api::{
    HistoryOwnerRollup, HistorySessionSummary, HistorySessionsResponse, HistoryTotals,
    DEFAULT_HISTORY_PAGE_SIZE, MAX_HISTORY_PAGE_SIZE,
};

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
    /// Page size. Defaults to [`DEFAULT_HISTORY_PAGE_SIZE`], clamped to
    /// [`MAX_HISTORY_PAGE_SIZE`] so a caller can't request the whole archive.
    pub limit: Option<usize>,
    /// Row offset into the filtered, sorted result set.
    pub offset: Option<usize>,
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
    let rows = on_blocking(move || scan_runtime.scan_rows_cached()).await?;
    let live_member_ids = live_member_session_ids(&app_state, user.id)?;

    // Visibility first: everything below operates on rows this caller may see.
    let visible: Vec<&scan::FlatRow> = rows
        .iter()
        .filter(|r| row_visible(r, &user, &live_member_ids))
        .collect();

    // Owner rollup deliberately ignores the `user` filter — see the field doc on
    // `HistorySessionsResponse::owners`. Admin-only, since it drives an
    // admin-only control and would otherwise be dead weight in the payload.
    let owners = if user.is_admin {
        let without_user = scan::Filters {
            user: None,
            ..filters.clone()
        };
        owner_rollups(visible.iter().copied().filter(|r| without_user.matches(r)))
    } else {
        Vec::new()
    };

    let mut kept: Vec<&scan::FlatRow> =
        visible.into_iter().filter(|r| filters.matches(r)).collect();
    // Tie-break on name so equal timestamps don't reorder between page fetches —
    // an unstable sort here would let a row repeat on one page and vanish from
    // the next.
    kept.sort_by(|a, b| {
        b.manifest
            .last_activity
            .cmp(&a.manifest.last_activity)
            .then_with(|| a.manifest.session_name.cmp(&b.manifest.session_name))
    });

    let totals = HistoryTotals {
        session_count: kept.len() as i64,
        message_count: kept.iter().map(|r| r.message_count()).sum(),
        // `+ 0.0` normalises negative zero: Rust's `Sum for f64` folds from
        // `-0.0`, so an empty (or all-zero) result set otherwise serialises as
        // `-0.0` and renders as "$-0.00".
        total_cost_usd: kept.iter().map(|r| r.manifest.total_cost_usd).sum::<f64>() + 0.0,
    };
    let total = kept.len() as i64;

    let limit = q
        .limit
        .unwrap_or(DEFAULT_HISTORY_PAGE_SIZE)
        .clamp(1, MAX_HISTORY_PAGE_SIZE);
    let offset = q.offset.unwrap_or(0).min(kept.len());

    Ok(Json(HistorySessionsResponse {
        sessions: kept
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(session_summary)
            .collect(),
        is_admin: user.is_admin,
        total,
        totals,
        owners,
    }))
}

/// Per-owner session count and spend, ordered by spend descending so the admin
/// tiles lead with the biggest consumers.
fn owner_rollups<'a>(rows: impl Iterator<Item = &'a scan::FlatRow>) -> Vec<HistoryOwnerRollup> {
    let mut by_user: BTreeMap<String, HistoryOwnerRollup> = BTreeMap::new();
    for row in rows {
        let m = &row.manifest;
        let entry = by_user
            .entry(m.user_id.to_string())
            .or_insert_with(|| HistoryOwnerRollup {
                user_id: m.user_id.to_string(),
                label: owner_label(m),
                session_count: 0,
                total_cost_usd: 0.0,
            });
        entry.session_count += 1;
        entry.total_cost_usd += m.total_cost_usd;
    }
    let mut out: Vec<HistoryOwnerRollup> = by_user.into_values().collect();
    out.sort_by(|a, b| {
        b.total_cost_usd
            .partial_cmp(&a.total_cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.label.cmp(&b.label))
    });
    out
}

/// Display name → email → raw id, matching what the browser used to derive
/// client-side from a summary row.
fn owner_label(m: &archive_format::SessionArchiveManifest) -> String {
    m.owner_name
        .clone()
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            if m.owner_email.is_empty() {
                m.user_id.to_string()
            } else {
                m.owner_email.clone()
            }
        })
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
        user_message_count: m.user_message_count,
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

    let backfill_runtime = runtime.clone();
    let (ndjson, backfilled) = on_blocking(move || {
        match runtime
            .store
            .get_object(&transcript_key(owner_id, session_id))?
        {
            // Decode per the manifest's declared codec, not a hardcoded zstd
            // (#1466). Manifest-last write order means an absent manifest is a
            // mid-write read; fall back to the write-side default.
            Some(raw) => {
                let manifest = runtime.store.get_session_manifest(owner_id, session_id)?;
                let compression = manifest
                    .as_ref()
                    .and_then(|m| m.transcript.as_ref())
                    .map(|t| t.compression.clone())
                    .unwrap_or_else(|| TRANSCRIPT_COMPRESSION.to_string());
                let ndjson = decode_transcript(&compression, &raw)?;
                let backfilled =
                    backfill_user_message_count(&runtime, manifest, owner_id, session_id, &ndjson);
                Ok((Some(ndjson), backfilled))
            }
            None => Ok((None, false)),
        }
    })
    .await?;
    let ndjson = ndjson.unwrap_or_default();
    if backfilled {
        // The manifest object changed under the list cache; refresh it behind
        // this request so the new count shows on the next list fetch rather
        // than after the self-heal window.
        backfill_runtime.warm_scan_cache();
    }

    let stream = ReaderStream::new(Cursor::new(ndjson));
    Ok((
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(stream),
    )
        .into_response())
}

/// Lazily backfill `user_message_count` on a pre-existing manifest the first
/// time its transcript is viewed. New archives get the count at archive time
/// (`background.rs`); this fills manifests written before the field existed
/// without a bulk migration pass over the whole archive. Best-effort: a failed
/// write only delays the backfill to the next view. Returns true when the
/// manifest object was rewritten.
fn backfill_user_message_count(
    runtime: &ArchiveRuntime,
    manifest: Option<SessionArchiveManifest>,
    owner_id: Uuid,
    session_id: Uuid,
    ndjson: &[u8],
) -> bool {
    // Cheap pre-checks on the manifest fetched earlier in this request; the
    // authoritative read happens under the write lock below.
    match &manifest {
        None => return false,
        Some(m) if m.user_message_count.is_some() => return false,
        Some(_) => {}
    }
    let count = ndjson
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<crate::archive::ArchiveMessageLine>(line).ok())
        .filter(|l| {
            l.role == "user" && shared::user_messages::is_substantive_user_record(&l.content)
        })
        .count() as i64;

    // Serialize with the archive sweep and RE-READ before writing: both
    // writers read-modify-write the whole manifest object, and writing our
    // earlier copy here could clobber a concurrently re-archived (newer)
    // manifest — losing its fresher last_activity, counts, and transcript
    // info. Under the lock, a fresh read + conditional write is a CAS.
    let _manifest_guard = runtime
        .manifest_write_lock
        .lock()
        // A poisoned lock only means another writer panicked mid-write; the
        // ordering guarantee is unaffected, so continue.
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut manifest = match runtime.store.get_session_manifest(owner_id, session_id) {
        Ok(Some(m)) => m,
        Ok(None) => return false,
        Err(e) => {
            tracing::warn!("history backfill: manifest re-read failed for {session_id}: {e}");
            return false;
        }
    };
    if manifest.user_message_count.is_some() {
        // A re-archive filled it in while we were counting — theirs is
        // computed from the same merged transcript; nothing to do.
        return false;
    }
    manifest.user_message_count = Some(count);
    let bytes = match serde_json::to_vec(&manifest) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!("history backfill: could not serialize manifest {session_id}: {e}");
            return false;
        }
    };
    match runtime
        .store
        .put_object(&manifest_key(owner_id, session_id), bytes)
    {
        Ok(()) => {
            tracing::info!("history backfill: user_message_count={count} for session {session_id}");
            true
        }
        Err(e) => {
            tracing::warn!("history backfill: manifest write failed for {session_id}: {e}");
            false
        }
    }
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
    // The sidecar carries the content type. When it's missing, sniff the bytes
    // rather than serving `application/octet-stream`: browsers content-sniff
    // raster formats and render them regardless, but they never sniff SVG, so a
    // blanket octet-stream fallback breaks *only* SVG while PNG/JPEG keep
    // working — a failure shaped to escape notice. Octet-stream remains the
    // last resort for bytes we don't recognize.
    let content_type = meta
        .map(|m| m.content_type)
        .filter(|ct| !ct.trim().is_empty())
        .or_else(|| shared::media::sniff_content_type(&bytes).map(str::to_string))
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
