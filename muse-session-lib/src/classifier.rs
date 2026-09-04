//! Classifies Muse journal records into neutral [`AgentOutput`] decisions.
//!
//! Muse's stream is an event-sourced journal rather than a message list, so
//! the rules here follow from the envelope, and every one of them was
//! measured against the real CLI (see `docs/MUSE_SUPPORT.md`, "Measured
//! corrections") rather than assumed:
//!
//! - **Durability decides routing, read off the wire.** Records the journal
//!   marks `ephemeral` (streamed output deltas, task-status chatter) become
//!   [`AgentOutput::Ephemeral`] — streamed to the UI, never buffered or
//!   persisted, because their final content arrives separately on a durable
//!   record. Durable records become [`AgentOutput::Visible`]. The branch is
//!   on the envelope's own `durability` field rather than a list of payload
//!   types, so a NEW ephemeral type muse introduces routes correctly
//!   automatically instead of silently landing in the transcript.
//! - **Identity is `(stream_id, id)`.** The record `id` is a UUID-*shaped*
//!   counter that repeats byte-for-byte across sessions, and `sequence`
//!   repeats across turns, so neither is a safe handle alone. Treat `id` as
//!   stream-local **everywhere** — dedup, correlation, and render keys.
//! - **Unknown frames still name themselves.** An unmodeled `payload_type`
//!   is forwarded with its dotted label plus the raw body, never dropped.

use muse_codes::{Durability, MusePayload, MuseRecord, RecordType};
use serde::Serialize;
use session_lib::adapter::{AgentOutput, AgentOutputClassifier};
use std::collections::HashSet;

/// Muse-protocol output classifier.
///
/// Mostly stateless — turn grouping is carried on the wire
/// (`causation_id`), so unlike Codex this needs no request-id ↔ tool map.
/// The state is the screen for muse's content-free bookkeeping tasks:
/// `tbh-reminders` plugin lifecycles (skill/scope/goal/verify reminders,
/// measured at 41–47% of a turn's durable records) and `model.meta.response`
/// lifecycles (one per meta-model call, carrying only a `not_applicable`
/// side-effect and transient status). Both render as nothing, so screening
/// them here as [`AgentOutput::Noop`] removes them from the buffer, the
/// database, and the socket in one place.
///
/// The screen must be stateful because only the `proposed` record names
/// `task_kind` — every later record for that task carries `task_id` alone.
/// And it must *hold* `task.stream.linked` records briefly: linked always
/// precedes `proposed` on the measured wire, so at linked-time the kind is
/// unknowable. A held link is dropped with its reminder, emitted with its
/// real task, and flushed fail-open at the turn terminal if its `proposed`
/// never arrived.
#[derive(Debug, Clone, Default)]
pub struct MuseClassifier {
    /// Task ids whose `proposed` named a `reminder.*` kind.
    reminder_tasks: HashSet<String>,
    /// Task ids whose `proposed` named `model.meta.response`. Muse journals a
    /// full task lifecycle every time it calls the meta model, but the record
    /// carries nothing user-facing: a `side_effect_intent` of
    /// `model.meta.response — policy: not_applicable` plus two transient
    /// status lines (`opening/completed meta model stream attempt 1/10`). The
    /// actual reply lands separately on `run.terminal.*` (rendered as the
    /// answer). Measured at 0/10 tasks carrying any output across every
    /// capture, so screen them as [`AgentOutput::Noop`]. Kept as a distinct set
    /// from `reminder_tasks` because they describe different internal work;
    /// only genuine streamed `output` breaks either kind of task out.
    meta_response_tasks: HashSet<String>,
    /// `task.stream.linked` records awaiting their task's `proposed`
    /// (insertion order; nearly always a single entry).
    pending_links: Vec<(String, MuseRecord)>,
}

impl AgentOutputClassifier for MuseClassifier {
    /// One parsed journal record (one stdout line).
    type Raw = MuseRecord;

    fn classify(&mut self, record: MuseRecord) -> Vec<AgentOutput> {
        self.screen(record)
    }
}

