//! Turn-metrics REST handlers.
//!
//! Two endpoints live here:
//!
//! - `GET /api/sessions/{id}/turn-metrics` (PR 2): per-session list for the
//!   `SessionView` per-turn footer hydration on cold start. Reuses the same
//!   `session_members` ACL gate as `GET /api/sessions/{id}/messages`.
//! - `GET /api/metrics/recent` (PR 3): the calling user's most recent N turns
//!   across all sessions they own, used to seed the dashboard-header
//!   sparkline pill. Joins through `sessions.user_id` (owner-only — the v1
//!   pill only summarizes the dashboard user's own sessions; multi-tenant
//!   member-shared views can land in a later PR alongside multi-pill UI).

use crate::auth::extract_user_id;
use crate::errors::AppError;
use crate::models::TurnMetric;
use crate::AppState;
use axum::{
    extract::{Path, State},
    Json,
};
use diesel::prelude::*;
use shared::api::TurnMetricsResponse;
use shared::TurnMetrics;
use std::sync::Arc;
use tower_cookies::Cookies;

/// Verify that the caller is a member of the session. Reuses the same
/// `session_members` join the messages handler uses — read access is
/// "any role, including viewer," matching the metrics' visibility model
/// (no mutation possible from this endpoint).
fn verify_session_access(
    conn: &mut diesel::pg::PgConnection,
    session_id: uuid::Uuid,
    user_id: uuid::Uuid,
) -> Result<(), AppError> {
    use crate::schema::{session_members, sessions};
    let exists = sessions::table
        .inner_join(session_members::table.on(session_members::session_id.eq(sessions::id)))
        .filter(sessions::id.eq(session_id))
        .filter(session_members::user_id.eq(user_id))
        .select(sessions::id)
        .first::<uuid::Uuid>(conn)
        .optional()
        .map_err(|e| AppError::DbQuery(e.to_string()))?;
    if exists.is_none() {
        return Err(AppError::NotFound("Session not found"));
    }
    Ok(())
}

/// Map a DB `TurnMetric` row into the wire-facing `shared::TurnMetrics`
/// shape. Field-by-field rather than `From` impl so the two structs stay
/// explicitly synchronized without one silently picking up a stray field
/// from the other.
fn row_to_wire(row: TurnMetric) -> TurnMetrics {
    TurnMetrics {
        id: Some(row.id),
        session_id: row.session_id,
        user_message_id: row.user_message_id,
        agent_type: row.agent_type,
        model: row.model,
        service_tier: row.service_tier,
        started_at: row.started_at,
        first_token_at: row.first_token_at,
        completed_at: row.completed_at,
        ttft_ms: row.ttft_ms,
        total_duration_ms: row.total_duration_ms,
        generation_duration_ms: row.generation_duration_ms,
        max_inter_token_gap_ms: row.max_inter_token_gap_ms,
        input_tokens: row.input_tokens,
        output_tokens: row.output_tokens,
        cache_creation_tokens: row.cache_creation_tokens,
        cache_read_tokens: row.cache_read_tokens,
        thinking_tokens: row.thinking_tokens,
        stop_reason: row.stop_reason,
        is_error: row.is_error,
        tool_call_count: row.tool_call_count,
        stream_restarts: row.stream_restarts,
        total_cost_usd: row.total_cost_usd,
    }
}

/// `GET /api/sessions/{id}/turn-metrics` — returns all per-turn metrics rows
/// for the session, ordered by `started_at ASC` so the SessionView's
/// pair-by-ordering join walks correctly without a second sort. No
/// pagination today: per-turn rows are O(turns), which is tiny next to the
/// message stream — if a session ever accumulates enough turns to make this
/// feel slow, we'll add `?before` / `?after` cursors mirroring the messages
/// handler.
pub async fn list_turn_metrics(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Path(session_id): Path<uuid::Uuid>,
) -> Result<Json<TurnMetricsResponse>, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;
    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    verify_session_access(&mut conn, session_id, current_user_id)?;

    use crate::schema::turn_metrics;
    let rows: Vec<TurnMetric> = turn_metrics::table
        .filter(turn_metrics::session_id.eq(session_id))
        .order(turn_metrics::started_at.asc())
        .select(TurnMetric::as_select())
        .load(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    let metrics = rows.into_iter().map(row_to_wire).collect();
    Ok(Json(TurnMetricsResponse { metrics }))
}

/// Window size for `GET /api/metrics/recent`. Picked to comfortably cover a
/// day of moderate use (a handful of sessions, ~10 turns each) while staying
/// small enough that the dashboard pill can plot every point as a sub-pixel
/// of an 80px-wide sparkline without subsampling.
const RECENT_TURN_LIMIT: i64 = 50;

/// `GET /api/metrics/recent` — returns the calling user's most recent
/// `RECENT_TURN_LIMIT` turns across all sessions they own, ordered
/// `started_at ASC` so the client can spark-plot left→right oldest→newest
/// without a second sort. Joins `turn_metrics` to `sessions` on
/// `session_id` and filters by `sessions.user_id = current_user_id`.
///
/// We deliberately take the SQL's "newest N" (ORDER BY started_at DESC LIMIT
/// N) and then reverse the result in Rust rather than ordering ASC in SQL:
/// ordering ASC would force a full-table scan for a user with thousands of
/// turns; DESC + LIMIT uses the `started_at DESC` index from PR 1's
/// migration directly. The reverse is O(50) and a non-event.
pub async fn list_recent_user_turn_metrics(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
) -> Result<Json<TurnMetricsResponse>, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;
    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    use crate::schema::{sessions, turn_metrics};
    let mut rows: Vec<TurnMetric> = turn_metrics::table
        .inner_join(sessions::table.on(sessions::id.eq(turn_metrics::session_id)))
        .filter(sessions::user_id.eq(current_user_id))
        .order(turn_metrics::started_at.desc())
        .limit(RECENT_TURN_LIMIT)
        .select(TurnMetric::as_select())
        .load(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    // SQL gave us newest-first; flip to oldest-first so the sparkline reads
    // left→right oldest→newest without a second pass on the frontend.
    rows.reverse();

    let metrics = rows.into_iter().map(row_to_wire).collect();
    Ok(Json(TurnMetricsResponse { metrics }))
}
