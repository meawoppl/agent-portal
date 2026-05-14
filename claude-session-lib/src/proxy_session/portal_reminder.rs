//! Portal features reminder injected at session start and after each
//! compaction boundary.
//!
//! Two parallel streams off the same trigger:
//! 1. The agent-facing copy is sent as user input wrapped in
//!    `<system-reminder>…</system-reminder>` tags. Claude treats those tags
//!    as out-of-band context rather than a real user message.
//! 2. The user-facing copy is emitted as a `PortalContent::Reminder` and
//!    rendered as a collapsed block on the frontend.
//!
//! The reminder body lives in `claude-session-lib/portal_reminder.md` as a
//! readable markdown file and is baked into the binary via `include_str!`.
//! Operators can override at runtime by pointing `PORTAL_REMINDER_FILE` at a
//! readable path; on a missing or unreadable override we log a warning and
//! fall back to the bundled default.

use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::output_buffer::PendingOutputBuffer;
use crate::session::Session as ClaudeSession;

use super::SharedWsWrite;

/// Bundled fallback body (relative to this file).
const DEFAULT_BODY: &str = include_str!("../../portal_reminder.md");

/// Resolve the reminder body. Honors `PORTAL_REMINDER_FILE` at call time so
/// operators can hot-edit the file and have the next compaction pick it up
/// without restarting the proxy.
pub fn load_reminder_body() -> String {
    match std::env::var("PORTAL_REMINDER_FILE") {
        Ok(path) if !path.is_empty() => match std::fs::read_to_string(&path) {
            Ok(body) => {
                info!(
                    "Loaded portal reminder override from PORTAL_REMINDER_FILE={} ({} bytes)",
                    path,
                    body.len()
                );
                body
            }
            Err(e) => {
                warn!(
                    "PORTAL_REMINDER_FILE={} is set but the file could not be read ({}); \
                     falling back to the bundled portal reminder.",
                    path, e
                );
                DEFAULT_BODY.to_string()
            }
        },
        _ => DEFAULT_BODY.to_string(),
    }
}

fn agent_facing(body: &str) -> String {
    format!("<system-reminder>\n{}\n</system-reminder>", body.trim())
}

/// Inject the reminder on both streams: agent-bound (via stdin) and
/// user-bound (as a sequenced portal message). Idempotency / "only on the
/// right trigger" is the caller's responsibility — this function unconditionally
/// fires both.
pub async fn inject_portal_reminder(
    claude_session: &mut ClaudeSession,
    ws_write: &SharedWsWrite,
    output_buffer: &Arc<Mutex<PendingOutputBuffer>>,
) {
    let body = load_reminder_body();

    // Agent-bound: a single user-input message wrapped in <system-reminder>.
    if let Err(e) = claude_session
        .send_input(serde_json::Value::String(agent_facing(&body)))
        .await
    {
        error!("Failed to inject portal reminder into agent stdin: {}", e);
    }

    // User-bound: a sequenced portal message rendered collapsed on the frontend.
    let portal_msg = shared::PortalMessage::reminder("Portal features".to_string(), body);
    let portal_content = portal_msg.to_json();
    let seq = {
        let mut buf = output_buffer.lock().await;
        buf.push(portal_content.clone())
    };
    let ws_msg = shared::ProxyToServer::SequencedOutput {
        seq,
        content: portal_content,
    };
    let mut ws = ws_write.lock().await;
    if let Err(e) = ws.send(ws_msg).await {
        error!("Failed to send portal reminder message to backend: {}", e);
    }
}
