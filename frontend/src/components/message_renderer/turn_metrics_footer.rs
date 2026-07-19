//! Per-turn metrics footer renderer for the `Result` (Claude) and
//! `turn.completed` (Codex) terminator cards.
//!
//! Renders a small one-line chip strip directly below the existing
//! `result-stats-bar` card, e.g.
//!
//! ```text
//! 47.2 tok/s · TTFT 1.31s · 2.1k in / 547 out · cache 84% hit · max gap 0.4s · $0.014
//! ```
//!
//! Each chip is a `<span class="turn-metric-chip">` so we can color the
//! individual values independently later (per-tier coloring is a future
//! follow-up; this PR ships the chips as plain muted text). The chip strip
//! lives in `<div class="turn-metrics-footer">…</div>` underneath the result
//! card — see `frontend/styles/messages.css` for the visual.
//!
//! All formatters are pure helpers (no Yew dependency, no DOM) so they can be
//! unit-tested in isolation. The single Yew-facing entry point is
//! [`render_turn_metrics_footer`], which takes a `&TurnMetrics` and returns
//! the `Html` for the footer (or `html! {}` if nothing meaningful would
//! render — e.g. a synthetic test row with all fields empty).
//!
//! Field-by-field formatting rules (from PR 2 scope):
//!
//! - **tok/s**: `output_tokens / (generation_duration_ms / 1000)` formatted
//!   with 1 decimal place. Omitted when `generation_duration_ms` is `None`
//!   or `0` (avoids div-by-zero / nonsensical infinities on Codex error
//!   paths where the field is unset).
//! - **TTFT**: `ttft_ms` rendered as seconds with 2 decimals. Omitted when
//!   `None`. Codex turns that latch a first-content frame still get TTFT.
//! - **Tokens in/out**: `compact_count(input_tokens)` in, `compact_count
//!   (output_tokens)` out — `compact_count` returns the integer for values
//!   `< 1000`, and a one-decimal-k abbreviation (`"1.5k"`, `"2.1k"`) for
//!   `≥ 1000`. Always renders (zero tokens is a meaningful signal).
//! - **Thinking/subagent tokens**: rendered as count-up chips after the
//!   regular text chips when the corresponding counter is positive.
//! - **Cache hit %**: `cache_read / (input + cache_read + cache_creation) *
//!   100`. Omitted when all three are zero — for Codex turns this is
//!   universally true today (the proxy doesn't surface cache breakdown), so
//!   this chip naturally vanishes for codex sessions.
//! - **Max gap**: `max_inter_token_gap_ms` rendered as seconds with 1
//!   decimal. Only rendered when the gap is `> 1000ms` — sub-1s gaps are
//!   the steady-state hum and showing them would just clutter the chip
//!   strip with `"max gap 0.2s"` on every successful turn.
//! - **Cost**: `total_cost_usd` rendered as `$X.XXX` for values below $1.00
//!   and `$X.XX` at $1.00 and above. Omitted when `None` (Codex turns).

use shared::TurnMetrics;
use yew::prelude::*;

/// Compact integer for "2.1k in / 547 out" chips.
///
/// Values `< 1000` render as integers; values `≥ 1000` render as a
/// one-decimal-k abbreviation (`1500` → `"1.5k"`, `1_000_000` → `"1000.0k"`).
/// We deliberately don't introduce an `M` suffix — a single turn that emits
/// 1M tokens is so far out of distribution that a long, easy-to-eyeball
/// `1000.0k` is preferable to an opaque `1.0M`.
pub fn compact_count(n: i64) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        format!("{:.1}k", n as f64 / 1000.0)
    }
}

/// Tokens-per-second chip text, e.g. `"47.2 tok/s"`.
///
/// Returns `None` when `generation_duration_ms` is `None` or `0` — both
/// happen on error paths (turn aborted before any content frame, Codex
/// failed-turn frames with missing duration) and rendering "inf tok/s" or
/// dividing by zero would be misleading.
pub fn format_tok_per_sec(
    output_tokens: i64,
    generation_duration_ms: Option<i64>,
) -> Option<String> {
    let gen_ms = generation_duration_ms?;
    if gen_ms <= 0 {
        return None;
    }
    let tok_per_sec = output_tokens as f64 / (gen_ms as f64 / 1000.0);
    Some(format!("{:.1} tok/s", tok_per_sec))
}

