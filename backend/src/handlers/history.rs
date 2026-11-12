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
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use diesel::prelude::*;
use serde::Deserialize;
use tower_cookies::Cookies;
use uuid::Uuid;

use shared::api::{
    HistoryOwnerRollup, HistorySessionSummary, HistorySessionsResponse, HistoryTotals,
    DEFAULT_HISTORY_PAGE_SIZE, MAX_HISTORY_PAGE_SIZE,
};

use crate::archive::{manifest_key, scan, ArchiveRuntime, SessionArchiveManifest};
use crate::auth::extract_user;
use crate::errors::AppError;
use crate::handlers::media_store::{parse_range, RangeOutcome};
use crate::models::User;
use crate::AppState;

pub(crate) fn archive_runtime(app_state: &AppState) -> Result<Arc<ArchiveRuntime>, AppError> {
    app_state
        .archive
        .clone()
        .ok_or(AppError::NotFound("session archive is not enabled"))
}

/// Run a blocking archive-store closure on the blocking pool, mapping both
/// the join failure and the store's io::Error to [`AppError`].
pub(crate) async fn on_blocking<T: Send + 'static>(
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
pub(super) async fn verify_history_reader(
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
    let mut sanitized: Vec<scan::FlatRow> = rows
        .iter()
        .filter(|row| row_visible(row, &user, &live_member_ids))
        .cloned()
        .collect();
    {
        let mut conn = app_state.conn()?;
        for row in &mut sanitized {
            crate::secrets::redaction::snapshot_for_user(&mut conn, row.user_id)?
                .redact_manifest(&mut row.manifest);
        }
    }
    let visible: Vec<&scan::FlatRow> = sanitized.iter().collect();

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
        .filter(|n| shared::strings::is_non_empty(n))
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
        transcript_available: m.transcript.is_some(),
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
    let mut manifest: SessionArchiveManifest = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::ServiceUnavailable("Invalid archive manifest"))?;
    crate::secrets::redaction::snapshot_for_user(&mut *app_state.conn()?, owner_id)?
        .redact_manifest(&mut manifest);
    Ok(Json(manifest).into_response())
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

    let permit = runtime
        .read_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::ServiceUnavailable("Archive readers are busy; retry shortly"))?;
    let pool = app_state.db_pool.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(2);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let result = (|| -> Result<(), AppError> {
            use std::io::{BufRead, Read};
            let mut conn = pool.get()?;
            let mut reader = runtime
                .store
                .transcript_reader(owner_id, session_id)
                .map_err(|_| AppError::ServiceUnavailable("Archive unavailable"))?;
            let mut count = 0i64;
            loop {
                let mut bytes = Vec::new();
                let read = reader
                    .by_ref()
                    .take(4 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut bytes)
                    .map_err(|_| AppError::ServiceUnavailable("Archive unavailable"))?;
                if read == 0 {
                    break;
                }
                if bytes.len() > 4 * 1024 * 1024 {
                    return Err(AppError::ServiceUnavailable(
                        "Archive record exceeds safe read budget",
                    ));
                }
                if bytes.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                crate::secrets::redaction::validate_json_budget(&bytes)?;
                let mut line: crate::archive::ArchiveMessageLine =
                    serde_json::from_slice(&bytes)
                        .map_err(|_| AppError::ServiceUnavailable("Invalid archive record"))?;
                if line.role == "user"
                    && shared::user_messages::is_substantive_user_record(&line.content)
                {
                    count += 1;
                }
                let rules = crate::secrets::redaction::snapshot_for_user(&mut conn, owner_id)?;
                rules.redact_agent_json(&mut line.content);
                let mut encoded = serde_json::to_vec(&line)
                    .map_err(|_| AppError::Internal("Cannot encode archive record".into()))?;
                encoded.push(b'\n');
                drop(rules);
                if tx.blocking_send(Ok(encoded)).is_err() {
                    return Ok(());
                }
            }
            backfill_stream_count(&runtime, &mut conn, owner_id, session_id, count)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = tx.blocking_send(Err(std::io::Error::other("Archive stream unavailable")));
        }
    });
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    Ok((
        [
            (header::CONTENT_TYPE, "application/x-ndjson"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

fn backfill_stream_count(
    runtime: &ArchiveRuntime,
    conn: &mut diesel::PgConnection,
    owner: Uuid,
    session: Uuid,
    count: i64,
) -> Result<(), AppError> {
    use diesel::Connection;
    let _guard = shared::sync::lock_recovering_poison(&runtime.manifest_write_lock);
    conn.transaction::<_, AppError, _>(|conn| {
        crate::secrets::expunge::archive_session_lock(conn, session)?;
        let Some(mut manifest) = runtime
            .store
            .get_session_manifest(owner, session)
            .map_err(|_| AppError::ServiceUnavailable("Archive metadata unavailable"))?
        else {
            return Ok(());
        };
        if manifest.user_message_count.is_some() {
            return Ok(());
        }
        manifest.user_message_count = Some(count);
        let rules = crate::secrets::redaction::snapshot_for_user(conn, owner)?;
        rules.redact_manifest(&mut manifest);
        let value = serde_json::to_value(&manifest)
            .map_err(|_| AppError::Internal("Cannot encode archive metadata".into()))?;
        runtime
            .store
            .put_object(
                &manifest_key(owner, session),
                serde_json::to_vec(&value)
                    .map_err(|_| AppError::Internal("Cannot encode archive metadata".into()))?,
            )
            .map_err(|_| AppError::ServiceUnavailable("Archive metadata unavailable"))?;
        Ok(())
    })
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

    let permit = runtime
        .read_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::ServiceUnavailable("Archive readers are busy; retry shortly"))?;
    let pool = app_state.db_pool.clone();
    let (content_type, file) = tokio::task::spawn_blocking(move || -> Result<_, AppError> {
        use std::io::{Read, Seek, Write};
        let mut conn = pool.get()?;
        if crate::secrets::expunge::media_removed(&mut conn, media_id)? {
            return Err(AppError::NotFound("Media not found"));
        }
        let meta = runtime
            .store
            .get_media_meta(owner_id, session_id, media_id)
            .map_err(|_| AppError::ServiceUnavailable("Archive media unavailable"))?;
        let mut reader = runtime
            .store
            .object_reader(
                &crate::archive::media_key(owner_id, session_id, media_id),
                512 * 1024 * 1024,
            )
            .map_err(|_| AppError::ServiceUnavailable("Archive media exceeds read budget"))?
            .ok_or(AppError::NotFound("Media not found"))?;
        let mut file = tempfile::tempfile()
            .map_err(|_| AppError::ServiceUnavailable("Archive spool unavailable"))?;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|_| AppError::ServiceUnavailable("Archive media unavailable"))?;
            if count == 0 {
                break;
            }
            file.write_all(&buffer[..count])
                .map_err(|_| AppError::ServiceUnavailable("Archive spool unavailable"))?;
        }
        file.rewind()
            .map_err(|_| AppError::ServiceUnavailable("Archive spool unavailable"))?;
        let rules = crate::secrets::redaction::snapshot_for_user(&mut conn, owner_id)?;
        if crate::secrets::expunge::scan_media(&mut file, &rules)
            .map_err(|_| AppError::ServiceUnavailable("Archive media inspection unavailable"))?
            .0
        {
            return Err(AppError::NotFound(
                "Media unavailable after secret redaction",
            ));
        }
        if crate::secrets::expunge::media_removed(&mut conn, media_id)? {
            return Err(AppError::NotFound("Media not found"));
        }
        file.rewind()
            .map_err(|_| AppError::ServiceUnavailable("Archive spool unavailable"))?;
        let mut prefix = [0; 4096];
        let prefix_len = file
            .read(&mut prefix)
            .map_err(|_| AppError::ServiceUnavailable("Archive media unavailable"))?;
        file.rewind()
            .map_err(|_| AppError::ServiceUnavailable("Archive spool unavailable"))?;
        let content_type = meta
            .map(|meta| meta.content_type)
            .filter(|ct| shared::media::media_kind(ct).is_some())
            .or_else(|| shared::media::sniff_content_type(&prefix[..prefix_len]).map(str::to_owned))
            .unwrap_or_else(|| "application/octet-stream".into());
        Ok((content_type, file))
    })
    .await
    .map_err(|_| AppError::ServiceUnavailable("Archive media reader unavailable"))??;
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let total = file
        .metadata()
        .map_err(|_| AppError::NotFound("Media not found"))?
        .len();
    let (start, len, status) = match parse_range(&request_headers, total) {
        RangeOutcome::NotSatisfiable => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .map_err(|_| AppError::Internal("Cannot build media response".into()))
        }
        RangeOutcome::Partial { start, end } => {
            (start, end - start + 1, StatusCode::PARTIAL_CONTENT)
        }
        RangeOutcome::Full => (0, total, StatusCode::OK),
    };
    let mut file = tokio::fs::File::from_std(file);
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(|_| AppError::ServiceUnavailable("Archive media unavailable"))?;
    let source = tokio_util::io::ReaderStream::new(file.take(len));
    let stream =
        futures_util::stream::unfold((source, permit), |(mut source, permit)| async move {
            use futures_util::StreamExt;
            source.next().await.map(|chunk| (chunk, (source, permit)))
        });
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, &content_type)
        .header(header::CONTENT_LENGTH, len)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "private, no-store");
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{total}", start, start + len - 1),
        );
    }
    let mut response = response
        .body(Body::from_stream(stream))
        .map_err(|_| AppError::Internal("Cannot build media response".into()))?;
    response
        .headers_mut()
        .extend(crate::handlers::media_security::media_security_headers(
            &content_type,
        ));
    Ok(response)
}

/// Build a media response honoring a single (or suffix) HTTP Range. Bytes are
/// already in memory (`get_media_bytes`), so ranging is a slice.
#[cfg(test)]
fn bytes_response(bytes: &[u8], content_type: &str, headers: &HeaderMap) -> Response {
    let total = bytes.len() as u64;
    let mut response = match parse_range(headers, total) {
        RangeOutcome::NotSatisfiable => Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::ACCEPT_RANGES, "bytes")
            .header(header::CONTENT_RANGE, format!("bytes */{total}"))
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        RangeOutcome::Partial { start, end } => {
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
        RangeOutcome::Full => Response::builder()
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
