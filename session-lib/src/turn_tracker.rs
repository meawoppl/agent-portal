//! Per-turn performance metrics tracker (PR 1 of N).
//!
//! `TurnTracker` is a pure-logic state machine that captures the timing,
//! token, and outcome signals of a single agent turn (user input → terminator)
//! and produces a typed [`shared::TurnMetrics`] payload at finalize time.
//!
//! Wall-clock (UTC) timestamps come from `chrono::Utc::now()`; relative
//! durations and the max inter-token gap are measured against the proxy's
//! `std::time::Instant` clock so they're monotonic across system clock
//! adjustments.
//!
//! No async, no I/O — this module is fully unit-testable.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use shared::TurnMetrics;
use uuid::Uuid;

/// Running state of the current turn.
///
/// Constructed once per session; agent-specific I/O tasks call `start` on
/// each user input and `finalize` on each terminator. `record_content_frame`
/// is invoked for every content-bearing frame (assistant text, codex
/// `agent_message` item, etc.) — it doubles as the TTFT trigger (the first
/// such call after `start`) and as the inter-token gap probe.
#[derive(Debug)]
pub struct TurnTracker {
    /// Session this tracker belongs to. Stamped onto every emitted
    /// `TurnMetrics` so the backend can route the row without an extra
    /// out-of-band lookup.
    session_id: Uuid,
    state: TurnState,
}

#[derive(Debug)]
enum TurnState {
    /// No turn in flight.
    Idle,
    /// Turn started but no content frame seen yet (TTFT still unknown).
    Running(RunningTurn),
}

#[derive(Debug)]
struct RunningTurn {
    /// Monotonic clock anchor for TTFT, total duration, and inter-token gap
    /// measurements. Immune to wall-clock adjustments.
    started_at_instant: Instant,
    /// Wall-clock UTC matching `started_at_instant`. Persisted onto the
    /// `TurnMetrics` payload for the DB.
    started_at_utc: DateTime<Utc>,

    /// First content-bearing frame instant, if seen. `None` until the first
    /// `record_content_frame` after `start`.
    first_token_instant: Option<Instant>,
    first_token_utc: Option<DateTime<Utc>>,

    /// Instant of the most recent content frame (for inter-token gap).
    last_token_instant: Option<Instant>,

    /// Max gap (ms) observed between consecutive content frames in this turn.
    max_gap_ms: i64,

    /// Tool calls counted in this turn (claude `ToolUse` blocks, codex
    /// tool-style `item.started` notifications).
    tool_call_count: i32,

    /// Stream restarts observed during this turn (e.g. claude rate-limit
    /// retry, codex turn restart).
    stream_restarts: i32,
}

impl TurnTracker {
    /// Build a fresh tracker for a session. No turn is in flight yet — call
    /// `start` when the first user input is sent.
    pub fn new(session_id: Uuid) -> Self {
        Self {
            session_id,
            state: TurnState::Idle,
        }
    }

    /// The session id this tracker is attached to.
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// Begin a new turn. Resets all counters; safe to call mid-turn (a
    /// subsequent `finalize` would carry only the new turn's state).
    pub fn start(&mut self, now_instant: Instant, now_utc: DateTime<Utc>) {
        self.state = TurnState::Running(RunningTurn {
            started_at_instant: now_instant,
            started_at_utc: now_utc,
            first_token_instant: None,
            first_token_utc: None,
            last_token_instant: None,
            max_gap_ms: 0,
            tool_call_count: 0,
            stream_restarts: 0,
        });
    }

    /// Whether a turn is currently in flight.
    pub fn is_running(&self) -> bool {
        matches!(self.state, TurnState::Running(_))
    }

