//! Drive [`CodexClassifier`] from `codex_io_task` and emit the resulting
//! neutral [`IoEvent::Classified`] decisions.
//!
//! The `ServerMessage` → neutral-output mapping lives in
//! [`CodexClassifier`](crate::classifier::CodexClassifier) (the single source
//! of Codex output classification, #1165 item 2). This module is the thin
//! adapter that runs it and forwards each [`AgentOutput`] verbatim as
//! `IoEvent::Classified` — the same neutral channel Claude uses, so `Session`
//! has no codex-specific output/permission arms.
//!
//! It also owns the one carve-out the classifier deliberately skips:
//! `TurnCompleted`. That frame carries token usage and drives per-turn metrics
//! finalization — turn/token orchestration owned by `codex_io_task` — so the
//! `turn.completed` event (with usage) is shaped here (as a `Visible`
//! decision), and `turn_ended` is returned to the I/O task's turn-lifecycle
//! loop.

use codex_codes::{Notification, ServerMessage};
use session_lib::io::IoEvent;
use session_lib::{AgentOutput, AgentOutputClassifier};
use tokio::sync::mpsc;

use crate::classifier::CodexClassifier;
use crate::events::{to_raw_output, CodexUsageEvent, ErrorEvent, TurnCompletedEvent};

/// Classify a Codex app-server ServerMessage and emit neutral
/// [`IoEvent::Classified`] decisions. Returns (event_sent_ok, turn_ended).
pub(crate) fn handle_codex_server_message(
    msg: ServerMessage,
    event_tx: &mpsc::UnboundedSender<IoEvent>,
    latest_token_usage: Option<&CodexUsageEvent>,
) -> (bool, bool) {
    // Turn completion stays here: it carries token usage + drives per-turn
    // metrics, which is turn/token orchestration owned by the I/O task.
    // `CodexClassifier` deliberately returns `Noop` for it, so shape the
    // user-visible `turn.completed` event here and emit it as `Visible`.
    if let ServerMessage::Notification(Notification::TurnCompleted(p)) = &msg {
        // Interrupted turns can carry an error too (Guardian's denial limit).
        // Surface the terminal explanation regardless of status, while keeping
        // the completion event that drives usage and turn lifecycle handling.
        let error_sent = p.turn.error.as_ref().is_none_or(|error| {
            event_tx
                .send(IoEvent::Classified(AgentOutput::Visible(to_raw_output(
                    &ErrorEvent::new(error.message.clone()),
                ))))
                .is_ok()
        });
        let event = TurnCompletedEvent::new(
            p.turn.id.clone(),
            turn_status_label(&p.turn.status).to_string(),
            p.turn.duration_ms,
            latest_token_usage.cloned(),
        );
        let ok = event_tx
            .send(IoEvent::Classified(AgentOutput::Visible(to_raw_output(
                &event,
            ))))
            .is_ok();
        return (ok && error_sent, true);
    }

    // Everything else: the classifier is the single mapping source; forward its
    // neutral decisions straight through (`Session` maps Visible→RawOutput,
    // PermissionRequest→PermissionRequest, Noop→skip — same as Claude).
    let mut classifier = CodexClassifier;
    let mut ok = true;
    for output in classifier.classify(msg) {
        if event_tx.send(IoEvent::Classified(output)).is_err() {
            ok = false;
        }
    }
    (ok, false)
}

fn turn_status_label(status: &codex_codes::TurnStatus) -> &'static str {
    match status {
        codex_codes::TurnStatus::Completed => "completed",
        codex_codes::TurnStatus::Interrupted => "interrupted",
        codex_codes::TurnStatus::Failed => "failed",
        codex_codes::TurnStatus::InProgress => "in_progress",
    }
}

#[cfg(test)]
mod tests {
    //! Handler-level behavior: the `TurnCompleted` carve-out (with usage) and
    //! that every frame is forwarded as a neutral `IoEvent::Classified`. The
    //! per-variant `ServerMessage` → `AgentOutput` mapping is characterized in
    //! `classifier.rs`; these tests only cover what the handler adds.
    use super::*;
    use codex_codes::{Notification, RequestId, ServerMessage, ServerRequest};
    use serde_json::json;