/// TTFT chip text, e.g. `"TTFT 1.31s"`. Returns `None` if `ttft_ms` is
/// `None` (turn errored before any content frame).
pub fn format_ttft(ttft_ms: Option<i64>) -> Option<String> {
    let ms = ttft_ms?;
    Some(format!("TTFT {:.2}s", ms as f64 / 1000.0))
}

/// Cache-hit-% chip text, e.g. `"cache 84% hit"`.
///
/// Denominator is `input + cache_read + cache_creation` — the total prompt
/// tokens this turn could plausibly have come from cache. Returns `None`
/// when all three are zero (no prompt at all, or Codex turn that doesn't
/// expose the cache breakdown — both should hide the chip rather than
/// render a misleading "0% hit").
pub fn format_cache_hit(input: i64, cache_read: i64, cache_creation: i64) -> Option<String> {
    let total = input + cache_read + cache_creation;
    if total <= 0 {
        return None;
    }
    let pct = (cache_read as f64 / total as f64) * 100.0;
    Some(format!("cache {:.0}% hit", pct))
}

/// Max-gap chip text, e.g. `"max gap 1.5s"`.
///
/// Only renders when the gap exceeds 1000ms — sub-1s inter-token gaps are
/// the normal streaming heartbeat and showing them would just clutter every
/// successful turn's chip strip. The 1000ms threshold matches the eyeball
/// "I noticed a stall" line for a streaming UI.
pub fn format_max_gap(max_inter_token_gap_ms: Option<i64>) -> Option<String> {
    let ms = max_inter_token_gap_ms?;
    if ms <= 1000 {
        return None;
    }
    Some(format!("max gap {:.1}s", ms as f64 / 1000.0))
}

/// Cost chip text, e.g. `"$0.014"` for sub-$1 and `"$1.23"` for $1+.
///
/// Returns `None` when `total_cost_usd` is `None` — Codex turns don't
/// surface cost on their wire today, so the chip vanishes naturally for
/// codex sessions.
pub fn format_cost(total_cost_usd: Option<f64>) -> Option<String> {
    let cost = total_cost_usd?;
    if cost.abs() < 1.0 {
        Some(format!("${:.3}", cost))
    } else {
        Some(format!("${:.2}", cost))
    }
}

/// Build the full chip list for a `TurnMetrics` row, in render order. Each
/// `Some(text)` chip becomes one `<span class="turn-metric-chip">` in the
/// footer; `None` entries are skipped. The tokens-in/tokens-out chip is
/// always present (zero is a meaningful signal — it tells you a turn went
/// through tool routing without producing any prompt or generation tokens).
///
/// Returned as `Vec<String>` rather than `Vec<Html>` so the renderer can be
/// unit-tested against the textual content without spinning up Yew.
pub fn build_chip_list(metrics: &TurnMetrics) -> Vec<String> {
    let mut chips = Vec::new();
    if let Some(s) = format_tok_per_sec(metrics.output_tokens, metrics.generation_duration_ms) {
        chips.push(s);
    }
    if let Some(s) = format_ttft(metrics.ttft_ms) {
        chips.push(s);
    }
    chips.push(format!(
        "{} in / {} out",
        compact_count(metrics.input_tokens),
        compact_count(metrics.output_tokens),
    ));
    if let Some(s) = format_cache_hit(
        metrics.input_tokens,
        metrics.cache_read_tokens,
        metrics.cache_creation_tokens,
    ) {
        chips.push(s);
    }
    if let Some(s) = format_max_gap(metrics.max_inter_token_gap_ms) {
        chips.push(s);
    }
    if let Some(s) = format_cost(metrics.total_cost_usd) {
        chips.push(s);
    }
    chips
}