    /// Record a content-bearing frame at the given monotonic instant.
    ///
    /// The first call after `start` latches the TTFT; subsequent calls
    /// update the inter-token-gap maximum. No-op when no turn is in flight.
    pub fn record_content_frame(&mut self, now_instant: Instant) {
        let TurnState::Running(turn) = &mut self.state else {
            return;
        };

        if turn.first_token_instant.is_none() {
            turn.first_token_instant = Some(now_instant);
            turn.first_token_utc = Some(Utc::now());
        } else if let Some(last) = turn.last_token_instant {
            let gap = now_instant.saturating_duration_since(last);
            let gap_ms = duration_to_ms_i64(gap);
            if gap_ms > turn.max_gap_ms {
                turn.max_gap_ms = gap_ms;
            }
        }
        turn.last_token_instant = Some(now_instant);
    }

    /// Record a tool-call frame within the active turn. No-op when idle.
    pub fn record_tool_call(&mut self) {
        if let TurnState::Running(turn) = &mut self.state {
            turn.tool_call_count = turn.tool_call_count.saturating_add(1);
        }
    }

    /// Record a stream-restart within the active turn (e.g. rate-limit
    /// retry). No-op when idle.
    pub fn record_stream_restart(&mut self) {
        if let TurnState::Running(turn) = &mut self.state {
            turn.stream_restarts = turn.stream_restarts.saturating_add(1);
        }
    }

    /// Finalize the current turn, producing the typed `TurnMetrics` payload
    /// the proxy ships to the backend. Returns `None` if no turn was in
    /// flight (defensive — callers should only finalize after a successful
    /// `start`).
    ///
    /// `now_instant` / `now_utc` must come from the caller so the
    /// terminator's timestamps line up with whatever clock source the
    /// agent-specific I/O task is using.
    #[allow(clippy::too_many_arguments)]
    pub fn finalize(
        &mut self,
        now_instant: Instant,
        now_utc: DateTime<Utc>,
        outcome: TurnOutcome,
    ) -> Option<TurnMetrics> {
        let turn = match std::mem::replace(&mut self.state, TurnState::Idle) {
            TurnState::Running(turn) => turn,
            TurnState::Idle => return None,
        };

        let total_duration = now_instant.saturating_duration_since(turn.started_at_instant);
        let total_duration_ms = duration_to_ms_i64(total_duration);
        let ttft_ms = turn.first_token_instant.map(|inst| {
            duration_to_ms_i64(inst.saturating_duration_since(turn.started_at_instant))
        });
        let generation_duration_ms = ttft_ms.map(|t| (total_duration_ms - t).max(0));
        // Only report a gap value if at least two content frames were seen;
        // otherwise the field stays `None` so the UI can show "n/a" instead
        // of a misleading 0.
        let max_inter_token_gap_ms = if turn.first_token_instant.is_some() && turn.max_gap_ms > 0 {
            Some(turn.max_gap_ms)
        } else if turn.first_token_instant.is_some() {
            Some(0)
        } else {
            None
        };

        let metrics = TurnMetrics {
            id: None,
            session_id: self.session_id,
            user_message_id: None,
            agent_type: outcome.agent_type,
            model: outcome.model,
            service_tier: outcome.service_tier,
            started_at: turn.started_at_utc,
            first_token_at: turn.first_token_utc,
            completed_at: Some(now_utc),
            ttft_ms,
            total_duration_ms: Some(total_duration_ms),
            generation_duration_ms,
            max_inter_token_gap_ms,
            input_tokens: outcome.input_tokens,
            output_tokens: outcome.output_tokens,
            cache_creation_tokens: outcome.cache_creation_tokens,
            cache_read_tokens: outcome.cache_read_tokens,
            thinking_tokens: outcome.thinking_tokens,
            subagent_tokens: outcome.subagent_tokens,
            stop_reason: outcome.stop_reason,
            is_error: outcome.is_error,
            tool_call_count: turn.tool_call_count,
            stream_restarts: turn.stream_restarts,
            total_cost_usd: outcome.total_cost_usd,
        };

        // Structured per-turn performance + token-accounting log. One line per
        // finalized turn across every agent backend, so the most accurate
        // token counts we have are always observable in the proxy logs (not
        // only after they round-trip through the backend/DB). Subagent tokens
        // are logged distinctly, mirroring the Claude binary's own breakdown.
        tracing::info!(
            target: "turn_metrics",
            session_id = %metrics.session_id,
            agent_type = %metrics.agent_type,
            model = metrics.model.as_deref().unwrap_or("unknown"),
            service_tier = metrics.service_tier.as_deref().unwrap_or("-"),
            input_tokens = metrics.input_tokens,
            output_tokens = metrics.output_tokens,
            cache_creation_tokens = metrics.cache_creation_tokens,
            cache_read_tokens = metrics.cache_read_tokens,
            thinking_tokens = metrics.thinking_tokens,
            subagent_tokens = metrics.subagent_tokens,
            ttft_ms = metrics.ttft_ms.unwrap_or(-1),
            total_duration_ms = metrics.total_duration_ms.unwrap_or(-1),
            tool_call_count = metrics.tool_call_count,
            is_error = metrics.is_error,
            stop_reason = metrics.stop_reason.as_deref().unwrap_or("-"),
            "turn finalized"
        );

        Some(metrics)
    }
}

