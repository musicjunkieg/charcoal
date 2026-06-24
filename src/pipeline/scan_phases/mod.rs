// Scan-phase staging types — the three-phase scan pipeline work queue.
//
// Phase A (Gather): collect posts, compute behavioral signals, enqueue classifier work.
// Phase B (Burst): classifier verdict for each queued post.
// Phase C (Score): read back staged data and compute final AccountScore.

pub mod burst;
pub mod finalize;
pub mod gather;
pub mod staging;

// ── run_phased_scan: the orchestrating state machine ───────────────────────────
//
// `run_phased_scan` drives a single scan run through Gather → Burst → Finalize →
// Done. It is the keystone of the decoupled pipeline (#208): it owns the
// `scan_phase` marker in `scan_state`, sequences the three phase functions, and
// implements crash/cost-cap resume.
//
// Resume semantics: every call re-reads `scan_phase`. A fresh run (`None` or
// `"done"`) starts at Gather; a run interrupted mid-burst (`"burst"`) re-enters
// at Burst and SKIPS gather entirely; a run interrupted in finalize
// (`"finalize"`) re-enters at Finalize. The single-scan-per-user invariant
// (enforced upstream by ScanJobManager) makes the fresh-start staging wipe safe.

use std::path::Path;
use std::sync::Arc;

use anyhow::{bail, Result};
use futures::StreamExt;
use tracing::{info, warn};

use crate::bluesky::relationships::GraphDistance;
use crate::db::Database;
use crate::scoring::nli::NliScorer;
use crate::scoring::threat::ThreatWeights;
use crate::topics::embeddings::SentenceEmbedder;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::classifier::ToxicityClassifier;
use crate::toxicity::traits::ToxicityScorer;

use burst::{run_burst, BurstOutcome};
use finalize::{finalize_account, FinalizeOutcome};
use gather::{gather_account, CleanPassScorer, GatherInputs, GatherOutcome, PostFetcher};
use staging::ScanPhase;

/// One candidate account to scan, with the per-account inputs the orchestrator
/// owns (the scan-global shared refs live in [`PhasedScanDeps`]).
///
/// This is the OWNED counterpart to [`GatherInputs`] — the orchestrator holds a
/// `&[CandidateInput]` for the whole run and rebuilds a borrowing `GatherInputs`
/// per account by pairing a candidate with the shared deps.
#[derive(Debug, Clone)]
pub struct CandidateInput {
    /// DID of the account to gather + score.
    pub account_did: String,
    /// Handle used for fetching and Stage-1 logging.
    pub account_handle: String,
    /// Whether this account is in the precomputed pile-on DID set.
    pub is_pile_on: bool,
    /// Direct (amplifier) text pairs for NLI context scoring, if any.
    pub direct_pairs: Option<Vec<(String, String)>>,
    /// Social graph distance from the protected user, if classified.
    pub graph_distance: Option<GraphDistance>,
}

/// Scan-global shared references threaded into every phase call.
///
/// Held by borrow for the whole run; the orchestrator combines these with each
/// [`CandidateInput`] to build a per-account [`GatherInputs`]. Borrowed (not
/// owned) so the heavyweight models (embedder, NLI scorer) are shared, not
/// cloned.
pub struct PhasedScanDeps<'a> {
    /// Post-fetch seam (Phase A I/O).
    pub fetcher: &'a dyn PostFetcher,
    /// Stage-1 continuous toxicity scorer.
    pub scorer: &'a dyn ToxicityScorer,
    /// Phase A ONNX clean-pass.
    pub clean_pass: &'a dyn CleanPassScorer,
    /// Phase B classifier (RunPod/Zentropi, already cost-metered).
    pub classifier: &'a Arc<dyn ToxicityClassifier>,
    /// Protected user's topic fingerprint.
    pub protected_fingerprint: &'a TopicFingerprint,
    /// Scoring weights.
    pub weights: &'a ThreatWeights,
    /// Optional sentence embedder for semantic overlap (Phase C).
    pub embedder: Option<&'a SentenceEmbedder>,
    /// Optional protected-user embedding (Phase C).
    pub protected_embedding: Option<&'a [f64]>,
    /// Optional NLI scorer for context gating (Phase C).
    pub nli_scorer: Option<&'a NliScorer>,
    /// Optional protected posts with embeddings for follower NLI (Phase C).
    pub protected_posts_with_embeddings: Option<&'a [(String, Vec<f64>)]>,
    /// Optional data dir for NLI audit logging (Phase C).
    pub data_dir: Option<&'a Path>,
    /// Median engagement across the scan run (behavioral normalisation).
    pub median_engagement: f64,
    /// Concurrency for Phase A gather (`buffer_unordered` width).
    pub gather_concurrency: usize,
    /// Concurrency for Phase B burst (`run_burst` width).
    pub burst_concurrency: usize,
    /// Batch size for Phase B burst (`run_burst` fetch limit).
    pub burst_batch: i64,
}

