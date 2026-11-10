//! Error details retained when projecting Codex app-server frames into the
//! portal transcript. Old archives have only `message`; absent retry metadata
//! must retain their terminal-error behavior.
use codex_codes::CodexErrorInfo;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodexErrorDetails {
    pub message: Option<String>,
    #[serde(
        default,
        rename = "codexErrorInfo",
        alias = "codex_error_info",
        skip_serializing_if = "Option::is_none"
    )]
    pub codex_error_info: Option<CodexErrorInfo>,
    #[serde(default, rename = "willRetry", alias = "will_retry")]
    pub will_retry: bool,
}

impl CodexErrorDetails {
    pub fn from_turn_error(error: &codex_codes::TurnError, will_retry: bool) -> Self {
        Self {
            message: Some(if error.message.is_empty() {
                "Unknown error".into()
            } else {
                error.message.clone()
            }),
            codex_error_info: error.codex_error_info.clone(),
            will_retry,
        }
    }

    /// A capacity refusal remains throttled even when the provider ends its
    /// turn. Callers must inspect the completion's error too, rather than
    /// letting the subsequent completion reset a throttled session to idle.
    pub fn activity(&self) -> crate::api::SessionActivityState {
        use crate::api::SessionActivityState::{Busy, Idle, Throttled};
        if matches!(
            self.codex_error_info,
            Some(
                CodexErrorInfo::UsageLimitExceeded
                    | CodexErrorInfo::RateLimitExceeded
                    | CodexErrorInfo::ServerOverloaded
                    | CodexErrorInfo::PolicyCapacityUnavailable
            )
        ) {
            Throttled
        } else if self.will_retry {
            Busy
        } else {
            Idle
        }
    }

    pub fn action_hint(&self) -> Option<&'static str> {
        use CodexErrorInfo::*;
        match self.codex_error_info.as_ref()? {
            ModelNotFound => Some("This model is unavailable for your account. Choose another model in the session settings."),
            UsageLimitExceeded | RateLimitExceeded | ServerOverloaded | PolicyCapacityUnavailable | FlexUnavailable => Some("Try again later or choose another model in the session settings."),
            ContextWindowExceeded => Some("Compact the conversation or fork into a new session."),
            SessionBudgetExceeded => Some("Increase the session budget or start a new session."),
            Unauthorized => Some("Sign in to Codex again, then retry."),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::SessionActivityState::{Busy, Idle, Throttled};

    #[test]
    fn archived_errors_and_future_codes_keep_terminal_fallback() {
        for wire in [
            r#"{"message":"legacy error"}"#,
            r#"{"message":"future error","codexErrorInfo":{"newCode":{}}}"#,
        ] {
            let error: CodexErrorDetails = serde_json::from_str(wire).unwrap();
            assert_eq!(error.activity(), Idle);
            assert!(!error.will_retry);
        }
    }

    #[test]
    fn retry_and_capacity_have_distinct_activity() {
        let retry: CodexErrorDetails =
            serde_json::from_str(r#"{"message":"network retry","willRetry":true}"#).unwrap();
        assert_eq!(retry.activity(), Busy);
        for code in [
            "usageLimitExceeded",
            "rateLimitExceeded",
            "serverOverloaded",
            "policyCapacityUnavailable",
        ] {
            for will_retry in [false, true] {
                let wire = format!(
                    r#"{{"message":"capacity","codexErrorInfo":"{code}","willRetry":{will_retry}}}"#
                );
                let error: CodexErrorDetails = serde_json::from_str(&wire).unwrap();
                assert_eq!(error.activity(), Throttled);
                assert!(error.action_hint().is_some());
            }
        }
    }
}
