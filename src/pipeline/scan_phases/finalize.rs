// Phase C — `finalize_account`: the scoring phase of the three-phase scan.
//
// For a single account this:
//   1. Reads the stashed `AccountInput` blob (Phase A) and the per-post
//      classifier verdicts (Phase B, recorded onto the queue rows).
//   2. Re-aligns the arbitrary-order verdict rows back to the blob's sample
//      ordering (originals ++ replies ++ quotes), the order
//      `score_from_sample` slices by.
//   3. Scores the account via `score_from_sample`, applying the adaptive
//      two-pass NLI context gate (Step 4 below) so amplifier/follower/sweep
//      sources keep their pre-decouple NLI behavior exactly, then writes the
//      final `AccountScore`.
//
// Two failure modes return `FinalizeOutcome::NeedsRegather` instead of
// scoring:
//   - The blob is missing, unreadable, or version-stale (a deploy straddled
//     the scan and the on-disk blob predates the current schema). The stale
//     account's staging rows are cleared so Phase A can re-gather it cleanly.
//   - The verdict rows are incomplete/inconsistent with the blob (a sample
//     post has no matching row, or a row is still `pending`). We never score a
//     partial account — the orchestrator re-gathers it.
//
// The classifier is never invoked here — verdicts already exist on the queue
// rows. This phase is pure scoring + DB I/O.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tracing::warn;

use crate::bluesky::relationships::GraphDistance;
use crate::db::Database;
use crate::scoring::nli::NliScorer;
use crate::scoring::profile::score_from_sample;
use crate::scoring::threat::ThreatWeights;
use crate::topics::embeddings::SentenceEmbedder;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::traits::{BinaryVerdict, ToxicityAttributes};

use super::staging::{AccountInput, ACCOUNT_INPUT_SCHEMA_VERSION};

/// Outcome of finalising one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeOutcome {
    /// The account was scored and its `AccountScore` written.
    Scored,
    /// The account could not be scored from staged data and must be
    /// re-gathered by Phase A. Either nothing was staged, the blob was
    /// unreadable/version-stale, or the verdicts were incomplete.
    NeedsRegather,
}

