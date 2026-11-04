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

use anyhow::Result;
use async_trait::async_trait;

use crate::toxicity::classifier::{ClassifierVerdict, ItemOutcome, ToxicityClassifier};
use crate::toxicity::cost_meter::CostCeilingExceeded;

/// Refuses every classification as if the scan's cost ceiling had been hit.
pub struct EstimateOnlyClassifier;

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
        Err(refusal())
    }

    async fn classify_batch(&self, _contents: &[String]) -> Result<Vec<ItemOutcome>> {
        Err(refusal())
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
            let c = EstimateOnlyClassifier;
            let batch = rt.block_on(c.classify_batch(&contents));
            let err = batch.expect_err("a batch must never be classified");
            prop_assert!(err.downcast_ref::<CostCeilingExceeded>().is_some());
            for text in &contents {
                let one = rt.block_on(c.classify(text));
                let err = one.expect_err("a single text must never be classified");
                prop_assert!(err.downcast_ref::<CostCeilingExceeded>().is_some());
            }
        }
    }
}
