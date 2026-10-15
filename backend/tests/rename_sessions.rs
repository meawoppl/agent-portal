//! Renaming through the real router: ownership, persistence, and archive freshness.
use std::sync::Arc;

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use backend::{
    handlers::proxy_tokens::{issue_proxy_token, TokenPersist},
    models::{NewSessionMember, Session},
    schema::{session_members, sessions},
    test_support::{insert_session, insert_user, shared_pool, test_app_state},
    AppState,
};
use diesel::prelude::*;
use shared::api::{RenameSessionRequest, RenameSessionResponse};
use tower::ServiceExt;
use uuid::Uuid;

#[allow(clippy::unwrap_used)] // Test fixture setup.
fn token(state: &AppState, user: Uuid) -> String {
    issue_proxy_token(
        &mut state.conn().unwrap(),
        state.jwt_secret.as_bytes(),
        user,
        TokenPersist::Create {
            name: "rename-test",
        },
        Some(1),
    )
    .unwrap()
    .token
}

#[allow(clippy::unwrap_used)] // Test request construction and response collection.
async fn rename(
    state: &Arc<AppState>,
    bearer: Option<&str>,
    id: Uuid,
    name: &str,
) -> (StatusCode, Vec<u8>) {
    let mut req = Request::builder()
        .method("PATCH")
        .uri(format!("/api/sessions/{id}/name"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = bearer {
        req = req.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = backend::routes::build_router(state.clone())
        .unwrap()
        .oneshot(
            req.body(Body::from(
                serde_json::to_vec(&RenameSessionRequest {
                    session_name: name.into(),
                })
                .unwrap(),
            ))
            .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap()
        .to_vec();
    (status, bytes)
}

#[tokio::test]
async fn rename_authorizes_mutators_and_preserves_session_identity_and_state() {
    let Some(pool) = shared_pool() else { return };
    let mut state = test_app_state(pool);
    state.dev_mode = false;
    let state = Arc::new(state);
    let mut conn = state.conn().unwrap();
    let owner = insert_user(&mut conn, "rename-owner");
    let editor = insert_user(&mut conn, "rename-editor");
    let viewer = insert_user(&mut conn, "rename-viewer");
    let stranger = insert_user(&mut conn, "rename-stranger");
    let original = insert_session(&mut conn, owner.id, "old name");
    for (user, role) in [(editor.id, "editor"), (viewer.id, "viewer")] {
        diesel::insert_into(session_members::table)
            .values(NewSessionMember {
                session_id: original.id,
                user_id: user,
                role: role.into(),
            })
            .execute(&mut conn)
            .unwrap();
    }
    diesel::update(sessions::table.find(original.id))
        .set((
            sessions::paused.eq(true),
            sessions::archived_at.eq(Some(chrono::Utc::now().naive_utc())),
        ))
        .execute(&mut conn)
        .unwrap();
    let owner_token = token(&state, owner.id);
    let editor_token = token(&state, editor.id);
    let viewer_token = token(&state, viewer.id);
    let stranger_token = token(&state, stranger.id);
    assert_eq!(
        rename(&state, None, original.id, "bad").await.0,
        StatusCode::UNAUTHORIZED
    );
    for token in [&viewer_token, &stranger_token] {
        assert_eq!(
            rename(&state, Some(token), original.id, "bad").await.0,
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        rename(&state, Some(&owner_token), Uuid::new_v4(), "bad")
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    for invalid in [
        "".to_owned(),
        "  ".into(),
        "line\nbreak".into(),
        "null\0byte".into(),
        "a".repeat(256),
        "🔭".repeat(256),
    ] {
        assert_eq!(
            rename(&state, Some(&owner_token), original.id, &invalid)
                .await
                .0,
            StatusCode::BAD_REQUEST,
            "{invalid:?}"
        );
    }
    assert_eq!(
        sessions::table
            .find(original.id)
            .select(sessions::session_name)
            .first::<String>(&mut conn)
            .unwrap(),
        original.session_name
    );
    let (status, body) = rename(&state, Some(&owner_token), original.id, "  New 🔭 name  ").await;
    assert_eq!(status, StatusCode::OK);
    let response: RenameSessionResponse = serde_json::from_slice(&body).unwrap();
    assert_eq!(response.session_id, original.id);
    assert_eq!(response.session_name, "New 🔭 name");
    let renamed = sessions::table
        .find(original.id)
        .first::<Session>(&mut conn)
        .unwrap();
    assert_eq!(renamed.session_name, response.session_name);
    assert!(renamed.paused);
    assert_eq!(renamed.archived_at, None);
    assert_eq!(renamed.session_key, original.session_key);
    assert_eq!(renamed.status, original.status);
    assert_eq!(renamed.last_activity, original.last_activity);
    assert_eq!(renamed.last_messaged_at, original.last_messaged_at);
    assert_eq!(renamed.input_seq, original.input_seq);
    assert_eq!(renamed.working_directory, original.working_directory);
    assert_eq!(renamed.claude_args, original.claude_args);
    // Same-name retries don't invalidate an archive made after the first rename.
    diesel::update(sessions::table.find(original.id))
        .set(sessions::archived_at.eq(Some(chrono::Utc::now().naive_utc())))
        .execute(&mut conn)
        .unwrap();
    assert_eq!(
        rename(&state, Some(&owner_token), original.id, "New 🔭 name")
            .await
            .0,
        StatusCode::OK
    );
    assert!(sessions::table
        .find(original.id)
        .select(sessions::archived_at)
        .first::<Option<chrono::NaiveDateTime>>(&mut conn)
        .unwrap()
        .is_some());
    // Editors may rename, and the limit counts Unicode characters, not bytes.
    assert_eq!(
        rename(&state, Some(&editor_token), original.id, &"🔭".repeat(255))
            .await
            .0,
        StatusCode::OK
    );
}
