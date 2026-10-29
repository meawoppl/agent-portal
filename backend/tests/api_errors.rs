//! Typed API errors through the real router: JSON only when asked for, the
//! historical plain text otherwise, statuses unchanged.
use std::sync::Arc;

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use backend::test_support::{shared_pool, test_app_state};
use shared::api::{ApiError, ApiErrorCode};
use tower::ServiceExt;

#[allow(clippy::unwrap_used)] // Test request construction and response collection.
async fn get(
    state: &Arc<backend::AppState>,
    path: &str,
    accept: Option<&str>,
) -> (StatusCode, Option<String>, String) {
    let mut req = Request::get(path);
    if let Some(accept) = accept {
        req = req.header(header::ACCEPT, accept);
    }
    let response = backend::routes::build_router(state.clone())
        .unwrap()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(response.into_body(), 10_000_000)
        .await
        .unwrap();
    (status, content_type, String::from_utf8_lossy(&bytes).into())
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn unauthenticated_api_errors_negotiate_and_keep_their_status() {
    let Some(pool) = shared_pool() else { return };
    let mut state = test_app_state(pool);
    state.dev_mode = false;
    let state = Arc::new(state);

    let (status, _, body) = get(&state, "/api/sessions", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body, "Unauthorized");

    let (status, content_type, body) = get(&state, "/api/sessions", Some("application/json")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(content_type.as_deref(), Some("application/json"));
    let error: ApiError = serde_json::from_str(&body).unwrap();
    assert_eq!(error.code, ApiErrorCode::Unauthorized);
    assert_eq!(error.message, "Unauthorized");
    assert!(!error.retryable);

    // A page the SPA serves is not an API error, whatever the client accepts.
    let (status, content_type, _) = get(&state, "/dashboard", Some("application/json")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_type.unwrap().starts_with("text/html"));
}
