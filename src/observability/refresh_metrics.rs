//! Refresh-tick metrics (#343 §4.4, #344).
//!
//! The tick runs on the admitter loop and swallows its errors — it must not
//! take the admitter down — so the only evidence a tick failed is what it
//! emits here. One `tracing` event per failure for the log scraper, plus a
//! process-lifetime counter so a health endpoint or a test can ask "has this
//! ever failed?" without parsing logs.

use std::sync::atomic::{AtomicU64, Ordering};

static TICK_FAILURES: AtomicU64 = AtomicU64::new(0);

/// One refresh tick failed: nothing was scheduled, and the same users are due
/// on the next tick. A persistent failure here is a wedge — refreshes stop
/// silently — so it is a metric, not just an error line.
pub fn record_tick_failure() {
    let total = TICK_FAILURES.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::info!(metric = "refresh_tick_failure_total", count = total);
}

/// How many refresh ticks have failed since this process started.
pub fn tick_failures() -> u64 {
    TICK_FAILURES.load(Ordering::Relaxed)
}
