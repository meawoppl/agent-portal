//! Durable media for `agent-portal show` (write-through + read-through).
//!
//! #1450 stores shown media in ephemeral, bounded stores: images in the
//! in-memory [`ImageStore`](crate::handlers::images::ImageStore) (TTL + LRU),
//! videos in the on-disk [`MediaStore`](crate::handlers::media_store::MediaStore)
//! (TTL + byte-cap). The persisted transcript row outlives the blob, so once
//! the blob is evicted an archived/backed-up session shows only a "media
//! expired" placeholder — the bytes are gone.
//!
//! **Why write-through (not archive-at-sweep):** the archive sweep only runs
//! after a session has been idle ~1h ([`ARCHIVE_IDLE_SECS`](crate::archive::ARCHIVE_IDLE_SECS)),
//! and the default blob TTL is also ~1h (`PORTAL_IMAGE_STORE_TTL_SECS`, reused
//! by the media store). Archiving media at sweep time therefore *races* blob
//! expiry and loses. So we copy the blob to the archive **at upload time**,
//! best-effort — the served store still holds the live copy for the immediate
//! view.
//!
//! **The media_id → session mapping** has no dedicated table: #1450 embeds the
//! served URL (`/api/images/{id}` / `/api/media/{id}`) in the portal transcript
//! row. [`resolve_media_owner`] recovers `(owner_user_id, session_id)` from that
//! row, which is all the read-through needs to locate the archived object
//! (co-located under the owner's session prefix) and to re-apply the same
//! ownership/membership auth as the live serve path.

use std::sync::Arc;

use diesel::prelude::*;
use tracing::{debug, error, warn};
use uuid::Uuid;

use shared::media::MediaKind;

use crate::archive::{ArchiveRuntime, ArchivedMediaMeta};
use crate::db::DbPool;
use crate::handlers::images::StoredImage;
use crate::handlers::media_store::MediaMeta;
use crate::markers::MEDIA_ARCHIVE_FAILED;
use crate::AppState;

/// URL path prefix under which a media kind is served.
fn url_prefix(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "images",
        MediaKind::Video => "media",
    }
}

fn kind_str(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
    }
}

/// One media blob to write through to the archive (built at the upload site).
pub struct MediaWriteThrough {
    /// Session owner (the archive keys media under the owner, like the manifest).
    pub owner_user_id: Uuid,
    pub session_id: Uuid,
    /// Served id from `/api/images/{id}` / `/api/media/{id}`.
    pub media_id: Uuid,
    pub kind: MediaKind,
    pub content_type: String,
    pub filename: Option<String>,
    pub bytes: Vec<u8>,
}

/// Best-effort write-through of an uploaded media blob to the session archive.
///
/// Never propagates failure — the upload has already stored the live copy and
/// must not fail because the archive is unhealthy. A failure is logged with the
/// [`MEDIA_ARCHIVE_FAILED`] marker and recorded in the archive stats. Runs on
/// the blocking pool (the object-store backend blocks on its async client).
pub fn write_through(runtime: &ArchiveRuntime, media: MediaWriteThrough) {
    let meta = ArchivedMediaMeta {
        media_id: media.media_id,
        kind: kind_str(media.kind).to_string(),
        content_type: media.content_type,
        filename: media.filename,
        bytes: media.bytes.len() as u64,
        uploaded_at: chrono::Utc::now().naive_utc(),
    };
    match runtime
        .store
        .put_media(media.owner_user_id, media.session_id, &meta, &media.bytes)
    {
        Ok(()) => {
            runtime.stats.record_success(meta.bytes);
            debug!(
                "Archived media {} ({} bytes) for session {}",
                media.media_id, meta.bytes, media.session_id
            );
        }
        Err(e) => {
            runtime.stats.record_failure(&e.to_string());
            error!(
                "{MEDIA_ARCHIVE_FAILED} media={} session={}: {e}",
                media.media_id, media.session_id
            );
        }
    }
}

/// Recover `(owner_user_id, session_id)` for a served media id by finding the
/// transcript row that references its URL. `None` when no row references it
/// (never shown, or its transcript was fully trimmed) or on DB error.
pub fn resolve_media_owner(pool: &DbPool, media_id: Uuid, kind: MediaKind) -> Option<(Uuid, Uuid)> {
    use crate::schema::{messages, sessions};

    let mut conn = match pool.get() {
        Ok(c) => c,
        Err(e) => {
            warn!("Media read-through: failed to get DB conn: {e}");
            return None;
        }
    };

    // The served URL appears verbatim in the stored portal JSON. media_id is a
    // UUID (no LIKE metacharacters), so the pattern is safe to interpolate.
    let needle = format!("%/api/{}/{}%", url_prefix(kind), media_id);
    let session_id: Uuid = messages::table
        .filter(messages::content.like(needle))
        .select(messages::session_id)
        .first(&mut conn)
        .optional()
        .ok()
        .flatten()?;

    // The archive keys media under the session *owner*'s prefix (matching
    // `manifest_key`/`transcript_key`), so resolve the owner, not the sender.
    sessions::table
        .find(session_id)
        .select(sessions::user_id)
        .first::<Uuid>(&mut conn)
        .optional()
        .ok()
        .flatten()
        .map(|owner| (owner, session_id))
}

