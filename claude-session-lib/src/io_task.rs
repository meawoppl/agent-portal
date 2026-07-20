//! Background task for Claude sessions.
//!
//! Owns the [`ClaudeAsyncClient`] exclusively, draining stdout into neutral
//! [`IoEvent::Classified`] decisions and translating [`IoCommand::UserInput`] /
//! [`IoCommand::Permission`] into claude's wire form on stdin. Also implements
//! the upstream-429 rate-limit
//! turn-retry state machine (see the `RATE_LIMIT_TEXT_PREFIX` comment
//! below).

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use chrono::{NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use claude_codes::io::{ContentBlock, ControlResponse, PermissionResult};
use claude_codes::{AsyncClient as ClaudeAsyncClient, ClaudeInput, ClaudeOutput};
use rand::Rng;
use session_lib::error::SessionError;
use session_lib::io::{IoCommand, IoEvent};
use session_lib::{
    AgentOutputClassifier, ClaudeAdapter, PermissionDecision, TurnOutcome, TurnTracker,
};
use shared::PortalMessage;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Build a Claude `ControlResponse` from a neutral [`PermissionDecision`].
///
/// Mirrors the allow/deny/remember mapping the generic `Session` used to do
/// inline; it now lives at the Claude edge since the I/O task owns the typed
/// client (#1165 item 2, phase A slice 2). `decision.remember` is opaque JSON
/// (serialized `claude_codes::Permission`s) — the Value-based allow handles it.
fn claude_control_response(request_id: &str, decision: PermissionDecision) -> ControlResponse {
    if decision.allow {
        let input_value = decision
            .modified_input
            .unwrap_or(serde_json::Value::Object(Default::default()));
        if decision.remember.is_empty() {
            ControlResponse::from_result(request_id, PermissionResult::allow(input_value))
        } else {
            ControlResponse::from_result(
                request_id,
                PermissionResult::allow_with_permissions(input_value, decision.remember),
            )
        }
    } else {
        let reason = decision.reason.unwrap_or_else(|| "User denied".to_string());
        ControlResponse::from_result(request_id, PermissionResult::deny(reason))
    }
}

const SESSION_LIMIT_TEXT: &str = "You've hit your session limit";
const SESSION_LIMIT_CONTINUE_PROMPT: &str =
    "Continue from where you stopped before the Claude session limit was reached.";

#[derive(Debug)]
struct EchoDropMarker {
    expected_text: String,
}

struct ClaudeUserInputSend<'a> {
    client: &'a mut ClaudeAsyncClient,
    session_id: Uuid,
    text: String,
    delivered: Option<oneshot::Sender<Result<(), String>>>,
    display_event: Option<Box<serde_json::Value>>,
    event_tx: &'a mpsc::UnboundedSender<IoEvent>,
    pending_echo_drops: &'a mut VecDeque<EchoDropMarker>,
    synthesize_visible_echo: bool,
}

async fn send_claude_user_input(
    args: ClaudeUserInputSend<'_>,
) -> Result<ClaudeInput, claude_codes::Error> {
    let ClaudeUserInputSend {
        client,
        session_id,
        text,
        delivered,
        display_event,
        event_tx,
        pending_echo_drops,
        synthesize_visible_echo,
    } = args;
    let input = ClaudeInput::user_message(text.clone(), session_id);
    let result = client.send(&input).await;

    if result.is_ok() {
        pending_echo_drops.push_back(EchoDropMarker {
            expected_text: text.clone(),
        });
        if synthesize_visible_echo {
            if let Some(value) = synthetic_user_echo_value(&text, display_event, session_id) {
                let _ = event_tx.send(IoEvent::RawOutput(value));
            }
        }
    }

    if let Some(delivered) = delivered {
        let _ = delivered.send(result.as_ref().map(|_| ()).map_err(|e| e.to_string()));
    }

    result.map(|_| input)
}

pub(crate) fn claude_user_echo_value(text: String, session_id: Uuid) -> serde_json::Value {
    let input = ClaudeInput::user_message(text, session_id);
    let ClaudeInput::User(user) = input else {
        return serde_json::Value::Null;
    };
    serde_json::to_value(ClaudeOutput::User(user)).unwrap_or_default()
}

fn synthetic_user_echo_value(
    text: &str,
    display_event: Option<Box<serde_json::Value>>,
    session_id: Uuid,
) -> Option<serde_json::Value> {
    if let Some(event) = display_event {
        return Some(*event);
    }
    if should_suppress_synthetic_user_echo(text) {
        return None;
    }
    Some(claude_user_echo_value(text.to_string(), session_id))
}

/// Build the interrupt to write to claude's stdin as a wrapped `control_request`.
///
/// TODO(SDK meawoppl/rust-code-agent-sdks#218): `claude_codes::ClaudeInput::interrupt()`
/// (claude-codes 2.1.161) serializes to a bare `{"subtype":"interrupt"}`, which the
/// current claude CLI silently ignores (verified against claude 2.1.211 — the in-flight
/// turn is never cancelled). The CLI's control protocol requires the interrupt wrapped as
/// a `control_request` with a unique `request_id`, the same envelope as `can_use_tool` /
/// `initialize`. There's no typed constructor for it today, so hand-build the JSON and
/// send it as `ClaudeInput::Raw`; revert to `ClaudeInput::interrupt()` once #218 lands.
fn interrupt_input() -> ClaudeInput {
    #[derive(serde::Serialize)]
    struct Request {
        #[serde(rename = "type")]
        message_type: &'static str,
        request_id: String,
        request: Payload,
    }
    #[derive(serde::Serialize)]
    struct Payload {
        subtype: &'static str,
    }
    let value = serde_json::to_value(Request {
        message_type: "control_request",
        request_id: format!("interrupt-{}", uuid::Uuid::new_v4()),
        request: Payload {
            subtype: "interrupt",
        },
    })
    .unwrap_or(serde_json::Value::Null);
    ClaudeInput::Raw(value)
}

