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
use crate::toxicity::classifier::{
    is_toxic, ClassifierTransientError, ItemOutcome, ToxicityClassifier,
};
use crate::toxicity::cost_meter::CostCeilingExceeded;

// ── BurstOutcome ──────────────────────────────────────────────────────────────

/// The result of a completed `run_burst` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstOutcome {
    /// All pending rows were classified. `errored` counts slots that failed to
    /// decode and were recorded as benign sentinels (scan is degraded if > 0).
    Complete { errored: usize },
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

    let batch_size = classifier.max_batch_size().max(1);
    let mut total_errored: usize = 0;

    loop {
        let pending = db
            .fetch_pending_classifications(user_did, burst_batch)
            .await?;
        if pending.is_empty() {
            return Ok(BurstOutcome::Complete {
                errored: total_errored,
            });
        }

        // Build (account_did, post_uri, envelope) per row, then chunk by
        // max_batch_size. Each chunk becomes one classify_batch request.
        let items: Vec<(String, String, String)> = pending
            .into_iter()
            .map(|row| {
                let input = match &row.context_text {
                    Some(ctx) => crate::toxicity::format_parent_reply(ctx, &row.text),
                    None => row.text.clone(),
                };
                (row.account_did, row.post_uri, input)
            })
            .collect();
        let chunks: Vec<Vec<(String, String, String)>> =
            items.chunks(batch_size).map(|c| c.to_vec()).collect();

        let mut stream = futures::stream::iter(chunks)
            .map(|chunk| {
                let classifier = classifier.clone();
                async move {
                    let contents: Vec<String> =
                        chunk.iter().map(|(_, _, input)| input.clone()).collect();
                    let result = classifier.classify_batch(&contents).await;
                    (chunk, result)
                }
            })
            .buffer_unordered(burst_concurrency);

        let mut verdicts: Vec<VerdictRow> = Vec::new();
        let mut cost_capped = false;
        let mut interrupted = false;
        let mut other_error: Option<anyhow::Error> = None;

        while let Some((chunk, result)) = stream.next().await {
            match result {
                Ok(outcomes) => {
                    // Once a stop condition fired, drain remaining chunks without
                    // accumulating so the next batch never starts.
                    if cost_capped || interrupted {
                        continue;
                    }
                    if outcomes.len() != chunk.len() {
                        // Contract violation: positional alignment broken. Log at
                        // detection time so this batching-specific failure signal
                        // is always visible — otherwise a concurrently-in-flight
                        // chunk tripping the cost cap would win outcome precedence
                        // and return CostCapped before `other_error` is ever
                        // inspected, discarding the mismatch silently. Abort
                        // (permanent) after persisting prior successes.
                        warn!(
                            expected = chunk.len(),
                            got = outcomes.len(),
                            "RunPod batch length mismatch — contract violation, aborting after persisting prior successes"
                        );
                        if other_error.is_none() {
                            other_error = Some(anyhow::anyhow!(
                                "RunPod batch length mismatch: {} verdicts for {} inputs",
                                outcomes.len(),
                                chunk.len()
                            ));
                        }
                        continue;
                    }
                    for ((account_did, post_uri, _input), outcome) in
                        chunk.into_iter().zip(outcomes)
                    {
                        match outcome {
                            ItemOutcome::Verdict(v) => verdicts.push(VerdictRow {
                                account_did,
                                post_uri,
                                toxic_token: is_toxic(classifier.as_ref(), &v),
                                confidence: v.confidence,
                                model_id: v.model_id,
                                policy_version: v.policy_version,
                            }),
                            ItemOutcome::Error(detail) => {
                                // Fail open to benign: an un-decodable post can
                                // never inflate a false threat. Explicitly labelled
                                // + logged + metered — not a silent fallback.
                                warn!(
                                    account_did = %account_did,
                                    post_uri = %post_uri,
                                    error = %detail,
                                    "classifier decode error — recording benign sentinel, scan degraded"
                                );
                                crate::observability::classifier_metrics::record_decode_error(
                                    classifier.name(),
                                    1,
                                );
                                total_errored += 1;
                                verdicts.push(VerdictRow {
                                    account_did,
                                    post_uri,
                                    toxic_token: false,
                                    confidence: 0.0,
                                    model_id: "decode-error".to_string(),
                                    policy_version: classifier.policy_version().to_string(),
                                });
                            }
                        }
                    }
                }
                Err(err) => {
                    if err.downcast_ref::<CostCeilingExceeded>().is_some() {
                        cost_capped = true;
                    } else if err.downcast_ref::<ClassifierTransientError>().is_some() {
                        warn!(
                            error = %format!("{err:#}"),
                            "classifier transient failure — interrupting burst, scan resumable"
                        );
                        interrupted = true;
                    } else if other_error.is_none() {
                        other_error = Some(err);
                    }
                }
            }
        }

        if !verdicts.is_empty() {
            db.record_classification_verdicts(user_did, &verdicts)
                .await?;
        }

        // After persisting, handle stop/error cases in precedence order:
        // cost cap (billing backstop) first; then permanent error (abort to avoid
        // cross-resume livelock); then transient interrupt (resumable).
        if cost_capped {
            return Ok(BurstOutcome::CostCapped);
        }
        if let Some(err) = other_error {
            return Err(err);
        }
        if interrupted {
            return Ok(BurstOutcome::Interrupted);
        }
        // All chunks in this batch succeeded — continue to the next batch.
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
