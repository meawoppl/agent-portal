//! Completion state survives refresh, message retention and backend restarts.
//! Receipts belong to the reader; a shared session never marks another user's reply read.
use crate::{auth::CurrentUserId, db::DbConnection, errors::AppError, AppState};
use axum::{
    extract::{Path, State},
    Json,
};
use diesel::prelude::*;
use diesel::sql_types::{Bool, Nullable, Text, Timestamp, Uuid as SqlUuid};
use shared::api::{SessionAttention, SessionAttentionResponse, UpdateSessionAttention};
use std::sync::Arc;
use uuid::Uuid;

#[derive(QueryableByName)]
struct AttentionRow {
    #[diesel(sql_type = SqlUuid)]
    session_id: Uuid,
    #[diesel(sql_type = Nullable<SqlUuid>)]
    reply_id: Option<Uuid>,
    #[diesel(sql_type = Nullable<Timestamp>)]
    reply_at: Option<chrono::NaiveDateTime>,
    #[diesel(sql_type = Text)]
    preview: String,
    #[diesel(sql_type = Bool)]
    unread: bool,
    #[diesel(sql_type = Bool)]
    needs_approval: bool,
    #[diesel(sql_type = Bool)]
    muted: bool,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    CurrentUserId(user): CurrentUserId,
) -> Result<Json<SessionAttentionResponse>, AppError> {
    let mut conn = state.conn()?;
    let rows = diesel::sql_query("SELECT s.id AS session_id, a.reply_id, a.reply_at,
        COALESCE(a.preview, '') AS preview,
        COALESCE(a.generation > COALESCE(r.seen_generation, 0), FALSE) AS unread,
        EXISTS(SELECT 1 FROM pending_permission_requests p WHERE p.session_id = s.id) AS needs_approval,
        COALESCE(r.muted, FALSE) AS muted
        FROM sessions s JOIN session_members m ON m.session_id = s.id AND m.user_id = $1
        LEFT JOIN session_attention a ON a.session_id = s.id
        LEFT JOIN session_attention_receipts r ON r.session_id = s.id AND r.user_id = $1
        WHERE s.status <> 'replaced' AND s.scheduled_task_id IS NULL AND NOT s.paused")
        .bind::<SqlUuid,_>(user).load::<AttentionRow>(&mut conn)?;
    let ids: Vec<_> = rows.iter().map(|r| r.session_id).collect();
    let activity = super::session_activity::session_activity_states(&state, &mut conn, &ids)?;
    Ok(Json(SessionAttentionResponse {
        sessions: rows
            .into_iter()
            .map(|r| SessionAttention {
                session_id: r.session_id,
                reply_id: r.reply_id,
                reply_at: r.reply_at.map(|t| t.and_utc().to_rfc3339()),
                preview: r.preview,
                unread: r.unread,
                needs_approval: r.needs_approval,
                muted: r.muted,
                working: activity.get(&r.session_id).is_some_and(|s| s.is_active()),
                connected: state
                    .session_manager
                    .is_proxy_connected(&r.session_id.to_string()),
            })
            .collect(),
    }))
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    CurrentUserId(user): CurrentUserId,
    Path(session): Path<Uuid>,
    Json(req): Json<UpdateSessionAttention>,
) -> Result<super::responses::EmptyResponse, AppError> {
    let mut conn = state.conn()?;
    super::session_access::verify_session_reader(&mut conn, session, user)?;
    update_receipt(&mut conn, user, session, &req)?;
    Ok(super::responses::EmptyResponse::NO_CONTENT)
}

pub(crate) fn update_receipt(
    conn: &mut DbConnection,
    user: Uuid,
    session: Uuid,
    req: &UpdateSessionAttention,
) -> QueryResult<()> {
    conn.transaction(|conn| {
        if let Some(reply) = req.seen_reply_id {
            // Read a specific generation. A delayed phone ack cannot consume a newer reply.
            diesel::sql_query("INSERT INTO session_attention_receipts (session_id,user_id,seen_generation)
                SELECT session_id,$1,generation FROM session_attention WHERE session_id=$2 AND reply_id=$3
                ON CONFLICT (session_id,user_id) DO UPDATE SET seen_generation =
                GREATEST(session_attention_receipts.seen_generation, EXCLUDED.seen_generation)")
                .bind::<SqlUuid,_>(user).bind::<SqlUuid,_>(session).bind::<SqlUuid,_>(reply).execute(conn)?;
        }
        if let Some(muted) = req.muted {
            diesel::sql_query("INSERT INTO session_attention_receipts (session_id,user_id,muted) VALUES ($1,$2,$3)
                ON CONFLICT (session_id,user_id) DO UPDATE SET muted=EXCLUDED.muted")
                .bind::<SqlUuid,_>(session).bind::<SqlUuid,_>(user).bind::<Bool,_>(muted).execute(conn)?;
        }
        if req.viewing {
            diesel::sql_query("INSERT INTO session_attention_views (user_id,client_id,session_id,expires_at)
                VALUES ($1,$2,$3,now()+interval '15 seconds')
                ON CONFLICT (user_id,client_id) DO UPDATE SET session_id=EXCLUDED.session_id,expires_at=EXCLUDED.expires_at")
                .bind::<SqlUuid,_>(user).bind::<SqlUuid,_>(req.client_id).bind::<SqlUuid,_>(session).execute(conn)?;
        } else {
            diesel::sql_query("DELETE FROM session_attention_views WHERE user_id=$1 AND client_id=$2 AND session_id=$3")
                .bind::<SqlUuid,_>(user).bind::<SqlUuid,_>(req.client_id).bind::<SqlUuid,_>(session).execute(conn)?;
        }
        // Expired presence is disposable; bounded cleanup also covers crashed tabs.
        diesel::sql_query("DELETE FROM session_attention_views WHERE expires_at < now()").execute(conn)?;
        Ok(())
    })
}

/// Called only after durable output insertion. Replays already rejected by the
/// sequence gate never get here; timestamp/id ordering protects delayed records.
pub(crate) fn record_reply(
    conn: &mut DbConnection,
    row: &crate::models::Message,
) -> QueryResult<bool> {
    let preview = latest_reply_preview(conn, row)?;
    let count = diesel::sql_query("INSERT INTO session_attention (session_id,reply_id,reply_at,preview)
        VALUES ($1,$2,$3,$4) ON CONFLICT (session_id) DO UPDATE SET
        reply_id=EXCLUDED.reply_id, reply_at=EXCLUDED.reply_at, preview=EXCLUDED.preview,
        generation=session_attention.generation+1
        WHERE (session_attention.reply_at,session_attention.reply_id) < (EXCLUDED.reply_at,EXCLUDED.reply_id)")
        .bind::<SqlUuid,_>(row.session_id).bind::<SqlUuid,_>(row.id)
        .bind::<Timestamp,_>(row.created_at).bind::<Text,_>(preview).execute(conn)?;
    Ok(count > 0)
}
fn latest_reply_preview(
    conn: &mut DbConnection,
    terminal: &crate::models::Message,
) -> QueryResult<String> {
    use crate::schema::messages;
    let rows = messages::table
        .filter(messages::session_id.eq(terminal.session_id))
        .filter(messages::created_at.le(terminal.created_at))
        .order((messages::created_at.desc(), messages::id.desc()))
        .limit(32)
        .load::<crate::models::Message>(conn)?;
    for row in rows {
        if row.role == "user" {
            break;
        }
        let summary = super::peek_summary::summarize_message(
            row.id,
            &row.agent_type,
            &row.role,
            &row.content,
            row.created_at,
        );
        if summary.kind == "text" && !summary.summary.trim().is_empty() {
            return Ok(summary.summary);
        }
    }
    Ok("Response ready".into())
}
#[derive(QueryableByName)]
struct DeliveryPolicy {
    #[diesel(sql_type = Bool)]
    muted: bool,
    #[diesel(sql_type = Bool)]
    viewing: bool,
}
pub(crate) fn push_suppressed(
    conn: &mut DbConnection,
    user: Uuid,
    session: Uuid,
    always: bool,
) -> QueryResult<bool> {
    let row = diesel::sql_query("SELECT
        COALESCE((SELECT muted FROM session_attention_receipts WHERE user_id=$1 AND session_id=$2),FALSE) AS muted,
        EXISTS(SELECT 1 FROM session_attention_views WHERE user_id=$1 AND session_id=$2 AND expires_at > now()) AS viewing")
        .bind::<SqlUuid,_>(user).bind::<SqlUuid,_>(session).get_result::<DeliveryPolicy>(conn)?;
    Ok(row.muted || (!always && row.viewing))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Message, NewMessage};
    use crate::test_support::{insert_session, insert_user, shared_pool, test_app_state};

    fn member(conn: &mut DbConnection, session: Uuid, user: Uuid) {
        use crate::schema::session_members as m;
        diesel::insert_into(m::table)
            .values((
                m::id.eq(Uuid::new_v4()),
                m::session_id.eq(session),
                m::user_id.eq(user),
                m::role.eq("owner"),
            ))
            .execute(conn)
            .unwrap();
    }
    fn reply(conn: &mut DbConnection, session: Uuid, user: Uuid) -> Message {
        diesel::insert_into(crate::schema::messages::table).values(NewMessage {
            session_id:session,user_id:user,role:"unknown".into(),agent_type:"codex".into(),content:r#"{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1,"cached_input_tokens":0}}"#.into(),
            provenance_kind:None,provenance_session_id:None,provenance_agent_type:None,
        }).get_result(conn).unwrap()
    }
    fn request(id: Option<Uuid>) -> UpdateSessionAttention {
        UpdateSessionAttention {
            client_id: Uuid::new_v4(),
            viewing: false,
            seen_reply_id: id,
            muted: None,
        }
    }
    #[tokio::test]
    async fn receipts_survive_restart_and_old_ack_cannot_consume_new_reply() {
        let Some(pool) = shared_pool() else {
            return;
        };
        let mut conn = pool.get().unwrap();
        let a = insert_user(&mut conn, "attention");
        let b = insert_user(&mut conn, "attention");
        let session = insert_session(&mut conn, a.id, "attention");
        member(&mut conn, session.id, a.id);
        member(&mut conn, session.id, b.id);
        let first = reply(&mut conn, session.id, a.id);
        assert!(record_reply(&mut conn, &first).unwrap());
        assert!(
            !record_reply(&mut conn, &first).unwrap(),
            "replay must not generate another unread response"
        );
        update_receipt(&mut conn, a.id, session.id, &request(Some(first.id))).unwrap();
        let second = reply(&mut conn, session.id, a.id);
        assert!(record_reply(&mut conn, &second).unwrap());
        update_receipt(&mut conn, a.id, session.id, &request(Some(first.id))).unwrap();
        let state = Arc::new(test_app_state(pool.clone()));
        let rows = list(State(state.clone()), CurrentUserId(a.id))
            .await
            .unwrap()
            .0
            .sessions;
        assert!(
            rows.iter()
                .find(|r| r.session_id == session.id)
                .unwrap()
                .unread
        );
        update_receipt(&mut conn, a.id, session.id, &request(Some(second.id))).unwrap();
        // Restart state and retain only the durable read receipt.
        let state = Arc::new(test_app_state(pool));
        let rows = list(State(state.clone()), CurrentUserId(a.id))
            .await
            .unwrap()
            .0
            .sessions;
        assert!(
            !rows
                .iter()
                .find(|r| r.session_id == session.id)
                .unwrap()
                .unread
        );
        let rows = list(State(state), CurrentUserId(b.id))
            .await
            .unwrap()
            .0
            .sessions;
        assert!(
            rows.iter()
                .find(|r| r.session_id == session.id)
                .unwrap()
                .unread,
            "another reader is independent"
        );
    }
    #[tokio::test]
    async fn explicit_receipt_survives_retention_and_stays_reader_and_reply_scoped() {
        let Some(pool) = shared_pool() else {
            return;
        };
        let mut conn = pool.get().unwrap();
        let owner = insert_user(&mut conn, "retained-attention");
        let reader = insert_user(&mut conn, "retained-attention");
        let session = insert_session(&mut conn, owner.id, "retained-attention");
        member(&mut conn, session.id, owner.id);
        member(&mut conn, session.id, reader.id);
        let terminal = reply(&mut conn, session.id, owner.id);
        record_reply(&mut conn, &terminal).unwrap();
        // Exercise the real count-retention path, leaving only the snapshot.
        assert_eq!(
            super::super::retention::truncate_session_messages(
                &mut conn,
                session.id,
                super::super::retention::RetentionConfig::new(0, 0),
                1,
            )
            .unwrap(),
            1
        );
        let state = Arc::new(test_app_state(pool));
        update(
            State(state.clone()),
            CurrentUserId(owner.id),
            Path(session.id),
            Json(request(Some(terminal.id))),
        )
        .await
        .unwrap();
        let rows = list(State(state.clone()), CurrentUserId(owner.id))
            .await
            .unwrap()
            .0
            .sessions;
        assert!(
            !rows
                .iter()
                .find(|r| r.session_id == session.id)
                .unwrap()
                .unread
        );
        let rows = list(State(state.clone()), CurrentUserId(reader.id))
            .await
            .unwrap()
            .0
            .sessions;
        assert!(
            rows.iter()
                .find(|r| r.session_id == session.id)
                .unwrap()
                .unread
        );
        let newer = reply(&mut conn, session.id, owner.id);
        record_reply(&mut conn, &newer).unwrap();
        update(
            State(state.clone()),
            CurrentUserId(owner.id),
            Path(session.id),
            Json(request(Some(terminal.id))),
        )
        .await
        .unwrap();
        let rows = list(State(state), CurrentUserId(owner.id))
            .await
            .unwrap()
            .0
            .sessions;
        assert!(
            rows.iter()
                .find(|r| r.session_id == session.id)
                .unwrap()
                .unread
        );
    }

    #[tokio::test]
    async fn receipt_does_not_clear_approval_and_nonmembers_cannot_read_or_ack() {
        let Some(pool) = shared_pool() else {
            return;
        };
        let mut conn = pool.get().unwrap();
        let owner = insert_user(&mut conn, "attention");
        let outsider = insert_user(&mut conn, "attention");
        let session = insert_session(&mut conn, owner.id, "attention");
        member(&mut conn, session.id, owner.id);
        let terminal = reply(&mut conn, session.id, owner.id);
        record_reply(&mut conn, &terminal).unwrap();
        diesel::insert_into(crate::schema::pending_permission_requests::table)
            .values(crate::models::NewPendingPermissionRequest {
                session_id: session.id,
                request_id: "permission".into(),
                tool_name: "Bash".into(),
                input: serde_json::Value::Null,
                permission_suggestions: None,
            })
            .execute(&mut conn)
            .unwrap();
        update_receipt(&mut conn, owner.id, session.id, &request(Some(terminal.id))).unwrap();
        let state = Arc::new(test_app_state(pool));
        let rows = list(State(state.clone()), CurrentUserId(owner.id))
            .await
            .unwrap()
            .0
            .sessions;
        let row = rows.iter().find(|r| r.session_id == session.id).unwrap();
        assert!(row.ready());
        assert!(row.needs_approval);
        assert!(!row.unread);
        assert!(list(State(state.clone()), CurrentUserId(outsider.id))
            .await
            .unwrap()
            .0
            .sessions
            .is_empty());
        assert!(matches!(
            update(
                State(state),
                CurrentUserId(outsider.id),
                Path(session.id),
                Json(request(Some(terminal.id)))
            )
            .await,
            Err(AppError::NotFound(_))
        ));
    }
    #[test]
    fn pushes_only_suppress_for_unexpired_same_conversation_or_muting() {
        let Some(pool) = shared_pool() else {
            return;
        };
        let mut conn = pool.get().unwrap();
        let user = insert_user(&mut conn, "attention");
        let a = insert_session(&mut conn, user.id, "a");
        let b = insert_session(&mut conn, user.id, "b");
        assert!(!push_suppressed(&mut conn, user.id, a.id, false).unwrap());
        let mut req = request(None);
        req.viewing = true;
        update_receipt(&mut conn, user.id, a.id, &req).unwrap();
        assert!(push_suppressed(&mut conn, user.id, a.id, false).unwrap());
        assert!(!push_suppressed(&mut conn, user.id, b.id, false).unwrap());
        assert!(!push_suppressed(&mut conn, user.id, a.id, true).unwrap());
        // A suspended/crashed phone stops heartbeating; its lease expires.
        diesel::sql_query("UPDATE session_attention_views SET expires_at=now()-interval '1 second' WHERE user_id=$1").bind::<SqlUuid,_>(user.id).execute(&mut conn).unwrap();
        assert!(!push_suppressed(&mut conn, user.id, a.id, false).unwrap());
        req.viewing = false;
        req.muted = Some(true);
        update_receipt(&mut conn, user.id, a.id, &req).unwrap();
        assert!(push_suppressed(&mut conn, user.id, a.id, true).unwrap());
        req.muted = Some(false);
        update_receipt(&mut conn, user.id, a.id, &req).unwrap();
        assert!(!push_suppressed(&mut conn, user.id, a.id, false).unwrap());
    }
}
