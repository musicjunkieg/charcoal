//! Per-post isolation in the Phase-A clean pass (#221, follow-up to #220).
//!
//! #220's real damage was not that one post failed to score — it was that
//! scoring is BATCHED PER ACCOUNT, so a single unscoreable post took the whole
//! batch down and gather dropped the entire account, including all of its
//! perfectly scoreable posts. On the 2026-07-19 staging scan that turned a 4.3%
//! post-level failure rate into 100% account loss for 34 accounts.
//!
//! #220 fixed the specific cause (over-long input). This bounds the blast
//! radius of the NEXT one, whatever it turns out to be.
//!
//! Design note on the unscoreable post itself: it is DROPPED, not assigned a
//! fallback score. Every sentinel value would be wrong somewhere — the score
//! feeds an early-exit gate in scoring/profile.rs that treats
//! `< ONNX_CLEAN_THRESHOLD` as clean (so a low sentinel silently passes a post
//! nobody scored) and it drives evidence sorting (so a high sentinel puts a
//! post we could not read at the top of the evidence list). Absence is the only
//! honest representation of "we could not score this".

use anyhow::{bail, Result};
use async_trait::async_trait;
use charcoal::pipeline::scan_phases::gather::{clean_pass_isolated, CleanPassScorer};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Fails the whole batch whenever it contains the poison text, mimicking an
/// ONNX graph error. Succeeds for any batch without it. Counts calls so tests
/// can assert the retry actually happened per item.
struct PoisonCleanPass {
    poison: String,
    calls: AtomicUsize,
}

impl PoisonCleanPass {
    fn new(poison: &str) -> Self {
        Self {
            poison: poison.to_string(),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl CleanPassScorer for PoisonCleanPass {
    async fn onnx_clean_pass(&self, texts: &[String]) -> Result<Vec<f64>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if texts.iter().any(|t| t.contains(&self.poison)) {
            bail!("ONNX inference failed: invalid expand shape");
        }
        Ok(texts.iter().map(|_| 0.05).collect())
    }
}

/// Never succeeds, for asserting the total-failure path.
struct AlwaysFails;

#[async_trait]
impl CleanPassScorer for AlwaysFails {
    async fn onnx_clean_pass(&self, _texts: &[String]) -> Result<Vec<f64>> {
        bail!("ONNX inference failed")
    }
}

fn texts(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[tokio::test]
async fn healthy_batch_scores_in_one_call() {
    let scorer = PoisonCleanPass::new("POISON");
    let input = texts(&["hello", "world", "friend"]);

    let scores = clean_pass_isolated(&scorer, &input).await;

    assert_eq!(scores, vec![Some(0.05), Some(0.05), Some(0.05)]);
    assert_eq!(
        scorer.calls.load(Ordering::SeqCst),
        1,
        "a healthy batch must not pay for per-item retries"
    );
}

#[tokio::test]
async fn one_bad_post_does_not_lose_the_others() {
    let scorer = PoisonCleanPass::new("POISON");
    let input = texts(&["hello", "POISON here", "friend"]);

    let scores = clean_pass_isolated(&scorer, &input).await;

    // THE POINT OF #221: the good posts survive.
    assert_eq!(scores[0], Some(0.05));
    assert_eq!(scores[2], Some(0.05));
    assert_eq!(
        scores[1], None,
        "the unscoreable post must be marked absent"
    );
}

#[tokio::test]
async fn positions_are_preserved_so_scores_cannot_be_misattributed() {
    let scorer = PoisonCleanPass::new("POISON");
    let input = texts(&["a", "POISON", "c", "POISON too", "e"]);

    let scores = clean_pass_isolated(&scorer, &input).await;

    // A shifted result would silently attach one post's score to another post —
    // worse than failing, because nothing would look wrong.
    assert_eq!(scores.len(), input.len());
    assert_eq!(scores, vec![Some(0.05), None, Some(0.05), None, Some(0.05)]);
}

#[tokio::test]
async fn retries_per_item_only_after_a_batch_failure() {
    let scorer = PoisonCleanPass::new("POISON");
    let input = texts(&["a", "POISON", "c"]);

    let _ = clean_pass_isolated(&scorer, &input).await;

    // 1 failed batch attempt + 3 individual retries.
    assert_eq!(scorer.calls.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_totally_broken_scorer_yields_all_none_rather_than_erroring() {
    let scorer = AlwaysFails;
    let input = texts(&["a", "b"]);

    let scores = clean_pass_isolated(&scorer, &input).await;

    // Even a completely dead scorer must not propagate an error out of gather —
    // that is precisely the account-loss path #221 exists to close. The caller
    // decides what an all-unscoreable account means.
    assert_eq!(scores, vec![None, None]);
}

#[tokio::test]
async fn empty_input_is_a_no_op() {
    let scorer = PoisonCleanPass::new("POISON");

    let scores = clean_pass_isolated(&scorer, &[]).await;

    assert!(scores.is_empty());
    assert_eq!(
        scorer.calls.load(Ordering::SeqCst),
        0,
        "no posts means no scorer call at all"
    );
}

// NOTE: the end-to-end proof that this is wired into `gather_account` lives in
// tests/unit_scan_phases.rs, where the PostSample/fingerprint/inputs fixtures
// already exist — see `one_poisoned_post_no_longer_costs_the_whole_account`.
