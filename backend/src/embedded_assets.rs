use axum::{
    body::Body,
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;
use std::collections::HashMap;
use std::io::Write;
use std::sync::OnceLock;

#[derive(RustEmbed)]
#[folder = "../frontend/dist"]
pub struct FrontendAssets;

struct CompressedAsset {
    raw: Vec<u8>,
    brotli: Vec<u8>,
    gzip: Vec<u8>,
    mime: String,
    is_index: bool,
}

static CACHE: OnceLock<HashMap<String, CompressedAsset>> = OnceLock::new();

/// Pre-compress all embedded assets at startup.
/// Call this before starting the server so the first request is fast.
pub fn init_cache() {
    CACHE.get_or_init(|| {
        let mut map = HashMap::new();
        let mut total_raw = 0u64;
        let mut total_br = 0u64;

        for path in FrontendAssets::iter() {
            if let Some(content) = FrontendAssets::get(&path) {
                let raw = content.data.to_vec();
                let mime = mime_guess::from_path(&*path)
                    .first_or_octet_stream()
                    .to_string();
                let is_index = *path == *"index.html";

                // Brotli compress (quality 11 = max)
                let brotli_buf = {
                    let mut buf = Vec::new();
                    {
                        let mut writer = brotli::CompressorWriter::new(&mut buf, 4096, 11, 22);
                        writer.write_all(&raw).unwrap();
                    }
                    buf
                };

                // Gzip compress (best)
                let gzip_buf = {
                    let mut encoder =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
                    encoder.write_all(&raw).unwrap();
                    encoder.finish().unwrap()
                };

                total_raw += raw.len() as u64;
                total_br += brotli_buf.len() as u64;

                map.insert(
                    path.to_string(),
                    CompressedAsset {
                        raw,
                        brotli: brotli_buf,
                        gzip: gzip_buf,
                        mime,
                        is_index,
                    },
                );
            }
        }

        tracing::info!(
            "Pre-compressed {} assets: {:.1} MB raw -> {:.1} MB brotli ({:.0}% reduction)",
            map.len(),
            total_raw as f64 / 1_048_576.0,
            total_br as f64 / 1_048_576.0,
            (1.0 - total_br as f64 / total_raw as f64) * 100.0
        );

        map
    });
}

fn cache_control(is_index: bool) -> HeaderValue {
    if is_index {
        HeaderValue::from_static("no-cache")
    } else {
        HeaderValue::from_static("public, max-age=31536000, immutable")
    }
}

/// Serve pre-compressed embedded frontend assets with SPA fallback.
/// Checks Accept-Encoding and returns brotli/gzip/raw accordingly.
pub async fn serve_embedded_frontend(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    let cache = CACHE.get().expect("asset cache not initialized");

    let accept = headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Look up the exact path, fall back to index.html for SPA routing
    let asset = cache.get(path).or_else(|| cache.get("index.html"));

    match asset {
        Some(asset) => {
            let (body, encoding) = if accept.contains("br") {
                (asset.brotli.as_slice(), Some("br"))
            } else if accept.contains("gzip") {
                (asset.gzip.as_slice(), Some("gzip"))
            } else {
                (asset.raw.as_slice(), None)
            };

            let mut resp = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, &asset.mime)
                .header(header::CACHE_CONTROL, cache_control(asset.is_index))
                .body(Body::from(body.to_vec()))
                .unwrap();

            if let Some(enc) = encoding {
                resp.headers_mut()
                    .insert(header::CONTENT_ENCODING, HeaderValue::from_static(enc));
            }

            resp
        }
        None => (StatusCode::NOT_FOUND, "Frontend not found").into_response(),
    }
}
