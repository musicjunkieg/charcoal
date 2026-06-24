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

use crate::db::Database;
use crate::pipeline::scan_phases::staging::VerdictRow;
use crate::toxicity::classifier::{is_toxic, ToxicityClassifier};
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

        // Classify the batch concurrently, collecting (account_did, post_uri, Result<verdict>).
        let results = futures::stream::iter(pending)
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
            .buffer_unordered(burst_concurrency)
            .collect::<Vec<_>>()
            .await;

        // Partition results: collect successful VerdictRows; detect cost-cap or
        // other errors.
        let mut verdicts: Vec<VerdictRow> = Vec::with_capacity(results.len());
        let mut cost_capped = false;
        let mut other_error: Option<anyhow::Error> = None;

        for (account_did, post_uri, outcome) in results {
            match outcome {
                Ok(verdict) => {
                    verdicts.push(VerdictRow {
                        account_did,
                        post_uri,
                        toxic_token: is_toxic(classifier.as_ref(), &verdict),
                        confidence: verdict.confidence,
                        model_id: verdict.model_id,
                        policy_version: verdict.policy_version,
                    });
                }
                Err(err) => {
                    if err.downcast_ref::<CostCeilingExceeded>().is_some() {
                        // Cost ceiling fired. Mark cap and stop accumulating —
                        // we may receive more cap errors from the remaining
                        // concurrent calls; they are all the same signal.
                        cost_capped = true;
                    } else if other_error.is_none() {
                        // Capture the first non-ceiling error; remaining
                        // successes still persist before we propagate.
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

        // After persisting, handle the error cases.
        if cost_capped {
            return Ok(BurstOutcome::CostCapped);
        }
        if let Some(err) = other_error {
            return Err(err);
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