    /// Collect the `AgentOutput`s the handler emits (each wrapped in
    /// `IoEvent::Classified`); panics if it ever emits a non-Classified event.
    fn classified(
        msg: ServerMessage,
        usage: Option<&CodexUsageEvent>,
    ) -> (Vec<AgentOutput>, bool, bool) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (sent, ended) = handle_codex_server_message(msg, &tx, usage);
        drop(tx);
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            match ev {
                IoEvent::Classified(ao) => out.push(ao),
                _ => panic!("expected IoEvent::Classified"),
            }
        }
        (out, sent, ended)
    }

    #[test]
    fn turn_completed_emits_visible_with_usage_and_ends_turn() {
        let notif: codex_codes::TurnCompletedNotification = serde_json::from_value(json!({
            "threadId": "thread-1",
            "turn": { "id": "turn-1", "status": "completed", "durationMs": 4200, "items": [] }
        }))
        .unwrap();
        let usage = CodexUsageEvent {
            last: codex_codes::TokenUsageBreakdown {
                input_tokens: 100,
                cached_input_tokens: 25,
                cache_write_input_tokens: Some(0),
                output_tokens: 40,
                reasoning_output_tokens: 7,
                total_tokens: 147,
            },
            total: codex_codes::TokenUsageBreakdown {
                input_tokens: 300,
                cached_input_tokens: 75,
                cache_write_input_tokens: Some(0),
                output_tokens: 90,
                reasoning_output_tokens: 17,
                total_tokens: 407,
            },
            model_context_window: Some(200000),
        };
        let msg = ServerMessage::Notification(Notification::TurnCompleted(notif));
        let (outputs, sent, ended) = classified(msg, Some(&usage));

        assert!(sent);
        assert!(ended, "TurnCompleted must signal turn_ended=true");
        assert_eq!(outputs.len(), 1);
        match &outputs[0] {
            AgentOutput::Visible(value) => {
                assert_eq!(value["type"], "turn.completed");
                assert_eq!(value["turn_id"], "turn-1");
                assert_eq!(value["status"], "completed");
                assert_eq!(value["duration_ms"], 4200);
                assert_eq!(value["usage"]["last"]["inputTokens"], 100);
                assert_eq!(value["usage"]["model_context_window"], 200000);
            }
            other => panic!("expected Visible, got {other:?}"),
        }
    }

    #[test]
    fn terminal_errors_are_visible_for_interrupted_and_failed_turns() {
        for status in ["interrupted", "failed"] {
            let wire = format!(
                r#"{{"threadId":"thread-1","turn":{{"id":"turn-1","status":"{status}","items":[],"error":{{"message":"Guardian denial limit reached","codexErrorInfo":"tooManyDenials"}}}}}}"#
            );
            let notification = serde_json::from_str(&wire).unwrap();
            let msg = ServerMessage::Notification(Notification::TurnCompleted(notification));
            let (outputs, sent, ended) = classified(msg, None);
            assert!(sent && ended);
            assert_eq!(outputs.len(), 2);
            match &outputs[0] {
                AgentOutput::Visible(value) => {
                    assert_eq!(value["type"], "error");
                    assert_eq!(value["message"], "Guardian denial limit reached");
                }
                other => panic!("expected visible terminal error, got {other:?}"),
            }
            match &outputs[1] {
                AgentOutput::Visible(value) => {
                    assert_eq!(value["type"], "turn.completed");
                    assert_eq!(value["status"], status);
                    assert_eq!(value["turn_id"], "turn-1");
                }
                other => panic!("expected turn completion, got {other:?}"),
            }
        }
    }

    /// A newer app-server error code must not discard the terminal frame:
    /// the error stays visible and the session still leaves its active turn.
    #[test]
    fn unknown_terminal_error_codes_still_end_the_turn() {
        for code in [r#""futureError""#, r#"{"futureError":{"detail":"new"}}"#] {
            let wire = format!(
                r#"{{"method":"turn/completed","params":{{"threadId":"thread-1","turn":{{"id":"turn-1","status":"failed","items":[],"error":{{"message":"Future server failure","codexErrorInfo":{code}}}}}}}}}"#
            );
            let msg = ServerMessage::from_json_str(&wire).unwrap();
            let (outputs, sent, ended) = classified(msg, None);
            assert!(sent && ended);
            assert_eq!(outputs.len(), 2);
            match &outputs[0] {
                AgentOutput::Visible(value) => {
                    assert_eq!(value["type"], "error");
                    assert_eq!(value["message"], "Future server failure");
                }
                other => panic!("expected visible terminal error, got {other:?}"),
            }
            match &outputs[1] {
                AgentOutput::Visible(value) => {
                    assert_eq!(value["type"], "turn.completed");
                    assert_eq!(value["status"], "failed");
                }
                other => panic!("expected turn completion, got {other:?}"),
            }
        }
    }

    #[test]
    fn permission_request_forwarded_as_classified() {
        let req: codex_codes::FileChangeRequestApprovalParams = serde_json::from_value(json!({
            "itemId": "item-1",
            "reason": "writes /etc/passwd",
            "threadId": "t1",
            "turnId": "tu1",
            "startedAtMs": 0
        }))
        .unwrap();
        let msg = ServerMessage::Request {
            id: RequestId::Integer(7),
            request: ServerRequest::FileChangeApproval(req),
        };
        let (outputs, _, ended) = classified(msg, None);
        assert!(!ended);
        assert_eq!(outputs.len(), 1);
        match &outputs[0] {
            AgentOutput::PermissionRequest {
                request_id,
                tool_name,
                input,
                ..
            } => {
                assert_eq!(request_id, "7");
                assert_eq!(tool_name, "FileChange");
                assert_eq!(input["tool"], "fileChange");
                assert_eq!(input["itemId"], "item-1");
            }
            other => panic!("expected PermissionRequest, got {other:?}"),
        }
    }

    #[test]
    fn visible_notification_forwarded_as_classified() {
        let notif: codex_codes::ContextCompactedNotification = serde_json::from_value(json!({
            "threadId": "thread-1",
            "turnId": "turn-1"
        }))
        .unwrap();
        let msg = ServerMessage::Notification(Notification::ContextCompacted(notif));
        let (outputs, sent, ended) = classified(msg, None);
        assert!(sent);
        assert!(!ended);
        assert_eq!(outputs.len(), 1);
        match &outputs[0] {
            AgentOutput::Visible(value) => assert_eq!(value["type"], "thread/compacted"),
            other => panic!("expected Visible, got {other:?}"),
        }
    }
}
