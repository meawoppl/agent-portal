//! In-memory image store for serving uploaded images via HTTP.
//!
//! Images are extracted from portal messages by the WebSocket handler,
//! stored in memory, and served at `/api/images/{id}`. This avoids
//! sending large base64 blobs over WebSocket to web clients.

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
};
use base64::Engine;
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, warn};
use uuid::Uuid;

/// A stored image with its content type and raw bytes
struct StoredImage {
    content_type: String,
    data: Vec<u8>,
}

/// An upload-in-progress: bytes accumulated so far, expected total, and limit.
struct PendingUpload {
    content_type: String,
    expected_bytes: u64,
    max_bytes: u64,
    buffer: Vec<u8>,
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

/// In-memory image store
#[derive(Clone, Default)]
pub struct ImageStore {
    images: Arc<DashMap<Uuid, StoredImage>>,
    pending: Arc<DashMap<Uuid, PendingUpload>>,
}

impl ImageStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a base64-encoded image, returning the UUID key.
    /// Returns None if the base64 data is invalid.
    pub fn store_base64(&self, content_type: &str, base64_data: &str) -> Option<Uuid> {
        let data = base64::engine::general_purpose::STANDARD
            .decode(base64_data)
            .ok()?;
        let id = Uuid::new_v4();
        debug!(
            "Stored image {} ({}, {} bytes)",
            id,
            content_type,
            data.len()
        );
        self.images.insert(
            id,
            StoredImage {
                content_type: content_type.to_string(),
                data,
            },
        );
        Some(id)
    }

    /// Begin a chunked upload. Reserves an entry in the pending map and
    /// fails up front if `expected_bytes` already exceeds the limit.
    pub fn start_upload(
        &self,
        upload_id: Uuid,
        content_type: &str,
        expected_bytes: u64,
        max_bytes: u64,
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
            "Stored image {} via chunked upload {} ({}, {} bytes)",
            id, upload_id, pending.content_type, actual
        );
        self.images.insert(
            id,
            StoredImage {
                content_type: pending.content_type,
                data: pending.buffer,
            },
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

    /// Get the number of stored images
    pub fn count(&self) -> usize {
        self.images.len()
    }
}

/// GET /api/images/{id} - Serve a stored image
pub async fn serve_image(
    State(app_state): State<Arc<crate::AppState>>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let image = app_state
        .image_store
        .images
        .get(&id)
        .ok_or(StatusCode::NOT_FOUND)?;

    Ok((
        [
            (header::CONTENT_TYPE, image.content_type.clone()),
            (
                header::CACHE_CONTROL,
                "public, max-age=86400, immutable".to_string(),
            ),
        ],
        image.data.clone(),
    ))
}
