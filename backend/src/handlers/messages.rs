use crate::auth::CurrentUserId;
use crate::errors::AppError;
use crate::handlers::helpers::{parse_iso_cursor, sender_names};
use crate::handlers::session_access::verify_session_mutator;
use crate::models::{Message, NewMessage};
use crate::schema::messages;
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use shared::api::MessagesListResponse;
use std::sync::Arc;

/// Hard upper bound on `limit` for `GET /api/sessions/{id}/messages`.
///
/// Beyond this we return `400 BAD_REQUEST` so a misbehaving client can't ask
/// the backend to materialize an unbounded list (which was the pre-#788
/// behavior — the handler used to `.load(...)` the entire session history
/// every call).
pub const MAX_LIST_MESSAGES_LIMIT: i64 = 1000;

/// Query parameters for `GET /api/sessions/{id}/messages`.
///
/// All three fields are optional; the no-params case (which is the only one
/// the current frontend issues) returns the *last* `MESSAGE_RETENTION_COUNT`
/// messages in chronological order, matching the frontend's render-window
/// trimming so the wire payload stops being a `O(session_lifetime)` blob.
///
/// Cursors:
/// - `before` returns messages strictly older than the given timestamp
///   (back-pagination — fetch a page of older messages above the current
///   render window).
/// - `after` returns messages strictly newer than the given timestamp
///   (forward-pagination — top-up after a stale tab regains focus). When
///   `after` is set, the query is `created_at ASC` so the natural page
///   order is "oldest new message first."
///
/// `before` and `after` may not be combined — that's a `400 BAD_REQUEST`.
/// Both accept the RFC 3339-ish naive ISO format produced by
/// `Message.created_at` serialization (e.g. `2026-05-17T12:34:56.789`).
#[derive(Debug, Default, Deserialize)]
pub struct ListMessagesQuery {
    /// Max messages to return. Defaults to `AppState.message_retention_count`
    /// when omitted, clamped to `MAX_LIST_MESSAGES_LIMIT`; negative or zero
    /// values are rejected with `400 BAD_REQUEST`.
    #[serde(default)]
    pub limit: Option<i64>,
    /// Back-pagination cursor: only return messages with
    /// `created_at < before`.
    #[serde(default)]
    pub before: Option<String>,
    /// Forward-pagination cursor: only return messages with
    /// `created_at > after`.
    #[serde(default)]
    pub after: Option<String>,
}

/// Request body for creating a new message
#[derive(Debug, Deserialize)]
pub struct CreateMessageRequest {
    pub role: String,
    pub content: String,
}

/// Response for message operations
#[derive(Debug, Serialize)]
pub struct MessageResponse {
    pub message: Message,
}

/// A message with optional sender name (for user-role messages in shared sessions)
#[derive(Debug, Serialize)]
pub struct MessageWithSender {
    #[serde(flatten)]
    pub message: Message,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sender_name: Option<String>,
}

/// Verify that a user has access to a session (is a member with any role)
fn verify_session_access(
    conn: &mut diesel::pg::PgConnection,
    session_id: uuid::Uuid,
    user_id: uuid::Uuid,
) -> Result<crate::models::Session, AppError> {
    use crate::schema::{session_members, sessions};
    sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .select(crate::models::Session::as_select())
        .first::<crate::models::Session>(conn)
        .map_err(|_| AppError::NotFound("Session not found"))
}

/// Create a new message for a session
pub async fn create_message(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<uuid::Uuid>,
    Json(req): Json<CreateMessageRequest>,
) -> Result<Json<MessageResponse>, AppError> {
    let mut conn = app_state.conn()?;

    // Creating a message is a mutation — require editor/owner role (or the
    // session-row owner). See `session_access` for the layered rules.
    let session = verify_session_mutator(&mut conn, session_id, current_user_id)?;

    let new_message = NewMessage {
        session_id,
        role: req.role,
        content: req.content,
        user_id: current_user_id,
        agent_type: session.agent_type.clone(),
    };

    let message: Message = diesel::insert_into(messages::table)
        .values(&new_message)
        .get_result(&mut conn)?;

    app_state.session_manager.queue_truncation(session_id);

    Ok(Json(MessageResponse { message }))
}

