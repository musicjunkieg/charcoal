//! Tests for the two-stage toxicity scorer.
//!
//! Stage 1 = ONNX clean-pass filter (< 0.10 means cleared, no Stage-2 call).
//! Stage 2 = the required `ToxicityClassifier` (binary verdict) for everything
//! at or above the clean threshold. There is no ONNX-only fallback: a Stage-2
//! classifier error propagates rather than degrading to an ONNX guess.

use anyhow::Result;
use async_trait::async_trait;
use charcoal::toxicity::classifier::{ClassifierVerdict, StubClassifier};
use charcoal::toxicity::ensemble::{TwoStageToxicityScorer, VerdictSource};
use charcoal::toxicity::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};
use std::sync::Arc;

/// Test scorer that returns a fixed continuous toxicity score for any input.
/// Used as the ONNX-equivalent primary scorer.
struct FixedScorer(f64);

#[async_trait]
impl ToxicityScorer for FixedScorer {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        Ok(ToxicityResult {
            toxicity: self.0,
            attributes: ToxicityAttributes::default(),
        })
    }
}

/// Build a verdict for the stub script. Confidence is set above any plausible
/// threshold so `is_toxic` tracks `toxic_token` (StubClassifier threshold is 0).
fn verdict(toxic: bool) -> ClassifierVerdict {
    ClassifierVerdict {
        toxic_token: toxic,
        confidence: 0.99,
        latency_ms: 1,
        model_id: "stub".into(),
        policy_version: "stub".into(),
    }
}

/// Two-stage scorer with a scripted Stage-2 classifier. For ONNX-cleared cases
/// pass an empty script — the classifier is never invoked.
fn two_stage(onnx_score: f64, script: Vec<ClassifierVerdict>) -> TwoStageToxicityScorer {
    TwoStageToxicityScorer::new(
        Box::new(FixedScorer(onnx_score)),
        Arc::new(StubClassifier::with_script(script)),
    )
}

#[tokio::test]
async fn onnx_below_clean_threshold_skips_stage2() {
    // ONNX 0.05 < 0.10 clean threshold → cleared, is_toxic = false. The stub
    // script is empty: a Stage-2 call here would error (exhausted), proving the
    // clean-pass short-circuit really skips the classifier.
    let scorer = two_stage(0.05, vec![]);
    let v = scorer.classify_post("benign text", None).await.unwrap();
    assert!(!v.is_toxic);
    assert_eq!(v.source, VerdictSource::OnnxCleared);
    assert!(v.classifier_confidence.is_none());
    assert!(v.classifier_model_id.is_none());
    assert!((v.onnx_score - 0.05).abs() < 1e-9);
}

#[tokio::test]
async fn onnx_above_clean_threshold_classifier_safe() {
    // ONNX 0.30 reaches Stage 2; the classifier returns not-toxic → safe.
    let scorer = two_stage(0.30, vec![verdict(false)]);
    let v = scorer.classify_post("ambiguous text", None).await.unwrap();
    assert!(!v.is_toxic);
    assert_eq!(v.source, VerdictSource::ClassifierSafe);
    assert_eq!(v.classifier_confidence, Some(0.99));
    assert_eq!(v.classifier_model_id.as_deref(), Some("stub"));
}

#[tokio::test]
async fn onnx_above_clean_threshold_classifier_toxic() {
    // ONNX 0.70 reaches Stage 2; the classifier returns toxic → toxic.
    let scorer = two_stage(0.70, vec![verdict(true)]);
    let v = scorer.classify_post("hostile text", None).await.unwrap();
    assert!(v.is_toxic);
    assert_eq!(v.source, VerdictSource::ClassifierToxic);
    assert_eq!(v.classifier_policy_version.as_deref(), Some("stub"));
}

