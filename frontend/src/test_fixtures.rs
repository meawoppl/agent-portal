//! Test fixtures and builders for frontend tests — per-crate helpers with
//! sensible defaults and explicit overrides (see #924).
//!
//! Keep this module small and focused: one builder per shared shape that is
//! repeatedly constructed in `#[cfg(test)]` code. Builders carry the real
//! crate's types and default to a *valid* value so a test only spells out
//! the field it cares about.

use chrono::{DateTime, TimeZone, Utc};
use shared::{api::MetricBucket, AgentType, TurnMetrics};
use std::collections::BTreeMap;
use uuid::Uuid;

/// Builder for `shared::TurnMetrics` with builder-style overrides.
///
/// Defaults mirror the Claude-shaped `sample_metrics` that was previously
/// copy-pasted across four test modules (footer, state, client_websocket,
/// etc.): a started-at of `2026-05-01 00:00:00 UTC`, Claude agent, a known
/// model, and zeroed counters. Callers override only the field under test.
#[derive(Debug, Clone)]
pub struct TurnMetricsBuilder {
    inner: TurnMetrics,
}

impl Default for TurnMetricsBuilder {
    fn default() -> Self {
        Self {
            inner: TurnMetrics {
                id: Some(Uuid::nil()),
                session_id: Uuid::nil(),
                user_message_id: None,
                agent_type: AgentType::Claude,
                model: Some("claude-opus-4-7".to_string()),
                service_tier: Some("standard".to_string()),
                started_at: Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap(),
                first_token_at: None,
                completed_at: None,
                ttft_ms: Some(1310),
                total_duration_ms: Some(12900),
                generation_duration_ms: Some(11590),
                max_inter_token_gap_ms: Some(1500),
                input_tokens: 16,
                output_tokens: 547,
                cache_creation_tokens: 0,
                cache_read_tokens: 84,
                thinking_tokens: 0,
                subagent_tokens: 0,
                stop_reason: Some("end_turn".to_string()),
                is_error: false,
                tool_call_count: 0,
                stream_restarts: 0,
                total_cost_usd: Some(0.014),
                model_context_window: None,
                context_snapshot_tokens: None,
            },
        }
    }
}

impl TurnMetricsBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn id(mut self, id: Option<Uuid>) -> Self {
        self.inner.id = id;
        self
    }

    pub fn session_id(mut self, id: Uuid) -> Self {
        self.inner.session_id = id;
        self
    }

    pub fn agent_type(mut self, agent: AgentType) -> Self {
        self.inner.agent_type = agent;
        self
    }

    pub fn model(mut self, model: Option<&str>) -> Self {
        self.inner.model = model.map(|s| s.to_string());
        self
    }

    pub fn service_tier(mut self, tier: Option<&str>) -> Self {
        self.inner.service_tier = tier.map(|s| s.to_string());
        self
    }

    pub fn started_at(mut self, ts: DateTime<Utc>) -> Self {
        self.inner.started_at = ts;
        self
    }

    /// Convenience: set `started_at` from whole seconds since epoch.
    pub fn started_secs(mut self, secs: i64) -> Self {
        self.inner.started_at = Utc.timestamp_opt(secs, 0).unwrap();
        self
    }

    pub fn ttft_ms(mut self, v: Option<i64>) -> Self {
        self.inner.ttft_ms = v;
        self
    }

    pub fn generation_duration_ms(mut self, v: Option<i64>) -> Self {
        self.inner.generation_duration_ms = v;
        self
    }

    pub fn max_gap_ms(mut self, v: Option<i64>) -> Self {
        self.inner.max_inter_token_gap_ms = v;
        self
    }

    pub fn input_tokens(mut self, n: i64) -> Self {
        self.inner.input_tokens = n;
        self
    }

    pub fn output_tokens(mut self, n: i64) -> Self {
        self.inner.output_tokens = n;
        self
    }

    pub fn cache_read(mut self, n: i64) -> Self {
        self.inner.cache_read_tokens = n;
        self
    }

    pub fn cache_creation(mut self, n: i64) -> Self {
        self.inner.cache_creation_tokens = n;
        self
    }

    pub fn thinking_tokens(mut self, n: i64) -> Self {
        self.inner.thinking_tokens = n;
        self
    }

    pub fn subagent_tokens(mut self, n: i64) -> Self {
        self.inner.subagent_tokens = n;
        self
    }

    pub fn total_cost_usd(mut self, v: Option<f64>) -> Self {
        self.inner.total_cost_usd = v;
        self
    }

    pub fn is_error(mut self, v: bool) -> Self {
        self.inner.is_error = v;
        self
    }

    pub fn stop_reason(mut self, v: Option<&str>) -> Self {
        self.inner.stop_reason = v.map(|s| s.to_string());
        self
    }

    pub fn tool_call_count(mut self, n: i32) -> Self {
        self.inner.tool_call_count = n;
        self
    }

    pub fn stream_restarts(mut self, n: i32) -> Self {
        self.inner.stream_restarts = n;
        self
    }

    pub fn build(self) -> TurnMetrics {
        self.inner
    }
}

