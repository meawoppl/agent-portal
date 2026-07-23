//! The push dispatcher task (mobile-apps plan §8.2 delivery policy).
//!
//! One long-lived tokio task drains the notification channel and, for each
//! [`NotificationEvent`]:
//!
//! 1. resolves the recipient (the session's owning user) and a display name;
//! 2. suppresses the push when that user has a live web client — in-app
//!    WS delivery already covers them (§8.2). Presence is trustworthy thanks to
//!    the eager-prune work in #1291;
//! 3. filters on the user's notification preferences;
//! 4. loads the user's non-disabled subscriptions and delivers to each via the
//!    [`PushTransport`], updating `last_success_at` / `disabled_at` and logging
//!    real failures under the stable `PUSH_DISPATCH_FAILED` marker.

use std::sync::Arc;

use diesel::prelude::*;
use shared::api::NotificationPrefs;
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, error};
use uuid::Uuid;

use crate::db::DbConnection;
use crate::markers::PUSH_DISPATCH_FAILED;
use crate::models::PushSubscription;
use crate::push::transport::{PushTransport, SendOutcome};
use crate::push::{ConfiguredTransport, NotificationEvent};
use crate::AppState;

/// Spawn the dispatcher task. Consumes the receiver end of the notification
/// channel; runs until the channel closes (backend shutdown).
pub fn spawn_dispatcher(
    app_state: Arc<AppState>,
    mut rx: UnboundedReceiver<NotificationEvent>,
    transport: ConfiguredTransport,
) {
    tokio::spawn(async move {
        tracing::info!("push dispatcher started");
        while let Some(event) = rx.recv().await {
            dispatch_one(&app_state, &transport, event).await;
        }
        tracing::warn!("push notification channel closed; dispatcher exiting");
    });
}

/// Apply the §8.2 delivery policy to a single event. Never propagates errors —
/// a push is best-effort and must never wedge the dispatcher.
async fn dispatch_one<T: PushTransport>(
    app_state: &AppState,
    transport: &T,
    event: NotificationEvent,
) {
    let session_id = event.session_id();

    let mut conn = match app_state.db_pool.get() {
        Ok(conn) => conn,
        Err(e) => {
            error!("{PUSH_DISPATCH_FAILED}: no DB connection resolving push recipient: {e}");
            return;
        }
    };

    // (a) Resolve recipient + name in one query. A missing row (session
    // deleted between emit and dispatch) means we can't route — skip.
    let resolved = match resolve_recipient(&mut conn, session_id) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            debug!("push skipped: session {session_id} has no row (deleted?)");
            return;
        }
        Err(e) => {
            error!("{PUSH_DISPATCH_FAILED}: recipient lookup failed for {session_id}: {e}");
            return;
        }
    };
    let (user_id, db_name, prefs) = resolved;

    // (b) Suppress when the user has a live web client — in-app WS delivery
    // already covers them. Presence is trustworthy post-#1291 (eager prune).
    if app_state.session_manager.has_user_client(user_id) {
        debug!(
            "push suppressed for user {user_id} (session {session_id}): live web client present"
        );
        return;
    }

    // (c) Prefs filter, on the user's stored prefs (C6 storage, loaded in the
    // same query as the recipient above).
    if !event.is_enabled(&prefs) {
        debug!(
            "push suppressed for user {user_id}: event kind {} disabled by prefs",
            event.event_kind()
        );
        return;
    }

    // Prefer the name the hook supplied (freshest at the source); fall back to
    // the DB name when the hook didn't have it cheaply.
    let name = if event.session_name().is_empty() {
        db_name
    } else {
        event.session_name().to_string()
    };
    let payload = event.into_payload(name, prefs.content_detail);

    // (d) Fan out to every non-disabled subscription for the user.
    let subs: Vec<PushSubscription> = match crate::schema::push_subscriptions::table
        .filter(crate::schema::push_subscriptions::user_id.eq(user_id))
        .filter(crate::schema::push_subscriptions::disabled_at.is_null())
        .load::<PushSubscription>(&mut conn)
    {
        Ok(subs) => subs,
        Err(e) => {
            error!("{PUSH_DISPATCH_FAILED}: loading subscriptions for {user_id} failed: {e}");
            return;
        }
    };

    if subs.is_empty() {
        debug!("push: user {user_id} has no active subscriptions for session {session_id}");
        return;
    }

    for sub in &subs {
        match transport.send(sub, &payload).await {
            Ok(SendOutcome::Delivered) => {
                if let Err(e) = record_success(&mut conn, sub.id) {
                    error!(
                        "{PUSH_DISPATCH_FAILED}: recording success for {} failed: {e}",
                        sub.id
                    );
                }
            }
            Ok(SendOutcome::GoneDeadEndpoint) => {
                debug!("push endpoint {} gone; disabling subscription", sub.id);
                if let Err(e) = disable_subscription(&mut conn, sub.id) {
                    error!(
                        "{PUSH_DISPATCH_FAILED}: disabling dead endpoint {} failed: {e}",
                        sub.id
                    );
                }
            }
            Err(e) => {
                error!("{PUSH_DISPATCH_FAILED}: delivery to {} failed: {e}", sub.id);
            }
        }
    }
}

