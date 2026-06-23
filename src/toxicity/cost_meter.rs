//! Per-scan RunPod cost backstop — a disaster brake (not a budget).
//!
//! RunPod bills GPU worker *uptime*, not classifications. The meter
//! conservatively assumes the worker stays warm from the first call onward and
//! stops the scan if estimated spend crosses a generous ceiling. See
//! docs/superpowers/specs/2026-06-21-runpod-scan-cost-backstop-design.md.

use thiserror::Error;

/// Backstop ceiling default: $5. On by default; only an explicit `0` disables.
pub const DEFAULT_CEILING_CENTS: u32 = 500;
/// GPU rate default: observed H100 $3.29/hr. Conservatively covers H200 fallback.
pub const DEFAULT_RATE_CENTS_PER_HOUR: u32 = 329;

/// Returned (non-retryable) from `classify` once the backstop trips. It rides the
/// same graceful skip-and-continue path the live HTTP 402 already exercised.
#[derive(Debug, Error)]
#[error(
    "scan cost ceiling exceeded: est ~{est_cents}c >= ceiling {ceiling_cents}c (non-retryable)"
)]
pub struct CostCeilingExceeded {
    pub est_cents: u32,
    pub ceiling_cents: u32,
}

/// Pure trip predicate — the sole trip authority. ALWAYS called with
/// `ceiling_cents > 0` (the meter short-circuits the disabled case first). Using
/// f64 here means truncation in the u32 estimate can never disagree with the trip.
pub fn over_ceiling(elapsed_secs: f64, rate_cents_per_hour: u32, ceiling_cents: u32) -> bool {
    elapsed_secs / 3600.0 * rate_cents_per_hour as f64 >= ceiling_cents as f64
}