/// Builder for `shared::api::MetricBucket` with sensible defaults.
///
/// Defaults mirror the `mk_bucket` helper that was previously copy-pasted in
/// `performance_panel::tests`: 1 turn, Claude, standard tier, 1000 in / 200 out
/// etc., so a test only spells out the fields it varies.
#[derive(Debug, Clone)]
pub struct MetricBucketBuilder {
    inner: MetricBucket,
}

impl MetricBucketBuilder {
    pub fn new(bucket_start: DateTime<Utc>) -> Self {
        Self {
            inner: MetricBucket {
                bucket_start,
                agent_type: AgentType::Claude,
                model: None,
                service_tier: None,
                turn_count: 1,
                error_count: 0,
                ttft_p50_ms: None,
                ttft_p95_ms: None,
                throughput_p50_tps: None,
                throughput_p95_tps: None,
                input_tokens_sum: 1000,
                output_tokens_sum: 200,
                cache_read_tokens_sum: 500,
                cache_creation_tokens_sum: 100,
                thinking_tokens_sum: 0,
                subagent_tokens_sum: 0,
                total_cost_usd_sum: Some(0.05),
                stop_reason_counts: BTreeMap::new(),
            },
        }
    }

    pub fn agent_type(mut self, agent: AgentType) -> Self {
        self.inner.agent_type = agent;
        self
    }

    pub fn model(mut self, model: Option<&str>) -> Self {
        self.inner.model = model.map(|s| s.to_string());
        self
    }

    pub fn service_tier(mut self, tier: Option<&str>) -> Self {
        self.inner.service_tier = tier.map(|s| s.to_string());
        self
    }

    pub fn ttft_p50(mut self, v: Option<i64>) -> Self {
        self.inner.ttft_p50_ms = v;
        self
    }

    pub fn throughput_p50(mut self, v: Option<f64>) -> Self {
        self.inner.throughput_p50_tps = v;
        self
    }

    pub fn input_sum(mut self, n: i64) -> Self {
        self.inner.input_tokens_sum = n;
        self
    }

    pub fn output_sum(mut self, n: i64) -> Self {
        self.inner.output_tokens_sum = n;
        self
    }

    pub fn cache_read_sum(mut self, n: i64) -> Self {
        self.inner.cache_read_tokens_sum = n;
        self
    }

    pub fn cache_creation_sum(mut self, n: i64) -> Self {
        self.inner.cache_creation_tokens_sum = n;
        self
    }

    pub fn total_cost_sum(mut self, v: Option<f64>) -> Self {
        self.inner.total_cost_usd_sum = v;
        self
    }

    pub fn stop_counts(mut self, counts: Vec<(&str, i64)>) -> Self {
        let mut m = BTreeMap::new();
        for (k, v) in counts {
            m.insert(k.to_string(), v);
        }
        self.inner.stop_reason_counts = m;
        self
    }

    pub fn build(self) -> MetricBucket {
        self.inner
    }
}