impl MuseClassifier {
    fn screen(&mut self, record: MuseRecord) -> Vec<AgentOutput> {
        // Wire drift is never screened: a decode failure must persist and
        // be seen even if it happens to belong to a reminder task.
        if record.typed_payload().is_err() {
            return vec![classify_record(&record)];
        }

        let payload = &record.payload;
        let event = payload.get("event");
        let event_kind = event.and_then(|e| e.get("kind")).and_then(|k| k.as_str());
        let task_id = event
            .and_then(|e| e.get("task_id"))
            .and_then(|t| t.as_str())
            .or_else(|| payload.get("task_id").and_then(|t| t.as_str()))
            .map(str::to_string);

        // Linked precedes `proposed` (measured), so the kind is unknowable
        // here — hold the record until its task declares itself.
        if record.payload_type == "task.stream.linked" {
            if let Some(id) = task_id {
                self.pending_links.push((id, record));
                return Vec::new();
            }
            return vec![classify_record(&record)];
        }

        if event_kind == Some("proposed") {
            if let Some(id) = task_id {
                let held = self.take_pending(&id);
                let kind = event
                    .and_then(|e| e.get("task_kind"))
                    .and_then(|k| k.as_str());
                if kind.is_some_and(|k| k.starts_with("reminder.")) {
                    // The held link vanishes with its task.
                    self.reminder_tasks.insert(id);
                    return vec![AgentOutput::Noop];
                }
                if kind == Some("model.meta.response") {
                    self.meta_response_tasks.insert(id);
                    return vec![AgentOutput::Noop];
                }
                let mut out = Vec::with_capacity(2);
                if let Some(link) = held {
                    out.push(classify_record(&link));
                }
                out.push(classify_record(&record));
                return out;
            }
        }

        if task_id
            .as_deref()
            .is_some_and(|id| self.reminder_tasks.contains(id))
        {
            // Escape hatch: no capture shows a reminder carrying content,
            // but if Muse ever puts genuine output on one, it must surface.
            // Policy decisions remain internal bookkeeping; rendering them
            // produces repeated `reminder.child_run — policy: ...` noise.
            if event_kind == Some("output") {
                return vec![classify_record(&record)];
            }
            return vec![AgentOutput::Noop];
        }

        if task_id
            .as_deref()
            .is_some_and(|id| self.meta_response_tasks.contains(id))
        {
            // Narrower hatch than reminders: a meta-response's only
            // side-effect is the `not_applicable` audit line we're screening,
            // so only genuine streamed `output` breaks it out — everything
            // else (status, side_effect_intent, lifecycle) is dropped.
            if event_kind == Some("output") {
                return vec![classify_record(&record)];
            }
            return vec![AgentOutput::Noop];
        }

        // Reminder scaffolding identifies itself by *operation* even when its
        // task was never registered above (Muse Code 1.0.x emits the
        // `reminder.child_run` auto-approval under a task id whose `proposed`
        // this classifier may never see). Same rationale as the tracked case:
        // policy bookkeeping renders as repeated
        // "reminder.child_run — policy: …" noise, so screen on the operation
        // name too, not only on task tracking.
        if event_kind == Some("side_effect_intent")
            && event
                .and_then(|e| e.get("operation"))
                .and_then(|o| o.as_str())
                .is_some_and(|op| op.starts_with("reminder."))
        {
            return vec![AgentOutput::Noop];
        }

        // Turn boundary: any link whose `proposed` never arrived (crash,
        // drift) flushes fail-open as visible rather than being held
        // forever.
        if record.payload_type.starts_with("run.terminal.") && !self.pending_links.is_empty() {
            let mut out: Vec<AgentOutput> = std::mem::take(&mut self.pending_links)
                .into_iter()
                .map(|(_, link)| classify_record(&link))
                .collect();
            out.push(classify_record(&record));
            return out;
        }

        vec![classify_record(&record)]
    }

    fn take_pending(&mut self, task_id: &str) -> Option<MuseRecord> {
        let idx = self
            .pending_links
            .iter()
            .position(|(id, _)| id == task_id)?;
        Some(self.pending_links.remove(idx).1)
    }
}