#[tokio::test]
async fn classifier_error_propagates_no_silent_fallback() {
    // ONNX 0.70 reaches Stage 2, but the stub script is empty → the classifier
    // errors. The new design propagates that error rather than falling back to
    // an ONNX threshold guess (spec: no silent fallback).
    let scorer = two_stage(0.70, vec![]);
    let err = scorer
        .classify_post("hostile text", None)
        .await
        .unwrap_err();
    assert!(format!("{err}").contains("stub script exhausted"));
}

#[tokio::test]
async fn classify_batch_preserves_input_order() {
    // Stream concurrency reorders execution, but classify_batch must restore
    // the original index ordering for caller alignment with their text vec.
    // All inputs are ONNX-cleared, so the empty stub script is never touched.
    let scorer = two_stage(0.05, vec![]);
    let texts: Vec<String> = (0..16).map(|i| format!("post {}", i)).collect();
    let contexts: Vec<Option<String>> = vec![None; texts.len()];
    let verdicts = scorer.classify_batch(&texts, &contexts).await.unwrap();

    assert_eq!(verdicts.len(), 16);
    for v in &verdicts {
        assert!(!v.is_toxic);
        assert_eq!(v.source, VerdictSource::OnnxCleared);
    }
}

#[tokio::test]
async fn classify_batch_rejects_mismatched_lengths() {
    let scorer = two_stage(0.05, vec![]);
    let texts = vec!["a".to_string(), "b".to_string()];
    let contexts = vec![None];
    let err = scorer.classify_batch(&texts, &contexts).await.unwrap_err();
    assert!(format!("{err}").contains("texts.len()"));
}

#[tokio::test]
async fn score_text_returns_primary_continuous_score() {
    // ToxicityScorer::score_text on TwoStage delegates to the primary,
    // preserving the continuous score for legacy continuous-score callers.
    let scorer = two_stage(0.42, vec![]);
    let r = scorer.score_text("anything").await.unwrap();
    assert!((r.toxicity - 0.42).abs() < 1e-9);
}

#[tokio::test]
async fn classify_batch_via_trait_method_works() {
    // ToxicityScorer::classify_batch_with_contexts on TwoStage uses the
    // two-stage pipeline (overrides the trait default).
    let scorer = two_stage(0.05, vec![]);
    let texts = vec!["a".to_string(), "b".to_string()];
    let contexts: Vec<Option<String>> = vec![None, None];
    let verdicts = ToxicityScorer::classify_batch_with_contexts(&scorer, &texts, &contexts)
        .await
        .unwrap();

    assert_eq!(verdicts.len(), 2);
    assert!(verdicts.iter().all(|v| !v.is_toxic));
}

#[tokio::test]
async fn default_trait_classify_batch_uses_05_threshold() {
    // Plain FixedScorer (no override of classify_batch_with_contexts) uses
    // the trait default with 0.50 threshold.
    let scorer = FixedScorer(0.65);
    let texts = vec!["x".to_string()];
    let contexts: Vec<Option<String>> = vec![None];
    let verdicts = scorer
        .classify_batch_with_contexts(&texts, &contexts)
        .await
        .unwrap();

    assert!(verdicts[0].is_toxic);
    assert!((verdicts[0].onnx_score - 0.65).abs() < 1e-9);
}

#[tokio::test]
async fn default_trait_classify_batch_below_threshold_safe() {
    let scorer = FixedScorer(0.40);
    let texts = vec!["x".to_string()];
    let contexts: Vec<Option<String>> = vec![None];
    let verdicts = scorer
        .classify_batch_with_contexts(&texts, &contexts)
        .await
        .unwrap();

    assert!(!verdicts[0].is_toxic);
}

#[tokio::test]
async fn classifier_name_reflects_construction() {
    let scorer = two_stage(0.05, vec![]);
    assert_eq!(scorer.classifier_name(), "stub");
}

// ---------------------------------------------------------------------------
// onnx_clean_pass tests (Task 2.1)
// ---------------------------------------------------------------------------

/// A scorer that maps each text to a score by index position.
/// Texts are matched to scores in the order they arrive, cycling if needed.
/// This lets us straddle ONNX_CLEAN_THRESHOLD (0.10) in a single batch.
struct IndexedScorer(Vec<f64>);

