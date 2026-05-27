//! Turn-metrics REST handlers.
//!
//! Three endpoints live here:
//!
//! - `GET /api/sessions/{id}/turn-metrics` (PR 2): per-session list for the
//!   `SessionView` per-turn footer hydration on cold start. Reuses the same
//!   `session_members` ACL gate as `GET /api/sessions/{id}/messages`.
//! - `GET /api/metrics/recent` (PR 3): the calling user's most recent N turns
//!   across all sessions they own, used to seed the dashboard-header
//!   sparkline pill. Joins through `sessions.user_id` (owner-only — the v1
//!   pill only summarizes the dashboard user's own sessions; multi-tenant
//!   member-shared views can land in a later PR alongside multi-pill UI).
//! - `GET /api/metrics/turns?bucket=…&window=…` (PR 4): aggregated rollups for
//!   the Settings → Performance drill-in page. Bucketed by `date_trunc` (hour
//!   or day), grouped by `(agent_type, model, service_tier)`, with p50/p95
//!   latency + throughput computed via `percentile_cont` server-side. Same
//!   owner-only gate as `/api/metrics/recent`.

use crate::auth::extract_user_id;
use crate::errors::AppError;
use crate::models::TurnMetric;
use crate::AppState;
use axum::{
    extract::{Path, Query, State},
    Json,
};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Double, Nullable, Text, Timestamptz};
use serde::Deserialize;
use shared::api::{MetricBucket, MetricBucketsResponse, TurnMetricsResponse};
use shared::TurnMetrics;
use std::collections::BTreeMap;
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

// =============================================================================
// `GET /api/metrics/turns` — aggregated bucketed rollups for the Settings →
// Performance drill-in page (PR 4). Bucketed by `date_trunc('hour' | 'day',
// started_at)` and grouped by `(agent_type, model, service_tier)`; p50/p95
// latency and throughput are computed server-side via Postgres
// `percentile_cont(...)`. Stop-reason histogram is built in Rust from a
// second `GROUP BY (..., stop_reason, is_error)` pass so the SQL stays
// readable and the aggregation function pool stays small.
// =============================================================================

/// Bucketing granularity for the aggregated endpoint. The wire form is a tiny
/// allowlist (`"hour" | "day"`) so an attacker can't smuggle arbitrary SQL into
/// the `date_trunc` argument — only these two values reach the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BucketKind {
    Hour,
    Day,
}

impl BucketKind {
    /// String that flows into the `date_trunc($1, ...)` bind parameter. Safe
    /// because the value is one of two constants — there is no path from
    /// untrusted input to this `&'static str`.
    fn as_pg(self) -> &'static str {
        match self {
            BucketKind::Hour => "hour",
            BucketKind::Day => "day",
        }
    }
}

/// Query-string for `GET /api/metrics/turns`. Both fields optional; the
/// handler defaults `bucket=day` and `window=30d`. Validation rejects unknown
/// values with `400 Bad Request`.
#[derive(Debug, Deserialize)]
pub struct TurnMetricsAggregateQuery {
    #[serde(default)]
    pub bucket: Option<String>,
    #[serde(default)]
    pub window: Option<String>,
}

/// Parse `bucket=hour|day`. Empty / missing → `Day`. Unknown → `Err`.
fn parse_bucket(raw: Option<&str>) -> Result<BucketKind, AppError> {
    let trimmed = raw.unwrap_or("").trim();
    if trimmed.is_empty() {
        return Ok(BucketKind::Day);
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "hour" | "h" => Ok(BucketKind::Hour),
        "day" | "d" => Ok(BucketKind::Day),
        _ => Err(AppError::BadRequest("bucket must be 'hour' or 'day'")),
    }
}

/// Parse `window=Nh` / `window=Nd`. Empty / missing → `30 days`. Unknown
/// suffix or zero/negative → `Err`. Returns an INTERVAL string suitable for
/// `NOW() - $2::interval` (a typed bind, so still safe).
fn parse_window_to_interval(raw: Option<&str>) -> Result<String, AppError> {
    let s = raw.unwrap_or("30d").trim();
    if s.is_empty() {
        return Ok("30 days".to_string());
    }
    // Last character is the unit, the prefix is a positive integer count.
    let (count_str, unit) = match s.chars().last() {
        Some(c) => (&s[..s.len() - c.len_utf8()], c),
        None => return Err(AppError::BadRequest("window must be like '7d' or '48h'")),
    };
    let count: u32 = count_str
        .parse()
        .map_err(|_| AppError::BadRequest("window count must be a positive integer"))?;
    if count == 0 {
        return Err(AppError::BadRequest("window must be > 0"));
    }
    match unit.to_ascii_lowercase() {
        'd' => Ok(format!("{count} days")),
        'h' => Ok(format!("{count} hours")),
        _ => Err(AppError::BadRequest(
            "window unit must be 'd' (days) or 'h' (hours)",
        )),
    }
}

