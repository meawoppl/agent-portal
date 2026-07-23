//! `serve` — a loopback-only HTTP server that exposes the archive over a small
//! JSON API and serves the embedded `viewer-frontend` WASM UI.
//!
//! ## Trust model — NO AUTH, LOOPBACK ONLY, BY DESIGN
//!
//! This server performs **no authentication and no authorization**. It is an
//! operator tool run over operator-controlled archive data (the same data an
//! operator can already read straight off disk / S3 with the other subcommands),
//! so it binds to `127.0.0.1` **only** and never to a routable address. Anyone
//! who can reach the port can read every archived transcript, manifest, and
//! media blob for every user. Do not port-forward it, put it behind a reverse
//! proxy, or bind it to `0.0.0.0`. If you need multi-user access with
//! authentication, that is the portal backend's job, not this tool's.
//!
//! ## Blocking store methods
//!
//! `ArchiveStore`'s read methods are synchronous and the S3 backend blocks on a
//! captured tokio runtime handle, so every handler that touches the store does
//! so inside [`tokio::task::spawn_blocking`]. The store is constructed once at
//! startup and shared as an `Arc`.
//!
//! ## Manifest cache
//!
//! The list/users/rollup endpoints need every manifest, which for S3 is one GET
//! per session. We scan once and cache the flattened rows for `--refresh-secs`
//! (default 10s; `0` disables the cache and rescans every request). One cache
//! serves both backends: it bounds S3 list/GET cost and, at the short default,
//! keeps a local archive effectively live. Per-session endpoints (manifest,
//! messages, media) always read straight through — they are a single object
//! fetch and must reflect the latest bytes.

use std::io::Cursor;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use archive_format::{manifest_key, transcript_key, zstd_decode, ArchiveStore};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use chrono::NaiveDateTime;
use clap::Args as ClapArgs;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::rows::{collect_rows, filter_and_sort, parse_date_arg, Filters, FlatRow};

#[derive(ClapArgs, Debug)]
pub struct ServeArgs {
    /// TCP port to listen on (always bound to 127.0.0.1 only).
    #[arg(long, default_value_t = 8890)]
    port: u16,
    /// Open the viewer in the default browser once the server is listening.
    #[arg(long)]
    open: bool,
    /// Seconds to cache the scanned manifest list before rescanning. `0`
    /// rescans on every request (freshest, but slower on large S3 archives).
    #[arg(long, default_value_t = 10)]
    refresh_secs: u64,
}

/// Shared server state: the store plus the manifest-row cache.
struct ServeState {
    store: Arc<ArchiveStore>,
    refresh: Duration,
    cache: Mutex<Cache>,
}

#[derive(Default)]
struct Cache {
    rows: Option<Arc<Vec<FlatRow>>>,
    fetched_at: Option<Instant>,
}

impl ServeState {
    /// Return the flattened manifest rows, rescanning if the cache is empty or
    /// older than `refresh`. The scan runs on the blocking pool.
    async fn rows(&self) -> Result<Arc<Vec<FlatRow>>, ApiError> {
        {
            let cache = self.cache.lock().await;
            if let (Some(rows), Some(at)) = (&cache.rows, cache.fetched_at) {
                if at.elapsed() < self.refresh {
                    return Ok(rows.clone());
                }
            }
        }
        let store = self.store.clone();
        let scanned = tokio::task::spawn_blocking(move || collect_rows(&store))
            .await
            .map_err(|e| ApiError::internal(format!("scan task failed: {e}")))?
            .map_err(|e| ApiError::internal(format!("failed to scan archive: {e}")))?;
        let arc = Arc::new(scanned);
        let mut cache = self.cache.lock().await;
        cache.rows = Some(arc.clone());
        cache.fetched_at = Some(Instant::now());
        Ok(arc)
    }
}

/// Plain-text error responses (the contract: 404s and friends are plain text).
struct ApiError(StatusCode, String);

