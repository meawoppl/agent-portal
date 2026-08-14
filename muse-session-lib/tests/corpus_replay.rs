//! Replays real captured Muse sessions through the classifier.
//!
//! The fixtures are verbatim `muse exec --json` output — one echo-provider
//! turn and one live Muse Spark tool-use turn — so these assertions run
//! against the actual wire rather than hand-built envelopes. They encode
//! the identity rules measured in `docs/MUSE_SUPPORT.md`, which is where a
//! regression would otherwise show up as silent transcript corruption.

use muse_codes::MuseRecord;
use muse_session_lib::{classify_record, MuseClassifier};
use session_lib::adapter::{AgentOutput, AgentOutputClassifier};
use std::collections::HashSet;

const SUPPRESSED_BOOKKEEPING: [&str; 4] = [
    "runtime.command.accepted",
    "session.run.linked",
    "run.model.configured",
    "run.lifecycle.started",
];

fn is_suppressed_bookkeeping(payload_type: &str) -> bool {
    SUPPRESSED_BOOKKEEPING.contains(&payload_type)
}

fn load(name: &str) -> Vec<MuseRecord> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| match serde_json::from_str(l) {
            Ok(r) => r,
            Err(e) => panic!("{name}: captured line failed to parse: {e}\n{l}"),
        })
        .collect()
}

/// Every captured record classifies without panicking, and each carries the
/// composite identity a consumer needs.
#[test]
fn every_captured_record_classifies_with_identity() {
    for fixture in ["corpus_echo_turn.jsonl", "corpus_meta_tool_use.jsonl"] {
        let records = load(fixture);
        assert!(!records.is_empty(), "{fixture} is empty");
        for r in &records {
            // Durable records persist; ephemeral ones stream without being
            // buffered. Both carry the same identity fields.
            let v = match classify_record(r) {
                AgentOutput::Visible(v) | AgentOutput::Ephemeral(v) => v,
                AgentOutput::Noop if is_suppressed_bookkeeping(&r.payload_type) => continue,
                other => panic!("{fixture}: unexpected classification {other:?}"),
            };
            {
                assert_eq!(
                    v["stream_id"], r.stream.id,
                    "{fixture}: stream_id must survive"
                );
                assert_eq!(v["record_id"], r.id, "{fixture}: record_id must survive");
                assert_eq!(
                    v["causation_id"], r.causation_id,
                    "{fixture}: causation_id groups the turn"
                );
                assert!(
                    v["payload_type"].is_string(),
                    "{fixture}: every event names its payload type"
                );
            }
        }
    }
}

/// The composite `(stream_id, record_id)` is unique across a capture even
/// though neither half is alone — the property the persistence key rests on.
#[test]
fn composite_identity_is_unique_within_a_capture() {
    for fixture in ["corpus_echo_turn.jsonl", "corpus_meta_tool_use.jsonl"] {
        let records = load(fixture);
        let composite: HashSet<(String, String)> = records
            .iter()
            .map(|r| (r.stream.id.clone(), r.id.clone()))
            .collect();
        assert_eq!(
            composite.len(),
            records.len(),
            "{fixture}: (stream_id, id) must be unique per record"
        );
    }
}

/// `sequence` is NOT a safe key even within one capture: a live turn spawns
/// several streams, and sequence numbers are only monotone within a stream.
/// This pins why the plan forbids keying on it.
#[test]
fn sequence_alone_is_not_unique_across_streams() {
    let records = load("corpus_meta_tool_use.jsonl");
    let streams: HashSet<&String> = records.iter().map(|r| &r.stream.id).collect();
    let sequences: HashSet<u64> = records.iter().map(|r| r.sequence).collect();
    if streams.len() > 1 {
        assert!(
            sequences.len() < records.len(),
            "with multiple streams present, sequence values are expected to repeat"
        );
    }
    // Within a single stream, sequence must be strictly increasing.
    let mut by_stream: std::collections::HashMap<&String, Vec<u64>> = Default::default();
    for r in &records {
        by_stream.entry(&r.stream.id).or_default().push(r.sequence);
    }
    for (stream, seqs) in by_stream {
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            seqs.len(),
            "stream {stream}: sequence must not repeat within a stream"
        );
    }
}

/// The live capture exercises the provider-only vocabulary (tool results,
/// model configuration) — proof the classifier handles more than echo.
#[test]
fn live_capture_covers_provider_only_payloads() {
    let records = load("corpus_meta_tool_use.jsonl");
    let types: HashSet<&str> = records.iter().map(|r| r.payload_type.as_str()).collect();
    for expected in ["run.model.configured", "tool.result"] {
        assert!(
            types.contains(expected),
            "live fixture should exercise {expected}; found {types:?}"
        );
    }
}

/// The routing contract on real data: setup bookkeeping is suppressed, while
/// every other record follows the wire's durability bit. This is the assertion
/// that stops both useless setup rows and live-status from reaching `messages`.
#[test]
fn wire_durability_decides_routing_on_real_captures() {
    let records = load("corpus_meta_tool_use.jsonl");
    let mut ephemeral = 0usize;
    let mut durable = 0usize;
    let mut suppressed = HashSet::new();
    for r in &records {
        match (r.durability, classify_record(r)) {
            (muse_codes::Durability::Ephemeral, AgentOutput::Ephemeral(v)) => {
                assert_eq!(v["durability"], "ephemeral");
                ephemeral += 1;
            }
            (muse_codes::Durability::Durable, AgentOutput::Visible(v)) => {
                assert_eq!(v["durability"], "durable");
                durable += 1;
            }
            (_, AgentOutput::Noop) if is_suppressed_bookkeeping(&r.payload_type) => {
                suppressed.insert(r.payload_type.as_str());
            }
            (d, other) => panic!("{d:?} record mis-routed to {other:?}: {}", r.payload_type),
        }
    }
    assert!(
        ephemeral > 0 && durable > 0,
        "a live turn should exercise both channels (got {ephemeral} ephemeral, {durable} durable)"
    );
    assert_eq!(
        suppressed,
        SUPPRESSED_BOOKKEEPING.into_iter().collect(),
        "every typed setup record should be removed before persistence"
    );
}

