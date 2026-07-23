//! Disk-backed media store for serving videos uploaded via `agent-portal show`.
//!
//! WHY A SEPARATE STORE (not the in-memory `ImageStore`): the image store is a
//! `mini-moka` LRU capped at 256 MiB of **resident** bytes. Videos are large
//! (default per-file cap 100 MB) — dropping one into that cache would evict
//! every image to stay under the byte budget, and hold ~100 MB resident per
//! video. So videos live on disk under a temp root; only lightweight metadata
//! (content type, size, owner, path) is kept in memory. The store is bounded by
//! a total-byte cap (LRU-ish: oldest evicted first) and a per-entry TTL, swept
//! periodically and lazily on `get`. Because bytes are TTL/size-bounded, a
//! persisted transcript row can outlive its blob — `serve_media` then 404s and
//! the frontend renders a "media expired" placeholder.
//!
//! SEAM: only whole-file + single-range serving is implemented (enough for
//! `<video>` playback and scrubbing in browsers). Multipart ranges, on-the-fly
//! transcoding, and thumbnail generation are deliberately out of scope for v1.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use dashmap::DashMap;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::CurrentUserId;
use crate::errors::AppError;

/// Default total-byte cap for the on-disk media store (1 GiB).
pub const DEFAULT_MEDIA_STORE_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// One stored media blob: bytes live at `path`, metadata lives here.
struct MediaEntry {
    content_type: String,
    path: PathBuf,
    size: u64,
    user_id: Uuid,
    session_id: Option<Uuid>,
    created: Instant,
}

/// Resolved metadata for a served-media fetch (auth + streaming inputs).
pub struct MediaMeta {
    pub content_type: String,
    pub path: PathBuf,
    pub size: u64,
    pub user_id: Uuid,
    pub session_id: Option<Uuid>,
}

struct MediaStoreInner {
    root: PathBuf,
    entries: DashMap<Uuid, MediaEntry>,
    max_bytes: u64,
    ttl: Duration,
    total_bytes: AtomicU64,
}

/// Disk-backed, TTL + byte-capped store for large media (video).
#[derive(Clone)]
pub struct MediaStore {
    inner: Arc<MediaStoreInner>,
}

