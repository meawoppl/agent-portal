//! Exercise the real dispatcher with PostgreSQL and a fake transport.
//! No device notifications are sent by these tests.
use super::*;
use crate::schema::{messages, session_disconnect_notifications as claims, sessions, users};
use std::sync::Mutex;

#[derive(Default)]
struct Delivery {
    attempts: Mutex<Vec<(Uuid, String)>>,
    fail: Option<Uuid>,
}

impl PushTransport for Delivery {
    async fn send(
        &self,
        sub: &PushSubscription,
        payload: &super::super::PushPayload,
    ) -> Result<SendOutcome, super::super::PushError> {
        self.attempts
            .lock()
            .unwrap()
            .push((sub.id, payload.event_kind.clone()));
        if self.fail == Some(sub.id) {
            Err(super::super::PushError::Transport(
                "test delivery failure".into(),
            ))
        } else {
            Ok(SendOutcome::Delivered)
        }
    }
}

fn disconnected(conn: &mut DbConnection, user: Uuid) -> crate::models::Session {
    let session = crate::test_support::insert_session(conn, user, "disconnect-test");
    diesel::update(sessions::table.find(session.id))
        .set((
            sessions::status.eq("disconnected"),
            sessions::input_seq.eq(1),
        ))
        .execute(conn)
        .unwrap();
    session
}

fn message(conn: &mut DbConnection, session: &crate::models::Session, role: &str, content: &str) {
    diesel::insert_into(messages::table)
        .values((
            messages::session_id.eq(session.id),
            messages::user_id.eq(session.user_id),
            messages::role.eq(role),
            messages::content.eq(content),
            messages::agent_type.eq("codex"),
        ))
        .execute(conn)
        .unwrap();
}

fn event(session: Uuid) -> NotificationEvent {
    NotificationEvent::SessionDisconnected {
        session_id: session,
        session_name: String::new(),
    }
}

