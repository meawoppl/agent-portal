//! Per-model usage and per-turn performance metrics types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::AgentType;

/// Per-model usage / cost breakdown carried by Claude's
/// `ResultMessage.modelUsage` field.
pub use claude_codes::io::ModelUsageEntry;

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

    pub agent_type: AgentType,
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
    /// Tokens consumed by spawned subagents (Claude `Task` / sidechains, Codex
    /// sub-threads), rolled up and reported separately from the main turn's
    /// tokens — mirroring the Claude binary's distinct `<subagent_tokens>`
    /// line in its result `<usage>` envelope.
    ///
    /// `0` when the agent doesn't run subagents on the turn, or when the
    /// agent's wire protocol doesn't surface the rollup. The Claude
    /// stream-json `usage` shape exposes no subagent field today, so claude
    /// turns always report `0` until the SDK does — see the upstream gap noted
    /// at the claude `TurnOutcome` build site.
    #[serde(default)]
    pub subagent_tokens: i64,

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

    /// The model's context window in tokens, when the agent reports it. Codex
    /// sends `model_context_window` directly; Claude carries the CLI-resolved
    /// value in `ResultMessage.model_usage`. Older proxies leave this `None`,
    /// so [`TurnMetrics::context_window`] retains the model-id fallback.
    /// Powers the context-usage gauge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context_window: Option<i64>,
    /// Context occupancy at the end of the turn, from the LAST real assistant
    /// usage snapshot (#1517). The other token fields are the turn's accumulated
    /// roll-up — right for cost, wrong for context, because the roll-up
    /// re-counts `cache_read` once per API call in a tool-use loop. `None` from
    /// agents/proxies that don't supply a snapshot, where
    /// [`TurnMetrics::context_tokens`] falls back to the roll-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_snapshot_tokens: Option<i64>,
}

impl TurnMetrics {
    /// True when `model` is usable telemetry: present, non-blank, and not the
    /// literal placeholders `"unknown"` and `"<synthetic>"`. The latter is
    /// Claude's label for CLI-injected bookkeeping, not an executing model. All three ingest points (Claude
    /// proxy, Codex proxy, backend persist) warn-and-drop turn metrics that
    /// fail this check, so the rule must stay identical everywhere.
    pub fn has_known_model(&self) -> bool {
        self.model.as_deref().is_some_and(|value| {
            let value = value.trim();
            !value.is_empty() && !value.eq_ignore_ascii_case("unknown") && value != "<synthetic>"
        })
    }

    /// Tokens occupying the context window on this turn's most recent request —
    /// the numerator of the context-usage gauge.
    ///
    /// The math is **agent-specific** because the two protocols count input
    /// tokens differently:
    /// - **Claude** reports the three input buckets *disjointly* (`input`,
    ///   `cache_read`, `cache_creation`), so the full prompt is their sum.
    /// - **Codex** reports `input_tokens` as the *whole* prompt, with the
    ///   cached / cache-write counts as subsets already inside it — so summing
    ///   them would double-count. Its occupancy is just `input_tokens`.
    pub fn context_tokens(&self) -> i64 {
        // Prefer the per-request snapshot when the proxy supplied one: for
        // Claude the fields below are a roll-up across the turn's API calls, so
        // a tool-heavy turn re-counts `cache_read` once per call and can read
        // several times the window (#1517).
        if let Some(snapshot) = self.context_snapshot_tokens.filter(|t| *t > 0) {
            return snapshot;
        }
        match self.agent_type {
            // Muse: no token-usage events exist in the observed 0.1.0 stream;
            // whatever the proxy reports (typically 0 until usage lands on
            // the wire) passes through flat, Codex-style.
            AgentType::Codex | AgentType::Muse => self.input_tokens,
            // Claude fallback (older proxy, or no usable assistant usage this
            // turn): the disjoint buckets sum to the prompt. Correct for a
            // single-call turn and an over-count for a multi-call one — the
            // pre-#1517 behavior, kept so an old proxy still shows something.
            AgentType::Claude => {
                self.input_tokens + self.cache_read_tokens + self.cache_creation_tokens
            }
        }
    }

    /// Effective context window in tokens: the value the agent reported, else a
    /// nominal size derived from the model id (Claude). `None` when neither is
    /// available (e.g. a Codex turn from before the window was on the wire, or
    /// an unrecognized model) — the caller hides the gauge rather than guess.
    pub fn context_window(&self) -> Option<i64> {
        self.model_context_window.filter(|w| *w > 0).or_else(|| {
            self.model
                .as_deref()
                .and_then(crate::context_window_for)
                .map(|w| w as i64)
        })
    }

    /// Fraction of the context window occupied on this turn, `0.0..`. Can
    /// briefly exceed `1.0` right before an auto-compaction; callers clamp for
    /// display. `None` when the window is unknown.
    pub fn context_fraction(&self) -> Option<f64> {
        let window = self.context_window()?;
        (window > 0).then(|| self.context_tokens() as f64 / window as f64)
    }
}

