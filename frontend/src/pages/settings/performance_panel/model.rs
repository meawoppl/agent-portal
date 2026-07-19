//! View-model helpers for the Performance settings panel.

use chrono::{DateTime, Utc};
use shared::api::MetricBucket;
use shared::AgentType;

/// (agent_type, model, service_tier) tuple used as the group-by key. Codex
/// currently reports no model or tier; keeping the agent in the key lets the
/// UI label that shape explicitly without colliding with missing Claude
/// metadata.
pub(super) type GroupKey = (AgentType, Option<String>, Option<String>);

/// Pure helper: list the distinct (agent, model, tier) groups present in the
/// bucket list.
pub(super) fn distinct_pairs(buckets: &[MetricBucket]) -> Vec<GroupKey> {
    let mut seen: std::collections::BTreeSet<GroupKey> = std::collections::BTreeSet::new();
    for b in buckets {
        seen.insert(bucket_group_key(b));
    }
    seen.into_iter().collect()
}

pub(super) fn bucket_group_key(bucket: &MetricBucket) -> GroupKey {
    (
        bucket.agent_type,
        bucket.model.clone(),
        bucket.service_tier.clone(),
    )
}

/// Format an (agent, model, tier) group as a human-readable label for the
/// dropdown and legend.
///
/// Deliberately not `turn_metrics_pill::format_model_tier_label`: this page
/// shows the full model id (no vendor-prefix shortening), keeps the tier's
/// original case, and adds codex / agent-without-model handling.
pub(super) fn pair_label(pair: &GroupKey) -> String {
    let base = match (pair.0, pair.1.as_deref()) {
        (AgentType::Codex, None) => "Codex".to_string(),
        (_, Some(model)) => model.to_string(),
        (agent, None) => format!("{agent} unknown"),
    };
    match pair.2.as_deref() {
        Some(t) if !t.is_empty() && !t.eq_ignore_ascii_case("standard") => {
            format!("{base} {t}")
        }
        _ => base,
    }
}

/// Pick a stable color from the Tokyo-Night palette. We cycle through a
/// fixed palette by pair-index so the same pair always gets the same color
/// across re-renders.
pub(super) fn pair_color(idx: usize) -> &'static str {
    const PALETTE: &[&str] = &[
        "#7aa2f7", // accent blue
        "#bb9af7", // purple
        "#9ece6a", // green
        "#e0af68", // yellow
        "#f7768e", // red (used by max_tokens band)
        "#7dcfff", // cyan
        "#ff9e64", // orange
    ];
    PALETTE[idx % PALETTE.len()]
}

/// Build distinct bucket-start timestamps (the x-axis) preserving order.
pub(super) fn distinct_bucket_starts(buckets: &[MetricBucket]) -> Vec<DateTime<Utc>> {
    let mut seen: std::collections::BTreeSet<DateTime<Utc>> = std::collections::BTreeSet::new();
    for b in buckets {
        seen.insert(b.bucket_start);
    }
    seen.into_iter().collect()
}

/// Index a bucket-start timestamp to its position in the x-axis, returning
/// `None` if missing.
pub(super) fn bucket_index(buckets: &[DateTime<Utc>], ts: DateTime<Utc>) -> Option<usize> {
    buckets.iter().position(|b| *b == ts)
}

/// Build the bucket-granularity query string for the selected window.
///
/// Bucket width is chosen so each bucket holds *enough turns* for its
/// percentile/rate aggregates to be stable — the dashboard is for spotting
/// trends, not per-minute shape. The old very-fine widths (1m over 6h = 360
/// buckets, hourly over 90d = 2160) put 1-2 turns in most buckets, so each
/// "p50/p95" was effectively a single turn's value and the lines were pure
/// spike noise. These coarser widths trade temporal resolution for readable
/// trends; the moving average in `series.rs` smooths whatever remains.
pub(super) fn bucket_param(window: TimeWindow) -> &'static str {
    match window {
        TimeWindow::Hours1 => "5m",  // 12 buckets
        TimeWindow::Hours6 => "15m", // 24 buckets
        TimeWindow::Days1 => "15m",  // 96 buckets
        TimeWindow::Days7 => "hour", // 168 buckets
        TimeWindow::Days30 => "day", // 30 buckets
        TimeWindow::Days90 => "day", // 90 buckets
    }
}

/// Selectable time window for the radio group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TimeWindow {
    Hours1,
    Hours6,
    Days1,
    Days7,
    Days30,
    Days90,
}

impl TimeWindow {
    /// Radio-button label, which doubles as the exact wire value sent to
    /// `GET /api/metrics/turns?window=…` (the backend's window parser
    /// accepts the same `Nh` / `Nd` suffix form).
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Hours1 => "1h",
            Self::Hours6 => "6h",
            Self::Days1 => "1d",
            Self::Days7 => "7d",
            Self::Days30 => "30d",
            Self::Days90 => "90d",
        }
    }

    pub(super) fn all() -> &'static [TimeWindow] {
        &[
            TimeWindow::Hours1,
            TimeWindow::Hours6,
            TimeWindow::Days1,
            TimeWindow::Days7,
            TimeWindow::Days30,
            TimeWindow::Days90,
        ]
    }
}

/// Group-by selection: either a specific (agent, model, tier) group or "All".
#[derive(Debug, Clone, PartialEq)]
pub(super) enum GroupBy {
    All,
    Pair(GroupKey),
}

impl GroupBy {
    /// Serialize to a stable string for the `<select>` `value` attribute.
    pub(super) fn key(&self) -> String {
        match self {
            Self::All => "__ALL__".to_string(),
            Self::Pair((agent, m, t)) => format!(
                "{}|{}|{}",
                agent,
                m.as_deref().unwrap_or(""),
                t.as_deref().unwrap_or("")
            ),
        }
    }

    /// Inverse of [`key`]. Returns `GroupBy::All` for an unrecognized key.
    pub(super) fn from_key(key: &str, pairs: &[GroupKey]) -> Self {
        if key == "__ALL__" {
            return Self::All;
        }
        for p in pairs {
            if Self::Pair(p.clone()).key() == key {
                return Self::Pair(p.clone());
            }
        }
        Self::All
    }
}
