//! Input-delivery arm of the proxy connection loop (#1165 item 3).
//!
//! Extracted from the `run_main_loop` `select!` so the god-loop reads as thin
//! dispatch. Hands a [`PortalInput`] to the agent: refreshes git metadata,
//! emits the [`InputDeliveryStage`](shared::InputDeliveryStage) progress events
//! (#939), and schedules the final `AgentAccepted`/input ack without blocking
//! the connection loop. Display-event echo synthesis now lives in the agent
//! I/O task so it enters the replay buffer before proxy forwarding.

use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::oneshot;
use tracing::{debug, error};
use uuid::Uuid;

use super::{
    ack_portal_input, emit_input_progress, portal_reminder, ConnectionResult, PortalInput,
    PortalInputAck, SharedWsWrite,
};
use crate::agent::Agent;
use crate::session::Session;
use shared::fmt::truncate_str as truncate;

/// Deliver one user input to the agent. Returns `Some(ConnectionResult)` to end
/// the connection (delivery failure), or `None` to continue the loop.
pub async fn handle_input<A, Refresh, RefreshFuture, Display>(
    ws_write: &SharedWsWrite,
    session_id: Uuid,
    reminder_pending: &AtomicBool,
    claude_session: &mut Session<A>,
    input: PortalInput,
    refresh_git: Refresh,
    default_display: Display,
) -> Option<ConnectionResult>
where
    A: Agent,
    Refresh: FnOnce() -> RefreshFuture,
    RefreshFuture: std::future::Future<Output = ()>,
    Display: FnOnce(&str) -> serde_json::Value,
{
    refresh_git().await;

    debug!("sending to agent process: {}", truncate(&input.text, 100));

    // First input of a fresh agent context carries the portal-features
    // reminder. `swap` makes the claim atomic, so a burst of queued inputs
    // prefixes exactly one of them. Real resumes initialize this flag false
    // because their transcript already contains the reminder. Slash commands
    // never carry it (the prefix would stop the CLI from recognising the
    // command), and `/clear` re-arms it because the conversation it starts has
    // never seen the reminder.
    let slash_command = portal_reminder::is_slash_command(&input.text);
    if slash_command && portal_reminder::is_clear_command(&input.text) {
        reminder_pending.store(true, Ordering::SeqCst);
    }
    let recovery = if slash_command {
        None
    } else {
        claude_session.take_recovery_context()
    };
    let inject_reminder = !slash_command && reminder_pending.swap(false, Ordering::SeqCst);
    // A plugin change posted while the session ran. Peeked, not taken: it is
    // acknowledged only once the agent accepts this input, so a failed
    // delivery leaves it for the next one.
    let inbox = claude_session.context_inbox().clone();
    let context_update = if slash_command { None } else { inbox.pending() };
    let (text, display_event) = if inject_reminder || recovery.is_some() {
        let mut context = fresh_plugin_context(
            &inbox,
            claude_session.config().plugin_skill_reminder.as_deref(),
        );
        if let Some(recovery) = recovery {
            context.push_str("\n\n");
            context.push_str(&recovery);
        }
        portal_reminder::fold_session_start_reminder(
            input.text,
            input.display_event,
            Some(&context),
            default_display,
        )
    } else if let Some(update) = &context_update {
        portal_reminder::fold_context_update(
            input.text,
            input.display_event,
            &update.text,
            default_display,
        )
    } else {
        (input.text, input.display_event)
    };

    // Delivery stage: the proxy has the input and is about to hand it to the
    // agent (#939).
    emit_input_progress(
        ws_write,
        session_id,
        input.client_msg_id,
        shared::InputDeliveryStage::ProxyReceived,
    )
    .await;

    let delivered = match claude_session.enqueue_input_with_display(
        serde_json::Value::String(text),
        display_event,
        input.reasoning_effort,
    ) {
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
            return Some(ConnectionResult::AgentExited);
        }
    };
    if let Some(update) = context_update {
        inbox.acknowledge(update.revision);
    }
    spawn_delivery_completion(
        ws_write.clone(),
        session_id,
        input.client_msg_id,
        input.ack,
        delivered,
    );
    None
}

/// The plugin context a fresh conversation (first input, `/clear`, a restart
/// with recovery) starts with: the latest posted one, else the launch-time
/// one. Covers any pending change, which a fresh start makes moot.
fn fresh_plugin_context(inbox: &crate::ContextInbox, launch: Option<&str>) -> String {
    inbox
        .current()
        .or_else(|| launch.map(str::to_string))
        .unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Toggle a plugin off, deliver the change, then `/clear`: the fresh
    /// conversation must start with the new plugin context, not the launch one.
    #[test]
    fn a_fresh_conversation_uses_the_latest_plugin_context() {
        let inbox = crate::ContextInbox::default();
        let old = "## Plugin Skills\n- `old:skill`";
        let launch = Some(old);
        assert_eq!(fresh_plugin_context(&inbox, launch), old);

        inbox.post(1, String::new(), "## Plugin context changed".into());
        let delivered = inbox.pending().unwrap();
        inbox.acknowledge(delivered.revision);
        assert!(!inbox.is_pending());
        assert_eq!(fresh_plugin_context(&inbox, launch), "", "no plugins now");

        inbox.post(
            2,
            "## Plugin Skills\n- `new:skill`".into(),
            "changed".into(),
        );
        assert_eq!(
            fresh_plugin_context(&inbox, launch),
            "## Plugin Skills\n- `new:skill`"
        );
    }
}
