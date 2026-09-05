//! Hook for managing the client WebSocket connection.

#[path = "client_websocket_events.rs"]
mod client_websocket_events;

use crate::utils::{self, On401};
use client_websocket_events::handle_server_message;
use shared::api::TurnMetricsResponse;
use shared::{AppConfig, ClientEndpoint, TurnMetrics, WsEndpoint};
use std::collections::HashMap;
use uuid::Uuid;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Return value from the use_client_websocket hook.
pub struct UseClientWebSocket {
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
    /// individual session WS streams. Capped by the websocket event helper.
    pub recent_turn_metrics: Vec<TurnMetrics>,
    /// Newest context-capable row per session, independent of the bounded
    /// trend ring. Context status must not disappear when another session is
    /// busy or emits a newer row without context metadata.
    pub latest_session_metrics: HashMap<Uuid, TurnMetrics>,
    /// Monotonic counter that ticks every time the backend broadcasts a
    /// `ServerToClient::LaunchSessionResult` frame (proxy registered, or
    /// the launch failed). Consumers can hang a `use_effect_with` on this
    /// value to fire a single `/api/sessions` refresh at the exact moment
    /// the new session becomes findable — no polling burst needed. The
    /// value itself is opaque; only its *change* is meaningful.
    pub launch_event_counter: u32,
    /// Ticks on every `ServerToClient::LaunchersChanged` broadcast — a
    /// launcher connected, disconnected, or was evicted. Consumers hang a
    /// `use_effect_with` on it to refetch `/api/launchers` (#710).
    pub launcher_event_counter: u32,
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
/// * `UseClientWebSocket` - Shutdown status, pending-update version, and
///   turn-metrics data.
#[hook]
pub fn use_client_websocket() -> UseClientWebSocket {
    let shutdown_reason = use_state(|| None::<String>);
    let update_available = use_state(|| None::<String>);
    // Everything updated *relative to its previous value* lives in one reducer.
    // A `use_state` handle captured by the long-lived WS task below reads the
    // mount-time snapshot forever, so `read → modify → set` from that task
    // silently discards every update since mount. See `ClientWsState`.
    let live = use_reducer(client_websocket_events::ClientWsState::default);

    // One-shot REST hydration on hook mount. Fires alongside (not gated on)
    // the WS connect — the dashboard pill shows immediately if the user
    // has any prior turns, and the WS path keeps it fresh once it lands.
    {
        let live = live.clone();
        use_effect_with((), move |_| {
            spawn_local(async move {
                if let Ok(parsed) =
                    utils::fetch_json::<TurnMetricsResponse>("/api/metrics/recent", On401::Ignore)
                        .await
                {
                    live.dispatch(client_websocket_events::ClientWsAction::Hydrate {
                        trend: parsed.metrics,
                        latest: parsed.latest_by_session,
                    });
                }
            });
            || ()
        });
    }

    {
        let shutdown_reason = shutdown_reason.clone();
        let update_available = update_available.clone();
        let live = live.clone();

        use_effect_with((), move |_| {
            let shutdown_reason = shutdown_reason.clone();
            let update_available = update_available.clone();
            let live = live.clone();

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
                            if let Ok(cfg) =
                                utils::fetch_json::<AppConfig>("/api/config", On401::Ignore).await
                            {
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

                            let (_sender, mut receiver) = conn.split();

                            while let Some(result) = receiver.recv().await {
                                match result {
                                    Ok(msg) => handle_server_message(msg, &shutdown_reason, &live),
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
        shutdown_reason: (*shutdown_reason).clone(),
        update_available: (*update_available).clone(),
        recent_turn_metrics: live.recent_turn_metrics.clone(),
        latest_session_metrics: live.latest_session_metrics.clone(),
        launch_event_counter: live.launch_event_counter,
        launcher_event_counter: live.launcher_event_counter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