/// Finalise (score) a single account from its staged Phase A blob + Phase B
/// verdicts.
///
/// The protected-context params (`protected_fingerprint`, `weights`,
/// `embedder`, `protected_embedding`, `nli_scorer`,
/// `protected_posts_with_embeddings`, `data_dir`) are runtime values supplied
/// by the orchestrator (Chunk 6) — they are NOT carried in the blob, which only
/// holds per-account data. Their names/types mirror `score_from_sample`.
#[allow(clippy::too_many_arguments)]
pub async fn finalize_account(
    db: &Arc<dyn Database>,
    user_did: &str,
    account_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    nli_scorer: Option<&NliScorer>,
    protected_posts_with_embeddings: Option<&[(String, Vec<f64>)]>,
    data_dir: Option<&std::path::Path>,
) -> Result<FinalizeOutcome> {
    // ── Step 1: load + validate the stashed blob ──
    let Some(payload) = db.fetch_account_input(user_did, account_did).await? else {
        // Nothing staged for this account — orchestrator must re-gather.
        return Ok(FinalizeOutcome::NeedsRegather);
    };

    let blob: AccountInput = match serde_json::from_str(&payload) {
        Ok(blob) => blob,
        Err(e) => {
            // Unreadable blob (e.g. a pre-deploy schema shape). Discard this
            // account's staging so Phase A re-gathers it from scratch.
            warn!(
                account_did,
                error = %e,
                "AccountInput blob failed to deserialize — clearing staging and re-gathering"
            );
            db.clear_account_staging(user_did, account_did).await?;
            return Ok(FinalizeOutcome::NeedsRegather);
        }
    };

    if blob.schema_version != ACCOUNT_INPUT_SCHEMA_VERSION {
        warn!(
            account_did,
            blob_version = blob.schema_version,
            expected_version = ACCOUNT_INPUT_SCHEMA_VERSION,
            "AccountInput schema_version mismatch — clearing staging and re-gathering"
        );
        db.clear_account_staging(user_did, account_did).await?;
        return Ok(FinalizeOutcome::NeedsRegather);
    }

    // ── Step 2: fetch verdict rows (arbitrary order) ──
    let rows = db.fetch_account_verdicts(user_did, account_did).await?;

    // Index rows by post URI for order-independent lookup.
    let row_by_uri: HashMap<&str, &super::staging::QueueRow> =
        rows.iter().map(|r| (r.post_uri.as_str(), r)).collect();

    // ── Step 3: rebuild ordered verdicts + texts + contexts in the SAME order
    //            `score_from_sample` slices by: originals ++ replies ++ quotes ──
    let sample = &blob.sample;
    let total = sample.total_posts;

    let mut verdicts: Vec<BinaryVerdict> = Vec::with_capacity(total);
    let mut all_post_texts: Vec<String> = Vec::with_capacity(total);
    let mut contexts: Vec<Option<String>> = Vec::with_capacity(total);

    // Originals — raw text, no context.
    for p in &sample.originals {
        let Some(verdict) = verdict_for(&row_by_uri, &p.uri) else {
            return needs_regather_incomplete(account_did, &p.uri);
        };
        verdicts.push(verdict);
        all_post_texts.push(p.text.clone());
        contexts.push(None);
    }
    // Replies — reply text, context = stashed parent text (by parent_uri).
    for r in &sample.replies {
        let Some(verdict) = verdict_for(&row_by_uri, &r.post.uri) else {
            return needs_regather_incomplete(account_did, &r.post.uri);
        };
        verdicts.push(verdict);
        all_post_texts.push(r.post.text.clone());
        contexts.push(blob.parent_texts.get(&r.parent_uri).cloned());
    }
    // Quotes — raw text, no context.
    for p in &sample.quotes {
        let Some(verdict) = verdict_for(&row_by_uri, &p.uri) else {
            return needs_regather_incomplete(account_did, &p.uri);
        };
        verdicts.push(verdict);
        all_post_texts.push(p.text.clone());
        contexts.push(None);
    }

    // ── Step 4: score (carrying the adaptive two-pass NLI gate) ──
    //
    // The pre-decouple amplification/sweep code chose whether to run the
    // (local ONNX) NLI context pass differently per source. `score_from_sample`
    // runs NLI whenever `nli_scorer` is `Some` — it has NO raw>=8.0 gate of its
    // own (that gate lived in amplification's manual two-pass). So we reproduce
    // the original gate HERE, calling `score_from_sample` as the pure scoring
    // core. It does not fetch or classify, so a discarded extra pass is cheap.
    //
    // Three cases (matching the old amplification.rs structure exactly):
    //   - Sweep (`nli_scorer = None`): never runs NLI. Single pass.
    //   - Amplifier (`direct_pairs = Some`): ALWAYS runs NLI on its direct
    //     interaction pairs — no >=8.0 gate. Single pass with the scorer.
    //   - Follower (`direct_pairs = None`, scorer present): TWO-PASS. Pass 1
    //     without NLI; only if pass-1 `threat_score >= 8.0` (the Watch-tier
    //     boundary) AND inferred pairs (`protected_posts_with_embeddings`) are
    //     present do we run pass 2 WITH NLI. Otherwise keep pass 1.
    let graph_distance = blob
        .graph_distance
        .as_deref()
        .and_then(GraphDistance::from_str);

    // Watch-tier boundary, the raw-score gate above which the follower NLI pass
    // is worth running. Mirrors `TIER_BOUNDARIES[0]` in `scoring/profile.rs`
    // (private there) and the spec's "if >= 8.0, runs the local NLI pass"
    // (docs/superpowers/specs/2026-06-23-classification-burst-decouple-design.md
    // lines 151-168). Match the old code's `score.threat_score.unwrap_or(0.0)
    // >= 8.0` comparison exactly.
    const WATCH_THRESHOLD: f64 = 8.0;

    // All NON-NLI args are identical across passes — ONLY the four NLI-related
    // args vary (`nli_scorer`, `protected_posts_with_embeddings`, `direct_pairs`,
    // `data_dir`). A local macro keeps the long arg list in one place without a
    // closure (a closure returning a borrowing future trips the borrow checker).
    macro_rules! run_pass {
        ($nli:expr, $ppwe:expr, $pairs:expr, $ddir:expr) => {
            score_from_sample(
                sample,
                &all_post_texts,
                &contexts,
                &verdicts,
                protected_fingerprint,
                weights,
                embedder,
                protected_embedding,
                // Precomputed in Phase A (gather); when present the overlap step
                // skips the (mutex-serialized) embedding entirely (#213).
                blob.target_embedding.as_deref(),
                blob.median_engagement,
                blob.is_pile_on,
                $nli,
                $ppwe,
                $pairs,
                $ddir,
                graph_distance,
                &blob.account_handle,
                account_did,
                None, // stage1_overlap: ignored by score_from_sample (parity only)
            )
            .await?
        };
    }

    let score = if nli_scorer.is_none() {
        // Sweep: no NLI at all.
        run_pass!(None, None, None, None)
    } else if blob.direct_pairs.is_some() {
        // Amplifier: always NLI on the direct pairs (Mode A). `score_from_sample`
        // uses `direct_pairs` and ignores `protected_posts_with_embeddings` in
        // that mode, so pass `None` for ppwe. NO >=8.0 gate.
        run_pass!(nli_scorer, None, blob.direct_pairs.as_deref(), data_dir)
    } else {
        // Follower: two-pass adaptive gate.
        // Pass 1 — no NLI, no audit logging (data_dir = None).
        let pass1 = run_pass!(None, None, None, None);
        if pass1.threat_score.unwrap_or(0.0) >= WATCH_THRESHOLD
            && protected_posts_with_embeddings.is_some()
        {
            // Pass 2 — NLI over inferred pairs, WITH audit logging (data_dir).
            run_pass!(nli_scorer, protected_posts_with_embeddings, None, data_dir)
        } else {
            pass1
        }
    };

    // ── Step 5: persist + done ──
    db.upsert_account_score(user_did, &score).await?;
    Ok(FinalizeOutcome::Scored)
}

