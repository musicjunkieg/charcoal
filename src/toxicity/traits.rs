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

/// Binary toxicity verdict for a single post — drives the threat formula's
/// toxicity rate. `onnx_score` is preserved for evidence sorting and audit logs.
#[derive(Debug, Clone)]
pub struct BinaryVerdict {
    /// True when the classifier flagged the post as toxic.
    pub is_toxic: bool,
    /// Continuous primary scorer output (typically ONNX), in [0.0, 1.0].
    /// Used for ranking evidence posts; not used by the threat formula.
    pub onnx_score: f64,
    /// Primary scorer category breakdown when available.
    pub onnx_attributes: ToxicityAttributes,
}

/// Default binary threshold used when a scorer has no classifier of its own.
const DEFAULT_BINARY_THRESHOLD: f64 = 0.50;

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

    /// Score text with optional context (e.g., the original post being
    /// replied to or quoted). Default implementation ignores context.
    async fn score_with_context(
        &self,
        text: &str,
        _context: Option<&str>,
    ) -> Result<ToxicityResult> {
        self.score_text(text).await
    }

    /// Classify a batch of texts as toxic or not, with optional per-post context.
    ///
    /// `contexts` must have the same length as `texts`. Each entry is the parent
    /// post text for replies, or `None` for originals/quotes.
    ///
    /// The default implementation derives binary verdicts from the continuous
    /// `score_with_context` output using `DEFAULT_BINARY_THRESHOLD` (0.50) — safe
    /// for any continuous scorer but coarse. Implementations with native binary
    /// classification (e.g. `TwoStageToxicityScorer`) should override.
    async fn classify_batch_with_contexts(
        &self,
        texts: &[String],
        contexts: &[Option<String>],
    ) -> Result<Vec<BinaryVerdict>> {
        if texts.len() != contexts.len() {
            anyhow::bail!(
                "classify_batch_with_contexts: texts.len() ({}) != contexts.len() ({})",
                texts.len(),
                contexts.len()
            );
        }
        let mut verdicts = Vec::with_capacity(texts.len());
        for (text, ctx) in texts.iter().zip(contexts.iter()) {
            let r = self.score_with_context(text, ctx.as_deref()).await?;
            verdicts.push(BinaryVerdict {
                is_toxic: r.toxicity >= DEFAULT_BINARY_THRESHOLD,
                onnx_score: r.toxicity,
                onnx_attributes: r.attributes,
            });
        }
        Ok(verdicts)
    }
}