/// Summary of a completed (or cost-capped) `run_phased_scan` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScanSummary {
    /// Number of accounts that reached `FinalizeOutcome::Scored` in this call.
    pub accounts_scored: usize,
    /// Number of accounts that were re-gathered after a `NeedsRegather`.
    pub regathered: usize,
    /// True when a `CostCapped` burst left the scan incomplete/resumable.
    pub degraded: bool,
}

/// Build a per-account [`GatherInputs`] by pairing a candidate with shared deps.
fn gather_inputs<'a>(
    candidate: &'a CandidateInput,
    deps: &'a PhasedScanDeps<'a>,
) -> GatherInputs<'a> {
    GatherInputs {
        account_did: &candidate.account_did,
        account_handle: &candidate.account_handle,
        protected_fingerprint: deps.protected_fingerprint,
        weights: deps.weights,
        median_engagement: deps.median_engagement,
        is_pile_on: candidate.is_pile_on,
        direct_pairs: candidate.direct_pairs.as_deref(),
        graph_distance: candidate.graph_distance,
    }
}

/// Run one phased scan to completion (or to a resumable cost-cap).
///
/// Reads the current `scan_phase` marker and dispatches into the state machine.
/// A fresh run flows Gather → Burst → Finalize → Done in a single call;
/// a resume jumps straight to the recorded phase. On a `CostCapped` burst, the
/// call returns early with `degraded: true` and leaves the phase at `"burst"`
/// so a later call drains the remaining pending rows.
pub async fn run_phased_scan(
    db: &Arc<dyn Database>,
    user_did: &str,
    candidates: &[CandidateInput],
    deps: &PhasedScanDeps<'_>,
) -> Result<ScanSummary> {
    // Read the resume point. Distinguish three cases:
    //   - missing marker (`None`)            ⇒ fresh start (Gather).
    //   - recognised value (`Some(phase)`)   ⇒ resume at that phase.
    //   - UNRECOGNISED value                 ⇒ fail closed (do NOT wipe staging).
    //
    // A typo/corruption/future-schema value must NOT be silently treated like a
    // missing marker: that path runs `clear_scan_staging`, which would destroy
    // resumable work. Bail instead so a human can investigate.
    let raw_phase = db.get_scan_state(user_did, "scan_phase").await?;
    let phase = match &raw_phase {
        None => None,
        Some(value) => match ScanPhase::from_value(value) {
            Some(p) => Some(p),
            None => bail!(
                "unknown scan_phase marker {value:?} for user {user_did} — refusing to \
                 fresh-start (would wipe resumable staging); investigate and reset the marker"
            ),
        },
    };

    let mut summary = ScanSummary::default();

    // ── Phase: Gather (also the fresh-start entry) ──
    // A fresh run (None or Done) wipes any stale staging from a prior run as a
    // backstop, then gathers. Resume entries (Burst/Finalize) skip gather.
    if phase.is_none() || phase == Some(ScanPhase::Done) || phase == Some(ScanPhase::Gather) {
        info!(
            phase = "gather",
            candidates = candidates.len(),
            "entering gather phase"
        );
        db.clear_scan_staging(user_did).await?;
        // Terminal (early-exit / insufficient-data) accounts are scored inside
        // gather and never reach Phase C, so count them here.
        summary.accounts_scored += run_gather(db, user_did, candidates, deps).await?;
        db.set_scan_state(user_did, "scan_phase", ScanPhase::Burst.as_str())
            .await?;
    }

    // ── Phase: Burst (also the resume entry for phase == "burst") ──
    if matches!(
        phase,
        None | Some(ScanPhase::Done) | Some(ScanPhase::Gather) | Some(ScanPhase::Burst)
    ) {
        if phase == Some(ScanPhase::Burst) {
            info!(resumed_phase = "burst", "resuming interrupted burst phase");
        }
        let pending = db.count_pending_classifications(user_did).await?;
        info!(phase = "burst", pending = pending, "entering burst phase");
        match run_burst(
            db,
            user_did,
            deps.classifier,
            deps.burst_concurrency,
            deps.burst_batch,
        )
        .await?
        {
            BurstOutcome::CostCapped => {
                // Resumable: leave phase == "burst", report degraded, stop here.
                // A later call re-enters Burst and drains the remaining pending.
                info!(
                    phase = "burst",
                    outcome = "cost_capped",
                    "burst cost-capped — scan resumable"
                );
                summary.degraded = true;
                return Ok(summary);
            }
            BurstOutcome::Complete => {
                info!(
                    phase = "burst",
                    outcome = "complete",
                    "burst phase complete"
                );
                db.set_scan_state(user_did, "scan_phase", ScanPhase::Finalize.as_str())
                    .await?;
            }
        }
    }

    // ── Phase: Finalize (also the resume entry for phase == "finalize") ──
    if phase == Some(ScanPhase::Finalize) {
        info!(
            resumed_phase = "finalize",
            "resuming interrupted finalize phase"
        );
    }
    let finalize_account_count = db.list_scan_accounts(user_did).await?.len();
    info!(
        phase = "finalize",
        accounts = finalize_account_count,
        "entering finalize phase"
    );
    run_finalize(db, user_did, candidates, deps, &mut summary).await?;
    db.set_scan_state(user_did, "scan_phase", ScanPhase::Done.as_str())
        .await?;

    // ── Phase: Done — clear both staging tables (leaves scan_phase intact) ──
    db.clear_scan_staging(user_did).await?;

    info!(
        phase = "done",
        accounts_scored = summary.accounts_scored,
        degraded = summary.degraded,
        "phased scan complete"
    );

    Ok(summary)
}

