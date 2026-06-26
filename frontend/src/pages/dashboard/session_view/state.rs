//! Pure state helpers for `SessionView` buffers.
//!
//! These helpers keep retention and turn-metric ordering rules out of the
//! component event loop. They mutate only the buffer passed to them, which
//! makes the invariants easy to unit-test without mounting Yew.

use shared::TurnMetrics;

/// Trim a newest-first-retained message buffer to `max_len`.
///
/// `SessionView` appends messages in chronological order, so retaining the
/// tail keeps the newest rows and drops only old history.
pub(super) fn retain_newest_items<T>(items: &mut Vec<T>, max_len: usize) {
    if items.len() > max_len {
        let excess = items.len() - max_len;
        items.drain(0..excess);
    }
}

/// Append one live message and apply the same retention rule as history
/// hydration and replay batches.
pub(super) fn push_message_with_limit<T>(messages: &mut Vec<T>, message: T, max_len: usize) {
    messages.push(message);
    retain_newest_items(messages, max_len);
}

/// Insert one live `TurnMetrics` into the buffer, preserving `started_at ASC`
/// order and deduping by populated DB `id`.
///
/// Dedup matters because REST hydration and websocket broadcasts can deliver
/// the same row during reconnect. Rows with `None` ids are not deduped: today
/// live backend broadcasts have ids, but keeping `None` rows distinct avoids
/// collapsing future backfills before they are persisted.
pub(super) fn insert_turn_metrics_sorted(buffer: &mut Vec<TurnMetrics>, metrics: TurnMetrics) {
    if let Some(new_id) = metrics.id {
        if let Some(slot) = buffer.iter_mut().find(|m| m.id == Some(new_id)) {
            *slot = metrics;
            return;
        }
    }

    let pos = buffer
        .binary_search_by(|m| m.started_at.cmp(&metrics.started_at))
        .unwrap_or_else(|p| p);
    buffer.insert(pos, metrics);
}

/// Sort a hydrated metrics batch defensively before the view pairs the Nth
/// terminator card with the Nth metrics row.
pub(super) fn sort_turn_metrics_by_start(metrics: &mut [TurnMetrics]) {
    metrics.sort_by_key(|m| m.started_at);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    fn metric(id: Option<Uuid>, started_secs: i64, output_tokens: i64) -> TurnMetrics {
        TurnMetrics {
            id,
            session_id: Uuid::nil(),
            user_message_id: None,
            agent_type: "claude".to_string(),
            model: None,
            service_tier: None,
            started_at: Utc.timestamp_opt(started_secs, 0).unwrap(),
            first_token_at: None,
            completed_at: None,
            ttft_ms: None,
            total_duration_ms: None,
            generation_duration_ms: None,
            max_inter_token_gap_ms: None,
            input_tokens: 0,
            output_tokens,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            thinking_tokens: 0,
            subagent_tokens: 0,
            stop_reason: None,
            is_error: false,
            tool_call_count: 0,
            stream_restarts: 0,
            total_cost_usd: None,
        }
    }

    #[test]
    fn retain_newest_items_keeps_tail() {
        let mut messages = vec!["old".to_string(), "middle".to_string(), "new".to_string()];

        retain_newest_items(&mut messages, 2);

        assert_eq!(messages, vec!["middle", "new"]);
    }

    #[test]
    fn retain_newest_items_noops_when_within_limit() {
        let mut messages = vec!["one".to_string(), "two".to_string()];

        retain_newest_items(&mut messages, 2);

        assert_eq!(messages, vec!["one", "two"]);
    }

    #[test]
    fn push_message_with_limit_appends_then_trims_oldest() {
        let mut messages = vec!["one".to_string(), "two".to_string()];

        push_message_with_limit(&mut messages, "three".to_string(), 2);

        assert_eq!(messages, vec!["two", "three"]);
    }

    #[test]
    fn insert_turn_metrics_sorted_preserves_started_at_order() {
        let mut buffer = vec![metric(None, 20, 20), metric(None, 40, 40)];

        insert_turn_metrics_sorted(&mut buffer, metric(None, 30, 30));

        let starts: Vec<_> = buffer.iter().map(|m| m.started_at.timestamp()).collect();
        assert_eq!(starts, vec![20, 30, 40]);
    }

    #[test]
    fn insert_turn_metrics_sorted_replaces_matching_id() {
        let id = Uuid::new_v4();
        let mut buffer = vec![metric(Some(id), 20, 20)];

        insert_turn_metrics_sorted(&mut buffer, metric(Some(id), 30, 99));

        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer[0].started_at.timestamp(), 30);
        assert_eq!(buffer[0].output_tokens, 99);
    }

    #[test]
    fn insert_turn_metrics_sorted_keeps_none_id_rows_distinct() {
        let mut buffer = vec![metric(None, 20, 20)];

        insert_turn_metrics_sorted(&mut buffer, metric(None, 20, 99));

        assert_eq!(buffer.len(), 2);
        assert_eq!(
            buffer.iter().map(|m| m.output_tokens).collect::<Vec<_>>(),
            vec![99, 20]
        );
    }

    #[test]
    fn sort_turn_metrics_by_start_orders_hydrated_batch() {
        let mut metrics = vec![metric(None, 30, 30), metric(None, 10, 10)];

        sort_turn_metrics_by_start(&mut metrics);

        assert_eq!(
            metrics
                .iter()
                .map(|m| m.started_at.timestamp())
                .collect::<Vec<_>>(),
            vec![10, 30]
        );
    }
}
