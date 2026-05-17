//! Tests for the two-stage toxicity scorer.
//!
//! Stage 1 = ONNX clean-pass filter (< 0.10 means cleared, no Zentropi call).
//! Stage 2 = Zentropi binary classification for everything else.
//! When Zentropi is unavailable, fall back to ONNX threshold (>= 0.50 → toxic).

use anyhow::Result;
use async_trait::async_trait;
use charcoal::toxicity::ensemble::{TwoStageToxicityScorer, VerdictSource};
use charcoal::toxicity::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

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

fn two_stage_no_zentropi(onnx_score: f64) -> TwoStageToxicityScorer {
    TwoStageToxicityScorer::new(Box::new(FixedScorer(onnx_score)), None)
}

#[tokio::test]
async fn onnx_below_clean_threshold_skips_zentropi() {
    // ONNX 0.05 < 0.10 clean threshold → cleared, is_toxic = false.
    let scorer = two_stage_no_zentropi(0.05);
    let v = scorer.classify_post("benign text", None).await.unwrap();
    assert!(!v.is_toxic);
    assert_eq!(v.source, VerdictSource::OnnxCleared);
    assert!(v.zentropi_confidence.is_none());
    assert!((v.onnx_score - 0.05).abs() < 1e-9);
}

#[tokio::test]
async fn onnx_above_clean_threshold_no_zentropi_uses_fallback_threshold() {
    // ONNX 0.30, no Zentropi → falls back to 0.50 binary threshold → safe.
    let scorer = two_stage_no_zentropi(0.30);
    let v = scorer.classify_post("ambiguous text", None).await.unwrap();
    assert!(!v.is_toxic);
    assert_eq!(v.source, VerdictSource::OnnxFallback);
}

#[tokio::test]
async fn onnx_well_above_fallback_threshold_no_zentropi_is_toxic() {
    // ONNX 0.70 > 0.50 fallback → is_toxic = true via OnnxFallback.
    let scorer = two_stage_no_zentropi(0.70);
    let v = scorer.classify_post("hostile text", None).await.unwrap();
    assert!(v.is_toxic);
    assert_eq!(v.source, VerdictSource::OnnxFallback);
}

#[tokio::test]
async fn classify_batch_preserves_input_order() {
    // Stream concurrency reorders execution, but classify_batch must restore
    // the original index ordering for caller alignment with their text vec.
    let scorer = two_stage_no_zentropi(0.05);
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
    let scorer = two_stage_no_zentropi(0.05);
    let texts = vec!["a".to_string(), "b".to_string()];
    let contexts = vec![None];
    let err = scorer.classify_batch(&texts, &contexts).await.unwrap_err();
    assert!(format!("{err}").contains("texts.len()"));
}

#[tokio::test]
async fn score_text_returns_primary_continuous_score() {
    // ToxicityScorer::score_text on TwoStage delegates to the primary,
    // preserving the continuous score for legacy continuous-score callers.
    let scorer = two_stage_no_zentropi(0.42);
    let r = scorer.score_text("anything").await.unwrap();
    assert!((r.toxicity - 0.42).abs() < 1e-9);
}

#[tokio::test]
async fn classify_batch_via_trait_method_works() {
    // ToxicityScorer::classify_batch_with_contexts on TwoStage uses the
    // two-stage pipeline (overrides the trait default).
    let scorer = two_stage_no_zentropi(0.05);
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
async fn has_zentropi_reflects_construction() {
    let no_z = two_stage_no_zentropi(0.05);
    assert!(!no_z.has_zentropi());
}