impl MediaStore {
    /// Build a store rooted at `root` (created if missing). Any pre-existing
    /// files under `root` from a previous run are removed so a crash doesn't
    /// leak orphaned blobs into the byte budget.
    pub fn new(root: PathBuf, max_bytes: u64, ttl: Duration) -> std::io::Result<Self> {
        std::fs::create_dir_all(&root)?;
        // Clear stale blobs from an earlier process; their metadata is gone.
        if let Ok(read_dir) = std::fs::read_dir(&root) {
            for entry in read_dir.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        Ok(Self {
            inner: Arc::new(MediaStoreInner {
                root,
                entries: DashMap::new(),
                max_bytes,
                ttl,
                total_bytes: AtomicU64::new(0),
            }),
        })
    }

    /// Persist `data` to disk and register it, returning the media id.
    ///
    /// Enforces the total-byte cap by evicting the oldest entries first; a
    /// single blob larger than the whole cap is rejected (it could never fit).
    pub fn store_bytes(
        &self,
        content_type: &str,
        data: &[u8],
        user_id: Uuid,
        session_id: Option<Uuid>,
    ) -> std::io::Result<Uuid> {
        let id = Uuid::new_v4();
        self.store_bytes_with_id(id, content_type, data, user_id, session_id)?;
        Ok(id)
    }

    /// Persist `data` under a caller-supplied id. Used by the archive
    /// read-through to re-warm the store under the media's original id (so the
    /// same `/api/media/{id}` URL — and its Range serving — resolves again)
    /// after its live entry was evicted.
    pub fn store_bytes_with_id(
        &self,
        id: Uuid,
        content_type: &str,
        data: &[u8],
        user_id: Uuid,
        session_id: Option<Uuid>,
    ) -> std::io::Result<()> {
        let size = data.len() as u64;
        if size > self.inner.max_bytes {
            return Err(std::io::Error::other(
                "media exceeds the total media-store capacity",
            ));
        }

        self.sweep_expired();
        self.evict_until_fits(size);

        let path = self.inner.root.join(id.to_string());
        std::fs::write(&path, data)?;

        self.inner.total_bytes.fetch_add(size, Ordering::Relaxed);
        self.inner.entries.insert(
            id,
            MediaEntry {
                content_type: content_type.to_string(),
                path,
                size,
                user_id,
                session_id,
                created: Instant::now(),
            },
        );
        debug!(
            "Stored media {} ({}, {} bytes, user={}, session={:?})",
            id, content_type, size, user_id, session_id
        );
        Ok(())
    }

    /// Fetch metadata for `id`, or `None` if it never existed, has expired past
    /// its TTL (removed here, lazily), or was evicted to stay under the cap.
    pub fn get(&self, id: &Uuid) -> Option<MediaMeta> {
        let expired = {
            let entry = self.inner.entries.get(id)?;
            entry.created.elapsed() > self.inner.ttl
        };
        if expired {
            self.remove(id);
            return None;
        }
        let entry = self.inner.entries.get(id)?;
        Some(MediaMeta {
            content_type: entry.content_type.clone(),
            path: entry.path.clone(),
            size: entry.size,
            user_id: entry.user_id,
            session_id: entry.session_id,
        })
    }

    /// Periodic maintenance: drop expired entries and re-enforce the byte cap.
    /// Called from the background sweep task and lazily from `store_bytes`.
    pub fn sweep(&self) {
        self.sweep_expired();
        self.evict_until_fits(0);
    }

    fn remove(&self, id: &Uuid) {
        if let Some((_, entry)) = self.inner.entries.remove(id) {
            let _ = std::fs::remove_file(&entry.path);
            self.inner
                .total_bytes
                .fetch_sub(entry.size, Ordering::Relaxed);
        }
    }

    fn sweep_expired(&self) {
        let ttl = self.inner.ttl;
        let expired: Vec<Uuid> = self
            .inner
            .entries
            .iter()
            .filter(|e| e.value().created.elapsed() > ttl)
            .map(|e| *e.key())
            .collect();
        for id in expired {
            self.remove(&id);
        }
    }

    /// Evict oldest-first until `total_bytes + incoming <= max_bytes`.
    fn evict_until_fits(&self, incoming: u64) {
        loop {
            let total = self.inner.total_bytes.load(Ordering::Relaxed);
            if total + incoming <= self.inner.max_bytes {
                return;
            }
            // Find the oldest surviving entry.
            let oldest = self
                .inner
                .entries
                .iter()
                .min_by_key(|e| e.value().created)
                .map(|e| *e.key());
            match oldest {
                Some(id) => {
                    warn!("Media store over cap; evicting oldest entry {}", id);
                    self.remove(&id);
                }
                None => return, // nothing left to evict
            }
        }
    }
}

/// Parse a single-range `Range: bytes=start-end` header against `total` bytes.
/// Returns the inclusive `(start, end)` byte range, or `None` when the header
/// is absent/malformed (caller then serves the whole file). Returns
/// `Some(Err(()))` when the range is syntactically valid but unsatisfiable.
#[allow(clippy::type_complexity)]
fn parse_range(headers: &HeaderMap, total: u64) -> Option<Result<(u64, u64), ()>> {
    let raw = headers.get(header::RANGE)?.to_str().ok()?;
    let spec = raw.strip_prefix("bytes=")?;
    // Only a single range is supported; reject multi-range specs.
    if spec.contains(',') {
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

/// GET /api/media/{id} — serve a stored video, with HTTP Range support so
/// browsers can scrub. Authenticated the same way as `serve_image`: the caller
/// must be the uploader or a member of the owning session, else `404`.
pub async fn serve_media(
    State(app_state): State<Arc<crate::AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Response, AppError> {
    let meta = match app_state.media_store.get(&id) {
        Some(meta) => meta,
        None => {
            // Live copy evicted/expired: try the durable archive (#1450 media
            // is ephemeral). Re-warms the on-disk store under the same id, so
            // Range serving below is unchanged.
            let state = app_state.clone();
            tokio::task::spawn_blocking(move || {
                crate::handlers::media_archive::readthrough_media(&state, id)
            })
            .await
            .ok()
            .flatten()
            .ok_or(AppError::NotFound("Media not found"))?
        }
    };

    if meta.user_id != current_user_id {
        let allowed = match meta.session_id {
            Some(session_id) => super::images::user_is_session_member(
                &app_state.db_pool,
                session_id,
                current_user_id,
            ),
            None => false,
        };
        if !allowed {
            return Err(AppError::NotFound("Media not found"));
        }
    }

    let total = meta.size;
    let range = parse_range(&headers, total);

    match range {
        Some(Err(())) => {
            // Syntactically valid but unsatisfiable range.
            Ok(Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{total}"))
                .body(Body::empty())
                .map_err(|e| AppError::Internal(format!("build range response: {e}")))?)
        }
        Some(Ok((start, end))) => {
            let len = end - start + 1;
            let mut file = tokio::fs::File::open(&meta.path)
                .await
                .map_err(|_| AppError::NotFound("Media not found"))?;
            file.seek(std::io::SeekFrom::Start(start))
                .await
                .map_err(|e| AppError::Internal(format!("seek media: {e}")))?;
            let stream = ReaderStream::new(file.take(len));
            Ok(Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, meta.content_type)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, len)
                .header(
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end}/{total}"),
                )
                .header(header::CACHE_CONTROL, "private, max-age=86400, immutable")
                .body(Body::from_stream(stream))
                .map_err(|e| AppError::Internal(format!("build range response: {e}")))?)
        }
        None => {
            let file = tokio::fs::File::open(&meta.path)
                .await
                .map_err(|_| AppError::NotFound("Media not found"))?;
            let stream = ReaderStream::new(file);
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, meta.content_type)
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CONTENT_LENGTH, total)
                .header(header::CACHE_CONTROL, "private, max-age=86400, immutable")
                .body(Body::from_stream(stream))
                .map_err(|e| AppError::Internal(format!("build media response: {e}")))?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(max_bytes: u64, ttl: Duration) -> MediaStore {
        let root = std::env::temp_dir().join(format!("media-store-test-{}", Uuid::new_v4()));
        MediaStore::new(root, max_bytes, ttl).expect("create store")
    }

    #[test]
    fn roundtrip_returns_bytes_metadata() {
        let store = tmp_store(1024 * 1024, Duration::from_secs(60));
        let user = Uuid::new_v4();
        let session = Uuid::new_v4();
        let id = store
            .store_bytes("video/mp4", b"hello-video", user, Some(session))
            .expect("stored");
        let meta = store.get(&id).expect("present");
        assert_eq!(meta.content_type, "video/mp4");
        assert_eq!(meta.size, 11);
        assert_eq!(meta.user_id, user);
        assert_eq!(meta.session_id, Some(session));
        assert_eq!(std::fs::read(&meta.path).unwrap(), b"hello-video");
    }

    #[test]
    fn entries_past_ttl_are_dropped_on_get() {
        let store = tmp_store(1024 * 1024, Duration::from_millis(20));
        let id = store
            .store_bytes("video/mp4", b"data", Uuid::new_v4(), None)
            .expect("stored");
        let path = store.get(&id).expect("present").path;
        std::thread::sleep(Duration::from_millis(40));
        assert!(store.get(&id).is_none(), "expired entry must not return");
        assert!(!path.exists(), "expired blob file should be deleted");
    }

    #[test]
    fn exceeding_byte_cap_evicts_oldest() {
        // Cap 300 bytes; three 200-byte blobs. Each insert evicts to fit.
        let store = tmp_store(300, Duration::from_secs(60));
        let first = store
            .store_bytes("video/mp4", &[0u8; 200], Uuid::new_v4(), None)
            .expect("first");
        std::thread::sleep(Duration::from_millis(5));
        let second = store
            .store_bytes("video/mp4", &[0u8; 200], Uuid::new_v4(), None)
            .expect("second");
        assert!(store.get(&first).is_none(), "oldest should be evicted");
        assert!(store.get(&second).is_some(), "newest should survive");
    }

    #[test]
    fn single_blob_larger_than_cap_is_rejected() {
        let store = tmp_store(100, Duration::from_secs(60));
        let err = store
            .store_bytes("video/mp4", &[0u8; 200], Uuid::new_v4(), None)
            .expect_err("should reject oversize blob");
        assert_eq!(err.kind(), std::io::ErrorKind::Other);
    }

    #[test]
    fn parse_range_variants() {
        let mut headers = HeaderMap::new();
        // No Range header → whole file.
        assert!(parse_range(&headers, 100).is_none());

        headers.insert(header::RANGE, "bytes=0-49".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Ok((0, 49))));

        headers.insert(header::RANGE, "bytes=50-".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Ok((50, 99))));

        headers.insert(header::RANGE, "bytes=-20".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Ok((80, 99))));

        // End past EOF clamps to last byte.
        headers.insert(header::RANGE, "bytes=90-200".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Ok((90, 99))));

        // Start past EOF is unsatisfiable.
        headers.insert(header::RANGE, "bytes=200-".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Err(())));

        // Multi-range unsupported.
        headers.insert(header::RANGE, "bytes=0-10,20-30".parse().unwrap());
        assert_eq!(parse_range(&headers, 100), Some(Err(())));
    }
}
