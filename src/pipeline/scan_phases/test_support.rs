// Never-called pipeline seams, shipped in the library rather than behind
// `#[cfg(test)]` (#344 V7-03).
//
// They live here for the same reason `StubClassifier` lives in
// `toxicity::classifier`: the properties they exist to prove — a candidate-less
// run still resumes or refuses staged work, and an unreadable skip count is a
// distinct outcome from a zero one — have to be assertable from BOTH the
// inline tests in `web::scan_job` and the integration tests in
// `tests/unit_scan_phases.rs`. A `#[cfg(test)]` module in the pipeline is
// invisible to the second.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::bluesky::posts::PostSample;
use crate::db::Database;
use crate::pipeline::scan_phases::gather::{CleanPassScorer, PostFetcher};
use crate::pipeline::scan_phases::staging::{ClassifierIdentity, EvidenceContract};
use crate::pipeline::scan_phases::{PhasedScanDeps, SkipCounter};
use crate::scoring::threat::ThreatWeights;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::classifier::ToxicityClassifier;
use crate::toxicity::traits::{ToxicityResult, ToxicityScorer};

/// A skip counter that errors on its FIRST call and delegates on every one
/// after (V7-03).
///
/// Failing the whole table (dropping `scan_skips`) would also break the
/// operations that follow — the fresh start's `clear_scan_skips`, the resumed
/// run's own count — so a test using that fixture could not tell "the drain's
/// read failed" from "everything after it failed too". One armed failure can.
pub struct FailOnce {
    inner: Arc<dyn Database>,
    armed: AtomicBool,
    calls: AtomicUsize,
}

impl FailOnce {
    pub fn new(inner: Arc<dyn Database>) -> Self {
        Self {
            inner,
            armed: AtomicBool::new(true),
            calls: AtomicUsize::new(0),
        }
    }

    /// True once the armed failure has actually been consumed — so a test can
    /// assert the read it meant to break is the read that broke.
    pub fn fired(&self) -> bool {
        !self.armed.load(Ordering::SeqCst)
    }

    /// How many counts were requested in total.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SkipCounter for FailOnce {
    async fn count(&self, user_did: &str) -> Result<i64> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.armed.swap(false, Ordering::SeqCst) {
            anyhow::bail!("simulated scan_skips read failure");
        }
        self.inner.count_scan_skips(user_did).await
    }
}

/// A fetcher that panics if it is ever asked for anything.
struct NeverFetcher;

#[async_trait]
impl PostFetcher for NeverFetcher {
    async fn fetch_sample(&self, _did: &str, _handle: &str, _limit: usize) -> Result<PostSample> {
        panic!("fetch_sample called: this run has no candidates to gather");
    }
    async fn fetch_parents(
        &self,
        _uris: &[String],
    ) -> Result<std::collections::HashMap<String, String>> {
        panic!("fetch_parents called: this run has no candidates to gather");
    }
}

/// A Stage-1 scorer that panics if it is ever asked for anything.
struct NeverScorer;

#[async_trait]
impl ToxicityScorer for NeverScorer {
    async fn score_text(&self, _text: &str) -> Result<ToxicityResult> {
        panic!("score_text called: this run has no candidates to gather");
    }
}

#[async_trait]
impl CleanPassScorer for NeverScorer {
    async fn onnx_clean_pass(&self, _texts: &[String]) -> Result<Vec<f64>> {
        panic!("onnx_clean_pass called: this run has no candidates to gather");
    }
}

/// [`PhasedScanDeps`] for candidate-less `run_phased_scan` calls: the gather
/// seams panic if reached, which is what makes "no candidates went through
/// gather" an assertion rather than a hope.
///
/// With no candidates a fresh start is gather(∅) → burst(∅) → finalize(∅) →
/// `Done`, and a seeded resumable marker is resumed (own kind) or surfaces
/// `OwnedByOtherKind` (other kind) — exactly the V4-02 paths.
pub struct EmptyDeps {
    classifier: Arc<dyn ToxicityClassifier>,
    fingerprint: TopicFingerprint,
    weights: ThreatWeights,
    never: NeverScorer,
    fetcher: NeverFetcher,
}

impl EmptyDeps {
    pub fn new(classifier: Arc<dyn ToxicityClassifier>) -> Self {
        Self {
            classifier,
            fingerprint: TopicFingerprint {
                clusters: vec![],
                post_count: 0,
            },
            weights: ThreatWeights::default(),
            never: NeverScorer,
            fetcher: NeverFetcher,
        }
    }

    /// Build the borrowing deps. `skip_counter` is the V7-03 seam — `None`
    /// reads the real `count_scan_skips`.
    pub fn deps<'a>(&'a self, skip_counter: Option<&'a dyn SkipCounter>) -> PhasedScanDeps<'a> {
        PhasedScanDeps {
            fetcher: &self.fetcher,
            scorer: &self.never,
            clean_pass: &self.never,
            classifier: &self.classifier,
            protected_fingerprint: &self.fingerprint,
            weights: &self.weights,
            embedder: None,
            protected_embedding: None,
            protected_topic_centroids: None,
            nli_scorer: None,
            protected_posts_with_embeddings: None,
            data_dir: None,
            median_engagement: 1.0,
            gather_concurrency: 1,
            burst_concurrency: 1,
            burst_batch: 100,
            evidence: EvidenceContract {
                onnx_model_id: crate::toxicity::onnx::ONNX_MODEL_ID,
                classifier: ClassifierIdentity {
                    model_id: self.classifier.model_id(),
                    policy_version: self.classifier.policy_version(),
                },
            },
            skip_counter,
        }
    }
}
