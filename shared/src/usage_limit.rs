//! Whether a transcript frame is the agent refusing work because the account
//! is out of usage: Claude's typed `usage_limit_reached` /
//! `model_requires_usage_credits` assistant errors and Codex's
//! `usageLimitExceeded`. Transient overloads and per-request rate limits are
//! not included — waiting a moment fixes those, while a usage limit is what
//! makes handing the work to another agent worthwhile.
//!
//! It also recognizes the *warning* each agent gives before that point, which
//! Portal uses to ask the agent for a handoff summary while it still can.

use crate::AgentType;

/// Claude `api_error` kinds that mean the account cannot run more turns.
const CLAUDE_USAGE_ERRORS: [&str; 2] = ["usage_limit_reached", "model_requires_usage_credits"];

pub fn is_usage_limit_refusal(agent: AgentType, content: &str) -> bool {
    match agent {
        AgentType::Claude => serde_json::from_str::<crate::AssistantMessage>(content)
            .is_ok_and(|message| is_claude_usage_refusal(&message)),
        AgentType::Codex => {
            serde_json::from_str::<crate::codex_errors::CodexLifecycleFrame>(content)
                .is_ok_and(|frame| frame.is_usage_limit())
        }
        AgentType::Muse | AgentType::Antigravity => false,
    }
}

/// Only the top-level assistant's own typed error counts. Subagent limits and
/// quoted content do not stop this session.
pub fn is_claude_usage_refusal(message: &crate::AssistantMessage) -> bool {
    message.parent_tool_use_id.is_none()
        && message.agent_id.is_none()
        && message
            .api_error
            .as_deref()
            .is_some_and(|kind| CLAUDE_USAGE_ERRORS.contains(&kind))
}

/// Frame type the Codex session forwards for an `account/rateLimits/updated`
/// notification. Only snapshots near a limit are forwarded; the rest would
/// fill the transcript with a row per turn.
pub const CODEX_RATE_LIMITS_EVENT: &str = "account/rateLimits/updated";

/// A Codex usage window this full counts as a warning.
pub const CODEX_USAGE_WARNING_PERCENT: i64 = 90;

/// Heading the agent opens its handoff summary with, so a handoff can find it
/// in the transcript.
pub const HANDOFF_SUMMARY_HEADING: &str = "## Handoff summary";

/// Whether any of a Codex snapshot's usage windows is nearly used up.
pub fn codex_near_limit(snapshot: &codex_codes::RateLimitSnapshot) -> bool {
    codex_warning_window(snapshot).is_some()
}

fn codex_warning_window(snapshot: &codex_codes::RateLimitSnapshot) -> Option<String> {
    [
        ("primary", &snapshot.primary),
        ("secondary", &snapshot.secondary),
    ]
    .into_iter()
    .find_map(|(name, window)| {
        let window = window.as_ref()?;
        (window.used_percent >= CODEX_USAGE_WARNING_PERCENT)
            .then(|| format!("codex:{name}:{}", window.resets_at.unwrap_or_default()))
    })
}

#[derive(serde::Deserialize)]
struct CodexRateLimitsFrame {
    params: codex_codes::AccountRateLimitsUpdatedNotification,
}