/// Classify a single record. Split out so tests can drive it directly with
/// corpus lines.
pub fn classify_record(record: &MuseRecord) -> AgentOutput {
    // Muse decides tool policy itself in headless runs — there is no
    // approval round-trip on this stream (a `side_effect_intent` records
    // the decision after the fact), so no record ever becomes a
    // PermissionRequest. See MUSE_SUPPORT.md.
    match record.typed_payload() {
        Ok(payload) => classify_payload(record, &payload),
        // A payload that fails its typed shape is real wire drift. Always
        // Visible — never routed by durability: drift must persist and be
        // seen, not silently dropped down the ephemeral channel.
        Err(e) => AgentOutput::Visible(to_value(&MuseDecodeError {
            kind: "muse_decode_error",
            payload_type: &record.payload_type,
            error: e.to_string(),
            raw: &record.payload,
            stream_id: &record.stream.id,
            record_id: &record.id,
        })),
    }
}

/// Drop typed run-setup bookkeeping that has no user-facing renderer before it
/// reaches the buffer, database, or frontend socket. Unknown payloads still
/// fail open through the durability branch below; only SDK-modeled shapes are
/// eligible for suppression.
fn classify_payload(record: &MuseRecord, payload: &MusePayload) -> AgentOutput {
    if matches!(
        payload,
        MusePayload::CommandAccepted(_)
            | MusePayload::SessionRunLinked(_)
            | MusePayload::ModelConfigured(_)
            | MusePayload::RunStarted(_)
    ) {
        return AgentOutput::Noop;
    }

    // Route every remaining record on the wire's own durability flag. Muse is
    // a young protocol and will add kinds; an unlisted ephemeral type must not
    // default into the transcript, while an unknown durable type must remain
    // visible so schema drift is diagnosable.
    match record.durability {
        muse_codes::Durability::Ephemeral => AgentOutput::Ephemeral(to_event(record)),
        muse_codes::Durability::Durable => AgentOutput::Visible(to_event(record)),
    }
}

/// Wire shape the frontend receives for one journal record.
///
/// The envelope is preserved verbatim under `record` so the renderer can
/// key on `(stream_id, id)`, group by `causation_id`, and order within a
/// turn by `sequence` — the identity rules measured in MUSE_SUPPORT.md.
/// `payload_type` is lifted to the top level so a renderer can dispatch (or
/// fall back to a labeled passthrough) without re-parsing.
fn to_event(record: &MuseRecord) -> serde_json::Value {
    to_value(&MuseWireEvent {
        kind: "muse_record",
        payload_type: &record.payload_type,
        stream_id: &record.stream.id,
        record_id: &record.id,
        causation_id: &record.causation_id,
        sequence: record.sequence,
        durability: record.durability,
        record_type: record.record_type,
        recorded_at: record.recorded_at,
        payload: &record.payload,
    })
}

/// One journal record as the frontend receives it.
///
/// The composite identity is spelled out field-by-field so a consumer
/// cannot accidentally key on `record_id` alone: it repeats across sessions
/// (UUID-shaped counter), and `sequence` repeats across turns.
#[derive(Debug, Serialize)]
struct MuseWireEvent<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    payload_type: &'a str,
    stream_id: &'a str,
    record_id: &'a str,
    causation_id: &'a str,
    sequence: u64,
    durability: Durability,
    record_type: RecordType,
    recorded_at: u64,
    payload: &'a serde_json::Value,
}

/// A record whose payload failed its typed shape — forwarded so the drift
/// is visible rather than silently dropped.
#[derive(Debug, Serialize)]
struct MuseDecodeError<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    payload_type: &'a str,
    error: String,
    raw: &'a serde_json::Value,
    stream_id: &'a str,
    record_id: &'a str,
}

/// Serialization of these local structs cannot fail today (no maps with
/// non-string keys, no failing custom impls), so a failure would be a bug
/// in this module rather than a runtime condition.
///
/// Panicking in a session's I/O hot path over a provably-impossible branch
/// would be worse than the impossible thing. But a silent `Value::Null`
/// would be the one place in this file that can go quiet later: if a future
/// refactor adds a non-string-keyed field, the failure would vanish into
/// the stream looking like nothing happened. So the impossible branch
/// surfaces as a visible, persisted error instead — same no-silent-gap rule
/// the rest of this module follows.
fn to_value<T: Serialize>(value: &T) -> serde_json::Value {
    match serde_json::to_value(value) {
        Ok(v) => v,
        Err(e) => serde_json::to_value(SerializeFailure {
            kind: "muse_wire_serialize_error",
            error: e.to_string(),
        })
        .unwrap_or(serde_json::Value::Null),
    }
}

