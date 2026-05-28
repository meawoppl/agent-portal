//! In-memory image store for serving uploaded images via HTTP.
//!
//! Images are extracted from portal messages by the WebSocket handler,
//! stored in memory, and served at `/api/images/{id}`. This avoids
//! sending large base64 blobs over WebSocket to web clients.
//!
//! The store is bounded by both a total-byte cap and a per-entry TTL via
//! `mini-moka` (LRU + TTL cache). Without these caps, a long session that
//! streams image-heavy output would grow backend memory indefinitely and
//! eventually OOM the process. See issue #787.

use axum::{
    extract::{Path, State},
    http::header,
    response::IntoResponse,
};
use base64::Engine;
use dashmap::DashMap;
use diesel::prelude::*;
use mini_moka::sync::Cache;
use std::sync::Arc;
use std::time::Duration;
use tower_cookies::Cookies;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::extract_user_id;
use crate::errors::AppError;

/// Default total-byte cap for the served image cache (256 MiB).
pub const DEFAULT_IMAGE_STORE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// Default per-entry TTL for the served image cache (1 hour).
pub const DEFAULT_IMAGE_STORE_TTL: Duration = Duration::from_secs(3600);

/// A stored image with its content type and raw bytes.
///
/// Wrapped in `Arc` inside the cache so `Cache::get` (which returns by clone)
/// doesn't copy the underlying bytes on every fetch.
///
/// `user_id` is the inserting user — used as the primary ownership check on
/// `GET /api/images/{id}` (closes #786). `session_id` (if present) lets other
/// members of the same session also read the image, mirroring how
/// `verify_session_access` in `messages.rs` joins `session_members` for the
/// message-list endpoint. Both fields are mandatory for new inserts; older
/// images from before this PR are no longer in the bounded LRU and 404 cleanly.
pub struct StoredImage {
    pub content_type: String,
    pub data: Vec<u8>,
    pub user_id: Uuid,
    pub session_id: Option<Uuid>,
}

/// An upload-in-progress: bytes accumulated so far, expected total, and limit.
struct PendingUpload {
    content_type: String,
    expected_bytes: u64,
    max_bytes: u64,
    buffer: Vec<u8>,
    user_id: Uuid,
    session_id: Option<Uuid>,
}

/// Errors that can occur while accumulating a chunked upload.
#[derive(Debug)]
pub enum UploadError {
    UnknownUpload,
    SizeExceedsLimit { limit_bytes: u64 },
    OffsetMismatch { expected: u64, got: u64 },
    SizeMismatch { expected: u64, got: u64 },
}

impl std::fmt::Display for UploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownUpload => write!(f, "unknown upload_id"),
            Self::SizeExceedsLimit { limit_bytes } => {
                let mb = *limit_bytes as f64 / (1024.0 * 1024.0);
                write!(f, "upload exceeds size limit of {:.0} MB", mb)
            }
            Self::OffsetMismatch { expected, got } => {
                write!(
                    f,
                    "chunk offset mismatch: expected {}, got {}",
                    expected, got
                )
            }
            Self::SizeMismatch { expected, got } => write!(
                f,
                "upload size mismatch: expected {} bytes, got {}",
                expected, got
            ),
        }
    }
}

/// In-memory image store with TTL + byte-cap LRU eviction.
///
/// Backed by `mini_moka::sync::Cache<Uuid, Arc<StoredImage>>` with a weigher
/// that returns each entry's byte length so `max_capacity` is interpreted as
/// a total-bytes budget. Entries that go un-fetched for `time_to_live` are
/// dropped automatically; when an insert would push total bytes past the cap,
/// the least-recently-used entries are evicted first.
#[derive(Clone)]
pub struct ImageStore {
    images: Cache<Uuid, Arc<StoredImage>>,
    pending: Arc<DashMap<Uuid, PendingUpload>>,
}

impl ImageStore {
    /// Build a store with the given total-byte cap and per-entry TTL.
    pub fn new(max_bytes: u64, ttl: Duration) -> Self {
        let images = Cache::builder()
            .max_capacity(max_bytes)
            .weigher(|_k: &Uuid, v: &Arc<StoredImage>| {
                // `weigher` returns u32; saturate so a >4 GiB entry doesn't
                // wrap to a tiny weight and silently dodge eviction.
                u32::try_from(v.data.len()).unwrap_or(u32::MAX)
            })
            .time_to_live(ttl)
            .build();
        Self {
            images,
            pending: Arc::new(DashMap::new()),
        }
    }

