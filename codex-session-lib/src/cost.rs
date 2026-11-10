//! Optional native billing counters. Missing USD is unknown, never an estimate
//! based on credits. A resumed/forked thread requires an observed baseline.
use codex_codes::{AsyncClient, GetAccountTokenUsageParams};
use std::time::Duration;

pub(crate) async fn read_thread_cost(client: &mut AsyncClient, thread: &str) -> Option<i64> {
    let params = GetAccountTokenUsageParams {
        thread_id: Some(thread.to_owned()),
    };
    let response = tokio::time::timeout(
        Duration::from_secs(2),
        client.account_usage_read_with(&params),
    )
    .await;
    match response {
        Ok(Ok(response)) => response
            .thread_usage
            .filter(|usage| usage.thread_id == thread)
            .and_then(|usage| usage.native_usage_usd_micros)
            .filter(|cost| *cost >= 0),
        // Optional accounting must never prevent turn delivery/completion on
        // an older app-server or a ChatGPT plan without native billing data.
        Ok(Err(error)) => {
            tracing::debug!(%error, "Codex native billing unavailable");
            None
        }
        Err(_) => {
            tracing::debug!("Codex native billing query timed out");
            None
        }
    }
}

pub(crate) struct ThreadCostTracker {
    previous: Option<i64>,
}
impl ThreadCostTracker {
    pub fn new(fresh: bool, baseline: Option<i64>) -> Self {
        Self {
            previous: baseline.filter(|cost| *cost >= 0).or(fresh.then_some(0)),
        }
    }
    pub fn observe(&mut self, value: Option<i64>) -> Option<f64> {
        let value = value.filter(|cost| *cost >= 0);
        let previous = std::mem::replace(&mut self.previous, value);
        let difference = value?.checked_sub(previous?)?;
        (difference >= 0).then_some(difference as f64 / 1_000_000.0)
    }
}

pub(crate) fn is_parent_lifecycle(message: &codex_codes::ServerMessage, parent: &str) -> bool {
    match message {
        codex_codes::ServerMessage::Notification(codex_codes::Notification::TurnStarted(p)) => {
            p.thread_id == parent
        }
        codex_codes::ServerMessage::Notification(codex_codes::Notification::TurnCompleted(p)) => {
            p.thread_id == parent
        }
        _ => true,
    }
}

pub(crate) fn request_error(error: &codex_codes::Error) -> String {
    use codex_codes::ModelProviderError::*;
    match error.model_provider_error() {
        Some(ModelProviderMismatch { model_provider, current_model_provider }) => format!("This conversation uses provider {model_provider}, but this computer is configured for {current_model_provider}. Restore the original provider configuration or fork a new session to start fresh."),
        Some(ModelProviderNotConfigured { model_provider }) => format!("This conversation's provider {model_provider} is not configured on this computer. Restore that provider or fork a new session to start fresh."),
        Some(ModelProviderPolicyConflict { model_provider, required_model_provider }) => format!("This conversation uses provider {model_provider}, but this computer's policy requires {required_model_provider}. Use a compatible computer or fork a new session under the required provider."),
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn child_lifecycle_cannot_finalize_or_restart_parent_accounting() {
        for method in ["turn/started", "turn/completed"] {
            let wire = format!(
                r#"{{"method":"{method}","params":{{"threadId":"child","turn":{{"id":"child-turn","items":[],"status":"completed"}}}}}}"#
            );
            let message = codex_codes::ServerMessage::from_json_str(&wire).unwrap();
            assert!(!is_parent_lifecycle(&message, "parent"));
            assert!(is_parent_lifecycle(&message, "child"));
        }
    }

    #[test]
    fn native_cost_deltas_keep_zero_and_unknown_distinct_across_resume_and_gaps() {
        let mut resumed = ThreadCostTracker::new(false, Some(10_000_000));
        assert_eq!(resumed.observe(Some(12_500_000)), Some(2.5));
        assert_eq!(resumed.observe(Some(12_500_000)), Some(0.0));
        assert_eq!(resumed.observe(None), None);
        assert_eq!(
            resumed.observe(Some(20_000_000)),
            None,
            "unknown intervening cost cannot be assigned to this turn"
        );
        assert_eq!(resumed.observe(Some(21_000_000)), Some(1.0));
        assert_eq!(
            resumed.observe(Some(100)),
            None,
            "counter reset is not negative spend"
        );
        assert_eq!(resumed.observe(Some(200)), Some(0.0001));
        assert_eq!(
            ThreadCostTracker::new(false, None).observe(Some(10_000_000)),
            None
        );
        assert_eq!(
            ThreadCostTracker::new(true, None).observe(Some(250_000)),
            Some(0.25)
        );
    }
    #[test]
    fn provider_errors_are_actionable_without_exposing_unrelated_error_data() {
        for (wire, expected) in [
            (
                r#"{"type":"modelProviderMismatch","modelProvider":"original","currentModelProvider":"current"}"#,
                "configured for current",
            ),
            (
                r#"{"type":"modelProviderNotConfigured","modelProvider":"original"}"#,
                "not configured",
            ),
            (
                r#"{"type":"modelProviderPolicyConflict","modelProvider":"original","requiredModelProvider":"required"}"#,
                "policy requires required",
            ),
        ] {
            let error = codex_codes::Error::JsonRpc {
                code: -32000,
                message: "request failed".into(),
                data: Some(serde_json::from_str(wire).unwrap()),
            };
            let message = request_error(&error);
            assert!(message.contains(expected), "{message}");
            assert!(message.contains("original"));
        }
        let error = codex_codes::Error::JsonRpc {
            code: -32000,
            message: "request failed".into(),
            data: Some(serde_json::from_str(r#"{"private":"do not show"}"#).unwrap()),
        };
        assert!(!request_error(&error).contains("do not show"));
    }
}
