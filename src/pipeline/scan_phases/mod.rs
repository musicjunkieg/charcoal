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
use futures::FutureExt as _;
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
        embedder: deps.embedder,
        has_protected_embedding: deps.protected_embedding.is_some(),
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
        // Mark the phase at entry so GET /api/status can show "gathering"
        // while this runs. Safe for resume: a crash mid-gather previously left
        // a stale marker and fresh-started; a "gather" marker re-enters this
        // same block (staging is cleared either way).
        db.set_scan_state(user_did, "scan_phase", ScanPhase::Gather.as_str())
            .await?;
        db.clear_scan_staging(user_did).await?;
        // Terminal (early-exit / insufficient-data) accounts are scored inside
        // gather and never reach Phase C, so count them here.
        let gathered = run_gather(db, user_did, candidates, deps).await?;
        summary.accounts_scored += gathered.terminal_scored;
        // A skipped gather (per-account failure) means the scan is incomplete —
        // those accounts were never enqueued and will never be scored.
        summary.degraded |= gathered.skipped;
        db.set_scan_state(user_did, "scan_phase", ScanPhase::Burst.as_str())
            .await?;
        // Record the burst denominator while pending == everything enqueued,
        // so GET /api/status can report "X of Y classified" during the burst.
        // On a resume that skips gather, the prior run's value is still
        // correct: rows classified before the interruption are already done.
        let enqueued = db.count_pending_classifications(user_did).await?;
        db.set_scan_state(user_did, "classifications_total", &enqueued.to_string())
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
            BurstOutcome::Interrupted => {
                // A transient classifier failure (e.g. a RunPod blip with the
                // retry budget exhausted) stopped the burst. Same handling as a
                // cost cap: leave phase == "burst", report degraded, stop here.
                // A later resume re-enters Burst and retries the pending rows
                // once the backend recovers — far better than aborting the whole
                // scan (and losing finalize) over one transient network error.
                info!(
                    phase = "burst",
                    outcome = "interrupted",
                    "burst interrupted by transient classifier failure — scan resumable"
                );
                summary.degraded = true;
                return Ok(summary);
            }
            BurstOutcome::Complete { errored } => {
                if errored > 0 {
                    // Some posts failed to decode and were recorded as benign
                    // sentinels — the scan is incomplete/degraded.
                    summary.degraded = true;
                }
                info!(
                    phase = "burst",
                    outcome = "complete",
                    errored = errored,
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

/// Result of the Phase A gather sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct GatherSweep {
    /// Accounts that Stage 1 scored terminally (early-exit / insufficient-data).
    /// They never reach Phase C, so the orchestrator folds this into
    /// `accounts_scored`.
    terminal_scored: usize,
    /// True if at least one account's gather failed and was skipped — the scan
    /// is then incomplete and the caller should mark the summary `degraded`.
    skipped: bool,
}

/// Extract a human-readable message from a panic payload.
///
/// Rust panics carry a `Box<dyn Any + Send>`; the most common payloads are
/// `&'static str` (e.g. `unwrap()`) and `String` (e.g. `panic!("{}", …)`).
/// Any other payload type is reported as `"<non-string panic>"`.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic>".to_string()
    }
}

/// Phase A: gather every candidate concurrently (`buffer_unordered`).
///
/// Returns a [`GatherSweep`] with the terminal-scored count and whether any
/// account was skipped due to a gather failure.
async fn run_gather(
    db: &Arc<dyn Database>,
    user_did: &str,
    candidates: &[CandidateInput],
    deps: &PhasedScanDeps<'_>,
) -> Result<GatherSweep> {
    // Map over owned indices (not `&CandidateInput` iterator items) and re-index
    // inside the `async move`. Mapping the borrowing items directly trips the
    // compiler's `FnOnce is not general enough` HRTB inference when this future
    // is held across the web background-scan's `tokio::spawn` boundary; indexing
    // by `usize` keeps the closure free of a higher-ranked borrow.
    let results: Vec<(String, Result<GatherOutcome>)> = futures::stream::iter(0..candidates.len())
        .map(|i| {
            let did = candidates[i].account_did.clone();
            let did_for_panic = did.clone();
            async move {
                // Wrap in `catch_unwind` so a panic inside `gather_one` (e.g.
                // an `unwrap()` in atrium-api on a malformed API response) is
                // caught and turned into an `Err`, treated just like a transient
                // fetch error: logged, skipped, scan marked degraded. Without
                // this, a single bad response unwinds `buffer_unordered` and
                // kills the entire scan for ~1 200 candidates.
                //
                // `AssertUnwindSafe` is required because the future borrows
                // `db`, `candidates[i]`, and `deps` (none are `UnwindSafe`).
                // This is sound: we're only catching to log-and-skip, not
                // resuming or re-using any potentially-poisoned state.
                let outcome =
                    std::panic::AssertUnwindSafe(gather_one(db, user_did, &candidates[i], deps))
                        .catch_unwind()
                        .await;

                let result = match outcome {
                    Ok(inner) => inner,
                    Err(payload) => {
                        let msg = panic_message(&payload);
                        Err(anyhow::anyhow!(
                            "gather panicked for account {did_for_panic}: {msg}"
                        ))
                    }
                };
                (did, result)
            }
        })
        .buffer_unordered(deps.gather_concurrency.clamp(1, 64))
        .collect()
        .await;

    // Resilient gather: a single account's failure (e.g. a transient Bluesky
    // fetch error) must NOT abort the whole scan — and on resume must not
    // re-fail the batch (livelock risk). Log per-account failures and continue
    // with the accounts that gathered successfully.
    let mut sweep = GatherSweep::default();
    for (account_did, result) in results {
        match result {
            Ok(GatherOutcome::Terminal) => sweep.terminal_scored += 1,
            Ok(GatherOutcome::Enqueued) => {}
            Err(e) => {
                sweep.skipped = true;
                // `{e:#}` (alternate Display) walks the anyhow source chain; plain
                // `%e` prints only the outermost .context() and drops the cause.
                // That difference cost hours on #220: every one of these read
                // "ONNX inference failed" with the actual ort error invisible.
                warn!(
                    account_did,
                    error = %format!("{e:#}"),
                    "gather failed for account — skipping it and continuing the scan"
                );
            }
        }
    }
    Ok(sweep)
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
                // Recovery runs in its own fallible helper so a single
                // deleted/suspended account's re-gather/re-burst/re-finalize
                // error is isolated: it `warn!`-logs + marks the scan degraded
                // and the loop continues, instead of `?`-aborting every
                // remaining account's finalize.
                if !recover_account(db, user_did, &account_did, candidates, deps, summary).await {
                    // Account was not recovered (skipped, errored, or still
                    // incomplete) — the scan is incomplete.
                    summary.degraded = true;
                }
            }
        }
    }
    Ok(())
}

