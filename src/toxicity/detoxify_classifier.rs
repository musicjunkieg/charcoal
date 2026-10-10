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
        "unbiased-toxic-roberta"
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