/// When the frame warns that the agent is close to its usage limit, a key
/// naming the usage window, so one warning per window can be acted on.
/// `None` for every other frame, including the refusal itself.
pub fn usage_warning(agent: AgentType, content: &serde_json::Value) -> Option<String> {
    let kind = content.get("type")?.as_str()?;
    match agent {
        AgentType::Claude if kind == "rate_limit_event" => {
            let event = serde_json::from_value::<crate::RateLimitEvent>(content.clone()).ok()?;
            let info = &event.rate_limit_info;
            (info.status.as_str() == "allowed_warning").then(|| {
                format!(
                    "claude:{}:{}",
                    info.rate_limit_type
                        .as_ref()
                        .map_or("unknown", |t| t.as_str()),
                    info.resets_at.unwrap_or_default()
                )
            })
        }
        AgentType::Codex if kind == CODEX_RATE_LIMITS_EVENT => {
            let frame = serde_json::from_value::<CodexRateLimitsFrame>(content.clone()).ok()?;
            codex_warning_window(&frame.params.rate_limits)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_LIMIT: &str = r#"{"type":"assistant","session_id":"s","api_error":"usage_limit_reached","message":{"id":"m","type":"message","role":"assistant","model":"<synthetic>","content":[]}}"#;

    #[test]
    fn claude_typed_usage_errors_count_only_at_top_level() {
        assert!(is_usage_limit_refusal(AgentType::Claude, CLAUDE_LIMIT));
        let credits = CLAUDE_LIMIT.replace("usage_limit_reached", "model_requires_usage_credits");
        assert!(is_usage_limit_refusal(AgentType::Claude, &credits));
        let subagent = CLAUDE_LIMIT.replace(
            r#""session_id":"s","#,
            r#""session_id":"s","parent_tool_use_id":"t","#,
        );
        assert!(!is_usage_limit_refusal(AgentType::Claude, &subagent));
        let other = CLAUDE_LIMIT.replace("usage_limit_reached", "connection_failed");
        assert!(!is_usage_limit_refusal(AgentType::Claude, &other));
    }

    #[test]
    fn codex_usage_limit_counts_but_overload_does_not() {
        assert!(is_usage_limit_refusal(
            AgentType::Codex,
            r#"{"type":"error","message":"You've hit your usage limit","codexErrorInfo":"usageLimitExceeded"}"#
        ));
        assert!(is_usage_limit_refusal(
            AgentType::Codex,
            r#"{"type":"turn.completed","status":"failed","error":{"message":"limit","codexErrorInfo":"usageLimitExceeded"}}"#
        ));
        assert!(!is_usage_limit_refusal(
            AgentType::Codex,
            r#"{"type":"error","message":"busy","codexErrorInfo":"serverOverloaded","willRetry":true}"#
        ));
        assert!(!is_usage_limit_refusal(
            AgentType::Codex,
            r#"{"type":"turn.completed"}"#
        ));
    }

    #[test]
    fn warnings_name_their_window_and_ignore_ordinary_status() {
        let claude: serde_json::Value = serde_json::from_str(
            r#"{"type":"rate_limit_event","uuid":"u","session_id":"s","rate_limit_info":{"status":"allowed_warning","rateLimitType":"five_hour","resetsAt":1893456000}}"#,
        )
        .unwrap();
        assert_eq!(
            usage_warning(AgentType::Claude, &claude).as_deref(),
            Some("claude:five_hour:1893456000")
        );
        let mut allowed = claude.clone();
        allowed["rate_limit_info"]["status"] = "allowed".into();
        assert_eq!(usage_warning(AgentType::Claude, &allowed), None);

        let codex: serde_json::Value = serde_json::from_str(
            r#"{"type":"account/rateLimits/updated","params":{"rateLimits":{"primary":{"usedPercent":40,"resetsAt":1},"secondary":{"usedPercent":93,"resetsAt":2}}}}"#,
        )
        .unwrap();
        assert_eq!(
            usage_warning(AgentType::Codex, &codex).as_deref(),
            Some("codex:secondary:2")
        );
        let mut low = codex.clone();
        low["params"]["rateLimits"]["secondary"]["usedPercent"] = 50.into();
        assert_eq!(usage_warning(AgentType::Codex, &low), None);
        assert_eq!(usage_warning(AgentType::Claude, &codex), None);
    }

    #[test]
    fn agents_are_not_confused() {
        assert!(!is_usage_limit_refusal(AgentType::Codex, CLAUDE_LIMIT));
        assert!(!is_usage_limit_refusal(AgentType::Muse, CLAUDE_LIMIT));
    }
}
