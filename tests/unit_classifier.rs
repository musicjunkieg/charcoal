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

mod runpod {
    use charcoal::toxicity::classifier::ToxicityClassifier;
    use charcoal::toxicity::runpod_cope_b::RunPodCopeBClient;

    #[tokio::test]
    async fn runpod_client_constructs_with_valid_env_inputs() {
        let client = RunPodCopeBClient::new(
            "https://api.runpod.ai/v2/endpoint-id".into(),
            "test-api-key".into(),
        );
        assert!(client.is_ok());
    }

    #[tokio::test]
    async fn runpod_client_rejects_empty_credentials() {
        let err1 = RunPodCopeBClient::new("".into(), "key".into()).unwrap_err();
        assert!(format!("{err1}").contains("endpoint"));
        let err2 =
            RunPodCopeBClient::new("https://api.runpod.ai/v2/x".into(), "".into()).unwrap_err();
        assert!(format!("{err2}").contains("api key"));
    }

    // Wire-shape test: build the JSON body the client sends and assert structure.
    // Doesn't hit the network.
    #[test]
    fn runpod_client_request_body_shape() {
        use serde_json::json;
        let body = RunPodCopeBClient::build_request_body("hello world");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v, json!({"input": {"content": "hello world"}}));
    }

    // Response parse: well-formed
    #[test]
    fn runpod_client_parses_well_formed_response() {
        let raw = r#"{"output":{"toxic":true,"confidence":0.92,"model":"cope-b-a4b","policy_version":"policy-v3"}}"#;
        let parsed = RunPodCopeBClient::parse_response(raw, 250).unwrap();
        assert!(parsed.toxic_token);
        assert!((parsed.confidence - 0.92).abs() < 1e-4);
        assert_eq!(parsed.model_id, "cope-b-a4b");
        assert_eq!(parsed.policy_version, "policy-v3");
        assert_eq!(parsed.latency_ms, 250);
    }

    // Response parse: missing required field. The "missing field `output`"
    // detail comes from serde and lives in the error *source chain*, not in
    // anyhow's outermost context (which is "parse RunPod response body: ..."),
    // so we format with {:#} to inspect the whole chain.
    #[test]
    fn runpod_client_response_missing_output_field_errors() {
        let raw = r#"{"status":"COMPLETED"}"#;
        let err = RunPodCopeBClient::parse_response(raw, 0).unwrap_err();
        assert!(format!("{err:#}").to_lowercase().contains("output"));
    }

    // Threshold is a const baked into the impl
    #[test]
    fn runpod_threshold_is_const_per_spec() {
        let client =
            RunPodCopeBClient::new("https://api.runpod.ai/v2/x".into(), "k".into()).unwrap();
        // Value tuned in Chunk 5 — assert it's in a reasonable range now.
        let t = client.threshold();
        assert!((0.0..=1.0).contains(&t), "threshold must be in [0,1]");
    }
}
