//! Per-scan RunPod cost backstop — a disaster brake (not a budget).
//!
//! RunPod bills GPU worker *uptime*, not classifications. The meter
//! conservatively assumes the worker stays warm from the first call onward and
//! stops the scan if estimated spend crosses a generous ceiling. See
//! docs/superpowers/specs/2026-06-21-runpod-scan-cost-backstop-design.md.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;
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

/// Per-scan cost meter. Created once per scan, shared (Arc) with the RunPod
/// client. Immutable after construction except the first-call clock and the
/// one-shot warn flag.
#[derive(Debug)]
pub struct ScanCostMeter {
    /// First-RunPod-call instant. `OnceLock` gives lock-free set-exactly-once
    /// arming and lock-free reads, required because `buffer_unordered` runs many
    /// `classify` calls concurrently. Do NOT replace with `Mutex<Option<_>>`.
    started_at: OnceLock<Instant>,
    rate_cents_per_hour: u32,
    /// 0 = backstop disabled.
    ceiling_cents: u32,
    /// One-shot guard so the trip WARN is emitted once, not once per concurrent
    /// in-flight call.
    warned: AtomicBool,
}

impl ScanCostMeter {
    pub fn new(ceiling_cents: u32, rate_cents_per_hour: u32) -> Self {
        Self {
            started_at: OnceLock::new(),
            rate_cents_per_hour,
            ceiling_cents,
            warned: AtomicBool::new(false),
        }
    }

    /// Build from env. Backstop is ON by default.
    /// - `CHARCOAL_SCAN_COST_CEILING_CENTS`: unset or malformed -> 500; explicit
    ///   `0` -> disabled; other positive int -> that ceiling.
    /// - `CHARCOAL_GPU_COST_CENTS_PER_HOUR`: unset / malformed / 0 -> 329.
    pub fn from_env() -> Self {
        let ceiling_cents = match std::env::var("CHARCOAL_SCAN_COST_CEILING_CENTS") {
            Err(_) => DEFAULT_CEILING_CENTS, // unset
            Ok(s) => match s.trim().parse::<u32>() {
                Ok(v) => v, // includes explicit 0 (disabled)
                Err(_) => {
                    tracing::warn!(
                        value = %s,
                        "CHARCOAL_SCAN_COST_CEILING_CENTS malformed; using default {DEFAULT_CEILING_CENTS}"
                    );
                    DEFAULT_CEILING_CENTS
                }
            },
        };
        let rate_cents_per_hour = std::env::var("CHARCOAL_GPU_COST_CENTS_PER_HOUR")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_RATE_CENTS_PER_HOUR);
        Self::new(ceiling_cents, rate_cents_per_hour)
    }

    /// Whether the one-shot trip WARN has fired. Introspection for tests
    /// (integration tests link the lib in non-test config, so this cannot be
    /// `#[cfg(test)]`); harmless in prod — a single relaxed atomic load.
    pub fn has_warned(&self) -> bool {
        self.warned.load(Ordering::Relaxed)
    }

    /// Estimated spend in cents from the first call to now; 0 before arming.
    /// Display/logging only — never gates the trip (see `over_ceiling`).
    pub fn estimated_cents(&self) -> u32 {
        match self.started_at.get() {
            Some(t0) => {
                (t0.elapsed().as_secs_f64() / 3600.0 * self.rate_cents_per_hour as f64) as u32
            }
            None => 0,
        }
    }

    /// Core trip logic with an injected elapsed — the test seam (no Instant, no
    /// sleeping). Disabled fast-path first, then the pure predicate, then a
    /// de-duped WARN, then the error.
    pub fn check_with_elapsed(&self, elapsed_secs: f64) -> Result<(), CostCeilingExceeded> {
        if self.ceiling_cents == 0 {
            return Ok(()); // disabled
        }
        if over_ceiling(elapsed_secs, self.rate_cents_per_hour, self.ceiling_cents) {
            let est = (elapsed_secs / 3600.0 * self.rate_cents_per_hour as f64) as u32;
            if !self.warned.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    metric = "scan_cost_capped",
                    est_cents = est,
                    ceiling_cents = self.ceiling_cents,
                    "scan cost-capped: est ~${:.2} after {:.0}min (ceiling ${:.2})",
                    est as f64 / 100.0,
                    elapsed_secs / 60.0,
                    self.ceiling_cents as f64 / 100.0,
                );
            }
            return Err(CostCeilingExceeded {
                est_cents: est,
                ceiling_cents: self.ceiling_cents,
            });
        }
        Ok(())
    }

    /// Arm-then-check. Called before each RunPod request. The first-ever call
    /// arms with elapsed 0 (cannot trip, given ceiling > 0); later calls measure
    /// real elapsed. Order is load-bearing: arm THEN check.
    pub fn arm_and_check(&self) -> Result<(), CostCeilingExceeded> {
        let t0 = *self.started_at.get_or_init(Instant::now);
        self.check_with_elapsed(t0.elapsed().as_secs_f64())
    }

    /// Test/seam only: force the armed clock to a specific instant so trips are
    /// deterministic without sleeping. No-op if already armed.
    ///
    /// Kept `pub` (not `#[cfg(test)]`) because integration tests in `tests/`
    /// link the lib *without* `cfg(test)` and exercise this seam
    /// (`tests/unit_classifier.rs`); gating it on `cfg(test)` would make it
    /// invisible to them. `#[doc(hidden)]` keeps it out of the public docs.
    #[doc(hidden)]
    pub fn force_started_at(&self, t: Instant) {
        let _ = self.started_at.set(t);
    }
}
