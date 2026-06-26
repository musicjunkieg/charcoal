//! Per-scan RunPod cost backstop — a disaster brake (not a budget).
//!
//! RunPod bills GPU worker *uptime*, not classifications. The meter
//! conservatively assumes the worker stays warm from the first call onward and
//! stops the scan if estimated spend crosses a generous ceiling. See
//! docs/superpowers/specs/2026-06-21-runpod-scan-cost-backstop-design.md.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
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

/// Default for `CHARCOAL_RUNPOD_WORKERS_MAX` — the max concurrent RunPod workers
/// the burst can spin up. RunPod bills `$rate` *per worker*, so the cost integral
/// caps concurrency at this value (extra in-flight calls queue, not bill).
pub const DEFAULT_WORKERS_MAX: u32 = 10;

/// Worker-seconds integral. RunPod bills GPU worker *uptime*, and the burst runs
/// up to `workers_max` workers concurrently — so real spend is the integral of
/// `active_workers` over time, not single-worker wall-clock. `active_workers` is
/// `in_flight` capped at `workers_max` (RunPod runs no more than that many).
///
/// Time is injected as monotonic seconds so the accumulation is unit-testable
/// without `Instant`/sleeps (see `acc_tests`).
#[derive(Debug)]
struct Acc {
    worker_secs: f64,
    in_flight: u32,
    workers_max: u32,
    last_secs: Option<f64>,
}

impl Acc {
    fn new(workers_max: u32) -> Self {
        Self {
            worker_secs: 0.0,
            in_flight: 0,
            workers_max: workers_max.max(1),
            last_secs: None,
        }
    }

    /// Charge the interval since the last event at the worker level active during
    /// it, then mark `now_secs` as the new boundary. Calling twice with the same
    /// `now_secs` charges nothing the second time.
    fn accrue(&mut self, now_secs: f64) {
        if let Some(last) = self.last_secs {
            let dt = (now_secs - last).max(0.0);
            let active = self.in_flight.min(self.workers_max) as f64;
            self.worker_secs += active * dt;
        }
        self.last_secs = Some(now_secs);
    }

    /// A RunPod call started: charge the prior interval, then count this call.
    fn enter(&mut self, now_secs: f64) {
        self.accrue(now_secs);
        self.in_flight += 1;
    }

    /// A RunPod call finished: charge the interval it was part of, then drop it.
    fn leave(&mut self, now_secs: f64) {
        self.accrue(now_secs);
        self.in_flight = self.in_flight.saturating_sub(1);
    }
}

/// Per-scan cost meter. Created once per scan, shared (Arc) with the RunPod
/// client. Immutable after construction except the first-call clock and the
/// one-shot warn flag.
#[derive(Debug)]
pub struct ScanCostMeter {
    /// Worker-seconds integral, mutated under a short (no-await) lock on every
    /// `enter`/`leave`. The lock is held only for the arithmetic — safe across
    /// the concurrent `classify` calls `buffer_unordered` runs.
    acc: Mutex<Acc>,
    /// Base instant for converting wall-clock to the accumulator's monotonic
    /// seconds. Armed lazily on the first event.
    base: OnceLock<Instant>,
    rate_cents_per_hour: u32,
    /// 0 = backstop disabled.
    ceiling_cents: u32,
    /// One-shot guard so the trip WARN is emitted once, not once per concurrent
    /// in-flight call.
    warned: AtomicBool,
}

/// RAII guard: while held, its RunPod call counts toward in-flight worker usage;
/// on drop (call complete, success or error) it leaves the in-flight set,
/// charging its share of the worker-seconds integral. Hold it for the full
/// duration of the RunPod request.
#[must_use = "hold the guard for the duration of the RunPod call so its worker time is billed"]
#[derive(Debug)]
pub struct InFlightGuard<'a> {
    meter: &'a ScanCostMeter,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        let now = self.meter.now_secs();
        self.meter.acc.lock().unwrap().leave(now);
    }
}

impl ScanCostMeter {
    pub fn new(ceiling_cents: u32, rate_cents_per_hour: u32) -> Self {
        Self::with_workers(ceiling_cents, rate_cents_per_hour, DEFAULT_WORKERS_MAX)
    }

    /// As [`new`](Self::new), with an explicit max-concurrent-workers cap — the
    /// per-worker billing multiplier the cost integral is capped at.
    pub fn with_workers(ceiling_cents: u32, rate_cents_per_hour: u32, workers_max: u32) -> Self {
        Self {
            acc: Mutex::new(Acc::new(workers_max)),
            base: OnceLock::new(),
            rate_cents_per_hour,
            ceiling_cents,
            warned: AtomicBool::new(false),
        }
    }

