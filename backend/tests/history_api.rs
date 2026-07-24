#![allow(clippy::expect_used, clippy::unwrap_used)]
//! `/api/history` visibility integration tests.
//!
//! Boots the real router against a test Postgres and a tempdir archive store,
//! then exercises every visibility path with per-user bearer tokens: owner,
//! manifest-member ("shared with me", surviving DB deletion), live
//! `session_members` row (share granted after the final archive), admin, and
//! outsider (404, not 403 — existence must not leak).
//!
//! Requires `DATABASE_URL` (see `harness.rs`); skips gracefully without it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use diesel::prelude::*;
use tower::ServiceExt;
use uuid::Uuid;

use archive_format::scan::test_support::manifest;
use archive_format::{
    ArchiveBackendConfig, ArchiveConfig, ArchiveMemberEntry, ArchivedMediaMeta,
    SessionArchiveBundle,
};
use backend::archive::ArchiveRuntime;
use backend::handlers::proxy_tokens::{issue_proxy_token, TokenPersist};
use backend::models::{NewSessionMember, NewSessionWithId, User};
use backend::test_support::test_app_state;
use backend::AppState;
use shared::api::HistorySessionsResponse;

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

fn create_user(conn: &mut PgConnection, email: &str, is_admin: bool) -> User {
    use backend::schema::users;
    let user: User = diesel::insert_into(users::table)
        .values(backend::models::NewUser {
            google_id: format!("hist-test-{}", Uuid::new_v4()),
            email: email.to_string(),
            name: Some(email.split('@').next().unwrap_or(email).to_string()),
            avatar_url: None,
        })
        .get_result(conn)
        .expect("insert user");
    if is_admin {
        diesel::update(users::table.find(user.id))
            .set(users::is_admin.eq(true))
            .execute(conn)
            .expect("set admin");
    }
    user
}

fn bearer(state: &AppState, conn: &mut PgConnection, user_id: Uuid) -> String {
    let issued = issue_proxy_token(
        conn,
        state.jwt_secret.as_bytes(),
        user_id,
        TokenPersist::Create {
            name: "history-test",
        },
        Some(1),
    )
    .expect("issue token");
    format!("Bearer {}", issued.token)
}

/// The full multi-user fixture: a tempdir archive with three sessions owned
/// by `owner`, differing in how (and whether) they are shared.
struct Fixture {
    state: Arc<AppState>,
    _archive_dir: tempfile::TempDir,
    owner_auth: String,
    manifest_member_auth: String,
    live_member_auth: String,
    outsider_auth: String,
    admin_auth: String,
    owner_id: Uuid,
    own_session: Uuid,
    shared_session: Uuid,
    live_shared_session: Uuid,
    media_id: Uuid,
}

fn day(d: u32) -> chrono::NaiveDateTime {
    chrono::NaiveDate::from_ymd_opt(2026, 7, d)
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
}

