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

mod zentropi_trait {
    use charcoal::toxicity::classifier::ToxicityClassifier;
    use charcoal::toxicity::zentropi::{ZentropiClient, ZENTROPI_THRESHOLD};

    #[test]
    fn zentropi_threshold_preserves_current_behavior() {
        // Spec: existing CoPE-A behavior is "label == 1 = toxic", regardless
        // of confidence value. Threshold 0.0 matches that semantically since
        // any toxic_token=true && confidence >= 0.0 is true.
        assert_eq!(ZENTROPI_THRESHOLD, 0.0);
    }

    #[test]
    fn zentropi_client_implements_trait_with_static_ids() {
        // Avoid the network: build a client with placeholder creds and verify
        // the trait's accessor methods return the documented constants.
        let client = ZentropiClient::new("k".into(), "labeler-id".into(), None).unwrap();
        let dyn_ref: &dyn ToxicityClassifier = &client;
        assert_eq!(dyn_ref.name(), "zentropi-hosted");
        assert_eq!(dyn_ref.threshold(), ZENTROPI_THRESHOLD);
    }
}

mod factory {
    use charcoal::toxicity::classifier::build_from_env;
    use serial_test::serial;

    /// Save + restore the existing value so tests don't break a developer's
    /// shell-exported env.
    struct EnvGuard {
        prior: Option<String>,
    }
    impl EnvGuard {
        fn new() -> Self {
            Self {
                prior: std::env::var("CHARCOAL_CLASSIFIER").ok(),
            }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => std::env::set_var("CHARCOAL_CLASSIFIER", v),
                None => std::env::remove_var("CHARCOAL_CLASSIFIER"),
            }
        }
    }

    #[test]
    #[serial(charcoal_classifier_env)]
    fn build_fails_when_classifier_unset() {
        let _g = EnvGuard::new();
        std::env::remove_var("CHARCOAL_CLASSIFIER");
        // let-else (not unwrap_err): the Ok type Arc<dyn ToxicityClassifier>
        // is a trait object and doesn't implement Debug, which unwrap_err needs.
        let Err(err) = build_from_env() else {
            panic!("expected build_from_env to fail when CHARCOAL_CLASSIFIER is unset");
        };
        assert!(format!("{err}").contains("CHARCOAL_CLASSIFIER"));
    }

    #[test]
    #[serial(charcoal_classifier_env)]
    fn build_fails_on_unrecognized_backend() {
        let _g = EnvGuard::new();
        std::env::set_var("CHARCOAL_CLASSIFIER", "not-a-backend");
        let Err(err) = build_from_env() else {
            panic!("expected build_from_env to reject an unrecognized backend");
        };
        assert!(format!("{err}").contains("not a known backend"));
    }

    /// RAII save+restore for the RunPod env vars, matching the `EnvGuard`
    /// pattern above. Ensures cleanup runs even if an assertion panics, so the
    /// vars don't leak into later (serial) tests.
    struct RunPodEnvGuard {
        prior_url: Option<String>,
        prior_key: Option<String>,
    }
    impl RunPodEnvGuard {
        fn new() -> Self {
            Self {
                prior_url: std::env::var("RUNPOD_ENDPOINT_URL").ok(),
                prior_key: std::env::var("RUNPOD_API_KEY").ok(),
            }
        }
    }
    impl Drop for RunPodEnvGuard {
        fn drop(&mut self) {
            match &self.prior_url {
                Some(v) => std::env::set_var("RUNPOD_ENDPOINT_URL", v),
                None => std::env::remove_var("RUNPOD_ENDPOINT_URL"),
            }
            match &self.prior_key {
                Some(v) => std::env::set_var("RUNPOD_API_KEY", v),
                None => std::env::remove_var("RUNPOD_API_KEY"),
            }
        }
    }

    #[test]
    #[serial(charcoal_classifier_env)]
    fn build_backend_named_runpod_constructs() {
        let _g = RunPodEnvGuard::new();
        std::env::set_var("RUNPOD_ENDPOINT_URL", "https://example.invalid/v2/x");
        std::env::set_var("RUNPOD_API_KEY", "k");
        let c = charcoal::toxicity::classifier::build_backend_named("runpod");
        assert!(
            c.is_ok(),
            "named runpod backend should build: {:?}",
            c.err()
        );
    }
}

