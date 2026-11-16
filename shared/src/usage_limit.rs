//! Whether a transcript frame is the agent refusing work because the account
//! is out of usage: Claude's typed `usage_limit_reached` /
//! `model_requires_usage_credits` assistant errors and Codex's
//! `usageLimitExceeded`. Transient overloads and per-request rate limits are
//! not included — waiting a moment fixes those, while a usage limit is what
//! makes handing the work to another agent worthwhile.

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
    fn agents_are_not_confused() {
        assert!(!is_usage_limit_refusal(AgentType::Codex, CLAUDE_LIMIT));
        assert!(!is_usage_limit_refusal(AgentType::Muse, CLAUDE_LIMIT));
    }
}