#[async_trait]
impl ToxicityScorer for IndexedScorer {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        // Single-text calls return the first score (used by classify_post).
        Ok(ToxicityResult {
            toxicity: *self.0.first().unwrap_or(&0.0),
            attributes: ToxicityAttributes::default(),
        })
    }

    async fn score_batch(&self, texts: &[String]) -> Result<Vec<ToxicityResult>> {
        Ok(texts
            .iter()
            .enumerate()
            .map(|(i, _)| ToxicityResult {
                // Guard against an empty score Vec (modulo-by-zero panic): fall
                // back to 0.0 when no scores were configured.
                toxicity: self.0.get(i % self.0.len().max(1)).copied().unwrap_or(0.0),
                attributes: ToxicityAttributes::default(),
            })
            .collect())
    }
}

/// Classifier double that panics immediately if `classify` is ever invoked.
/// Using this in the scorer proves `onnx_clean_pass` never touches Stage 2.
struct PanicClassifier;

#[async_trait]
impl charcoal::toxicity::classifier::ToxicityClassifier for PanicClassifier {
    async fn classify(
        &self,
        _content: &str,
    ) -> Result<charcoal::toxicity::classifier::ClassifierVerdict> {
        panic!("PanicClassifier::classify was invoked — onnx_clean_pass must NOT touch Stage 2");
    }
    fn name(&self) -> &'static str {
        "panic"
    }
    fn model_id(&self) -> &'static str {
        "panic"
    }
    fn policy_version(&self) -> &'static str {
        "panic"
    }
    fn threshold(&self) -> f32 {
        0.5
    }
}

#[tokio::test]
async fn onnx_clean_pass_returns_primary_scores_in_order() {
    // Scores straddle ONNX_CLEAN_THRESHOLD (0.10): some below, some at/above.
    // onnx_clean_pass must return the raw f64 scores in input order without
    // filtering — the caller decides what to do with them.
    let scores = vec![0.05, 0.15, 0.03, 0.90, 0.08];
    let scorer = TwoStageToxicityScorer::new(
        Box::new(IndexedScorer(scores.clone())),
        Arc::new(PanicClassifier),
    );

    let texts: Vec<String> = scores
        .iter()
        .enumerate()
        .map(|(i, _)| format!("post {}", i))
        .collect();

    let result = scorer.onnx_clean_pass(&texts).await.unwrap();

    assert_eq!(result.len(), scores.len(), "must return one score per text");
    for (i, (&expected, actual)) in scores.iter().zip(result.iter()).enumerate() {
        assert!(
            (expected - actual).abs() < 1e-9,
            "score[{}]: expected {expected}, got {actual}",
            i
        );
    }
}

#[tokio::test]
async fn onnx_clean_pass_never_invokes_classifier() {
    // PanicClassifier panics if called. The test passes only if the classifier
    // is truly never invoked — even for scores at/above ONNX_CLEAN_THRESHOLD.
    let scores = vec![0.50, 0.80, 0.99]; // all above threshold
    let scorer = TwoStageToxicityScorer::new(
        Box::new(IndexedScorer(scores.clone())),
        Arc::new(PanicClassifier),
    );

    let texts: Vec<String> = scores
        .iter()
        .enumerate()
        .map(|(i, _)| format!("text {}", i))
        .collect();

    // Must NOT panic (PanicClassifier was never called).
    let result = scorer.onnx_clean_pass(&texts).await.unwrap();
    assert_eq!(result.len(), 3);
}

#[tokio::test]
async fn onnx_clean_pass_empty_input_returns_empty() {
    let scorer = TwoStageToxicityScorer::new(
        Box::new(IndexedScorer(vec![0.50])),
        Arc::new(PanicClassifier),
    );
    let result = scorer.onnx_clean_pass(&[]).await.unwrap();
    assert!(result.is_empty());
}
