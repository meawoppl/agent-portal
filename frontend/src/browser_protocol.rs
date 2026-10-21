//! Browser sockets must negotiate before callers can expose a mutation sender.
//! A successful config response without metadata is the sole legacy fallback;
//! silence, failed fetches and timed-out acks never authorize commands.
use crate::portal_client::PortalClient;
use crate::utils::On401;
use shared::{AppConfig, ClientEndpoint, ClientToServer, ServerToClient};

pub(crate) async fn connect(
    portal: &PortalClient,
    mut on_seed: impl FnMut(ServerToClient),
) -> Result<(ws_bridge::yew_client::Connection<ClientEndpoint>, AppConfig), String> {
    // Use the originating PortalClient, preserving linked-account auth headers.
    let config = portal.fetch_json::<AppConfig>("/api/config", On401::Ignore)
        .await.map_err(|e| format!("Could not check Portal compatibility: {e}. Reconnect after the server is available."))?;
    shared::protocol::check_protocol_version(config.protocol_version, "server")?;
    let endpoint = portal
        .socket_url()
        .await
        .map_err(|e| format!("Portal authorization failed: {e}"))?;
    let mut socket = ws_bridge::yew_client::connect_to::<ClientEndpoint>(&endpoint)
        .map_err(|e| format!("Portal connection failed: {e}"))?;
    if config.protocol_version.is_none() {
        return Ok((socket, config));
    }
    socket
        .send(ClientToServer::Hello {
            protocol_version: shared::protocol::PROTOCOL_VERSION,
        })
        .await
        .map_err(|e| format!("Could not negotiate Portal compatibility: {e}"))?;
    {
        let negotiation = async {
            while let Some(frame) = socket.recv().await {
                let frame = frame.map_err(|e| format!("Invalid Portal handshake: {e}"))?;
                if let ServerToClient::HelloAck {
                    protocol_version,
                    success,
                    error,
                } = frame
                {
                    shared::protocol::check_protocol_version(Some(protocol_version), "server")?;
                    return if success {
                        Ok(())
                    } else {
                        Err(error.unwrap_or_else(|| "Portal rejected compatibility negotiation. Update the browser and server to compatible releases.".into()))
                    };
                }
                // The server may seed read-only progress/service snapshots first.
                // Deliver them exactly once; do not drop state while awaiting ack.
                on_seed(frame);
            }
            Err("Portal closed before confirming compatibility. Reconnect or update to compatible releases.".into())
        };
        let timeout = gloo::timers::future::TimeoutFuture::new(10_000);
        futures_util::pin_mut!(negotiation, timeout);
        match futures_util::future::select(negotiation, timeout).await {
            futures_util::future::Either::Left((result, _)) => result?,
            futures_util::future::Either::Right(_) => {
                return Err(concat!(
                    "Portal compatibility confirmation timed out. ",
                    "Reconnect after checking the server; no commands were sent."
                )
                .into());
            }
        }
    }

    Ok((socket, config))
}