/// Fetch an archived media blob's `(content_type, bytes)`, or `None` if the
/// archive is disabled, media archiving is off, or the blob is absent. The
/// sidecar is read first (its presence implies complete bytes).
fn fetch_archived(
    runtime: &ArchiveRuntime,
    owner_user_id: Uuid,
    session_id: Uuid,
    media_id: Uuid,
) -> Option<(String, Vec<u8>)> {
    if !runtime.config.media {
        return None;
    }
    let meta = runtime
        .store
        .get_media_meta(owner_user_id, session_id, media_id)
        .ok()
        .flatten()?;
    let bytes = runtime
        .store
        .get_media_bytes(owner_user_id, session_id, media_id)
        .ok()
        .flatten()?;
    Some((meta.content_type, bytes))
}

/// Read-through for images: on an `ImageStore` miss, fetch the archived blob,
/// re-warm the store under the same id, and return the entry. `None` falls back
/// to today's "media expired" behavior. Blocking (DB + object-store IO); call
/// via `spawn_blocking`.
pub fn readthrough_image(app_state: &AppState, media_id: Uuid) -> Option<Arc<StoredImage>> {
    let runtime = app_state.archive.as_ref()?;
    if !runtime.config.media {
        return None;
    }
    let (owner, session_id) = resolve_media_owner(&app_state.db_pool, media_id, MediaKind::Image)?;
    let (content_type, bytes) = fetch_archived(runtime, owner, session_id, media_id)?;
    debug!(
        "Re-warmed image {media_id} from archive ({} bytes)",
        bytes.len()
    );
    Some(app_state.image_store.insert_with_id(
        media_id,
        &content_type,
        bytes,
        owner,
        Some(session_id),
    ))
}

