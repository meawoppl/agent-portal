use shared::{ServerToClient, TurnMetrics};
use yew::UseStateHandle;

/// Cap for the dashboard's recent-turn ring buffer. Matches the server-side
/// `RECENT_TURN_LIMIT` window: REST hydration returns at most this many, and
/// the live WS path trims the buffer back to this length after every
/// insertion so a long-lived dashboard session can't grow unboundedly.
pub const RECENT_TURN_BUFFER_CAP: usize = 50;

pub(crate) fn handle_server_message(
    msg: ServerToClient,
    total_spend: &UseStateHandle<f64>,
    shutdown_reason: &UseStateHandle<Option<String>>,
    recent_turn_metrics: &UseStateHandle<Vec<TurnMetrics>>,
    launch_event_counter: &UseStateHandle<u32>,
) {
    match msg {
        ServerToClient::UserSpendUpdate {
            total_spend_usd,
            session_costs: _,
        } => {
            total_spend.set(total_spend_usd);
        }
        ServerToClient::ServerShutdown {
            reason,
            reconnect_delay_ms,
        } => {
            log::info!(
                "Server shutdown: {} (reconnect in {}ms)",
                reason,
                reconnect_delay_ms
            );
            shutdown_reason.set(Some(reason));
        }
        ServerToClient::TurnMetrics(metrics) => {
            let next = insert_recent_metric(
                (**recent_turn_metrics).clone(),
                *metrics,
                RECENT_TURN_BUFFER_CAP,
            );
            recent_turn_metrics.set(next);
        }
        ServerToClient::LaunchSessionResult { success, error, .. } => {
            // Push signal from the backend that the launcher finished registering
            // (or failed). Tick the counter so the dashboard refreshes its session
            // list at the exact moment the new row becomes findable.
            if !success {
                log::warn!(
                    "Launch failed: {}",
                    error.as_deref().unwrap_or("(no detail)")
                );
            }
            launch_event_counter.set(launch_event_counter.wrapping_add(1));
        }
        _ => {}
    }
}

/// Insert a new metrics row into a sorted-by-`started_at`-ASC buffer,
/// deduping on `metrics.id` (so a REST-hydrated row plus a live broadcast for
/// the same id collapse into one entry), and trim back to the cap on the
/// oldest side. Pure helper so the recent-buffer logic is unit-testable
/// without spinning up a WebSocket. Returns the new buffer.
pub(crate) fn insert_recent_metric(
    mut buf: Vec<TurnMetrics>,
    incoming: TurnMetrics,
    cap: usize,
) -> Vec<TurnMetrics> {
    // Dedup on id when both sides have one. Rows that come off the WS
    // broadcast always carry the server-assigned id; REST-hydrated rows
    // always carry it too — proxy-emit rows (which have `id == None`) only
    // exist on the proxy → backend side and never reach the frontend.
    if let Some(incoming_id) = incoming.id {
        if let Some(existing) = buf.iter_mut().find(|m| m.id == Some(incoming_id)) {
            *existing = incoming;
            return buf;
        }
    }
    // Insert sorted by `started_at` ASC. binary_search_by keeps the buffer
    // ordered without a full re-sort on every insertion.
    let idx = buf
        .binary_search_by(|m| m.started_at.cmp(&incoming.started_at))
        .unwrap_or_else(|e| e);
    buf.insert(idx, incoming);
    if buf.len() > cap {
        // Drop the oldest entries; the sparkline plots the newest window.
        let excess = buf.len() - cap;
        buf.drain(0..excess);
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use uuid::Uuid;

    /// Build a minimal `TurnMetrics` with the fields the recent-buffer
    /// insertion path actually reads (`id`, `started_at`).
    fn sample(id: Option<Uuid>, started_secs: i64) -> TurnMetrics {
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
            output_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            thinking_tokens: 0,
            stop_reason: None,
            is_error: false,
            tool_call_count: 0,
            stream_restarts: 0,
            total_cost_usd: None,
        }
    }

    #[test]
    fn insert_into_empty_buffer() {
        let id = Uuid::new_v4();
        let buf = insert_recent_metric(Vec::new(), sample(Some(id), 100), 50);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf[0].id, Some(id));
    }

    #[test]
    fn insert_keeps_ascending_order() {
        let mut buf = Vec::new();
        buf = insert_recent_metric(buf, sample(Some(Uuid::new_v4()), 200), 50);
        buf = insert_recent_metric(buf, sample(Some(Uuid::new_v4()), 100), 50);
        buf = insert_recent_metric(buf, sample(Some(Uuid::new_v4()), 150), 50);
        let secs: Vec<i64> = buf.iter().map(|m| m.started_at.timestamp()).collect();
        assert_eq!(secs, vec![100, 150, 200]);
    }

    #[test]
    fn dedup_on_id_collapses_repeat_broadcast() {
        // A REST-hydrated row followed by a live WS broadcast for the same
        // id should produce a single buffer entry — the live row replaces
        // the REST one in place. This guards against the row count drifting
        // upward on every reconnect.
        let id = Uuid::new_v4();
        let buf = Vec::new();
        let buf = insert_recent_metric(buf, sample(Some(id), 100), 50);
        let buf = insert_recent_metric(buf, sample(Some(id), 100), 50);
        assert_eq!(buf.len(), 1);
    }

    #[test]
    fn cap_drops_oldest_when_exceeded() {
        // Insert 5 entries with a cap of 3 — only the newest 3 survive.
        let mut buf = Vec::new();
        for t in [10, 20, 30, 40, 50] {
            buf = insert_recent_metric(buf, sample(Some(Uuid::new_v4()), t), 3);
        }
        assert_eq!(buf.len(), 3);
        let secs: Vec<i64> = buf.iter().map(|m| m.started_at.timestamp()).collect();
        assert_eq!(secs, vec![30, 40, 50]);
    }
}
