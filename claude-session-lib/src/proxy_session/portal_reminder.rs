//! Portal features reminder injected at session start and after each
//! compaction boundary.
//!
//! The reminder is sent to the agent only — wrapped in
//! `<system-reminder>…</system-reminder>` tags so Claude treats it as
//! out-of-band context. The user-facing copy was removed in #692: it ate too
//! much vertical scrollback for content the user already knows (they built
//! the portal), and the reminder's value is the agent recovering its
//! affordance knowledge after a fresh start / compaction.
//!
//! The reminder body lives in `claude-session-lib/portal_reminder.md` as a
//! readable markdown file and is baked into the binary via `include_str!`.
//! Operators can override at runtime by pointing `PORTAL_REMINDER_FILE` at a
//! readable path; on a missing or unreadable override we log a warning and
//! fall back to the bundled default.

use tracing::{error, info, warn};

use crate::session::Session as ClaudeSession;

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

/// Inject the reminder into the agent's stdin only. The user-bound copy was
/// removed (#692): it bloated the scrollback for content the user already
/// knew, and the agent-side reminder is the part that actually does work
/// (re-priming the model after a compaction). The companion fix in the proxy
/// output forwarder also filters Claude's user-message echo of the
/// `<system-reminder>` text so the wrapper doesn't leak into the transcript.
pub async fn inject_portal_reminder(claude_session: &mut ClaudeSession) {
    let body = load_reminder_body();

    if let Err(e) = claude_session
        .send_input(serde_json::Value::String(agent_facing(&body)))
        .await
    {
        error!("Failed to inject portal reminder into agent stdin: {}", e);
    }
}