/// Resolve `(owning user, session name, notification prefs)` for a session in
/// one round trip, or `None` when the session row is gone. Prefs semantics
/// match the C6 endpoints: a NULL `users.notification_prefs` column or a
/// stored value that no longer parses both fall back to
/// [`NotificationPrefs::default`].
fn resolve_recipient(
    conn: &mut DbConnection,
    session_id: Uuid,
) -> QueryResult<Option<(Uuid, String, NotificationPrefs)>> {
    use crate::schema::{sessions, users};
    let row = sessions::table
        .inner_join(users::table.on(users::id.eq(sessions::user_id)))
        .filter(sessions::id.eq(session_id))
        .select((
            sessions::user_id,
            sessions::session_name,
            users::notification_prefs,
        ))
        .first::<(Uuid, String, Option<serde_json::Value>)>(conn)
        .optional()?;
    Ok(row.map(|(user_id, name, stored)| {
        let prefs = stored
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        (user_id, name, prefs)
    }))
}

/// Stamp `last_success_at = now()` after a delivered push.
pub(crate) fn record_success(conn: &mut DbConnection, sub_id: Uuid) -> QueryResult<usize> {
    use crate::schema::push_subscriptions;
    diesel::update(push_subscriptions::table.find(sub_id))
        .set(push_subscriptions::last_success_at.eq(chrono::Utc::now()))
        .execute(conn)
}

/// Mark a subscription's endpoint dead (`disabled_at = now()`) so future
/// dispatches skip it until a re-registration clears the timestamp.
pub(crate) fn disable_subscription(conn: &mut DbConnection, sub_id: Uuid) -> QueryResult<usize> {
    use crate::schema::push_subscriptions;
    diesel::update(push_subscriptions::table.find(sub_id))
        .set(push_subscriptions::disabled_at.eq(chrono::Utc::now()))
        .execute(conn)
}

// DB-touching tests for the dead-endpoint / success bookkeeping. Auto-skip when
// `DATABASE_URL` is unset (so CI without a DB stays green), mirroring the
// pattern in `handlers::messages::db_tests`. Run locally via:
//   DATABASE_URL=postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal \
//     cargo test -p backend push::dispatcher::db_tests
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::models::{NewPushSubscription, NewUser, PushSubscription, User};

    fn make_user(conn: &mut DbConnection) -> User {
        use crate::schema::users;
        let nonce = Uuid::new_v4();
        let new_user = NewUser {
            google_id: format!("test_push_{nonce}"),
            email: format!("test_push_{nonce}@example.invalid"),
            name: Some("Push Test".to_string()),
            avatar_url: None,
        };
        diesel::insert_into(users::table)
            .values(&new_user)
            .get_result::<User>(conn)
            .expect("insert test user")
    }

    fn make_sub(conn: &mut DbConnection, user_id: Uuid) -> PushSubscription {
        use crate::schema::push_subscriptions;
        let new_sub = NewPushSubscription {
            user_id,
            platform: "webpush".to_string(),
            endpoint_or_token: format!("https://example.invalid/{}", Uuid::new_v4()),
            p256dh: Some("p256dh".to_string()),
            auth: Some("auth".to_string()),
            device_label: Some("test".to_string()),
        };
        diesel::insert_into(push_subscriptions::table)
            .values(&new_sub)
            .get_result::<PushSubscription>(conn)
            .expect("insert test subscription")
    }

    #[test]
    fn dead_endpoint_sets_disabled_at() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("db conn");
        let user = make_user(&mut conn);
        let sub = make_sub(&mut conn, user.id);
        assert!(sub.disabled_at.is_none());

        let n = disable_subscription(&mut conn, sub.id).expect("disable");
        assert_eq!(n, 1);

        use crate::schema::push_subscriptions;
        let reloaded: PushSubscription = push_subscriptions::table
            .find(sub.id)
            .first(&mut conn)
            .expect("reload");
        assert!(reloaded.disabled_at.is_some(), "disabled_at should be set");

        // A disabled endpoint drops out of the dispatch fan-out query.
        let active: Vec<PushSubscription> = push_subscriptions::table
            .filter(push_subscriptions::user_id.eq(user.id))
            .filter(push_subscriptions::disabled_at.is_null())
            .load(&mut conn)
            .expect("load active");
        assert!(active.iter().all(|s| s.id != sub.id));
    }

    #[test]
    fn record_success_sets_last_success_at() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("db conn");
        let user = make_user(&mut conn);
        let sub = make_sub(&mut conn, user.id);
        assert!(sub.last_success_at.is_none());

        let n = record_success(&mut conn, sub.id).expect("record success");
        assert_eq!(n, 1);

        use crate::schema::push_subscriptions;
        let reloaded: PushSubscription = push_subscriptions::table
            .find(sub.id)
            .first(&mut conn)
            .expect("reload");
        assert!(reloaded.last_success_at.is_some());
    }

    #[test]
    fn resolve_recipient_finds_owner_and_name() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("db conn");
        let user = make_user(&mut conn);

        use crate::models::NewSessionWithId;
        use crate::schema::sessions;
        let session_id = Uuid::new_v4();
        let new_session = NewSessionWithId {
            id: session_id,
            user_id: user.id,
            session_name: "resolve-test".to_string(),
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
            launcher_version: None,
        };
        diesel::insert_into(sessions::table)
            .values(&new_session)
            .execute(&mut conn)
            .expect("insert session");

        let resolved = resolve_recipient(&mut conn, session_id).expect("resolve");
        // A fresh user has NULL notification_prefs -> defaults (C6 semantics).
        assert_eq!(
            resolved,
            Some((
                user.id,
                "resolve-test".to_string(),
                NotificationPrefs::default()
            ))
        );

        // Unknown session resolves to None.
        assert_eq!(
            resolve_recipient(&mut conn, Uuid::new_v4()).expect("resolve missing"),
            None
        );
    }
}
