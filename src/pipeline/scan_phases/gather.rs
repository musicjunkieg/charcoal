// Phase A — `gather_account`: the I/O phase of the three-phase scan.
//
// For a single account this:
//   1. Fetches a 25-post Stage-1 sample and runs `stage1_outcome`.
//   2. On a terminal Stage-1 outcome (`< 5` posts → "Insufficient Data";
//      clean-and-irrelevant → early-exit "Low") it writes the terminal
//      `AccountScore` directly and stops — nothing is enqueued or stashed.
//   3. On `Proceed`, it replicates Stage 2's I/O (fetch 50 posts + parent
//      texts) but, INSTEAD of calling the RunPod/Zentropi classifier, runs the
//      ONNX clean-pass over the *same envelope text* Stage 2 would score. Each
//      post becomes a `QueueRow`: clean (ONNX < `ONNX_CLEAN_THRESHOLD`) → a
//      `done` row already labelled non-toxic (no classifier needed); survivor
//      → a `pending` row for Phase B to classify. It then stashes a versioned
//      `AccountInput` blob carrying everything Phase C needs to finalise the
//      score.
//
// The classifier is never invoked here — that GPU/API cost is deferred to
// Phase B (the "burst"). This is what lets the scan batch all classifier calls
// into one phase and stay inside the cost ceiling.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::posts::{self, PostSample};
use crate::bluesky::relationships::GraphDistance;
use crate::db::Database;
use crate::scoring::profile::{stage1_outcome, Stage1Outcome};
use crate::scoring::threat::ThreatWeights;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::ensemble::{TwoStageToxicityScorer, ONNX_CLEAN_THRESHOLD};
use crate::toxicity::traits::ToxicityScorer;

use super::staging::{AccountInput, QueueRow, ACCOUNT_INPUT_SCHEMA_VERSION};

/// Minimal post-fetch seam so Phase A's I/O can be exercised with canned data.
///
/// `posts::fetch_posts_with_replies` / `fetch_parent_posts` take a concrete
/// `&PublicAtpClient`, which can't be mocked. This trait wraps exactly those
/// two calls; the production [`AtpPostFetcher`] forwards to the real functions,
/// and tests supply a canned double. Kept deliberately small and local to this
/// task — it covers only the two fetches `gather_account` performs.
#[async_trait]
pub trait PostFetcher: Send + Sync {
    /// Fetch up to `limit` recent posts (with replies/quotes partitioned).
    async fn fetch_sample(&self, handle: &str, limit: usize) -> Result<PostSample>;

    /// Fetch parent post texts for the given AT URIs, keyed by URI.
    async fn fetch_parents(&self, uris: &[String]) -> Result<HashMap<String, String>>;
}

/// Production [`PostFetcher`] backed by the public AT Protocol client.
pub struct AtpPostFetcher<'a> {
    pub client: &'a PublicAtpClient,
}

#[async_trait]
impl PostFetcher for AtpPostFetcher<'_> {
    async fn fetch_sample(&self, handle: &str, limit: usize) -> Result<PostSample> {
        posts::fetch_posts_with_replies(self.client, handle, limit).await
    }

    async fn fetch_parents(&self, uris: &[String]) -> Result<HashMap<String, String>> {
        posts::fetch_parent_posts(self.client, uris).await
    }
}

/// The ONNX clean-pass seam used by Phase A's clean/survivor split.
///
/// Phase A must score the *exact same envelope* Stage 2's ONNX would, so it
/// reuses `TwoStageToxicityScorer::onnx_clean_pass`. That method is concrete,
/// so we expose it behind a tiny trait to keep `gather_account` testable with a
/// `ToxicityScorer` double. The production impl delegates straight through.
#[async_trait]
pub trait CleanPassScorer: Send + Sync {
    /// Return the raw ONNX toxicity score for each input text, in order.
    async fn onnx_clean_pass(&self, texts: &[String]) -> Result<Vec<f64>>;
}

#[async_trait]
impl CleanPassScorer for TwoStageToxicityScorer {
    async fn onnx_clean_pass(&self, texts: &[String]) -> Result<Vec<f64>> {
        // Delegate to the concrete seam from Task 2.1 (runs only the primary
        // ONNX scorer, never the Stage-2 classifier).
        TwoStageToxicityScorer::onnx_clean_pass(self, texts).await
    }
}

