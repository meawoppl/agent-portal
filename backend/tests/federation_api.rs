#![allow(clippy::expect_used, clippy::unwrap_used)]
//! Federation discovery: reachable unauthenticated and cross-origin, stable
//! instance identity, and nothing advertised until it is enforced.
//!
//! Requires `DATABASE_URL` (see `harness.rs`); skips gracefully without it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use tower::ServiceExt;

use backend::test_support::test_app_state;
use shared::api::{FederationDiscoveryResponse, FEDERATION_DISCOVERY_PATH, FEDERATION_PROTOCOL};

fn test_pool() -> Option<backend::db::DbPool> {
    if std::env::var("DATABASE_URL").is_err() {
        eprintln!("DATABASE_URL not set, skipping DB-backed test");
        return None;
    }
    static POOL: std::sync::OnceLock<backend::db::DbPool> = std::sync::OnceLock::new();
    Some(
        POOL.get_or_init(|| {
            let pool = backend::db::create_pool().expect("create test pool");
            backend::db::run_migrations_logged(&pool).expect("run migrations");
            pool
        })
        .clone(),
    )
}

async fn discover(state: Arc<backend::AppState>) -> (FederationDiscoveryResponse, bool) {
    let resp = backend::routes::build_router(state)
        .expect("router builds")
        .oneshot(
            Request::get(FEDERATION_DISCOVERY_PATH)
                .header(header::ORIGIN, "https://home.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cors = resp
        .headers()
        .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (serde_json::from_slice(&bytes).unwrap(), cors)
}

#[tokio::test]
async fn discovery_is_public_stable_and_advertises_nothing_unenforced() {
    let Some(pool) = test_pool() else { return };
    let state = Arc::new(test_app_state(pool));

    let (first, cors) = discover(state.clone()).await;
    assert!(
        cors,
        "a client on another origin must be able to read discovery"
    );
    assert_eq!(first.protocol, FEDERATION_PROTOCOL);
    assert!(first
        .capabilities
        .contains(&shared::api::CAP_LINKED_CLIENT_ENROLLMENT.into()));
    assert!(first
        .capabilities
        .contains(&shared::api::CAP_WS_TICKET.into()));
    assert!(first
        .auth_modes
        .contains(&shared::api::AUTH_MODE_WS_TICKET.into()));
    assert!(!first
        .capabilities
        .contains(&shared::api::CAP_SCOPED_MEDIA_URLS.into()));

    let (second, _) = discover(state).await;
    assert_eq!(first.instance_id, second.instance_id);
}
