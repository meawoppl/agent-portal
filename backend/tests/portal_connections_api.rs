#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `/api/portal-connections` integration tests: origin canonicalization,
//! upsert identity, per-user isolation, the cap, and self-link rejection.
//!
//! Requires `DATABASE_URL` (see `harness.rs`); skips gracefully without it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Method, Request, StatusCode};
use diesel::prelude::*;
use tower::ServiceExt;
use uuid::Uuid;

use backend::handlers::proxy_tokens::{issue_proxy_token, TokenPersist};
use backend::models::User;
use backend::test_support::test_app_state;
use backend::AppState;
use shared::api::{
    PortalConnection, PortalConnectionsResponse, TokenScope, UpdatePortalConnectionRequest,
    UpsertPortalConnectionRequest,
};

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

struct Fixture {
    state: Arc<AppState>,
    alice: String,
    bob: String,
}

fn fixture(pool: backend::db::DbPool) -> Fixture {
    let state = test_app_state(pool.clone());
    let mut conn = pool.get().expect("conn");
    let mut user = |name: &str| -> String {
        use backend::schema::users;
        let user: User = diesel::insert_into(users::table)
            .values(backend::models::NewUser {
                email: format!("{name}-{}@example.test", Uuid::new_v4()),
                name: Some(name.to_string()),
                avatar_url: None,
            })
            .get_result(&mut conn)
            .expect("insert user");
        let issued = issue_proxy_token(
            &mut conn,
            state.jwt_secret.as_bytes(),
            user.id,
            TokenPersist::Create {
                name: "portal-connections-test",
            },
            Some(1),
        )
        .expect("issue token");
        format!("Bearer {}", issued.token)
    };
    let alice = user("alice");
    let bob = user("bob");
    Fixture {
        state: Arc::new(state),
        alice,
        bob,
    }
}

async fn call(
    f: &Fixture,
    method: Method,
    uri: &str,
    auth: &str,
    body: Option<String>,
) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::AUTHORIZATION, auth);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    let resp = backend::routes::build_router(f.state.clone())
        .expect("router builds")
        .oneshot(req.body(body.map(Body::from).unwrap_or_default()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

fn upsert_body(origin: &str, remote_user_id: Uuid, label: &str) -> String {
    serde_json::to_string(&UpsertPortalConnectionRequest {
        portal_origin: origin.to_string(),
        instance_id: Uuid::new_v4(),
        remote_user_id,
        remote_email: "matt@work.example".to_string(),
        label: label.to_string(),
        scope: TokenScope::Write,
    })
    .unwrap()
}

async fn list(f: &Fixture, auth: &str) -> Vec<PortalConnection> {
    let (status, bytes) = call(f, Method::GET, "/api/portal-connections", auth, None).await;
    assert_eq!(status, StatusCode::OK);
    serde_json::from_slice::<PortalConnectionsResponse>(&bytes)
        .unwrap()
        .connections
}

#[tokio::test]
async fn upsert_canonicalizes_and_is_keyed_by_origin_and_remote_user() {
    let Some(pool) = test_pool() else { return };
    let f = fixture(pool);
    let remote = Uuid::new_v4();

    let (status, bytes) = call(
        &f,
        Method::POST,
        "/api/portal-connections",
        &f.alice,
        Some(upsert_body("HTTPS://Work.Example:443/", remote, "work")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let created: PortalConnection = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(created.portal_origin, "https://work.example");
    assert_eq!(created.scope, TokenScope::Write);

    // Same origin (different spelling) + same remote account: an update.
    let (status, bytes) = call(
        &f,
        Method::POST,
        "/api/portal-connections",
        &f.alice,
        Some(upsert_body("https://work.example", remote, "  day job ")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let updated: PortalConnection = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(updated.id, created.id);
    assert_eq!(updated.label, "day job");

    // A second account on the same portal is its own connection.
    let (status, _) = call(
        &f,
        Method::POST,
        "/api/portal-connections",
        &f.alice,
        Some(upsert_body(
            "https://work.example",
            Uuid::new_v4(),
            "shared",
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list(&f, &f.alice).await.len(), 2);
}

#[tokio::test]
async fn connections_are_private_to_their_owner() {
    let Some(pool) = test_pool() else { return };
    let f = fixture(pool);
    let (_, bytes) = call(
        &f,
        Method::POST,
        "/api/portal-connections",
        &f.alice,
        Some(upsert_body("https://work.example", Uuid::new_v4(), "work")),
    )
    .await;
    let alices: PortalConnection = serde_json::from_slice(&bytes).unwrap();

    assert!(list(&f, &f.bob).await.is_empty());
    let uri = format!("/api/portal-connections/{}", alices.id);
    let rename = serde_json::to_string(&UpdatePortalConnectionRequest {
        label: "mine now".into(),
    })
    .unwrap();
    let (status, _) = call(&f, Method::PATCH, &uri, &f.bob, Some(rename.clone())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&f, Method::DELETE, &uri, &f.bob, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(&f, Method::PATCH, &uri, &f.alice, Some(rename)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&f, Method::DELETE, &uri, &f.alice, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(list(&f, &f.alice).await.is_empty());
}

#[tokio::test]
async fn rejects_bad_origins_self_links_and_bad_labels() {
    let Some(pool) = test_pool() else { return };
    let f = fixture(pool);
    for (origin, label) in [
        ("http://work.example", "work"),
        ("https://work.example/dashboard", "work"),
        ("https://*.work.example", "work"),
        // test_app_state's public_url, in another spelling.
        ("http://LOCALHOST:3000/", "self"),
        ("https://work.example", "   "),
    ] {
        let (status, _) = call(
            &f,
            Method::POST,
            "/api/portal-connections",
            &f.alice,
            Some(upsert_body(origin, Uuid::new_v4(), label)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{origin} / {label:?}");
    }
    assert!(list(&f, &f.alice).await.is_empty());
}

#[tokio::test]
async fn new_links_stop_at_the_cap_but_updates_still_pass() {
    let Some(pool) = test_pool() else { return };
    let f = fixture(pool);
    let first = Uuid::new_v4();
    for i in 0..32 {
        let remote = if i == 0 { first } else { Uuid::new_v4() };
        let (status, _) = call(
            &f,
            Method::POST,
            "/api/portal-connections",
            &f.alice,
            Some(upsert_body("https://work.example", remote, "work")),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }
    let (status, _) = call(
        &f,
        Method::POST,
        "/api/portal-connections",
        &f.alice,
        Some(upsert_body(
            "https://work.example",
            Uuid::new_v4(),
            "one more",
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(
        &f,
        Method::POST,
        "/api/portal-connections",
        &f.alice,
        Some(upsert_body("https://work.example", first, "renamed")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}
