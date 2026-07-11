//! Input-delivery arm of the proxy connection loop (#1165 item 3).
//!
//! Extracted from the `run_main_loop` `select!` so the god-loop reads as thin
//! dispatch. Hands a [`PortalInput`] to the agent: refreshes git metadata,
//! emits the [`InputDeliveryStage`](shared::InputDeliveryStage) progress events
//! (#939), and schedules the final `AgentAccepted`/input ack without blocking
//! the connection loop. Display-event echo synthesis now lives in the agent
//! I/O task so it enters the replay buffer before proxy forwarding.

use tokio::sync::oneshot;
use tracing::{debug, error};
use uuid::Uuid;

use session_lib::agent::Agent;
use session_lib::session::Session;

use super::git_metadata::{check_and_send_branch_update_if_branch_changed, GitMetadataState};
use super::{
    ack_portal_input, emit_input_progress, truncate, ConnectionResult, PortalInput, PortalInputAck,
    SharedWsWrite,
};

/// Deliver one user input to the agent. Returns `Some(ConnectionResult)` to end
/// the connection (delivery failure), or `None` to continue the loop.
pub(super) async fn handle_input<A: Agent>(
    ws_write: &SharedWsWrite,
    session_id: Uuid,
    working_directory: &str,
    git_metadata: &GitMetadataState,
    claude_session: &mut Session<A>,
    input: PortalInput,
) -> Option<ConnectionResult> {
    check_and_send_branch_update_if_branch_changed(
        ws_write,
        session_id,
        working_directory,
        git_metadata,
    )
    .await;

    debug!("sending to agent process: {}", truncate(&input.text, 100));

    // Delivery stage: the proxy has the input and is about to hand it to the
    // agent (#939).
    emit_input_progress(
        ws_write,
        session_id,
        input.client_msg_id,
        shared::InputDeliveryStage::ProxyReceived,
    )
    .await;

    let delivered = match claude_session
        .enqueue_input_with_display(serde_json::Value::String(input.text), input.display_event)
    {
        Ok(delivered) => delivered,
        Err(e) => {
            error!("Failed to send to Claude: {}", e);
            emit_input_progress(
                ws_write,
                session_id,
                input.client_msg_id,
                shared::InputDeliveryStage::Failed,
            )
            .await;
            return Some(ConnectionResult::ClaudeExited);
        }
    };
    spawn_delivery_completion(
        ws_write.clone(),
        session_id,
        input.client_msg_id,
        input.ack,
        delivered,
    );
    None
}

fn spawn_delivery_completion(
    ws_write: SharedWsWrite,
    session_id: Uuid,
    client_msg_id: Option<Uuid>,
    ack: Option<PortalInputAck>,
    delivered: oneshot::Receiver<Result<(), String>>,
) {
    tokio::spawn(async move {
        match delivered.await {
            Ok(Ok(())) => {
                // Delivery stage: the input is now in the agent's stream — the
                // optimistic row clears here (#939).
                emit_input_progress(
                    &ws_write,
                    session_id,
                    client_msg_id,
                    shared::InputDeliveryStage::AgentAccepted,
                )
                .await;
                ack_portal_input(&ws_write, ack).await;
            }
            Ok(Err(message)) => {
                error!("Agent rejected input: {}", message);
                emit_input_progress(
                    &ws_write,
                    session_id,
                    client_msg_id,
                    shared::InputDeliveryStage::Failed,
                )
                .await;
            }
            Err(_) => {
                error!("I/O task closed before accepting input");
                emit_input_progress(
                    &ws_write,
                    session_id,
                    client_msg_id,
                    shared::InputDeliveryStage::Failed,
                )
                .await;
            }
        }
    });
}
