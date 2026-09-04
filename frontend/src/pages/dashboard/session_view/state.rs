//! Pure state helpers for `SessionView` buffers.
//!
//! These helpers keep retention and turn-metric ordering rules out of the
//! component event loop. They mutate only the buffer passed to them, which
//! makes the invariants easy to unit-test without mounting Yew.

use shared::TurnMetrics;

/// Trim a chronological buffer to at most `max_cost` counted items.
///
/// Items for which `counts_toward_limit` is false remain alongside the newest
/// counted tail without displacing it. Free items older than that tail are
/// discarded too, preventing detached metadata from growing without bound.
pub(super) fn retain_newest_items_by_cost<T>(
    items: &mut Vec<T>,
    max_cost: usize,
    counts_toward_limit: impl Fn(&T) -> bool,
) {
    let excess = items
        .iter()
        .filter(|item| counts_toward_limit(item))
        .count()
        .saturating_sub(max_cost);
    if excess == 0 {
        return;
    }

    let mut counted = 0;
    let keep_from = items
        .iter()
        .position(|item| {
            if counts_toward_limit(item) {
                counted += 1;
            }
            counted == excess
        })
        .map_or(items.len(), |index| index + 1);
    items.drain(0..keep_from);
}

/// Append one live message and apply the same retention rule as history
/// hydration and replay batches.
pub(super) fn push_message_with_cost_limit<T>(
    messages: &mut Vec<T>,
    message: T,
    max_cost: usize,
    counts_toward_limit: impl Fn(&T) -> bool,
) {
    messages.push(message);
    retain_newest_items_by_cost(messages, max_cost, counts_toward_limit);
}

/// Claude emits many bodyless cumulative thinking-token markers during one
/// turn. They render as one compact chip and therefore cost nothing against
/// the live DOM budget.
pub(super) fn counts_toward_render_limit(content: &str) -> bool {
    !matches!(
        serde_json::from_str::<shared::ClaudeOutput>(content),
        Ok(shared::ClaudeOutput::System(message)) if message.is_thinking_tokens()
    )
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
    use crate::test_fixtures::TurnMetricsBuilder;
    use uuid::Uuid;

    fn metric(id: Option<Uuid>, started_secs: i64, output_tokens: i64) -> TurnMetrics {
        TurnMetricsBuilder::new()
            .id(id)
            .started_secs(started_secs)
            .output_tokens(output_tokens)
            .model(None)
            .service_tier(None)
            .ttft_ms(None)
            .generation_duration_ms(None)
            .max_gap_ms(None)
            .input_tokens(0)
            .cache_creation(0)
            .cache_read(0)
            .total_cost_usd(None)
            .stop_reason(None)
            .build()
    }

    #[test]
    fn retain_newest_items_keeps_counted_tail() {
        let mut messages = vec!["old".to_string(), "middle".to_string(), "new".to_string()];

        retain_newest_items_by_cost(&mut messages, 2, |_| true);

        assert_eq!(messages, vec!["middle", "new"]);
    }

    #[test]
    fn retain_newest_items_noops_when_within_limit() {
        let mut messages = vec!["one".to_string(), "two".to_string()];

        retain_newest_items_by_cost(&mut messages, 2, |_| true);

        assert_eq!(messages, vec!["one", "two"]);
    }

    #[test]
    fn push_message_with_limit_appends_then_trims_oldest() {
        let mut messages = vec!["one".to_string(), "two".to_string()];

        push_message_with_cost_limit(&mut messages, "three".to_string(), 2, |_| true);

        assert_eq!(messages, vec!["two", "three"]);
    }

    #[test]
    fn free_items_do_not_displace_counted_history() {
        let mut messages = vec!["old", "free-a", "middle", "free-b", "new"];

        retain_newest_items_by_cost(&mut messages, 2, |message| !message.starts_with("free"));

        assert_eq!(messages, vec!["free-a", "middle", "free-b", "new"]);
    }

    #[test]
    fn free_items_before_the_retained_tail_are_discarded() {
        let mut messages = vec!["orphan-free", "old", "middle", "new"];

        retain_newest_items_by_cost(&mut messages, 2, |message| !message.ends_with("free"));

        assert_eq!(messages, vec!["middle", "new"]);
    }

    #[test]
    fn only_claude_thinking_token_markers_are_free() {
        let thinking = r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":150,"estimated_tokens_delta":50,"session_id":"01890000-0000-7000-8000-000000000001","uuid":"01890000-0000-7000-8000-000000000002"}"#;

        assert!(!counts_toward_render_limit(thinking));
        assert!(counts_toward_render_limit(
            r#"{"type":"system","subtype":"init","session_id":"s"}"#
        ));
        assert!(counts_toward_render_limit("not json"));
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