    /// Build a store with the project defaults — for tests and as a fallback
    /// when env vars aren't set.
    pub fn with_defaults() -> Self {
        Self::new(DEFAULT_IMAGE_STORE_MAX_BYTES, DEFAULT_IMAGE_STORE_TTL)
    }

    /// Store a base64-encoded image, returning the UUID key.
    /// Returns None if the base64 data is invalid.
    ///
    /// `user_id` is the inserting user (used for the ownership check on
    /// `serve_image`); `session_id` lets session members read the image too.
    pub fn store_base64(
        &self,
        content_type: &str,
        base64_data: &str,
        user_id: Uuid,
        session_id: Option<Uuid>,
    ) -> Option<Uuid> {
        let data = base64::engine::general_purpose::STANDARD
            .decode(base64_data)
            .ok()?;
        let id = Uuid::new_v4();
        debug!(
            "Stored image {} ({}, {} bytes, user={}, session={:?})",
            id,
            content_type,
            data.len(),
            user_id,
            session_id,
        );
        self.images.insert(
            id,
            Arc::new(StoredImage {
                content_type: content_type.to_string(),
                data,
                user_id,
                session_id,
            }),
        );
        Some(id)
    }

    /// Begin a chunked upload. Reserves an entry in the pending map and
    /// fails up front if `expected_bytes` already exceeds the limit.
    ///
    /// `user_id` is the inserting user (validated via the proxy auth token);
    /// `session_id` lets other session members read the finalized image.
    pub fn start_upload(
        &self,
        upload_id: Uuid,
        content_type: &str,
        expected_bytes: u64,
        max_bytes: u64,
        user_id: Uuid,
        session_id: Option<Uuid>,
    ) -> Result<(), UploadError> {
        if expected_bytes > max_bytes {
            return Err(UploadError::SizeExceedsLimit {
                limit_bytes: max_bytes,
            });
        }
        self.pending.insert(
            upload_id,
            PendingUpload {
                content_type: content_type.to_string(),
                expected_bytes,
                max_bytes,
                buffer: Vec::with_capacity(expected_bytes as usize),
                user_id,
                session_id,
            },
        );
        Ok(())
    }

    /// Append a chunk at the given offset. Chunks must arrive in order
    /// (offset must equal `buffer.len()`); we enforce that to keep the
    /// accumulator a simple `Vec<u8>` without sparse reassembly.
    pub fn append_chunk(
        &self,
        upload_id: Uuid,
        offset: u64,
        data: &[u8],
    ) -> Result<(), UploadError> {
        let mut entry = self
            .pending
            .get_mut(&upload_id)
            .ok_or(UploadError::UnknownUpload)?;
        let pending = entry.value_mut();
        if pending.buffer.len() as u64 != offset {
            return Err(UploadError::OffsetMismatch {
                expected: pending.buffer.len() as u64,
                got: offset,
            });
        }
        let new_len = pending.buffer.len() as u64 + data.len() as u64;
        if new_len > pending.max_bytes {
            return Err(UploadError::SizeExceedsLimit {
                limit_bytes: pending.max_bytes,
            });
        }
        pending.buffer.extend_from_slice(data);
        Ok(())
    }

    /// Finalize an upload, moving the accumulated bytes into the served
    /// image store and returning the assigned image id.
    pub fn finalize_upload(&self, upload_id: Uuid) -> Result<Uuid, UploadError> {
        let (_, pending) = self
            .pending
            .remove(&upload_id)
            .ok_or(UploadError::UnknownUpload)?;
        let actual = pending.buffer.len() as u64;
        if actual != pending.expected_bytes {
            return Err(UploadError::SizeMismatch {
                expected: pending.expected_bytes,
                got: actual,
            });
        }
        let id = Uuid::new_v4();
        debug!(
            "Stored image {} via chunked upload {} ({}, {} bytes, user={}, session={:?})",
            id, upload_id, pending.content_type, actual, pending.user_id, pending.session_id,
        );
        self.images.insert(
            id,
            Arc::new(StoredImage {
                content_type: pending.content_type,
                data: pending.buffer,
                user_id: pending.user_id,
                session_id: pending.session_id,
            }),
        );
        Ok(id)
    }

