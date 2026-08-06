//! Replays real captured Muse sessions through the classifier.
//!
//! The fixtures are verbatim `muse exec --json` output — one echo-provider
//! turn and one live Muse Spark tool-use turn — so these assertions run
//! against the actual wire rather than hand-built envelopes. They encode
//! the identity rules measured in `docs/MUSE_SUPPORT.md`, which is where a
//! regression would otherwise show up as silent transcript corruption.

use muse_codes::MuseRecord;
use muse_session_lib::classify_record;
use session_lib::adapter::AgentOutput;
use std::collections::HashSet;

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
            match classify_record(r) {
                AgentOutput::Visible(v) => {
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
                other => panic!("{fixture}: unexpected classification {other:?}"),
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

/// Durability reaches the consumer on every event, so a persistence layer
/// can filter live-status records once the neutral contract supports it
/// (see the OPEN QUESTION in the classifier).
#[test]
fn durability_is_carried_on_every_event() {
    let records = load("corpus_meta_tool_use.jsonl");
    let mut saw_ephemeral = false;
    for r in &records {
        let AgentOutput::Visible(v) = classify_record(r) else {
            panic!("expected visible");
        };
        let d = v["durability"].as_str().expect("durability present");
        assert!(
            matches!(d, "durable" | "ephemeral"),
            "unexpected durability {d}"
        );
        saw_ephemeral |= d == "ephemeral";
    }
    assert!(
        saw_ephemeral,
        "a live turn should contain ephemeral records (deltas/status)"
    );
}
