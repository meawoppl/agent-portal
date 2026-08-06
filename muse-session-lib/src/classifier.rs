//! Classifies Muse journal records into neutral [`AgentOutput`] decisions.
//!
//! Muse's stream is an event-sourced journal rather than a message list, so
//! the rules here follow from the envelope, and every one of them was
//! measured against the real CLI (see `docs/MUSE_SUPPORT.md`, "Measured
//! corrections") rather than assumed:
//!
//! - **Durability is carried, not yet enforced.** The journal marks
//!   `run.output.delta` and `task.lifecycle.status` as `ephemeral`: pure
//!   live-status that should stream to the UI without becoming transcript
//!   rows. `AgentOutput` has no neutral ephemeral variant today (only
//!   Claude-shaped `ToolProgress`), so these are classified `Visible` and
//!   every event carries `durability` for the consumer to honor. See the
//!   OPEN QUESTION below — this is a session-lib contract decision, not a
//!   muse one.
//! - **Identity is `(stream_id, id)`.** The record `id` is a UUID-*shaped*
//!   counter that repeats byte-for-byte across sessions, and `sequence`
//!   repeats across turns, so neither is a safe handle alone. Treat `id` as
//!   stream-local **everywhere** — dedup, correlation, and render keys.
//! - **Unknown frames still name themselves.** An unmodeled `payload_type`
//!   is forwarded with its dotted label plus the raw body, never dropped.

use muse_codes::{MusePayload, MuseRecord};
use serde_json::json;
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
        // A payload that fails its typed shape is real wire drift: forward
        // it rather than dropping it, so the UI shows something and the
        // drift is visible instead of silent.
        Err(e) => AgentOutput::Visible(json!({
            "type": "muse_decode_error",
            "payload_type": record.payload_type,
            "error": e.to_string(),
            "raw": record.payload,
            "stream_id": record.stream.id,
            "record_id": record.id,
        })),
    }
}

/// OPEN QUESTION for session-lib's owner — deliberately visible rather than
/// silently decided here:
///
/// Muse marks a third of its records `ephemeral` (`run.output.delta`,
/// `task.lifecycle.status`). Semantically these are live-status: a long task
/// emits status chatter continuously, and the complete final text arrives
/// separately on `run.terminal.*`, so persisting deltas duplicates the
/// answer in the transcript.
///
/// `AgentOutput` currently offers no neutral way to say that.
/// `ToolProgress` is the right *concept* but a Claude-shaped variant
/// (`tool_use_id`, `elapsed_time_seconds`) that muse records do not fit.
/// Until a neutral variant exists, these classify as `Visible` and the
/// event carries `"durability"` so the persistence layer can filter. The
/// alternative — dropping them — would break streaming text, which is
/// strictly worse than an over-full transcript.
fn classify_payload(record: &MuseRecord, _payload: &MusePayload) -> AgentOutput {
    AgentOutput::Visible(to_event(record))
}

/// Wire shape the frontend receives for one journal record.
///
/// The envelope is preserved verbatim under `record` so the renderer can
/// key on `(stream_id, id)`, group by `causation_id`, and order within a
/// turn by `sequence` — the identity rules measured in MUSE_SUPPORT.md.
/// `payload_type` is lifted to the top level so a renderer can dispatch (or
/// fall back to a labeled passthrough) without re-parsing.
fn to_event(record: &MuseRecord) -> serde_json::Value {
    json!({
        "type": "muse_record",
        "payload_type": record.payload_type,
        // Composite identity — NEVER use `id` alone: it repeats across
        // sessions (UUID-shaped counter), and `sequence` repeats across
        // turns.
        "stream_id": record.stream.id,
        "record_id": record.id,
        "causation_id": record.causation_id,
        "sequence": record.sequence,
        "durability": record.durability,
        "record_type": record.record_type,
        "recorded_at": record.recorded_at,
        "payload": record.payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Deltas reach the UI (streaming would break otherwise) but carry the
    /// `ephemeral` marker so a persistence layer can filter them once the
    /// neutral contract supports it. See the OPEN QUESTION in this module.
    #[test]
    fn output_deltas_are_visible_and_marked_ephemeral() {
        let r = env(
            "run.output.delta",
            "ephemeral",
            json!({"kind": "run_output_delta", "command_id": "c", "text": "hi",
                   "run_stream": {"kind": "run", "id": "r"}}),
        );
        match classify_record(&r) {
            AgentOutput::Visible(v) => assert_eq!(v["durability"], "ephemeral"),
            other => panic!("deltas must reach the UI, got {other:?}"),
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
    fn task_status_carries_its_durability_marker() {
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
            AgentOutput::Visible(v) => assert_eq!(v["durability"], "ephemeral"),
            other => panic!("status must reach the UI, got {other:?}"),
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