/// Phase A: gather every candidate concurrently (`buffer_unordered`).
///
/// Returns the number of accounts that Stage 1 scored terminally (early-exit /
/// insufficient-data). Those accounts never reach Phase C, so the orchestrator
/// folds this count into `accounts_scored`.
async fn run_gather(
    db: &Arc<dyn Database>,
    user_did: &str,
    candidates: &[CandidateInput],
    deps: &PhasedScanDeps<'_>,
) -> Result<usize> {
    // Map over owned indices (not `&CandidateInput` iterator items) and re-index
    // inside the `async move`. Mapping the borrowing items directly trips the
    // compiler's `FnOnce is not general enough` HRTB inference when this future
    // is held across the web background-scan's `tokio::spawn` boundary; indexing
    // by `usize` keeps the closure free of a higher-ranked borrow.
    let results: Vec<(String, Result<GatherOutcome>)> = futures::stream::iter(0..candidates.len())
        .map(|i| {
            let did = candidates[i].account_did.clone();
            async move { (did, gather_one(db, user_did, &candidates[i], deps).await) }
        })
        .buffer_unordered(deps.gather_concurrency.max(1))
        .collect()
        .await;

    // Resilient gather: a single account's failure (e.g. a transient Bluesky
    // fetch error) must NOT abort the whole scan — and on resume must not
    // re-fail the batch (livelock risk). Log per-account failures and continue
    // with the accounts that gathered successfully.
    let mut terminal_scored = 0;
    for (account_did, result) in results {
        match result {
            Ok(GatherOutcome::Terminal) => terminal_scored += 1,
            Ok(GatherOutcome::Enqueued) => {}
            Err(e) => {
                warn!(
                    account_did,
                    error = %e,
                    "gather failed for account — skipping it and continuing the scan"
                );
            }
        }
    }
    Ok(terminal_scored)
}