/// Attempt to recover one `NeedsRegather` account with a single bounded
/// re-gather + re-burst + re-finalize pass.
///
/// Returns `true` only when the account was successfully scored (terminal
/// re-gather or a re-finalize that reached `Scored`). Any other path — a
/// missing candidate, a re-gather/re-burst/re-finalize *error*, a cost-cap, or
/// a still-incomplete re-finalize — returns `false` so the caller marks the
/// scan degraded. Errors are caught here (not propagated) so one bad account
/// never aborts the whole finalize loop.
async fn recover_account(
    db: &Arc<dyn Database>,
    user_did: &str,
    account_did: &str,
    candidates: &[CandidateInput],
    deps: &PhasedScanDeps<'_>,
    summary: &mut ScanSummary,
) -> bool {
    let Some(candidate) = candidates.iter().find(|c| c.account_did == account_did) else {
        warn!(
            account_did,
            "finalize needs re-gather but no matching candidate — skipping"
        );
        return false;
    };

    // Wrap the fallible recovery steps so a per-account error (deleted/suspended
    // account, transient fetch failure) is contained rather than aborting the
    // whole finalize loop via `?`.
    match recover_account_inner(db, user_did, account_did, candidate, deps).await {
        Ok(true) => {
            summary.accounts_scored += 1;
            summary.regathered += 1;
            true
        }
        Ok(false) => {
            // Recovery completed without error but did not score the account
            // (cost-cap, or still incomplete after one pass). Already warned
            // inside the helper.
            false
        }
        Err(e) => {
            warn!(
                account_did,
                error = %format!("{e:#}"),
                "re-gather recovery failed for account — skipping it and continuing the scan"
            );
            false
        }
    }
}

/// The fallible body of [`recover_account`]: clear stale staging, re-gather,
/// re-burst, re-finalize. Returns `Ok(true)` when the account was scored,
/// `Ok(false)` when a cost-cap or still-incomplete verdict stops recovery, and
/// `Err` on any DB/fetch failure (caught by the caller, never propagated to the
/// finalize loop).
async fn recover_account_inner(
    db: &Arc<dyn Database>,
    user_did: &str,
    account_did: &str,
    candidate: &CandidateInput,
    deps: &PhasedScanDeps<'_>,
) -> Result<bool> {
    // Clear any stale staging from the prior attempt before re-gathering.
    // finalize's incomplete-verdict path intentionally leaves staging in place,
    // so the prior queue rows / blob still exist; if we re-gather without
    // clearing, those stale `pending` rows would be burst-classified and the
    // stale blob could be read by the follow-up finalize. Clear first so the
    // re-gather starts from a clean per-account slate.
    db.clear_account_staging(user_did, account_did).await?;

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
        // Re-gather hit a terminal Stage-1 outcome: the score was written by
        // gather itself, nothing was enqueued. Done — no re-burst / re-finalize.
        return Ok(true);
    }

    // Drain the account's freshly-enqueued pending rows. A cost-cap here just
    // means the retry could not complete — stop and report not-scored.
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
        return Ok(false);
    }

    match finalize_one(db, user_did, account_did, deps).await? {
        FinalizeOutcome::Scored => Ok(true),
        FinalizeOutcome::NeedsRegather => {
            // Still incomplete after one recovery pass — give up on this account
            // (bounded) rather than loop forever.
            warn!(
                account_did,
                "account still needs re-gather after one recovery pass — skipping"
            );
            Ok(false)
        }
    }
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
