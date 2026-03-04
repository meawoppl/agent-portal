use axum::{
    body::Body,
    http::{header, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../frontend/dist"]
pub struct FrontendAssets;

/// Assets with content hashes in filenames can be cached indefinitely.
/// index.html must always be revalidated so the browser picks up new hashes.
fn cache_header(path: &str) -> HeaderValue {
    if path == "index.html" {
        HeaderValue::from_static("no-cache")
    } else {
        // 1 year — the hash in the filename guarantees cache busting
        HeaderValue::from_static("public, max-age=31536000, immutable")
    }
}

/// Serve embedded frontend assets with SPA fallback
pub async fn serve_embedded_frontend(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    serve_asset(path)
}

fn serve_asset(path: &str) -> Response {
    match FrontendAssets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [
                    (
                        header::CONTENT_TYPE,
                        HeaderValue::from_str(mime.as_ref()).unwrap(),
                    ),
                    (header::CACHE_CONTROL, cache_header(path)),
                ],
                Body::from(content.data.to_vec()),
            )
                .into_response()
        }
        None => {
            // SPA fallback: serve index.html for any unknown path
            match FrontendAssets::get("index.html") {
                Some(content) => (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, HeaderValue::from_static("text/html")),
                        (header::CACHE_CONTROL, cache_header("index.html")),
                    ],
                    Body::from(content.data.to_vec()),
                )
                    .into_response(),
                None => (StatusCode::NOT_FOUND, "Frontend not found").into_response(),
            }
        }
    }
}
