//! Merge Codex's sparse rate-limit updates and decide when to forward one.
//!
//! Codex sends `account/rateLimits/updated` with every token count during a
//! turn. Its protocol documents the update as *sparse*: a field it omits is
//! unchanged, not empty, and clients should merge it into the last full
//! reading. Codex also reports several limits side by side, told apart by
//! `limit_id`. So the tracker keeps one merged snapshot per limit and forwards
//! it only the first time a usage window crosses the warning threshold —
//! Portal uses that to ask for a handoff summary, and forwarding every update
//! past the threshold would add a transcript row per model response.

use std::collections::{HashMap, HashSet};

use codex_codes::{RateLimitSnapshot, RateLimitWindow};
use shared::usage_limit::{codex_warning_window, CODEX_DEFAULT_LIMIT_ID};

#[derive(Debug, Default)]
pub(crate) struct RateLimitTracker {
    by_limit: HashMap<String, RateLimitSnapshot>,
    forwarded: HashSet<String>,
}

impl RateLimitTracker {
    /// Merge `update` into its limit's snapshot. Returns the merged snapshot
    /// when it is past the warning threshold for a window not yet forwarded.
    pub(crate) fn observe(&mut self, update: RateLimitSnapshot) -> Option<RateLimitSnapshot> {
        let limit = update
            .limit_id
            .clone()
            .unwrap_or_else(|| CODEX_DEFAULT_LIMIT_ID.to_string());
        let merged = merge_snapshot(self.by_limit.remove(&limit), update);
        self.by_limit.insert(limit, merged.clone());
        let key = codex_warning_window(&merged)?;
        self.forwarded.insert(key).then_some(merged)
    }
}

/// Every field is listed, with no `..`, so a field the SDK adds fails to
/// compile here instead of being silently dropped from merged snapshots.
fn merge_snapshot(
    previous: Option<RateLimitSnapshot>,
    update: RateLimitSnapshot,
) -> RateLimitSnapshot {
    let previous = previous.unwrap_or_default();
    RateLimitSnapshot {
        credits: update.credits.or(previous.credits),
        individual_limit: update.individual_limit.or(previous.individual_limit),
        limit_id: update.limit_id.or(previous.limit_id),
        limit_name: update.limit_name.or(previous.limit_name),
        normal_model_slug: update.normal_model_slug.or(previous.normal_model_slug),
        plan_type: update.plan_type.or(previous.plan_type),
        primary: merge_window(previous.primary, update.primary),
        rate_limit_reached_type: update
            .rate_limit_reached_type
            .or(previous.rate_limit_reached_type),
        secondary: merge_window(previous.secondary, update.secondary),
        spend_control_reached: update
            .spend_control_reached
            .or(previous.spend_control_reached),
    }
}

/// A reported window always carries its usage; its reset time and length
/// may be omitted and are then kept from the previous reading.
fn merge_window(
    previous: Option<RateLimitWindow>,
    update: Option<RateLimitWindow>,
) -> Option<RateLimitWindow> {
    let Some(update) = update else {
        return previous;
    };
    let previous = previous.unwrap_or_default();
    Some(RateLimitWindow {
        resets_at: update.resets_at.or(previous.resets_at),
        used_percent: update.used_percent,
        window_duration_mins: update
            .window_duration_mins
            .or(previous.window_duration_mins),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(value: serde_json::Value) -> RateLimitSnapshot {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn sparse_updates_keep_earlier_fields_and_forward_once_per_window() {
        let mut tracker = RateLimitTracker::default();
        assert_eq!(
            tracker.observe(snapshot(json!({
                "primary": {"usedPercent": 60, "resetsAt": 100, "windowDurationMins": 300},
                "planType": "pro"
            }))),
            None
        );
        // Crossing the threshold without repeating the reset time still keys
        // on the window seen earlier.
        let warned = tracker
            .observe(snapshot(json!({"primary": {"usedPercent": 91}})))
            .expect("first update past the threshold is forwarded");
        assert_eq!(
            codex_warning_window(&warned).as_deref(),
            Some("codex:codex:primary:100")
        );
        let primary = warned.primary.as_ref().unwrap();
        assert_eq!(
            (primary.resets_at, primary.window_duration_mins),
            (Some(100), Some(300))
        );
        assert!(warned.plan_type.is_some());

        // Further updates in the same window add nothing.
        assert_eq!(
            tracker.observe(snapshot(json!({"primary": {"usedPercent": 97}}))),
            None
        );
        // An update that omits the window keeps it rather than clearing it.
        assert_eq!(tracker.observe(snapshot(json!({"planType": "pro"}))), None);
        // A new window after the reset warns again.
        assert!(tracker
            .observe(snapshot(
                json!({"primary": {"usedPercent": 92, "resetsAt": 200}})
            ))
            .is_some());
    }

    #[test]
    fn limits_are_tracked_separately() {
        let mut tracker = RateLimitTracker::default();
        tracker.observe(snapshot(
            json!({"primary": {"usedPercent": 10, "resetsAt": 1}}),
        ));
        tracker.observe(snapshot(json!({
            "limitId": "codex_other",
            "primary": {"usedPercent": 10, "resetsAt": 5}
        })));
        let other = tracker
            .observe(snapshot(
                json!({"limitId": "codex_other", "primary": {"usedPercent": 95}}),
            ))
            .expect("the other limit warns on its own window");
        assert_eq!(
            codex_warning_window(&other).as_deref(),
            Some("codex:codex_other:primary:5")
        );
        // The default limit is still under the threshold.
        assert_eq!(
            tracker.observe(snapshot(json!({"primary": {"usedPercent": 20}}))),
            None
        );
    }
}