/// List messages for a session
///
/// Pagination contract (see [`ListMessagesQuery`]):
/// - No params → last `AppState.message_retention_count` messages,
///   chronological order (the only call shape today's frontend issues).
/// - `?limit=N` → at most `N` (clamped to [`MAX_LIST_MESSAGES_LIMIT`]).
/// - `?before=TS` → newest page strictly older than `TS` (back-pagination).
/// - `?after=TS` → oldest page strictly newer than `TS` (forward-pagination).
/// - `before` and `after` together → `400 BAD_REQUEST`.
///
/// Closes #788: previously this handler `.load(...)`'d the full history with
/// no SQL limit and the frontend trimmed locally — for long sessions every
/// reload pushed `O(session_lifetime)` bytes over the wire just to discard
/// all but the trailing N. SQL now does the trim.
pub async fn list_messages(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(current_user_id): CurrentUserId,
    Path(session_id): Path<uuid::Uuid>,
    Query(params): Query<ListMessagesQuery>,
) -> Result<Json<MessagesListResponse<MessageWithSender>>, AppError> {
    let mut conn = app_state.conn()?;

    let _session = verify_session_access(&mut conn, session_id, current_user_id)?;

    // Validate + parse params before touching the DB so a malformed cursor
    // fails fast with `400` rather than e.g. silently returning everything
    // (the historic behavior when `replay_after` failed to parse).
    if params.before.is_some() && params.after.is_some() {
        return Err(AppError::BadRequest(
            "`before` and `after` are mutually exclusive",
        ));
    }
    if let Some(l) = params.limit {
        if l <= 0 {
            return Err(AppError::BadRequest("`limit` must be > 0"));
        }
        if l > MAX_LIST_MESSAGES_LIMIT {
            return Err(AppError::BadRequest("`limit` exceeds maximum"));
        }
    }
    let limit = params
        .limit
        .unwrap_or(app_state.message_retention_count)
        .min(MAX_LIST_MESSAGES_LIMIT);

    let before_ts = match params.before.as_deref() {
        Some(s) => Some(parse_iso_cursor(s).ok_or(AppError::BadRequest(
            "`before` is not a valid ISO timestamp",
        ))?),
        None => None,
    };
    let after_ts = match params.after.as_deref() {
        Some(s) => Some(
            parse_iso_cursor(s)
                .ok_or(AppError::BadRequest("`after` is not a valid ISO timestamp"))?,
        ),
        None => None,
    };

    // Build the page. When forward-paging (`after`) we order ASC and the page
    // already arrives oldest-first. Otherwise (default + `before`) we order
    // DESC with a limit so the DB returns just the tail; then we reverse
    // in-memory before serializing so the wire shape stays chronological —
    // matches the prior `order(created_at.asc())` contract that the frontend
    // depends on (it appends to a chronological vector and trims the front).
    let message_list: Vec<Message> = if let Some(after) = after_ts {
        messages::table
            .filter(messages::session_id.eq(session_id))
            .filter(messages::created_at.gt(after))
            .order(messages::created_at.asc())
            .limit(limit)
            .load(&mut conn)?
    } else {
        let mut query = messages::table
            .filter(messages::session_id.eq(session_id))
            .into_boxed();
        if let Some(before) = before_ts {
            query = query.filter(messages::created_at.lt(before));
        }
        let mut newest_first: Vec<Message> = query
            .order(messages::created_at.desc())
            .limit(limit)
            .load(&mut conn)?;
        newest_first.reverse();
        newest_first
    };

    // Look up sender names for user-role messages
    let user_names = sender_names(&mut conn, &message_list);

    let total = message_list.len() as i64;
    let enriched: Vec<MessageWithSender> = message_list
        .into_iter()
        .map(|msg| {
            let sender_name = if msg.role == "user" {
                user_names.get(&msg.user_id).cloned()
            } else {
                None
            };
            MessageWithSender {
                message: msg,
                sender_name,
            }
        })
        .collect();

    Ok(Json(MessagesListResponse {
        messages: enriched,
        total,
    }))
}

// =============================================================================
// Unit tests (pure parsing / validation logic — no DB required)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_messages_query_defaults_all_none() {
        // serde-default round-trip via JSON (Axum's `Query` extractor uses
        // serde_urlencoded but it's not a direct dep here; JSON exercises
        // the same `#[serde(default)]` plumbing).
        let q: ListMessagesQuery = serde_json::from_str("{}").expect("empty");
        assert!(q.limit.is_none());
        assert!(q.before.is_none());
        assert!(q.after.is_none());
    }

    #[test]
    fn list_messages_query_parses_full_form() {
        let q: ListMessagesQuery =
            serde_json::from_str(r#"{"limit":42,"before":"2026-05-17T12:34:56.789Z"}"#)
                .expect("parse");
        assert_eq!(q.limit, Some(42));
        assert_eq!(q.before.as_deref(), Some("2026-05-17T12:34:56.789Z"));
        assert!(q.after.is_none());
    }

    #[test]
    fn max_list_messages_limit_is_sane() {
        // Guard against an accidental rename / number change that would
        // either re-introduce the unbounded read (very large) or make the
        // default unusable (very small). 1000 = roughly 10x the default
        // render window, plenty for a "load older" UI to walk back.
        assert_eq!(MAX_LIST_MESSAGES_LIMIT, 1000);
    }
}