fn build_fixture(pool: backend::db::DbPool) -> Fixture {
    let archive_dir = tempfile::tempdir().expect("tempdir");
    let runtime = ArchiveRuntime::new(ArchiveConfig {
        backend: ArchiveBackendConfig::Local {
            root: archive_dir.path().to_path_buf(),
        },
        transcripts: true,
        media: true,
    })
    .expect("archive runtime");

    let mut state = test_app_state(pool);
    state.archive = Some(Arc::new(runtime));
    let state = Arc::new(state);
    let runtime = state.archive.clone().unwrap();

    let mut conn = state.db_pool.get().expect("conn");
    let suffix = Uuid::new_v4().simple().to_string();
    let owner = create_user(&mut conn, &format!("owner-{suffix}@x.io"), false);
    let manifest_member = create_user(&mut conn, &format!("shared-{suffix}@x.io"), false);
    let live_member = create_user(&mut conn, &format!("live-{suffix}@x.io"), false);
    let outsider = create_user(&mut conn, &format!("outsider-{suffix}@x.io"), false);
    let admin = create_user(&mut conn, &format!("admin-{suffix}@x.io"), true);

    let own_session = Uuid::new_v4();
    let shared_session = Uuid::new_v4();
    let live_shared_session = Uuid::new_v4();

    // Session 1: owner-only, with a transcript line and a media blob.
    let mut m1 = manifest(
        owner.id,
        own_session,
        &owner.email,
        "own refactor",
        "claude",
        day(14),
    );
    m1.message_counts.insert("user".into(), 1);
    let line = archive_format::ArchiveMessageLine {
        id: Uuid::new_v4(),
        role: "user".to_string(),
        created_at: day(14),
        agent_type: "claude".to_string(),
        content: serde_json::Value::String("hello archive".to_string()),
    };
    let mut ndjson = serde_json::to_vec(&line).unwrap();
    ndjson.push(b'\n');
    runtime
        .store
        .put_session_archive(&SessionArchiveBundle {
            manifest: m1,
            transcript_ndjson: Some(ndjson),
        })
        .expect("archive session 1");
    let media_id = Uuid::new_v4();
    runtime
        .store
        .put_media(
            owner.id,
            own_session,
            &ArchivedMediaMeta {
                media_id,
                kind: "image".to_string(),
                content_type: "image/png".to_string(),
                filename: Some("plot.png".to_string()),
                bytes: 8,
                uploaded_at: day(14),
            },
            b"PNGBYTES",
        )
        .expect("archive media");

    // Session 2: shared with `manifest_member` via the manifest snapshot
    // (the hot DB rows for it no longer exist).
    let mut m2 = manifest(
        owner.id,
        shared_session,
        &owner.email,
        "shared docs pass",
        "claude",
        day(13),
    );
    m2.members = Some(vec![ArchiveMemberEntry {
        user_id: manifest_member.id,
        email: manifest_member.email.clone(),
        role: "viewer".to_string(),
    }]);
    runtime
        .store
        .put_session_archive(&SessionArchiveBundle {
            manifest: m2,
            transcript_ndjson: None,
        })
        .expect("archive session 2");

    // Session 3: archived with no members (pre-share archive), but a live
    // sessions + session_members row grants `live_member` access.
    let m3 = manifest(
        owner.id,
        live_shared_session,
        &owner.email,
        "late share",
        "codex",
        day(12),
    );
    runtime
        .store
        .put_session_archive(&SessionArchiveBundle {
            manifest: m3,
            transcript_ndjson: None,
        })
        .expect("archive session 3");
    diesel::insert_into(backend::schema::sessions::table)
        .values(NewSessionWithId {
            id: live_shared_session,
            user_id: owner.id,
            session_name: "late share".to_string(),
            session_key: format!("hist-test-{suffix}"),
            working_directory: "/repo".to_string(),
            status: "disconnected".to_string(),
            git_branch: None,
            client_version: None,
            hostname: "host-1".to_string(),
            launcher_id: None,
            agent_type: "codex".to_string(),
            repo_url: None,
            scheduled_task_id: None,
            paused: false,
            claude_args: serde_json::Value::Array(vec![]),
            launcher_version: None,
        })
        .execute(&mut conn)
        .expect("insert session row");
    diesel::insert_into(backend::schema::session_members::table)
        .values(NewSessionMember {
            session_id: live_shared_session,
            user_id: live_member.id,
            role: "viewer".to_string(),
        })
        .execute(&mut conn)
        .expect("insert membership");

    let owner_auth = bearer(&state, &mut conn, owner.id);
    let manifest_member_auth = bearer(&state, &mut conn, manifest_member.id);
    let live_member_auth = bearer(&state, &mut conn, live_member.id);
    let outsider_auth = bearer(&state, &mut conn, outsider.id);
    let admin_auth = bearer(&state, &mut conn, admin.id);

    Fixture {
        state,
        _archive_dir: archive_dir,
        owner_auth,
        manifest_member_auth,
        live_member_auth,
        outsider_auth,
        admin_auth,
        owner_id: owner.id,
        own_session,
        shared_session,
        live_shared_session,
        media_id,
    }
}

