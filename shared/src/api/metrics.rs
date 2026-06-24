//! Per-model usage and per-turn performance metrics types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Per-model usage / cost breakdown carried by Claude's `ResultMessage.modelUsage`
/// field. Keyed by model name (e.g. `"claude-opus-4-7[1m]"`) in the parent map.
///
/// Wire shape from claude-codes' own `test_result_with_new_fields`:
/// ```json
/// "modelUsage": {
///     "claude-opus-4-7[1m]": {
///         "inputTokens": 3817,
///         "outputTokens": 14,
///         "costUSD": 0.06
///     }
/// }
/// ```
///
/// TODO(SDK #140): `claude-codes::ResultMessage.model_usage` is currently
/// `Option<serde_json::Value>` upstream. This local typed mirror exists so the
/// frontend can iterate the per-model breakdown without poking JSON field
/// names. When the SDK adopts a typed `BTreeMap<String, ModelUsageEntry>`
/// itself, callers can `serde_json::from_value::<ModelUsage>(value)` directly
/// against the upstream type instead, and this struct can be deleted.
///
/// All fields default so partial / older frames still parse.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsageEntry {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    /// Wire name is `costUSD`, not `costUsd`.
    #[serde(default, rename = "costUSD")]
    pub cost_usd: f64,
    #[serde(default)]
    pub web_search_requests: u32,
}

/// Convenience alias for the full `modelUsage` map. The map key is the model
/// name string as emitted by claude (e.g. `"claude-opus-4-7[1m]"`).
pub type ModelUsage = BTreeMap<String, ModelUsageEntry>;

/// Per-turn performance metrics captured by the proxy and persisted by the
/// backend. One row per user-input → terminator (`ClaudeOutput::Result` for
/// Claude, `CodexEvent::TurnCompleted` / `TurnFailed` for Codex).
///
/// Shared on the wire in two places:
///   - proxy → backend: `ProxyToServer::TurnMetricsReport(TurnMetrics)`
///   - backend → frontend: `ServerToClient::TurnMetrics(TurnMetrics)`
///
/// Frontend rendering ships in a follow-up PR; this type is the foundation
/// the capture pipeline writes to (and the broadcast pipeline reads from).
///
/// Field shapes mirror the `turn_metrics` DB columns:
///   - timestamps are `chrono::DateTime<Utc>` (`Option<_>` for the post-start
///     ones because an error before any content gives `None`)
///   - all token counters and derived ms durations are `i64`
///   - tool/restart counters are `i32`
///   - `total_cost_usd` is `Option<f64>` because Codex does not surface cost
///   - `id` and `user_message_id` are server-side (assigned at insert) and
///     therefore optional on the proxy-emit side; populated by the backend
///     before broadcast.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnMetrics {
    /// DB row id. None on the proxy-emit side; populated by the backend
    /// after insert and present on the broadcast frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,

    pub session_id: Uuid,

    /// Optional foreign key into `messages` for the user prompt that opened
    /// this turn. The proxy doesn't know the backend's `messages.id`, so
    /// this stays `None` on the proxy-emit side until the backend wires up
    /// per-turn linkage in a future PR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_message_id: Option<Uuid>,

    pub agent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,

    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_inter_token_gap_ms: Option<i64>,

    #[serde(default)]
    pub input_tokens: i64,
    #[serde(default)]
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_creation_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: i64,
    #[serde(default)]
    pub thinking_tokens: i64,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub tool_call_count: i32,
    #[serde(default)]
    pub stream_restarts: i32,

    /// Cost in USD. Claude provides this on `Result.total_cost_usd`; Codex
    /// does not surface cost on its wire today, so for codex turns this stays
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
}

impl TurnMetrics {
    /// True when `model` is usable telemetry: present, non-blank, and not the
    /// literal placeholder `"unknown"`. All three ingest points (Claude
    /// proxy, Codex proxy, backend persist) warn-and-drop turn metrics that
    /// fail this check, so the rule must stay identical everywhere.
    pub fn has_known_model(&self) -> bool {
        self.model.as_deref().is_some_and(|value| {
            let value = value.trim();
            !value.is_empty() && !value.eq_ignore_ascii_case("unknown")
        })
    }
}

/// Response from `GET /api/sessions/{id}/turn-metrics`.
///
/// Rows are ordered `started_at ASC` so the SessionView's join walk (pair Nth
/// terminator message with Nth metrics row) works without a second sort.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnMetricsResponse {
    #[serde(default)]
    pub metrics: Vec<TurnMetrics>,
}

/// One bucket in the `GET /api/metrics/turns` response. Aggregates `turn_metrics`
/// rows over the time slice keyed by `bucket_start`, grouped by
/// `(agent_type, model, service_tier)`. Percentiles and throughput are computed
/// server-side via Postgres `percentile_cont(...)` so the frontend gets ready-
/// to-plot scalars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetricBucket {
    /// Bucket start timestamp (UTC). `date_trunc('hour' | 'day', started_at)`.
    pub bucket_start: DateTime<Utc>,
    pub agent_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    // Counts
    pub turn_count: i64,
    pub error_count: i64,
    // Latency aggregates (millis)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_p50_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_p95_ms: Option<i64>,
    /// Throughput in output tokens per second (computed server-side).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_p50_tps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_p95_tps: Option<f64>,
    // Tokens
    pub input_tokens_sum: i64,
    pub output_tokens_sum: i64,
    pub cache_read_tokens_sum: i64,
    pub cache_creation_tokens_sum: i64,
    // Cost
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cost_usd_sum: Option<f64>,
    /// Stop-reason mix for this bucket — keyed by the raw `stop_reason` string
    /// (`end_turn`, `max_tokens`, `tool_use`, …). Rows with `is_error = true`
    /// fold into the `"error"` key regardless of their `stop_reason` value so
    /// the stacked-area chart's red band reads as "errors" not as a particular
    /// reason. Rows with `stop_reason = NULL && is_error = false` fold into
    /// `"unknown"`.
    #[serde(default)]
    pub stop_reason_counts: BTreeMap<String, i64>,
}

/// Response shape for `GET /api/metrics/turns?bucket=…&window=…`.
///
/// Buckets are ordered `(bucket_start ASC, agent_type ASC, model ASC, tier ASC)`
/// so the frontend can stream-render a stacked area / multi-line chart without
/// a second sort. The frontend `(model, service_tier)` drop-down filters
/// client-side.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricBucketsResponse {
    #[serde(default)]
    pub buckets: Vec<MetricBucket>,
}
