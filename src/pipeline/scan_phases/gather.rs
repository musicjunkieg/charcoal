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
use crate::scoring::profile::{select_fingerprint_posts, stage1_outcome, Stage1Outcome};
use crate::scoring::threat::ThreatWeights;
use crate::topics::embeddings::{mean_embedding, SentenceEmbedder};
use crate::topics::fingerprint::TopicFingerprint;
use tracing::warn;

use crate::toxicity::ensemble::{TwoStageToxicityScorer, ONNX_CLEAN_THRESHOLD};
use crate::toxicity::traits::ToxicityScorer;

use super::staging::{AccountInput, QueueRow, ACCOUNT_INPUT_SCHEMA_VERSION};

/// What `gather_account` did for one account.
///
/// The orchestrator needs to know whether Phase A already wrote a terminal
/// `AccountScore` so it can count those accounts toward `accounts_scored`
/// (early-exit / insufficient-data accounts never reach Phase C finalize, so
/// finalize alone undercounts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatherOutcome {
    /// Stage 1 was terminal: an `AccountScore` was upserted here. Nothing was
    /// enqueued or stashed — this account will NOT appear in Phase C.
    Terminal,
    /// Stage 1 said `Proceed`: queue rows were enqueued and a blob stashed.
    /// Phase C will finalize this account.
    Enqueued,
}

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

/// Score every text, isolating per-post failures so one bad post cannot cost
/// the account its entire scan (#221).
///
/// Scoring is batched per account for throughput. That made #220's damage
/// wildly disproportionate: a single unscoreable post failed the whole batch,
/// `gather_account` returned `Err`, and the account was dropped along with all
/// of its perfectly scoreable posts — turning a 4.3% post-level failure rate
/// into 100% account loss for 34 accounts.
///
/// Fast path is unchanged: one batch call. Only on failure does this fall back
/// to scoring each text individually, so a healthy scan pays nothing.
///
/// `None` marks a text that could not be scored even alone. It is deliberately
/// NOT a fallback number: `onnx_score` feeds an early-exit gate in
/// `scoring::profile` that reads `< ONNX_CLEAN_THRESHOLD` as clean (a low
/// sentinel would silently pass a post nobody scored) and it drives evidence
/// sorting (a high sentinel would put an unreadable post at the top of the
/// evidence list). Absence is the only honest encoding, and it leaves the
/// caller to decide what to do about it.
///
/// Never returns `Err`: propagating one here would reopen the exact
/// account-loss path this exists to close.
pub async fn clean_pass_isolated(
    clean_pass: &dyn CleanPassScorer,
    texts: &[String],
) -> Vec<Option<f64>> {
    if texts.is_empty() {
        return Vec::new();
    }

    match clean_pass.onnx_clean_pass(texts).await {
        Ok(scores) if scores.len() == texts.len() => scores.into_iter().map(Some).collect(),
        Ok(scores) => {
            // A length mismatch means we cannot trust the positional mapping,
            // and mis-attributing one post's score to another is worse than
            // failing: nothing would look wrong. Fall back to per-item, where
            // each score is unambiguously its own.
            warn!(
                returned = scores.len(),
                expected = texts.len(),
                "clean pass returned the wrong number of scores — retrying per post"
            );
            score_individually(clean_pass, texts).await
        }
        Err(e) => {
            warn!(
                posts = texts.len(),
                error = %format!("{e:#}"),
                "clean pass failed for the batch — retrying per post to isolate the bad one"
            );
            score_individually(clean_pass, texts).await
        }
    }
}

/// Score one text at a time so a single failure is contained to that text.
async fn score_individually(
    clean_pass: &dyn CleanPassScorer,
    texts: &[String],
) -> Vec<Option<f64>> {
    let mut out = Vec::with_capacity(texts.len());
    for text in texts {
        // Deliberately sequential: this path only runs after a batch already
        // failed, so it is rare, and the ONNX session is behind a global mutex
        // anyway — concurrency here would buy nothing but contention.
        let slice = std::slice::from_ref(text);
        match clean_pass.onnx_clean_pass(slice).await {
            Ok(scores) if scores.len() == 1 => out.push(Some(scores[0])),
            _ => out.push(None),
        }
    }
    out
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
    /// Sentence embedder for precomputing the target mean embedding here in
    /// Phase A (#213), where the ONNX work overlaps I/O instead of serializing
    /// in the Phase-C finalize loop. `None` disables the precompute (Phase C
    /// then embeds at score time or falls back to TF-IDF).
    pub embedder: Option<&'a SentenceEmbedder>,
    /// Whether the scan has a protected-user embedding. The precomputed target
    /// vector is only ever consumed by Phase C when a protected embedding also
    /// exists; without one, Phase C scores via TF-IDF regardless. So we skip
    /// the precompute entirely when this is false — otherwise Phase A would run
    /// pointless inference AND an embed failure would `?`-skip the account,
    /// where TF-IDF would have scored it fine.
    pub has_protected_embedding: bool,
}