fn plain_input_text(input: &ClaudeInput) -> Option<String> {
    let ClaudeInput::User(user) = input else {
        return None;
    };
    let mut text = String::new();
    let mut saw_text = false;
    for block in &user.message.content {
        match block {
            ContentBlock::Text(t) => {
                saw_text = true;
                text.push_str(&t.text);
            }
            _ => return None,
        }
    }
    saw_text.then_some(text)
}

fn should_suppress_synthetic_user_echo(text: &str) -> bool {
    text.trim().is_empty() || is_system_reminder_text(text)
}

fn is_system_reminder_text(text: &str) -> bool {
    text.trim_start().starts_with("<system-reminder>")
}

fn plain_user_echo_text(output: &ClaudeOutput) -> Option<String> {
    let ClaudeOutput::User(user) = output else {
        return None;
    };
    if user.tool_use_result.is_some() {
        return None;
    }

    let mut saw_text = false;
    let mut text = String::new();
    for block in &user.message.content {
        match block {
            ContentBlock::Text(t) => {
                saw_text = true;
                text.push_str(&t.text);
            }
            // Tool results, images, and other structured user frames are real
            // agent output, not CLI echoes of stdin.
            _ => return None,
        }
    }

    saw_text.then_some(text)
}

fn truncate_for_log(text: &str) -> String {
    const LIMIT: usize = 120;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    format!("{}...", text.chars().take(LIMIT).collect::<String>())
}

// Detector for the upstream-429 turn shape. When Anthropic's API rate-limits a
// request, `claude --print` doesn't fail — it streams an assistant message
// whose first text block starts with this prefix, then emits a Result with
// is_error=true. We retry that turn for the user with full-jitter exponential
// backoff.
const RATE_LIMIT_TEXT_PREFIX: &str = "API Error: Server is temporarily limiting requests";
const MAX_RATE_LIMIT_RETRIES: u32 = 30;
const RATE_LIMIT_BACKOFF_CAP_SECS: u64 = 60;

/// Decision produced at a rate-limited turn's error terminator. The RNG-drawn
/// backoff delay is intentionally left to the caller (the state machine stays
/// deterministic); it only reports the backoff *window* to draw from.
#[derive(Debug)]
enum RateLimitDecision {
    /// No prior input to re-issue — do nothing.
    NoInput,
    /// Retry budget exhausted after `attempts` consecutive rate-limited turns;
    /// the caller surfaces the give-up notice. The counter has been reset.
    GiveUp { attempts: u32 },
    /// Re-issue `input` after a full-jitter backoff drawn from `[0, exp_cap]`
    /// seconds; `attempt`/`max` are for the user-facing retry notice.
    Retry {
        input: Box<ClaudeInput>,
        exp_cap: u64,
        attempt: u32,
        max: u32,
    },
}

/// Mutable driver state for a Claude session's I/O loop, gathered out of the
/// select-loop so the rate-limit retry machine and the stdin-echo-drop
/// bookkeeping have pure, unit-testable transition methods (issue #917). The
/// loop keeps only channel recv, effect execution, and claude-client I/O.
///
/// Deliberately excludes the metrics/token trackers (`TurnTracker`, the
/// subagent rollup, model/tier latches): those are already cohesive and their
/// updates are interleaved with client reads, so folding them in would not add
/// testability.
#[derive(Default)]
struct ClaudeIoState {
    /// The most recent user input we sent — kept so we can re-issue it on a
    /// rate-limited turn without bothering the user.
    last_input: Option<ClaudeInput>,
    /// Consecutive rate-limited turns. Reset on success, on fresh user input,
    /// and after a max-out give-up.
    rate_limit_attempts: u32,
    /// Set while an assistant frame in the current turn matched the rate-limit
    /// text prefix. Latched until the turn's Result frame so we can classify the
    /// whole turn on its terminator.
    current_turn_was_rate_limited: bool,
    /// Parsed `(reset_at_rfc3339, source_message)` when the current turn hit the
    /// weekly session limit; consumed on the error terminator.
    pending_session_limit: Option<(String, String)>,
    /// FIFO markers for CLI stdin echoes we expect to drop from the transcript
    /// (claude re-emits each user message we send it as a `user` frame).
    pending_echo_drops: VecDeque<EchoDropMarker>,
}

impl ClaudeIoState {
    fn set_last_input(&mut self, input: ClaudeInput) {
        self.last_input = Some(input);
    }

    fn current_turn_was_rate_limited(&self) -> bool {
        self.current_turn_was_rate_limited
    }

    fn has_pending_session_limit(&self) -> bool {
        self.pending_session_limit.is_some()
    }

    /// An assistant frame's first text block matched the upstream-429 prefix.
    fn note_rate_limited_turn(&mut self) {
        self.current_turn_was_rate_limited = true;
    }

    /// An assistant frame carried a parseable weekly-session-limit banner.
    fn note_session_limit(&mut self, reset_at: String, source_message: String) {
        self.pending_session_limit = Some((reset_at, source_message));
    }

    /// Clear the per-turn retry/limit latches. Shared by fresh user input, a
    /// successful (non-error) Result, and retry cancellation (a new command
    /// arriving mid-backoff) — all of which reset the same three fields.
    fn reset_turn_retry_state(&mut self) {
        self.rate_limit_attempts = 0;
        self.current_turn_was_rate_limited = false;
        self.pending_session_limit = None;
    }

    /// Consume the pending session-limit banner (emitted once on the error
    /// terminator).
    fn take_session_limit(&mut self) -> Option<(String, String)> {
        self.pending_session_limit.take()
    }

    /// Decide what to do at a rate-limited turn's error terminator. Only called
    /// when the turn's error Result was rate-limited; clears the per-turn
    /// rate-limit latch, then advances the retry budget.
    fn on_rate_limit_terminator(&mut self) -> RateLimitDecision {
        self.current_turn_was_rate_limited = false;
        let Some(input) = self.last_input.clone() else {
            return RateLimitDecision::NoInput;
        };
        if self.rate_limit_attempts >= MAX_RATE_LIMIT_RETRIES {
            let attempts = self.rate_limit_attempts;
            self.rate_limit_attempts = 0;
            return RateLimitDecision::GiveUp { attempts };
        }
        self.rate_limit_attempts += 1;
        // Full-jitter exponential backoff:
        //   delay ∈ [0, min(cap, 2^attempt)] seconds.
        // attempt=1 -> [0,2]; 2 -> [0,4]; 3 -> [0,8]; 4 -> [0,16]; the cap
        // saturates by attempt 5 but we max-out before that.
        let exp_cap = 2u64
            .saturating_pow(self.rate_limit_attempts)
            .min(RATE_LIMIT_BACKOFF_CAP_SECS);
        RateLimitDecision::Retry {
            input: Box::new(input),
            exp_cap,
            attempt: self.rate_limit_attempts,
            max: MAX_RATE_LIMIT_RETRIES,
        }
    }

