use charcoal::cli::classify_gate::{evaluate, GateInputs, GateOutcome, GateRow, MIN_SAMPLE};
use charcoal::toxicity::classifier::ClassifierVerdict;

fn v(toxic: bool, conf: f32) -> ClassifierVerdict {
    ClassifierVerdict {
        toxic_token: toxic,
        confidence: conf,
        latency_ms: 1,
        model_id: "m".into(),
        policy_version: "p".into(),
    }
}

// One row pairs a candidate verdict with the reference verdict for the same post.
fn row(id: &str, candidate_toxic: bool, reference_toxic: bool) -> GateRow {
    GateRow {
        id: id.into(),
        candidate: v(candidate_toxic, if candidate_toxic { 0.99 } else { 0.01 }),
        reference: v(reference_toxic, if reference_toxic { 0.99 } else { 0.01 }),
    }
}

#[test]
fn gate_passes_when_agreement_at_least_85pct() {
    // 50 rows, 7 disagreements -> 43/50 = 86% agreement -> passes the 85% bar.
    // (Note: 88% — the live RunPod-vs-Zentropi result — now passes too.)
    let mut rows: Vec<GateRow> = (0..43).map(|i| row(&format!("a{i}"), true, true)).collect();
    rows.extend((0..7).map(|i| row(&format!("d{i}"), true, false)));
    let inputs = GateInputs {
        candidate_name: "runpod".into(),
        reference_name: "zentropi".into(),
        rows,
    };
    assert!(matches!(evaluate(&inputs), GateOutcome::Pass { .. }));
}

#[test]
fn gate_fails_when_agreement_below_85pct() {
    // 50 rows, 8 disagreements -> 42/50 = 84% agreement -> fails.
    let mut rows: Vec<GateRow> = (0..42).map(|i| row(&format!("a{i}"), true, true)).collect();
    rows.extend((0..8).map(|i| row(&format!("d{i}"), true, false)));
    let inputs = GateInputs {
        candidate_name: "runpod".into(),
        reference_name: "zentropi".into(),
        rows,
    };
    match evaluate(&inputs) {
        GateOutcome::Fail { reason, .. } => assert!(reason.contains("agreement below")),
        _ => panic!("expected Fail for sub-85% agreement"),
    }
}

#[test]
fn gate_fails_when_sample_below_min() {
    // Perfect agreement, but only 5 rows -> below MIN_SAMPLE -> fails.
    let rows: Vec<GateRow> = (0..5).map(|i| row(&format!("a{i}"), true, true)).collect();
    assert!(rows.len() < MIN_SAMPLE);
    let inputs = GateInputs {
        candidate_name: "runpod".into(),
        reference_name: "zentropi".into(),
        rows,
    };
    match evaluate(&inputs) {
        GateOutcome::Fail { reason, .. } => assert!(reason.contains("sample too small")),
        _ => panic!("expected Fail for tiny sample"),
    }
}