/// Per-account inputs Phase A needs in order to build the `AccountInput` blob.
///
/// These are all owned/derived by the caller (the scan orchestrator) and are
/// independent of the post sample — they're threaded straight into the blob so
/// Phase C can finalise the score without re-deriving them.
pub struct GatherInputs<'a> {
    /// DID of the account being gathered.
    pub account_did: &'a str,
    /// Handle used for fetching and Stage-1 logging.
    pub account_handle: &'a str,
    /// Protected user's topic fingerprint (Stage-1 overlap gate).
    pub protected_fingerprint: &'a TopicFingerprint,
    /// Scoring weights (overlap gate threshold, etc.).
    pub weights: &'a ThreatWeights,
    /// Median engagement across the scan run (behavioral normalisation).
    pub median_engagement: f64,
    /// Whether this account is in the precomputed pile-on DID set. Stored as a
    /// bare bool in the blob — the scan-global set is owned upstream.
    pub is_pile_on: bool,
    /// Direct (amplifier) text pairs for NLI context scoring, if any.
    pub direct_pairs: Option<&'a [(String, String)]>,
    /// Social graph distance from the protected user, if classified.
    pub graph_distance: Option<GraphDistance>,
}

/// Gather Phase A work for a single account.
///
/// On a terminal Stage-1 outcome, writes the terminal `AccountScore` and
/// returns — nothing is enqueued or stashed. On `Proceed`, enqueues one
/// `QueueRow` per Stage-2 post (clean rows pre-finalised as `done`/non-toxic,
/// survivors left `pending`) and stashes one `AccountInput` blob; it does NOT
/// write an `AccountScore` (Phase C scores survivors).
pub async fn gather_account(
    db: &Arc<dyn Database>,
    user_did: &str,
    fetcher: &dyn PostFetcher,
    scorer: &dyn ToxicityScorer,
    clean_pass: &dyn CleanPassScorer,
    inputs: &GatherInputs<'_>,
) -> Result<()> {
    // ── Stage 1: quick check with 25 posts ──
    let stage1_sample = fetcher.fetch_sample(inputs.account_handle, 25).await?;

    match stage1_outcome(
        &stage1_sample,
        scorer,
        inputs.account_handle,
        inputs.account_did,
        inputs.protected_fingerprint,
        inputs.weights,
        inputs.graph_distance,
    )
    .await?
    {
        Stage1Outcome::Terminal(score) => {
            // Non-threat: finalise directly, enqueue/stash nothing.
            db.upsert_account_score(user_did, &score).await?;
            return Ok(());
        }
        Stage1Outcome::Proceed { .. } => {}
    }

    // ── Stage 2: full I/O with 50 posts (no classification — that's Phase B) ──
    let sample = fetcher.fetch_sample(inputs.account_handle, 50).await?;

    let parent_uris: Vec<String> = sample
        .replies
        .iter()
        .map(|r| r.parent_uri.clone())
        .collect();
    let parent_texts = fetcher.fetch_parents(&parent_uris).await?;

    // Build the per-post rows. Order matches `score_from_sample`'s expectation
    // (originals ++ replies ++ quotes) so Phase C can reconstruct verdicts
    // positionally if needed. For each post we also build a TRANSIENT envelope
    // text — the exact string Stage 2's ONNX would score — and run the
    // clean-pass over those envelopes (NOT raw text) so a reply that's clean in
    // isolation but hostile in `[Parent]/[Reply]` context survives to Phase B.
    let mut rows: Vec<QueueRow> = Vec::with_capacity(sample.total_posts);
    let mut envelope_texts: Vec<String> = Vec::with_capacity(sample.total_posts);

    // Originals — raw text, no context.
    for p in &sample.originals {
        envelope_texts.push(p.text.clone());
        rows.push(survivor_row(
            inputs.account_did,
            &p.uri,
            &p.text,
            None,
            "original",
        ));
    }
    // Replies — envelope = format_parent_reply(parent, reply); context = parent.
    for r in &sample.replies {
        let parent = parent_texts.get(&r.parent_uri).cloned();
        let envelope = match &parent {
            Some(parent_text) => crate::toxicity::format_parent_reply(parent_text, &r.post.text),
            None => r.post.text.clone(),
        };
        envelope_texts.push(envelope);
        rows.push(survivor_row(
            inputs.account_did,
            &r.post.uri,
            &r.post.text,
            parent,
            "reply",
        ));
    }
    // Quotes — raw text, no context (first-person commentary).
    for p in &sample.quotes {
        envelope_texts.push(p.text.clone());
        rows.push(survivor_row(
            inputs.account_did,
            &p.uri,
            &p.text,
            None,
            "quote",
        ));
    }

    // Run the ONNX clean-pass over the envelope texts and split each post into
    // clean (done) vs survivor (pending) based on the envelope-based score.
    let onnx_scores = clean_pass.onnx_clean_pass(&envelope_texts).await?;
    anyhow::ensure!(
        onnx_scores.len() == rows.len(),
        "onnx_clean_pass returned {} scores for {} posts",
        onnx_scores.len(),
        rows.len()
    );
    for (row, onnx_score) in rows.iter_mut().zip(onnx_scores.iter()) {
        row.onnx_score = *onnx_score;
        if *onnx_score < ONNX_CLEAN_THRESHOLD {
            mark_clean(row);
        }
        // else: leave as a pending survivor (verdict fields stay None).
    }

    // Batch the writes: one enqueue, one stash.
    db.enqueue_classifications(user_did, &rows).await?;

    let blob = AccountInput {
        schema_version: ACCOUNT_INPUT_SCHEMA_VERSION,
        sample,
        parent_texts,
        median_engagement: inputs.median_engagement,
        is_pile_on: inputs.is_pile_on,
        direct_pairs: inputs.direct_pairs.map(|p| p.to_vec()),
        graph_distance: inputs.graph_distance.map(|d| d.as_str().to_string()),
        fingerprint_quality: fingerprint_quality(&blob_sample_counts(&rows)),
    };
    let payload = serde_json::to_string(&blob)?;
    db.stash_account_input(user_did, inputs.account_did, &payload)
        .await?;

    Ok(())
}

