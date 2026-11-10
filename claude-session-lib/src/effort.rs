//! Bounded acknowledgement gate for live Claude effort changes.

use claude_codes::io::{ApplyFlagSettingsRequest, ControlResponsePayload};
use claude_codes::{ClaudeInput, ClaudeOutput, EffortLevel};
use shared::ReasoningEffort;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

pub(crate) const EFFORT_ACK_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct UserInput {
    pub text: String,
    pub delivered: Option<oneshot::Sender<session_lib::io::InputDeliveryResult>>,
    pub display_event: Option<Box<serde_json::Value>>,
}

impl UserInput {
    pub fn reject(self, reason: &str) {
        if let Some(delivered) = self.delivered {
            let _ = delivered.send(Err(
                session_lib::io::InputDeliveryError::RejectedBeforeSend(reason.to_string()),
            ));
        }
    }
}

struct Pending {
    request_id: String,
    deadline: Instant,
    input: UserInput,
    clearing: bool,
    previously_touched: bool,
}

pub(crate) enum PreparedInput {
    Direct(UserInput),
    Configure(Box<ClaudeInput>),
}

#[derive(Default)]
pub(crate) struct EffortController {
    pending: Option<Pending>,
    /// Even a timed-out request might later apply. Auto must clear it before
    /// a later prompt, including after interruption or rejection.
    flag_touched: bool,
}

impl EffortController {
    pub fn prepare(
        &mut self,
        input: UserInput,
        effort: Option<ReasoningEffort>,
    ) -> Result<PreparedInput, String> {
        if self.pending.is_some() {
            let reason =
                "An effort change is still pending. Retry this message after it completes.";
            input.reject(reason);
            return Err(reason.to_string());
        }
        if effort.is_none() && !self.flag_touched {
            return Ok(PreparedInput::Direct(input));
        }
        let settings = match effort {
            None => ApplyFlagSettingsRequest::new().clear_effort_level(),
            Some(ReasoningEffort::Low) => {
                ApplyFlagSettingsRequest::new().effort_level(EffortLevel::Low)
            }
            Some(ReasoningEffort::Medium) => {
                ApplyFlagSettingsRequest::new().effort_level(EffortLevel::Medium)
            }
            Some(ReasoningEffort::High) => {
                ApplyFlagSettingsRequest::new().effort_level(EffortLevel::High)
            }
            Some(ReasoningEffort::Xhigh) => {
                ApplyFlagSettingsRequest::new().effort_level(EffortLevel::Xhigh)
            }
            Some(ReasoningEffort::Minimal) => {
                let reason =
                    "Claude does not support Minimal effort. Choose Auto or Low and retry.";
                input.reject(reason);
                return Err(reason.to_string());
            }
        };
        let request_id = format!("portal-effort-{}", uuid::Uuid::new_v4());
        let request = ClaudeInput::apply_flag_settings(&request_id, settings);
        self.pending = Some(Pending {
            request_id,
            input,
            deadline: Instant::now() + EFFORT_ACK_TIMEOUT,
            clearing: effort.is_none(),
            previously_touched: self.flag_touched,
        });
        self.flag_touched = true;
        Ok(PreparedInput::Configure(Box::new(request)))
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.pending.as_ref().map(|pending| pending.deadline)
    }

    /// Unrelated permission/interrupt responses cannot release the prompt.
    pub fn acknowledge(&mut self, output: &ClaudeOutput) -> Option<Result<UserInput, String>> {
        let ClaudeOutput::ControlResponse(response) = output else {
            return None;
        };
        let (id, error) = match &response.response {
            ControlResponsePayload::Success { request_id, .. } => (request_id, None),
            ControlResponsePayload::Error { request_id, error } => (request_id, Some(error)),
        };
        if self.pending.as_ref()?.request_id != *id {
            return None;
        }
        let pending = self.pending.take()?;
        if let Some(error) = error {
            self.flag_touched = pending.previously_touched;
            let reason = format!("Claude rejected the effort setting: {error}. The prompt was not sent; choose Auto and retry.");
            pending.input.reject(&reason);
            Some(Err(reason))
        } else {
            if pending.clearing {
                self.flag_touched = false;
            }
            Some(Ok(pending.input))
        }
    }

