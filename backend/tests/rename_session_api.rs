#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `PATCH /api/sessions/{id}/name` integration tests.
//!
//! Boots the real router against a test Postgres and checks who may rename:
//! owners and editors yes; viewers, outsiders and anonymous callers no, and a
//! refused rename must leave the stored name untouched.
//!
//! Requires `DATABASE_URL` (see `harness.rs`); skips gracefully without it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use diesel::prelude::*;
use tower::ServiceExt;
use uuid::Uuid;

use backend::handlers::proxy_tokens::{issue_proxy_token, TokenPersist};
use backend::models::{NewSessionMember, NewSessionWithId, NewUser, User};
use backend::test_support::test_app_state;
use backend::AppState;
use shared::api::{RenameSessionRequest, MAX_SESSION_NAME_CHARS};

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

fn create_user(conn: &mut PgConnection, label: &str) -> User {
    use backend::schema::users;
    diesel::insert_into(users::table)
        .values(NewUser {
            email: format!("{label}-{}@x.io", Uuid::new_v4().simple()),
            name: Some(label.to_string()),
            avatar_url: None,
        })
        .get_result(conn)
        .expect("insert user")
}

fn bearer(state: &AppState, conn: &mut PgConnection, user_id: Uuid) -> String {
    let issued = issue_proxy_token(
        conn,
        state.jwt_secret.as_bytes(),
        user_id,
        TokenPersist::Create {
            name: "rename-test",
        },
        Some(1),
    )
    .expect("issue token");
    format!("Bearer {}", issued.token)
}

struct Fixture {
    state: Arc<AppState>,
    session_id: Uuid,
    owner: String,
    editor: String,
    viewer: String,
    outsider: String,
}

fn build_fixture(pool: backend::db::DbPool) -> Fixture {
    let state = Arc::new(test_app_state(pool));
    let mut conn = state.db_pool.get().expect("conn");
    let owner = create_user(&mut conn, "owner");
    let editor = create_user(&mut conn, "editor");
    let viewer = create_user(&mut conn, "viewer");
    let outsider = create_user(&mut conn, "outsider");

    let session_id = Uuid::new_v4();
    diesel::insert_into(backend::schema::sessions::table)
        .values(NewSessionWithId {
            id: session_id,
            user_id: owner.id,
            session_name: "original".to_string(),
            session_key: format!("rename-test-{}", Uuid::new_v4().simple()),
            working_directory: "/repo".to_string(),
            status: "disconnected".to_string(),
            git_branch: None,
            client_version: None,
            hostname: "host-1".to_string(),
            launcher_id: None,
            agent_type: "claude".to_string(),
            repo_url: None,
            scheduled_task_id: None,
            paused: false,
            claude_args: serde_json::Value::Array(vec![]),
            launcher_version: None,
        })
        .execute(&mut conn)
        .expect("insert session");
    for (user, role) in [(&editor, "editor"), (&viewer, "viewer")] {
        diesel::insert_into(backend::schema::session_members::table)
            .values(NewSessionMember {
                session_id,
                user_id: user.id,
                role: role.to_string(),
            })
            .execute(&mut conn)
            .expect("insert membership");
    }

    let owner_auth = bearer(&state, &mut conn, owner.id);
    let editor_auth = bearer(&state, &mut conn, editor.id);
    let viewer_auth = bearer(&state, &mut conn, viewer.id);
    let outsider_auth = bearer(&state, &mut conn, outsider.id);
    Fixture {
        state,
        session_id,
        owner: owner_auth,
        editor: editor_auth,
        viewer: viewer_auth,
        outsider: outsider_auth,
    }
}

async fn rename(fixture: &Fixture, auth: Option<&str>, name: &str) -> StatusCode {
    let mut req = Request::builder()
        .method("PATCH")
        .uri(format!("/api/sessions/{}/name", fixture.session_id))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(auth) = auth {
        req = req.header(header::AUTHORIZATION, auth);
    }
    let body = serde_json::to_vec(&RenameSessionRequest {
        name: name.to_string(),
    })
    .unwrap();
    backend::routes::build_router(fixture.state.clone())
        .expect("router builds")
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap()
        .status()
}

fn stored_name(fixture: &Fixture) -> String {
    use backend::schema::sessions;
    let mut conn = fixture.state.db_pool.get().expect("conn");
    sessions::table
        .find(fixture.session_id)
        .select(sessions::session_name)
        .first(&mut conn)
        .expect("session row")
}

#[tokio::test]
async fn owner_and_editor_can_rename_and_the_name_is_trimmed() {
    let Some(pool) = test_pool() else { return };
    let fixture = build_fixture(pool);

    assert_eq!(
        rename(&fixture, Some(&fixture.owner), "  review pass  ").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(stored_name(&fixture), "review pass");

    assert_eq!(
        rename(&fixture, Some(&fixture.editor), "editor was here").await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(stored_name(&fixture), "editor was here");
}

#[tokio::test]
async fn viewers_outsiders_and_anonymous_callers_cannot_rename() {
    let Some(pool) = test_pool() else { return };
    let fixture = build_fixture(pool);

    assert_eq!(
        rename(&fixture, Some(&fixture.viewer), "viewer").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        rename(&fixture, Some(&fixture.outsider), "outsider").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        rename(&fixture, None, "anonymous").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(stored_name(&fixture), "original", "refusals change nothing");
}

#[tokio::test]
async fn blank_and_oversized_names_are_rejected_without_changing_anything() {
    let Some(pool) = test_pool() else { return };
    let fixture = build_fixture(pool);

    assert_eq!(
        rename(&fixture, Some(&fixture.owner), "   ").await,
        StatusCode::BAD_REQUEST
    );
    let too_long = "x".repeat(MAX_SESSION_NAME_CHARS + 1);
    assert_eq!(
        rename(&fixture, Some(&fixture.owner), &too_long).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(stored_name(&fixture), "original");
}
