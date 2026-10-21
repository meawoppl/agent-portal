//! Read-only activity-driven long poll. Wake hints may coalesce or be lost;
//! only the transactionally assigned PostgreSQL sequence advances the cursor.
mod lifecycle;

use crate::schema::{messages, session_follow_heads, session_follow_messages, session_members};
use crate::{errors::AppError, AppState};
use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    Json,
};
use diesel::prelude::*;
use shared::api::{
    follow_cursor, parse_follow_cursor, FollowEvent, FollowPage, SessionActivityState,
};
use std::{sync::Arc, time::Duration};
use tower_cookies::Cookies;
use uuid::Uuid;

#[derive(serde::Deserialize, Default)]
pub struct FollowQuery {
    pub since: Option<String>,
    /// Long polling is bounded so tokens and membership are rechecked.
    pub wait_ms: Option<u64>,
    pub state: Option<SessionActivityState>,
    pub connected: Option<bool>,
}

pub async fn follow(
    State(app): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(query): Query<FollowQuery>,
    headers: HeaderMap,
    cookies: Cookies,
) -> Result<Json<FollowPage>, AppError> {
    let user = super::agent_comms::resolve_user(&app, &headers, &cookies)?;
    follow_authorized(app, user, id, query).await
}

async fn follow_authorized(
    app: Arc<AppState>,
    user: Uuid,
    id: Uuid,
    query: FollowQuery,
) -> Result<Json<FollowPage>, AppError> {
    let since = query
        .since
        .as_deref()
        .map(|s| {
            parse_follow_cursor(s, id).ok_or(AppError::BadRequest(
                "invalid or cross-session follow cursor",
            ))
        })
        .transpose()?;
    // Subscribe BEFORE reading: a commit between read and wait cannot disappear.
    let mut wake = app.session_manager.subscribe_follow();
    let deadline =
        tokio::time::Instant::now() + Duration::from_millis(query.wait_ms.unwrap_or(0).min(30_000));
    loop {
        let page = read_page(&app, user, id, since)?;
        if since.is_none()
            || !page.events.is_empty()
            || query.state != Some(page.state)
            || query.connected != Some(page.connected)
            || tokio::time::Instant::now() >= deadline
        {
            return Ok(Json(page));
        }
        loop {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            match tokio::time::timeout_at(deadline, wake.recv()).await {
                Ok(Ok(key)) if key != id.to_string() => continue,
                // Lag and timeout both reconcile against durable rows and permission.
                _ => break,
            }
        }
    }
}

