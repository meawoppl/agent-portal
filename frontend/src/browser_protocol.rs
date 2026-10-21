//! Browser sockets must negotiate before callers can expose a mutation sender.
//! A successful config response without metadata is the sole legacy fallback;
//! silence, failed fetches and timed-out acks never authorize commands.
use crate::portal_client::PortalClient;
use crate::utils::On401;
use shared::{AppConfig, ClientEndpoint, ClientToServer, ServerToClient};

#[derive(Debug)]
pub(crate) struct ConnectError {
    pub message: String,
    pub retryable: bool,
}

impl ConnectError {
    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }
    fn incompatible(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

pub(crate) async fn connect(
    portal: &PortalClient,
    mut on_seed: impl FnMut(ServerToClient),
) -> Result<(ws_bridge::yew_client::Connection<ClientEndpoint>, AppConfig), ConnectError> {
    // Use the originating PortalClient, preserving linked-account auth headers.
    let config = portal.fetch_json::<AppConfig>("/api/config", On401::Ignore)
        .await.map_err(|e| ConnectError::transient(format!("Could not check Portal compatibility: {e}. Reconnecting after the server is available.")))?;
    shared::protocol::check_protocol_version(config.protocol_version, "server")
        .map_err(ConnectError::incompatible)?;
    let endpoint = portal
        .socket_url()
        .await
        .map_err(|e| ConnectError::transient(format!("Portal authorization failed: {e}")))?;
    let mut socket = ws_bridge::yew_client::connect_to::<ClientEndpoint>(&endpoint)
        .map_err(|e| ConnectError::transient(format!("Portal connection failed: {e}")))?;
    if config.protocol_version.is_none() {
        return Ok((socket, config));
    }
    socket
        .send(ClientToServer::Hello {
            protocol_version: shared::protocol::PROTOCOL_VERSION,
        })
        .await
        .map_err(|e| {
            ConnectError::transient(format!("Could not negotiate Portal compatibility: {e}"))
        })?;
    {
        let negotiation = async {
            loop {
                if handshake_frame(socket.recv().await, &mut on_seed)? {
                    return Ok(());
                }
            }
        };
        let timeout = gloo::timers::future::TimeoutFuture::new(10_000);
        futures_util::pin_mut!(negotiation, timeout);
        match futures_util::future::select(negotiation, timeout).await {
            futures_util::future::Either::Left((result, _)) => result?,
            futures_util::future::Either::Right(_) => {
                return Err(ConnectError::transient(concat!(
                    "Portal compatibility confirmation timed out. ",
                    "Reconnect after checking the server; no commands were sent."
                )));
            }
        }
    }

    Ok((socket, config))
}

/// Only an explicit compatible acknowledgement opens the command gate.
/// Seed frames are delivered but never imply readiness; lost transport retries.
fn handshake_frame(
    frame: Option<Result<ServerToClient, ws_bridge::RecvError>>,
    on_seed: &mut impl FnMut(ServerToClient),
) -> Result<bool, ConnectError> {
    let frame = frame
        .ok_or_else(|| {
            ConnectError::transient("Portal closed before confirming compatibility. Reconnecting.")
        })?
        .map_err(|e| ConnectError::transient(format!("Invalid Portal handshake: {e}")))?;
    if let ServerToClient::HelloAck {
        protocol_version,
        success,
        error,
    } = frame
    {
        shared::protocol::check_protocol_version(Some(protocol_version), "server")
            .map_err(ConnectError::incompatible)?;
        if !success {
            return Err(ConnectError::incompatible(error.unwrap_or_else(|| "Portal rejected compatibility negotiation. Update the browser and server to compatible releases.".into())));
        }
        return Ok(true);
    }
    on_seed(frame);
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_snapshots_do_not_authorize_commands_and_closed_transport_retries() {
        let mut seeds = Vec::new();
        assert!(
            !handshake_frame(Some(Ok(ServerToClient::SessionProgressReset)), &mut |v| {
                seeds.push(v)
            })
            .unwrap()
        );
        assert_eq!(seeds.len(), 1);
        let error = handshake_frame(None, &mut |_| panic!("no seed")).unwrap_err();
        assert!(error.retryable);
        let error = handshake_frame(Some(Err(ws_bridge::RecvError::Closed)), &mut |_| {
            panic!("no seed")
        })
        .unwrap_err();
        assert!(error.retryable);
    }

    #[test]
    fn success_does_not_override_skew_and_rejection_is_fatal() {
        for (major, success, accepted) in [(1, true, true), (2, true, false), (1, false, false)] {
            let result = handshake_frame(
                Some(Ok(ServerToClient::HelloAck {
                    protocol_version: major,
                    success,
                    error: None,
                })),
                &mut |_| panic!("ack is not a seed"),
            );
            if accepted {
                assert!(result.unwrap());
            } else {
                assert!(!result.unwrap_err().retryable);
            }
        }
    }
}
