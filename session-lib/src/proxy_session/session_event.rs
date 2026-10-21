//! Agent-neutral session-event forwarding.

use std::time::Instant;

use crate::io::SessionEvent;
use shared::ProxyToServer;

use super::{ConnectionResult, SharedWsWrite};

pub enum DispatchResult {
    Handled,
    Unhandled(Option<SessionEvent>),
    Disconnect(ConnectionResult),
}

/// Forward event variants whose meaning is independent of the producing agent.
/// Visible output remains unhandled so the caller can apply its agent-specific
/// rendering, git-signal, and compatibility behavior.
pub async fn dispatch(
    event: Option<SessionEvent>,
    ws_write: &SharedWsWrite,
    session_id: uuid::Uuid,
    connection_start: Instant,
    thread_id_sink: Option<&(dyn Fn(String) + Send + Sync)>,
) -> DispatchResult {
    let message = match event {
        Some(SessionEvent::TurnMetricsReady(metrics)) => ProxyToServer::TurnMetricsReport(metrics),
        Some(SessionEvent::ToolProgress {
            tool_use_id,
            parent_tool_use_id,
            tool_name,
            elapsed_time_seconds,
            subagent_type,
            subagent_retry,
        }) => ProxyToServer::ToolProgress {
            session_id,
            tool_use_id,
            parent_tool_use_id,
            tool_name,
            elapsed_time_seconds,
            subagent_type,
            subagent_retry,
        },
        Some(SessionEvent::PermissionCancelled { request_id }) => {
            ProxyToServer::PermissionCancelled { request_id }
        }
        Some(SessionEvent::Ephemeral(payload)) => ProxyToServer::Ephemeral {
            session_id,
            payload,
        },
        Some(SessionEvent::CodexThreadId(thread_id)) => {
            if let Some(sink) = thread_id_sink {
                sink(thread_id);
            }
            return DispatchResult::Handled;
        }
        Some(SessionEvent::SessionLimitReached {
            session_id,
            reset_at,
            source_message,
            prompt,
        }) => ProxyToServer::SessionLimitReached(shared::SessionLimitContinuationFields {
            session_id,
            reset_at,
            source_message,
            prompt,
        }),
        other => return DispatchResult::Unhandled(other),
    };

    if ws_write.lock().await.send(message).await.is_err() {
        tracing::error!("Failed to forward agent-neutral session event");
        DispatchResult::Disconnect(ConnectionResult::Disconnected(connection_start.elapsed()))
    } else {
        DispatchResult::Handled
    }
}