/// Gather a single candidate. Extracted into a named async fn (rather than an
/// inline closure) so the future produced by `buffer_unordered` has a clean
/// higher-ranked lifetime signature — an inline `|candidate| async move {…}`
/// borrowing closure trips the compiler's `FnOnce is not general enough`
/// inference when the whole scan future is held across a `tokio::spawn`
/// boundary (the web background-scan path).
async fn gather_one(
    db: &Arc<dyn Database>,
    user_did: &str,
    candidate: &CandidateInput,
    deps: &PhasedScanDeps<'_>,
) -> Result<GatherOutcome> {
    let inputs = gather_inputs(candidate, deps);
    gather_account(
        db,
        user_did,
        deps.fetcher,
        deps.scorer,
        deps.clean_pass,
        &inputs,
    )
    .await
}

/// Phase C: finalize every staged account, handling `NeedsRegather` with a
/// single bounded re-gather + re-burst + re-finalize attempt per account.
async fn run_finalize(
    db: &Arc<dyn Database>,
    user_did: &str,
    candidates: &[CandidateInput],
    deps: &PhasedScanDeps<'_>,
    summary: &mut ScanSummary,
) -> Result<()> {
    for account_did in db.list_scan_accounts(user_did).await? {
        match finalize_one(db, user_did, &account_did, deps).await? {
            FinalizeOutcome::Scored => {
                summary.accounts_scored += 1;
            }
            FinalizeOutcome::NeedsRegather => {
                // Recover this account once: re-gather (fresh blob + rows),
                // re-burst (drain its freshly-enqueued pending), re-finalize.
                // Bounded to a single retry — never loop.
                let Some(candidate) = candidates.iter().find(|c| c.account_did == account_did)
                else {
                    warn!(
                        account_did,
                        "finalize needs re-gather but no matching candidate — skipping"
                    );
                    continue;
                };

                // Clear any stale staging from the prior attempt before
                // re-gathering. finalize's incomplete-verdict path intentionally
                // leaves staging in place, so the prior queue rows / blob still
                // exist; if we re-gather without clearing, those stale `pending`
                // rows would be burst-classified and the stale blob could be read
                // by the follow-up finalize. Clear first so the re-gather starts
                // from a clean per-account slate.
                db.clear_account_staging(user_did, &account_did).await?;

                let inputs = gather_inputs(candidate, deps);
                if matches!(
                    gather_account(
                        db,
                        user_did,
                        deps.fetcher,
                        deps.scorer,
                        deps.clean_pass,
                        &inputs,
                    )
                    .await?,
                    GatherOutcome::Terminal
                ) {
                    // Re-gather hit a terminal Stage-1 outcome: the score was
                    // written by gather itself, nothing was enqueued. Count it
                    // and move on — no re-burst / re-finalize needed.
                    summary.accounts_scored += 1;
                    summary.regathered += 1;
                    continue;
                }

                // Drain the account's freshly-enqueued pending rows. A cost-cap
                // here just means the retry could not complete — skip it.
                if matches!(
                    run_burst(
                        db,
                        user_did,
                        deps.classifier,
                        deps.burst_concurrency,
                        deps.burst_batch,
                    )
                    .await?,
                    BurstOutcome::CostCapped
                ) {
                    warn!(
                        account_did,
                        "re-burst hit the cost ceiling during re-gather — skipping account"
                    );
                    // The scan is now incomplete (this account was not scored
                    // because of the cost cap) — flag it so the caller knows.
                    summary.degraded = true;
                    continue;
                }

                match finalize_one(db, user_did, &account_did, deps).await? {
                    FinalizeOutcome::Scored => {
                        summary.accounts_scored += 1;
                        summary.regathered += 1;
                    }
                    FinalizeOutcome::NeedsRegather => {
                        // Still incomplete after one recovery pass — give up on
                        // this account (bounded) rather than loop forever.
                        warn!(
                            account_did,
                            "account still needs re-gather after one recovery pass — skipping"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

/// Thin wrapper that forwards the shared Phase C deps to `finalize_account`.
async fn finalize_one(
    db: &Arc<dyn Database>,
    user_did: &str,
    account_did: &str,
    deps: &PhasedScanDeps<'_>,
) -> Result<FinalizeOutcome> {
    finalize_account(
        db,
        user_did,
        account_did,
        deps.protected_fingerprint,
        deps.weights,
        deps.embedder,
        deps.protected_embedding,
        deps.nli_scorer,
        deps.protected_posts_with_embeddings,
        deps.data_dir,
    )
    .await
}