fn read_page(
    app: &AppState,
    user: Uuid,
    id: Uuid,
    since: Option<i64>,
) -> Result<FollowPage, AppError> {
    let mut conn = app.conn()?;
    let allowed = session_members::table
        .filter(session_members::session_id.eq(id))
        .filter(session_members::user_id.eq(user))
        .select(session_members::session_id)
        .first::<Uuid>(&mut conn)
        .optional()?
        .is_some();
    if !allowed {
        return Err(AppError::NotFound("session"));
    }
    let head = session_follow_heads::table
        .find(id)
        .select(session_follow_heads::sequence)
        .first::<i64>(&mut conn)
        .optional()?
        .unwrap_or(0);
    let after = since.unwrap_or(head);
    if after > head {
        return Err(AppError::Conflict(
            "follow cursor is ahead of this session; check server or restored database",
        ));
    }
    let rows = session_follow_messages::table
        .left_join(messages::table.on(messages::id.eq(session_follow_messages::message_id)))
        .filter(session_follow_messages::session_id.eq(id))
        .filter(session_follow_messages::sequence.gt(after))
        .filter(session_follow_messages::sequence.le(head))
        .order(session_follow_messages::sequence.asc())
        .limit(100)
        .select((
            session_follow_messages::sequence,
            session_follow_messages::message_id,
            messages::agent_type.nullable(),
            messages::role.nullable(),
            messages::content.nullable(),
            messages::created_at.nullable(),
        ))
        .load::<(
            i64,
            Uuid,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<chrono::NaiveDateTime>,
        )>(&mut conn)?;
    if rows.is_empty() && after < head {
        return Err(AppError::Conflict("follow journal gap"));
    }
    let mut cursor = after;
    let mut events = Vec::new();
    for (sequence, message_id, agent, role, content, time) in rows {
        let (Some(agent), Some(role), Some(content), Some(time)) = (agent, role, content, time)
        else {
            return Err(AppError::Conflict("follow retention gap; use message history --since with the same cursor to recover archived bodies"));
        };
        if sequence != cursor + 1 {
            return Err(AppError::Conflict("follow cursor gap"));
        }
        let summary =
            super::peek_summary::summarize_message(message_id, &agent, &role, &content, time);
        let lifecycle = lifecycle::classify(&agent, &content);
        let kind = match summary.kind.as_str() {
            "text"
                if role == "portal"
                    && serde_json::from_str::<shared::PortalMessage>(&content).is_ok_and(|m| {
                        m.content
                            .iter()
                            .any(|p| matches!(p, shared::PortalContent::AgentMessage { .. }))
                    }) =>
            {
                "send"
            }
            "text" if role == "user" => "input",
            "tool_use" => "tool",
            "tool_result" => "tool_result",
            other => other,
        }
        .to_string();
        events.push(FollowEvent {
            cursor: follow_cursor(id, sequence),
            message_id,
            activity: lifecycle.activity,
            kind: lifecycle.kind.map(str::to_owned).unwrap_or(kind),
            text: summary.summary,
        });
        cursor = sequence;
    }
    let state = super::agent_comms::session_activity_states(app, &mut conn, &[id])?
        .get(&id)
        .copied()
        .unwrap_or(SessionActivityState::Idle);
    Ok(FollowPage {
        cursor: follow_cursor(id, cursor),
        events,
        state,
        connected: app.session_manager.is_proxy_connected(&id.to_string()),
        more: cursor < head,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{insert_session, insert_user, shared_pool, test_app_state};
    fn insert(conn: &mut diesel::PgConnection, session: Uuid, user: Uuid, text: &str) -> Uuid {
        let id = Uuid::new_v4();
        diesel::sql_query("INSERT INTO messages(id,session_id,user_id,role,content,agent_type,created_at) VALUES($1,$2,$3,'assistant',$4,'portal','2020-01-01')")
            .bind::<diesel::sql_types::Uuid,_>(id).bind::<diesel::sql_types::Uuid,_>(session)
            .bind::<diesel::sql_types::Uuid,_>(user).bind::<diesel::sql_types::Text,_>(text).execute(conn).unwrap();
        id
    }
    #[test]
    fn replay_preserves_turn_activity_independently_of_current_connectivity() {
        let Some(pool) = shared_pool() else { return };
        let app = test_app_state(pool.clone());
        let mut conn = pool.get().unwrap();
        let user = insert_user(&mut conn, "follow-lifecycle");
        let session = insert_session(&mut conn, user.id, "follow-lifecycle");
        diesel::insert_into(session_members::table)
            .values((
                session_members::session_id.eq(session.id),
                session_members::user_id.eq(user.id),
                session_members::role.eq("owner"),
            ))
            .execute(&mut conn)
            .unwrap();
        let first = insert(&mut conn, session.id, user.id, r#"{"type":"turn.started"}"#);
        let last = insert(
            &mut conn,
            session.id,
            user.id,
            r#"{"type":"turn.failed","error":{"message":"failure"}}"#,
        );
        diesel::update(messages::table.filter(messages::session_id.eq(session.id)))
            .set(messages::agent_type.eq("codex"))
            .execute(&mut conn)
            .unwrap();
        let page = read_page(&app, user.id, session.id, Some(0)).unwrap();
        assert_eq!(page.events.len(), 2);
        assert_eq!(page.events[0].message_id, first);
        assert_eq!(page.events[0].activity, Some(SessionActivityState::Busy));
        assert_eq!(page.events[1].message_id, last);
        assert_eq!(page.events[1].activity, Some(SessionActivityState::Idle));
        assert_eq!(page.events[1].kind, "error");
        assert!(!page.connected);
        assert_eq!(page.state, SessionActivityState::Idle);
        let replay = read_page(&app, user.id, session.id, Some(1)).unwrap();
        assert_eq!(replay.events.len(), 1);
        assert_eq!(replay.events[0].message_id, last);
        assert_eq!(replay.cursor, page.cursor);
    }

    #[test]
    fn pages_replay_equal_timestamps_and_detect_retention_and_revocation() {
        let Some(pool) = shared_pool() else { return };
        let app = test_app_state(pool.clone());
        let mut conn = pool.get().unwrap();
        let user = insert_user(&mut conn, "follow");
        let session = insert_session(&mut conn, user.id, "follow");
        diesel::sql_query("INSERT INTO session_members(session_id,user_id,role) VALUES($1,$2,'owner') ON CONFLICT DO NOTHING")
            .bind::<diesel::sql_types::Uuid,_>(session.id).bind::<diesel::sql_types::Uuid,_>(user.id).execute(&mut conn).unwrap();
        let ids: Vec<_> = (0..105)
            .map(|i| insert(&mut conn, session.id, user.id, &format!("line {i}")))
            .collect();
        let first = read_page(&app, user.id, session.id, Some(0)).unwrap();
        assert_eq!(first.events.len(), 100);
        assert!(first.more);
        assert_eq!(first.events[0].message_id, ids[0]);
        let second = read_page(&app, user.id, session.id, Some(100)).unwrap();
        assert_eq!(second.events.len(), 5);
        assert_eq!(second.events[0].message_id, ids[100]);
        assert!(!second.more);
        assert!(read_page(&app, Uuid::new_v4(), session.id, Some(0)).is_err());
        assert!(read_page(&app, user.id, session.id, Some(106)).is_err());
        assert!(read_page(&app, user.id, session.id, None)
            .unwrap()
            .events
            .is_empty());
        diesel::delete(messages::table.find(ids[0]))
            .execute(&mut conn)
            .unwrap();
        assert!(matches!(
            read_page(&app, user.id, session.id, Some(0)),
            Err(AppError::Conflict(_))
        ));
        assert!(read_page(&app, user.id, session.id, Some(1)).is_ok());
        diesel::delete(session_members::table.filter(session_members::session_id.eq(session.id)))
            .execute(&mut conn)
            .unwrap();
        assert!(matches!(
            read_page(&app, user.id, session.id, Some(105)),
            Err(AppError::NotFound(_))
        ));
        diesel::delete(crate::schema::sessions::table.find(session.id))
            .execute(&mut conn)
            .unwrap();
        diesel::delete(crate::schema::users::table.find(user.id))
            .execute(&mut conn)
            .unwrap();
    }

    #[test]
    fn sequence_serializes_concurrent_commits_and_rolls_back_without_holes() {
        let Some(pool) = shared_pool() else { return };
        let mut conn = pool.get().unwrap();
        let user = insert_user(&mut conn, "follow-order");
        let session = insert_session(&mut conn, user.id, "follow-order");
        let (tx, rx) = std::sync::mpsc::channel();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let second_pool = pool.clone();
        let mut worker = None;
        conn.transaction::<_, diesel::result::Error, _>(|conn| {
            insert(conn, session.id, user.id, "first");
            worker = Some(std::thread::spawn(move || {
                let mut conn = second_pool.get().unwrap();
                ready_tx.send(()).unwrap();
                let id = insert(&mut conn, session.id, user.id, "second");
                tx.send(id).unwrap();
            }));
            // The second insert must not commit its cursor ahead of the first.
            ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
            Ok(())
        })
        .unwrap();
        let second = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.unwrap().join().unwrap();
        let sequence: i64 = session_follow_messages::table
            .filter(session_follow_messages::message_id.eq(second))
            .select(session_follow_messages::sequence)
            .first(&mut conn)
            .unwrap();
        assert_eq!(sequence, 2);
        let _: Result<(), diesel::result::Error> = conn.transaction(|conn| {
            insert(conn, session.id, user.id, "rolled back");
            Err(diesel::result::Error::RollbackTransaction)
        });
        let next = insert(&mut conn, session.id, user.id, "third");
        let sequence: i64 = session_follow_messages::table
            .filter(session_follow_messages::message_id.eq(next))
            .select(session_follow_messages::sequence)
            .first(&mut conn)
            .unwrap();
        assert_eq!(sequence, 3);
        diesel::delete(crate::schema::sessions::table.find(session.id))
            .execute(&mut conn)
            .unwrap();
        diesel::delete(crate::schema::users::table.find(user.id))
            .execute(&mut conn)
            .unwrap();
    }
    #[tokio::test]
    async fn wake_catches_committed_rows_and_rechecks_permission() {
        let Some(pool) = shared_pool() else { return };
        let app = Arc::new(test_app_state(pool.clone()));
        let mut conn = pool.get().unwrap();
        let user = insert_user(&mut conn, "follow-wake");
        let session = insert_session(&mut conn, user.id, "follow-wake");
        diesel::sql_query("INSERT INTO session_members(session_id,user_id,role) VALUES($1,$2,'owner') ON CONFLICT DO NOTHING")
            .bind::<diesel::sql_types::Uuid,_>(session.id).bind::<diesel::sql_types::Uuid,_>(user.id).execute(&mut conn).unwrap();
        let query = FollowQuery {
            since: Some(follow_cursor(session.id, 0)),
            wait_ms: Some(30000),
            state: Some(SessionActivityState::Idle),
            connected: Some(false),
        };
        let waiting = tokio::spawn(follow_authorized(app.clone(), user.id, session.id, query));
        tokio::task::yield_now().await;
        let message = insert(&mut conn, session.id, user.id, "ready");
        app.session_manager.broadcast_to_web_clients(
            &session.id.to_string(),
            shared::ServerToClient::Error {
                message: "wake".into(),
            },
        );
        let page = tokio::time::timeout(Duration::from_secs(2), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(page.events[0].message_id, message);
        let query = FollowQuery {
            since: Some(page.cursor),
            wait_ms: Some(30000),
            state: Some(SessionActivityState::Idle),
            connected: Some(false),
        };
        let waiting = tokio::spawn(follow_authorized(app.clone(), user.id, session.id, query));
        tokio::task::yield_now().await;
        diesel::delete(session_members::table.filter(session_members::session_id.eq(session.id)))
            .execute(&mut conn)
            .unwrap();
        app.session_manager.broadcast_to_web_clients(
            &session.id.to_string(),
            shared::ServerToClient::Error {
                message: "wake".into(),
            },
        );
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), waiting)
                .await
                .unwrap()
                .unwrap(),
            Err(AppError::NotFound(_))
        ));
        diesel::delete(crate::schema::sessions::table.find(session.id))
            .execute(&mut conn)
            .unwrap();
        diesel::delete(crate::schema::users::table.find(user.id))
            .execute(&mut conn)
            .unwrap();
    }
}