// =============================================================================
// DB-touching integration tests for the pagination contract.
//
// These mirror the harness pattern in `session_access.rs::db_tests`: auto-skip
// when `DATABASE_URL` is not set (so CI stays green without a DB) and run
// locally via:
//
//   DATABASE_URL=postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal \
//     cargo test -p backend handlers::messages::db_tests
//
// Each test seeds a fresh session + N messages with strictly-monotonic
// `created_at` stamps (NaiveDateTime, microsecond-bumped) and exercises the
// raw query path that `list_messages` builds. We test the *query* layer
// rather than the Axum handler because the handler depends on `AppState`
// (cookie key, OAuth client, etc.) which is non-trivial to mock; the DB
// query is the load-bearing piece — the param validation is covered by
// the pure unit tests above.
// =============================================================================
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::models::{NewSessionWithId, NewUser, Session, User};
    use chrono::Utc;
    use diesel::r2d2::{ConnectionManager, Pool};

    type DbPool = Pool<ConnectionManager<diesel::pg::PgConnection>>;

    fn try_pool() -> Option<DbPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let manager = ConnectionManager::<diesel::pg::PgConnection>::new(url);
        Pool::builder().build(manager).ok()
    }

    fn make_user(conn: &mut diesel::pg::PgConnection, label: &str) -> User {
        use crate::schema::users;
        let nonce = uuid::Uuid::new_v4();
        let new_user = NewUser {
            google_id: format!("test_messages_{}_{}", label, nonce),
            email: format!("test_messages_{}_{}@example.invalid", label, nonce),
            name: Some(format!("Test {}", label)),
            avatar_url: None,
        };
        diesel::insert_into(users::table)
            .values(&new_user)
            .get_result::<User>(conn)
            .expect("insert test user")
    }

    fn make_session(conn: &mut diesel::pg::PgConnection, owner_id: uuid::Uuid) -> Session {
        use crate::schema::sessions;
        let session_id = uuid::Uuid::new_v4();
        let new_session = NewSessionWithId {
            id: session_id,
            user_id: owner_id,
            session_name: format!("test-msg-session-{}", session_id),
            session_key: session_id.to_string(),
            working_directory: "/tmp".to_string(),
            status: shared::SessionStatus::Active.as_str().to_string(),
            git_branch: None,
            client_version: None,
            hostname: "test-host".to_string(),
            launcher_id: None,
            agent_type: "claude".to_string(),
            repo_url: None,
            scheduled_task_id: None,
            paused: false,
            claude_args: serde_json::Value::Array(Vec::new()),
        };
        diesel::insert_into(sessions::table)
            .values(&new_session)
            .get_result::<Session>(conn)
            .expect("insert test session")
    }

    /// Seed `count` messages with strictly increasing `created_at`. We use
    /// raw SQL for the timestamp override because `NewMessage` doesn't
    /// expose `created_at` (the DB default fills it on insert).
    fn seed_messages(
        conn: &mut diesel::pg::PgConnection,
        session_id: uuid::Uuid,
        user_id: uuid::Uuid,
        count: usize,
    ) -> Vec<chrono::NaiveDateTime> {
        use diesel::sql_query;
        let base = Utc::now().naive_utc();
        let mut stamps = Vec::with_capacity(count);
        for i in 0..count {
            // Microsecond-step keeps the order strict and avoids fractional
            // collisions when the DB timestamp's microsecond resolution
            // would otherwise round neighboring inserts together.
            let ts = base + chrono::Duration::microseconds((i as i64 + 1) * 1000);
            stamps.push(ts);
            sql_query(
                "INSERT INTO messages (id, session_id, role, content, created_at, user_id, agent_type)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind::<diesel::sql_types::Uuid, _>(uuid::Uuid::new_v4())
            .bind::<diesel::sql_types::Uuid, _>(session_id)
            .bind::<diesel::sql_types::VarChar, _>("assistant")
            .bind::<diesel::sql_types::Text, _>(format!("msg #{}", i))
            .bind::<diesel::sql_types::Timestamp, _>(ts)
            .bind::<diesel::sql_types::Uuid, _>(user_id)
            .bind::<diesel::sql_types::VarChar, _>("claude")
            .execute(conn)
            .expect("seed message");
        }
        stamps
    }

    fn cleanup(
        conn: &mut diesel::pg::PgConnection,
        session_id: uuid::Uuid,
        user_ids: &[uuid::Uuid],
    ) {
        use crate::schema::{messages, session_members, sessions, users};
        let _ = diesel::delete(messages::table.filter(messages::session_id.eq(session_id)))
            .execute(conn);
        let _ = diesel::delete(
            session_members::table.filter(session_members::session_id.eq(session_id)),
        )
        .execute(conn);
        let _ = diesel::delete(sessions::table.find(session_id)).execute(conn);
        for uid in user_ids {
            let _ = diesel::delete(users::table.find(uid)).execute(conn);
        }
    }

    /// Default (no params) path: returns the last `limit` rows in
    /// chronological order, matching the frontend's render-window trim.
    /// This is the load-bearing case for #788 — the only call the current
    /// frontend issues.
    #[test]
    fn default_returns_last_n_chronological() {
        let Some(pool) = try_pool() else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let mut conn = pool.get().expect("conn");

        let user = make_user(&mut conn, "default");
        let session = make_session(&mut conn, user.id);
        let stamps = seed_messages(&mut conn, session.id, user.id, 250);

        // Same DESC + reverse pattern the handler uses for the no-cursor case.
        let default_limit: i64 = 100;
        let mut page: Vec<Message> = messages::table
            .filter(messages::session_id.eq(session.id))
            .order(messages::created_at.desc())
            .limit(default_limit)
            .load(&mut conn)
            .expect("load");
        page.reverse();

        cleanup(&mut conn, session.id, &[user.id]);

        assert_eq!(page.len(), default_limit as usize);
        // First row of the page == 250 - 100 = the 150th seeded row.
        assert_eq!(page.first().unwrap().created_at, stamps[150]);
        // Last row of the page == newest seeded row.
        assert_eq!(page.last().unwrap().created_at, stamps[249]);
        // Strict chronological ASC inside the page.
        for w in page.windows(2) {
            assert!(w[0].created_at < w[1].created_at);
        }
    }

    /// `before=TS` returns the page of messages strictly older than `TS`,
    /// limited to `limit` rows, chronological order.
    #[test]
    fn before_cursor_returns_older_page() {
        let Some(pool) = try_pool() else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let mut conn = pool.get().expect("conn");

        let user = make_user(&mut conn, "before");
        let session = make_session(&mut conn, user.id);
        let stamps = seed_messages(&mut conn, session.id, user.id, 50);

        // Cursor sits at index 30 — we expect rows 20..30 (10 entries).
        let cursor = stamps[30];
        let limit: i64 = 10;
        let mut page: Vec<Message> = messages::table
            .filter(messages::session_id.eq(session.id))
            .filter(messages::created_at.lt(cursor))
            .order(messages::created_at.desc())
            .limit(limit)
            .load(&mut conn)
            .expect("load");
        page.reverse();

        cleanup(&mut conn, session.id, &[user.id]);

        assert_eq!(page.len(), 10);
        // Strictly before the cursor.
        for m in &page {
            assert!(m.created_at < cursor);
        }
        // Page covers stamps[20..30].
        assert_eq!(page.first().unwrap().created_at, stamps[20]);
        assert_eq!(page.last().unwrap().created_at, stamps[29]);
    }

    /// `after=TS` returns the page of messages strictly newer than `TS`,
    /// limited to `limit` rows, chronological order.
    #[test]
    fn after_cursor_returns_newer_page() {
        let Some(pool) = try_pool() else {
            eprintln!("DATABASE_URL not set, skipping");
            return;
        };
        let mut conn = pool.get().expect("conn");

        let user = make_user(&mut conn, "after");
        let session = make_session(&mut conn, user.id);
        let stamps = seed_messages(&mut conn, session.id, user.id, 50);

        let cursor = stamps[20];
        let limit: i64 = 10;
        let page: Vec<Message> = messages::table
            .filter(messages::session_id.eq(session.id))
            .filter(messages::created_at.gt(cursor))
            .order(messages::created_at.asc())
            .limit(limit)
            .load(&mut conn)
            .expect("load");

        cleanup(&mut conn, session.id, &[user.id]);

        assert_eq!(page.len(), 10);
        for m in &page {
            assert!(m.created_at > cursor);
        }
        // First row after the cursor is index 21.
        assert_eq!(page.first().unwrap().created_at, stamps[21]);
        assert_eq!(page.last().unwrap().created_at, stamps[30]);
    }
}