/// Render the per-turn metrics footer for one terminator card. Sits in a
/// `<div class="turn-metrics-footer">` directly below the parent
/// `result-message` card; chips are separated by the `·` character (a
/// `<span class="turn-metric-sep">` so the CSS can dim it independently).
///
/// The text chips come from [`build_chip_list`]. The reasoning-token and
/// subagent-token chips are rendered separately as animated odometers
/// (`CountUp`) — they roll 0→total on mount — and are appended last. They only
/// appear when their counters are positive.
///
/// Takes an `Option` and renders nothing for `None` so callers can pass
/// their `Option<&TurnMetrics>` straight through.
pub fn render_turn_metrics_footer(metrics: Option<&TurnMetrics>) -> Html {
    use crate::components::CountUp;

    let Some(metrics) = metrics else {
        return html! {};
    };

    let text_chips = build_chip_list(metrics);
    let has_thinking = metrics.thinking_tokens > 0;
    let has_subagent = metrics.subagent_tokens > 0;
    if text_chips.is_empty() && !has_thinking && !has_subagent {
        return html! {};
    }

    let mut chips: Vec<Html> = text_chips
        .into_iter()
        .map(|chip| html! { <span class="turn-metric-chip">{ chip }</span> })
        .collect();
    if has_thinking {
        chips.push(html! {
            <span class="turn-metric-chip thinking-chip">
                <CountUp target={metrics.thinking_tokens} suffix={" thinking"} compact={true} />
            </span>
        });
    }
    if has_subagent {
        chips.push(html! {
            <span class="turn-metric-chip subagent-chip">
                <CountUp target={metrics.subagent_tokens} suffix={" subagent"} compact={true} />
            </span>
        });
    }

    html! {
        <div class="turn-metrics-footer">
            { for chips.into_iter().enumerate().map(|(i, chip)| {
                html! {
                    <>
                        if i > 0 {
                            <span class="turn-metric-sep">{ "\u{00b7}" }</span>
                        }
                        { chip }
                    </>
                }
            }) }
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use shared::AgentType;
    use uuid::Uuid;

    // ---- compact_count ----

    #[test]
    fn compact_count_single_digit() {
        assert_eq!(compact_count(1), "1");
    }

    #[test]
    fn compact_count_just_below_threshold() {
        assert_eq!(compact_count(999), "999");
    }

    #[test]
    fn compact_count_at_threshold() {
        assert_eq!(compact_count(1000), "1.0k");
    }

    #[test]
    fn compact_count_one_and_a_half_k() {
        assert_eq!(compact_count(1500), "1.5k");
    }

    #[test]
    fn compact_count_one_million() {
        // We deliberately stay on the k-suffix all the way up — see the
        // doc comment on `compact_count` for the rationale (1M tokens in a
        // single turn is so far out of distribution that the long form is
        // easier to eyeball than an opaque `1.0M`).
        assert_eq!(compact_count(1_000_000), "1000.0k");
    }

    // ---- format_tok_per_sec ----

    #[test]
    fn tok_per_sec_basic() {
        // 1000 output tokens over 2 seconds = 500 tok/s
        assert_eq!(
            format_tok_per_sec(1000, Some(2000)),
            Some("500.0 tok/s".to_string())
        );
    }

    #[test]
    fn tok_per_sec_one_decimal_clamp() {
        // 100 / 2.123 = 47.103… → "47.1 tok/s"
        let got = format_tok_per_sec(100, Some(2123)).expect("some");
        assert!(got.starts_with("47.1"), "got: {}", got);
        assert!(got.ends_with(" tok/s"), "got: {}", got);
    }

    #[test]
    fn tok_per_sec_none_duration() {
        assert_eq!(format_tok_per_sec(100, None), None);
    }

    #[test]
    fn tok_per_sec_zero_duration() {
        // div-by-zero guard — must not produce "inf tok/s" or panic.
        assert_eq!(format_tok_per_sec(100, Some(0)), None);
    }

    // ---- format_ttft ----

    #[test]
    fn ttft_basic() {
        assert_eq!(format_ttft(Some(1310)), Some("TTFT 1.31s".to_string()));
    }

    #[test]
    fn ttft_sub_second() {
        assert_eq!(format_ttft(Some(420)), Some("TTFT 0.42s".to_string()));
    }

    #[test]
    fn ttft_none() {
        assert_eq!(format_ttft(None), None);
    }

    // ---- format_cache_hit ----

    #[test]
    fn cache_hit_all_zero_omits() {
        // Codex turns: all three are 0 because the proxy doesn't surface the
        // breakdown. The chip must drop rather than render "0% hit".
        assert_eq!(format_cache_hit(0, 0, 0), None);
    }

    #[test]
    fn cache_hit_partial() {
        // 84% of (100 + 600 + 100) = 800 → cache_read=672, but we just need
        // a known case. 84 / 100 = 84.
        assert_eq!(
            format_cache_hit(16, 84, 0),
            Some("cache 84% hit".to_string())
        );
    }

    #[test]
    fn cache_hit_all_cache() {
        // All tokens came from cache.
        assert_eq!(
            format_cache_hit(0, 100, 0),
            Some("cache 100% hit".to_string())
        );
    }

    #[test]
    fn cache_hit_all_fresh() {
        // All tokens were fresh-input (no cache).
        assert_eq!(
            format_cache_hit(100, 0, 0),
            Some("cache 0% hit".to_string())
        );
    }

    // ---- format_max_gap ----

    #[test]
    fn max_gap_sub_one_second_omits() {
        // 400ms is the streaming heartbeat — not interesting.
        assert_eq!(format_max_gap(Some(400)), None);
    }

    #[test]
    fn max_gap_at_one_second_omits() {
        // The 1000ms threshold is exclusive — exactly 1000ms still hides.
        assert_eq!(format_max_gap(Some(1000)), None);
    }

    #[test]
    fn max_gap_just_over_one_second_renders() {
        assert_eq!(format_max_gap(Some(1500)), Some("max gap 1.5s".to_string()));
    }

    #[test]
    fn max_gap_none() {
        assert_eq!(format_max_gap(None), None);
    }

    // ---- format_cost ----

    #[test]
    fn cost_none() {
        // Codex turns: cost is None.
        assert_eq!(format_cost(None), None);
    }

    #[test]
    fn cost_sub_dollar_uses_three_decimals() {
        assert_eq!(format_cost(Some(0.014)), Some("$0.014".to_string()));
    }

    #[test]
    fn cost_at_dollar_uses_two_decimals() {
        assert_eq!(format_cost(Some(1.00)), Some("$1.00".to_string()));
    }

    #[test]
    fn cost_above_dollar_uses_two_decimals() {
        assert_eq!(format_cost(Some(1.234)), Some("$1.23".to_string()));
    }

    // ---- end-to-end chip-list builder ----

    fn sample_metrics() -> TurnMetrics {
        TurnMetrics {
            id: Some(Uuid::nil()),
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: AgentType::Claude,
            model: Some("claude-opus-4-7".to_string()),
            service_tier: Some("standard".to_string()),
            started_at: Utc::now(),
            first_token_at: None,
            completed_at: None,
            // 547 output / 11.59s gen ≈ 47.2 tok/s.
            ttft_ms: Some(1310),
            total_duration_ms: Some(12900),
            generation_duration_ms: Some(11590),
            // 1500ms — above the 1000ms threshold so the chip renders.
            max_inter_token_gap_ms: Some(1500),
            // 84% of 2100 = ~1764; pick numbers that give exactly 84%.
            // (cache_read 84, fresh 16, cache_creation 0) → 84% of 100.
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
        }
    }

    /// End-to-end builder test: assemble a representative `TurnMetrics`
    /// (Claude-shape, with every optional populated) and assert the chip
    /// list is the exact order + content the spec calls out:
    ///
    /// ```text
    /// 47.2 tok/s · TTFT 1.31s · 16 in / 547 out · cache 84% hit · max gap 1.5s · $0.014
    /// ```
    #[test]
    fn end_to_end_full_chip_list() {
        let m = sample_metrics();
        let chips = build_chip_list(&m);
        assert_eq!(
            chips,
            vec![
                "47.2 tok/s".to_string(),
                "TTFT 1.31s".to_string(),
                "16 in / 547 out".to_string(),
                "cache 84% hit".to_string(),
                "max gap 1.5s".to_string(),
                "$0.014".to_string(),
            ],
        );
    }

    /// Codex-shape: no cost, no cache breakdown, no max gap above 1s — the
    /// chip list is just tok/s, TTFT, and the tokens chip.
    #[test]
    fn end_to_end_codex_shape_drops_chips() {
        let m = TurnMetrics {
            id: Some(Uuid::nil()),
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: AgentType::Codex,
            model: None,
            service_tier: None,
            started_at: Utc::now(),
            first_token_at: None,
            completed_at: None,
            ttft_ms: Some(800),
            total_duration_ms: Some(4200),
            generation_duration_ms: Some(3400),
            max_inter_token_gap_ms: Some(200), // sub-1s → drops
            input_tokens: 0,
            output_tokens: 200,
            cache_creation_tokens: 0,
            cache_read_tokens: 0, // all zero → cache chip drops
            thinking_tokens: 0,
            subagent_tokens: 0,
            stop_reason: None,
            is_error: false,
            tool_call_count: 0,
            stream_restarts: 0,
            total_cost_usd: None, // None → cost chip drops
        };
        let chips = build_chip_list(&m);
        assert_eq!(
            chips,
            vec![
                // 200 / 3.4 = 58.82… → "58.8 tok/s"
                "58.8 tok/s".to_string(),
                "TTFT 0.80s".to_string(),
                "0 in / 200 out".to_string(),
            ],
        );
    }
}
