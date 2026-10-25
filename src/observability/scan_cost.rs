//! Per-scan GPU cost ledger (#393). Every full scan and refresh leaves one
//! `scan_costs` row, success or failure, so the cost of onboarding a user is
//! a query instead of a billing-API investigation.
//!
//! The figure is the meter's worker-seconds integral: a LOWER BOUND on the
//! RunPod bill, which also charges each worker's idle tail and cold start.

use tracing::{info, warn};

use crate::db::{Database, ScanCostRow, ScanKind};
use crate::observability::cache_stats::CacheStats;
use crate::toxicity::cost_meter::{CostSnapshot, ScanCostMeter};

/// UTC, whole seconds, `Z` — sorts lexicographically in time order, which the
/// windowed sums rely on.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn build_scan_cost_row(
    user_did: &str,
    kind: ScanKind,
    completed: bool,
    started_at: &str,
    finished_at: &str,
    snap: CostSnapshot,
    classifier_stats: &CacheStats,
) -> ScanCostRow {
    ScanCostRow {
        user_did: user_did.to_string(),
        kind: kind.as_str().to_string(),
        outcome: if completed { "completed" } else { "failed" }.to_string(),
        started_at: started_at.to_string(),
        finished_at: finished_at.to_string(),
        // Cache misses are the verdicts that actually went to the backend.
        classifications: i64::try_from(classifier_stats.misses()).unwrap_or(i64::MAX),
        classifier_cache_hits: i64::try_from(classifier_stats.hits()).unwrap_or(i64::MAX),
        metered_worker_secs: snap.worker_secs,
        peak_in_flight: i64::from(snap.peak_in_flight),
        est_cents: i64::from(snap.est_cents),
        rate_cents_per_hour: i64::from(snap.rate_cents_per_hour),
    }
}

/// Log and persist this run's cost. Best-effort: a failed write is logged
/// and swallowed, because losing a cost row must never fail the scan whose
/// results are already saved.
pub async fn record_scan_cost(
    db: &dyn Database,
    user_did: &str,
    kind: ScanKind,
    completed: bool,
    started_at: &str,
    meter: &ScanCostMeter,
    classifier_stats: &CacheStats,
) {
    let row = build_scan_cost_row(
        user_did,
        kind,
        completed,
        started_at,
        &now_rfc3339(),
        meter.snapshot(),
        classifier_stats,
    );
    info!(
        user_did = %row.user_did,
        kind = %row.kind,
        outcome = %row.outcome,
        classified = row.classifications,
        cache_hits = row.classifier_cache_hits,
        gpu_secs = format!("{:.1}", row.metered_worker_secs),
        peak = row.peak_in_flight,
        est_cents = row.est_cents,
        "scan cost (est., excludes idle time)"
    );
    if let Err(e) = db.insert_scan_cost(&row).await {
        warn!(user_did = %row.user_did, error = %format!("{e:#}"), "failed to record scan cost");
    }
}
