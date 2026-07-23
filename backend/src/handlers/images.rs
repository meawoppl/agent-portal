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
use diesel::prelude::*;
use mini_moka::sync::Cache;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::CurrentUserId;
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
/// `session_access::verify_session_reader` joins `session_members` for the
/// message-list endpoint. Both fields are mandatory for new inserts; older
/// images from before this PR are no longer in the bounded LRU and 404 cleanly.
pub struct StoredImage {
    pub content_type: String,
    pub data: Vec<u8>,
    pub user_id: Uuid,
    pub session_id: Option<Uuid>,
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
        Self { images }
    }

    /// Build a store with the project defaults.
    #[cfg(test)]
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

    /// Store raw image bytes, returning the UUID key. Used by the
    /// `agent-portal show` media endpoint, which receives the file bytes
    /// directly over HTTP (no base64 hop). `user_id`/`session_id` gate
    /// `serve_image` the same way as [`Self::store_base64`].
    pub fn store_bytes(
        &self,
        content_type: &str,
        data: Vec<u8>,
        user_id: Uuid,
        session_id: Option<Uuid>,
    ) -> Uuid {
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
        id
    }

    /// Insert bytes under a caller-supplied id, returning the stored entry.
    /// Used by the archive read-through to re-warm the cache under the media's
    /// original id (so the same `/api/images/{id}` URL resolves again) after
    /// its live entry was evicted. `store_bytes` mints a fresh id and so can't
    /// be used to restore a specific one.
    pub fn insert_with_id(
        &self,
        id: Uuid,
        content_type: &str,
        data: Vec<u8>,
        user_id: Uuid,
        session_id: Option<Uuid>,
    ) -> Arc<StoredImage> {
        let stored = Arc::new(StoredImage {
            content_type: content_type.to_string(),
            data,
            user_id,
            session_id,
        });
        self.images.insert(id, stored.clone());
        stored
    }

    /// Fetch a stored image by id, or `None` if it was never stored, has
    /// expired past its TTL, or has been evicted to stay under the byte cap.
    pub fn get(&self, id: &Uuid) -> Option<Arc<StoredImage>> {
        self.images.get(id)
    }
}

/// GET /api/images/{id} - Serve a stored image.
///
/// Authenticated route (closes #786). Returns the bytes only if the caller
/// is the inserting user OR is a member of the session that owns the image
/// (matching how `session_access::verify_session_reader` gates message reads).
/// On any access-failure we return `404 NOT_FOUND` — the same wire shape as
/// "no such image" — so callers can't use the response to probe for the
/// existence of an image they don't own (no existence oracle on UUIDs).
///
/// `Cache-Control: private` ensures the per-user response is never stored by
/// a shared cache between us and the browser (we used to send `public` —
/// fine when every image was world-readable, wrong now that auth gates it).
pub async fn serve_image(
    State(app_state): State<Arc<crate::AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, AppError> {
    let image = match app_state.image_store.get(&id) {
        Some(image) => image,
        None => {
            // Live copy evicted/expired: try the durable archive (#1450 media
            // is ephemeral). Re-warms the store under the same id on hit.
            let state = app_state.clone();
            tokio::task::spawn_blocking(move || {
                crate::handlers::media_archive::readthrough_image(&state, id)
            })
            .await
            .ok()
            .flatten()
            .ok_or(AppError::NotFound("Image not found"))?
        }
    };

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
/// Mirrors `session_access::verify_session_reader` but as a boolean — we don't
/// need the full `Session` row here, just the membership check. Any DB error
/// is treated as "not a member" (deny by default) since the only signal we
/// emit to the client is the 404 above.
pub(crate) fn user_is_session_member(
    db_pool: &crate::db::DbPool,
    session_id: Uuid,
    user_id: Uuid,
) -> bool {
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
}
