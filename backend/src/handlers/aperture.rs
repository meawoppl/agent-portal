//! Public legacy exhibit redirects. No authentication or secret-bearing query is used.
use axum::{extract::OriginalUri, response::Redirect, routing::get, Router};
use std::sync::Arc;

const EXHIBIT_URL: &str = "https://meawoppl.github.io/agent-portal/";

async fn exhibit() -> Redirect {
    // Browsers retain the old fragment (observer invitation) across this redirect.
    Redirect::temporary(EXHIBIT_URL)
}

async fn asset(OriginalUri(uri): OriginalUri) -> Redirect {
    // Keep percent encoding intact. The host is constant; paths cannot select it.
    Redirect::temporary(&format!(
        "{EXHIBIT_URL}{}",
        uri.path().trim_start_matches('/')
    ))
}

pub fn router() -> Router<Arc<crate::AppState>> {
    Router::new()
        .route("/aperture", get(exhibit))
        .route("/aperture/", get(exhibit))
        .route("/aperture-assets/{*path}", get(asset))
        .route("/aperture-demo/{*path}", get(asset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    #[tokio::test]
    async fn legacy_media_redirects_preserve_encoded_paths_and_external_host() {
        let app = Router::new()
            .route("/aperture", get(exhibit))
            .route("/aperture-assets/{*path}", get(asset))
            .route("/aperture-demo/{*path}", get(asset));
        for (path, suffix) in [
            ("/aperture", ""),
            (
                "/aperture-assets/evidence/cake.stl",
                "aperture-assets/evidence/cake.stl",
            ),
            (
                "/aperture-assets/a%20b.mp4?ignored=1",
                "aperture-assets/a%20b.mp4",
            ),
            (
                "/aperture-demo/companion.html",
                "aperture-demo/companion.html",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), 307);
            assert_eq!(
                response.headers()["location"],
                format!("{EXHIBIT_URL}{suffix}")
            );
        }
    }
}