/// Aggregated bucket row produced by the main rollup query. One row per
/// `(bucket_start, agent_type, model, service_tier)` tuple.
#[derive(QueryableByName, Debug)]
struct AggregateRow {
    #[diesel(sql_type = Timestamptz)]
    bucket_start: chrono::DateTime<chrono::Utc>,
    #[diesel(sql_type = Text)]
    agent_type: String,
    #[diesel(sql_type = Nullable<Text>)]
    model: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    service_tier: Option<String>,
    #[diesel(sql_type = BigInt)]
    turn_count: i64,
    #[diesel(sql_type = BigInt)]
    error_count: i64,
    #[diesel(sql_type = Nullable<BigInt>)]
    ttft_p50_ms: Option<i64>,
    #[diesel(sql_type = Nullable<BigInt>)]
    ttft_p95_ms: Option<i64>,
    #[diesel(sql_type = Nullable<Double>)]
    throughput_p50_tps: Option<f64>,
    #[diesel(sql_type = Nullable<Double>)]
    throughput_p95_tps: Option<f64>,
    #[diesel(sql_type = BigInt)]
    input_tokens_sum: i64,
    #[diesel(sql_type = BigInt)]
    output_tokens_sum: i64,
    #[diesel(sql_type = BigInt)]
    cache_read_tokens_sum: i64,
    #[diesel(sql_type = BigInt)]
    cache_creation_tokens_sum: i64,
    #[diesel(sql_type = Nullable<Double>)]
    total_cost_usd_sum: Option<f64>,
}

/// Stop-reason mix row. Folded into the main rollup in Rust by
/// `(bucket_start, agent_type, model, service_tier)`.
#[derive(QueryableByName, Debug)]
struct StopReasonRow {
    #[diesel(sql_type = Timestamptz)]
    bucket_start: chrono::DateTime<chrono::Utc>,
    #[diesel(sql_type = Text)]
    agent_type: String,
    #[diesel(sql_type = Nullable<Text>)]
    model: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    service_tier: Option<String>,
    /// Folded reason key — `is_error = true` collapses every row's stop
    /// reason into `"error"`; `is_error = false && stop_reason IS NULL` folds
    /// to `"unknown"`; everything else carries the raw `stop_reason`.
    #[diesel(sql_type = Text)]
    reason_key: String,
    #[diesel(sql_type = BigInt)]
    count: i64,
}