impl ApiError {
    fn not_found(msg: impl Into<String>) -> Self {
        Self(StatusCode::NOT_FOUND, msg.into())
    }
    fn bad_request(msg: impl Into<String>) -> Self {
        Self(StatusCode::BAD_REQUEST, msg.into())
    }
    fn internal(msg: impl Into<String>) -> Self {
        let msg = msg.into();
        eprintln!("error: {msg}");
        Self(StatusCode::INTERNAL_SERVER_ERROR, msg)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, self.1).into_response()
    }
}

// ---------------------------------------------------------------------------
// API response shapes (mirror viewer-frontend/src/api.rs — the pinned contract)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct UserSummary {
    user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner_name: Option<String>,
    session_count: i64,
}

#[derive(Serialize)]
struct SessionSummary {
    session_id: String,
    user_id: String,
    session_name: String,
    agent_type: String,
    status: String,
    hostname: String,
    created_at: String,
    last_activity: String,
    total_cost_usd: f64,
    message_count: i64,
    media_count: i64,
    models: Vec<String>,
}

/// A rollup row. The first five fields are the REQUIRED contract (PR 3a); the
/// rest are the extra token/tool/media metrics the CLI rollup computes — the
/// viewer's deserializer tolerates them.
#[derive(Serialize)]
struct RollupRow {
    group: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    session_count: i64,
    message_count: i64,
    total_cost_usd: f64,
    // --- extras (tolerated by the viewer) ---
    turns: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache_tokens: i64,
    thinking_tokens: i64,
    tool_calls: i64,
    media_bytes: u64,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the server until Ctrl-C. `store` is the already-constructed archive
/// store (target resolution is shared with the CLI subcommands).
pub async fn run(store: ArchiveStore, args: ServeArgs) -> Result<()> {
    let state = Arc::new(ServeState {
        store: Arc::new(store),
        refresh: Duration::from_secs(args.refresh_secs),
        cache: Mutex::new(Cache::default()),
    });

    let app = router(state);

    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, args.port));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr} (is the port already in use?)"))?;
    let url = format!("http://{addr}/");

    println!("portal-archive viewer serving at {url}");
    println!("  loopback-only, NO AUTH — operator tool over operator-controlled data.");
    if args.refresh_secs == 0 {
        println!("  manifest cache: disabled (rescanning every request)");
    } else {
        println!("  manifest cache: {}s", args.refresh_secs);
    }
    println!("  press Ctrl-C to stop.");

    if args.open {
        open_browser(&url);
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .context("server error")?;
    Ok(())
}

