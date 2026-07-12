//! Push-subscription CRUD handlers (mobile-apps plan C1).
//!
//! The caller's identity comes from [`CurrentUserId`], so every query is scoped
//! to `user_id` — a user can only see and delete their own subscriptions.
//! Registration upserts on the `(user_id, endpoint_or_token)` unique key so a
//! browser/device re-subscribing refreshes its keys and revives a previously
//! disabled endpoint rather than piling up duplicate rows.

use axum::{
    extract::{Path, State},
    Json,
};
use diesel::prelude::*;
use shared::api::{
    NotificationPrefs, PushSubscriptionInfo, PushSubscriptionsResponse,
    RegisterPushSubscriptionRequest, VapidKeyResponse,
};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

use crate::{
    auth::CurrentUserId,
    errors::AppError,
    handlers::responses::EmptyResponse,
    models::{NewPushSubscription, PushSubscription},
    schema::push_subscriptions,
    AppState,
};

/// Build the client-facing view of a stored subscription. The endpoint/token
/// and crypto keys stay server-side and are never echoed back.
fn to_info(row: PushSubscription) -> PushSubscriptionInfo {
    PushSubscriptionInfo {
        id: row.id,
        // Rows are only ever written from a typed `PushPlatform`, so an
        // unrecognized value would mean DB corruption; fall back to the raw
        // string round-tripped through webpush rather than dropping the row.
        platform: shared::api::PushPlatform::from_wire(&row.platform)
            .unwrap_or(shared::api::PushPlatform::Webpush),
        device_label: row.device_label,
        created_at: row.created_at.to_rfc3339(),
        last_success_at: row.last_success_at.map(|t| t.to_rfc3339()),
        disabled_at: row.disabled_at.map(|t| t.to_rfc3339()),
    }
}

/// GET /api/push/vapid-key — the server's VAPID application-server public key.
///
/// Returns 404 when the deployment has no key configured
/// (`PORTAL_VAPID_PUBLIC_KEY` unset); the frontend treats that as "push
/// unavailable" and degrades gracefully rather than erroring.
pub async fn get_vapid_key(
    State(app_state): State<Arc<AppState>>,
) -> Result<Json<VapidKeyResponse>, AppError> {
    match &app_state.vapid_public_key {
        Some(public_key) => Ok(Json(VapidKeyResponse {
            public_key: public_key.clone(),
        })),
        None => Err(AppError::NotFound("push not configured")),
    }
}

/// POST /api/push/subscriptions — register or refresh a push subscription.
///
/// Upserts on `(user_id, endpoint_or_token)`: a repeat registration refreshes
/// the platform/keys/label and clears `disabled_at`, reviving an endpoint that
/// an earlier dead-endpoint prune had disabled.
pub async fn register_subscription(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Json(req): Json<RegisterPushSubscriptionRequest>,
) -> Result<Json<PushSubscriptionInfo>, AppError> {
    let mut conn = app_state.conn()?;

    let new_row = NewPushSubscription {
        user_id,
        platform: req.platform.as_wire().to_string(),
        endpoint_or_token: req.endpoint_or_token,
        p256dh: req.p256dh,
        auth: req.auth,
        device_label: req.device_label,
    };

    let row: PushSubscription = diesel::insert_into(push_subscriptions::table)
        .values(&new_row)
        .on_conflict((
            push_subscriptions::user_id,
            push_subscriptions::endpoint_or_token,
        ))
        .do_update()
        .set((
            push_subscriptions::platform.eq(&new_row.platform),
            push_subscriptions::p256dh.eq(&new_row.p256dh),
            push_subscriptions::auth.eq(&new_row.auth),
            push_subscriptions::device_label.eq(&new_row.device_label),
            push_subscriptions::disabled_at.eq(None::<chrono::DateTime<chrono::Utc>>),
        ))
        .get_result(&mut conn)?;

    info!(
        "Registered push subscription {} ({}) for user {}",
        row.id, row.platform, user_id
    );

    Ok(Json(to_info(row)))
}

/// GET /api/push/subscriptions — list the caller's own subscriptions.
pub async fn list_subscriptions(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
) -> Result<Json<PushSubscriptionsResponse>, AppError> {
    let mut conn = app_state.conn()?;

    let rows: Vec<PushSubscription> = push_subscriptions::table
        .filter(push_subscriptions::user_id.eq(user_id))
        .order(push_subscriptions::created_at.desc())
        .load(&mut conn)?;

    Ok(Json(PushSubscriptionsResponse {
        subscriptions: rows.into_iter().map(to_info).collect(),
    }))
}

