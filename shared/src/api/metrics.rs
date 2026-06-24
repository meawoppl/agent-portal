use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

// =============================================================================
// ResultMessage.modelUsage typed shape (closes #756)
// =============================================================================

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

// =============================================================================
// Per-turn performance metrics (PR 1 of N — capture + persist pipeline)
// =============================================================================

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

// =============================================================================
// Turn-metrics list endpoint (`GET /api/sessions/{id}/turn-metrics`)
//
// Hydrates the SessionView's per-turn metrics buffer on initial load. The live
// path is the existing `ServerToClient::TurnMetrics` WS frame; this endpoint
// covers the cold-start gap (frontend reload, tab restore) and matches the
// access gate used by `GET /api/sessions/{id}/messages` (session_members ACL).
// =============================================================================

/// Response from `GET /api/sessions/{id}/turn-metrics`.
///
/// Rows are ordered `started_at ASC` so the SessionView's join walk (pair Nth
/// terminator message with Nth metrics row) works without a second sort.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnMetricsResponse {
    #[serde(default)]
    pub metrics: Vec<TurnMetrics>,
}

// =============================================================================
// Aggregated turn-metrics endpoint (`GET /api/metrics/turns`)
//
// Powers the Settings → Performance page (PR 4). Bucketed (`hour` | `day`)
// rollups grouped by `(agent_type, model, service_tier)` over a sliding window
// (`7d` / `30d` / `48h` / `2h`, …). Each row carries server-side computed
// counts, p50/p95 latency / throughput aggregates, token sums, cost sum, and
// the stop-reason histogram for the bucket.
// =============================================================================

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Wire shape lifted verbatim from `claude-codes`'
    /// `test_result_with_new_fields`: per-model entry with camelCase keys and
    /// `costUSD` (not `costUsd`). The frontend renderer iterates this map for
    /// the timing tooltip; the typed parse must accept the live wire shape.
    #[test]
    fn model_usage_entry_roundtrip() {
        let json = serde_json::json!({
            "inputTokens": 3817,
            "outputTokens": 14,
            "costUSD": 0.06,
        });
        let parsed: ModelUsageEntry = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.input_tokens, 3817);
        assert_eq!(parsed.output_tokens, 14);
        assert!((parsed.cost_usd - 0.06).abs() < 1e-9);
        // Unset fields default to 0
        assert_eq!(parsed.cache_read_input_tokens, 0);
        assert_eq!(parsed.cache_creation_input_tokens, 0);
        assert_eq!(parsed.web_search_requests, 0);
    }

    /// The full `modelUsage` map: a `BTreeMap<String, ModelUsageEntry>` keyed
    /// by model name. Multiple models accumulate when a session uses haiku +
    /// opus or similar.
    #[test]
    fn model_usage_map_roundtrip() {
        let json = serde_json::json!({
            "claude-opus-4-7[1m]": {
                "inputTokens": 3817,
                "outputTokens": 14,
                "costUSD": 0.06,
            },
            "claude-haiku-4-5": {
                "inputTokens": 100,
                "outputTokens": 5,
                "costUSD": 0.001,
            },
        });
        let parsed: ModelUsage = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.len(), 2);
        let opus = parsed.get("claude-opus-4-7[1m]").unwrap();
        assert_eq!(opus.input_tokens, 3817);
        assert!((opus.cost_usd - 0.06).abs() < 1e-9);
        let haiku = parsed.get("claude-haiku-4-5").unwrap();
        assert_eq!(haiku.output_tokens, 5);
    }

    /// `TurnMetrics` must round-trip with all the optional fields populated
    /// and with cost present (Claude shape).
    #[test]
    fn turn_metrics_full_roundtrip() {
        let started = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let metrics = TurnMetrics {
            id: None,
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: "claude".to_string(),
            model: Some("claude-opus-4-7".to_string()),
            service_tier: Some("standard".to_string()),
            started_at: started,
            first_token_at: Some(started + chrono::Duration::milliseconds(420)),
            completed_at: Some(started + chrono::Duration::milliseconds(8000)),
            ttft_ms: Some(420),
            total_duration_ms: Some(8000),
            generation_duration_ms: Some(7580),
            max_inter_token_gap_ms: Some(150),
            input_tokens: 1234,
            output_tokens: 567,
            cache_creation_tokens: 0,
            cache_read_tokens: 90,
            thinking_tokens: 12,
            stop_reason: Some("end_turn".to_string()),
            is_error: false,
            tool_call_count: 3,
            stream_restarts: 0,
            total_cost_usd: Some(0.0145),
        };
        let json = serde_json::to_value(&metrics).unwrap();
        assert_eq!(json["agent_type"], "claude");
        assert_eq!(json["ttft_ms"], 420);
        assert_eq!(json["total_cost_usd"], 0.0145);
        let parsed: TurnMetrics = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, metrics);
    }

    /// The shared known-model gate: missing, blank, whitespace, and the
    /// literal `unknown` placeholder (any case) are all rejected; real model
    /// ids pass.
    #[test]
    fn turn_metrics_has_known_model() {
        let mut metrics = TurnMetrics {
            id: None,
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: "claude".to_string(),
            model: Some("claude-opus-4-7".to_string()),
            service_tier: None,
            started_at: Utc::now(),
            first_token_at: None,
            completed_at: None,
            ttft_ms: None,
            total_duration_ms: None,
            generation_duration_ms: None,
            max_inter_token_gap_ms: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            thinking_tokens: 0,
            stop_reason: None,
            is_error: false,
            tool_call_count: 0,
            stream_restarts: 0,
            total_cost_usd: None,
        };
        assert!(metrics.has_known_model());

        for bad in [
            None,
            Some(""),
            Some("   "),
            Some("unknown"),
            Some("UNKNOWN"),
        ] {
            metrics.model = bad.map(str::to_string);
            assert!(!metrics.has_known_model(), "expected {:?} rejected", bad);
        }
    }

    /// `MetricBucket` round-trip. Claude row with all fields populated; the
    /// stop-reason mix has a couple of representative entries.
    #[test]
    fn metric_bucket_full_roundtrip() {
        let bucket_start = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut counts = BTreeMap::new();
        counts.insert("end_turn".to_string(), 5);
        counts.insert("max_tokens".to_string(), 1);
        let bucket = MetricBucket {
            bucket_start,
            agent_type: "claude".to_string(),
            model: Some("claude-opus-4-7".to_string()),
            service_tier: Some("standard".to_string()),
            turn_count: 6,
            error_count: 0,
            ttft_p50_ms: Some(420),
            ttft_p95_ms: Some(1200),
            throughput_p50_tps: Some(47.5),
            throughput_p95_tps: Some(65.0),
            input_tokens_sum: 12_000,
            output_tokens_sum: 3_400,
            cache_read_tokens_sum: 800,
            cache_creation_tokens_sum: 200,
            total_cost_usd_sum: Some(0.18),
            stop_reason_counts: counts,
        };
        let json = serde_json::to_value(&bucket).unwrap();
        assert_eq!(json["agent_type"], "claude");
        assert_eq!(json["turn_count"], 6);
        assert_eq!(json["stop_reason_counts"]["end_turn"], 5);
        let parsed: MetricBucket = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, bucket);
    }

    /// `MetricBucketsResponse` round-trip via the top-level envelope.
    #[test]
    fn metric_buckets_response_roundtrip() {
        let bucket = MetricBucket {
            bucket_start: chrono::Utc::now(),
            agent_type: "codex".to_string(),
            model: None,
            service_tier: None,
            turn_count: 3,
            error_count: 1,
            ttft_p50_ms: None,
            ttft_p95_ms: None,
            throughput_p50_tps: None,
            throughput_p95_tps: None,
            input_tokens_sum: 0,
            output_tokens_sum: 0,
            cache_read_tokens_sum: 0,
            cache_creation_tokens_sum: 0,
            total_cost_usd_sum: None,
            stop_reason_counts: BTreeMap::new(),
        };
        let resp = MetricBucketsResponse {
            buckets: vec![bucket.clone()],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json["buckets"].is_array());
        let parsed: MetricBucketsResponse = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.buckets, vec![bucket]);
    }

    /// Codex-shape: cost is `None` and many of the optional fields are also
    /// unset. Must serialize without nulls for the skip-if-none fields and
    /// must round-trip back to the same value.
    #[test]
    fn turn_metrics_codex_no_cost_roundtrip() {
        let started = chrono::DateTime::parse_from_rfc3339("2026-05-27T18:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let metrics = TurnMetrics {
            id: None,
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: "codex".to_string(),
            model: None,
            service_tier: None,
            started_at: started,
            first_token_at: None,
            completed_at: Some(started + chrono::Duration::milliseconds(120)),
            ttft_ms: None,
            total_duration_ms: Some(120),
            generation_duration_ms: None,
            max_inter_token_gap_ms: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            thinking_tokens: 0,
            stop_reason: Some("failed".to_string()),
            is_error: true,
            tool_call_count: 0,
            stream_restarts: 0,
            total_cost_usd: None,
        };
        let json = serde_json::to_value(&metrics).unwrap();
        assert_eq!(json["agent_type"], "codex");
        assert!(json.get("total_cost_usd").is_none());
        assert!(json.get("ttft_ms").is_none());
        let parsed: TurnMetrics = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, metrics);
    }
}
