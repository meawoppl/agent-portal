//! Hardening headers for responses that serve user-uploaded media.
//!
//! Media bytes arrive from an agent's `agent-portal show <file>`, so they are
//! attacker-influenced input as far as the browser is concerned. Every path that
//! serves them back ([`images`](super::images), [`media_store`](super::media_store),
//! and the archived-media read in [`history`](super::history)) attaches these
//! headers so the three can't drift apart.

use axum::http::{header, HeaderMap, HeaderValue};

/// CSP for served SVG.
///
/// `sandbox` — with no `allow-*` tokens — is the part that actually disables
/// scripting; the rest stops an uploaded file from reaching the network.
/// `img-src data:` and `style-src 'unsafe-inline'` are what ordinary SVGs need
/// (an inline `<style>` block, embedded `data:` images); both matplotlib output
/// and hand-authored diagrams render unchanged under this policy.
const SVG_CSP: &str = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; sandbox";

/// Security headers for a served media blob of `content_type`.
///
/// `nosniff` goes on everything, so a browser can't disregard our declared type
/// and re-interpret the bytes as something executable.
///
/// The CSP is **SVG-only**, because SVG is the one supported format that is a
/// *document*: XML that can carry a `<script>` element. Inside `<img>` that
/// script is inert, but the lightbox's Download link and any direct navigation
/// to `/api/images/{id}` render the file as a top-level document on the portal's
/// **own origin**, where its script would run with access to same-origin cookies
/// and `localStorage`.
///
/// Deliberately not applied to rasters: they can't script, and a blanket
/// `default-src 'none'` would also govern the image-document view a browser
/// synthesizes when you open a PNG directly.
pub fn media_security_headers(content_type: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if is_svg(content_type) {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(SVG_CSP),
        );
    }
    headers
}

/// Is this an SVG content type, ignoring any `; charset=...` parameters?
fn is_svg(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("image/svg+xml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_gets_a_script_blocking_csp() {
        let h = media_security_headers("image/svg+xml");
        let csp = h
            .get(header::CONTENT_SECURITY_POLICY)
            .expect("svg must carry a CSP")
            .to_str()
            .unwrap();
        // `sandbox` is the token that disables scripting; without it the rest of
        // the policy would not stop an inline <script>.
        assert!(csp.contains("sandbox"), "CSP must sandbox: {csp}");
        assert!(csp.contains("default-src 'none'"), "{csp}");
    }

    #[test]
    fn svg_csp_survives_a_charset_parameter() {
        // Uploads may declare `image/svg+xml; charset=utf-8`; the protection must
        // not fall off just because a parameter is present.
        let h = media_security_headers("image/svg+xml; charset=utf-8");
        assert!(h.contains_key(header::CONTENT_SECURITY_POLICY));
    }

    #[test]
    fn rasters_and_video_get_nosniff_but_no_csp() {
        for ct in ["image/png", "image/jpeg", "image/gif", "image/webp"] {
            let h = media_security_headers(ct);
            assert_eq!(h.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
            assert!(
                !h.contains_key(header::CONTENT_SECURITY_POLICY),
                "{ct} should not be CSP-restricted"
            );
        }
        for ct in ["video/mp4", "video/webm"] {
            let h = media_security_headers(ct);
            assert_eq!(h.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(), "nosniff");
            assert!(!h.contains_key(header::CONTENT_SECURITY_POLICY), "{ct}");
        }
    }

    #[test]
    fn every_supported_image_type_is_covered() {
        // A new image format must get nosniff automatically, and must be
        // consciously classified as document-capable or not.
        for ct in shared::media::SUPPORTED_IMAGE_TYPES {
            let h = media_security_headers(ct);
            assert!(h.contains_key(header::X_CONTENT_TYPE_OPTIONS), "{ct}");
            assert_eq!(
                h.contains_key(header::CONTENT_SECURITY_POLICY),
                *ct == "image/svg+xml",
                "{ct}"
            );
        }
    }
}
