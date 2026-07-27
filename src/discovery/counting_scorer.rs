// Counting scorer — a dry-run instrument for tallying would-be Zentropi calls.
//
// This is a decorator over the real ONNX primary scorer. It implements the
// `ToxicityScorer` trait so it drops into `build_profile` (and therefore the
// whole scoring pipeline) unchanged, but instead of ever calling Zentropi it
// replicates the exact two-stage gate and *counts* the calls that would have
// been made.
//
// Faithfulness: Zentropi is only ever invoked from
// `TwoStageToxicityScorer::classify_post` (reached via
// `classify_batch_with_contexts`), and only for posts whose ONNX score is at or
// above `ONNX_CLEAN_THRESHOLD`. So this scorer:
//   - increments the would-be-call counter in exactly that method, per post
//     clearing the same shared threshold, using the same reply-envelope helper;
//   - delegates `score_text` / `score_batch` / `score_with_context` straight to
//     the primary, because in production those paths never touch Zentropi.
//
// Because it wraps *any* `ToxicityScorer`, it's unit-testable with a mock
// primary — no ONNX model or network required.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use serde::Serialize;

use crate::toxicity::ensemble::ONNX_CLEAN_THRESHOLD;
use crate::toxicity::format_parent_reply;
use crate::toxicity::traits::{BinaryVerdict, ToxicityResult, ToxicityScorer};

/// Atomic counters shared between a `CountingScorer` and its observers. Cheap to
/// clone (it's an `Arc` inside the scorer) and safe to read across the
/// concurrent follower-scoring tasks the pipeline spawns.
#[derive(Debug, Default)]
pub struct CountingStats {
    /// Posts pushed through the two-stage classification path.
    posts_classified: AtomicU64,
    /// Posts the ONNX clean-pass filter cleared (< threshold) — no Zentropi.
    posts_cleared: AtomicU64,
    /// Posts at/above threshold that would have triggered a Zentropi call.
    zentropi_calls: AtomicU64,
}

/// An immutable point-in-time read of the counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CountSnapshot {
    pub posts_classified: u64,
    pub posts_cleared: u64,
    pub zentropi_calls: u64,
}

impl CountSnapshot {
    /// Component-wise difference (`self - earlier`), saturating at 0. Used to
    /// attribute a slice of counter activity to one candidate's scoring run.
    pub fn delta_from(&self, earlier: &CountSnapshot) -> CountSnapshot {
        CountSnapshot {
            posts_classified: self
                .posts_classified
                .saturating_sub(earlier.posts_classified),
            posts_cleared: self.posts_cleared.saturating_sub(earlier.posts_cleared),
            zentropi_calls: self.zentropi_calls.saturating_sub(earlier.zentropi_calls),
        }
    }
}

impl CountingStats {
    /// Read all three counters into a snapshot.
    pub fn snapshot(&self) -> CountSnapshot {
        CountSnapshot {
            posts_classified: self.posts_classified.load(Ordering::Relaxed),
            posts_cleared: self.posts_cleared.load(Ordering::Relaxed),
            zentropi_calls: self.zentropi_calls.load(Ordering::Relaxed),
        }
    }
}

/// A `ToxicityScorer` that counts would-be Zentropi calls instead of making them.
pub struct CountingScorer {
    primary: Box<dyn ToxicityScorer>,
    stats: Arc<CountingStats>,
}

impl CountingScorer {
    /// Wrap a primary (typically the real ONNX scorer) with call counting.
    pub fn new(primary: Box<dyn ToxicityScorer>) -> Self {
        Self {
            primary,
            stats: Arc::new(CountingStats::default()),
        }
    }

    /// A handle to the shared counters — read it before/after a scoring run to
    /// attribute counts to a candidate, or at the end for the global total.
    pub fn stats(&self) -> Arc<CountingStats> {
        self.stats.clone()
    }
}

