//! Shared media-type policy for `agent-portal show <file>`.
//!
//! The CLI detects a file's content type (by extension + magic bytes) and the
//! backend validates the declared `Content-Type`. Both agree on the supported
//! set and the image/video split through this one module, so the allow-list
//! never drifts between the two sides.

/// Whether a supported media type is a still image or a video. Drives storage
/// (in-memory image store vs. on-disk media store) and rendering (`<img>` vs.
/// `<video>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
}

/// Supported image content types (stored in the in-memory image store).
pub const SUPPORTED_IMAGE_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/svg+xml",
];

/// Supported video content types (stored on disk, served with Range support).
pub const SUPPORTED_VIDEO_TYPES: &[&str] = &["video/mp4", "video/webm"];

/// Classify a content type as image or video, or `None` if unsupported.
pub fn media_kind(content_type: &str) -> Option<MediaKind> {
    let ct = content_type.trim();
    if SUPPORTED_IMAGE_TYPES.contains(&ct) {
        Some(MediaKind::Image)
    } else if SUPPORTED_VIDEO_TYPES.contains(&ct) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

/// Human-readable list of supported formats, for CLI/backend error messages.
pub const SUPPORTED_FORMATS_HINT: &str =
    "png, jpg, jpeg, gif, webp, svg (images); mp4, webm (video)";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_supported_types() {
        assert_eq!(media_kind("image/png"), Some(MediaKind::Image));
        assert_eq!(media_kind("image/svg+xml"), Some(MediaKind::Image));
        assert_eq!(media_kind("video/mp4"), Some(MediaKind::Video));
        assert_eq!(media_kind("video/webm"), Some(MediaKind::Video));
    }

    #[test]
    fn rejects_unsupported_types() {
        assert_eq!(media_kind("application/pdf"), None);
        assert_eq!(media_kind("image/tiff"), None);
        assert_eq!(media_kind("video/quicktime"), None);
        assert_eq!(media_kind(""), None);
    }
}
