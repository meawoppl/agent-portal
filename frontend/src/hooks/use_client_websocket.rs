//! Hook for managing the client WebSocket connection.

#[path = "client_websocket_events.rs"]
mod client_websocket_events;

use crate::federation::Federation;
use crate::portal_client::{use_portal, ConnId, SessionKey};
use crate::utils::{calculate_backoff, parse_version, On401};
use client_websocket_events::handle_server_message;
use shared::api::TurnMetricsResponse;
use shared::{AppConfig, ClientEndpoint, ServiceStats, TurnMetrics};
use std::collections::HashMap;
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
    pub latest_session_metrics: HashMap<SessionKey, TurnMetrics>,
    /// Per-session completion fraction of the agent's progress bars, pushed
    /// live by `ServerToClient::SessionProgress`; drives the rail pill fill.
    pub session_progress: HashMap<SessionKey, f32>,
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
    /// Services monitor table (backend plus the user's launchers), from the
    /// newest `ServerToClient::ServiceStatsUpdate`.
    pub service_stats: Vec<ServiceStats>,
    /// Host CPU history per service id for the header sparkline.
    pub service_cpu_history: HashMap<String, Vec<f64>>,
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

    let home = use_portal();
    let registry = use_context::<Federation>();
    let mut portals = vec![home];
    if let Some(registry) = &registry {
        portals.extend(registry.rows.iter().filter_map(|r| r.client.clone()));
    }
    {
        let shutdown_reason = shutdown_reason.clone();
        let update_available = update_available.clone();
        let live = live.clone();
        use_effect_with(portals.clone(), move |portals| {
            let mut aborts = Vec::new();
            for portal in portals.clone() {
                let (shutdown_reason, update_available, live) = (
                    shutdown_reason.clone(),
                    update_available.clone(),
                    live.clone(),
                );
                let (abort, registration) = futures_util::future::AbortHandle::new_pair();
                aborts.push(abort);
                spawn_local(async move {
                    let _ = futures_util::future::Abortable::new(
                        async move {
                            if let Ok(parsed) = portal
                                .fetch_json::<TurnMetricsResponse>(
                                    "/api/metrics/recent",
                                    On401::Ignore,
                                )
                                .await
                            {
                                live.dispatch(if portal.conn() == ConnId::HOME {
                                    client_websocket_events::ClientWsAction::Hydrate {
                                        trend: parsed.metrics,
                                        latest: parsed.latest_by_session,
                                    }
                                } else {
                                    client_websocket_events::ClientWsAction::LinkedHydrate(
                                        portal.conn(),
                                        parsed.latest_by_session,
                                    )
                                });
                            }
                            let mut attempt = 0;
                            let mut known_version: Option<String> = None;
                            loop {
                                match portal.socket_url().await {
                                    Ok(endpoint) => match ws_bridge::yew_client::connect_to::<
                                        ClientEndpoint,
                                    >(
                                        &endpoint
                                    ) {
                                        Ok(conn) => {
                                            attempt = 0;
                                            if portal.conn() == ConnId::HOME {
                                                shutdown_reason.set(None);
                                                if let Ok(config) = portal
                                                    .fetch_json::<AppConfig>(
                                                        "/api/config",
                                                        On401::Ignore,
                                                    )
                                                    .await
                                                {
                                                    if known_version.as_ref().is_some_and(|old| {
                                                        is_newer_version(
                                                            old,
                                                            &config.server_version,
                                                        )
                                                    }) {
                                                        update_available.set(Some(
                                                            config.server_version.clone(),
                                                        ));
                                                    }
                                                    known_version = Some(config.server_version);
                                                }
                                            }
                                            let (_sender, mut receiver) = conn.split();
                                            while let Some(result) = receiver.recv().await {
                                                match result {
                                                    Ok(msg) => handle_server_message(
                                                        portal.conn(),
                                                        msg,
                                                        &shutdown_reason,
                                                        &live,
                                                    ),
                                                    Err(_) => break,
                                                }
                                            }
                                        }
                                        Err(_) => log::warn!(
                                            "Portal user channel unavailable: {}",
                                            portal.origin()
                                        ),
                                    },
                                    Err(_) => log::warn!(
                                        "Portal user channel authorization failed: {}",
                                        portal.origin()
                                    ),
                                }
                                if attempt >= 10 {
                                    break;
                                }
                                let delay = calculate_backoff(attempt);
                                attempt += 1;
                                gloo::timers::future::TimeoutFuture::new(delay).await;
                            }
                        },
                        registration,
                    )
                    .await;
                });
            }
            // Abort drops socket halves, pending fetches and reconnect timers.
            // Removing a connection must never leave an old task dispatching.
            move || {
                for abort in aborts {
                    abort.abort();
                }
            }
        });
    }
    let current: std::collections::HashSet<_> = portals.iter().map(|p| p.conn()).collect();

    UseClientWebSocket {
        shutdown_reason: (*shutdown_reason).clone(),
        update_available: (*update_available).clone(),
        recent_turn_metrics: live.recent_turn_metrics.clone(),
        latest_session_metrics: live
            .latest_session_metrics
            .iter()
            .filter(|(key, _)| current.contains(&key.conn))
            .map(|(key, value)| (*key, value.clone()))
            .collect(),
        session_progress: live
            .session_progress
            .iter()
            .filter(|(key, _)| current.contains(&key.conn))
            .map(|(key, value)| (*key, *value))
            .collect(),
        launch_event_counter: live.launch_event_counter,
        launcher_event_counter: live.launcher_event_counter,
        service_stats: live.service_stats.clone(),
        service_cpu_history: live
            .service_cpu_history
            .iter()
            .map(|(id, points)| (id.clone(), points.iter().map(|(_, v)| *v).collect()))
            .collect(),
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