/// Emitted only if serializing a wire event ever fails — see [`to_value`].
#[derive(Debug, Serialize)]
struct SerializeFailure {
    #[serde(rename = "type")]
    kind: &'static str,
    error: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(line: &str) -> MuseRecord {
        serde_json::from_str(line).expect("corpus line parses")
    }

    /// Minimal envelope builder for shape-driven cases.
    fn env(payload_type: &str, durability: &str, payload: serde_json::Value) -> MuseRecord {
        serde_json::from_value(json!({
            "schema_version": 1,
            "id": "018f0000-0000-7000-8000-00000000c350",
            "stream": {"kind": "session", "id": "sess-1"},
            "sequence": 7,
            "recorded_at": 1780531400000000u64,
            "record_type": if durability == "ephemeral" { "status" } else { "event" },
            "durability": durability,
            "causation_id": "cause-1",
            "payload_type": payload_type,
            "payload_schema_version": 1,
            "payload": payload,
        }))
        .expect("envelope builds")
    }

    /// Deltas stream but must never be persisted — the terminal record
    /// carries the final text, so persisting deltas would duplicate the
    /// answer in the transcript.
    #[test]
    fn output_deltas_route_to_the_ephemeral_channel() {
        let r = env(
            "run.output.delta",
            "ephemeral",
            json!({"kind": "run_output_delta", "command_id": "c", "text": "hi",
                   "run_stream": {"kind": "run", "id": "r"}}),
        );
        match classify_record(&r) {
            AgentOutput::Ephemeral(v) => assert_eq!(v["durability"], "ephemeral"),
            other => panic!("deltas must not be persisted, got {other:?}"),
        }
    }

    #[test]
    fn terminal_record_is_visible_transcript() {
        let r = env(
            "run.terminal.completed",
            "durable",
            json!({"kind": "run_terminal", "command_id": "c", "terminal": "completed",
                   "reason": null, "text": "final answer",
                   "run_stream": {"kind": "run", "id": "r"}}),
        );
        assert!(matches!(classify_record(&r), AgentOutput::Visible(_)));
    }

    #[test]
    fn task_status_is_ephemeral_structural_transitions_are_not() {
        let status = env(
            "task.lifecycle.status",
            "ephemeral",
            json!({"kind": "task_lifecycle", "command_id": "c", "task_id": "t",
                   "run_stream": {"kind": "run", "id": "r"},
                   "task_stream": {"kind": "task", "id": "t"},
                   "event": {"kind": "status", "task_id": "t", "message": "opening stream",
                             "details": {"phase": "opening_stream"}}}),
        );
        match classify_record(&status) {
            AgentOutput::Ephemeral(v) => assert_eq!(v["durability"], "ephemeral"),
            other => panic!("status chatter must not be persisted, got {other:?}"),
        }

        let completed = env(
            "task.lifecycle.completed",
            "durable",
            json!({"kind": "task_lifecycle", "command_id": "c", "task_id": "t",
                   "run_stream": {"kind": "run", "id": "r"},
                   "task_stream": {"kind": "task", "id": "t"},
                   "event": {"kind": "completed", "task_id": "t"}}),
        );
        match classify_record(&completed) {
            AgentOutput::Visible(v) => assert_eq!(v["durability"], "durable"),
            other => panic!("structural transitions belong in the transcript, got {other:?}"),
        }
    }

    /// An unmodeled payload type must survive with its label — the
    /// conversation_reset lesson: a frame that renders as unrecognized
    /// should still name itself.
    #[test]
    fn unknown_payload_is_forwarded_with_its_label() {
        let r = env(
            "subagent.lifecycle.spawned",
            "durable",
            json!({"kind": "whatever", "novel_field": 1}),
        );
        match classify_record(&r) {
            AgentOutput::Visible(v) => {
                assert_eq!(v["payload_type"], "subagent.lifecycle.spawned");
                assert_eq!(v["payload"]["novel_field"], 1);
            }
            other => panic!("unknown payloads must stay visible, got {other:?}"),
        }
    }