fn router(state: Arc<ServeState>) -> Router {
    // The embedded WASM UI. The app is hash-routed, so every unknown path falls
    // back to index.html with a 200 (same config as the backend's SPA mount).
    let ui = memory_serve::load!()
        .index_file(Some("/index.html"))
        .fallback(Some("/index.html"))
        .fallback_status(StatusCode::OK)
        .into_router();

    Router::new()
        .route("/api/users", get(get_users))
        .route("/api/sessions", get(get_sessions))
        .route("/api/sessions/{user}/{session}/manifest", get(get_manifest))
        .route("/api/sessions/{user}/{session}/messages", get(get_messages))
        .route("/api/media/{user}/{session}/{media_id}", get(get_media))
        .route("/api/rollup", get(get_rollup))
        .with_state(state)
        .merge(ui)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn get_users(
    State(state): State<Arc<ServeState>>,
) -> Result<Json<Vec<UserSummary>>, ApiError> {
    let rows = state.rows().await?;
    Ok(Json(build_users(&rows)))
}

#[derive(Deserialize)]
struct SessionQuery {
    user: Option<String>,
    agent: Option<String>,
    from: Option<String>,
    to: Option<String>,
    q: Option<String>,
}

async fn get_sessions(
    State(state): State<Arc<ServeState>>,
    Query(q): Query<SessionQuery>,
) -> Result<Json<Vec<SessionSummary>>, ApiError> {
    let filters = Filters {
        user: q.user,
        agent: q.agent,
        name: q.q,
        from: q
            .from
            .as_deref()
            .map(|s| parse_date_arg(s, false))
            .transpose()
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
        to: q
            .to
            .as_deref()
            .map(|s| parse_date_arg(s, true))
            .transpose()
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
    };
    let rows = state.rows().await?;
    // `filter_and_sort` consumes its input; rebuild owned rows for it. Cheap at
    // v1 scale and keeps the cached `Arc<Vec<FlatRow>>` shared/immutable.
    let flat: Vec<FlatRow> = rows
        .iter()
        .filter(|r| filters.matches(r))
        .map(clone_row)
        .collect();
    let sorted = filter_and_sort(flat, &Filters::default());
    Ok(Json(sorted.iter().map(session_summary).collect()))
}

async fn get_manifest(
    State(state): State<Arc<ServeState>>,
    Path((user, session)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let (user_id, session_id) = parse_ids(&user, &session)?;
    let store = state.store.clone();
    let bytes =
        tokio::task::spawn_blocking(move || store.get_object(&manifest_key(user_id, session_id)))
            .await
            .map_err(|e| ApiError::internal(format!("task failed: {e}")))?
            .map_err(|e| ApiError::internal(format!("failed to read manifest: {e}")))?
            .ok_or_else(|| ApiError::not_found("manifest not found"))?;
    // Verbatim manifest JSON.
    Ok(([(header::CONTENT_TYPE, "application/json")], bytes).into_response())
}

async fn get_messages(
    State(state): State<Arc<ServeState>>,
    Path((user, session)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    let (user_id, session_id) = parse_ids(&user, &session)?;
    let store = state.store.clone();
    // Fetch + zstd-decode on the blocking pool; a session with no transcript
    // (metadata-only archive) returns an empty 200 NDJSON body.
    let ndjson = tokio::task::spawn_blocking(move || {
        match store.get_object(&transcript_key(user_id, session_id))? {
            Some(raw) => zstd_decode(&raw).map(Some),
            None => Ok(None),
        }
    })
    .await
    .map_err(|e| ApiError::internal(format!("task failed: {e}")))?
    .map_err(|e| ApiError::internal(format!("failed to read transcript: {e}")))?
    .unwrap_or_default();

    let stream = ReaderStream::new(Cursor::new(ndjson));
    Ok((
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(stream),
    )
        .into_response())
}

async fn get_media(
    State(state): State<Arc<ServeState>>,
    headers: HeaderMap,
    Path((user, session, media)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let (user_id, session_id) = parse_ids(&user, &session)?;
    let media_id =
        Uuid::parse_str(media.trim()).map_err(|_| ApiError::not_found("media not found"))?;

    let store = state.store.clone();
    let (meta, bytes) = tokio::task::spawn_blocking(move || {
        let meta = store.get_media_meta(user_id, session_id, media_id)?;
        let bytes = store.get_media_bytes(user_id, session_id, media_id)?;
        std::io::Result::Ok((meta, bytes))
    })
    .await
    .map_err(|e| ApiError::internal(format!("task failed: {e}")))?
    .map_err(|e| ApiError::internal(format!("failed to read media: {e}")))?;

    let bytes = bytes.ok_or_else(|| ApiError::not_found("media not found"))?;
    // The sidecar carries the content type; fall back to octet-stream if a blob
    // somehow lacks one.
    let content_type = meta
        .map(|m| m.content_type)
        .unwrap_or_else(|| "application/octet-stream".to_string());

    Ok(media_response(&bytes, &content_type, &headers))
}

#[derive(Deserialize)]
struct RollupQuery {
    group_by: Option<String>,
    from: Option<String>,
    to: Option<String>,
}

async fn get_rollup(
    State(state): State<Arc<ServeState>>,
    Query(q): Query<RollupQuery>,
) -> Result<Json<Vec<RollupRow>>, ApiError> {
    let group_by = match q.group_by.as_deref().unwrap_or("user") {
        "user" => GroupBy::User,
        "agent" => GroupBy::Agent,
        "model" => GroupBy::Model,
        other => {
            return Err(ApiError::bad_request(format!(
                "group_by must be user, agent, or model (got `{other}`)"
            )))
        }
    };
    let filters = Filters {
        from: q
            .from
            .as_deref()
            .map(|s| parse_date_arg(s, false))
            .transpose()
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
        to: q
            .to
            .as_deref()
            .map(|s| parse_date_arg(s, true))
            .transpose()
            .map_err(|e| ApiError::bad_request(e.to_string()))?,
        ..Default::default()
    };
    let rows = state.rows().await?;
    let kept: Vec<&FlatRow> = rows.iter().filter(|r| filters.matches(r)).collect();
    Ok(Json(build_rollup(&kept, group_by)))
}

// ---------------------------------------------------------------------------
// Pure helpers (unit-tested)
// ---------------------------------------------------------------------------

/// Group rows by `user_id`; identity (email/name) comes from each user's newest
/// manifest (by `last_activity`).
fn build_users(rows: &[FlatRow]) -> Vec<UserSummary> {
    use std::collections::HashMap;
    struct Acc<'a> {
        count: i64,
        newest: &'a FlatRow,
    }
    let mut by_user: HashMap<Uuid, Acc> = HashMap::new();
    for row in rows {
        by_user
            .entry(row.manifest.user_id)
            .and_modify(|a| {
                a.count += 1;
                if row.manifest.last_activity > a.newest.manifest.last_activity {
                    a.newest = row;
                }
            })
            .or_insert(Acc {
                count: 1,
                newest: row,
            });
    }
    let mut out: Vec<UserSummary> = by_user
        .into_iter()
        .map(|(user_id, acc)| {
            let m = &acc.newest.manifest;
            UserSummary {
                user_id: user_id.to_string(),
                owner_email: non_empty(&m.owner_email),
                owner_name: m.owner_name.as_ref().and_then(|n| non_empty(n)),
                session_count: acc.count,
            }
        })
        .collect();
    // Deterministic ordering: most sessions first, then id.
    out.sort_by(|a, b| {
        b.session_count
            .cmp(&a.session_count)
            .then_with(|| a.user_id.cmp(&b.user_id))
    });
    out
}

fn session_summary(row: &FlatRow) -> SessionSummary {
    let m = &row.manifest;
    SessionSummary {
        session_id: m.session_id.to_string(),
        user_id: m.user_id.to_string(),
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

#[derive(Clone, Copy)]
enum GroupBy {
    User,
    Agent,
    Model,
}

fn build_rollup(rows: &[&FlatRow], group_by: GroupBy) -> Vec<RollupRow> {
    use std::collections::BTreeMap;
    #[derive(Default)]
    struct Agg {
        label: Option<String>,
        newest: Option<NaiveDateTime>,
        sessions: i64,
        messages: i64,
        turns: i64,
        input: i64,
        output: i64,
        cache: i64,
        thinking: i64,
        cost: f64,
        tool_calls: i64,
        media_bytes: u64,
    }
    let mut groups: BTreeMap<String, Agg> = BTreeMap::new();
    for row in rows {
        let m = &row.manifest;
        for (key, label) in group_keys(row, group_by) {
            let a = groups.entry(key).or_default();
            // For user grouping the label is the identity from the newest
            // manifest in the group.
            if a.newest.is_none_or(|n| m.last_activity > n) {
                a.newest = Some(m.last_activity);
                if label.is_some() {
                    a.label = label.clone();
                }
            }
            a.sessions += 1;
            a.messages += row.message_count();
            a.turns += m.turns.count;
            a.input += m.tokens.input;
            a.output += m.tokens.output;
            a.cache += row.cache_tokens();
            a.thinking += m.tokens.thinking;
            a.cost += m.total_cost_usd;
            a.tool_calls += m.turns.tool_calls;
            a.media_bytes += row.media_bytes();
        }
    }
    groups
        .into_iter()
        .map(|(group, a)| RollupRow {
            group,
            label: a.label,
            session_count: a.sessions,
            message_count: a.messages,
            total_cost_usd: a.cost,
            turns: a.turns,
            input_tokens: a.input,
            output_tokens: a.output,
            cache_tokens: a.cache,
            thinking_tokens: a.thinking,
            tool_calls: a.tool_calls,
            media_bytes: a.media_bytes,
        })
        .collect()
}

/// The group key(s) and optional human label a row contributes to. Only `Model`
/// can yield more than one (a session that used multiple models counts in each).
fn group_keys(row: &FlatRow, group_by: GroupBy) -> Vec<(String, Option<String>)> {
    let m = &row.manifest;
    match group_by {
        // group == user id (stable), label == best human identity.
        GroupBy::User => {
            let label = m
                .owner_name
                .as_ref()
                .and_then(|n| non_empty(n))
                .or_else(|| non_empty(&m.owner_email));
            vec![(m.user_id.to_string(), label)]
        }
        GroupBy::Agent => vec![(m.agent_type.clone(), None)],
        GroupBy::Model => {
            if m.turns.models.is_empty() {
                vec![("(no model)".to_string(), None)]
            } else {
                m.turns.models.iter().map(|m| (m.clone(), None)).collect()
            }
        }
    }
}

/// Build a media response honoring a single (or suffix) HTTP Range. Bytes are
/// already in memory (`store.get_media_bytes`), so ranging is a slice — fine at
/// v1 scale.
fn media_response(bytes: &[u8], content_type: &str, headers: &HeaderMap) -> Response {
    let total = bytes.len() as u64;
    match parse_range(headers, total) {
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
    }
}

/// Parse a single-range `Range: bytes=start-end` header against `total` bytes.
/// `None` = no/malformed header (serve whole); `Some(Ok((start,end)))` = an
/// inclusive satisfiable range; `Some(Err(()))` = syntactically valid but
/// unsatisfiable (→ 416). Modeled on the backend's `serve_media` (#1450).
#[allow(clippy::type_complexity)]
fn parse_range(headers: &HeaderMap, total: u64) -> Option<Result<(u64, u64), ()>> {
    let raw = headers.get(header::RANGE)?.to_str().ok()?;
    let spec = raw.strip_prefix("bytes=")?;
    if spec.contains(',') {
        // Multi-range is unsupported; treat as unsatisfiable.
        return Some(Err(()));
    }
    let (start_s, end_s) = spec.split_once('-')?;
    let (start, end) = if start_s.is_empty() {
        // Suffix range: bytes=-N → last N bytes.
        let n: u64 = end_s.parse().ok()?;
        if n == 0 || total == 0 {
            return Some(Err(()));
        }
        let n = n.min(total);
        (total - n, total - 1)
    } else {
        let start: u64 = start_s.parse().ok()?;
        let end: u64 = if end_s.is_empty() {
            total.saturating_sub(1)
        } else {
            end_s.parse().ok()?
        };
        (start, end.min(total.saturating_sub(1)))
    };
    if total == 0 || start > end || start >= total {
        return Some(Err(()));
    }
    Some(Ok((start, end)))
}

fn parse_ids(user: &str, session: &str) -> Result<(Uuid, Uuid), ApiError> {
    let user_id =
        Uuid::parse_str(user.trim()).map_err(|_| ApiError::not_found("session not found"))?;
    let session_id =
        Uuid::parse_str(session.trim()).map_err(|_| ApiError::not_found("session not found"))?;
    Ok((user_id, session_id))
}

fn fmt_dt(dt: &NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Clone a `FlatRow` (it is intentionally not `Clone` upstream — only `serve`
/// needs to materialize an owned working set from the shared cache).
fn clone_row(row: &FlatRow) -> FlatRow {
    FlatRow {
        user_id: row.user_id,
        manifest: row.manifest.clone(),
    }
}

/// Best-effort "open this URL in the default browser". No new dependency — shells
/// out to the platform opener and only warns on failure.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![url]);
    #[cfg(target_os = "windows")]
    let cmd = ("cmd", vec!["/C", "start", "", url]);
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let cmd = ("xdg-open", vec![url]);

    if let Err(e) = std::process::Command::new(cmd.0).args(&cmd.1).spawn() {
        eprintln!("warning: could not open a browser ({e}); open {url} manually");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::test_support::{manifest, media_entry};
    use chrono::NaiveDate;

    fn dt(day: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 7, day)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
    }

    fn row(user: u128, session: u128, email: &str, name: &str, agent: &str, day: u32) -> FlatRow {
        FlatRow {
            user_id: Uuid::from_u128(user),
            manifest: manifest(
                Uuid::from_u128(user),
                Uuid::from_u128(session),
                email,
                name,
                agent,
                dt(day),
            ),
        }
    }

    #[test]
    fn users_use_newest_manifest_identity_and_count() {
        let mut old = row(1, 10, "old@x.io", "Old Name", "claude", 10);
        old.manifest.owner_name = Some("Old Name".to_string());
        let mut newer = row(1, 11, "new@x.io", "New Name", "claude", 14);
        newer.manifest.owner_name = Some("New Name".to_string());
        let other = row(2, 20, "bob@x.io", "Bob", "codex", 12);

        let users = build_users(&[old, newer, other]);
        let u1 = users
            .iter()
            .find(|u| u.user_id == Uuid::from_u128(1).to_string())
            .unwrap();
        assert_eq!(u1.session_count, 2);
        // Identity from the newest (day-14) manifest.
        assert_eq!(u1.owner_email.as_deref(), Some("new@x.io"));
        assert_eq!(u1.owner_name.as_deref(), Some("New Name"));
    }

    #[test]
    fn rollup_by_user_has_required_fields_and_extras() {
        let mut a = row(1, 10, "a@x.io", "s1", "claude", 10);
        a.manifest.message_counts.insert("user".into(), 3);
        a.manifest.tokens.input = 100;
        a.manifest.total_cost_usd = 0.5;
        a.manifest.turns.count = 2;
        a.manifest.turns.tool_calls = 4;
        a.manifest.media = Some(vec![media_entry(Uuid::new_v4(), 2048)]);
        let mut b = row(1, 11, "a@x.io", "s2", "claude", 12);
        b.manifest.message_counts.insert("user".into(), 1);
        b.manifest.total_cost_usd = 0.25;

        let rows = vec![&a, &b];
        let out = build_rollup(&rows, GroupBy::User);
        assert_eq!(out.len(), 1);
        let r = &out[0];
        assert_eq!(r.group, Uuid::from_u128(1).to_string());
        assert_eq!(r.label.as_deref(), Some("a@x.io"));
        assert_eq!(r.session_count, 2);
        assert_eq!(r.message_count, 4);
        assert!((r.total_cost_usd - 0.75).abs() < 1e-9);
        assert_eq!(r.input_tokens, 100);
        assert_eq!(r.tool_calls, 4);
        assert_eq!(r.media_bytes, 2048);
    }

    #[test]
    fn rollup_by_model_counts_session_in_each_model() {
        let mut a = row(1, 10, "a@x.io", "s1", "claude", 10);
        a.manifest.turns.count = 3;
        a.manifest.turns.models = vec!["opus".into(), "sonnet".into()];
        let rows = vec![&a];
        let out = build_rollup(&rows, GroupBy::Model);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|r| r.turns == 3));
    }

    #[test]
    fn parse_range_variants() {
        let mut headers = HeaderMap::new();
        assert!(parse_range(&headers, 100).is_none());

        headers.insert(header::RANGE, "bytes=0-49".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Ok((0, 49))));

        headers.insert(header::RANGE, "bytes=50-".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Ok((50, 99))));

        headers.insert(header::RANGE, "bytes=-20".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Ok((80, 99))));

        headers.insert(header::RANGE, "bytes=90-200".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Ok((90, 99))));

        headers.insert(header::RANGE, "bytes=200-".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Err(())));

        headers.insert(header::RANGE, "bytes=0-10,20-30".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Err(())));
    }

    #[test]
    fn media_response_range_sets_206_and_content_range() {
        let bytes = (0u8..100).collect::<Vec<u8>>();
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=10-19".parse().unwrap());
        let resp = media_response(&bytes, "image/png", &headers);
        assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            resp.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 10-19/100"
        );
        assert_eq!(resp.headers().get(header::CONTENT_LENGTH).unwrap(), "10");
        assert_eq!(resp.headers().get(header::ACCEPT_RANGES).unwrap(), "bytes");
    }

    #[test]
    fn media_response_unsatisfiable_is_416() {
        let bytes = vec![0u8; 10];
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=50-60".parse().unwrap());
        let resp = media_response(&bytes, "image/png", &headers);
        assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            resp.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes */10"
        );
    }

    #[test]
    fn media_response_no_range_is_200_full() {
        let bytes = vec![7u8; 42];
        let headers = HeaderMap::new();
        let resp = media_response(&bytes, "video/mp4", &headers);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers().get(header::CONTENT_LENGTH).unwrap(), "42");
        assert_eq!(resp.headers().get(header::ACCEPT_RANGES).unwrap(), "bytes");
    }

    // -- Live router integration (real request/response cycle via oneshot) ----

    mod integration {
        use super::super::*;
        use archive_format::{
            ArchiveMessageLine, ArchiveStore, ArchivedMediaMeta, LocalArchiveStore,
            SessionArchiveBundle,
        };
        use axum::body::{to_bytes, Body};
        use axum::http::Request;
        use chrono::NaiveDate;
        use tempfile::TempDir;
        use tower::ServiceExt; // for `oneshot`

        const USER: u128 = 0xAAAA_0000_0000_0000_0000_0000_0000_000A;
        const SESSION: u128 = 0x1111_0000_0000_0000_0000_0000_0000_0001;

        fn day(d: u32) -> NaiveDateTime {
            NaiveDate::from_ymd_opt(2026, 7, d)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap()
        }

        /// Write a one-user, one-session archive (with a media blob) to a
        /// tempdir and wrap it in a ready-to-serve state.
        fn fixture() -> (TempDir, Arc<ServeState>, Uuid) {
            let dir = tempfile::tempdir().unwrap();
            let store = ArchiveStore::Local(LocalArchiveStore::new(dir.path().to_path_buf()));
            let (user, session) = (Uuid::from_u128(USER), Uuid::from_u128(SESSION));

            let mut m = crate::rows::test_support::manifest(
                user,
                session,
                "alice@example.com",
                "refactor the rail",
                "claude",
                day(14),
            );
            m.message_counts
                .extend([("user".to_string(), 2), ("assistant".to_string(), 1)]);
            m.tokens.input = 100;
            m.total_cost_usd = 0.10;
            m.turns.count = 2;
            m.turns.models = vec!["opus".to_string()];
            let media_id = Uuid::new_v4();
            m.media = Some(vec![crate::rows::test_support::media_entry(media_id, 8)]);

            let line = ArchiveMessageLine {
                id: Uuid::new_v4(),
                role: "user".to_string(),
                created_at: day(14),
                agent_type: "claude".to_string(),
                content: serde_json::Value::String("please refactor the rail".to_string()),
            };
            let mut ndjson = serde_json::to_vec(&line).unwrap();
            ndjson.push(b'\n');

            store
                .put_session_archive(&SessionArchiveBundle {
                    manifest: m,
                    transcript_ndjson: Some(ndjson),
                })
                .unwrap();

            let meta = ArchivedMediaMeta {
                media_id,
                kind: "image".to_string(),
                content_type: "image/png".to_string(),
                filename: Some("plot.png".to_string()),
                bytes: 8,
                uploaded_at: day(14),
            };
            store.put_media(user, session, &meta, b"PNGBYTES").unwrap();

            let state = Arc::new(ServeState {
                store: Arc::new(store),
                refresh: Duration::from_secs(10),
                cache: Mutex::new(Cache::default()),
            });
            (dir, state, media_id)
        }

        async fn get(state: &Arc<ServeState>, uri: &str) -> Response {
            router(state.clone())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap()
        }

        async fn body_string(resp: Response) -> String {
            let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
            String::from_utf8(bytes.to_vec()).unwrap()
        }

        #[tokio::test]
        async fn users_sessions_manifest_messages_rollup_and_media_range() {
            let (_dir, state, media_id) = fixture();
            let user = Uuid::from_u128(USER);
            let session = Uuid::from_u128(SESSION);

            // /api/users
            let resp = get(&state, "/api/users").await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_string(resp).await;
            assert!(body.contains("alice@example.com"), "users: {body}");
            assert!(body.contains("\"session_count\":1"), "users: {body}");

            // /api/sessions
            let resp = get(&state, "/api/sessions").await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_string(resp).await;
            assert!(body.contains("refactor the rail"), "sessions: {body}");
            assert!(body.contains("\"message_count\":3"), "sessions: {body}");

            // /api/sessions filtered by agent that matches nothing → empty array
            let resp = get(&state, "/api/sessions?agent=codex").await;
            assert_eq!(body_string(resp).await, "[]");

            // manifest (verbatim JSON)
            let resp = get(&state, &format!("/api/sessions/{user}/{session}/manifest")).await;
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(
                resp.headers().get(header::CONTENT_TYPE).unwrap(),
                "application/json"
            );
            let body = body_string(resp).await;
            assert!(body.contains(&session.to_string()), "manifest: {body}");

            // messages (NDJSON)
            let resp = get(&state, &format!("/api/sessions/{user}/{session}/messages")).await;
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(
                resp.headers().get(header::CONTENT_TYPE).unwrap(),
                "application/x-ndjson"
            );
            let body = body_string(resp).await;
            let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
            assert_eq!(lines.len(), 1);
            let _: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
            assert!(lines[0].contains("please refactor the rail"));

            // rollup
            let resp = get(&state, "/api/rollup?group_by=user").await;
            assert_eq!(resp.status(), StatusCode::OK);
            let body = body_string(resp).await;
            assert!(body.contains("\"session_count\":1"), "rollup: {body}");
            assert!(body.contains("alice@example.com"), "rollup label: {body}");

            // media with a Range header → 206 + Content-Range
            let resp = router(state.clone())
                .oneshot(
                    Request::builder()
                        .uri(format!("/api/media/{user}/{session}/{media_id}"))
                        .header(header::RANGE, "bytes=0-3")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(
                resp.headers().get(header::CONTENT_RANGE).unwrap(),
                "bytes 0-3/8"
            );
            assert_eq!(
                resp.headers().get(header::CONTENT_TYPE).unwrap(),
                "image/png"
            );
            let body = body_string(resp).await;
            assert_eq!(body, "PNGB");

            // unknown session → 404 plain text
            let missing = Uuid::from_u128(0xDEAD);
            let resp = get(&state, &format!("/api/sessions/{user}/{missing}/manifest")).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        }
    }
}
