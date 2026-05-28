//! Hook for managing the client WebSocket connection with spend updates.

use crate::utils;
use gloo_net::http::Request;
use shared::api::TurnMetricsResponse;
use shared::{AppConfig, ClientEndpoint, ServerToClient, TurnMetrics, WsEndpoint};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Cap for the dashboard's recent-turn ring buffer. Matches the server-side
/// `RECENT_TURN_LIMIT` window: REST hydration returns at most this many, and
/// the live WS path trims the buffer back to this length after every
/// insertion so a long-lived dashboard session can't grow unboundedly.
pub const RECENT_TURN_BUFFER_CAP: usize = 50;

/// Return value from the use_client_websocket hook.
pub struct UseClientWebSocket {
    /// Total user spend across all sessions
    pub total_spend: f64,
    /// Server shutdown reason (if server is shutting down).
    /// Cleared automatically on the next successful (re)connect.
    pub shutdown_reason: Option<String>,
    /// Newer server version detected after a reconnect. When set, the UI
    /// should prompt the user to reload so the JS/WASM bundle catches up
    /// with the backend they just reconnected to. Once set, stays set
    /// until the user reloads.
    pub update_available: Option<String>,
    /// The user's most recent N turn-metrics rows across all of their
    /// sessions, ordered `started_at` ASC (oldest → newest). Seeded once on
    /// hook mount from `GET /api/metrics/recent` and then kept fresh by
    /// `ServerToClient::TurnMetrics` WS frames — the backend fans the
    /// per-session broadcast out to each session member's user channel so
    /// this buffer ticks live without the dashboard having to subscribe to
    /// individual session WS streams. Capped at [`RECENT_TURN_BUFFER_CAP`].
    pub recent_turn_metrics: Vec<TurnMetrics>,
    /// Monotonic counter that ticks every time the backend broadcasts a
    /// `ServerToClient::LaunchSessionResult` frame (proxy registered, or
    /// the launch failed). Consumers can hang a `use_effect_with` on this
    /// value to fire a single `/api/sessions` refresh at the exact moment
    /// the new session becomes findable — no polling burst needed. The
    /// value itself is opaque; only its *change* is meaningful.
    pub launch_event_counter: u32,
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

/// Calculate exponential backoff delay for reconnection attempts.
fn calculate_backoff(attempt: u32) -> u32 {
    const INITIAL_MS: u32 = 1000;
    const MAX_MS: u32 = 30000;
    INITIAL_MS
        .saturating_mul(2u32.saturating_pow(attempt.min(5)))
        .min(MAX_MS)
}

/// Parse a semver-ish "MAJOR.MINOR.PATCH" string into a comparable tuple.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Decide whether `next` is strictly newer than `prev`. Falls back to string
/// inequality when either side fails to parse (better to over-prompt than miss
/// an upgrade because of a non-semver tag).
fn is_newer_version(prev: &str, next: &str) -> bool {
    match (parse_version(prev), parse_version(next)) {
        (Some(p), Some(n)) => n > p,
        _ => prev != next,
    }
}

/// Hook for managing the client WebSocket connection.
///
/// Connects to the client WebSocket endpoint and receives spend updates and
/// server shutdown notifications. Automatically reconnects with exponential
/// backoff on disconnection; on every successful (re)connect the hook
/// refetches `/api/config` and surfaces an `update_available` signal when the
/// backend's `server_version` advances past what the client booted with.
///
/// # Returns
/// * `UseClientWebSocket` - The current spend data, shutdown status, and
///   pending-update version.
#[hook]
pub fn use_client_websocket() -> UseClientWebSocket {
    let total_spend = use_state(|| 0.0f64);
    let shutdown_reason = use_state(|| None::<String>);
    let update_available = use_state(|| None::<String>);
    let recent_turn_metrics = use_state(Vec::<TurnMetrics>::new);
    let launch_event_counter = use_state(|| 0u32);

    // One-shot REST hydration on hook mount. Fires alongside (not gated on)
    // the WS connect — the dashboard pill shows immediately if the user
    // has any prior turns, and the WS path keeps it fresh once it lands.
    {
        let recent_turn_metrics = recent_turn_metrics.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                let url = utils::api_url("/api/metrics/recent");
                if let Ok(resp) = Request::get(&url).send().await {
                    if resp.ok() {
                        if let Ok(parsed) = resp.json::<TurnMetricsResponse>().await {
                            recent_turn_metrics.set(parsed.metrics);
                        }
                    }
                }
            });
            || ()
        });
    }

    {
        let total_spend = total_spend.clone();
        let shutdown_reason = shutdown_reason.clone();
        let update_available = update_available.clone();
        let recent_turn_metrics = recent_turn_metrics.clone();
        let launch_event_counter = launch_event_counter.clone();

        use_effect_with((), move |_| {
            let total_spend = total_spend.clone();
            let shutdown_reason = shutdown_reason.clone();
            let update_available = update_available.clone();
            let recent_turn_metrics = recent_turn_metrics.clone();
            let launch_event_counter = launch_event_counter.clone();

            spawn_local(async move {
                let mut attempt: u32 = 0;
                let mut known_version: Option<String> = None;
                const MAX_ATTEMPTS: u32 = 10;

                loop {
                    let ws_endpoint = utils::ws_url(ClientEndpoint::PATH);
                    match ws_bridge::yew_client::connect_to::<ClientEndpoint>(&ws_endpoint) {
                        Ok(conn) => {
                            attempt = 0; // Reset on successful connection
                                         // We just (re)connected — any prior shutdown banner is now stale.
                            shutdown_reason.set(None);

                            // Compare the server's reported version against what we last saw.
                            // If it advanced, surface a reload prompt; the user's JS/WASM
                            // bundle was built against the earlier backend and may be drifted.
                            let cfg_url = utils::api_url("/api/config");
                            if let Ok(resp) = Request::get(&cfg_url).send().await {
                                if let Ok(cfg) = resp.json::<AppConfig>().await {
                                    let next = cfg.server_version;
                                    match &known_version {
                                        None => {
                                            known_version = Some(next);
                                        }
                                        Some(prev) => {
                                            if is_newer_version(prev, &next) {
                                                update_available.set(Some(next.clone()));
                                            }
                                            known_version = Some(next);
                                        }
                                    }
                                }
                            }

                            let (_sender, mut receiver) = conn.split();

                            while let Some(result) = receiver.recv().await {
                                match result {
                                    Ok(msg) => match msg {
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
                                                (*recent_turn_metrics).clone(),
                                                *metrics,
                                                RECENT_TURN_BUFFER_CAP,
                                            );
                                            recent_turn_metrics.set(next);
                                        }
                                        ServerToClient::LaunchSessionResult {
                                            success,
                                            error,
                                            ..
                                        } => {
                                            // Push signal from the backend that the
                                            // launcher finished registering (or failed).
                                            // Tick the counter so the dashboard refreshes
                                            // its session list at the exact moment the
                                            // new row becomes findable, instead of
                                            // waiting for the next 5s steady-poll tick.
                                            if !success {
                                                log::warn!(
                                                    "Launch failed: {}",
                                                    error.as_deref().unwrap_or("(no detail)")
                                                );
                                            }
                                            launch_event_counter
                                                .set(launch_event_counter.wrapping_add(1));
                                        }
                                        _ => {}
                                    },
                                    Err(e) => {
                                        log::error!("Client WebSocket error: {:?}", e);
                                        break;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to connect client WebSocket: {:?}", e);
                        }
                    }

                    // Reconnection with exponential backoff
                    if attempt >= MAX_ATTEMPTS {
                        log::error!("Client WebSocket: max reconnection attempts reached");
                        break;
                    }
                    let delay_ms = calculate_backoff(attempt);
                    attempt += 1;
                    log::info!(
                        "Client WebSocket reconnecting in {}ms (attempt {})",
                        delay_ms,
                        attempt
                    );
                    gloo::timers::future::TimeoutFuture::new(delay_ms).await;
                }
            });
            || ()
        });
    }

    UseClientWebSocket {
        total_spend: *total_spend,
        shutdown_reason: (*shutdown_reason).clone(),
        update_available: (*update_available).clone(),
        recent_turn_metrics: (*recent_turn_metrics).clone(),
        launch_event_counter: *launch_event_counter,
    }
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

    #[test]
    fn newer_patch_detected() {
        assert!(is_newer_version("2.5.91", "2.5.92"));
    }

    #[test]
    fn newer_minor_detected() {
        assert!(is_newer_version("2.5.91", "2.6.0"));
    }

    #[test]
    fn newer_major_detected() {
        assert!(is_newer_version("2.99.99", "3.0.0"));
    }

    #[test]
    fn same_version_not_newer() {
        assert!(!is_newer_version("2.5.91", "2.5.91"));
    }

    #[test]
    fn older_version_not_newer() {
        assert!(!is_newer_version("2.5.92", "2.5.91"));
    }

    #[test]
    fn unparseable_falls_back_to_inequality() {
        // If either side isn't semver, we prompt on any change rather than miss
        // an upgrade.
        assert!(is_newer_version("dev", "2.5.92"));
        assert!(is_newer_version("2.5.91", "main-abc123"));
        assert!(!is_newer_version("dev", "dev"));
    }
}