    /// The event carries the composite identity the renderer needs, and
    /// never invites keying on `id` alone.
    #[test]
    fn event_carries_composite_identity() {
        let r = env(
            "run.terminal.completed",
            "durable",
            json!({"kind": "run_terminal", "command_id": "c", "terminal": "completed",
                   "reason": null, "text": "done",
                   "run_stream": {"kind": "run", "id": "r"}}),
        );
        let AgentOutput::Visible(v) = classify_record(&r) else {
            panic!("expected visible");
        };
        assert_eq!(v["stream_id"], "sess-1");
        assert_eq!(v["record_id"], "018f0000-0000-7000-8000-00000000c350");
        assert_eq!(v["causation_id"], "cause-1");
        assert_eq!(v["sequence"], 7);
    }

    /// Real captured line from `muse exec --json --provider echo`, so the
    /// classifier is exercised against the actual wire and not only
    /// hand-built envelopes.
    #[test]
    fn real_captured_command_acceptance_is_suppressed() {
        let line = r#"{"schema_version":1,"id":"018f0000-0000-7000-8000-00000000c350","stream":{"kind":"session","id":"34c4f817-8c19-4778-a6fc-a9399ac4d034"},"sequence":1,"recorded_at":1780531400000000,"record_type":"reconciliation","durability":"durable","causation_id":"7af312fb-ae7d-40f7-bd79-21cd328c4583","payload_type":"runtime.command.accepted","payload_schema_version":1,"payload":{"client_id":null,"command_id":"7af312fb-ae7d-40f7-bd79-21cd328c4583","command_kind":"turn.submit","kind":"command_accepted"}}"#;
        let r = record(line);
        assert!(matches!(classify_record(&r), AgentOutput::Noop));
    }
}

#[cfg(test)]
mod reminder_screen {
    use super::*;
    use serde_json::json;
    use session_lib::adapter::AgentOutputClassifier;

    fn env(payload_type: &str, payload: serde_json::Value) -> MuseRecord {
        serde_json::from_value(json!({
            "schema_version": 1,
            "id": "018f0000-0000-7000-8000-00000000c350",
            "stream": {"kind": "session", "id": "sess-1"},
            "sequence": 7,
            "recorded_at": 1780531400000000u64,
            "record_type": "event",
            "durability": "durable",
            "causation_id": "cause-1",
            "payload_type": payload_type,
            "payload_schema_version": 1,
            "payload": payload,
        }))
        .expect("envelope builds")
    }

    fn linked(task_id: &str) -> MuseRecord {
        env(
            "task.stream.linked",
            json!({"kind": "task_stream_linked", "command_id": "c", "task_id": task_id,
                   "run_stream": {"kind": "run", "id": "r"},
                   "task_stream": {"kind": "task", "id": task_id}}),
        )
    }

    fn lifecycle(kind: &str, task_id: &str, extra: serde_json::Value) -> MuseRecord {
        let mut ev = json!({"kind": kind, "task_id": task_id});
        if let (Some(e), Some(x)) = (ev.as_object_mut(), extra.as_object()) {
            e.extend(x.clone());
        }
        env(
            &format!("task.lifecycle.{kind}"),
            json!({"kind": "task_lifecycle", "command_id": "c", "task_id": task_id,
                   "run_stream": {"kind": "run", "id": "r"},
                   "task_stream": {"kind": "task", "id": task_id},
                   "event": ev}),
        )
    }

    /// The core sequence, in measured wire order: linked arrives FIRST, so
    /// the screen must hold it until `proposed` names the kind — then drop
    /// both for a reminder, and every later record for that task.
    #[test]
    fn reminder_lifecycle_screens_to_noop_including_the_early_link() {
        let mut c = MuseClassifier::default();
        assert!(
            c.classify(linked("r1")).is_empty(),
            "link is held, not emitted"
        );
        let proposed = lifecycle(
            "proposed",
            "r1",
            json!({"task_kind": "reminder.agent.plugin:tbh-reminders:scope-reminder"}),
        );
        assert!(matches!(c.classify(proposed)[..], [AgentOutput::Noop]));
        assert!(matches!(
            c.classify(lifecycle("started", "r1", json!({})))[..],
            [AgentOutput::Noop]
        ));
        assert!(matches!(
            c.classify(lifecycle("completed", "r1", json!({})))[..],
            [AgentOutput::Noop]
        ));
    }

