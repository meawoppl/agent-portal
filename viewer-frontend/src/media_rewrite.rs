//! Rewrite archived media URLs to the viewer's own media endpoint.
//!
//! Archived transcript content carries the *original* portal served-media URLs
//! — `/api/images/{uuid}` for images and `/api/media/{uuid}` for videos (see
//! `backend/src/handlers/media_archive.rs` / `background.rs`). Those paths only
//! resolve on the live portal's origin; on the viewer's origin they 404 and the
//! renderer would show a broken image. The viewer instead serves each blob from
//! its session-scoped endpoint `/api/media/{user}/{session}/{media_id}` (the
//! pinned #1288 contract), where `{media_id}` is exactly the original `{uuid}`.
//!
//! This is a pure, string-level pass over the raw content JSON run *before*
//! `RenderedMessage::new`, so the real renderers (`render_image_source` /
//! `VideoViewer`) receive already-correct URLs and their existing
//! expired-media placeholder still fires when the archive lacks the blob (the
//! endpoint 404s → `<img onerror>` / `<video onerror>` → placeholder).

/// Original portal image URL prefix (`/api/images/{uuid}`).
const IMAGE_PREFIX: &str = "/api/images/";
/// Original portal video URL prefix (`/api/media/{uuid}`).
const VIDEO_PREFIX: &str = "/api/media/";
/// Length of a canonical hyphenated UUID (`8-4-4-4-12`).
const UUID_LEN: usize = 36;

/// Rewrite every `/api/images/{uuid}` and `/api/media/{uuid}` occurrence in
/// `content` to `/api/media/{user}/{session}/{uuid}`.
///
/// Operates on the raw JSON string so it catches the media id wherever it
/// appears (a standalone `data` value or embedded in prose). Only a prefix
/// immediately followed by a parseable UUID is rewritten; anything else is
/// emitted verbatim, and already-rewritten output is never rescanned (so the
/// `/api/media/` that the rewrite itself introduces is not re-matched).
pub fn rewrite_media_urls(content: &str, user: &str, session: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;

    loop {
        // Earliest of the two prefixes in what remains.
        let next = [IMAGE_PREFIX, VIDEO_PREFIX]
            .iter()
            .filter_map(|prefix| rest.find(prefix).map(|idx| (idx, *prefix)))
            .min_by_key(|(idx, _)| *idx);

        let Some((idx, prefix)) = next else {
            out.push_str(rest);
            break;
        };

        out.push_str(&rest[..idx]);
        let after = &rest[idx + prefix.len()..];

        // A rewrite requires exactly a canonical UUID right after the prefix.
        // `get(..UUID_LEN)` is `None` at a non-char boundary or when short, so
        // this never panics.
        if let Some(candidate) = after.get(..UUID_LEN) {
            if uuid::Uuid::parse_str(candidate).is_ok() {
                out.push_str(VIDEO_PREFIX);
                out.push_str(user);
                out.push('/');
                out.push_str(session);
                out.push('/');
                out.push_str(candidate);
                rest = &after[UUID_LEN..];
                continue;
            }
        }

        // Not a media reference: emit the prefix literally, advance past it.
        out.push_str(prefix);
        rest = after;
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "11111111-1111-1111-1111-111111111111";
    const SESSION: &str = "22222222-2222-2222-2222-222222222222";
    const MEDIA: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

    #[test]
    fn rewrites_image_url_in_data_value() {
        let content = format!(r#"{{"type":"image","data":"/api/images/{MEDIA}"}}"#);
        let out = rewrite_media_urls(&content, USER, SESSION);
        assert_eq!(
            out,
            format!(r#"{{"type":"image","data":"/api/media/{USER}/{SESSION}/{MEDIA}"}}"#)
        );
    }

    #[test]
    fn rewrites_video_url_in_data_value() {
        let content = format!(r#"{{"data":"/api/media/{MEDIA}"}}"#);
        let out = rewrite_media_urls(&content, USER, SESSION);
        assert_eq!(
            out,
            format!(r#"{{"data":"/api/media/{USER}/{SESSION}/{MEDIA}"}}"#)
        );
    }

    #[test]
    fn rewrites_multiple_occurrences() {
        let other = "12345678-1234-1234-1234-1234567890ab";
        let content = format!("/api/images/{MEDIA} and /api/media/{other}");
        let out = rewrite_media_urls(&content, USER, SESSION);
        assert_eq!(
            out,
            format!("/api/media/{USER}/{SESSION}/{MEDIA} and /api/media/{USER}/{SESSION}/{other}")
        );
    }

    #[test]
    fn rewritten_url_is_not_double_rewritten() {
        // The `/api/media/` the rewrite introduces must not be re-matched:
        // its next segment is the user UUID, not a fresh media reference, and
        // emitted output is never rescanned regardless.
        let content = format!("/api/media/{MEDIA}");
        let out = rewrite_media_urls(&content, USER, SESSION);
        assert_eq!(out, format!("/api/media/{USER}/{SESSION}/{MEDIA}"));
        // Idempotent-ish: running again over already-viewer-scoped URLs does
        // rewrite the *first* uuid segment (the user), which is why we only
        // ever run this once, at ingest.
        assert_eq!(out.matches("/api/media/").count(), 1);
    }

    #[test]
    fn leaves_non_uuid_paths_untouched() {
        let content = r#"see /api/images/latest and /api/media/thumbnail.png"#;
        let out = rewrite_media_urls(content, USER, SESSION);
        assert_eq!(out, content);
    }

    #[test]
    fn leaves_unrelated_content_untouched() {
        let content = r#"{"type":"text","text":"no media here"}"#;
        assert_eq!(rewrite_media_urls(content, USER, SESSION), content);
    }

    #[test]
    fn handles_prefix_at_end_without_uuid() {
        let content = "trailing /api/images/";
        assert_eq!(rewrite_media_urls(content, USER, SESSION), content);
    }

    #[test]
    fn preserves_surrounding_json_structure() {
        let content = format!(
            r#"{{"content":[{{"type":"image","media_type":"image/png","source_type":"url","data":"/api/images/{MEDIA}"}}]}}"#
        );
        let out = rewrite_media_urls(&content, USER, SESSION);
        assert!(out.contains(&format!("/api/media/{USER}/{SESSION}/{MEDIA}")));
        assert!(out.contains(r#""source_type":"url""#));
        // Still valid JSON.
        assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok());
    }
}