/// Turn-metrics response shared by the per-session and dashboard endpoints.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TurnMetricsResponse {
    /// Trend/history rows. The per-session endpoint returns the whole session;
    /// the dashboard endpoint returns its bounded newest-turn window.
    #[serde(default)]
    pub metrics: Vec<TurnMetrics>,
    /// Newest context-capable row for every existing session owned by the user.
    /// Kept separate from `metrics` so a busy session cannot evict a quiet
    /// session's current context gauge from the bounded trend window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub latest_by_session: Vec<TurnMetrics>,
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
    pub agent_type: AgentType,
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
    #[serde(default)]
    pub thinking_tokens_sum: i64,
    #[serde(default)]
    pub subagent_tokens_sum: i64,
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

    /// Build a minimal turn with the token fields that drive the gauge.
    ///
    /// Tests below use `claude-opus-4-6` where they assert a fraction: it is one
    /// of the models the CLI's capability table puts at the 200k default, which
    /// keeps the expected arithmetic legible. Current-generation ids
    /// (`claude-opus-4-8`, `claude-sonnet-5`, …) are `native_1m` and resolve to
    /// 1M, so they would change every denominator here.
    fn turn(
        agent_type: AgentType,
        model: Option<&str>,
        model_context_window: Option<i64>,
        input_tokens: i64,
        cache_read_tokens: i64,
        cache_creation_tokens: i64,
    ) -> TurnMetrics {
        TurnMetrics {
            id: None,
            session_id: uuid::Uuid::nil(),
            user_message_id: None,
            agent_type,
            model: model.map(str::to_string),
            service_tier: None,
            started_at: chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap(),
            first_token_at: None,
            completed_at: None,
            ttft_ms: None,
            total_duration_ms: None,
            generation_duration_ms: None,
            max_inter_token_gap_ms: None,
            input_tokens,
            output_tokens: 0,
            cache_creation_tokens,
            cache_read_tokens,
            thinking_tokens: 0,
            subagent_tokens: 0,
            context_snapshot_tokens: None,
            stop_reason: None,
            is_error: false,
            tool_call_count: 0,
            stream_restarts: 0,
            total_cost_usd: None,
            model_context_window,
        }
    }

    /// The per-request snapshot wins over the roll-up when present (#1517).
    /// This is the whole point of the field: on a tool-heavy Claude turn the
    /// roll-up re-counts `cache_read` once per API call, so it can exceed the
    /// window several times over while the true occupancy is far lower.
    #[test]
    fn snapshot_overrides_the_rolled_up_token_fields() {
        // A 6-call turn whose roll-up sums to ~900k against a 200k window…
        let mut t = turn(
            AgentType::Claude,
            Some("claude-opus-4-6"),
            None,
            60_000,
            800_000,
            40_000,
        );
        assert_eq!(t.context_tokens(), 900_000, "roll-up over-counts");
        assert!(
            t.context_fraction().unwrap() > 4.0,
            "roll-up would peg the gauge"
        );

        // …but the last request actually held 150k of the 200k window.
        t.context_snapshot_tokens = Some(150_000);
        assert_eq!(t.context_tokens(), 150_000);
        assert_eq!(t.context_fraction(), Some(150_000.0 / 200_000.0));
    }

    /// Absent or non-positive snapshots fall back to the pre-#1517 sum, so a
    /// turn recorded by an older proxy still renders something.
    #[test]
    fn missing_snapshot_falls_back_to_the_sum() {
        let mut t = turn(
            AgentType::Claude,
            Some("claude-opus-4-6"),
            None,
            30_000,
            10_000,
            3_000,
        );
        assert_eq!(t.context_snapshot_tokens, None);
        assert_eq!(t.context_tokens(), 43_000);
        // A zero snapshot is treated as "no data", not as an empty context.
        t.context_snapshot_tokens = Some(0);
        assert_eq!(t.context_tokens(), 43_000);
    }

    #[test]
    fn claude_context_tokens_sum_disjoint_buckets() {
        // Claude: input + cache_read + cache_creation = full prompt.
        let t = turn(
            AgentType::Claude,
            Some("claude-opus-4-6"),
            None,
            1_000,
            40_000,
            2_000,
        );
        assert_eq!(t.context_tokens(), 43_000);
        // Window derived from the model-name map (200k).
        assert_eq!(t.context_window(), Some(200_000));
        assert_eq!(t.context_fraction(), Some(43_000.0 / 200_000.0));
    }

    #[test]
    fn codex_context_tokens_do_not_double_count_cache() {
        // Codex: input_tokens already includes the cached subset — occupancy is
        // input_tokens alone, and the window comes from the wire.
        let t = turn(
            AgentType::Codex,
            Some("gpt-5-codex"),
            Some(400_000),
            120_000,
            90_000,
            5_000,
        );
        assert_eq!(t.context_tokens(), 120_000);
        assert_eq!(t.context_window(), Some(400_000));
        assert_eq!(t.context_fraction(), Some(120_000.0 / 400_000.0));
    }

    #[test]
    fn reported_window_wins_over_model_map() {
        // A Claude turn that somehow carries an explicit window uses it.
        let t = turn(
            AgentType::Claude,
            Some("claude-sonnet-5"),
            Some(1_000_000),
            500_000,
            0,
            0,
        );
        assert_eq!(t.context_window(), Some(1_000_000));
    }

    #[test]
    fn no_window_when_unknown() {
        // Codex turn with no wire window and a non-Claude model → gauge hidden.
        let t = turn(AgentType::Codex, Some("gpt-5-codex"), None, 100, 0, 0);
        assert_eq!(t.context_window(), None);
        assert_eq!(t.context_fraction(), None);
    }

    #[test]
    fn fraction_may_exceed_one_before_compaction() {
        let t = turn(
            AgentType::Claude,
            Some("claude-opus-4-6"),
            None,
            210_000,
            0,
            0,
        );
        assert!(t.context_fraction().unwrap() > 1.0);
    }
}
