//! Shared display helpers for turn-metrics UI surfaces.

use shared::AgentType;

/// Compact integer for `"2.1k in / 547 out"` chips.
pub(crate) fn compact_count(n: i64) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        format!("{:.1}k", n as f64 / 1000.0)
    }
}

pub(crate) fn compact_metric_count(value: f64) -> String {
    if value < 1000.0 {
        format!("{value:.0}")
    } else {
        format!("{:.1}k", value / 1000.0)
    }
}

/// Strip a vendor prefix and trailing dated suffix so a model name fits a
/// compact dashboard chip.
pub(crate) fn compact_model_label(model: &str) -> String {
    let trimmed = model
        .strip_prefix("claude-")
        .or_else(|| model.strip_prefix("gpt-"))
        .or_else(|| model.strip_prefix("o"))
        .unwrap_or(model);
    let mut parts: Vec<&str> = trimmed.split('-').collect();
    if let Some(last) = parts.last() {
        if last.len() == 8 && last.chars().all(|c| c.is_ascii_digit()) {
            parts.pop();
        }
    }
    parts.join("-")
}

/// Build the compact dashboard label for a model/tier pair.
pub(crate) fn format_compact_model_tier_label(
    model: &Option<String>,
    tier: &Option<String>,
) -> String {
    let short_model = model
        .as_deref()
        .map(compact_model_label)
        .unwrap_or_else(|| "unknown".to_string());
    append_nonstandard_tier(short_model, tier.as_deref())
}

/// Build the full settings-panel label for an agent/model/tier group.
pub(crate) fn format_agent_model_tier_label(
    agent_type: AgentType,
    model: &Option<String>,
    tier: &Option<String>,
) -> String {
    let base = match (agent_type, model.as_deref()) {
        (AgentType::Codex, None) => "Codex".to_string(),
        (_, Some(model)) => model.to_string(),
        (agent, None) => format!("{agent} unknown"),
    };
    append_nonstandard_tier(base, tier.as_deref())
}

fn append_nonstandard_tier(base: String, tier: Option<&str>) -> String {
    match tier {
        Some(t) if !t.is_empty() && !t.eq_ignore_ascii_case("standard") => {
            format!("{base} {}", t.to_ascii_lowercase())
        }
        _ => base,
    }
}

/// Tokens-per-second chip text, e.g. `"47.2 tok/s"`.
pub(crate) fn format_tok_per_sec(
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

/// TTFT chip text, e.g. `"TTFT 1.31s"`.
pub(crate) fn format_ttft(ttft_ms: Option<i64>) -> Option<String> {
    let ms = ttft_ms?;
    Some(format!("TTFT {:.2}s", ms as f64 / 1000.0))
}

/// Cache-hit-% chip text, e.g. `"cache 84% hit"`.
pub(crate) fn format_cache_hit(input: i64, cache_read: i64, cache_creation: i64) -> Option<String> {
    let total = input + cache_read + cache_creation;
    if total <= 0 {
        return None;
    }
    let pct = (cache_read as f64 / total as f64) * 100.0;
    Some(format!("cache {:.0}% hit", pct))
}

/// Max-gap chip text, e.g. `"max gap 1.5s"`.
pub(crate) fn format_max_gap(max_inter_token_gap_ms: Option<i64>) -> Option<String> {
    let ms = max_inter_token_gap_ms?;
    if ms <= 1000 {
        return None;
    }
    Some(format!("max gap {:.1}s", ms as f64 / 1000.0))
}

/// Cost chip text, e.g. `"$0.014"` for sub-$1 and `"$1.23"` for $1+.
pub(crate) fn format_cost(total_cost_usd: Option<f64>) -> Option<String> {
    let cost = total_cost_usd?;
    if cost.abs() < 1.0 {
        Some(format!("${:.3}", cost))
    } else {
        Some(format!("${:.2}", cost))
    }
}
