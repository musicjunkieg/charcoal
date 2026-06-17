//! Unit tests for the generalized audit log writer.
//! Validates: event-type parameterization, JSONL line shape, daily rotation,
//! and the env-var gate that controls whether events are written at all.

use charcoal::scoring::audit_log::{
    format_log_path, AuditEvent, AuditWriter, ClassifierFields, EventKind, NliFields,
};
use charcoal::scoring::nli::HypothesisScores;
use chrono::TimeZone;
use serde_json::Value;
use std::fs;
use tempfile::tempdir;

fn sample_classifier_event() -> AuditEvent {
    AuditEvent::classifier(ClassifierFields {
        backend: "runpod".into(),
        model_id: "cope-b-a4b".into(),
        policy_version: "policy-v3".into(),
        prompt_hash: "hash-abc".into(),
        toxic: true,
        confidence: 0.93,
        latency_ms: 120,
    })
}

fn sample_nli_event() -> AuditEvent {
    AuditEvent::nli(NliFields {
        target_did: "did:plc:abc".into(),
        target_handle: "alice.bsky.social".into(),
        pair_type: "direct".into(),
        original_text: "some parent post".into(),
        response_text: "some reply".into(),
        hypothesis_scores: HypothesisScores {
            attack: 0.10,
            contempt: 0.05,
            misrepresent: 0.30,
            good_faith_disagree: 0.20,
            support: 0.50,
        },
        hostility_score: 0.42,
        similarity: Some(0.61),
    })
}

#[test]
fn audit_writer_writes_jsonl_one_event_per_line_when_enabled() {
    let dir = tempdir().unwrap();
    let writer = AuditWriter::new(dir.path(), EventKind::Classifier, /*enabled=*/ true).unwrap();

    writer.record(sample_classifier_event()).unwrap();
    writer.record(sample_classifier_event()).unwrap();

    let path = writer.current_path();
    let body = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2, "one event per line");

    let first: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["kind"], "classifier");
    assert_eq!(first["backend"], "runpod");
    assert_eq!(first["model_id"], "cope-b-a4b");
    assert_eq!(first["policy_version"], "policy-v3");
    assert_eq!(first["toxic"], true);
    assert_eq!(first["confidence"], 0.93);
    assert_eq!(first["latency_ms"], 120);
    // Sanity: timestamp is RFC 3339-ish (parseable by chrono)
    let ts = first["timestamp"].as_str().unwrap();
    chrono::DateTime::parse_from_rfc3339(ts).expect("RFC 3339 timestamp");
}

#[test]
fn audit_writer_drops_events_when_disabled() {
    let dir = tempdir().unwrap();
    let writer = AuditWriter::new(dir.path(), EventKind::Classifier, /*enabled=*/ false).unwrap();
    writer.record(sample_classifier_event()).unwrap();
    // record() short-circuits before opening the file; the file must not exist.
    assert!(
        !writer.current_path().exists(),
        "disabled writer must not create the JSONL file"
    );
}

#[test]
fn audit_writer_supports_nli_events_with_full_schema() {
    let dir = tempdir().unwrap();
    let writer = AuditWriter::new(dir.path(), EventKind::Nli, true).unwrap();

    writer.record(sample_nli_event()).unwrap();

    let body = fs::read_to_string(writer.current_path()).unwrap();
    let event: Value = serde_json::from_str(body.lines().next().unwrap()).unwrap();
    assert_eq!(event["kind"], "nli");
    assert_eq!(event["target_handle"], "alice.bsky.social");
    assert_eq!(event["pair_type"], "direct");
    assert_eq!(event["original_text"], "some parent post");
    assert_eq!(event["response_text"], "some reply");
    assert_eq!(event["hostility_score"], 0.42);
    assert_eq!(event["similarity"], 0.61);
    assert_eq!(event["hypothesis_scores"]["attack"], 0.10);
    assert_eq!(event["hypothesis_scores"]["support"], 0.50);
}

#[test]
fn audit_writer_rotates_daily_filename() {
    let dir = tempdir().unwrap();
    let p1 = format_log_path(
        dir.path(),
        EventKind::Classifier,
        chrono::Utc.with_ymd_and_hms(2026, 6, 5, 12, 0, 0).unwrap(),
    );
    let p2 = format_log_path(
        dir.path(),
        EventKind::Classifier,
        chrono::Utc.with_ymd_and_hms(2026, 6, 6, 3, 0, 0).unwrap(),
    );
    assert_ne!(p1, p2);
    assert!(p1.to_string_lossy().contains("classifier-2026-06-05"));
    assert!(p2.to_string_lossy().contains("classifier-2026-06-06"));
}

// NOTE: `AuditWriter::from_env` is a one-line wrapper around `std::env::var(...)`
// + the explicit `new` constructor. We do not exercise it in a unit test
// because `std::env::set_var` is process-global and cargo runs tests in
// parallel by default — a unit test there would race against any other test
// that reads `CHARCOAL_AUDIT_CLASSIFIER`. The integration test in
// `tests/web_classifier.rs` (Chunk 4) covers the env-gated path end-to-end
// via a child process with controlled env.