    /// Drop a pending upload without finalizing — used when the connection
    /// drops mid-stream or the client cancels.
    pub fn abort_upload(&self, upload_id: Uuid) {
        if self.pending.remove(&upload_id).is_some() {
            warn!("Aborted in-progress image upload {}", upload_id);
        }
    }

    /// Fetch a stored image by id, or `None` if it was never stored, has
    /// expired past its TTL, or has been evicted to stay under the byte cap.
    pub fn get(&self, id: &Uuid) -> Option<Arc<StoredImage>> {
        self.images.get(id)
    }

    /// Approximate number of live entries — exposed for telemetry/tests.
    /// Note: mini-moka's `entry_count` is best-effort and may briefly include
    /// entries that have just been scheduled for eviction.
    pub fn count(&self) -> u64 {
        self.images.entry_count()
    }
}

/// GET /api/images/{id} - Serve a stored image.
///
/// Authenticated route (closes #786). Returns the bytes only if the caller
/// is the inserting user OR is a member of the session that owns the image
/// (matching how `messages.rs::verify_session_access` gates message reads).
/// On any access-failure we return `404 NOT_FOUND` — the same wire shape as
/// "no such image" — so callers can't use the response to probe for the
/// existence of an image they don't own (no existence oracle on UUIDs).
///
/// `Cache-Control: private` ensures the per-user response is never stored by
/// a shared cache between us and the browser (we used to send `public` —
/// fine when every image was world-readable, wrong now that auth gates it).
pub async fn serve_image(
    State(app_state): State<Arc<crate::AppState>>,
    cookies: Cookies,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    // Auth first — if the caller has no session cookie we don't even tell
    // them whether the image exists.
    let current_user_id = extract_user_id(&app_state, &cookies)?;

    let image = app_state
        .image_store
        .get(&id)
        .ok_or(AppError::NotFound("Image not found"))?;

    if image.user_id != current_user_id {
        // Inserter is someone else — fall back to session-member sharing if
        // the image is bound to a session, otherwise reject as not-found.
        let allowed = match image.session_id {
            Some(session_id) => {
                user_is_session_member(&app_state.db_pool, session_id, current_user_id)
            }
            None => false,
        };
        if !allowed {
            return Err(AppError::NotFound("Image not found"));
        }
    }

    Ok((
        [
            (header::CONTENT_TYPE, image.content_type.clone()),
            (
                header::CACHE_CONTROL,
                "private, max-age=86400, immutable".to_string(),
            ),
        ],
        image.data.clone(),
    ))
}

