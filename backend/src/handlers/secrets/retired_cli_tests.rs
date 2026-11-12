//! Retired CLI tokens and enrollment URLs must never regain vault authority.
use super::*;
use crate::schema::{secret_credentials, secret_enrollments};
use tower::ServiceExt;

#[tokio::test]
async fn legacy_vault_tokens_and_owner_jwts_cannot_access_browser_vault_routes() {
    let Some(db) = tests::Database::new() else {
        return;
    };
    let state = Arc::new(crate::test_support::test_app_state(db.pool.clone()));
    let user = crate::test_support::insert_user(&mut state.conn().unwrap(), "retired-cli");
    let legacy = format!("aps_secret_{}", Uuid::new_v4());
    diesel::insert_into(secret_credentials::table)
        .values((
            secret_credentials::id.eq(Uuid::new_v4()),
            secret_credentials::user_id.eq(user.id),
            secret_credentials::token_hash.eq(crate::jwt::hash_token(&legacy)),
            secret_credentials::label.eq("old computer"),
            secret_credentials::metadata_scope.eq(true),
            secret_credentials::mutation_scope.eq(true),
            secret_credentials::read_scope.eq(true),
            secret_credentials::expires_at.eq(Utc::now() + chrono::Duration::days(30)),
        ))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let owner_token = crate::handlers::proxy_tokens::issue_proxy_token(
        &mut state.conn().unwrap(),
        state.jwt_secret.as_bytes(),
        user.id,
        crate::handlers::proxy_tokens::TokenPersist::Create {
            name: "oauth-owner-test",
        },
        Some(1),
    )
    .unwrap()
    .token;
    let cookies = tower_cookies::Cookies::default();
    cookies
        .signed(&state.cookie_key)
        .add(tower_cookies::Cookie::new(
            shared::protocol::SESSION_COOKIE_NAME,
            user.id.to_string(),
        ));
    auth::record_verification(&cookies, &state, user.id);
    let cookie = cookies
        .list()
        .iter()
        .map(|c| format!("{}={}", c.name(), c.value()))
        .collect::<Vec<_>>()
        .join("; ");
    let router = router(state.clone())
        .with_state(state.clone())
        .layer(tower_cookies::CookieManagerLayer::new());
    for token in [legacy, owner_token] {
        for path in [
            "/api/secrets",
            "/api/secrets/members",
            "/api/secrets/bindings",
        ] {
            for include_cookie in [false, true] {
                let mut request = axum::http::Request::builder()
                    .uri(path)
                    .header("authorization", format!("Bearer {token}"));
                if include_cookie {
                    request = request.header("cookie", &cookie);
                }
                let response = router
                    .clone()
                    .oneshot(request.body(axum::body::Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    axum::http::StatusCode::FORBIDDEN,
                    "{path}"
                );
            }
        }
    }
    let response = router
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/secrets")
                .header("cookie", cookie)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        axum::http::StatusCode::OK,
        "Verified OAuth browser metadata remains available"
    );
    for (method, path) in [
        ("POST", "/api/secrets/authorize/start"),
        ("POST", "/api/secrets/authorize/approve"),
        ("POST", "/api/secrets/authorize/poll"),
        ("GET", "/api/secrets/authorize/credentials"),
        (
            "POST",
            "/api/secrets/authorize/credentials/00000000-0000-4000-8000-000000000001/revoke",
        ),
        (
            "POST",
            "/api/secrets/00000000-0000-4000-8000-000000000001/read",
        ),
    ] {
        let response = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method(method)
                    .uri(path)
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), axum::http::StatusCode::GONE, "{path}");
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
}

#[test]
fn retirement_migration_revokes_existing_tokens_and_pending_enrollments() {
    let Some(db) = tests::Database::new() else {
        return;
    };
    let state = crate::test_support::test_app_state(db.pool.clone());
    let user = crate::test_support::insert_user(&mut state.conn().unwrap(), "retire-migration");
    let token = Uuid::new_v4();
    diesel::insert_into(secret_credentials::table)
        .values((
            secret_credentials::id.eq(token),
            secret_credentials::user_id.eq(user.id),
            secret_credentials::token_hash.eq(Uuid::new_v4().to_string()),
            secret_credentials::label.eq("old"),
            secret_credentials::metadata_scope.eq(true),
            secret_credentials::mutation_scope.eq(true),
            secret_credentials::read_scope.eq(true),
            secret_credentials::expires_at.eq(Utc::now() + chrono::Duration::days(30)),
        ))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    let enrollment = Uuid::new_v4();
    diesel::insert_into(secret_enrollments::table)
        .values((
            secret_enrollments::id.eq(enrollment),
            secret_enrollments::user_id.eq(user.id),
            secret_enrollments::device_hash.eq(Uuid::new_v4().to_string()),
            secret_enrollments::user_code.eq(Uuid::new_v4().to_string()),
            secret_enrollments::label.eq("old"),
            secret_enrollments::state.eq("approved"),
            secret_enrollments::expires_at.eq(Utc::now() + chrono::Duration::minutes(10)),
        ))
        .execute(&mut state.conn().unwrap())
        .unwrap();
    use diesel::connection::SimpleConnection;
    state
        .conn()
        .unwrap()
        .batch_execute(include_str!(
            "../../../migrations/2026-11-12-220849_retire_human_secret_credentials/up.sql"
        ))
        .unwrap();
    let revoked = secret_credentials::table
        .find(token)
        .select(secret_credentials::revoked_at)
        .first::<Option<chrono::DateTime<Utc>>>(&mut state.conn().unwrap())
        .unwrap();
    assert!(revoked.is_some());
    let status = secret_enrollments::table
        .find(enrollment)
        .select(secret_enrollments::state)
        .first::<String>(&mut state.conn().unwrap())
        .unwrap();
    assert_eq!(status, "denied");
}