#[tokio::test]
async fn disconnects_ignore_idle_sessions_and_alert_once_per_input_across_restarts() {
    let Some(pool) = crate::test_support::shared_pool() else {
        return;
    };
    let mut conn = pool.get().unwrap();
    let user = crate::test_support::insert_user(&mut conn, "disconnect");
    let empty = disconnected(&mut conn, user.id);
    let done = disconnected(&mut conn, user.id);
    let busy = disconnected(&mut conn, user.id);
    message(&mut conn, &done, "user", "finished prompt");
    message(&mut conn, &done, "result", r#"{"type":"turn.completed"}"#);
    diesel::update(messages::table.filter(messages::session_id.eq(done.id)))
        .set(messages::created_at.eq(messages::created_at
            - diesel::dsl::sql::<diesel::sql_types::Interval>("INTERVAL '7 days'")))
        .execute(&mut conn)
        .unwrap();
    message(&mut conn, &busy, "user", "unfinished prompt");
    let first = super::db_tests::make_sub(&mut conn, user.id);
    let second = super::db_tests::make_sub(&mut conn, user.id);
    let state = crate::test_support::test_app_state(pool.clone());
    let transport = Delivery::default();
    for _ in 0..3 {
        dispatch_one(&state, &transport, event(empty.id)).await;
        dispatch_one(&state, &transport, event(done.id)).await;
    }
    assert!(
        transport.attempts.lock().unwrap().is_empty(),
        "old completed and empty sessions are silent"
    );

    // An event queued before recovery or a user stop is no longer actionable.
    for (status, paused) in [
        ("active", false),
        ("inactive", false),
        ("replaced", false),
        ("disconnected", true),
    ] {
        diesel::update(sessions::table.find(busy.id))
            .set((sessions::status.eq(status), sessions::paused.eq(paused)))
            .execute(&mut conn)
            .unwrap();
        dispatch_one(&state, &transport, event(busy.id)).await;
    }
    assert!(transport.attempts.lock().unwrap().is_empty());
    diesel::update(sessions::table.find(busy.id))
        .set((
            sessions::status.eq("disconnected"),
            sessions::paused.eq(false),
        ))
        .execute(&mut conn)
        .unwrap();
    dispatch_one(&state, &transport, event(busy.id)).await;
    assert_eq!(
        transport.attempts.lock().unwrap().len(),
        2,
        "all devices receive the first alert"
    );
    for id in [first.id, second.id] {
        assert_eq!(
            transport
                .attempts
                .lock()
                .unwrap()
                .iter()
                .filter(|(sub, _)| *sub == id)
                .count(),
            1
        );
    }

    // A fresh AppState has no in-memory dedup state. Registration refreshes
    // status/last_activity; simulate repeated reconnect/drop cycles too.
    let restarted = crate::test_support::test_app_state(pool);
    for _ in 0..5 {
        diesel::update(sessions::table.find(busy.id))
            .set((
                sessions::status.eq("active"),
                sessions::last_activity.eq(diesel::dsl::now),
            ))
            .execute(&mut conn)
            .unwrap();
        diesel::update(sessions::table.find(busy.id))
            .set(sessions::status.eq("disconnected"))
            .execute(&mut conn)
            .unwrap();
        dispatch_one(&restarted, &transport, event(busy.id)).await;
    }
    assert_eq!(
        transport.attempts.lock().unwrap().len(),
        2,
        "reconnects and restarts must not rearm alerts"
    );

    // New accepted input rearms the alert. Completion/permission events
    // remain independent of the disconnect watermark.
    diesel::update(sessions::table.find(busy.id))
        .set(sessions::input_seq.eq(2))
        .execute(&mut conn)
        .unwrap();
    message(&mut conn, &busy, "user", "new prompt");
    dispatch_one(&restarted, &transport, event(busy.id)).await;
    assert_eq!(transport.attempts.lock().unwrap().len(), 4);
    for notification in [
        NotificationEvent::TurnComplete {
            session_id: busy.id,
            session_name: String::new(),
        },
        NotificationEvent::PermissionRequest {
            session_id: busy.id,
            session_name: String::new(),
            tool_name: "Bash".into(),
            input_snippet: None,
        },
    ] {
        dispatch_one(&restarted, &transport, notification).await;
    }
    assert_eq!(transport.attempts.lock().unwrap().len(), 8);
}

#[tokio::test]
async fn disconnect_suppression_does_not_consume_the_notification_claim() {
    let Some(pool) = crate::test_support::shared_pool() else {
        return;
    };
    let mut conn = pool.get().unwrap();
    let user = crate::test_support::insert_user(&mut conn, "disconnect-suppression");
    let session = disconnected(&mut conn, user.id);
    message(&mut conn, &session, "user", "unfinished prompt");
    let state = crate::test_support::test_app_state(pool);
    let transport = Delivery::default();
    dispatch_one(&state, &transport, event(session.id)).await; // no subscription
    super::db_tests::make_sub(&mut conn, user.id);
    let prefs = NotificationPrefs {
        session_disconnected: false,
        ..Default::default()
    };
    diesel::update(users::table.find(user.id))
        .set(users::notification_prefs.eq(serde_json::to_value(prefs).unwrap()))
        .execute(&mut conn)
        .unwrap();
    dispatch_one(&state, &transport, event(session.id)).await;
    diesel::update(users::table.find(user.id))
        .set(users::notification_prefs.eq(None::<serde_json::Value>))
        .execute(&mut conn)
        .unwrap();
    let mut receipt = shared::api::UpdateSessionAttention {
        client_id: Uuid::new_v4(),
        viewing: false,
        seen_reply_id: None,
        muted: Some(true),
    };
    crate::handlers::attention::update_receipt(&mut conn, user.id, session.id, &receipt).unwrap();
    dispatch_one(&state, &transport, event(session.id)).await;
    receipt.muted = Some(false);
    receipt.viewing = true;
    crate::handlers::attention::update_receipt(&mut conn, user.id, session.id, &receipt).unwrap();
    dispatch_one(&state, &transport, event(session.id)).await;
    assert!(transport.attempts.lock().unwrap().is_empty());
    assert_eq!(
        claims::table
            .find(session.id)
            .count()
            .get_result::<i64>(&mut conn)
            .unwrap(),
        0
    );
    receipt.viewing = false;
    crate::handlers::attention::update_receipt(&mut conn, user.id, session.id, &receipt).unwrap();
    dispatch_one(&state, &transport, event(session.id)).await;
    assert_eq!(transport.attempts.lock().unwrap().len(), 1);
}

#[test]
fn concurrent_disconnect_claims_are_atomic_and_deleted_with_the_session() {
    let Some(pool) = crate::test_support::shared_pool() else {
        return;
    };
    let mut conn = pool.get().unwrap();
    let user = crate::test_support::insert_user(&mut conn, "disconnect-race");
    let session = disconnected(&mut conn, user.id);
    message(&mut conn, &session, "user", "unfinished prompt");
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let pool = pool.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut conn = pool.get().unwrap();
                barrier.wait();
                super::super::disconnect::claim_disconnect(&mut conn, session.id).unwrap()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum::<usize>(),
        1
    );
    diesel::delete(sessions::table.find(session.id))
        .execute(&mut conn)
        .unwrap();
    assert_eq!(
        claims::table
            .find(session.id)
            .count()
            .get_result::<i64>(&mut conn)
            .unwrap(),
        0
    );
    assert!(!super::super::disconnect::claim_disconnect(&mut conn, session.id).unwrap());
}

#[tokio::test]
async fn partial_delivery_failure_does_not_repeat_successful_disconnect_alerts() {
    let Some(pool) = crate::test_support::shared_pool() else {
        return;
    };
    let mut conn = pool.get().unwrap();
    let user = crate::test_support::insert_user(&mut conn, "disconnect-fanout");
    let session = disconnected(&mut conn, user.id);
    message(&mut conn, &session, "user", "unfinished prompt");
    super::db_tests::make_sub(&mut conn, user.id);
    let failing_sub = super::db_tests::make_sub(&mut conn, user.id);
    let state = crate::test_support::test_app_state(pool);
    let transport = Delivery {
        fail: Some(failing_sub.id),
        ..Default::default()
    };
    for _ in 0..3 {
        dispatch_one(&state, &transport, event(session.id)).await;
    }
    assert_eq!(
        transport.attempts.lock().unwrap().len(),
        2,
        "each device gets at most one attempt for the same input"
    );
}
