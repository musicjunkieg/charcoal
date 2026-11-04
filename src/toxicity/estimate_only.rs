// A stand-in Stage-2 classifier for free scan estimates (#424).
//
// `charcoal estimate` runs the REAL full-scan pipeline, so the number of posts
// it reports is exactly what a live scan would send to the GPU. It swaps in
// this classifier for RunPod. Its first batch reports "cost ceiling reached",
// which the burst already handles: it stops before recording any verdict and
// leaves every queued post pending, the same resumable pause a real cost cap
// produces. Nothing leaves the machine.
//
// It is deliberately NOT selectable through `CHARCOAL_CLASSIFIER`. A deployment
// that picked it by mistake would pause every scan without classifying
// anything, so the only way to build one is in code, from the estimate command.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use async_trait::async_trait;

use crate::toxicity::classifier::{ClassifierVerdict, ItemOutcome, ToxicityClassifier};
use crate::toxicity::cost_meter::CostCeilingExceeded;

/// Refuses every classification as if the scan's cost ceiling had been hit,
/// and counts what it was asked, because not every GPU request goes through
/// the burst queue.
///
/// Direct quote and reply scoring during discovery calls the classifier one
/// post at a time and never enqueues anything. Those requests are billed in a
/// live scan, so the estimate must count them here; the queue cannot.
#[derive(Default)]
pub struct EstimateOnlyClassifier {
    /// Texts refused across every call.
    texts_refused: AtomicU64,
    /// Texts in the most recent call.
    last_call_texts: AtomicU64,
}

impl EstimateOnlyClassifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Requests made outside the burst queue, given how many posts the queue
    /// still holds.
    ///
    /// The burst's refused call is always the LAST call (it pauses the scan;
    /// nothing classifies after it) and it carries only queued posts, which
    /// the caller counts from the queue. So when anything is queued, the
    /// direct requests are everything except that last call. This needs the
    /// burst to make exactly ONE call, which `charcoal estimate` guarantees by
    /// running it at concurrency 1: the refusal stops the burst before a
    /// second chunk can start.
    pub fn direct_requests(&self, queued: u64) -> u64 {
        let total = self.texts_refused.load(Ordering::SeqCst);
        if queued == 0 {
            return total;
        }
        total.saturating_sub(self.last_call_texts.load(Ordering::SeqCst))
    }

    fn refuse(&self, texts: usize) -> anyhow::Error {
        let texts = texts as u64;
        self.texts_refused.fetch_add(texts, Ordering::SeqCst);
        self.last_call_texts.store(texts, Ordering::SeqCst);
        refusal()
    }
}

/// The refusal both entry points return. Zero cents over a zero ceiling: the
/// estimate spent nothing, and the message must not suggest it did.
fn refusal() -> anyhow::Error {
    CostCeilingExceeded {
        est_cents: 0,
        ceiling_cents: 0,
    }
    .into()
}

#[async_trait]
impl ToxicityClassifier for EstimateOnlyClassifier {
    async fn classify(&self, _content: &str) -> Result<ClassifierVerdict> {
        Err(self.refuse(1))
    }

    async fn classify_batch(&self, contents: &[String]) -> Result<Vec<ItemOutcome>> {
        Err(self.refuse(contents.len()))
    }

    // `probe_identity` keeps the default `Ok(())`: there is no remote endpoint
    // whose policy could disagree with this binary.

    fn name(&self) -> &'static str {
        "estimate-only"
    }
    fn model_id(&self) -> &'static str {
        "estimate-only"
    }
    fn policy_version(&self) -> &'static str {
        "estimate-only"
    }
    fn threshold(&self) -> f32 {
        // Never consulted: no verdict is ever produced to compare against it.
        1.0
    }
}

#[cfg(test)]
mod props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        // Invariant: whatever the burst hands it, the stand-in answers with
        // the cost-ceiling error and never with a verdict. That error is what
        // makes the burst pause with every row still pending; any other error
        // would fail the scan, and any verdict would be a fabricated score.
        #[test]
        fn every_batch_is_refused_as_a_cost_cap(
            contents in proptest::collection::vec(".{0,40}", 0..20)
        ) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let c = EstimateOnlyClassifier::new();
            let batch = rt.block_on(c.classify_batch(&contents));
            let err = batch.expect_err("a batch must never be classified");
            prop_assert!(err.downcast_ref::<CostCeilingExceeded>().is_some());
            for text in &contents {
                let one = rt.block_on(c.classify(text));
                let err = one.expect_err("a single text must never be classified");
                prop_assert!(err.downcast_ref::<CostCeilingExceeded>().is_some());
            }
        }

        // Invariant: every request outside the final (burst) call is counted
        // as a direct request, and the burst call itself never is, because
        // its posts are already counted from the queue. With nothing queued
        // there was no burst call, so every request was direct.
        #[test]
        fn direct_requests_exclude_only_the_final_burst_call(
            direct in 0usize..50,
            burst in 1usize..600,
        ) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let c = EstimateOnlyClassifier::new();
            for _ in 0..direct {
                let _ = rt.block_on(c.classify("a quote"));
            }
            prop_assert_eq!(c.direct_requests(0), direct as u64);
            let _ = rt.block_on(c.classify_batch(&vec![String::from("queued"); burst]));
            prop_assert_eq!(c.direct_requests(burst as u64), direct as u64);
        }
    }
}