/// Does `user_id` appear in `session_members` for `session_id`?
///
/// Mirrors `messages.rs::verify_session_access` but as a boolean — we don't
/// need the full `Session` row here, just the membership check. Any DB error
/// is treated as "not a member" (deny by default) since the only signal we
/// emit to the client is the 404 above.
fn user_is_session_member(db_pool: &crate::db::DbPool, session_id: Uuid, user_id: Uuid) -> bool {
    use crate::schema::session_members;

    let mut conn = match db_pool.get() {
        Ok(c) => c,
        Err(e) => {
            warn!("Image auth: failed to get DB conn: {}", e);
            return false;
        }
    };

    diesel::dsl::select(diesel::dsl::exists(
        session_members::table
            .filter(session_members::session_id.eq(session_id))
            .filter(session_members::user_id.eq(user_id)),
    ))
    .get_result::<bool>(&mut conn)
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mini_moka::sync::ConcurrentCacheExt;

    /// Mini-moka maintenance (eviction + TTL sweeps) is asynchronous: writes
    /// land in a buffered channel and the housekeeper drains it on subsequent
    /// ops. Tests that observe post-cap or post-TTL state must call `sync()`
    /// (from `ConcurrentCacheExt`) to flush that buffer synchronously, the
    /// same pattern the crate's own tests use.
    fn flush(store: &ImageStore) {
        store.images.sync();
    }

    fn insert_bytes(store: &ImageStore, n: usize) -> Uuid {
        let data = vec![0u8; n];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&data);
        store
            .store_base64("image/png", &b64, Uuid::new_v4(), None)
            .expect("decoded")
    }

    #[test]
    fn inserting_beyond_byte_cap_evicts_oldest() {
        // Cap: 4 KiB. Stream many ~1 KiB images so the weighted size stays
        // bounded. Mini-moka's admission policy is TinyLFU-flavored, so we
        // assert on the cap invariant (weighted_size <= max_capacity) rather
        // than pinning *which* entry was evicted.
        let max_bytes = 4 * 1024;
        let store = ImageStore::new(max_bytes, Duration::from_secs(60));

        // 32 inserts of 1 KiB each = 32 KiB of pressure against a 4 KiB cap.
        // Re-fetch each id immediately after insert so TinyLFU sees some
        // admission frequency; otherwise the cap is enforced trivially by
        // refusing new admits and the test wouldn't exercise eviction.
        let ids: Vec<Uuid> = (0..32)
            .map(|_| {
                let id = insert_bytes(&store, 1024);
                let _ = store.get(&id);
                id
            })
            .collect();
        flush(&store);

        // Cap invariant: total stored bytes never exceed the configured max.
        let weighted = store.images.weighted_size();
        assert!(
            weighted <= max_bytes,
            "weighted_size {} should not exceed max_capacity {}",
            weighted,
            max_bytes
        );

        // Eviction actually happened — most of the 32 inserts shouldn't be
        // sitting in a 4 KiB cache.
        let surviving: usize = ids.iter().filter(|id| store.get(id).is_some()).count();
        assert!(
            surviving < ids.len(),
            "byte-cap should have evicted entries; {}/{} survived",
            surviving,
            ids.len()
        );
    }

    #[test]
    fn entries_past_ttl_are_not_returned() {
        // TTL: 50 ms. Insert, observe present, wait past TTL, observe miss.
        // The TTL check fires on `get` itself, so no explicit flush needed
        // for the negative observation — but we flush to be deterministic.
        let store = ImageStore::new(1024 * 1024, Duration::from_millis(50));
        let id = insert_bytes(&store, 64);
        assert!(store.get(&id).is_some(), "fresh entry should be present");

        std::thread::sleep(Duration::from_millis(120));
        flush(&store);

        assert!(
            store.get(&id).is_none(),
            "entry past TTL should not be returned"
        );
    }

    #[test]
    fn roundtrip_smoke_test() {
        // Sanity: typical insert/fetch path returns matching bytes + content type.
        let store = ImageStore::with_defaults();
        let payload = b"\x89PNG\r\n\x1a\n-fake-png-bytes";
        let b64 = base64::engine::general_purpose::STANDARD.encode(payload);
        let user = Uuid::new_v4();
        let session = Uuid::new_v4();
        let id = store
            .store_base64("image/png", &b64, user, Some(session))
            .expect("decoded");
        let fetched = store.get(&id).expect("present");
        assert_eq!(fetched.content_type, "image/png");
        assert_eq!(fetched.data, payload);
        assert_eq!(fetched.user_id, user);
        assert_eq!(fetched.session_id, Some(session));
    }

    #[test]
    fn invalid_base64_returns_none() {
        let store = ImageStore::with_defaults();
        assert!(store
            .store_base64("image/png", "not!!base64", Uuid::new_v4(), None)
            .is_none());
    }

    #[test]
    fn stored_image_carries_owner_user_id() {
        // The serve_image ownership check (issue #786) reads `user_id` off the
        // cache entry — pin that the field round-trips so a future refactor
        // can't silently drop it.
        let store = ImageStore::with_defaults();
        let owner = Uuid::new_v4();
        let session = Uuid::new_v4();
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"x");
        let id = store
            .store_base64("image/png", &b64, owner, Some(session))
            .expect("decoded");
        let fetched = store.get(&id).expect("present");
        assert_eq!(fetched.user_id, owner);
        assert_eq!(fetched.session_id, Some(session));
    }

    #[test]
    fn finalize_upload_carries_owner_and_session() {
        // Chunked-upload path: user/session are captured at start_upload and
        // must end up on the StoredImage at finalize_upload (same ownership
        // check applies — issue #786).
        let store = ImageStore::with_defaults();
        let owner = Uuid::new_v4();
        let session = Uuid::new_v4();
        let upload_id = Uuid::new_v4();
        let payload = b"hello";
        store
            .start_upload(
                upload_id,
                "image/png",
                payload.len() as u64,
                1024 * 1024,
                owner,
                Some(session),
            )
            .expect("start ok");
        store.append_chunk(upload_id, 0, payload).expect("chunk ok");
        let id = store.finalize_upload(upload_id).expect("finalize ok");
        let fetched = store.get(&id).expect("present");
        assert_eq!(fetched.user_id, owner);
        assert_eq!(fetched.session_id, Some(session));
        assert_eq!(fetched.data, payload);
    }
}