mod retry {
    use charcoal::toxicity::classifier::ToxicityClassifier;
    use charcoal::toxicity::runpod_cope_b::RunPodCopeBClient;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn retries_on_5xx_then_succeeds() {
        let server = MockServer::start().await;
        // First two requests return 503; third returns 200.
        let ok = r#"{"output":{"toxic":false,"confidence":0.1,"model":"cope-b-a4b","policy_version":"policy-v3"}}"#;
        Mock::given(method("POST"))
            .and(path("/runsync"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/runsync"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(ok, "application/json"))
            .mount(&server)
            .await;

        let client = RunPodCopeBClient::new(server.uri(), "k".into()).unwrap();
        let dyn_ref: &dyn ToxicityClassifier = &client;
        let v = dyn_ref.classify("hello").await.unwrap();
        assert!(!v.toxic_token);
    }

    #[tokio::test]
    async fn does_not_retry_on_4xx() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/runsync"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1) // wiremock asserts the mock fired exactly once → no retry
            .mount(&server)
            .await;

        let client = RunPodCopeBClient::new(server.uri(), "k".into()).unwrap();
        let dyn_ref: &dyn ToxicityClassifier = &client;
        let err = dyn_ref.classify("hello").await.unwrap_err();
        assert!(format!("{err}").contains("401"));
    }

    #[tokio::test]
    async fn warm_up_helper_runs_against_endpoint() {
        let server = MockServer::start().await;
        let ok = r#"{"output":{"toxic":false,"confidence":0.0,"model":"cope-b-a4b","policy_version":"policy-v3"}}"#;
        Mock::given(method("POST"))
            .and(path("/runsync"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(ok, "application/json"))
            .expect(1)
            .mount(&server)
            .await;

        let client = RunPodCopeBClient::new(server.uri(), "k".into()).unwrap();
        charcoal::toxicity::runpod_cope_b::warm_up(&client)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn runsync_pending_then_polls_status_to_completion() {
        // /runsync returns ~90s before a cold-start job finishes, yielding a
        // non-terminal envelope with NO output. The client must then poll
        // /status/{id} until the verdict lands. (Regression: the first live
        // gate run failed parsing the IN_PROGRESS runsync body.)
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/runsync"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"id":"job-xyz","status":"IN_PROGRESS"}"#,
                "application/json",
            ))
            .mount(&server)
            .await;
        let done = r#"{"id":"job-xyz","status":"COMPLETED","output":{"toxic":true,"confidence":0.95,"model":"cope-b-a4b","policy_version":"policy-v3"}}"#;
        Mock::given(method("GET"))
            .and(path("/status/job-xyz"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(done, "application/json"))
            .mount(&server)
            .await;

        let client = RunPodCopeBClient::new(server.uri(), "k".into()).unwrap();
        let dyn_ref: &dyn ToxicityClassifier = &client;
        let v = dyn_ref.classify("hello").await.unwrap();
        assert!(v.toxic_token);
        assert!((v.confidence - 0.95).abs() < 1e-4);
    }

    #[tokio::test]
    async fn classify_short_circuits_when_over_ceiling() {
        use charcoal::toxicity::cost_meter::ScanCostMeter;
        use std::sync::Arc;

        let server = MockServer::start().await;
        // Any hit on /runsync would fail the test: expect ZERO requests.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let meter = Arc::new(ScanCostMeter::new(500, 329));
        // Pre-arm the meter to a time well past the ceiling (no sleeping).
        // Pre-seed the worker-seconds integral past the ceiling: 6000 worker-sec
        // × $3.29/hr ≈ $5.48 ≥ the $5 ceiling, so the next call short-circuits.
        meter.force_worker_seconds(6000.0);

        let client = RunPodCopeBClient::new(server.uri(), "test-key".into())
            .unwrap()
            .with_meter(meter);

        let err = client
            .classify("[Parent post]: x\n\n[Reply]: y")
            .await
            .unwrap_err();
        assert!(
            err.downcast_ref::<charcoal::toxicity::cost_meter::CostCeilingExceeded>()
                .is_some(),
            "expected CostCeilingExceeded, got: {err:#}"
        );
        // .expect(0) on drop verifies no HTTP request was issued.
    }

    #[tokio::test]
    async fn warm_up_short_circuits_when_over_ceiling() {
        // Regression: the cost backstop must gate the warm-up request path too,
        // not just classify(). warm_up() goes through classify_with_timeout, so
        // the meter check must live there (the single RunPod request chokepoint).
        use charcoal::toxicity::cost_meter::ScanCostMeter;
        use std::sync::Arc;

        let server = MockServer::start().await;
        // Any hit on /runsync would fail the test: expect ZERO requests.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&server)
            .await;

        let meter = Arc::new(ScanCostMeter::new(500, 329));
        // Pre-seed the worker-seconds integral past the ceiling: 6000 worker-sec
        // × $3.29/hr ≈ $5.48 ≥ the $5 ceiling, so the next call short-circuits.
        meter.force_worker_seconds(6000.0);

        let client = RunPodCopeBClient::new(server.uri(), "test-key".into())
            .unwrap()
            .with_meter(meter);

        let err = charcoal::toxicity::runpod_cope_b::warm_up(&client)
            .await
            .unwrap_err();
        assert!(
            err.downcast_ref::<charcoal::toxicity::cost_meter::CostCeilingExceeded>()
                .is_some(),
            "expected CostCeilingExceeded from warm_up, got: {err:#}"
        );
        // .expect(0) on drop verifies no HTTP request was issued.
    }
}
