//! Retention policy for the #343 shared cache tables.
//!
//! The v16 cache tables (`account_feed_snapshots`, `onnx_scores`,
//! `classifier_verdicts`) carry no `user_did`, so `delete_user_data` never
//! touches them and nothing else deletes from them either. Left alone they
//! grow forever: one permanent posts_json snapshot per distinct DID ever
//! sampled, and one row per text hash per model/policy generation. This
//! module owns the cutoffs and the best-effort sweep that bounds them
//! (CodeRabbit, PR #118).

use chrono::Utc;
use tracing::{info, warn};

use crate::db::traits::Database;

/// Feed snapshots are served only within SNAPSHOT_TTL (24 h); anything
/// older is dead weight. 7 days keeps a margin for the #343 A/B runbook,
/// which compares scans within a day, and for post-mortems.
pub const FEED_SNAPSHOT_RETENTION: chrono::Duration = chrono::Duration::days(7);

/// Scores and verdicts are recomputed after this — cheap relative to
/// storage, and it doubles as the re-score horizon #344 asked for: a text
/// scored under an older model/policy generation ages out instead of
/// living forever.
pub const SCORE_RETENTION: chrono::Duration = chrono::Duration::days(90);

/// Row counts removed by one `evict_stale_cache` sweep, per table.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CacheEviction {
    pub feed_snapshots: u64,
    pub onnx_scores: u64,
    pub classifier_verdicts: u64,
}

impl CacheEviction {
    /// True when the sweep removed nothing — the common case, and the one
    /// that should stay off the log.
    pub fn is_empty(&self) -> bool {
        self.feed_snapshots == 0 && self.onnx_scores == 0 && self.classifier_verdicts == 0
    }
}

/// Sweep the cache tables once, swallowing any failure.
///
/// A cache is a diagnostic/optimisation, never load-bearing: a scan that
/// cannot evict is still a correct scan, just one running against a larger
/// table. So this warns and returns rather than propagating — it is called
/// at scan start, where an error would abort work the user asked for.
pub async fn evict_stale_cache_best_effort(db: &dyn Database) {
    // Cutoffs are computed here, in Rust, and passed as bound parameters:
    // the timestamp columns are RFC3339 TEXT on both backends, so the
    // comparison is lexicographic. That is exact for `to_rfc3339()` output
    // because it is fixed-width UTC (`+00:00`). Using SQL `NOW()` would
    // compare a timestamptz against text and break on both backends.
    let now = Utc::now();
    let feed_cutoff = (now - FEED_SNAPSHOT_RETENTION).to_rfc3339();
    let score_cutoff = (now - SCORE_RETENTION).to_rfc3339();

    match db.evict_stale_cache(&feed_cutoff, &score_cutoff).await {
        Ok(evicted) if evicted.is_empty() => {}
        Ok(evicted) => info!(
            feed_snapshots = evicted.feed_snapshots,
            onnx_scores = evicted.onnx_scores,
            classifier_verdicts = evicted.classifier_verdicts,
            feed_cutoff = %feed_cutoff,
            score_cutoff = %score_cutoff,
            "Evicted stale shared-cache rows"
        ),
        Err(e) => warn!(
            error = %e,
            "Shared-cache eviction failed — continuing; the cache is an optimisation, not state"
        ),
    }
}