async fn get(fixture: &Fixture, auth: Option<&str>, uri: &str) -> axum::response::Response {
    let mut req = Request::builder().uri(uri);
    if let Some(auth) = auth {
        req = req.header(header::AUTHORIZATION, auth);
    }
    backend::routes::build_router(fixture.state.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn list_sessions(fixture: &Fixture, auth: &str) -> HistorySessionsResponse {
    let resp = get(fixture, Some(auth), "/api/history/sessions").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Which of the fixture's three sessions a listing contains. Other tests'
/// archived sessions may share the store? No — each fixture uses its own
/// tempdir, so the visible set is exactly the fixture's.
fn ids(resp: &HistorySessionsResponse) -> Vec<String> {
    resp.sessions.iter().map(|s| s.session_id.clone()).collect()
}

#[tokio::test]
async fn history_visibility_owner_shared_live_admin_outsider() {
    let Some(pool) = test_pool() else { return };
    let f = build_fixture(pool);

    // Owner sees all three of their sessions, newest first.
    let owner_list = list_sessions(&f, &f.owner_auth).await;
    assert!(!owner_list.is_admin);
    assert_eq!(
        ids(&owner_list),
        vec![
            f.own_session.to_string(),
            f.shared_session.to_string(),
            f.live_shared_session.to_string(),
        ]
    );

    // Manifest-member sees only the manifest-shared session.
    let shared_list = list_sessions(&f, &f.manifest_member_auth).await;
    assert_eq!(ids(&shared_list), vec![f.shared_session.to_string()]);

    // Live-membership (share granted after archive) sees only that session.
    let live_list = list_sessions(&f, &f.live_member_auth).await;
    assert_eq!(ids(&live_list), vec![f.live_shared_session.to_string()]);

    // Outsider sees nothing.
    let outsider_list = list_sessions(&f, &f.outsider_auth).await;
    assert_eq!(ids(&outsider_list), Vec::<String>::new());

    // Admin sees all three (their listing may also contain sessions other
    // concurrently-running DB tests created — but the archive store is this
    // fixture's tempdir, so it is exactly ours).
    let admin_list = list_sessions(&f, &f.admin_auth).await;
    assert!(admin_list.is_admin);
    assert_eq!(admin_list.sessions.len(), 3);
}

#[tokio::test]
async fn history_per_session_endpoints_enforce_visibility() {
    let Some(pool) = test_pool() else { return };
    let f = build_fixture(pool);
    let owner = f.owner_id;

    // Manifest-member can read the shared session's manifest…
    let uri = format!(
        "/api/history/sessions/{owner}/{}/manifest",
        f.shared_session
    );
    let resp = get(&f, Some(&f.manifest_member_auth), &uri).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // …but the owner-only session 404s for them (no existence leak).
    let uri = format!("/api/history/sessions/{owner}/{}/manifest", f.own_session);
    let resp = get(&f, Some(&f.manifest_member_auth), &uri).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Live-membership grants the messages endpoint (empty transcript → 200).
    let uri = format!(
        "/api/history/sessions/{owner}/{}/messages",
        f.live_shared_session
    );
    let resp = get(&f, Some(&f.live_member_auth), &uri).await;
    assert_eq!(resp.status(), StatusCode::OK);

    // Owner reads their transcript NDJSON.
    let uri = format!("/api/history/sessions/{owner}/{}/messages", f.own_session);
    let resp = get(&f, Some(&f.owner_auth), &uri).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("hello archive"), "messages: {text}");

    // Media honors Range for the owner…
    let uri = format!(
        "/api/history/media/{owner}/{}/{}",
        f.own_session, f.media_id
    );
    let req = Request::builder()
        .uri(&uri)
        .header(header::AUTHORIZATION, &f.owner_auth)
        .header(header::RANGE, "bytes=0-3")
        .body(Body::empty())
        .unwrap();
    let resp = backend::routes::build_router(f.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        resp.headers().get(header::CONTENT_RANGE).unwrap(),
        "bytes 0-3/8"
    );

    // …and 404s for the outsider.
    let resp = get(&f, Some(&f.outsider_auth), &uri).await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // Unauthenticated requests are rejected outright.
    let resp = get(&f, None, "/api/history/sessions").await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

/// Close = archive-then-delete: DELETE `/api/sessions/{id}` on a session the
/// sweep never archived must take a final snapshot first, so the transcript
/// stays readable in History after the hot rows are gone.
#[tokio::test]
async fn close_session_takes_final_archive_before_delete() {
    let Some(pool) = test_pool() else { return };
    let f = build_fixture(pool);
    let mut conn = f.state.db_pool.get().expect("conn");

    // A live, never-archived session with one hot message row.
    let session_id = Uuid::new_v4();
    diesel::insert_into(backend::schema::sessions::table)
        .values(NewSessionWithId {
            id: session_id,
            user_id: f.owner_id,
            session_name: "closing time".to_string(),
            session_key: format!("close-test-{session_id}"),
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
    diesel::insert_into(backend::schema::messages::table)
        .values(backend::models::NewMessage {
            session_id,
            role: "user".to_string(),
            content: r#"{"type":"user","content":"remember me"}"#.to_string(),
            user_id: f.owner_id,
            agent_type: "claude".to_string(),
            provenance_kind: None,
            provenance_session_id: None,
            provenance_agent_type: None,
        })
        .execute(&mut conn)
        .expect("insert message");

    // Close it.
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/sessions/{session_id}"))
        .header(header::AUTHORIZATION, &f.owner_auth)
        .body(Body::empty())
        .unwrap();
    let resp = backend::routes::build_router(f.state.clone())
        .oneshot(req)
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // Hot row is gone…
    use backend::schema::sessions;
    let remaining: i64 = sessions::table
        .filter(sessions::id.eq(session_id))
        .count()
        .get_result(&mut conn)
        .expect("count");
    assert_eq!(remaining, 0);

    // …but the final archive snapshot survives and serves the transcript.
    let uri = format!("/api/history/sessions/{}/{session_id}/messages", f.owner_id);
    let resp = get(&f, Some(&f.owner_auth), &uri).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("remember me"), "archived transcript: {text}");
}
