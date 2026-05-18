//! Hook for managing the client WebSocket connection with spend updates.

use crate::utils;
use gloo_net::http::Request;
use shared::{AppConfig, ClientEndpoint, ServerToClient, WsEndpoint};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

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

    {
        let total_spend = total_spend.clone();
        let shutdown_reason = shutdown_reason.clone();
        let update_available = update_available.clone();

        use_effect_with((), move |_| {
            let total_spend = total_spend.clone();
            let shutdown_reason = shutdown_reason.clone();
            let update_available = update_available.clone();

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