    /// A real task's held link is emitted with its `proposed`, in wire order.
    #[test]
    fn real_task_link_is_released_with_its_proposed() {
        let mut c = MuseClassifier::default();
        assert!(c.classify(linked("t1")).is_empty());
        // A real work task (not reminder.* / model.meta.response) releases its
        // held link and stays visible.
        let out = c.classify(lifecycle(
            "proposed",
            "t1",
            json!({"task_kind": "tool.bash"}),
        ));
        let types: Vec<_> = out
            .iter()
            .map(|o| match o {
                AgentOutput::Visible(v) => v["payload_type"].as_str().unwrap_or("?").to_string(),
                other => panic!("real task records must stay visible, got {other:?}"),
            })
            .collect();
        assert_eq!(types, ["task.stream.linked", "task.lifecycle.proposed"]);
    }

    /// No capture shows a reminder carrying content — but if one ever does,
    /// it surfaces instead of vanishing with the bookkeeping.
    #[test]
    fn reminder_content_escapes_the_screen() {
        let mut c = MuseClassifier::default();
        c.classify(linked("r1"));
        c.classify(lifecycle(
            "proposed",
            "r1",
            json!({"task_kind": "reminder.agent.plugin:tbh-reminders:goal-reminder"}),
        ));
        let out = c.classify(lifecycle("output", "r1", json!({"chunk": "surprise"})));
        assert!(
            matches!(out[..], [AgentOutput::Visible(_)]),
            "reminder content must surface, got {out:?}"
        );
    }

    #[test]
    fn reminder_child_run_policy_is_screened() {
        let mut c = MuseClassifier::default();
        c.classify(linked("r1"));
        c.classify(lifecycle(
            "proposed",
            "r1",
            json!({"task_kind": "reminder.child_run"}),
        ));
        let policy = lifecycle(
            "side_effect_intent",
            "r1",
            json!({
                "operation": "reminder.child_run",
                "policy_decision": "reminder_child:read_only:subagent_tool_auto_approval"
            }),
        );
        assert!(
            matches!(c.classify(policy)[..], [AgentOutput::Noop]),
            "reminder auto-approval policy is internal bookkeeping"
        );
    }

    #[test]
    fn reminder_operation_is_screened_even_without_task_registration() {
        // Live-captured (Muse Code 1.0.x): the `reminder.child_run`
        // auto-approval can arrive under a task id whose `proposed` this
        // classifier never saw — task tracking alone misses it, and the
        // record rendered as "reminder.child_run — policy: …" noise. The
        // operation name is screened directly.
        let mut c = MuseClassifier::default();
        let policy = lifecycle(
            "side_effect_intent",
            "never-registered",
            json!({
                "operation": "reminder.child_run",
                "policy_decision": "reminder_child:read_only:subagent_tool_auto_approval"
            }),
        );
        assert!(
            matches!(c.classify(policy)[..], [AgentOutput::Noop]),
            "reminder-operation bookkeeping is screened without registration"
        );
        // A non-reminder operation on an unregistered task still passes.
        let other = lifecycle(
            "side_effect_intent",
            "never-registered-2",
            json!({"operation": "tool.exec", "policy_decision": "deny:policy"}),
        );
        assert!(matches!(c.classify(other)[..], [AgentOutput::Visible(_)]));
    }

    #[test]
    fn meta_response_lifecycle_screens_to_noop_including_side_effect() {
        // model.meta.response is muse's "I called the meta model" bookkeeping —
        // its whole lifecycle is dropped, including the `not_applicable`
        // side_effect_intent that is the visible noise, and the transient
        // status lines. The reply itself arrives on run.terminal.* separately.
        let mut c = MuseClassifier::default();
        assert!(c.classify(linked("m1")).is_empty(), "link held");
        let proposed = lifecycle(
            "proposed",
            "m1",
            json!({"task_kind": "model.meta.response"}),
        );
        assert!(matches!(c.classify(proposed)[..], [AgentOutput::Noop]));
        let side_effect = lifecycle(
            "side_effect_intent",
            "m1",
            json!({"operation": "model.meta.response", "policy_decision": "not_applicable"}),
        );
        assert!(
            matches!(c.classify(side_effect)[..], [AgentOutput::Noop]),
            "the not_applicable side-effect is the noise — it must be dropped, unlike a reminder's"
        );
        assert!(matches!(
            c.classify(lifecycle(
                "status",
                "m1",
                json!({"message": "opening meta model stream attempt 1/10"})
            ))[..],
            [AgentOutput::Noop]
        ));
        assert!(matches!(
            c.classify(lifecycle("completed", "m1", json!({})))[..],
            [AgentOutput::Noop]
        ));
    }