/// `GET /api/metrics/turns?bucket=…&window=…` — Settings → Performance backend.
///
/// Owner-only gate (same as `/api/metrics/recent`): joins `turn_metrics` to
/// `sessions` and filters `sessions.user_id = current_user_id`. Bucket and
/// window are validated against a small allowlist before any SQL runs; only
/// the validated constants (`"hour" | "day"`, `"N days" | "N hours"` strings)
/// reach the query.
pub async fn list_aggregated_turn_metrics(
    State(app_state): State<Arc<AppState>>,
    cookies: Cookies,
    Query(query): Query<TurnMetricsAggregateQuery>,
) -> Result<Json<MetricBucketsResponse>, AppError> {
    let current_user_id = extract_user_id(&app_state, &cookies)?;
    let mut conn = app_state.db_pool.get().map_err(|_| AppError::DbPool)?;

    let bucket = parse_bucket(query.bucket.as_deref())?;
    let interval = parse_window_to_interval(query.window.as_deref())?;

    // Throughput is computed inline as
    //   output_tokens / (generation_duration_ms / 1000.0)
    // when generation_duration_ms > 0 and output_tokens > 0; null otherwise so
    // those rows don't pollute the percentile. SUMs are cast to bigint/float8
    // explicitly so Diesel can decode them per CLAUDE.md's raw-SQL guidance.
    let aggregate_sql = "\
        SELECT \
            date_trunc($1, tm.started_at) AS bucket_start, \
            tm.agent_type AS agent_type, \
            tm.model AS model, \
            tm.service_tier AS service_tier, \
            COUNT(*)::bigint AS turn_count, \
            COUNT(*) FILTER (WHERE tm.is_error)::bigint AS error_count, \
            (percentile_cont(0.5) WITHIN GROUP (ORDER BY tm.ttft_ms))::bigint AS ttft_p50_ms, \
            (percentile_cont(0.95) WITHIN GROUP (ORDER BY tm.ttft_ms))::bigint AS ttft_p95_ms, \
            percentile_cont(0.5) WITHIN GROUP ( \
                ORDER BY CASE \
                    WHEN tm.generation_duration_ms > 0 AND tm.output_tokens > 0 \
                    THEN (tm.output_tokens::float8 / (tm.generation_duration_ms::float8 / 1000.0)) \
                    ELSE NULL \
                END \
            )::float8 AS throughput_p50_tps, \
            percentile_cont(0.95) WITHIN GROUP ( \
                ORDER BY CASE \
                    WHEN tm.generation_duration_ms > 0 AND tm.output_tokens > 0 \
                    THEN (tm.output_tokens::float8 / (tm.generation_duration_ms::float8 / 1000.0)) \
                    ELSE NULL \
                END \
            )::float8 AS throughput_p95_tps, \
            COALESCE(SUM(tm.input_tokens), 0)::bigint AS input_tokens_sum, \
            COALESCE(SUM(tm.output_tokens), 0)::bigint AS output_tokens_sum, \
            COALESCE(SUM(tm.cache_read_tokens), 0)::bigint AS cache_read_tokens_sum, \
            COALESCE(SUM(tm.cache_creation_tokens), 0)::bigint AS cache_creation_tokens_sum, \
            SUM(tm.total_cost_usd)::float8 AS total_cost_usd_sum \
        FROM turn_metrics tm \
        INNER JOIN sessions s ON s.id = tm.session_id \
        WHERE s.user_id = $2 \
          AND tm.started_at >= NOW() - $3::interval \
        GROUP BY bucket_start, tm.agent_type, tm.model, tm.service_tier \
        ORDER BY bucket_start ASC, tm.agent_type ASC, tm.model ASC, tm.service_tier ASC";

    let aggregate_rows: Vec<AggregateRow> = diesel::sql_query(aggregate_sql)
        .bind::<Text, _>(bucket.as_pg())
        .bind::<diesel::sql_types::Uuid, _>(current_user_id)
        .bind::<Text, _>(&interval)
        .load(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    // Stop-reason mix runs as a separate `GROUP BY` so the main query stays
    // one-row-per-(bucket, group). Folds error rows under the `"error"` key
    // and null-stop-reason non-error rows under `"unknown"` for a stable
    // histogram.
    let stop_reason_sql = "\
        SELECT \
            date_trunc($1, tm.started_at) AS bucket_start, \
            tm.agent_type AS agent_type, \
            tm.model AS model, \
            tm.service_tier AS service_tier, \
            CASE \
                WHEN tm.is_error THEN 'error' \
                WHEN tm.stop_reason IS NULL THEN 'unknown' \
                ELSE tm.stop_reason \
            END AS reason_key, \
            COUNT(*)::bigint AS count \
        FROM turn_metrics tm \
        INNER JOIN sessions s ON s.id = tm.session_id \
        WHERE s.user_id = $2 \
          AND tm.started_at >= NOW() - $3::interval \
        GROUP BY bucket_start, tm.agent_type, tm.model, tm.service_tier, reason_key";

    let reason_rows: Vec<StopReasonRow> = diesel::sql_query(stop_reason_sql)
        .bind::<Text, _>(bucket.as_pg())
        .bind::<diesel::sql_types::Uuid, _>(current_user_id)
        .bind::<Text, _>(&interval)
        .load(&mut conn)
        .map_err(|e| AppError::DbQuery(e.to_string()))?;

    // Fold the stop-reason rows by `(bucket_start, agent_type, model, tier)`
    // into a histogram keyed by `reason_key`.
    type GroupKey = (
        chrono::DateTime<chrono::Utc>,
        String,
        Option<String>,
        Option<String>,
    );
    let mut reason_index: std::collections::HashMap<GroupKey, BTreeMap<String, i64>> =
        std::collections::HashMap::new();
    for row in reason_rows {
        let key = (
            row.bucket_start,
            row.agent_type.clone(),
            row.model.clone(),
            row.service_tier.clone(),
        );
        reason_index
            .entry(key)
            .or_default()
            .insert(row.reason_key, row.count);
    }

    let buckets = aggregate_rows
        .into_iter()
        .map(|row| {
            let key = (
                row.bucket_start,
                row.agent_type.clone(),
                row.model.clone(),
                row.service_tier.clone(),
            );
            let stop_reason_counts = reason_index.remove(&key).unwrap_or_default();
            MetricBucket {
                bucket_start: row.bucket_start,
                agent_type: row.agent_type,
                model: row.model,
                service_tier: row.service_tier,
                turn_count: row.turn_count,
                error_count: row.error_count,
                ttft_p50_ms: row.ttft_p50_ms,
                ttft_p95_ms: row.ttft_p95_ms,
                throughput_p50_tps: row.throughput_p50_tps,
                throughput_p95_tps: row.throughput_p95_tps,
                input_tokens_sum: row.input_tokens_sum,
                output_tokens_sum: row.output_tokens_sum,
                cache_read_tokens_sum: row.cache_read_tokens_sum,
                cache_creation_tokens_sum: row.cache_creation_tokens_sum,
                total_cost_usd_sum: row.total_cost_usd_sum,
                stop_reason_counts,
            }
        })
        .collect();

    Ok(Json(MetricBucketsResponse { buckets }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bucket_defaults_to_day() {
        assert_eq!(parse_bucket(None).unwrap(), BucketKind::Day);
        assert_eq!(parse_bucket(Some("")).unwrap(), BucketKind::Day);
    }

    #[test]
    fn parse_bucket_accepts_hour_and_day_case_insensitive() {
        assert_eq!(parse_bucket(Some("hour")).unwrap(), BucketKind::Hour);
        assert_eq!(parse_bucket(Some("HOUR")).unwrap(), BucketKind::Hour);
        assert_eq!(parse_bucket(Some("day")).unwrap(), BucketKind::Day);
        // Single-letter aliases for brevity (`?bucket=h`).
        assert_eq!(parse_bucket(Some("h")).unwrap(), BucketKind::Hour);
        assert_eq!(parse_bucket(Some("d")).unwrap(), BucketKind::Day);
    }

    #[test]
    fn parse_bucket_rejects_unknown_values() {
        let err = parse_bucket(Some("week")).unwrap_err();
        match err {
            AppError::BadRequest(_) => {}
            other => panic!("expected BadRequest, got {other:?}"),
        }
        assert!(parse_bucket(Some("minute")).is_err());
        assert!(parse_bucket(Some("'; DROP TABLE turn_metrics; --")).is_err());
    }

    #[test]
    fn parse_window_defaults_to_30_days() {
        assert_eq!(parse_window_to_interval(None).unwrap(), "30 days");
        assert_eq!(parse_window_to_interval(Some("")).unwrap(), "30 days");
    }

    #[test]
    fn parse_window_accepts_d_and_h_suffix() {
        assert_eq!(parse_window_to_interval(Some("7d")).unwrap(), "7 days");
        assert_eq!(parse_window_to_interval(Some("30d")).unwrap(), "30 days");
        assert_eq!(parse_window_to_interval(Some("48h")).unwrap(), "48 hours");
        assert_eq!(parse_window_to_interval(Some("2h")).unwrap(), "2 hours");
        // Case-insensitive suffix.
        assert_eq!(parse_window_to_interval(Some("7D")).unwrap(), "7 days");
    }

    #[test]
    fn parse_window_rejects_bad_input() {
        assert!(parse_window_to_interval(Some("0d")).is_err());
        assert!(parse_window_to_interval(Some("abc")).is_err());
        assert!(parse_window_to_interval(Some("7w")).is_err()); // weeks unsupported
        assert!(parse_window_to_interval(Some("-1d")).is_err());
        assert!(parse_window_to_interval(Some("'; DROP --")).is_err());
    }

    #[test]
    fn metric_bucket_response_serde_roundtrip_via_value() {
        // Belt-and-suspenders: build a response, serialize via serde_json,
        // and parse back. Mirrors the shared::api roundtrip tests but exercises
        // the handler-side use of the type.
        use chrono::{TimeZone, Utc};
        let resp = MetricBucketsResponse {
            buckets: vec![MetricBucket {
                bucket_start: Utc.with_ymd_and_hms(2026, 5, 27, 0, 0, 0).unwrap(),
                agent_type: "claude".to_string(),
                model: Some("claude-opus-4-7".to_string()),
                service_tier: Some("standard".to_string()),
                turn_count: 12,
                error_count: 1,
                ttft_p50_ms: Some(420),
                ttft_p95_ms: Some(1200),
                throughput_p50_tps: Some(47.5),
                throughput_p95_tps: Some(65.0),
                input_tokens_sum: 50_000,
                output_tokens_sum: 12_345,
                cache_read_tokens_sum: 1_000,
                cache_creation_tokens_sum: 200,
                total_cost_usd_sum: Some(0.18),
                stop_reason_counts: {
                    let mut m = BTreeMap::new();
                    m.insert("end_turn".to_string(), 10);
                    m.insert("error".to_string(), 1);
                    m.insert("tool_use".to_string(), 1);
                    m
                },
            }],
        };
        let value = serde_json::to_value(&resp).unwrap();
        let parsed: MetricBucketsResponse = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.buckets.len(), 1);
        let bucket = &parsed.buckets[0];
        assert_eq!(bucket.turn_count, 12);
        assert_eq!(bucket.stop_reason_counts.get("end_turn").copied(), Some(10));
        assert_eq!(bucket.stop_reason_counts.get("error").copied(), Some(1));
    }
}
