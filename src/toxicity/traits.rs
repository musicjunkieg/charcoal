// Toxicity scorer trait — the swap-ready abstraction.
//
// This trait defines the interface for toxicity scoring. The default
// implementation uses Google's Perspective API, but when it sunsets
// (Dec 2026), we can swap in OpenAI's moderation endpoint, a HuggingFace
// model, or a local rust-bert model — no other code changes needed.

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
