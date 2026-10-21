//! Hook for managing the client WebSocket connection.

#[path = "client_websocket_events.rs"]
mod client_websocket_events;

use crate::federation::{Federation, FederationAction};
use crate::portal_client::{use_portal, ConnId, SessionKey};
use crate::utils::{calculate_backoff, parse_version, On401};
use client_websocket_events::handle_server_message;
use shared::api::TurnMetricsResponse;
use shared::{ServiceStats, TurnMetrics};
use std::collections::HashMap;
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Return value from the use_client_websocket hook.
#[derive(Clone, Default, PartialEq)]
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

struct HomeRpcGuard(Option<u64>);
impl Drop for HomeRpcGuard {
    fn drop(&mut self) {
        if let Some(generation) = self.0 {
            crate::dashboard_rpc::disconnect(generation);
        }
    }
}

#[derive(Properties, PartialEq)]
pub struct ProviderProps {
    pub children: Children,
}
#[function_component(ClientWebSocketProvider)]
pub fn provider(props: &ProviderProps) -> Html {
    let live = use_live_client_websocket();
    html! { <ContextProvider<UseClientWebSocket> context={live}>{props.children.clone()}</ContextProvider<UseClientWebSocket>> }
}

#[hook]
pub fn use_client_websocket() -> UseClientWebSocket {
    use_context::<UseClientWebSocket>().unwrap_or_default()
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
fn use_live_client_websocket() -> UseClientWebSocket {
    let shutdown_reason = use_state(|| None::<String>);
    let update_available = use_state(|| None::<String>);
    // Everything updated *relative to its previous value* lives in one reducer.
    // A `use_state` handle captured by the long-lived WS task below reads the
    // mount-time snapshot forever, so `read → modify → set` from that task
    // silently discards every update since mount. See `ClientWsState`.
    let live = use_reducer(client_websocket_events::ClientWsState::default);

    let home = use_portal();
    crate::dashboard_rpc::bind_account(home.account_id());
    let registry = use_context::<Federation>();
    let mut portals = if home.account_id().is_some() {
        vec![home]
    } else {
        Vec::new()
    };
    if let Some(registry) = &registry {
        portals.extend(registry.rows.iter().filter_map(|r| r.client.clone()));
    }
    let tasks = use_mut_ref(
        HashMap::<
            ConnId,
            (
                crate::portal_client::PortalClient,
                futures_util::future::AbortHandle,
            ),
        >::new,
    );
    {
        let tasks = tasks.clone();
        use_effect_with((), move |_| {
            move || {
                for (_, (_, abort)) in tasks.borrow_mut().drain() {
                    abort.abort();
                }
            }
        });
    }
    {
        let tasks = tasks.clone();
        let registry = registry.clone();
        let shutdown_reason = shutdown_reason.clone();
        let update_available = update_available.clone();
        let live = live.clone();
        use_effect_with(portals.clone(), move |portals| {
            // A new linked account must not restart unrelated sockets or
            // reset home server-version tracking.
            tasks.borrow_mut().retain(|_, (client, abort)| {
                if portals.contains(client) {
                    true
                } else {
                    abort.abort();
                    false
                }
            });
            for portal in portals.clone() {
                if tasks.borrow().contains_key(&portal.conn()) {
                    continue;
                }
                let registry = registry.clone();
                let (shutdown_reason, update_available, live) = (
                    shutdown_reason.clone(),
                    update_available.clone(),
                    live.clone(),
                );
                let (abort, registration) = futures_util::future::AbortHandle::new_pair();
                tasks
                    .borrow_mut()
                    .insert(portal.conn(), (portal.clone(), abort));
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
                                if portal.is_linked() {
                                    live.dispatch(client_websocket_events::ClientWsAction::SessionProgressReset(portal.conn()));
                                    if let Some(registry) = &registry {
                                        registry.dispatch(FederationAction::LiveStatus(portal.conn(), Some("Connecting live updates".into())));
                                    }
                                }
                                match crate::browser_protocol::connect(&portal, |msg| {
                                    handle_server_message(portal.conn(), msg, &shutdown_reason, &live);
                                }).await {
                                        Ok((conn, config)) => {
                                            if portal.conn() == ConnId::HOME {
                                                shutdown_reason.set(None);
                                                if known_version.as_ref().is_some_and(|old| is_newer_version(old, &config.server_version)) {
                                                    update_available.set(Some(config.server_version.clone()));
                                                }
                                                known_version = Some(config.server_version);
                                            }
                                            let (mut sender, mut receiver) = conn.split();
                                            let (requests, mut requests_rx) = futures_channel::mpsc::unbounded();
                                            let _rpc_guard = HomeRpcGuard((portal.conn() == ConnId::HOME).then(|| crate::dashboard_rpc::connect(requests.clone())));
                                            let _requests_keepalive = requests;
                                            let mut announced = false;
                                            loop {
                                                use futures_util::{FutureExt, StreamExt};
                                                let result = futures_util::select! {
                                                    message = requests_rx.next().fuse() => {
                                                        if let Some(message) = message {
                                                            if sender.send(message).await.is_err() { break; }
                                                        }
                                                        continue;
                                                    },
                                                    result = receiver.recv().fuse() => {
                                                        let Some(result) = result else { break; };
                                                        result
                                                    }
                                                };
                                                match result {
                                                    Ok(msg) => {
                                                        if !announced {
                                                            announced = true;
                                                            attempt = 0;
                                                            if portal.conn() == ConnId::HOME {
                                                                // Refresh read models after reconnect; never replay mutations.
                                                                live.dispatch(client_websocket_events::ClientWsAction::LauncherEvent);
                                                            }
                                                            if let Some(registry) = &registry {
                                                                registry.dispatch(FederationAction::LiveStatus(portal.conn(), None));
                                                            }
                                                        }
                                                        if let shared::ServerToClient::DashboardResult { request_id, result } = msg {
                                                            if portal.conn() == ConnId::HOME { crate::dashboard_rpc::resolve(request_id, result); }
                                                        } else {
                                                            handle_server_message(portal.conn(), msg, &shutdown_reason, &live);
                                                        }
                                                    },
                                                    Err(_) => break,
                                                }
                                            }
                                        }
                                        Err(error) => {
                                            if portal.conn() == ConnId::HOME {
                                                shutdown_reason.set(Some(error.clone()));
                                            }
                                            if let Some(registry) = &registry {
                                                registry.dispatch(FederationAction::LiveStatus(portal.conn(), Some(error)));
                                            }
                                            // No sender was exposed; do not replay mutations or
                                            // turn a failed handshake into a legacy connection.
                                            break;
                                        }
                                }
                                if portal.is_linked() {
                                    live.dispatch(client_websocket_events::ClientWsAction::SessionProgressReset(portal.conn()));
                                    if let Some(registry) = &registry {
                                        registry.dispatch(FederationAction::LiveStatus(portal.conn(), Some("Reconnecting live updates".into())));
                                    }
                                }
                                if attempt >= 10 {
                                    if let Some(registry) = &registry {
                                        registry.dispatch(FederationAction::LiveStatus(portal.conn(), Some("Live updates unavailable; unlink and authorize again".into())));
                                    }
                                    live.dispatch(client_websocket_events::ClientWsAction::SessionProgressReset(portal.conn()));
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
            // Per-connection removal above and unmount cleanup own aborts.
            || {}
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