/// Look up a post's verdict row by URI and convert it to a `BinaryVerdict`.
///
/// Returns `None` when the post has no matching row OR the row is not yet
/// complete. The queue `status` is the source of truth: a row whose
/// `status != "done"` is treated as incomplete (fail closed), regardless of
/// whether `toxic_token` happens to be populated. Only after the status gate
/// passes do we read the verdict (a `done` row without a `toxic_token` is also
/// inconsistent and yields `None`). `score_from_sample` only reads `is_toxic` +
/// `onnx_score`; `onnx_attributes` is unused, so `default()` is correct.
fn verdict_for(
    row_by_uri: &HashMap<&str, &super::staging::QueueRow>,
    post_uri: &str,
) -> Option<BinaryVerdict> {
    let row = row_by_uri.get(post_uri)?;
    // Status is authoritative: a still-`pending` row is incomplete even if a
    // verdict token somehow leaked onto it. Fail closed → NeedsRegather.
    if row.status != "done" {
        return None;
    }
    let is_toxic = row.toxic_token?; // None on a done row ⇒ inconsistent
    Some(BinaryVerdict {
        is_toxic,
        onnx_score: row.onnx_score,
        onnx_attributes: ToxicityAttributes::default(),
    })
}

/// Build the `NeedsRegather` result for an incomplete/inconsistent account,
/// logging which post triggered it. We intentionally do NOT clear staging here
/// — the verdicts may simply be mid-burst, and the orchestrator decides whether
/// to re-run Phase B or re-gather.
fn needs_regather_incomplete(account_did: &str, post_uri: &str) -> Result<FinalizeOutcome> {
    warn!(
        account_did,
        post_uri, "account has a missing/pending verdict — needs re-gather, not scoring"
    );
    Ok(FinalizeOutcome::NeedsRegather)
}
