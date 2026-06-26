// Phase B — `run_burst`: the contiguous classifier window.
//
// This is the ONLY phase that calls the RunPod/Zentropi classifier. All
// classifier cost is concentrated here so the #206 ScanCostMeter is honest
// with zero meter changes needed outside this file.
//
// Phase A (Gather) already enqueued every pending QueueRow. Phase B drains
// them all in one loop. Phase C (Finalize) will read the recorded VerdictRows
// and compute final AccountScores.
//
// Loop invariant:
//   - Each iteration fetches up to `burst_batch` pending rows.
//   - All rows are classified concurrently (up to `burst_concurrency` in flight).
//   - Successful VerdictRows are recorded in a single batched DB write.
//   - If a CostCeilingExceeded error arrives, successful rows from that batch
//     are persisted, and the loop returns BurstOutcome::CostCapped.  Remaining
//     pending rows stay pending so a future resume can finish them.
//   - Any other classifier error records successes then propagates via Err.

use std::sync::Arc;

use anyhow::Result;
use futures::StreamExt;
use tracing::warn;

use crate::db::Database;
use crate::pipeline::scan_phases::staging::VerdictRow;
use crate::toxicity::classifier::{is_toxic, ClassifierTransientError, ToxicityClassifier};
use crate::toxicity::cost_meter::CostCeilingExceeded;

// ── BurstOutcome ──────────────────────────────────────────────────────────────

/// The result of a completed `run_burst` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstOutcome {
    /// All pending rows were classified successfully.
    Complete,
    /// The #206 scan cost ceiling fired mid-run. Rows classified before the cap
    /// are recorded as `done`; remaining `pending` rows can be resumed later.
    CostCapped,
    /// A *transient* classifier failure (e.g. a RunPod serverless blip with the
    /// retry budget exhausted) stopped the burst. Like `CostCapped`: successes
    /// are recorded `done`, the rest stay `pending` for a resume. Distinct from
    /// `Err` — a transient failure is expected to clear on retry, so the scan
    /// finishes degraded-but-resumable instead of hard-aborting before finalize.
    Interrupted,
}

// ── run_burst ─────────────────────────────────────────────────────────────────

