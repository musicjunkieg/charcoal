//! Unit tests for the A/B comparison summarizer. The pure summarize function
//! takes two backends names + per-fixture verdicts and returns a comparison
//! row + aggregate counts; UI rendering is separated from data so we can
//! assert numbers without parsing terminal output.

use charcoal::cli::classify_compare::{summarize, Pair, Summary};
use charcoal::toxicity::classifier::ClassifierVerdict;

fn v(t: bool, c: f32) -> ClassifierVerdict {
    ClassifierVerdict {
        toxic_token: t,
        confidence: c,
        latency_ms: 10,
        model_id: "x".into(),
        policy_version: "p".into(),
    }
}

#[test]
fn agreement_counted_when_both_backends_agree_on_toxic_token() {
    let pairs = vec![
        Pair {
            id: "kt-001".into(),
            label: "toxic".into(),
            a: v(true, 0.9),
            b: v(true, 0.85),
        },
        Pair {
            id: "kt-002".into(),
            label: "toxic".into(),
            a: v(false, 0.6),
            b: v(true, 0.9),
        },
        Pair {
            id: "kc-001".into(),
            label: "clean".into(),
            a: v(false, 0.1),
            b: v(false, 0.05),
        },
    ];
    let s: Summary = summarize(&pairs, "cope-a", "cope-b");
    assert_eq!(s.total, 3);
    assert_eq!(s.agreements, 2);
    assert_eq!(s.disagreements, 1);
    assert_eq!(s.a_toxic_only, 0);
    assert_eq!(s.b_toxic_only, 1);
}

#[test]
fn summarize_breaks_out_by_expected_label() {
    let pairs = vec![
        Pair {
            id: "kt-001".into(),
            label: "toxic".into(),
            a: v(true, 0.9),
            b: v(true, 0.9),
        },
        Pair {
            id: "kt-002".into(),
            label: "toxic".into(),
            a: v(false, 0.1),
            b: v(false, 0.1),
        },
        Pair {
            id: "kc-001".into(),
            label: "clean".into(),
            a: v(false, 0.1),
            b: v(false, 0.1),
        },
    ];
    let s = summarize(&pairs, "cope-a", "cope-b");
    assert_eq!(s.a_correct_on_toxic, 1);
    assert_eq!(s.a_correct_on_clean, 1);
    assert_eq!(s.b_correct_on_toxic, 1);
    assert_eq!(s.b_correct_on_clean, 1);
}

#[test]
fn uncertain_rows_skipped_from_correctness_totals() {
    let pairs = vec![
        Pair {
            id: "ec-001".into(),
            label: "uncertain".into(),
            a: v(true, 0.5),
            b: v(false, 0.5),
        },
        Pair {
            id: "kt-001".into(),
            label: "toxic".into(),
            a: v(true, 0.5),
            b: v(true, 0.5),
        },
    ];
    let s = summarize(&pairs, "cope-a", "cope-b");
    assert_eq!(s.scored_for_accuracy, 1); // only the kt- row counts
    assert_eq!(s.a_correct_on_toxic, 1);
}