    /// Record that we sent `text` on stdin — claude will echo it back as a
    /// `user` frame we must drop from the transcript.
    fn push_echo_drop(&mut self, expected_text: String) {
        self.pending_echo_drops
            .push_back(EchoDropMarker { expected_text });
    }

    /// Whether an inbound `user` frame is a CLI echo of stdin we should drop.
    /// Consumes the matching pending marker (FIFO); unconditional drops
    /// (system-reminders, empty) don't consume a marker.
    fn should_drop_user_echo(&mut self, output: &ClaudeOutput) -> bool {
        let Some(echoed_text) = plain_user_echo_text(output) else {
            return false;
        };

        if is_system_reminder_text(&echoed_text) {
            tracing::debug!("Dropping system-reminder user echo from transcript");
            return true;
        }

        if echoed_text.trim().is_empty() {
            tracing::debug!("Dropping empty user echo from transcript");
            return true;
        }

        if let Some(marker) = self.pending_echo_drops.pop_front() {
            if marker.expected_text != echoed_text {
                tracing::warn!(
                    "Dropping Claude user echo with mismatched text; expected={}, echoed={}",
                    truncate_for_log(&marker.expected_text),
                    truncate_for_log(&echoed_text)
                );
            }
            return true;
        }

        false
    }

    /// Drop any echo markers that never matched by the turn boundary — the CLI
    /// coalesced/omitted the echo, so a stale marker would wrongly swallow the
    /// next real user frame.
    fn clear_stale_echo_markers(&mut self) {
        let count = self.pending_echo_drops.len();
        if count == 0 {
            return;
        }
        self.pending_echo_drops.clear();
        tracing::warn!(
            "Cleared {} pending Claude user echo drop marker(s) at turn boundary",
            count
        );
    }
}

