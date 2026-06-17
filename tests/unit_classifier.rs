//! Unit tests for src/toxicity/classifier.rs — trait shape, ClassifierVerdict,
//! is_toxic helper threshold logic, and the StubClassifier used by integration
//! tests.

use charcoal::toxicity::classifier::{
    is_toxic, ClassifierVerdict, StubClassifier, ToxicityClassifier,
};
use serde_json::json;

#[tokio::test]
async fn stub_classifier_returns_scripted_verdict() {
    let stub = StubClassifier::with_script(vec![
        ClassifierVerdict {
            toxic_token: true,
            confidence: 0.91,
            latency_ms: 12,
            model_id: "stub".into(),
            policy_version: "stub-policy".into(),
        },
        ClassifierVerdict {
            toxic_token: false,
            confidence: 0.85,
            latency_ms: 12,
            model_id: "stub".into(),
            policy_version: "stub-policy".into(),
        },
    ]);

    let v1 = stub.classify("anything").await.unwrap();
    assert!(v1.toxic_token);
    assert_eq!(v1.model_id, "stub");

    let v2 = stub.classify("anything else").await.unwrap();
    assert!(!v2.toxic_token);
}

#[tokio::test]
async fn stub_classifier_exhaustion_errors_loudly() {
    let stub = StubClassifier::with_script(vec![]);
    let err = stub.classify("anything").await.unwrap_err();
    assert!(format!("{err}").contains("stub script exhausted"));
}

#[test]
fn classifier_verdict_serde_roundtrip() {
    // Purpose: lock in the serialized field names (snake_case, no renames).
    // confidence uses 0.5 — exactly representable in both f32 and f64 — so the
    // exact-equality assertion is meaningful. An arbitrary value like 0.73 would
    // widen from f32 to 0.7300000190734863 as f64 and never compare equal to the
    // JSON literal 0.73; that's an f32 artifact, not a serde issue.
    let v = ClassifierVerdict {
        toxic_token: true,
        confidence: 0.5,
        latency_ms: 200,
        model_id: "cope-b-a4b".into(),
        policy_version: "policy-2026-07-01".into(),
    };
    let json = serde_json::to_value(&v).unwrap();
    assert_eq!(
        json,
        json!({
            "toxic_token": true,
            "confidence": 0.5,
            "latency_ms": 200,
            "model_id": "cope-b-a4b",
            "policy_version": "policy-2026-07-01",
        })
    );
}

#[tokio::test]
async fn is_toxic_applies_threshold_from_implementation() {
    // StubClassifier's threshold is 0.0 (trust the script's boolean).
    let stub = StubClassifier::with_script(vec![ClassifierVerdict {
        toxic_token: true,
        confidence: 0.10,
        latency_ms: 1,
        model_id: "stub".into(),
        policy_version: "stub".into(),
    }]);
    let v = stub.classify("x").await.unwrap();
    assert!(is_toxic(&stub as &dyn ToxicityClassifier, &v));

    // A classifier whose threshold is > confidence rejects, even when the
    // model said toxic_token=true.
    let stub_strict = StubClassifier::with_script_and_threshold(
        vec![ClassifierVerdict {
            toxic_token: true,
            confidence: 0.10,
            latency_ms: 1,
            model_id: "stub-strict".into(),
            policy_version: "stub".into(),
        }],
        /* threshold = */ 0.5,
    );
    let v2 = stub_strict.classify("x").await.unwrap();
    assert!(!is_toxic(&stub_strict as &dyn ToxicityClassifier, &v2));
}

#[tokio::test]
async fn classifier_trait_is_send_sync() {
    // Compile-time check: the trait object must be Send + Sync so it can live
    // inside an Arc shared across tasks.
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Box<dyn ToxicityClassifier>>();
}