    pub fn cancel(&mut self, reason: &str) -> bool {
        let Some(pending) = self.pending.take() else {
            return false;
        };
        pending.input.reject(reason);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(text: &str) -> UserInput {
        UserInput {
            text: text.into(),
            delivered: None,
            display_event: None,
        }
    }

    fn request_id(action: PreparedInput) -> String {
        let PreparedInput::Configure(input) = action else {
            panic!("settings request")
        };
        let ClaudeInput::ControlRequest(request) = *input else {
            panic!("control request")
        };
        request.request_id
    }

    fn acknowledgement(id: &str, error: Option<&str>) -> ClaudeOutput {
        let response = match error {
            Some(error) => serde_json::json!({"subtype":"error", "request_id":id, "error":error}),
            None => serde_json::json!({"subtype":"success", "request_id":id}),
        };
        serde_json::from_value(serde_json::json!({"type":"control_response", "response":response}))
            .unwrap()
    }

    #[test]
    fn effort_input_waits_for_its_own_acknowledgement_and_auto_clears() {
        let mut controller = EffortController::default();
        assert!(matches!(
            controller.prepare(input("auto"), None).unwrap(),
            PreparedInput::Direct(_)
        ));
        let id = request_id(
            controller
                .prepare(input("high"), Some(ReasoningEffort::High))
                .unwrap(),
        );
        assert!(
            controller
                .deadline()
                .unwrap()
                .duration_since(Instant::now())
                <= EFFORT_ACK_TIMEOUT
        );
        assert!(controller
            .acknowledge(&acknowledgement("unrelated", None))
            .is_none());
        let accepted = controller
            .acknowledge(&acknowledgement(&id, None))
            .unwrap()
            .unwrap();
        assert_eq!(accepted.text, "high");
        let action = controller.prepare(input("auto again"), None).unwrap();
        let PreparedInput::Configure(request) = action else {
            panic!("clear before Auto")
        };
        let value = serde_json::to_value(&request).unwrap();
        assert!(value["request"]["settings"]["effortLevel"].is_null());
        assert!(value["request"]["settings"]
            .as_object()
            .unwrap()
            .contains_key("effortLevel"));
        let ClaudeInput::ControlRequest(request) = *request else {
            panic!("control request")
        };
        controller
            .acknowledge(&acknowledgement(&request.request_id, None))
            .unwrap()
            .unwrap();
        assert!(matches!(
            controller.prepare(input("default"), None).unwrap(),
            PreparedInput::Direct(_)
        ));
    }

    #[test]
    fn rejected_setting_rejects_delivery_and_keeps_auto_usable() {
        let mut controller = EffortController::default();
        let (tx, mut rx) = oneshot::channel();
        let pending_input = UserInput {
            delivered: Some(tx),
            ..input("must not be sent")
        };
        let id = request_id(
            controller
                .prepare(pending_input, Some(ReasoningEffort::High))
                .unwrap(),
        );
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(controller
            .acknowledge(&acknowledgement(&id, Some("unsupported settings")))
            .unwrap()
            .is_err());
        assert!(rx
            .try_recv()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("prompt was not sent"));
        assert!(matches!(
            controller.prepare(input("Auto"), None).unwrap(),
            PreparedInput::Direct(_)
        ));
    }

    #[test]
    fn overlapping_and_timed_out_inputs_are_bounded_and_not_delivered() {
        let mut controller = EffortController::default();
        let (first_tx, mut first_rx) = oneshot::channel();
        let id = request_id(
            controller
                .prepare(
                    UserInput {
                        delivered: Some(first_tx),
                        ..input("first")
                    },
                    Some(ReasoningEffort::Low),
                )
                .unwrap(),
        );
        let (next_tx, mut next_rx) = oneshot::channel();
        assert!(controller
            .prepare(
                UserInput {
                    delivered: Some(next_tx),
                    ..input("second")
                },
                Some(ReasoningEffort::High)
            )
            .is_err());
        assert!(next_rx.try_recv().unwrap().is_err());
        assert!(controller.cancel("timed out"));
        assert!(first_rx.try_recv().unwrap().is_err());
        assert!(
            controller
                .acknowledge(&acknowledgement(&id, None))
                .is_none(),
            "late ack cannot release the timed-out prompt"
        );
        assert!(
            matches!(
                controller.prepare(input("restore Auto"), None).unwrap(),
                PreparedInput::Configure(_)
            ),
            "a late setting might have applied; clear before a new Auto prompt"
        );
    }
}
