//! Hook for managing the client WebSocket connection with spend updates.

use crate::utils;
use shared::{ClientEndpoint, ServerToClient, WsEndpoint};
use wasm_bindgen_futures::spawn_local;
use yew::prelude::*;

/// Return value from the use_client_websocket hook.
pub struct UseClientWebSocket {
    /// Total user spend across all sessions
    pub total_spend: f64,
    /// Server shutdown reason (if server is shutting down)
    pub shutdown_reason: Option<String>,
}

/// Calculate exponential backoff delay for reconnection attempts.
fn calculate_backoff(attempt: u32) -> u32 {
    const INITIAL_MS: u32 = 1000;
    const MAX_MS: u32 = 30000;
    INITIAL_MS
        .saturating_mul(2u32.saturating_pow(attempt.min(5)))
        .min(MAX_MS)
}

/// Hook for managing the client WebSocket connection.
///
/// Connects to the client WebSocket endpoint and receives spend updates and server shutdown notifications.
/// Automatically reconnects with exponential backoff on disconnection.
///
/// # Returns
/// * `UseClientWebSocket` - The current spend data and shutdown status
///
#[hook]
pub fn use_client_websocket() -> UseClientWebSocket {
    let total_spend = use_state(|| 0.0f64);
    let shutdown_reason = use_state(|| None::<String>);

    {
        let total_spend = total_spend.clone();
        let shutdown_reason = shutdown_reason.clone();

        use_effect_with((), move |_| {
            let total_spend = total_spend.clone();
            let shutdown_reason = shutdown_reason.clone();

            spawn_local(async move {
                let mut attempt: u32 = 0;
                const MAX_ATTEMPTS: u32 = 10;

                loop {
                    let ws_endpoint = utils::ws_url(ClientEndpoint::PATH);
                    match ws_bridge::yew_client::connect_to::<ClientEndpoint>(&ws_endpoint) {
                        Ok(conn) => {
                            attempt = 0; // Reset on successful connection
                            let (_sender, mut receiver) = conn.split();

                            while let Some(result) = receiver.recv().await {
                                match result {
                                    Ok(msg) => match msg {
                                        ServerToClient::UserSpendUpdate {
                                            total_spend_usd,
                                            session_costs: _,
                                        } => {
                                            total_spend.set(total_spend_usd);
                                            shutdown_reason.set(None);
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
                                        _ => {
                                            shutdown_reason.set(None);
                                        }
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
    }
}
