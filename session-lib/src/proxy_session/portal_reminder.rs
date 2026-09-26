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
//! The reminder body lives in `session-lib/portal_reminder.md` as a
//! readable markdown file and is baked into the binary via `include_str!`.
//! Operators can override at runtime by pointing `PORTAL_REMINDER_FILE` at a
//! readable path; on a missing or unreadable override we log a warning and
//! fall back to the bundled default.

use tracing::{error, info, warn};

use crate::agent::Agent;
use crate::session::Session;

/// Bundled fallback body (relative to this file).
const DEFAULT_BODY: &str = include_str!("../../portal_reminder.md");

/// Running portal version, captured at compile time from the workspace
/// `Cargo.toml`. Surfaced in the system-reminder envelope so the agent
/// knows which portal features and fixes are in scope.
const PORTAL_VERSION: &str = shared::VERSION;

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
    format!(
        "<system-reminder>\nAgent Portal version {}.\n\n{}\n</system-reminder>",
        PORTAL_VERSION,
        body.trim()
    )
}

fn body_with_plugin_skills(plugin_skill_reminder: Option<&str>) -> String {
    let mut body = load_reminder_body();
    if let Some(extra) = plugin_skill_reminder
        .map(str::trim)
        .filter(|extra| !extra.is_empty())
    {
        body.push_str("\n\n");
        body.push_str(extra);
    }
    body
}

/// Whether `text` is a CLI slash command (`/clear`, `/cost`, ...).
///
/// The CLI only recognises a command when the input *starts* with `/`, so the
/// session-start reminder must never be folded in front of one: the command
/// would reach the model as prose instead of running.
pub fn is_slash_command(text: &str) -> bool {
    text.trim_start().starts_with('/')
}

/// Whether `text` is `/clear`, which starts a fresh conversation that has not
/// seen the reminder.
pub fn is_clear_command(text: &str) -> bool {
    text.split_whitespace().next() == Some("/clear")
}

/// Fold the reminder into the session's **first** user input rather than
/// sending it as an input of its own.
///
/// Injecting it standalone at session start would make the agent answer it:
/// every agent here treats an input as a turn (muse literally spawns a `muse
/// exec` run per input), so the user would get a reply to a message they never
/// sent, before they had said anything. Riding along on the first real input
/// costs no extra turn and reaches every agent type through the one funnel
/// they all share ([`handle_input`](super::input_delivery::handle_input)).
///
/// Returns the agent-facing text plus the display event that must accompany
/// it. The display event is essential, not cosmetic: the prefixed text now
/// starts with `<system-reminder>`, and both the claude and codex echo paths
/// suppress synthesized echoes for exactly that prefix — without an explicit
/// display event the user's own message would vanish from the transcript.
///
/// `default_display` supplies the event when the caller has none — the
/// agent-specific "echo the user's own text" synthesizer. Taken as a closure
/// (rather than calling one agent's synthesizer here) because this module is
/// agent-agnostic (#1657); it runs only when `display_event` is `None`, with
/// the ORIGINAL text, before the reminder prefix is applied.
pub fn fold_session_start_reminder(
    text: String,
    display_event: Option<serde_json::Value>,
    plugin_skill_reminder: Option<&str>,
    default_display: impl FnOnce(&str) -> serde_json::Value,
) -> (String, Option<serde_json::Value>) {
    let display_event = display_event.or_else(|| Some(default_display(&text)));
    let prefixed = format!(
        "{}\n\n{}",
        agent_facing(&body_with_plugin_skills(plugin_skill_reminder)),
        text
    );
    (prefixed, display_event)
}

/// Inject the reminder into the agent's stdin only. The user-bound copy was
/// removed (#692): it bloated the scrollback for content the user already
/// knew, and the agent-side reminder is the part that actually does work
/// (re-priming the model after a compaction). The companion fix in the proxy
/// output forwarder also filters Claude's user-message echo of the
/// `<system-reminder>` text so the wrapper doesn't leak into the transcript.
pub async fn inject_portal_reminder<A: Agent>(claude_session: &mut Session<A>) {
    let body = body_with_plugin_skills(claude_session.config().plugin_skill_reminder.as_deref());

    if let Err(e) = claude_session
        .send_input(serde_json::Value::String(agent_facing(&body)))
        .await
    {
        error!("Failed to inject portal reminder into agent stdin: {}", e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_commands_are_detected_by_their_leading_slash() {
        assert!(is_slash_command("/clear"));
        assert!(is_slash_command("  /cost"));
        assert!(!is_slash_command("please /clear later"));
        assert!(!is_slash_command("hello"));
    }

    #[test]
    fn only_clear_counts_as_clear() {
        assert!(is_clear_command("/clear"));
        assert!(is_clear_command(" /clear \n"));
        assert!(!is_clear_command("/clearly"));
        assert!(!is_clear_command("/cost"));
    }

    /// The agent must receive the reminder AND the user's words, in that
    /// order, from a single input — the point of folding rather than sending
    /// the reminder as a turn of its own.
    #[test]
    fn fold_prefixes_the_reminder_and_keeps_the_prompt() {
        let (text, _) = fold_session_start_reminder(
            "do the thing".to_string(),
            None,
            None,
            |t| serde_json::json!({"echo": t}),
        );

        assert!(text.starts_with("<system-reminder>"));
        assert!(text.contains("Agent Portal version"));
        assert!(text.ends_with("do the thing"));
        // The reminder body itself came along, not just the envelope.
        assert!(text.contains("agent-portal show"));
    }

    /// Regression guard for the transcript: the folded text starts with
    /// `<system-reminder>`, which both the claude and codex echo paths use as
    /// their "suppress the synthesized echo" signal. Without a display event
    /// carrying the user's own words, their message would silently vanish.
    #[test]
    fn fold_supplies_a_display_event_so_the_user_message_still_renders() {
        let (_, display) = fold_session_start_reminder(
            "hello agent".to_string(),
            None,
            None,
            |t| serde_json::json!({"type": "user", "text": t}),
        );

        let display = display.expect("a display event is required, not optional");
        assert_eq!(display["type"], "user");
        assert!(
            display.to_string().contains("hello agent"),
            "display event must echo the user's text, got {display}"
        );
        assert!(
            !display.to_string().contains("system-reminder"),
            "the reminder must not leak into the transcript: {display}"
        );
    }

    /// An input that already has a display event (an inter-agent message card)
    /// keeps it — folding must not overwrite provenance with a plain echo.
    #[test]
    fn fold_preserves_an_existing_display_event() {
        let provenance = serde_json::json!({"type": "portal", "content": [{"agent": "codex"}]});
        let (text, display) = fold_session_start_reminder(
            "relayed".to_string(),
            Some(provenance.clone()),
            None,
            |_| unreachable!("display provided"),
        );

        assert_eq!(display, Some(provenance));
        assert!(text.ends_with("relayed"));
    }

    #[test]
    fn fold_includes_plugin_skill_reminder_when_present() {
        let (text, _) = fold_session_start_reminder(
            "route this board".to_string(),
            None,
            Some("## Plugin Skills\n\n- `kicad-pcb:pcb-workflow`: /tmp/SKILL.md"),
            |t| serde_json::json!({"echo": t}),
        );

        assert!(text.contains("## Plugin Skills"));
        assert!(text.contains("kicad-pcb:pcb-workflow"));
        assert!(text.ends_with("route this board"));
    }
}