/// Read-through for videos: on a `MediaStore` miss, fetch the archived blob and
/// re-warm the on-disk store under the same id, then return its metadata so the
/// caller serves it through the normal path (Range support intact). We fetch
/// the whole object and re-warm rather than wiring ranged gets from the archive
/// — the extra round trip only happens once per eviction and keeps the serve
/// path (and its Range handling) a single code path. `None` falls back to
/// "media expired". Blocking; call via `spawn_blocking`.
pub fn readthrough_media(app_state: &AppState, media_id: Uuid) -> Option<MediaMeta> {
    let runtime = app_state.archive.as_ref()?;
    if !runtime.config.media {
        return None;
    }
    let (owner, session_id) = resolve_media_owner(&app_state.db_pool, media_id, MediaKind::Video)?;
    let (content_type, bytes) = fetch_archived(runtime, owner, session_id, media_id)?;
    debug!(
        "Re-warmed video {media_id} from archive ({} bytes)",
        bytes.len()
    );
    if let Err(e) = app_state.media_store.store_bytes_with_id(
        media_id,
        &content_type,
        &bytes,
        owner,
        Some(session_id),
    ) {
        warn!("Media read-through: failed to re-warm store for {media_id}: {e}");
        return None;
    }
    app_state.media_store.get(&media_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::{ArchiveBackendConfig, ArchiveConfig, ArchiveRuntime};
    use std::sync::atomic::Ordering;

    fn local_runtime(root: std::path::PathBuf) -> ArchiveRuntime {
        ArchiveRuntime::new(ArchiveConfig {
            backend: ArchiveBackendConfig::Local { root },
            transcripts: true,
            media: true,
        })
        .expect("local archive runtime")
    }

    #[test]
    fn write_through_puts_blob_and_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = local_runtime(tmp.path().to_path_buf());
        let (owner, session, media) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        write_through(
            &runtime,
            MediaWriteThrough {
                owner_user_id: owner,
                session_id: session,
                media_id: media,
                kind: MediaKind::Image,
                content_type: "image/png".into(),
                filename: Some("plot.png".into()),
                bytes: b"pixels".to_vec(),
            },
        );

        let bytes = runtime
            .store
            .get_media_bytes(owner, session, media)
            .unwrap()
            .expect("blob stored");
        assert_eq!(bytes, b"pixels");
        let meta = runtime
            .store
            .get_media_meta(owner, session, media)
            .unwrap()
            .expect("sidecar stored");
        assert_eq!(meta.content_type, "image/png");
        assert_eq!(meta.kind, "image");
        assert_eq!(meta.bytes, 6);
        assert_eq!(runtime.stats.failed_total.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn write_through_failure_records_marker_and_never_panics() {
        // Root points at a plain file, so `create_dir_all`/writes fail — the
        // MEDIA_ARCHIVE_FAILED path. Must be swallowed (best-effort) and
        // counted in the archive stats.
        let file = tempfile::NamedTempFile::new().unwrap();
        let runtime = local_runtime(file.path().to_path_buf());
        let (owner, session, media) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());

        write_through(
            &runtime,
            MediaWriteThrough {
                owner_user_id: owner,
                session_id: session,
                media_id: media,
                kind: MediaKind::Video,
                content_type: "video/mp4".into(),
                filename: None,
                bytes: b"frames".to_vec(),
            },
        );

        assert_eq!(runtime.stats.failed_total.load(Ordering::Relaxed), 1);
        // Nothing durable landed (the read itself may also error on the broken
        // root — either way, no bytes are recoverable).
        assert!(runtime
            .store
            .get_media_bytes(owner, session, media)
            .ok()
            .flatten()
            .is_none());
    }

    /// End-to-end read-through: an image whose live store entry is gone is
    /// recovered from the archive via the transcript's URL mapping and
    /// re-warmed under its original id. DB-gated.
    #[test]
    fn readthrough_recovers_evicted_image_from_archive() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        use crate::models::{NewMessage, NewSessionWithId, NewUser};
        use crate::schema::{messages, sessions, users};

        let mut conn = pool.get().expect("conn");
        let nonce = Uuid::new_v4();
        let owner: Uuid = diesel::insert_into(users::table)
            .values(&NewUser {
                google_id: format!("media_rt_{nonce}"),
                email: format!("media_rt_{nonce}@example.invalid"),
                name: Some("Media RT".into()),
                avatar_url: None,
            })
            .returning(users::id)
            .get_result::<Uuid>(&mut conn)
            .expect("insert user");

        let session_id = Uuid::new_v4();
        diesel::insert_into(sessions::table)
            .values(&NewSessionWithId {
                id: session_id,
                user_id: owner,
                session_name: format!("media-rt-{session_id}"),
                session_key: session_id.to_string(),
                working_directory: "/tmp".into(),
                status: shared::SessionStatus::Disconnected.as_str().to_string(),
                git_branch: None,
                client_version: None,
                hostname: "test".into(),
                launcher_id: None,
                agent_type: "claude".into(),
                repo_url: None,
                scheduled_task_id: None,
                paused: false,
                claude_args: serde_json::Value::Array(vec![]),
            })
            .execute(&mut conn)
            .expect("insert session");

        // The transcript row embeds the served URL — the durable media→session
        // mapping the read-through relies on.
        let media_id = Uuid::new_v4();
        let portal = shared::PortalMessage::with_content(vec![shared::PortalContent::Image {
            media_type: "image/png".into(),
            data: format!("/api/images/{media_id}"),
            file_path: Some("plot.png".into()),
            file_size: Some(6),
            source_type: Some("url".into()),
        }]);
        diesel::insert_into(messages::table)
            .values(&NewMessage {
                session_id,
                role: shared::MessageRole::Portal.to_string(),
                content: portal.to_json().to_string(),
                user_id: owner,
                agent_type: "claude".into(),
                provenance_kind: None,
                provenance_session_id: None,
                provenance_agent_type: None,
            })
            .execute(&mut conn)
            .expect("insert message");

        let tmp = tempfile::tempdir().unwrap();
        let mut state = crate::test_support::test_app_state(pool.clone());
        state.archive = Some(Arc::new(local_runtime(tmp.path().to_path_buf())));
        let runtime = state.archive.clone().unwrap();

        // Write-through the blob; the live image store is deliberately empty
        // (simulating post-eviction).
        write_through(
            &runtime,
            MediaWriteThrough {
                owner_user_id: owner,
                session_id,
                media_id,
                kind: MediaKind::Image,
                content_type: "image/png".into(),
                filename: Some("plot.png".into()),
                bytes: b"pixels".to_vec(),
            },
        );
        assert!(state.image_store.get(&media_id).is_none(), "store empty");

        let recovered = readthrough_image(&state, media_id).expect("recovered from archive");
        assert_eq!(recovered.data, b"pixels");
        assert_eq!(recovered.content_type, "image/png");
        assert_eq!(recovered.user_id, owner);
        assert_eq!(recovered.session_id, Some(session_id));
        // Re-warmed: a subsequent live-store fetch now hits.
        assert!(state.image_store.get(&media_id).is_some(), "re-warmed");

        // Cleanup.
        let _ = diesel::delete(messages::table.filter(messages::session_id.eq(session_id)))
            .execute(&mut conn);
        let _ = diesel::delete(sessions::table.find(session_id)).execute(&mut conn);
        let _ = diesel::delete(users::table.find(owner)).execute(&mut conn);
    }
}