/// DELETE /api/push/subscriptions/{id} — remove one of the caller's own
/// subscriptions. Returns 404 if the row does not exist or belongs to someone
/// else (the `user_id` filter makes the two indistinguishable, by design).
pub async fn delete_subscription(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Path(subscription_id): Path<Uuid>,
) -> Result<EmptyResponse, AppError> {
    let mut conn = app_state.conn()?;

    let deleted = diesel::delete(
        push_subscriptions::table
            .filter(push_subscriptions::id.eq(subscription_id))
            .filter(push_subscriptions::user_id.eq(user_id)),
    )
    .execute(&mut conn)?;

    if deleted == 0 {
        return Err(AppError::NotFound("push subscription"));
    }

    info!(
        "Deleted push subscription {} for user {}",
        subscription_id, user_id
    );
    Ok(EmptyResponse::NO_CONTENT)
}

/// GET /api/push/prefs — the caller's own notification preferences.
///
/// The prefs live in the nullable `users.notification_prefs` jsonb column. A
/// NULL column (user never saved prefs) or a value that no longer parses into
/// the current [`NotificationPrefs`] shape both fall back to
/// [`NotificationPrefs::default`], so an older/partial stored payload never
/// wedges the endpoint.
pub async fn get_prefs(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
) -> Result<Json<NotificationPrefs>, AppError> {
    use crate::schema::users;
    let mut conn = app_state.conn()?;

    let stored: Option<serde_json::Value> = users::table
        .find(user_id)
        .select(users::notification_prefs)
        .first(&mut conn)?;

    let prefs = stored
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();

    Ok(Json(prefs))
}

/// PUT /api/push/prefs — replace the caller's notification preferences.
///
/// Stores the typed prefs as jsonb and echoes back the stored value.
pub async fn put_prefs(
    State(app_state): State<Arc<AppState>>,
    CurrentUserId(user_id): CurrentUserId,
    Json(prefs): Json<NotificationPrefs>,
) -> Result<Json<NotificationPrefs>, AppError> {
    use crate::schema::users;
    let mut conn = app_state.conn()?;

    // `NotificationPrefs` is a flat struct of bools, so serialization can only
    // fail on OOM — an `AppError::Internal` is the honest response.
    let value = serde_json::to_value(prefs).map_err(|e| AppError::Internal(e.to_string()))?;

    diesel::update(users::table.find(user_id))
        .set(users::notification_prefs.eq(Some(value)))
        .execute(&mut conn)?;

    Ok(Json(prefs))
}

// DB-touching round-trip test for the notification-prefs query path.
//
// Mirrors the harness in `messages.rs::db_tests` / `auth.rs`: auto-skips when
// `DATABASE_URL` is unset (CI stays green without a DB) and runs locally via:
//
//   DATABASE_URL=postgresql://claude_portal:dev_password_change_in_production@localhost:5432/claude_portal \
//     cargo test -p backend handlers::push::db_tests
//
// We exercise the query layer directly (read/update the jsonb column) rather
// than the Axum handler, which needs a full `AppState`; the handlers are thin
// wrappers over exactly this read/parse/write logic.
#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::models::{NewUser, User};
    fn make_user(conn: &mut diesel::pg::PgConnection) -> User {
        use crate::schema::users;
        let nonce = Uuid::new_v4();
        let new_user = NewUser {
            google_id: format!("test_push_prefs_{nonce}"),
            email: format!("test_push_prefs_{nonce}@example.invalid"),
            name: Some("Test Prefs".to_string()),
            avatar_url: None,
        };
        diesel::insert_into(users::table)
            .values(&new_user)
            .get_result::<User>(conn)
            .expect("insert test user")
    }

    /// Read `users.notification_prefs`, applying the same NULL/parse-failure ->
    /// default fallback as the `get_prefs` handler.
    fn read_prefs(conn: &mut diesel::pg::PgConnection, user_id: Uuid) -> NotificationPrefs {
        use crate::schema::users;
        let stored: Option<serde_json::Value> = users::table
            .find(user_id)
            .select(users::notification_prefs)
            .first(conn)
            .expect("select notification_prefs");
        stored
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    #[test]
    fn prefs_round_trip_default_then_custom() {
        let Some(pool) = crate::test_support::shared_pool() else {
            return;
        };
        let mut conn = pool.get().expect("get conn");
        let user = make_user(&mut conn);

        // GET on a fresh user: column is NULL -> defaults.
        assert_eq!(read_prefs(&mut conn, user.id), NotificationPrefs::default());

        // PUT a custom (non-default) value.
        let custom = NotificationPrefs {
            permission_request: false,
            turn_complete: false,
            session_disconnected: true,
            agent_message: true,
            content_detail: shared::api::NotificationContentDetail::Snippet,
        };
        assert_ne!(custom, NotificationPrefs::default());
        {
            use crate::schema::users;
            let value = serde_json::to_value(custom).unwrap();
            diesel::update(users::table.find(user.id))
                .set(users::notification_prefs.eq(Some(value)))
                .execute(&mut conn)
                .expect("update prefs");
        }

        // GET again: the custom value round-trips.
        assert_eq!(read_prefs(&mut conn, user.id), custom);

        // Cleanup.
        use crate::schema::users;
        let _ = diesel::delete(users::table.find(user.id)).execute(&mut conn);
    }
}