    #[test]
    fn meta_response_streamed_output_still_escapes() {
        // Defensive: no capture shows a meta-response carrying streamed output,
        // but if muse ever does, it must surface rather than vanish.
        let mut c = MuseClassifier::default();
        c.classify(linked("m1"));
        c.classify(lifecycle(
            "proposed",
            "m1",
            json!({"task_kind": "model.meta.response"}),
        ));
        let out = c.classify(lifecycle(
            "output",
            "m1",
            json!({"chunk": "unexpected content"}),
        ));
        assert!(
            matches!(out[..], [AgentOutput::Visible(_)]),
            "meta-response streamed output must surface, got {out:?}"
        );
    }

    /// A link whose `proposed` never arrives flushes fail-open at the turn
    /// terminal rather than being held forever.
    #[test]
    fn orphan_link_flushes_at_the_turn_terminal() {
        let mut c = MuseClassifier::default();
        assert!(c.classify(linked("ghost")).is_empty());
        let terminal = env(
            "run.terminal.completed",
            json!({"kind": "run_terminal", "command_id": "c", "terminal": "completed",
                   "reason": null, "text": "done",
                   "run_stream": {"kind": "run", "id": "r"}}),
        );
        let out = c.classify(terminal);
        let types: Vec<_> = out
            .iter()
            .filter_map(|o| match o {
                AgentOutput::Visible(v) => {
                    Some(v["payload_type"].as_str().unwrap_or("?").to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            types,
            ["task.stream.linked", "run.terminal.completed"],
            "orphan link must flush before the terminal, not vanish"
        );
    }
}

#[cfg(test)]
mod durability_contract {
    use super::*;
    use serde_json::json;

    /// A payload type this crate has never seen, marked ephemeral, must
    /// still route to the ephemeral channel — the reason the branch reads
    /// `durability` instead of matching known payload types.
    #[test]
    fn unknown_ephemeral_type_does_not_land_in_the_transcript() {
        let r: MuseRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "id": "018f0000-0000-7000-8000-00000000c399",
            "stream": {"kind": "session", "id": "s"},
            "sequence": 1,
            "recorded_at": 1780531400000000u64,
            "record_type": "status",
            "durability": "ephemeral",
            "causation_id": "c",
            "payload_type": "subagent.progress.heartbeat",
            "payload_schema_version": 1,
            "payload": {"kind": "future_thing"},
        }))
        .expect("envelope");
        assert!(
            matches!(classify_record(&r), AgentOutput::Ephemeral(_)),
            "a NEW ephemeral payload type must route by durability, not fall into the transcript"
        );
    }

    /// Wire drift persists: a payload that fails its typed shape stays
    /// Visible even if the record is marked ephemeral, so the failure is
    /// seen rather than dropped down a non-persisting channel.
    #[test]
    fn decode_failure_persists_even_when_marked_ephemeral() {
        let r: MuseRecord = serde_json::from_value(json!({
            "schema_version": 1,
            "id": "018f0000-0000-7000-8000-00000000c39a",
            "stream": {"kind": "session", "id": "s"},
            "sequence": 2,
            "recorded_at": 1780531400000000u64,
            "record_type": "status",
            "durability": "ephemeral",
            "causation_id": "c",
            "payload_type": "run.output.delta",
            "payload_schema_version": 1,
            // `text` missing and `run_stream` malformed: fails RunOutputDelta.
            "payload": {"kind": "run_output_delta", "command_id": "c", "run_stream": 7},
        }))
        .expect("envelope");
        match classify_record(&r) {
            AgentOutput::Visible(v) => assert_eq!(v["type"], "muse_decode_error"),
            other => panic!("drift must persist, got {other:?}"),
        }
    }
}