#[async_trait]
impl ToxicityScorer for CountingScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        // ONNX-only path in production — no Zentropi, so no counting.
        self.primary.score_text(text).await
    }

    async fn score_with_context(
        &self,
        text: &str,
        context: Option<&str>,
    ) -> Result<ToxicityResult> {
        self.primary.score_with_context(text, context).await
    }

    /// The two-stage path. Mirrors `TwoStageToxicityScorer::classify_post`: score
    /// the reply envelope (or the text for originals) with ONNX, apply the same
    /// clean-pass threshold, and count a would-be Zentropi call for everything
    /// that clears it. No Zentropi request is made and verdicts are reported as
    /// non-toxic — we are measuring call volume, not classifying.
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
            // For replies, the production scorer runs ONNX on the parent/reply
            // envelope (so context-dependent toxicity reaches the gate). Match
            // that exactly via the shared helper.
            let envelope_owned;
            let input = match ctx {
                Some(parent) => {
                    envelope_owned = format_parent_reply(parent, text);
                    envelope_owned.as_str()
                }
                None => text.as_str(),
            };

            let result = self.primary.score_text(input).await?;
            self.stats.posts_classified.fetch_add(1, Ordering::Relaxed);
            if result.toxicity < ONNX_CLEAN_THRESHOLD {
                self.stats.posts_cleared.fetch_add(1, Ordering::Relaxed);
            } else {
                self.stats.zentropi_calls.fetch_add(1, Ordering::Relaxed);
            }

            verdicts.push(BinaryVerdict {
                is_toxic: false,
                onnx_score: result.toxicity,
                onnx_attributes: result.attributes,
            });
        }
        Ok(verdicts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toxicity::traits::ToxicityAttributes;

    /// Mock primary that returns a fixed toxicity for every text.
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

    fn texts(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("post {i}")).collect()
    }

    fn no_contexts(n: usize) -> Vec<Option<String>> {
        vec![None; n]
    }

    #[tokio::test]
    async fn all_clean_makes_no_calls() {
        let scorer = CountingScorer::new(Box::new(FixedScorer(0.0)));
        scorer
            .classify_batch_with_contexts(&texts(5), &no_contexts(5))
            .await
            .unwrap();
        let s = scorer.stats().snapshot();
        assert_eq!(s.posts_classified, 5);
        assert_eq!(s.posts_cleared, 5);
        assert_eq!(s.zentropi_calls, 0);
    }

    #[tokio::test]
    async fn all_toxic_makes_a_call_per_post() {
        let scorer = CountingScorer::new(Box::new(FixedScorer(0.9)));
        scorer
            .classify_batch_with_contexts(&texts(4), &no_contexts(4))
            .await
            .unwrap();
        let s = scorer.stats().snapshot();
        assert_eq!(s.posts_classified, 4);
        assert_eq!(s.posts_cleared, 0);
        assert_eq!(s.zentropi_calls, 4);
    }

    #[tokio::test]
    async fn exactly_at_threshold_counts_as_a_call() {
        // The gate is `< threshold` cleared, so a score equal to the threshold
        // is NOT cleared — it would hit Zentropi. Mirror that boundary.
        let scorer = CountingScorer::new(Box::new(FixedScorer(ONNX_CLEAN_THRESHOLD)));
        scorer
            .classify_batch_with_contexts(&texts(1), &no_contexts(1))
            .await
            .unwrap();
        assert_eq!(scorer.stats().snapshot().zentropi_calls, 1);
    }

    #[tokio::test]
    async fn just_below_threshold_is_cleared() {
        let scorer = CountingScorer::new(Box::new(FixedScorer(ONNX_CLEAN_THRESHOLD - 0.001)));
        scorer
            .classify_batch_with_contexts(&texts(1), &no_contexts(1))
            .await
            .unwrap();
        let s = scorer.stats().snapshot();
        assert_eq!(s.posts_cleared, 1);
        assert_eq!(s.zentropi_calls, 0);
    }

    #[tokio::test]
    async fn reply_context_still_counts_once_per_post() {
        // A reply (Some(parent)) goes through the envelope path but still counts
        // as a single classified post.
        let scorer = CountingScorer::new(Box::new(FixedScorer(0.5)));
        let contexts = vec![Some("parent post".to_string())];
        scorer
            .classify_batch_with_contexts(&texts(1), &contexts)
            .await
            .unwrap();
        let s = scorer.stats().snapshot();
        assert_eq!(s.posts_classified, 1);
        assert_eq!(s.zentropi_calls, 1);
    }

    #[tokio::test]
    async fn score_text_does_not_count() {
        // The ONNX-only path must not touch the Zentropi counter.
        let scorer = CountingScorer::new(Box::new(FixedScorer(0.9)));
        scorer.score_text("hello").await.unwrap();
        scorer
            .score_with_context("hi", Some("parent"))
            .await
            .unwrap();
        assert_eq!(
            scorer.stats().snapshot(),
            CountSnapshot {
                posts_classified: 0,
                posts_cleared: 0,
                zentropi_calls: 0,
            }
        );
    }

    #[tokio::test]
    async fn length_mismatch_is_an_error() {
        let scorer = CountingScorer::new(Box::new(FixedScorer(0.0)));
        let err = scorer
            .classify_batch_with_contexts(&texts(2), &no_contexts(3))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("texts.len()"));
    }

    #[test]
    fn snapshot_delta_subtracts_componentwise() {
        let before = CountSnapshot {
            posts_classified: 10,
            posts_cleared: 6,
            zentropi_calls: 4,
        };
        let after = CountSnapshot {
            posts_classified: 25,
            posts_cleared: 15,
            zentropi_calls: 10,
        };
        let d = after.delta_from(&before);
        assert_eq!(d.posts_classified, 15);
        assert_eq!(d.posts_cleared, 9);
        assert_eq!(d.zentropi_calls, 6);
    }
}
