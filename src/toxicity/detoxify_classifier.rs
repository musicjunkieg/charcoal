//! Detoxify (the local ONNX toxicity model) behind the classifier trait, so
//! the eval can use it as the free floor every paid model must beat.

use anyhow::Result;
use async_trait::async_trait;
use std::time::Instant;

use super::classifier::{ClassifierVerdict, ToxicityClassifier};
use super::traits::ToxicityScorer;

pub struct DetoxifyClassifier<S: ToxicityScorer> {
    scorer: S,
}

impl<S: ToxicityScorer> DetoxifyClassifier<S> {
    pub fn new(scorer: S) -> Self {
        Self { scorer }
    }
}

#[async_trait]
impl<S: ToxicityScorer> ToxicityClassifier for DetoxifyClassifier<S> {
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
        let started = Instant::now();
        let t = self.scorer.score_text(content).await?.toxicity as f32;
        // `NaN >= 0.5` is false: without this check a broken model output
        // would be recorded as a clean verdict. Errors are never clean. This
        // backend batches one post per call (the trait default), so an `Err`
        // here marks only this post as an error.
        if !t.is_finite() {
            anyhow::bail!("detoxify returned a toxicity score that is not a finite number ({t})");
        }
        let toxic = t >= 0.5;
        Ok(ClassifierVerdict {
            toxic_token: toxic,
            confidence: if toxic { t } else { 1.0 - t },
            latency_ms: started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32,
            model_id: self.model_id().to_string(),
            policy_version: self.policy_version().to_string(),
        })
    }
    fn name(&self) -> &'static str {
        "detoxify"
    }
    fn model_id(&self) -> &'static str {
        // Production's id, revision included: the eval cache keys on it.
        super::onnx::ONNX_MODEL_ID
    }
    fn policy_version(&self) -> &'static str {
        "none"
    }
    fn threshold(&self) -> f32 {
        0.5
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toxicity::traits::{ToxicityAttributes, ToxicityResult};

    struct Fixed(f64);
    #[async_trait::async_trait]
    impl ToxicityScorer for Fixed {
        async fn score_text(&self, _text: &str) -> anyhow::Result<ToxicityResult> {
            Ok(ToxicityResult {
                toxicity: self.0,
                attributes: ToxicityAttributes::default(),
            })
        }
    }

    /// The eval cache keys reuse on model_id, so it must carry the ONNX
    /// revision (a model fix like #400's `-r2` must invalidate old verdicts).
    #[test]
    fn model_id_is_the_production_onnx_id() {
        assert_eq!(
            DetoxifyClassifier::new(Fixed(0.0)).model_id(),
            crate::toxicity::onnx::ONNX_MODEL_ID
        );
    }

    #[tokio::test]
    async fn exactly_half_is_toxic_just_below_is_not() {
        let at = DetoxifyClassifier::new(Fixed(0.5))
            .classify("x")
            .await
            .unwrap();
        assert!(at.toxic_token);
        let below = DetoxifyClassifier::new(Fixed(0.4999))
            .classify("x")
            .await
            .unwrap();
        assert!(!below.toxic_token);
    }

    /// Final review fix 3: `NaN >= 0.5` is false, so a broken model output
    /// used to be recorded as clean. Errors are never clean.
    #[tokio::test]
    async fn a_non_finite_score_is_an_error_not_clean() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = DetoxifyClassifier::new(Fixed(bad))
                .classify("x")
                .await
                .expect_err("a non-finite score must not become a verdict");
            assert!(format!("{err:#}").contains("not a finite"), "{err:#}");
        }
    }

    #[tokio::test]
    async fn the_answer_is_detoxifys_own_half_way_cut() {
        assert!(
            DetoxifyClassifier::new(Fixed(0.8))
                .classify("x")
                .await
                .unwrap()
                .toxic_token
        );
        assert!(
            !DetoxifyClassifier::new(Fixed(0.3))
                .classify("x")
                .await
                .unwrap()
                .toxic_token
        );
    }
}