/// Build a survivor (`pending`) `QueueRow` with verdict fields unset.
///
/// `onnx_score` is filled in by the caller after the clean-pass runs; it
/// starts at the clean threshold's "unknown" default of `0.0` and is
/// overwritten below.
fn survivor_row(
    account_did: &str,
    post_uri: &str,
    text: &str,
    context_text: Option<String>,
    post_kind: &str,
) -> QueueRow {
    QueueRow {
        account_did: account_did.to_string(),
        post_uri: post_uri.to_string(),
        text: text.to_string(),
        context_text,
        post_kind: post_kind.to_string(),
        onnx_score: 0.0,
        status: "pending".to_string(),
        toxic_token: None,
        confidence: None,
        model_id: None,
        policy_version: None,
    }
}

/// Mark a row as ONNX-cleared: `done`, non-toxic, no classifier provenance.
///
/// The classifier never ran for clean rows, so `confidence`/`model_id`/
/// `policy_version` stay `None` — only `toxic_token` is set to `Some(false)`.
fn mark_clean(row: &mut QueueRow) {
    row.status = "done".to_string();
    row.toxic_token = Some(false);
    row.confidence = None;
    row.model_id = None;
    row.policy_version = None;
}

/// Counts of (originals, replies+quotes) inferred from the staged rows, used to
/// compute fingerprint quality identically to `score_from_sample`.
struct SampleCounts {
    originals: usize,
    replies_and_quotes: usize,
}

fn blob_sample_counts(rows: &[QueueRow]) -> SampleCounts {
    let originals = rows.iter().filter(|r| r.post_kind == "original").count();
    let replies_and_quotes = rows.len() - originals;
    SampleCounts {
        originals,
        replies_and_quotes,
    }
}

fn fingerprint_quality(counts: &SampleCounts) -> String {
    crate::bluesky::posts::FingerprintQuality::from_counts(
        counts.originals,
        counts.replies_and_quotes,
    )
    .as_str()
    .to_string()
}
