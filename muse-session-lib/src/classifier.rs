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

/// Muse-protocol output classifier.
///
/// Stateless: every decision is a function of the record in hand. Turn
/// grouping is carried on the wire (`causation_id`), so unlike Codex this
/// needs no request-id ↔ tool map.
#[derive(Debug, Clone, Copy, Default)]
pub struct MuseClassifier;

impl AgentOutputClassifier for MuseClassifier {
    /// One parsed journal record (one stdout line).
    type Raw = MuseRecord;

    fn classify(&mut self, record: MuseRecord) -> Vec<AgentOutput> {
        vec![classify_record(&record)]
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

/// Route on the wire's own durability flag. Deliberately not a match on
/// payload types: muse is a days-old beta and will add ephemeral kinds, and
/// an unlisted one must not default into the transcript — persisting
/// live-status is expensive to unwind (a migration, not a code change).
fn classify_payload(record: &MuseRecord, _payload: &MusePayload) -> AgentOutput {
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
            "run.lifecycle.started",
            "durable",
            json!({"kind": "run_started", "command_id": "c", "prompt": "p",
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
    fn real_captured_line_classifies() {
        let line = r#"{"schema_version":1,"id":"018f0000-0000-7000-8000-00000000c350","stream":{"kind":"session","id":"34c4f817-8c19-4778-a6fc-a9399ac4d034"},"sequence":1,"recorded_at":1780531400000000,"record_type":"reconciliation","durability":"durable","causation_id":"7af312fb-ae7d-40f7-bd79-21cd328c4583","payload_type":"runtime.command.accepted","payload_schema_version":1,"payload":{"client_id":null,"command_id":"7af312fb-ae7d-40f7-bd79-21cd328c4583","command_kind":"turn.submit","kind":"command_accepted"}}"#;
        let r = record(line);
        let AgentOutput::Visible(v) = classify_record(&r) else {
            panic!("command acceptance is durable transcript material");
        };
        assert_eq!(v["payload_type"], "runtime.command.accepted");
        assert_eq!(v["durability"], "durable");
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
