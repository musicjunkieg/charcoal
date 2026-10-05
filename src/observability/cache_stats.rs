//! Hit/miss counters for the #343 shared caches.
//!
//! One `CacheStats` per decorator per scan. The counters are atomics so the
//! decorators can be shared across the gather's concurrent tasks without a
//! lock, and they are persisted to `scan_state` at the end of the scan so
//! the hit rate is readable from the DB (the runbook reads it there, not
//! from Railway logs).

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

use crate::db::Database;

#[derive(Debug, Default)]
pub struct CacheStats {
    hits: AtomicU64,
    misses: AtomicU64,
}

impl CacheStats {
    pub fn hit(&self, n: u64) {
        self.hits.fetch_add(n, Ordering::Relaxed);
    }

    pub fn miss(&self, n: u64) {
        self.misses.fetch_add(n, Ordering::Relaxed);
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
}

/// Persist the counters as `scan_state` rows `{prefix}_cache_hits` and
/// `{prefix}_cache_misses` for `user_did`. Prefixes in use: `feed`, `onnx`,
/// `classifier`.
pub async fn record_cache_stats(
    db: &dyn Database,
    user_did: &str,
    prefix: &str,
    stats: &CacheStats,
) -> Result<()> {
    db.set_scan_state(
        user_did,
        &format!("{prefix}_cache_hits"),
        &stats.hits().to_string(),
    )
    .await?;
    db.set_scan_state(
        user_did,
        &format!("{prefix}_cache_misses"),
        &stats.misses().to_string(),
    )
    .await?;
    Ok(())
}
