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
use tracing::debug;
use uuid::Uuid;

/// A stored image with its content type and raw bytes
struct StoredImage {
    content_type: String,
    data: Vec<u8>,
}

/// In-memory image store
#[derive(Clone, Default)]
pub struct ImageStore {
    images: Arc<DashMap<Uuid, StoredImage>>,
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