/// Gather Phase A work for a single account.
///
/// On a terminal Stage-1 outcome, writes the terminal `AccountScore` and
/// returns [`GatherOutcome::Terminal`] — nothing is enqueued or stashed. On
/// `Proceed`, enqueues one `QueueRow` per Stage-2 post (clean rows pre-finalised
/// as `done`/non-toxic, survivors left `pending`) and stashes one `AccountInput`
/// blob, returning [`GatherOutcome::Enqueued`]; it does NOT write an
/// `AccountScore` (Phase C scores survivors).
pub async fn gather_account(
    db: &Arc<dyn Database>,
    user_did: &str,
    fetcher: &dyn PostFetcher,
    scorer: &dyn ToxicityScorer,
    clean_pass: &dyn CleanPassScorer,
    inputs: &GatherInputs<'_>,
) -> Result<GatherOutcome> {
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
            // Non-threat: finalise directly, enqueue/stash nothing. The score
            // WAS written here, so report Terminal so the orchestrator counts it.
            db.upsert_account_score(user_did, &score).await?;
            return Ok(GatherOutcome::Terminal);
        }
        Stage1Outcome::Proceed { .. } => {}
    }

    // ── Stage 2: full I/O with 50 posts (no classification — that's Phase B) ──
    let sample = fetcher.fetch_sample(inputs.account_handle, 50).await?;

    // #222: partition before building QueueRows so the burst never classifies
    // unassessable text; abstain if the assessable subset is too thin.
    let (sample, dropped) = crate::scoring::language::partition_assessable(&sample);
    if crate::scoring::language::coverage_gate(sample.total_posts, dropped)
        == crate::scoring::language::CoverageOutcome::NotAssessed
    {
        let score = crate::scoring::profile::not_assessed_score(
            inputs.account_did,
            inputs.account_handle,
            (sample.total_posts + dropped) as u32,
            inputs.graph_distance,
        );
        db.upsert_account_score(user_did, &score).await?;
        return Ok(GatherOutcome::Terminal);
    }

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
    //
    // Per-post isolated (#221): this used to be `.await?`, so ONE unscoreable
    // post failed the batch, propagated out of `gather_account`, and cost the
    // account its whole scan. Now a bad post is dropped and its neighbours
    // survive.
    let onnx_scores = clean_pass_isolated(clean_pass, &envelope_texts).await;
    debug_assert_eq!(onnx_scores.len(), rows.len());

    let total_posts = rows.len();
    let mut scored_rows = Vec::with_capacity(total_posts);
    for (mut row, onnx_score) in rows.into_iter().zip(onnx_scores) {
        // An unscoreable post is DROPPED rather than given a fallback score:
        // any sentinel would be wrong somewhere (see `clean_pass_isolated`).
        let Some(onnx_score) = onnx_score else {
            continue;
        };
        row.onnx_score = onnx_score;
        if onnx_score < ONNX_CLEAN_THRESHOLD {
            mark_clean(&mut row);
        }
        // else: leave as a pending survivor (verdict fields stay None).
        scored_rows.push(row);
    }
    let dropped = total_posts - scored_rows.len();
    let rows = scored_rows;

    if dropped > 0 {
        warn!(
            account_did = inputs.account_did,
            dropped,
            total_posts,
            "dropped unscoreable posts — the account is still scored on the rest"
        );
    }

    // Every post unscoreable means we have no toxicity signal at all for this
    // account. Scoring it anyway would emit a confident-looking result derived
    // from nothing, so fail here and let the caller record it as a skip — a
    // known gap is better than a fabricated score. This is the one case where
    // the account is still lost, and it is now a genuine absence of data rather
    // than one bad post poisoning the batch.
    if rows.is_empty() && total_posts > 0 {
        anyhow::bail!(
            "all {total_posts} posts were unscoreable by the ONNX clean pass — \
             no toxicity signal available for this account"
        );
    }

    // Precompute the target mean embedding HERE (#213), where it overlaps the
    // account's network I/O, instead of in the serial Phase-C finalize loop
    // where every account's embedding serializes on the global model mutex.
    // Uses the SAME `select_fingerprint_posts` selection Phase C would, so the
    // cosine computed downstream is byte-identical. Computed before `sample` is
    // moved into the blob below.
    // Gate on BOTH an embedder AND a protected embedding: Phase C consumes the
    // precomputed vector only when a protected embedding exists, and otherwise
    // scores via TF-IDF. Computing it without a protected embedding would be
    // wasted inference and — because of the `?` below — would turn an embed
    // failure into a skipped account that TF-IDF would have scored (#213).
    let target_embedding = match (inputs.embedder, inputs.has_protected_embedding) {
        (Some(emb), true) => {
            let fp_posts = select_fingerprint_posts(&sample);
            if fp_posts.is_empty() {
                None
            } else {
                Some(mean_embedding(&emb.embed_batch(&fp_posts).await?))
            }
        }
        _ => None,
    };

    // Batch the writes: one stash, one enqueue. Stash the AccountInput blob
    // BEFORE enqueuing the queue rows. If the stash fails we return early and
    // no `pending` queue rows exist — so Phase B can never classify orphaned
    // rows that have no Phase C blob to score against. (The reverse order would
    // leave orphaned pending rows on a stash failure.)
    let blob = AccountInput {
        schema_version: ACCOUNT_INPUT_SCHEMA_VERSION,
        account_handle: inputs.account_handle.to_string(),
        sample,
        parent_texts,
        median_engagement: inputs.median_engagement,
        is_pile_on: inputs.is_pile_on,
        direct_pairs: inputs.direct_pairs.map(|p| p.to_vec()),
        graph_distance: inputs.graph_distance.map(|d| d.as_str().to_string()),
        fingerprint_quality: fingerprint_quality(&blob_sample_counts(&rows)),
        target_embedding,
    };
    let payload = serde_json::to_string(&blob)?;
    db.stash_account_input(user_did, inputs.account_did, &payload)
        .await?;

    db.enqueue_classifications(user_did, &rows).await?;

    Ok(GatherOutcome::Enqueued)
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
