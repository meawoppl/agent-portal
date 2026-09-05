//! `agent-portal show <file>` (alias `display`): display media in
//! the calling session's transcript. A thin client over the backend's
//! `POST /api/agent/sessions/{id}/media` endpoint, authenticated with the
//! launcher's stored proxy token and attributed to the session the CLI is
//! running inside — the same identity plumbing as `agent-portal message`.
//!
//! Content type is detected from the file extension AND verified against the
//! file's magic bytes, so a misnamed file fails fast with a clear message
//! rather than rendering as a broken element in the transcript.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

use shared::api::ShowMediaResponse;
use shared::media::{
    has_gif_magic, has_jpeg_magic, has_mp4_magic, has_png_magic, has_rizzma_html_carrier,
    has_rizzma_magic, has_webm_magic, has_webp_magic, looks_like_svg, media_kind, MediaKind,
    PORTABLE_FIGURE_HTML_MAX_BYTES, PORTABLE_FIGURE_HTML_TYPE, PORTABLE_FIGURE_MAX_BYTES,
    PORTABLE_FIGURE_TYPE, SUPPORTED_FORMATS_HINT,
};

/// Detect a supported media content type from `path`'s extension, verified
/// against `bytes`' magic. Returns the canonical content type (e.g.
/// `image/png`, `video/mp4`) or a caller-facing error message.
///
/// Pure function (no I/O) so it can be unit-tested with byte fixtures.
pub(crate) fn detect_content_type(path: &Path, bytes: &[u8]) -> Result<&'static str, String> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_ascii_lowercase);
    if filename
        .as_deref()
        .is_some_and(|name| name.ends_with(".riz.html"))
    {
        return if has_rizzma_html_carrier(bytes) {
            Ok(PORTABLE_FIGURE_HTML_TYPE)
        } else {
            Err(format!(
                "file contents don't match its extension (expected {PORTABLE_FIGURE_HTML_TYPE}); \
                 refusing to display a possibly-corrupt or misnamed file"
            ))
        };
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());

    let (content_type, magic_ok) = match ext.as_deref() {
        Some("png") => ("image/png", has_png_magic(bytes)),
        Some("jpg" | "jpeg") => ("image/jpeg", has_jpeg_magic(bytes)),
        Some("gif") => ("image/gif", has_gif_magic(bytes)),
        Some("webp") => ("image/webp", has_webp_magic(bytes)),
        Some("svg") => ("image/svg+xml", looks_like_svg(bytes)),
        Some("mp4") => ("video/mp4", has_mp4_magic(bytes)),
        Some("webm") => ("video/webm", has_webm_magic(bytes)),
        Some("riz") => (PORTABLE_FIGURE_TYPE, has_rizzma_magic(bytes)),
        _ => {
            return Err(format!(
                "unsupported file type; supported formats: {SUPPORTED_FORMATS_HINT}"
            ));
        }
    };

    if !magic_ok {
        return Err(format!(
            "file contents don't match its extension (expected {content_type}); \
             refusing to display a possibly-corrupt or misnamed file"
        ));
    }
    Ok(content_type)
}

/// Per-format byte cap. The backend is authoritative; this is a fast local
/// pre-flight. HTML carriers get transport headroom for base64 and an embedded
/// runtime, while their decoded artifact remains capped at 10 MiB server-side.
fn cap_bytes(content_type: &str, kind: MediaKind) -> u64 {
    if content_type == PORTABLE_FIGURE_HTML_TYPE {
        return PORTABLE_FIGURE_HTML_MAX_BYTES as u64;
    }
    if kind == MediaKind::Figure {
        return PORTABLE_FIGURE_MAX_BYTES as u64;
    }
    let (var, default) = match kind {
        MediaKind::Image => ("PORTAL_MAX_IMAGE_MB", 10),
        MediaKind::Video => ("PORTAL_MAX_VIDEO_MB", 100),
        MediaKind::Figure => unreachable!("portable-figure cap returned above"),
    };
    std::env::var(var)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
        * 1024
        * 1024
}

fn human_size(bytes: u64) -> String {
    shared::fmt::format_file_size(bytes)
}

/// `agent-portal show <file>` — upload and display media in this session.
pub async fn show(path_str: &str) -> Result<()> {
    let client = reqwest::Client::new();
    let (base, token) = crate::message::api_base()?;
    let session_id = crate::message::current_session_id(&client, &base, &token)
        .await
        .context("could not determine the calling session for `agent-portal show`")?;

    let (data, filename, size, kind) = upload_media(&session_id, Path::new(path_str)).await?;
    let kind_word = match kind {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
        MediaKind::Figure => "portable figure",
    };
    println!(
        "Displayed {kind_word} {filename} ({}) in session {}.",
        human_size(size),
        data.session_name
    );
    if !data.persisted {
        println!("warning: media was shown live but not durably persisted for replay");
    }
    Ok(())
}

