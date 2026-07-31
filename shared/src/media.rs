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

// --- Format probes ---
//
// Shape checks for the supported formats, used two ways: the CLI verifies a
// file's bytes against its extension before upload, and the archive read path
// recovers a content type when a blob's sidecar is missing. Both live here so
// the two can't drift (see the module docs).

pub fn has_png_magic(b: &[u8]) -> bool {
    b.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A])
}

pub fn has_jpeg_magic(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0xD8, 0xFF])
}

pub fn has_gif_magic(b: &[u8]) -> bool {
    b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")
}

pub fn has_webp_magic(b: &[u8]) -> bool {
    b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP"
}

pub fn has_mp4_magic(b: &[u8]) -> bool {
    // ISO Base Media: an `ftyp` box at offset 4.
    b.len() >= 12 && &b[4..8] == b"ftyp"
}

pub fn has_webm_magic(b: &[u8]) -> bool {
    // EBML header (Matroska/WebM).
    b.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
}

/// SVG is XML text, so there's no single magic number. Skip a UTF-8 BOM and
/// leading whitespace, then look for an `<svg` (or `<?xml` prolog) near the
/// start.
pub fn looks_like_svg(b: &[u8]) -> bool {
    let b = b.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(b);
    let head = &b[..b.len().min(1024)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    let trimmed = text.trim_start();
    trimmed.starts_with("<svg") || trimmed.starts_with("<?xml") || text.contains("<svg")
}

/// Recover a supported content type from bytes alone, with no filename to go on.
///
/// For serving stored blobs whose declared type went missing. Guessing beats
/// `application/octet-stream` there because of an asymmetry in how browsers
/// treat it: they content-sniff raster formats and render them anyway, but they
/// **never** sniff SVG. So an octet-stream fallback silently breaks only SVG
/// while PNG/JPEG keep working — a bug shaped to hide from casual testing.
///
/// Binary magics are checked first since they're unambiguous; the SVG text
/// heuristic goes last because it's the loosest. Returns `None` for anything
/// outside the supported set, leaving the caller to pick a fallback — never
/// invent a type for bytes we don't recognize.
pub fn sniff_content_type(bytes: &[u8]) -> Option<&'static str> {
    if has_png_magic(bytes) {
        Some("image/png")
    } else if has_jpeg_magic(bytes) {
        Some("image/jpeg")
    } else if has_gif_magic(bytes) {
        Some("image/gif")
    } else if has_webp_magic(bytes) {
        Some("image/webp")
    } else if has_mp4_magic(bytes) {
        Some("video/mp4")
    } else if has_webm_magic(bytes) {
        Some("video/webm")
    } else if looks_like_svg(bytes) {
        Some("image/svg+xml")
    } else {
        None
    }
}

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

    #[test]
    fn sniffs_every_supported_format_from_bytes() {
        let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
        assert_eq!(sniff_content_type(&png), Some("image/png"));
        assert_eq!(
            sniff_content_type(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some("image/jpeg")
        );
        assert_eq!(sniff_content_type(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff_content_type(b"RIFF____WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_content_type(b"\0\0\0\x18ftypisom"), Some("video/mp4"));
        assert_eq!(
            sniff_content_type(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0]),
            Some("video/webm")
        );

        // Every sniffed type must be one the rest of the pipeline accepts.
        for bytes in [&png[..], &[0xFF, 0xD8, 0xFF, 0xE0][..]] {
            let ct = sniff_content_type(bytes).expect("sniffed");
            assert!(media_kind(ct).is_some(), "{ct} not in the supported set");
        }
    }

    #[test]
    fn sniffs_svg_in_its_common_shapes() {
        // The case that regressed: SVG is never content-sniffed by browsers, so
        // serving it as octet-stream renders nothing.
        for svg in [
            &b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"[..],
            &b"<?xml version=\"1.0\"?>\n<svg viewBox=\"0 0 1 1\"></svg>"[..],
            &b"\n  <svg/>"[..],
            &b"\xEF\xBB\xBF<svg/>"[..], // UTF-8 BOM
        ] {
            assert_eq!(sniff_content_type(svg), Some("image/svg+xml"));
        }
    }

    #[test]
    fn sniff_declines_unknown_bytes() {
        // Must not invent a type; the caller falls back deliberately.
        assert_eq!(sniff_content_type(b"%PDF-1.7"), None);
        assert_eq!(sniff_content_type(b""), None);
        assert_eq!(sniff_content_type(b"just some prose"), None);
    }
}
