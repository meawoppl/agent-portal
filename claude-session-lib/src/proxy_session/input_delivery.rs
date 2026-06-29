//! Input-delivery arm of the proxy connection loop (#1165 item 3).
//!
//! Extracted from the `run_main_loop` `select!` so the god-loop reads as thin
//! dispatch. Hands a [`PortalInput`] to the agent: refreshes git metadata,
//! emits the [`InputDeliveryStage`](shared::InputDeliveryStage) progress
//! events (#939), stashes the typed display-event echo for Claude's
//! `output_forwarder` to swap in (#1124), and acks the portal input.

use tracing::{debug, error};
use uuid::Uuid;

use session_lib::agent::Agent;
use session_lib::session::Session;

use super::git_metadata::{check_and_send_branch_update_if_branch_changed, GitMetadataState};
use super::{
    ack_portal_input, emit_input_progress, truncate, ConnectionResult, PendingInputDisplayEvent,
    PendingInputDisplayEvents, PortalInput, SharedWsWrite,
};

/// Deliver one user input to the agent. Returns `Some(ConnectionResult)` to end
/// the connection (delivery failure), or `None` to continue the loop.
#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_input<A: Agent>(
    ws_write: &SharedWsWrite,
    session_id: Uuid,
    working_directory: &str,
    git_metadata: &GitMetadataState,
    agent_type: shared::AgentType,
    pending_input_display_events: &PendingInputDisplayEvents,
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

    debug!("sending to claude process: {}", truncate(&input.text, 100));

    // Delivery stage: the proxy has the input and is about to hand it to the
    // agent (#939).
    emit_input_progress(
        ws_write,
        session_id,
        input.client_msg_id,
        shared::InputDeliveryStage::ProxyReceived,
    )
    .await;

    // Two mechanisms deliver the typed display event, exactly one per agent —
    // never both:
    //  - Claude echoes its input, so we stash the event here for the
    //    output_forwarder to swap in on the matching ClaudeOutput::User.
    //  - Codex doesn't echo, so its io-task emits the event itself (handed in
    //    via send_input_with_display below).
    // Gate the stash on Claude: pushing for Codex would leak the VecDeque entry
    // forever, since nothing consumes it there (#1124).
    if agent_type == shared::AgentType::Claude {
        if let Some(display_event) = input.display_event.clone() {
            let mut pending = pending_input_display_events.lock().await;
            pending.push_back(PendingInputDisplayEvent {
                echoed_text: input.text.clone(),
                content: display_event,
            });
        }
    }

    if let Err(e) = claude_session
        .send_input_with_display(serde_json::Value::String(input.text), input.display_event)
        .await
    {
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
    // Delivery stage: the input is now in the agent's stream — the optimistic
    // row clears here (#939).
    emit_input_progress(
        ws_write,
        session_id,
        input.client_msg_id,
        shared::InputDeliveryStage::AgentAccepted,
    )
    .await;
    ack_portal_input(ws_write, input.ack).await;
    None
}