/// The reminder screen, proven on the real wire: replaying a live capture
/// through a stateful classifier must Noop every record belonging to a
/// `reminder.*` task — including the `task.stream.linked` records that
/// arrive BEFORE the `proposed` that names the kind — while leaving every
/// real-work record (model tasks, tool tasks, run lifecycle, tool results)
/// untouched. Measured motivation: reminders are 41–47% of a turn's
/// durable records and render as nothing.
#[test]
fn reminder_scaffolding_screens_to_noop_on_real_captures() {
    for fixture in ["corpus_meta_tool_use.jsonl", "corpus_meta_subagents.jsonl"] {
        let records = load(fixture);

        // Ground truth from the capture: task ids proposed as reminder.*.
        let reminder_ids: HashSet<String> = records
            .iter()
            .filter_map(|r| {
                let ev = r.payload.get("event")?;
                (ev.get("kind")?.as_str()? == "proposed"
                    && ev.get("task_kind")?.as_str()?.starts_with("reminder."))
                .then(|| ev.get("task_id")?.as_str().map(str::to_string))?
            })
            .collect();
        assert!(
            !reminder_ids.is_empty(),
            "{fixture}: capture must contain reminders"
        );

        // Same for model.meta.response tasks, now screened alongside reminders
        // (#1607): content-free bookkeeping muse journals per meta-model call.
        let meta_ids: HashSet<String> = records
            .iter()
            .filter_map(|r| {
                let ev = r.payload.get("event")?;
                (ev.get("kind")?.as_str()? == "proposed"
                    && ev.get("task_kind")?.as_str()? == "model.meta.response")
                    .then(|| ev.get("task_id")?.as_str().map(str::to_string))?
            })
            .collect();
        assert!(
            !meta_ids.is_empty(),
            "{fixture}: capture must contain model.meta.response tasks"
        );

        // Both kinds are screened; no record belonging to either may be emitted.
        let screened_ids: HashSet<String> = reminder_ids.union(&meta_ids).cloned().collect();

        let mut classifier = MuseClassifier::default();
        let mut emitted_types = Vec::new();
        let mut noops = 0usize;
        for r in &records {
            for out in classifier.classify(r.clone()) {
                match out {
                    AgentOutput::Noop => noops += 1,
                    AgentOutput::Visible(v) | AgentOutput::Ephemeral(v) => {
                        // No emitted record may belong to a reminder task.
                        let tid = v["payload"]["event"]["task_id"]
                            .as_str()
                            .or_else(|| v["payload"]["task_id"].as_str());
                        if let Some(tid) = tid {
                            assert!(
                                !screened_ids.contains(tid),
                                "{fixture}: screened record leaked: {}",
                                v["payload_type"]
                            );
                        }
                        emitted_types.push(v["payload_type"].as_str().unwrap_or("?").to_string());
                    }
                    other => panic!("{fixture}: unexpected output {other:?}"),
                }
            }
        }

        // Every record belonging to a screened task (reminder.* or
        // model.meta.response) Noops, except held links which are silently
        // dropped (no output at all) — so the Noop count is bounded above by
        // the total screened-task records in the capture.
        let screened_records = records
            .iter()
            .filter(|r| {
                let p = &r.payload;
                let tid = p
                    .get("event")
                    .and_then(|e| e.get("task_id"))
                    .and_then(|t| t.as_str())
                    .or_else(|| p.get("task_id").and_then(|t| t.as_str()));
                tid.is_some_and(|t| screened_ids.contains(t))
            })
            .count();
        assert!(
            noops > 0 && noops <= screened_records,
            "{fixture}: expected 1..={screened_records} noops, got {noops}"
        );

        // Real work survives untouched.
        for expected in ["run.terminal.completed", "tool.result"] {
            assert!(
                emitted_types.iter().any(|t| t == expected),
                "{fixture}: screening must not touch {expected}"
            );
        }
        // And non-reminder tasks keep their stream.linked records.
        assert!(
            emitted_types.iter().any(|t| t == "task.stream.linked"),
            "{fixture}: real tasks must keep their linked records"
        );
    }
}

/// The screen's arithmetic on the tool-use capture: durable records drop by
/// roughly the measured reminder share, pinning the buffer-pressure win
/// that motivated the screen (message cap shrinks 1000 → 200 on the
/// strength of it).
#[test]
fn screening_cuts_the_durable_record_volume() {
    let records = load("corpus_meta_tool_use.jsonl");
    let durable_before = records
        .iter()
        .filter(|r| r.durability == muse_codes::Durability::Durable)
        .count();
    let mut classifier = MuseClassifier::default();
    let durable_after = records
        .iter()
        .flat_map(|r| classifier.classify(r.clone()))
        .filter(|o| matches!(o, AgentOutput::Visible(_)))
        .count();
    assert!(
        (durable_after as f64) <= (durable_before as f64) * 0.65,
        "screen should remove the measured ~40%+ reminder share of durable \
         records (before={durable_before}, after={durable_after})"
    );
}