    /// Monotonic seconds since the meter's base instant (armed on first use).
    fn now_secs(&self) -> f64 {
        self.base.get_or_init(Instant::now).elapsed().as_secs_f64()
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
        // Max concurrent RunPod workers — the per-worker billing multiplier the
        // cost integral caps at. Set this to match the endpoint's workersMax.
        let workers_max = std::env::var("CHARCOAL_RUNPOD_WORKERS_MAX")
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .filter(|&v| v > 0)
            .unwrap_or(DEFAULT_WORKERS_MAX);
        Self::with_workers(ceiling_cents, rate_cents_per_hour, workers_max)
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
        let now = self.now_secs();
        let mut acc = self.acc.lock().unwrap();
        acc.accrue(now); // bring the integral current before reporting
        (acc.worker_secs / 3600.0 * self.rate_cents_per_hour as f64) as u32
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
    pub fn arm_and_check(&self) -> Result<InFlightGuard<'_>, CostCeilingExceeded> {
        let now = self.now_secs();
        // Enter the in-flight set (charge the prior interval, then count this
        // call) and read the running worker-seconds total for the trip check.
        let worker_secs = {
            let mut acc = self.acc.lock().unwrap();
            acc.enter(now);
            acc.worker_secs
        };
        match self.check_with_elapsed(worker_secs) {
            Ok(()) => Ok(InFlightGuard { meter: self }),
            Err(e) => {
                // Over the ceiling — this call is skipped, so back its entry out
                // rather than leak an in-flight slot into the integral.
                let now = self.now_secs();
                self.acc.lock().unwrap().leave(now);
                Err(e)
            }
        }
    }

    /// Test/seam only: pre-seed the worker-seconds integral so a trip is
    /// deterministic without concurrent calls or sleeping. The next
    /// `arm_and_check` then evaluates against this seeded total.
    ///
    /// Kept `pub` (not `#[cfg(test)]`) because integration tests in `tests/`
    /// link the lib *without* `cfg(test)` and exercise this seam
    /// (`tests/unit_classifier.rs`); gating it on `cfg(test)` would make it
    /// invisible to them. `#[doc(hidden)]` keeps it out of the public docs.
    #[doc(hidden)]
    pub fn force_worker_seconds(&self, secs: f64) {
        let _ = self.base.get_or_init(Instant::now);
        self.acc.lock().unwrap().worker_secs = secs;
    }
}

#[cfg(test)]
mod acc_tests {
    use super::Acc;

    #[test]
    fn integral_single_worker_equals_elapsed() {
        // One worker for 10s = 10 worker-seconds (matches the old single-worker
        // model exactly).
        let mut a = Acc::new(10);
        a.enter(0.0); // in_flight 1
        a.leave(10.0); // charge 1 worker × 10s
        assert_eq!(a.worker_secs, 10.0);
    }

    #[test]
    fn integral_sums_concurrent_workers() {
        // Two workers for 10s (=20), then one for 10s (=10) → 30 worker-seconds,
        // even though only 20s of wall-clock elapsed.
        let mut a = Acc::new(10);
        a.enter(0.0); // 1
        a.enter(0.0); // 2
        a.leave(10.0); // charge 2×10=20, now 1
        a.leave(20.0); // charge 1×10=10, now 0
        assert_eq!(a.worker_secs, 30.0);
    }

    #[test]
    fn caps_active_workers_at_workers_max() {
        // 4 in flight but workers_max=2: RunPod runs at most 2 workers, so the
        // integral charges 2 (not 4) — accurate, not over-counted.
        let mut a = Acc::new(2);
        a.enter(0.0);
        a.enter(0.0);
        a.enter(0.0);
        a.enter(0.0); // in_flight 4
        a.leave(10.0); // active = min(4,2) = 2 → +20
        assert_eq!(a.worker_secs, 20.0);
    }

    #[test]
    fn idle_gaps_cost_nothing() {
        // No workers in flight → no cost accrues across the gap.
        let mut a = Acc::new(10);
        a.enter(0.0);
        a.leave(5.0); // 1×5 = 5, now 0 in flight
        a.accrue(100.0); // 95s with zero workers → no charge
        assert_eq!(a.worker_secs, 5.0);
    }
}

#[cfg(test)]
mod meter_tests {
    use super::{CostCeilingExceeded, ScanCostMeter};

    #[test]
    fn arm_and_check_trips_when_integral_over_ceiling() {
        // Seed 2200 worker-seconds: 2200/3600 × 329c ≈ 201c ≥ the 200c ceiling.
        let m = ScanCostMeter::new(200, 329);
        m.force_worker_seconds(2200.0);
        let err = m.arm_and_check().unwrap_err();
        assert_eq!(err.ceiling_cents, 200);
        // The skipped call must not leak an in-flight slot: estimate stays put,
        // not inflated by a phantom worker.
        assert!(m.estimated_cents() >= 200);
    }

    #[test]
    fn arm_and_check_returns_guard_when_under_ceiling() {
        let m = ScanCostMeter::new(500, 329);
        let guard = m.arm_and_check();
        assert!(
            guard.is_ok(),
            "should be under ceiling at zero worker-seconds"
        );
        drop(guard); // leaving the in-flight set must not panic
    }

    #[test]
    fn disabled_meter_never_trips() {
        // ceiling 0 = disabled: even a huge seeded integral does not trip.
        let m = ScanCostMeter::new(0, 329);
        m.force_worker_seconds(1_000_000.0);
        assert!(m.arm_and_check().is_ok());
    }

    #[test]
    fn trip_carries_the_real_dollar_estimate() {
        // The reported est_cents reflects the worker-seconds integral, not a
        // single-worker wall-clock guess.
        let m = ScanCostMeter::new(200, 329);
        m.force_worker_seconds(3600.0); // exactly one worker-hour = 329c
        let err: CostCeilingExceeded = m.arm_and_check().unwrap_err();
        assert_eq!(err.est_cents, 329);
    }
}
