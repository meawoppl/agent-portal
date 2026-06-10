//! Per-turn metrics persistence + broadcast (PR 1 of N).
//!
//! Handles `ProxyToServer::TurnMetricsReport` frames: insert into
//! `turn_metrics`, then broadcast the saved row to web clients connected to
//! the same session as `ServerToClient::TurnMetrics`. Frontend rendering
//! ships in a follow-up PR; the broadcast is wired now so the protocol is
//! in place and no second migration / wire change is needed when the UI
//! lands.

use diesel::prelude::*;
use shared::{ServerToClient, TurnMetrics};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::{SessionId, SessionManager};
use crate::db::DbPool;
use crate::models::{NewTurnMetric, TurnMetric};

/// Persist a `TurnMetrics` report into the DB, then broadcast the saved row
/// to web clients on the matching session.
///
/// `db_session_id` is the resolved sessions row id for this proxy
/// connection (set by registration); `metrics.session_id` should match,
/// but we trust the connection-bound id over the wire payload to keep a
/// misbehaving / malicious proxy from writing rows for other sessions.
pub fn handle_turn_metrics_report(
    session_manager: &SessionManager,
    session_key: &Option<SessionId>,
    db_session_id: Option<Uuid>,
    db_pool: &DbPool,
    mut metrics: TurnMetrics,
) {
    let Some(session_id) = db_session_id else {
        // No session bound to this proxy connection — nothing to do.
        return;
    };
    metrics.session_id = session_id;

    if !metrics.has_known_model() {
        warn!(
            "Dropping turn metrics for session {} with unknown model (agent={})",
            session_id, metrics.agent_type
        );
        return;
    }

    let mut conn = match db_pool.get() {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to get DB connection for turn metrics insert: {}", e);
            return;
        }
    };

    // Resolve the session's owner so the metric carries its own ownership.
    // `turn_metrics.user_id` is what the Performance queries filter on, which
    // lets a row outlive its session (`session_id` is `ON DELETE SET NULL`)
    // and still be attributed to the right user.
    let owner_id: Uuid = {
        use crate::schema::sessions;
        match sessions::table
            .find(session_id)
            .select(sessions::user_id)
            .first::<Uuid>(&mut conn)
        {
            Ok(uid) => uid,
            Err(e) => {
                error!(
                    "Failed to resolve owner for session {} (turn metrics insert): {}",
                    session_id, e
                );
                return;
            }
        }
    };

    let new_row = NewTurnMetric {
        session_id,
        user_id: owner_id,
        user_message_id: metrics.user_message_id,
        agent_type: metrics.agent_type.clone(),
        model: metrics.model.clone(),
        service_tier: metrics.service_tier.clone(),
        started_at: metrics.started_at,
        first_token_at: metrics.first_token_at,
        completed_at: metrics.completed_at,
        ttft_ms: metrics.ttft_ms,
        total_duration_ms: metrics.total_duration_ms,
        generation_duration_ms: metrics.generation_duration_ms,
        max_inter_token_gap_ms: metrics.max_inter_token_gap_ms,
        input_tokens: metrics.input_tokens,
        output_tokens: metrics.output_tokens,
        cache_creation_tokens: metrics.cache_creation_tokens,
        cache_read_tokens: metrics.cache_read_tokens,
        thinking_tokens: metrics.thinking_tokens,
        stop_reason: metrics.stop_reason.clone(),
        is_error: metrics.is_error,
        tool_call_count: metrics.tool_call_count,
        stream_restarts: metrics.stream_restarts,
        total_cost_usd: metrics.total_cost_usd,
    };

    use crate::schema::turn_metrics;
    let inserted: TurnMetric = match diesel::insert_into(turn_metrics::table)
        .values(&new_row)
        .get_result(&mut conn)
    {
        Ok(row) => row,
        Err(e) => {
            error!("Failed to insert turn metrics row: {}", e);
            return;
        }
    };

    info!(
        "Persisted turn_metrics row {} for session {} (agent={}, ttft_ms={:?}, total_ms={:?})",
        inserted.id, session_id, inserted.agent_type, inserted.ttft_ms, inserted.total_duration_ms
    );

    // Mirror the saved row back onto the wire-facing shape so the broadcast
    // carries the server-assigned `id`. Field-by-field rather than `..` so
    // the `TurnMetrics` shape stays explicitly synchronized with the DB row.
    let payload = TurnMetrics {
        id: Some(inserted.id),
        // Always the just-inserted session id (the DB column is now nullable —
        // for orphaned-from-session rows — but a freshly inserted row always
        // has one). Use the resolved local rather than unwrapping the Option.
        session_id,
        user_message_id: inserted.user_message_id,
        agent_type: inserted.agent_type,
        model: inserted.model,
        service_tier: inserted.service_tier,
        started_at: inserted.started_at,
        first_token_at: inserted.first_token_at,
        completed_at: inserted.completed_at,
        ttft_ms: inserted.ttft_ms,
        total_duration_ms: inserted.total_duration_ms,
        generation_duration_ms: inserted.generation_duration_ms,
        max_inter_token_gap_ms: inserted.max_inter_token_gap_ms,
        input_tokens: inserted.input_tokens,
        output_tokens: inserted.output_tokens,
        cache_creation_tokens: inserted.cache_creation_tokens,
        cache_read_tokens: inserted.cache_read_tokens,
        thinking_tokens: inserted.thinking_tokens,
        stop_reason: inserted.stop_reason,
        is_error: inserted.is_error,
        tool_call_count: inserted.tool_call_count,
        stream_restarts: inserted.stream_restarts,
        total_cost_usd: inserted.total_cost_usd,
    };

    // Per-session broadcast: feeds the `SessionView` per-turn footer (PR 2).
    // Reaches web clients that explicitly opened the session view.
    if let Some(key) = session_key {
        session_manager
            .broadcast_to_web_clients(key, ServerToClient::TurnMetrics(Box::new(payload.clone())));
    }
    // Per-user broadcast: feeds the dashboard-header sparkline pill (PR 3).
    // Look up every member of this session and forward the same frame to
    // their user-level `/ws/client` connections. Dashboards stay on the
    // user channel (not a session channel) so this is the only path that
    // reaches them live; without it the pill only refreshes on REST
    // hydration (mount / reload).
    let member_ids: Vec<Uuid> = {
        use crate::schema::session_members;
        match session_members::table
            .filter(session_members::session_id.eq(session_id))
            .select(session_members::user_id)
            .load::<Uuid>(&mut conn)
        {
            Ok(ids) => ids,
            Err(e) => {
                error!(
                    "Failed to load session members for turn-metrics fanout (session {}): {}",
                    session_id, e
                );
                Vec::new()
            }
        }
    };
    for user_id in member_ids {
        session_manager.broadcast_to_user(
            &user_id,
            ServerToClient::TurnMetrics(Box::new(payload.clone())),
        );
    }
}

// No unit tests here — the persist/broadcast round-trip needs a real
// Postgres connection (no in-process Diesel sqlite fallback is set up in
// this repo). The `TurnTracker` finalize path is exercised via
// `session_lib::turn_tracker` unit tests; the wire shape is exercised via
// `shared::api::tests::turn_metrics_*_roundtrip`.