/// Terminator-derived fields the agent-specific I/O task plugs into
/// `finalize`. The tracker can't know the model name, service tier, or
/// token totals on its own — those come from the agent's `Result` /
/// `TurnCompleted` frame.
#[derive(Debug, Clone, Default)]
pub struct TurnOutcome {
    pub agent_type: String,
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub thinking_tokens: i64,
    /// Tokens consumed by subagents spawned during the turn (Claude `Task` /
    /// sidechains, Codex sub-threads), rolled up separately from the main
    /// turn's tokens. `0` when no subagents ran or the agent protocol doesn't
    /// surface the rollup. See [`shared::TurnMetrics::subagent_tokens`].
    pub subagent_tokens: i64,
    pub stop_reason: Option<String>,
    pub is_error: bool,
    pub total_cost_usd: Option<f64>,
}

fn duration_to_ms_i64(d: Duration) -> i64 {
    // `as_millis()` returns u128; clamp to i64::MAX defensively so a wildly
    // wedged session doesn't overflow on cast.
    let m = d.as_millis();
    if m > i64::MAX as u128 {
        i64::MAX
    } else {
        m as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome() -> TurnOutcome {
        TurnOutcome {
            agent_type: "claude".to_string(),
            model: Some("claude-opus-4-7".to_string()),
            service_tier: Some("standard".to_string()),
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_tokens: 0,
            cache_read_tokens: 10,
            thinking_tokens: 3,
            subagent_tokens: 25,
            stop_reason: Some("end_turn".to_string()),
            is_error: false,
            total_cost_usd: Some(0.005),
        }
    }

    /// A turn that ends in an error before any content frame arrives: TTFT
    /// stays `None`, gap stays `None`, but the terminator-derived fields
    /// still flow through.
    #[test]
    fn finalize_with_no_first_token() {
        let session_id = Uuid::new_v4();
        let mut tracker = TurnTracker::new(session_id);
        let t0_inst = Instant::now();
        let t0_utc = Utc::now();
        tracker.start(t0_inst, t0_utc);

        let t1_inst = t0_inst + Duration::from_millis(200);
        let t1_utc = t0_utc + chrono::Duration::milliseconds(200);
        let mut outcome = outcome();
        outcome.is_error = true;
        outcome.stop_reason = Some("error".to_string());
        let m = tracker.finalize(t1_inst, t1_utc, outcome).unwrap();
        assert_eq!(m.session_id, session_id);
        assert!(m.is_error);
        assert!(m.ttft_ms.is_none());
        assert!(m.first_token_at.is_none());
        assert!(m.max_inter_token_gap_ms.is_none());
        assert!(m.generation_duration_ms.is_none());
        assert_eq!(m.total_duration_ms, Some(200));
        assert_eq!(m.stop_reason.as_deref(), Some("error"));
    }

    /// TTFT is the gap from `start` to the first `record_content_frame`.
    #[test]
    fn ttft_is_first_content_frame() {
        let session_id = Uuid::new_v4();
        let mut tracker = TurnTracker::new(session_id);
        let t0 = Instant::now();
        tracker.start(t0, Utc::now());

        tracker.record_content_frame(t0 + Duration::from_millis(120));
        tracker.record_content_frame(t0 + Duration::from_millis(170));

        let m = tracker
            .finalize(t0 + Duration::from_millis(500), Utc::now(), outcome())
            .unwrap();
        assert_eq!(m.ttft_ms, Some(120));
        // total minus ttft
        assert_eq!(m.generation_duration_ms, Some(380));
        assert_eq!(m.total_duration_ms, Some(500));
    }

    /// `max_inter_token_gap_ms` is the largest gap between successive content
    /// frames. Three frames with gaps 40 and 90 → 90 wins.
    #[test]
    fn max_inter_token_gap_picks_largest() {
        let session_id = Uuid::new_v4();
        let mut tracker = TurnTracker::new(session_id);
        let t0 = Instant::now();
        tracker.start(t0, Utc::now());

        tracker.record_content_frame(t0 + Duration::from_millis(100));
        tracker.record_content_frame(t0 + Duration::from_millis(140)); // gap 40
        tracker.record_content_frame(t0 + Duration::from_millis(230)); // gap 90
        tracker.record_content_frame(t0 + Duration::from_millis(260)); // gap 30

        let m = tracker
            .finalize(t0 + Duration::from_millis(300), Utc::now(), outcome())
            .unwrap();
        assert_eq!(m.max_inter_token_gap_ms, Some(90));
    }

    /// Tool calls and stream restarts accumulate into the finalized payload.
    /// Cost flows through verbatim (Claude shape).
    #[test]
    fn finalize_with_cost_and_tool_calls() {
        let session_id = Uuid::new_v4();
        let mut tracker = TurnTracker::new(session_id);
        let t0 = Instant::now();
        tracker.start(t0, Utc::now());
        tracker.record_content_frame(t0 + Duration::from_millis(50));
        tracker.record_tool_call();
        tracker.record_tool_call();
        tracker.record_stream_restart();

        let m = tracker
            .finalize(t0 + Duration::from_millis(800), Utc::now(), outcome())
            .unwrap();
        assert_eq!(m.tool_call_count, 2);
        assert_eq!(m.stream_restarts, 1);
        assert_eq!(m.total_cost_usd, Some(0.005));
        assert_eq!(m.model.as_deref(), Some("claude-opus-4-7"));
        // Subagent token rollup flows verbatim from the terminator outcome.
        assert_eq!(m.subagent_tokens, 25);
    }

    /// Codex turns have no cost on the wire. Finalize must accept `None` and
    /// propagate it.
    #[test]
    fn finalize_without_cost_codex() {
        let session_id = Uuid::new_v4();
        let mut tracker = TurnTracker::new(session_id);
        let t0 = Instant::now();
        tracker.start(t0, Utc::now());
        tracker.record_content_frame(t0 + Duration::from_millis(80));
        let outcome = TurnOutcome {
            agent_type: "codex".to_string(),
            model: None,
            service_tier: None,
            input_tokens: 42,
            output_tokens: 7,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            thinking_tokens: 0,
            subagent_tokens: 0,
            stop_reason: Some("completed".to_string()),
            is_error: false,
            total_cost_usd: None,
        };
        let m = tracker
            .finalize(t0 + Duration::from_millis(400), Utc::now(), outcome)
            .unwrap();
        assert_eq!(m.agent_type, "codex");
        assert!(m.total_cost_usd.is_none());
        assert!(m.model.is_none());
        assert_eq!(m.input_tokens, 42);
        assert_eq!(m.output_tokens, 7);
        // Only one content frame seen → gap measurable as 0 (one frame, no
        // inter-frame interval), not None.
        assert_eq!(m.max_inter_token_gap_ms, Some(0));
    }

    /// Finalizing without ever calling `start` is a no-op (returns `None`),
    /// not a panic.
    #[test]
    fn finalize_without_start_returns_none() {
        let mut tracker = TurnTracker::new(Uuid::new_v4());
        assert!(tracker
            .finalize(Instant::now(), Utc::now(), outcome())
            .is_none());
    }
}