/// Upload a local file as media for an explicit session.
///
/// Split out of [`show`] because the Read-triggered auto-display runs inside the
/// launcher daemon, which manages many sessions at once and therefore cannot use
/// `show`'s "resolve the calling session from my own environment" step.
pub(crate) async fn upload_media(
    session_id: &str,
    path: &Path,
) -> Result<(ShowMediaResponse, String, u64, MediaKind)> {
    let path_str = path.display().to_string();
    let bytes = std::fs::read(path).with_context(|| format!("could not read file `{path_str}`"))?;
    if bytes.is_empty() {
        return Err(anyhow!("`{path_str}` is empty"));
    }

    let content_type = detect_content_type(path, &bytes).map_err(|e| anyhow!(e))?;
    let kind = media_kind(content_type).expect("detector only returns supported types");

    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let size = bytes.len() as u64;
    let limit = cap_bytes(content_type, kind);
    if size > limit {
        return Err(anyhow!(
            "{filename} is {} — exceeds the {} MB transport limit for {}",
            human_size(size),
            limit / (1024 * 1024),
            match kind {
                MediaKind::Image => "images",
                MediaKind::Video => "videos",
                MediaKind::Figure => "portable figures",
            },
        ));
    }

    let (base, token) = crate::message::api_base()?;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/agent/sessions/{session_id}/media"))
        .bearer_auth(&token)
        .query(&[("filename", &filename)])
        .header(reqwest::header::CONTENT_TYPE, content_type)
        .body(bytes)
        .send()
        .await
        .context("request to backend failed")?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("backend returned {}: {}", status, body.trim()));
    }

    let data: ShowMediaResponse = resp.json().await.context("malformed response")?;
    Ok((data, filename, size, kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn png_bytes() -> Vec<u8> {
        let mut v = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&[0u8; 16]);
        v
    }

    #[test]
    fn detects_png_by_extension_and_magic() {
        assert_eq!(
            detect_content_type(Path::new("plot.png"), &png_bytes()),
            Ok("image/png")
        );
    }

    #[test]
    fn detects_portable_figure_by_extension_and_magic() {
        assert_eq!(
            detect_content_type(Path::new("plot.riz"), b"RZFG\x01\0\0\0"),
            Ok(PORTABLE_FIGURE_TYPE)
        );
        assert!(detect_content_type(Path::new("plot.riz"), b"not a figure").is_err());

        let wrapper = format!(
            "<!doctype html>{}AAAA</script>",
            shared::media::RIZZMA_HTML_CARRIER_OPEN
        );
        assert_eq!(
            detect_content_type(Path::new("plot.riz.html"), wrapper.as_bytes()),
            Ok(PORTABLE_FIGURE_HTML_TYPE)
        );
        assert_eq!(
            detect_content_type(Path::new("PLOT.RIZ.HTML"), wrapper.as_bytes()),
            Ok(PORTABLE_FIGURE_HTML_TYPE)
        );
        assert!(detect_content_type(Path::new("plot.html"), wrapper.as_bytes()).is_err());
        assert!(detect_content_type(Path::new("plot.riz.html"), b"not a wrapper").is_err());
    }

    #[test]
    fn detects_jpeg_and_gif_and_webp() {
        assert_eq!(
            detect_content_type(Path::new("a.jpg"), &[0xFF, 0xD8, 0xFF, 0x00]),
            Ok("image/jpeg")
        );
        assert_eq!(
            detect_content_type(Path::new("a.jpeg"), &[0xFF, 0xD8, 0xFF, 0x00]),
            Ok("image/jpeg")
        );
        assert_eq!(
            detect_content_type(Path::new("a.gif"), b"GIF89a....."),
            Ok("image/gif")
        );
        let webp = b"RIFF\x00\x00\x00\x00WEBPVP8 ";
        assert_eq!(
            detect_content_type(Path::new("a.webp"), webp),
            Ok("image/webp")
        );
    }

    #[test]
    fn detects_svg_text() {
        let svg = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
        assert_eq!(
            detect_content_type(Path::new("d.svg"), svg),
            Ok("image/svg+xml")
        );
        let bare = b"<svg viewBox=\"0 0 10 10\"></svg>";
        assert_eq!(
            detect_content_type(Path::new("d.svg"), bare),
            Ok("image/svg+xml")
        );
    }

    #[test]
    fn detects_mp4_and_webm() {
        let mp4 = b"\x00\x00\x00\x18ftypmp42";
        assert_eq!(
            detect_content_type(Path::new("clip.mp4"), mp4),
            Ok("video/mp4")
        );
        let webm = &[0x1A, 0x45, 0xDF, 0xA3, 0x00, 0x00];
        assert_eq!(
            detect_content_type(Path::new("clip.webm"), webm),
            Ok("video/webm")
        );
    }

    #[test]
    fn rejects_unsupported_extension() {
        let err = detect_content_type(Path::new("doc.pdf"), b"%PDF-1.7").unwrap_err();
        assert!(err.contains("unsupported file type"), "{err}");
    }

    #[test]
    fn rejects_no_extension() {
        let err = detect_content_type(Path::new("noext"), &png_bytes()).unwrap_err();
        assert!(err.contains("unsupported file type"), "{err}");
    }

    #[test]
    fn rejects_extension_magic_mismatch() {
        // .png extension but JPEG magic — a misnamed/corrupt file.
        let err = detect_content_type(Path::new("fake.png"), &[0xFF, 0xD8, 0xFF]).unwrap_err();
        assert!(err.contains("don't match its extension"), "{err}");
    }

    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024 + 512 * 1024), "1.5 MB");
    }
}
