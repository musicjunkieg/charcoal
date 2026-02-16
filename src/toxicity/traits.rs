// Toxicity scorer trait — the swap-ready abstraction.
//
// This trait defines the interface for toxicity scoring. The default
// implementation uses a local ONNX model (Detoxify unbiased-toxic-roberta).
// Google's Perspective API is available as a fallback.

use anyhow::Result;
use async_trait::async_trait;

/// The result of scoring a single piece of text for toxicity.
#[derive(Debug, Clone)]
pub struct ToxicityResult {
    /// Overall toxicity score from 0.0 (benign) to 1.0 (very toxic)
    pub toxicity: f64,
    /// Breakdown of specific attributes (if the provider supports them)
    pub attributes: ToxicityAttributes,
}

/// Detailed toxicity attribute scores (all 0.0 to 1.0).
/// Not all providers will populate every field.
#[derive(Debug, Clone, Default)]
pub struct ToxicityAttributes {
    pub severe_toxicity: Option<f64>,
    pub identity_attack: Option<f64>,
    pub insult: Option<f64>,
    pub profanity: Option<f64>,
    pub threat: Option<f64>,
}

/// No-op scorer used when toxicity scoring isn't needed (e.g. scan without --analyze).
/// Panics if actually called — ensures we don't silently produce fake scores.
pub struct NoopScorer;

#[async_trait]
impl ToxicityScorer for NoopScorer {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        anyhow::bail!("NoopScorer should never be called — use --analyze to enable scoring")
    }
}

/// Trait for scoring text toxicity. Implementations must be async because
/// most providers require HTTP API calls.
#[async_trait]
pub trait ToxicityScorer: Send + Sync {
    /// Score a single text for toxicity.
    async fn score_text(&self, text: &str) -> Result<ToxicityResult>;

    /// Score multiple texts, returning results in the same order.
    /// Default implementation calls score_text sequentially — providers
    /// can override for batching if they support it.
    async fn score_batch(&self, texts: &[String]) -> Result<Vec<ToxicityResult>> {
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.score_text(text).await?);
        }
        Ok(results)
    }
}