/// Background task that owns the Claude process and handles all I/O.
///
/// This task:
/// - Continuously reads stdout to prevent OS pipe buffer overflow.
/// - Processes commands from the command channel to send input to Claude.
///
/// By owning the client exclusively, we avoid deadlocks that would occur
/// if we tried to share it between tasks with a mutex.
pub(crate) async fn claude_io_task(
    session_id: Uuid,
    mut client: ClaudeAsyncClient,
    mut command_rx: mpsc::UnboundedReceiver<IoCommand>,
    event_tx: mpsc::UnboundedSender<IoEvent>,
) {
    // Take stderr so we can read it if Claude exits unexpectedly.
    let mut stderr_reader = client.take_stderr();

    // Rate-limit retry machine + stdin-echo-drop bookkeeping, gathered into a
    // state struct with pure transition methods (issue #917).
    let mut state = ClaudeIoState::default();

    // Per-turn performance metrics tracker. `start`ed on each user input,
    // `record_content_frame`d on assistant/text frames, finalized on
    // `ClaudeOutput::Result`. See `session_lib::turn_tracker`.
    let mut turn_tracker = TurnTracker::new(session_id);
    // Model + service tier observed for the current turn. Picked up from
    // `ClaudeOutput::System(Init)` (issued at session start) and the
    // assistant message's `model` field (per-message); the tracker doesn't
    // know either on its own.
    let mut current_model: Option<String> = None;
    let mut current_service_tier: Option<String> = None;
    // Session-lifetime subagent (`Task`) usage rollup (claude-codes 2.1.160,
    // #1275). Unlike the hand-rolled per-turn sum it replaces, the rollup
    // dedupes Task results by agentId — so frames replayed on resume can
    // never double-count. Per-turn attribution is total-at-finalize minus
    // total-at-turn-start; resume-replayed results that arrive BEFORE the
    // first turn starts land outside every turn window.
    let mut subagent_rollup = claude_codes::SubagentUsageRollup::default();
    let mut subagent_tokens_at_turn_start: i64 = 0;

    loop {
        tokio::select! {
            // Handle incoming commands (input to send to Claude).
            Some(cmd) = command_rx.recv() => {
                let result = match cmd {
                    IoCommand::UserInput {
                        text,
                        delivered,
                        display_event,
                    } => {
                        // Each fresh user input gets its own retry budget.
                        state.reset_turn_retry_state();
                        subagent_tokens_at_turn_start = subagent_rollup.subagent_tokens as i64;
                        // Begin per-turn metrics capture. Wall-clock UTC is
                        // chrono::Utc::now(); the monotonic instant is the
                        // anchor for TTFT / total / gap durations.
                        turn_tracker.start(Instant::now(), chrono::Utc::now());
                        let r = send_claude_user_input(ClaudeUserInputSend {
                            client: &mut client,
                            session_id,
                            text,
                            delivered,
                            display_event,
                            event_tx: &event_tx,
                            pending_echo_drops: &mut state.pending_echo_drops,
                            synthesize_visible_echo: true,
                        })
                        .await;
                        if let Ok(input) = &r {
                            state.set_last_input(input.clone());
                        }
                        r.map(|_| ())
                    }
                    IoCommand::Permission {
                        request_id,
                        decision,
                    } => {
                        let response = claude_control_response(&request_id, decision);
                        client.send_control_response(response).await
                    }
                    IoCommand::Interrupt => {
                        // Cancel claude's in-flight turn. Sent as a wrapped
                        // `control_request` (see `interrupt_input`); a no-op on
                        // the CLI side when no turn is active.
                        client.send(&interrupt_input()).await.map(|_| ())
                    }
                };
                if let Err(e) = result {
                    let _ = event_tx.send(IoEvent::Error(SessionError::Agent(e.to_string())));
                }
            }

            // Read output from Claude.
            result = client.receive() => {
                match result {
                    Ok(output) => {
                        if state.should_drop_user_echo(&output) {
                            continue;
                        }

                        // Feed every frame to the subagent rollup (cheap
                        // no-op for non-Task-result frames; agentId-deduped).
                        subagent_rollup.observe(&output);
                        // Classify the frame before forwarding so we can
                        // decide whether the turn's terminator triggers an
                        // auto-retry, and feed the per-turn metrics tracker.
                        match &output {
                            ClaudeOutput::System(sys) => {
                                // Init frames carry the model name — latch
                                // it so we can stamp the turn metrics.
                                if let Some(init) = sys.as_init() {
                                    if init.model.is_some() {
                                        current_model = init.model.clone();
                                    }
                                }
                            }
                            ClaudeOutput::Assistant(asst) => {
                                // Any assistant frame counts as a content
                                // frame for TTFT / inter-token-gap purposes.
                                // The Result frame carries the *terminator*
                                // and is excluded from "content".
                                if turn_tracker.is_running() {
                                    turn_tracker.record_content_frame(Instant::now());
                                }
                                // Refresh per-turn model + service tier from
                                // the most recent assistant frame.
                                if !asst.message.model.is_empty() {
                                    current_model = Some(asst.message.model.clone());
                                }
                                if let Some(usage) = &asst.message.usage {
                                    if let Some(tier) = &usage.service_tier {
                                        current_service_tier = Some(tier.clone());
                                    }
                                }
                                // Count tool-use blocks for the turn.
                                for block in &asst.message.content {
                                    if matches!(
                                        block,
                                        ContentBlock::ToolUse(_)
                                            | ContentBlock::ServerToolUse(_)
                                            | ContentBlock::McpToolUse(_)
                                    ) {
                                        turn_tracker.record_tool_call();
                                    }
                                }
                                let first_text =
                                    asst.message.content.iter().find_map(|b| {
                                        if let ContentBlock::Text(t) = b {
                                            Some(t.text.as_str())
                                        } else {
                                            None
                                        }
                                    });
                                if let Some(text) = first_text {
                                    if text.starts_with(RATE_LIMIT_TEXT_PREFIX) {
                                        state.note_rate_limited_turn();
                                    }
                                    if let Some(reset_at) = parse_session_limit_reset(text) {
                                        state.note_session_limit(reset_at, text.to_string());
                                    }
                                }
                            }
                            ClaudeOutput::Result(r) if !r.is_error => {
                                // Successful turn — clear consecutive-failure state.
                                state.reset_turn_retry_state();
                            }
                            _ => {}
                        }

                        // Finalize the per-turn metrics on the terminator.
                        // Done before the auto-retry branch so the metrics
                        // row reflects this turn, not the eventual retried
                        // turn (which will get its own row when `start` runs
                        // again).
                        if let ClaudeOutput::Result(ref r) = output {
                            state.clear_stale_echo_markers();
                            let usage = r.usage.as_ref();
                            let model = current_model.clone();
                            let outcome = TurnOutcome {
                                agent_type: shared::AgentType::Claude,
                                model,
                                service_tier: current_service_tier.clone().or_else(|| {
                                    usage
                                        .map(|u| u.service_tier.clone())
                                        .filter(|s| !s.is_empty())
                                }),
                                input_tokens: usage
                                    .map(|u| u.input_tokens as i64)
                                    .unwrap_or(0),
                                output_tokens: usage
                                    .map(|u| u.output_tokens as i64)
                                    .unwrap_or(0),
                                cache_creation_tokens: usage
                                    .map(|u| u.cache_creation_input_tokens as i64)
                                    .unwrap_or(0),
                                cache_read_tokens: usage
                                    .map(|u| u.cache_read_input_tokens as i64)
                                    .unwrap_or(0),
                                thinking_tokens: 0,
                                // Subagent (`Task`) tokens attributed to THIS
                                // turn: the session-lifetime rollup's total
                                // minus its value when the turn started —
                                // the `<subagent_tokens>` line the CLI renders
                                // in its terminal `<usage>` (2.1.160, #1275).
                                subagent_tokens: (subagent_rollup.subagent_tokens as i64)
                                    .saturating_sub(subagent_tokens_at_turn_start),
                                stop_reason: r.stop_reason.clone(),
                                is_error: r.is_error,
                                total_cost_usd: Some(r.total_cost_usd),
                            };
                            if let Some(metrics) = turn_tracker.finalize(
                                Instant::now(),
                                chrono::Utc::now(),
                                outcome,
                            ) {
                                if metrics.has_known_model() {
                                    let _ = event_tx
                                        .send(IoEvent::TurnMetricsReady(Box::new(metrics)));
                                } else {
                                    tracing::warn!(
                                        "Claude result completed without model metadata for session {}; dropping turn metrics",
                                        session_id
                                    );
                                }
                            }
                        }

                        let terminator_is_error =
                            matches!(&output, ClaudeOutput::Result(r) if r.is_error);
                        let is_rate_limit_terminator =
                            terminator_is_error && state.current_turn_was_rate_limited();
                        let is_session_limit_terminator =
                            terminator_is_error && state.has_pending_session_limit();

                        // Classify the frame into neutral decisions here (the
                        // boundary that keeps `Session` free of `ClaudeOutput`)
                        // and forward each. `Session` buffers every `Visible`
                        // value before emitting it, preserving ordering.
                        let value = serde_json::to_value(&output).unwrap_or_default();
                        let mut classifier = ClaudeAdapter;
                        let mut send_failed = false;
                        for decision in classifier.classify(value) {
                            if event_tx.send(IoEvent::Classified(decision)).is_err() {
                                // Receiver dropped, session ended.
                                send_failed = true;
                                break;
                            }
                        }
                        if send_failed {
                            break;
                        }

                        if is_session_limit_terminator {
                            if let Some((reset_at, source_message)) = state.take_session_limit() {
                                let _ = event_tx.send(IoEvent::SessionLimitReached {
                                    session_id,
                                    reset_at,
                                    source_message,
                                    prompt: SESSION_LIMIT_CONTINUE_PROMPT.to_string(),
                                });
                            }
                        }

                        if !is_rate_limit_terminator {
                            continue;
                        }

                        // Advance the retry machine on the rate-limited error
                        // terminator; the loop executes the resulting effect.
                        let (input, delay_secs, attempt, max) = match state
                            .on_rate_limit_terminator()
                        {
                            RateLimitDecision::NoInput => continue,
                            RateLimitDecision::GiveUp { attempts } => {
                                let _ = event_tx.send(IoEvent::RawOutput(
                                    PortalMessage::text(format!(
                                        "Rate-limited by upstream API {} times in a row \u{2014} not retrying. Send your message again to try once more.",
                                        attempts
                                    ))
                                    .to_json(),
                                ));
                                continue;
                            }
                            RateLimitDecision::Retry {
                                input,
                                exp_cap,
                                attempt,
                                max,
                            } => {
                                let delay_secs = {
                                    let mut rng = rand::thread_rng();
                                    rng.gen_range(0.0..=exp_cap as f64)
                                };
                                (input, delay_secs, attempt, max)
                            }
                        };
                        let _ = event_tx.send(IoEvent::RawOutput(
                            PortalMessage::text(format!(
                                "Rate-limited by upstream API. Retrying in {:.1}s (attempt {}/{}).",
                                delay_secs, attempt, max
                            ))
                            .to_json(),
                        ));

                        // Wait for the backoff window, but let a new user
                        // input cancel the retry and run normally.
                        let sleep = tokio::time::sleep(Duration::from_secs_f64(delay_secs));
                        tokio::pin!(sleep);
                        tokio::select! {
                            _ = &mut sleep => {
                                // Auto-retry of the same user input — fresh
                                // turn from a metrics standpoint, but tag
                                // the new turn with a stream-restart count
                                // so the dashboard can spot retried turns.
                                subagent_tokens_at_turn_start = subagent_rollup.subagent_tokens as i64;
                                turn_tracker.start(Instant::now(), chrono::Utc::now());
                                turn_tracker.record_stream_restart();
                                if let Err(e) = client.send(&input).await {
                                    let _ = event_tx.send(IoEvent::Error(
                                        SessionError::Agent(e.to_string()),
                                    ));
                                } else if let Some(expected_text) = plain_input_text(&input) {
                                    state.push_echo_drop(expected_text);
                                }
                            }
                            Some(cmd) = command_rx.recv() => {
                                match cmd {
                                    IoCommand::UserInput {
                                        text,
                                        delivered,
                                        display_event,
                                    } => {
                                        // User typed something while we were waiting
                                        // — honor that, abandon the retry, and reset
                                        // the budget for the new prompt. (The
                                        // rate-limit latch was already cleared by
                                        // `on_rate_limit_terminator`.)
                                        state.reset_turn_retry_state();
                                        subagent_tokens_at_turn_start = subagent_rollup.subagent_tokens as i64;
                                        turn_tracker.start(Instant::now(), chrono::Utc::now());
                                        let r = send_claude_user_input(ClaudeUserInputSend {
                                            client: &mut client,
                                            session_id,
                                            text,
                                            delivered,
                                            display_event,
                                            event_tx: &event_tx,
                                            pending_echo_drops: &mut state.pending_echo_drops,
                                            synthesize_visible_echo: true,
                                        })
                                        .await;
                                        match r {
                                            Ok(new_input) => {
                                                state.set_last_input(new_input);
                                            }
                                            Err(e) => {
                                                let _ = event_tx.send(IoEvent::Error(
                                                    SessionError::Agent(e.to_string()),
                                                ));
                                            }
                                        }
                                    }
                                    IoCommand::Permission {
                                        request_id,
                                        decision,
                                    } => {
                                        let response =
                                            claude_control_response(&request_id, decision);
                                        if let Err(e) =
                                            client.send_control_response(response).await
                                        {
                                            let _ = event_tx.send(IoEvent::Error(
                                                SessionError::Agent(e.to_string()),
                                            ));
                                        }
                                    }
                                    IoCommand::Interrupt => {
                                        // Interrupt while waiting to retry a
                                        // rate-limited turn: abandon the retry
                                        // and cancel (a no-op on the CLI if the
                                        // turn isn't running).
                                        state.reset_turn_retry_state();
                                        if let Err(e) =
                                            client.send(&interrupt_input()).await
                                        {
                                            let _ = event_tx.send(IoEvent::Error(
                                                SessionError::Agent(e.to_string()),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        // Deserialization errors are non-fatal: the CLI sent a message
                        // we don't understand yet. Wrap in a portal message so the
                        // frontend renders it cleanly, and keep the session alive.
                        if let claude_codes::Error::Deserialization(ref parse_err) = e {
                            tracing::warn!(
                                "Unparsable message from CLI (session continues): {}",
                                parse_err.error_message
                            );

                            let raw_display = parse_err
                                .raw_json
                                .as_ref()
                                .and_then(|v| serde_json::to_string_pretty(v).ok())
                                .unwrap_or_else(|| parse_err.error_message.clone());

                            let portal_json = PortalMessage::text(format!(
                                "Received an unrecognized message from the agent CLI. \
                                The session will continue normally.\n\n\
                                {}\n\n\
                                If you believe this is a bug, please report it at:\n\
                                https://github.com/meawoppl/rust-code-agent-sdks/issues",
                                raw_display
                            ))
                            .to_json();
                            let _ = event_tx.send(IoEvent::RawOutput(portal_json));
                            continue;
                        }

                        let err_str = e.to_string();
                        if err_str.contains("exit") || err_str.contains("terminated") {
                            let _ = event_tx.send(IoEvent::Exited { code: 1 });
                            break;
                        }
                        // Connection closed is fatal.
                        if matches!(e, claude_codes::Error::ConnectionClosed) {
                            let _ = event_tx.send(IoEvent::Exited { code: 1 });
                            break;
                        }
                        // Try to read stderr for more context.
                        let stderr_output = read_stderr(&mut stderr_reader).await;
                        let enriched_error = if let Some(stderr) = stderr_output {
                            SessionError::CommunicationError(format!(
                                "{}\nClaude stderr: {}",
                                e, stderr
                            ))
                        } else {
                            SessionError::Agent(e.to_string())
                        };
                        if event_tx.send(IoEvent::Error(enriched_error)).is_err() {
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn parse_session_limit_reset(text: &str) -> Option<String> {
    if !text.contains(SESSION_LIMIT_TEXT) {
        return None;
    }

    let after_resets = text.split("resets ").nth(1)?;
    let open = after_resets.find('(')?;
    let close = after_resets[open + 1..].find(')')? + open + 1;
    let time_text = after_resets[..open].trim();
    let tz_text = after_resets[open + 1..close].trim();

    let tz: Tz = tz_text.parse().ok()?;
    let reset_time = parse_limit_time(time_text)?;
    let now_utc = Utc::now();
    let now_local = now_utc.with_timezone(&tz);
    let mut date = now_local.date_naive();

    let local_dt = match tz.from_local_datetime(&date.and_time(reset_time)) {
        chrono::LocalResult::Single(dt) => dt,
        chrono::LocalResult::Ambiguous(earlier, _) => earlier,
        chrono::LocalResult::None => {
            date = date.succ_opt()?;
            match tz.from_local_datetime(&date.and_time(reset_time)) {
                chrono::LocalResult::Single(dt) => dt,
                chrono::LocalResult::Ambiguous(earlier, _) => earlier,
                chrono::LocalResult::None => return None,
            }
        }
    };

    let reset_utc = if local_dt.with_timezone(&Utc) <= now_utc {
        let next_date = date.succ_opt()?;
        match tz.from_local_datetime(&next_date.and_time(reset_time)) {
            chrono::LocalResult::Single(dt) => dt,
            chrono::LocalResult::Ambiguous(earlier, _) => earlier,
            chrono::LocalResult::None => return None,
        }
    } else {
        local_dt
    };

    Some(reset_utc.with_timezone(&Utc).to_rfc3339())
}

fn parse_limit_time(input: &str) -> Option<NaiveTime> {
    let lower = input.trim().to_ascii_lowercase();
    for fmt in ["%-I:%M%P", "%-I:%M %P", "%-I%P", "%-I %P"] {
        if let Ok(time) = NaiveTime::parse_from_str(&lower, fmt) {
            return Some(time);
        }
    }
    None
}

/// Read available stderr output from the Claude process.
async fn read_stderr(
    stderr_reader: &mut Option<tokio::io::BufReader<tokio::process::ChildStderr>>,
) -> Option<String> {
    use tokio::io::AsyncReadExt;

    let reader = stderr_reader.as_mut()?;
    let mut buf = Vec::with_capacity(4096);

    // Use a short timeout — stderr may have data already buffered.
    match tokio::time::timeout(
        std::time::Duration::from_millis(500),
        reader.read_to_end(&mut buf),
    )
    .await
    {
        Ok(Ok(_)) if !buf.is_empty() => {
            let text = String::from_utf8_lossy(&buf).trim().to_string();
            if text.is_empty() {
                None
            } else {
                Some(text)
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- claude_control_response: neutral PermissionDecision → ControlResponse.
    // Ported from the deleted `ClaudeAdapter::permission_response` tests; this is
    // permission hot-path code so the characterization net moves with the logic.

    #[test]
    fn control_response_allow_uses_empty_input_by_default() {
        let resp = claude_control_response(
            "req-1",
            PermissionDecision {
                allow: true,
                ..Default::default()
            },
        );
        let v = serde_json::to_value(&resp).unwrap();
        let inner = &v["response"];
        assert_eq!(inner["subtype"], serde_json::json!("success"));
        assert_eq!(inner["request_id"], serde_json::json!("req-1"));
        assert_eq!(inner["response"]["behavior"], serde_json::json!("allow"));
        // input defaults to an empty object.
        assert_eq!(inner["response"]["updatedInput"], serde_json::json!({}));
        assert!(inner["response"].get("updatedPermissions").is_none());
    }

    #[test]
    fn control_response_allow_with_modified_input_and_remember() {
        let perm =
            serde_json::to_value(claude_codes::Permission::allow_tool("Bash", "npm test")).unwrap();
        let resp = claude_control_response(
            "req-2",
            PermissionDecision {
                allow: true,
                modified_input: Some(serde_json::json!({"command": "npm test"})),
                remember: vec![perm],
                reason: None,
            },
        );
        let result = &serde_json::to_value(&resp).unwrap()["response"]["response"];
        assert_eq!(result["behavior"], serde_json::json!("allow"));
        assert_eq!(
            result["updatedInput"],
            serde_json::json!({"command": "npm test"})
        );
        assert_eq!(
            result["updatedPermissions"][0]["type"],
            serde_json::json!("addRules")
        );
        assert_eq!(
            result["updatedPermissions"][0]["rules"][0]["toolName"],
            serde_json::json!("Bash")
        );
    }

    #[test]
    fn control_response_deny_uses_reason_with_default() {
        let explicit = serde_json::to_value(claude_control_response(
            "req-3",
            PermissionDecision {
                allow: false,
                reason: Some("nope".to_string()),
                ..Default::default()
            },
        ))
        .unwrap();
        assert_eq!(
            explicit["response"]["response"]["behavior"],
            serde_json::json!("deny")
        );
        assert_eq!(
            explicit["response"]["response"]["message"],
            serde_json::json!("nope")
        );

        // Default reason mirrors the old respond_permission "User denied".
        let defaulted = serde_json::to_value(claude_control_response(
            "req-4",
            PermissionDecision {
                allow: false,
                ..Default::default()
            },
        ))
        .unwrap();
        assert_eq!(
            defaulted["response"]["response"]["message"],
            serde_json::json!("User denied")
        );
    }

    /// Pins the wiring this module relies on: a `Task` tool's result is a
    /// `ClaudeOutput::User` whose `tool_use_result` parses to a typed
    /// `SubagentResult`, exposing the `total_tokens` we sum into the per-turn
    /// subagent rollup (claude-codes 2.1.159, #169). If the SDK reshapes this,
    /// the per-turn `subagent_tokens` would silently fall back to 0 — so lock it.
    #[test]
    fn subagent_result_total_tokens_is_readable_from_task_result_user_frame() {
        let frame = serde_json::json!({
            "type": "user",
            "session_id": "00000000-0000-0000-0000-000000000000",
            "message": { "role": "user", "content": [] },
            "tool_use_result": {
                "status": "completed",
                "agentType": "general-purpose",
                "totalTokens": 12345
            }
        });
        let output: ClaudeOutput = serde_json::from_value(frame).expect("parses as ClaudeOutput");
        let ClaudeOutput::User(user) = output else {
            panic!("expected a user frame");
        };
        let sub = user.subagent_result().expect("typed SubagentResult");
        assert_eq!(sub.total_tokens, Some(12345));
    }

    /// Locks the rollup semantics the per-turn attribution relies on
    /// (claude-codes 2.1.160, #1275): Task results are deduped by agentId,
    /// so a frame replayed on resume contributes zero to a later
    /// turn-boundary diff — and results lacking an agentId/totalTokens
    /// (arbitrary tool_use_result objects) don't count at all.
    #[test]
    fn subagent_rollup_dedupes_replayed_task_results_by_agent_id() {
        let task_result = |agent_id: &str, tokens: u64| {
            serde_json::from_value::<ClaudeOutput>(serde_json::json!({
                "type": "user",
                "session_id": "00000000-0000-0000-0000-000000000000",
                "message": { "role": "user", "content": [] },
                "tool_use_result": {
                    "status": "completed",
                    "agentId": agent_id,
                    "totalTokens": tokens
                }
            }))
            .expect("parses as ClaudeOutput")
        };

        let mut rollup = claude_codes::SubagentUsageRollup::default();
        rollup.observe(&task_result("agent-a", 1000));
        assert_eq!(rollup.subagent_tokens, 1000);

        // Turn boundary: snapshot, then replay the SAME result (resume).
        let at_turn_start = rollup.subagent_tokens;
        rollup.observe(&task_result("agent-a", 1000));
        assert_eq!(
            rollup.subagent_tokens, at_turn_start,
            "replayed Task result must not re-count"
        );

        // A genuinely new subagent in this turn is attributed by the diff.
        rollup.observe(&task_result("agent-b", 500));
        assert_eq!(rollup.subagent_tokens - at_turn_start, 500);
    }

    fn user_output_with_content(content: serde_json::Value) -> ClaudeOutput {
        serde_json::from_value(serde_json::json!({
            "type": "user",
            "session_id": "00000000-0000-0000-0000-000000000000",
            "message": {
                "role": "user",
                "content": content,
            },
        }))
        .expect("valid user output")
    }

    fn text_user_output(text: &str) -> ClaudeOutput {
        user_output_with_content(serde_json::json!([
            { "type": "text", "text": text }
        ]))
    }

    #[test]
    fn claude_user_echo_value_roundtrips_as_plain_user_output() {
        let value = claude_user_echo_value("hello portal".to_string(), Uuid::nil());
        let output: ClaudeOutput = serde_json::from_value(value).expect("synthetic output parses");
        assert_eq!(
            plain_user_echo_text(&output).as_deref(),
            Some("hello portal")
        );
    }

    #[test]
    fn synthetic_user_echo_prefers_display_event_and_suppresses_private_prompts() {
        let display = serde_json::json!({"type": "portal", "message": "agent card"});
        assert_eq!(
            synthetic_user_echo_value("agent-facing", Some(Box::new(display.clone())), Uuid::nil()),
            Some(display)
        );
        assert!(synthetic_user_echo_value(
            "<system-reminder>hidden</system-reminder>",
            None,
            Uuid::nil()
        )
        .is_none());
        assert!(synthetic_user_echo_value("   \n\t", None, Uuid::nil()).is_none());
    }

    fn state_with_drops(texts: &[&str]) -> ClaudeIoState {
        ClaudeIoState {
            pending_echo_drops: texts
                .iter()
                .map(|t| EchoDropMarker {
                    expected_text: t.to_string(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn pending_echo_drop_markers_are_fifo() {
        let mut state = state_with_drops(&["first", "second"]);

        assert!(state.should_drop_user_echo(&text_user_output("first")));
        assert_eq!(state.pending_echo_drops.len(), 1);
        assert!(state.should_drop_user_echo(&text_user_output("second")));
        assert!(state.pending_echo_drops.is_empty());
    }

    #[test]
    fn pending_echo_marker_drops_mismatched_plain_echo_but_not_future_frames() {
        let mut state = state_with_drops(&["expected"]);

        assert!(state.should_drop_user_echo(&text_user_output("changed by cli")));
        assert!(state.pending_echo_drops.is_empty());
        assert!(!state.should_drop_user_echo(&text_user_output("future legitimate user frame")));
    }

    #[test]
    fn stale_echo_markers_clear_at_turn_boundary() {
        let mut state = state_with_drops(&["never echoed"]);
        state.clear_stale_echo_markers();
        assert!(state.pending_echo_drops.is_empty());
    }

    #[test]
    fn system_reminder_and_empty_user_echoes_drop_without_marker() {
        let mut state = ClaudeIoState::default();
        assert!(state.should_drop_user_echo(&text_user_output(
            "<system-reminder>hidden</system-reminder>"
        )));
        assert!(state.should_drop_user_echo(&text_user_output(" \n\t ")));
    }

    #[test]
    fn unconditional_echo_drops_do_not_consume_pending_marker() {
        let mut state = state_with_drops(&["real prompt"]);

        assert!(state.should_drop_user_echo(&text_user_output(" \n\t ")));
        assert_eq!(state.pending_echo_drops.len(), 1);

        assert!(state.should_drop_user_echo(&text_user_output(
            "<system-reminder>hidden</system-reminder>"
        )));
        assert_eq!(state.pending_echo_drops.len(), 1);

        assert!(state.should_drop_user_echo(&text_user_output("real prompt")));
        assert!(state.pending_echo_drops.is_empty());
    }

    #[test]
    fn structured_user_frames_are_not_plain_echoes() {
        let mut state = state_with_drops(&["next prompt"]);
        let tool_result = user_output_with_content(serde_json::json!([
            {
                "type": "tool_result",
                "tool_use_id": "tool-1",
                "content": "ok"
            }
        ]));
        assert!(!state.should_drop_user_echo(&tool_result));
        assert_eq!(state.pending_echo_drops.len(), 1);

        let no_blocks = user_output_with_content(serde_json::json!([]));
        assert!(!state.should_drop_user_echo(&no_blocks));
        assert_eq!(state.pending_echo_drops.len(), 1);
    }

    #[test]
    fn rate_limit_retry_counter_advances_then_resets_on_fresh_input() {
        // The retry machine, previously buried in the select-loop, is now a
        // pure transition. A fresh user input must reset the retry budget.
        let mut state = ClaudeIoState::default();
        state.set_last_input(ClaudeInput::user_message(
            "do the thing".to_string(),
            Uuid::nil(),
        ));

        // First rate-limited terminator → attempt 1, backoff window [0, 2].
        state.note_rate_limited_turn();
        match state.on_rate_limit_terminator() {
            RateLimitDecision::Retry {
                exp_cap,
                attempt,
                max,
                ..
            } => {
                assert_eq!(attempt, 1);
                assert_eq!(exp_cap, 2);
                assert_eq!(max, MAX_RATE_LIMIT_RETRIES);
            }
            other => panic!("expected Retry, got {other:?}"),
        }
        // The latch was cleared by the terminator.
        assert!(!state.current_turn_was_rate_limited());

        // Second consecutive rate-limited terminator → attempt 2, window [0, 4].
        state.note_rate_limited_turn();
        match state.on_rate_limit_terminator() {
            RateLimitDecision::Retry {
                exp_cap, attempt, ..
            } => {
                assert_eq!(attempt, 2);
                assert_eq!(exp_cap, 4);
            }
            other => panic!("expected Retry, got {other:?}"),
        }

        // A fresh user input resets the budget: the next rate-limited
        // terminator restarts at attempt 1.
        state.reset_turn_retry_state();
        state.note_rate_limited_turn();
        match state.on_rate_limit_terminator() {
            RateLimitDecision::Retry { attempt, .. } => assert_eq!(attempt, 1),
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn rate_limit_terminator_without_prior_input_does_nothing() {
        let mut state = ClaudeIoState::default();
        assert!(matches!(
            state.on_rate_limit_terminator(),
            RateLimitDecision::NoInput
        ));
    }

    #[test]
    fn rate_limit_gives_up_after_max_retries_then_resets_counter() {
        let mut state = ClaudeIoState::default();
        state.set_last_input(ClaudeInput::user_message(
            "retry me".to_string(),
            Uuid::nil(),
        ));

        // Exhaust the budget: attempts climb from 1 to MAX.
        for _ in 0..MAX_RATE_LIMIT_RETRIES {
            assert!(matches!(
                state.on_rate_limit_terminator(),
                RateLimitDecision::Retry { .. }
            ));
        }
        // Now at the cap → give up, reporting the attempt count, and reset.
        match state.on_rate_limit_terminator() {
            RateLimitDecision::GiveUp { attempts } => {
                assert_eq!(attempts, MAX_RATE_LIMIT_RETRIES);
            }
            other => panic!("expected GiveUp, got {other:?}"),
        }
        // Counter reset: a subsequent terminator retries from attempt 1 again.
        match state.on_rate_limit_terminator() {
            RateLimitDecision::Retry { attempt, .. } => assert_eq!(attempt, 1),
            other => panic!("expected Retry, got {other:?}"),
        }
    }

    #[test]
    fn successful_result_clears_session_limit_and_rate_limit_latches() {
        let mut state = ClaudeIoState::default();
        state.note_rate_limited_turn();
        state.note_session_limit("2026-01-01T00:00:00Z".to_string(), "banner".to_string());
        assert!(state.current_turn_was_rate_limited());
        assert!(state.has_pending_session_limit());

        state.reset_turn_retry_state();
        assert!(!state.current_turn_was_rate_limited());
        assert!(!state.has_pending_session_limit());
        assert!(state.take_session_limit().is_none());
    }

    #[test]
    fn parse_limit_time_accepts_the_cli_time_formats() {
        // The CLI renders reset times in a handful of am/pm shapes; each must
        // parse so the continuation timer lands at the right hour.
        // The CLI always emits a minute component ("resets 3:00pm"); these are
        // the shapes parse_limit_time must handle.
        let cases = [
            ("3:00pm", NaiveTime::from_hms_opt(15, 0, 0)),
            ("3:00 pm", NaiveTime::from_hms_opt(15, 0, 0)),
            ("3:00 PM", NaiveTime::from_hms_opt(15, 0, 0)),
            ("12:30am", NaiveTime::from_hms_opt(0, 30, 0)),
            ("11:59pm", NaiveTime::from_hms_opt(23, 59, 0)),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_limit_time(input), expected, "input: {input:?}");
        }
    }

    #[test]
    fn parse_limit_time_rejects_garbage() {
        assert!(parse_limit_time("").is_none());
        assert!(parse_limit_time("noon").is_none());
        assert!(parse_limit_time("25:00pm").is_none());
    }

    #[test]
    fn parse_session_limit_reset_ignores_unrelated_text() {
        assert!(parse_session_limit_reset("just a normal assistant reply").is_none());
    }

    #[test]
    fn parse_session_limit_reset_returns_none_without_resets_clause() {
        // Has the limit banner but no parseable "resets … (TZ)" clause.
        let text = format!("{SESSION_LIMIT_TEXT}. Try again later.");
        assert!(parse_session_limit_reset(&text).is_none());
    }

    #[test]
    fn parse_session_limit_reset_yields_future_rfc3339() {
        // A well-formed banner must produce a valid, future-dated UTC instant
        // (the parser always rolls forward to the next occurrence of the time).
        let text = format!("{SESSION_LIMIT_TEXT}. Your limit resets 3:00pm (America/New_York).");
        let reset = parse_session_limit_reset(&text).expect("should parse a reset time");
        let parsed = chrono::DateTime::parse_from_rfc3339(&reset).expect("valid rfc3339");
        assert!(
            parsed.with_timezone(&Utc) > Utc::now(),
            "reset {reset} should be in the future"
        );
    }
}
