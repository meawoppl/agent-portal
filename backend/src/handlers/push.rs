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
    PushSubscriptionInfo, PushSubscriptionsResponse, RegisterPushSubscriptionRequest,
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
