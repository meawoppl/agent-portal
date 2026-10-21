//! Follow-specific lifecycle metadata; peek remains a compact message digest.
use shared::api::SessionActivityState;

#[derive(Default, Debug)]
pub(super) struct Lifecycle {
    pub activity: Option<SessionActivityState>,
    pub kind: Option<&'static str>,
}
impl Lifecycle {
    fn activity(activity: SessionActivityState) -> Self {
        Self {
            activity: Some(activity),
            kind: None,
        }
    }
    fn terminal(failed: bool) -> Self {
        Self {
            activity: Some(SessionActivityState::Idle),
            kind: Some(if failed { "error" } else { "status" }),
        }
    }
}

pub(super) fn classify(agent: &str, content: &str) -> Lifecycle {
    use SessionActivityState::{Busy, Throttled};
    match agent {
        "claude" => {
            use claude_codes::{ClaudeOutput, RateLimitStatus};
            match serde_json::from_str::<ClaudeOutput>(content) {
                Ok(ClaudeOutput::User(_) | ClaudeOutput::Assistant(_)) => Lifecycle::activity(Busy),
                Ok(ClaudeOutput::Result(result)) => Lifecycle::terminal(result.is_error),
                Ok(ClaudeOutput::RateLimitEvent(event))
                    if event.rate_limit_info.status == RateLimitStatus::Rejected =>
                {
                    Lifecycle {
                        activity: Some(Throttled),
                        kind: Some("status"),
                    }
                }
                _ => Lifecycle::default(),
            }
        }
        "codex" => {
            use codex_codes::ThreadEvent;
            match serde_json::from_str::<ThreadEvent>(content) {
                Ok(ThreadEvent::TurnStarted(_)) => Lifecycle {
                    activity: Some(Busy),
                    kind: Some("status"),
                },
                Ok(ThreadEvent::TurnCompleted(_)) => Lifecycle::terminal(false),
                Ok(ThreadEvent::TurnFailed(_)) => Lifecycle::terminal(true),
                _ => Lifecycle::default(),
            }
        }
        "muse" => {
            use muse_codes::{MusePayload, MuseRecord};
            let Ok(record) = serde_json::from_str::<MuseRecord>(content) else {
                return Lifecycle::default();
            };
            match record.typed_payload() {
                Ok(MusePayload::RunStarted(_)) => Lifecycle {
                    activity: Some(Busy),
                    kind: Some("status"),
                },
                Ok(MusePayload::RunTerminal(terminal)) => {
                    Lifecycle::terminal(terminal.terminal != "completed")
                }
                _ => Lifecycle::default(),
            }
        }
        _ => Lifecycle::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn failed_terminals_are_errors_with_durable_idle_for_all_typed_agents() {
        let claude = json!({"type":"result","subtype":"error_during_execution","is_error":true,
            "duration_ms":1,"duration_api_ms":1,"num_turns":1,"session_id":"s",
            "total_cost_usd":0.0,"usage":{"input_tokens":0,"output_tokens":0}});
        let codex = json!({"type":"turn.failed","error":{"message":"execution failed"}});
        let muse = muse_record(
            "run.terminal.failed",
            json!({"kind":"run_terminal","command_id":"c","run_stream":{"kind":"run","id":"r"},"terminal":"failed","reason":"failure"}),
        );
        for (agent, value) in [("claude", claude), ("codex", codex), ("muse", muse)] {
            let result = classify(agent, &value.to_string());
            assert_eq!(result.kind, Some("error"), "{agent}");
            assert_eq!(result.activity, Some(SessionActivityState::Idle), "{agent}");
        }
    }
    fn muse_record(kind: &str, payload: serde_json::Value) -> serde_json::Value {
        json!({"schema_version":1,"id":"m","stream":{"kind":"run","id":"r"},"sequence":1,"recorded_at":1,"record_type":"event","durability":"durable","causation_id":"c","payload_type":kind,"payload_schema_version":1,"payload":payload})
    }
    #[test]
    fn typed_starts_and_completions_replay_but_unknown_frames_never_claim_idle() {
        assert_eq!(
            classify("codex", r#"{"type":"turn.started"}"#).activity,
            Some(SessionActivityState::Busy)
        );
        assert_eq!(classify("codex", r#"{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":2}}"#).kind, Some("status"));
        for (agent, body) in [
            ("claude", "not json"),
            ("codex", r#"{"type":"future.terminal"}"#),
            ("muse", r#"{"terminal":"completed"}"#),
            ("unknown", r#"{"type":"turn.completed"}"#),
        ] {
            assert_eq!(classify(agent, body).activity, None);
        }
    }
    #[test]
    fn only_rejected_limits_are_throttled() {
        for status in ["allowed", "allowed_warning", "rejected", "future"] {
            let value = json!({"type":"rate_limit_event","session_id":"s","rate_limit_info":{"status":status}});
            assert_eq!(
                classify("claude", &value.to_string()).activity,
                (status == "rejected").then_some(SessionActivityState::Throttled)
            );
        }
    }
}