/// Run Phase B: drain all `pending` classification_queue rows through the
/// classifier, record verdicts (flipping rows to `done`), and stop cleanly if
/// the #206 cost ceiling fires.
///
/// # Arguments
/// - `db` — shared database handle (Arc<dyn Database>)
/// - `user_did` — scopes all DB operations to this user
/// - `classifier` — the production classifier (already wired with
///   ScanCostMeter by `build_from_env()`); run_burst adds no meter logic of its own
/// - `burst_concurrency` — max concurrent classify calls per batch iteration
/// - `burst_batch` — rows fetched per loop iteration (limit passed to
///   `fetch_pending_classifications`)
pub async fn run_burst(
    db: &Arc<dyn Database>,
    user_did: &str,
    classifier: &Arc<dyn ToxicityClassifier>,
    burst_concurrency: usize,
    burst_batch: i64,
) -> Result<BurstOutcome> {
    // Clamp defensively, mirroring the `burst_concurrency()` / `burst_batch()`
    // env helpers below. A direct caller passing 0 (a zero-width
    // `buffer_unordered` deadlocks; a zero `LIMIT` fetches nothing → infinite
    // loop) or a huge value can't break the loop.
    let burst_concurrency = burst_concurrency.clamp(1, 64);
    let burst_batch = burst_batch.clamp(1, 10_000);

    loop {
        // Fetch the next batch of pending rows.
        let pending = db
            .fetch_pending_classifications(user_did, burst_batch)
            .await?;

        if pending.is_empty() {
            return Ok(BurstOutcome::Complete);
        }

        // Classify the batch concurrently. Drive the stream with a manual
        // next() loop so we can stop accumulating verdicts the moment a
        // CostCeilingExceeded arrives — any already-dispatched concurrent
        // calls are drained to completion (buffer_unordered guarantees they
        // finish before next() returns None) but their results are discarded
        // after the cap fires, avoiding spurious billable calls in the NEXT
        // batch while bounding over-run to the current in-flight window.
        let mut stream = futures::stream::iter(pending)
            .map(|row| {
                let classifier = classifier.clone();
                async move {
                    // Reconstruct the exact envelope that classify_post uses:
                    // reply rows get the [Parent post] / [Reply] envelope;
                    // originals and quotes are passed as raw text.
                    let input = match &row.context_text {
                        Some(ctx) => crate::toxicity::format_parent_reply(ctx, &row.text),
                        None => row.text.clone(),
                    };
                    let result = classifier.classify(&input).await;
                    (row.account_did, row.post_uri, result)
                }
            })
            .buffer_unordered(burst_concurrency);

        let mut verdicts: Vec<VerdictRow> = Vec::new();
        let mut cost_capped = false;
        let mut interrupted = false;
        let mut other_error: Option<anyhow::Error> = None;

        while let Some((account_did, post_uri, outcome)) = stream.next().await {
            match outcome {
                Ok(verdict) => {
                    // Once a stop condition has fired (cost cap or a transient
                    // interrupt), drain the remaining in-flight calls without
                    // accumulating their verdicts so the next batch never starts.
                    if !cost_capped && !interrupted {
                        verdicts.push(VerdictRow {
                            account_did,
                            post_uri,
                            toxic_token: is_toxic(classifier.as_ref(), &verdict),
                            confidence: verdict.confidence,
                            model_id: verdict.model_id,
                            policy_version: verdict.policy_version,
                        });
                    }
                }
                Err(err) => {
                    if err.downcast_ref::<CostCeilingExceeded>().is_some() {
                        // Cost ceiling fired. Stop accumulating new verdicts;
                        // drain the rest of the in-flight batch harmlessly.
                        cost_capped = true;
                    } else if err.downcast_ref::<ClassifierTransientError>().is_some() {
                        // Transient backend failure (e.g. a RunPod blip with the
                        // retry budget exhausted). Stop gracefully like the cost
                        // cap — record what succeeded, leave the rest pending —
                        // so a resume can retry once the backend recovers, rather
                        // than hard-aborting the whole scan before finalize.
                        warn!(
                            account_did = %account_did,
                            error = %err,
                            "classifier transient failure — interrupting burst, scan resumable"
                        );
                        interrupted = true;
                    } else if other_error.is_none() {
                        // A permanent error (4xx / parse). Capture the first one;
                        // remaining successes still persist before we propagate.
                        // It must abort (not interrupt): leaving its row pending
                        // would livelock every resume.
                        other_error = Some(err);
                    }
                }
            }
        }

        // Persist whatever verdicts we collected (may be a partial batch if
        // some calls failed). One batched write per loop iteration.
        if !verdicts.is_empty() {
            db.record_classification_verdicts(user_did, &verdicts)
                .await?;
        }

        // After persisting, handle the stop/error cases in precedence order:
        // cost cap (billing backstop) first; then a permanent error (must abort
        // to avoid cross-resume livelock); then a transient interrupt (resumable).
        if cost_capped {
            return Ok(BurstOutcome::CostCapped);
        }
        if let Some(err) = other_error {
            return Err(err);
        }
        if interrupted {
            return Ok(BurstOutcome::Interrupted);
        }

        // All rows in this batch succeeded — continue to the next batch.
    }
}

// ── Env helpers ───────────────────────────────────────────────────────────────

/// Read `CHARCOAL_BURST_CONCURRENCY` (default 16, clamped to 1..=64).
///
/// Parse failures fall back to the default. The orchestrator (Chunk 6) calls
/// this once and passes the value to `run_burst`.
pub fn burst_concurrency() -> usize {
    let raw = std::env::var("CHARCOAL_BURST_CONCURRENCY")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok());
    match raw {
        Some(v) => v.clamp(1, 64),
        None => 16,
    }
}

/// Read `CHARCOAL_BURST_BATCH` (default 500, clamped to 1..=10_000).
///
/// Parse failures fall back to the default. The orchestrator (Chunk 6) calls
/// this once and passes the value to `run_burst`.
pub fn burst_batch() -> i64 {
    let raw = std::env::var("CHARCOAL_BURST_BATCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok());
    match raw {
        Some(v) => v.clamp(1, 10_000),
        None => 500,
    }
}
