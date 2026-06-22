# RunPod Scan Cost Backstop Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a per-scan cost backstop that hard-stops a runaway RunPod scan before it racks up disaster spend, enforced at the per-RunPod-call boundary.

**Architecture:** A per-scan `ScanCostMeter` (armed on the first RunPod call, conservative `elapsed × rate` metering) is consulted inside `RunPodCopeBClient::classify` before every request. Over the ceiling, the client returns a non-retryable `CostCeilingExceeded` error that rides the existing graceful-skip path the live HTTP 402 already proved. The meter is built per scan in `build_from_env` (which `scan_job::run_scan` calls per scan) and disabled for the one-off gate/compare CLI. All units are architecture-independent — reused unchanged by the later decouple work.

**Tech Stack:** Rust, tokio, thiserror, tracing, `std::sync::OnceLock`/`AtomicBool`, wiremock (client tests). Suite: `cargo test --features web`; clippy across `--features web` / default / `--features postgres`.

**Spec:** `docs/superpowers/specs/2026-06-21-runpod-scan-cost-backstop-design.md`

**Branch:** `feat/cope-b-cost-guard`

---

## File Structure

- **Create `src/toxicity/cost_meter.rs`** — the whole feature surface: `over_ceiling` (pure predicate), `CostCeilingExceeded` (error), `ScanCostMeter` (meter + `from_env`), default constants. One focused module, no pipeline knowledge.
- **Modify `src/toxicity/mod.rs`** — add `pub mod cost_meter;`.
- **Modify `src/toxicity/runpod_cope_b.rs`** — add an `Arc<ScanCostMeter>` field, a `with_meter` builder, and the pre-call check in `classify`.
- **Modify `src/toxicity/classifier.rs`** — in `build_from_env` (per-scan) attach `ScanCostMeter::from_env()`; in `build_backend_named` (one-off CLI) attach a disabled meter.
- **Modify `src/observability/classifier_metrics.rs`** — repoint the dead `estimate_cost_cents` RunPod branch at the shared default rate constant.
- **Create `tests/unit_cost_meter.rs`** — pure-fn + meter unit tests.
- **Modify the RunPod client test module** — wiremock test that the meter short-circuits the HTTP call. (Locate existing RunPod client tests first — Task 4 Step 1.)

---

## Chunk 1: Cost backstop core + enforcement

### Task 1: Pure `over_ceiling`, error type, constants, module wiring

**Files:**
- Create: `src/toxicity/cost_meter.rs`
- Modify: `src/toxicity/mod.rs`
- Test: `tests/unit_cost_meter.rs`

- [ ] **Step 1: Wire the module**

In `src/toxicity/mod.rs`, add alongside the other `pub mod` lines:
```rust
pub mod cost_meter;
```

- [ ] **Step 2: Write the failing test for the pure predicate**

Create `tests/unit_cost_meter.rs`:
```rust
use charcoal::toxicity::cost_meter::{over_ceiling, DEFAULT_CEILING_CENTS, DEFAULT_RATE_CENTS_PER_HOUR};

// At ceiling=500c, rate=329c/hr the trip point is elapsed = 500*3600/329 = 5471.13s.
// `>=` semantics: false just under, true just over.
#[test]
fn over_ceiling_boundary() {
    assert!(!over_ceiling(5471.0, 329, 500), "just under must not trip");
    assert!(over_ceiling(5472.0, 329, 500), "just over must trip");
}

#[test]
fn over_ceiling_zero_elapsed_never_trips() {
    assert!(!over_ceiling(0.0, 329, 500));
}

#[test]
fn defaults_are_500_and_329() {
    assert_eq!(DEFAULT_CEILING_CENTS, 500);
    assert_eq!(DEFAULT_RATE_CENTS_PER_HOUR, 329);
}
```

- [ ] **Step 3: Run it, verify it fails to compile**

Run: `cargo test --test unit_cost_meter`
Expected: FAIL — `cost_meter` / symbols unresolved.

- [ ] **Step 4: Implement the pure pieces**

Create `src/toxicity/cost_meter.rs`:
```rust
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
#[error("scan cost ceiling exceeded: est ~{est_cents}c >= ceiling {ceiling_cents}c (non-retryable)")]
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
```

(The `ScanCostMeter` struct lands in Task 2 — keep this step to the pure surface so the boundary test passes in isolation.)

- [ ] **Step 5: Run the tests, verify pass**

Run: `cargo test --test unit_cost_meter`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add src/toxicity/mod.rs src/toxicity/cost_meter.rs tests/unit_cost_meter.rs
git commit -m 'feat(cost-guard): pure over_ceiling predicate + CostCeilingExceeded (#206)'
```

---

### Task 2: `ScanCostMeter` — arming, disabled fast-path, de-duped warn, `from_env`

**Files:**
- Modify: `src/toxicity/cost_meter.rs`
- Test: `tests/unit_cost_meter.rs`

- [ ] **Step 1: Write the failing tests**

Append to `tests/unit_cost_meter.rs`:
```rust
use charcoal::toxicity::cost_meter::ScanCostMeter;

#[test]
fn check_with_elapsed_under_is_ok() {
    let m = ScanCostMeter::new(500, 329);
    assert!(m.check_with_elapsed(5471.0).is_ok());
}

#[test]
fn check_with_elapsed_over_errors() {
    let m = ScanCostMeter::new(500, 329);
    let err = m.check_with_elapsed(5472.0).unwrap_err();
    assert_eq!(err.ceiling_cents, 500);
    assert!(err.est_cents >= 500);
}

#[test]
fn ceiling_zero_is_disabled() {
    let m = ScanCostMeter::new(0, 329);
    // Disabled: never trips no matter how large the elapsed.
    assert!(m.check_with_elapsed(1_000_000.0).is_ok());
}

#[test]
fn warn_fires_at_most_once() {
    let m = ScanCostMeter::new(500, 329);
    // Two over-ceiling observations; the dedup flag means only the first warns.
    assert!(m.check_with_elapsed(6000.0).is_err());
    assert!(m.check_with_elapsed(6000.0).is_err());
    // No assertion on log output here (tracing has no global sink in unit tests);
    // the once-guard is verified by `warned_flag_flips_once` below.
}

#[test]
fn warned_flag_flips_once() {
    let m = ScanCostMeter::new(500, 329);
    assert!(!m.has_warned());
    let _ = m.check_with_elapsed(6000.0);
    assert!(m.has_warned());
}

#[test]
fn estimate_is_zero_before_arming() {
    let m = ScanCostMeter::new(500, 329);
    // Not yet armed (no classify call): estimate must be 0.
    assert_eq!(m.estimated_cents(), 0);
}

#[test]
fn from_env_unset_defaults_enabled() {
    // No env vars set in this test process path -> defaults.
    // (Run-order independent: construct directly with the documented defaults.)
    let m = ScanCostMeter::new(DEFAULT_CEILING_CENTS, DEFAULT_RATE_CENTS_PER_HOUR);
    assert!(m.check_with_elapsed(6000.0).is_err(), "default ceiling is enabled");
}
```

- [ ] **Step 2: Run, verify it fails**

Run: `cargo test --test unit_cost_meter`
Expected: FAIL — `ScanCostMeter`, `new`, `check_with_elapsed`, `has_warned`, `estimated_cents` unresolved.

- [ ] **Step 3: Implement `ScanCostMeter`**

Append to `src/toxicity/cost_meter.rs`:
```rust
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

    #[cfg(test)]
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
            return Err(CostCeilingExceeded { est_cents: est, ceiling_cents: self.ceiling_cents });
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
}
```

- [ ] **Step 4: Run, verify pass**

Run: `cargo test --test unit_cost_meter`
Expected: PASS (all meter tests).

- [ ] **Step 5: Commit**

```bash
git add src/toxicity/cost_meter.rs tests/unit_cost_meter.rs
git commit -m 'feat(cost-guard): ScanCostMeter (OnceLock arming, disabled+warn-once, from_env) (#206)'
```

---

### Task 3: Enforce in `RunPodCopeBClient::classify`

**Files:**
- Modify: `src/toxicity/runpod_cope_b.rs`
- Test: the RunPod client wiremock test module (located in Step 1)

- [ ] **Step 1: Confirm the RunPod client test location**

The existing wiremock/`MockServer` tests for `RunPodCopeBClient` (including the
`/runsync` poll regression test) live in **`tests/unit_classifier.rs`**. Add the
new test there. Verify with: `grep -rln "wiremock\|MockServer" tests/`.
Note: Task 4's factory test also lands in `tests/unit_classifier.rs` — both
tasks append to the same existing file; do not create a new test file.

- [ ] **Step 2: Write the failing test — over-ceiling short-circuits the HTTP call**

Add (adapting `MockServer` setup to match the file's existing helpers):
```rust
#[tokio::test]
async fn classify_short_circuits_when_over_ceiling() {
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use charcoal::toxicity::classifier::ToxicityClassifier; // brings the `.classify` trait method into scope
    use charcoal::toxicity::cost_meter::ScanCostMeter;

    let server = wiremock::MockServer::start().await;
    // Any hit on /runsync would fail the test: expect ZERO requests.
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .respond_with(wiremock::ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let meter = Arc::new(ScanCostMeter::new(500, 329));
    // Pre-arm the meter to a time well past the ceiling (no sleeping).
    meter.force_started_at(Instant::now() - Duration::from_secs(6000));

    let client = charcoal::toxicity::runpod_cope_b::RunPodCopeBClient::new(
        server.uri(), // already a String; do not wrap in format!() (clippy::useless_format under -D warnings)
        "test-key".into(),
    )
    .unwrap()
    .with_meter(meter);

    let err = client.classify("[Parent post]: x\n\n[Reply]: y").await.unwrap_err();
    assert!(
        err.downcast_ref::<charcoal::toxicity::cost_meter::CostCeilingExceeded>().is_some(),
        "expected CostCeilingExceeded, got: {err:#}"
    );
    // .expect(0) on drop verifies no HTTP request was issued.
}
```

This requires a `pub(crate)` test seam `force_started_at`. Add to `ScanCostMeter` (cost_meter.rs):
```rust
impl ScanCostMeter {
    /// Test/seam only: force the armed clock to a specific instant so trips are
    /// deterministic without sleeping.
    pub fn force_started_at(&self, t: Instant) {
        let _ = self.started_at.set(t);
    }
}
```
(Keep it public — it's harmless in prod and needed by the integration test in another crate. If the test ends up co-located in `src/`, `pub(crate)` is fine.)

- [ ] **Step 3: Run, verify it fails to compile**

Run: `cargo test --features web --test unit_classifier classify_short_circuits_when_over_ceiling` (or the inline module path)
Expected: FAIL — `with_meter` / `force_started_at` unresolved.

- [ ] **Step 4: Add the meter field, builder, and pre-call check**

In `src/toxicity/runpod_cope_b.rs`:

Imports (top, with the others):
```rust
use std::sync::Arc;
use super::cost_meter::ScanCostMeter;
```

Add the field to the struct (after `max_retries`):
```rust
    /// Per-scan cost backstop. Default (from `new`) is disabled; `build_from_env`
    /// attaches an env-configured meter per scan.
    meter: Arc<ScanCostMeter>,
```

In `new`, initialize it disabled (ceiling 0) so existing callers/tests are unchanged in behavior:
```rust
        Ok(Self {
            client,
            endpoint_url,
            api_key,
            steady_timeout: Duration::from_millis(steady_ms),
            warmup_timeout: Duration::from_millis(warmup_ms),
            max_retries,
            meter: Arc::new(ScanCostMeter::new(0, super::cost_meter::DEFAULT_RATE_CENTS_PER_HOUR)),
        })
```

Add a builder after `new`:
```rust
    /// Attach a per-scan cost meter. Builder so `new`'s signature (and its
    /// existing callers/tests) stay unchanged.
    pub fn with_meter(mut self, meter: Arc<ScanCostMeter>) -> Self {
        self.meter = meter;
        self
    }
```

In the trait `classify`, check the meter BEFORE issuing the request:
```rust
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
        // Cost backstop: arm-then-check before every RunPod request. Over the
        // ceiling this returns a non-retryable error that rides the same skip
        // path the live HTTP 402 already exercised — no new caller handling.
        self.meter.arm_and_check()?;

        let (verdict, retries) = self
            .classify_with_timeout(content, self.steady_timeout)
            .await?;
        crate::observability::classifier_metrics::record_request(
            self.name(),
            verdict.latency_ms,
            verdict.toxic_token,
            retries,
        );
        Ok(verdict)
    }
```
(`?` converts `CostCeilingExceeded` into `anyhow::Error` via its `std::error::Error` impl.)

- [ ] **Step 5: Run, verify pass**

Run: `cargo test --features web --test unit_classifier classify_short_circuits_when_over_ceiling`
Expected: PASS (no HTTP request issued; `CostCeilingExceeded` returned).

- [ ] **Step 6: Run the full RunPod client test module to confirm no regression**

Run: `cargo test --features web --test unit_classifier`
Expected: PASS — existing client tests still green (they use the disabled default meter).

- [ ] **Step 7: Commit**

```bash
git add src/toxicity/runpod_cope_b.rs src/toxicity/cost_meter.rs tests/unit_classifier.rs
git commit -m 'feat(cost-guard): enforce backstop at the per-RunPod-call boundary (#206)'
```

---

### Task 4: Thread the meter through the factory

**Files:**
- Modify: `src/toxicity/classifier.rs`
- Test: `tests/unit_classifier.rs` (existing factory tests)

- [ ] **Step 1: Attach the meter in the factories**

In `src/toxicity/classifier.rs`, the `runpod` branch of `build_from_env` currently ends:
```rust
            let client = crate::toxicity::runpod_cope_b::RunPodCopeBClient::new(endpoint, api_key)?;
            Ok(Arc::new(client))
```
Change to attach a per-scan env-configured meter:
```rust
            let meter = std::sync::Arc::new(crate::toxicity::cost_meter::ScanCostMeter::from_env());
            let client = crate::toxicity::runpod_cope_b::RunPodCopeBClient::new(endpoint, api_key)?
                .with_meter(meter);
            Ok(Arc::new(client))
```

In `build_backend_named`'s `runpod` branch, leave the **disabled** default (the gate/compare CLI is one-off, not a scan, so no backstop). I.e. keep:
```rust
            let client = crate::toxicity::runpod_cope_b::RunPodCopeBClient::new(endpoint, api_key)?;
            Ok(Arc::new(client))
```
Add a one-line comment there: `// one-off compare/gate CLI: no per-scan cost backstop (disabled meter from new()).`

- [ ] **Step 2: Add a regression test for the gate-CLI no-backstop path**

In `tests/unit_classifier.rs` (set the env so `build_backend_named("runpod")` succeeds, then assert it constructs without a backstop trip). Use `serial_test` (already a dep) since this manipulates process env:
```rust
#[test]
#[serial_test::serial]
fn build_backend_named_runpod_constructs() {
    std::env::set_var("RUNPOD_ENDPOINT_URL", "https://example.invalid/v2/x");
    std::env::set_var("RUNPOD_API_KEY", "k");
    let c = charcoal::toxicity::classifier::build_backend_named("runpod");
    assert!(c.is_ok(), "named runpod backend should build: {:?}", c.err());
    std::env::remove_var("RUNPOD_ENDPOINT_URL");
    std::env::remove_var("RUNPOD_API_KEY");
}
```
(This asserts construction; the disabled-backstop behavior itself is covered by Task 3's short-circuit test using `new()`'s disabled default.)

- [ ] **Step 3: Run, verify pass**

Run: `cargo test --features web --test unit_classifier`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/toxicity/classifier.rs tests/unit_classifier.rs
git commit -m 'feat(cost-guard): per-scan meter in build_from_env; disabled for gate CLI (#206)'
```

---

### Task 5: Repoint the informational `estimate_cost_cents` rate

**Files:**
- Modify: `src/observability/classifier_metrics.rs`
- Test: `tests/unit_cost_meter.rs` (or the metrics test file if one exists — Step 1)

- [ ] **Step 1: Confirm scope**

Run: `grep -rn "estimate_cost_cents" src/ tests/`
Expectation: no callers in `src/` (dead/informational). If a caller exists, note it — the rate change must not alter that caller's intent (it only changes the constant).

- [ ] **Step 2: Write the failing test**

Append to `tests/unit_cost_meter.rs`:
```rust
use charcoal::observability::classifier_metrics::estimate_cost_cents;
use charcoal::toxicity::cost_meter::DEFAULT_RATE_CENTS_PER_HOUR;

#[test]
fn estimate_cost_uses_shared_default_rate() {
    // 1 hour of busy ms at the default rate == the default hourly cents.
    let one_hour_ms = 3_600_000u32;
    assert_eq!(
        estimate_cost_cents("runpod-cope-b", one_hour_ms),
        DEFAULT_RATE_CENTS_PER_HOUR
    );
    // Non-runpod backends remain 0.
    assert_eq!(estimate_cost_cents("zentropi", one_hour_ms), 0);
}
```

- [ ] **Step 3: Run, verify it fails**

Run: `cargo test --test unit_cost_meter estimate_cost_uses_shared_default_rate`
Expected: FAIL — current constant is $2.72/hr (7.56e-5/ms), so 3.6e6 ms → 272, not 329.

- [ ] **Step 4: Repoint the constant**

In `src/observability/classifier_metrics.rs`, change the RunPod branch of `estimate_cost_cents` to derive from the shared default so informational logs match the guard's default rate:
```rust
pub fn estimate_cost_cents(backend: &str, elapsed_ms: u32) -> u32 {
    // RunPod: busy-time informational estimate at the backstop's default rate
    // (cost_meter::DEFAULT_RATE_CENTS_PER_HOUR). NOTE: this is busy-latency, not
    // worker-uptime — the real guard lives in ScanCostMeter. Non-runpod = 0.
    match backend {
        "runpod-cope-b" => {
            let per_ms =
                crate::toxicity::cost_meter::DEFAULT_RATE_CENTS_PER_HOUR as f64 / 3_600_000.0;
            (elapsed_ms as f64 * per_ms).round() as u32
        }
        _ => 0,
    }
}
```

- [ ] **Step 5: Run, verify pass**

Run: `cargo test --test unit_cost_meter estimate_cost_uses_shared_default_rate`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/observability/classifier_metrics.rs tests/unit_cost_meter.rs
git commit -m 'chore(cost-guard): repoint informational estimate_cost_cents at shared default rate (#206)'
```

---

### Task 6: Full verification

**Files:** none (verification only)

- [ ] **Step 1: Full web suite**

Run: `cargo test --features web`
Expected: PASS — all existing tests plus the new cost-meter / client / factory tests.

- [ ] **Step 2: Clippy across the feature matrix**

Run:
```bash
cargo clippy --features web --all-targets -- -D warnings
cargo clippy --all-targets -- -D warnings
cargo clippy --features postgres --all-targets -- -D warnings
```
Expected: clean (no warnings). Fix any (e.g. needless `Arc` clone, float-cmp lints — the boundary tests use distinct values to avoid float-eq lints).

- [ ] **Step 3: Format**

Run: `cargo fmt --all && git diff --stat`
Commit any formatting, staging the touched files **explicitly by name** (project
rule: never `git add -A`/`-u`/`.`):
```bash
git add src/toxicity/cost_meter.rs src/toxicity/runpod_cope_b.rs \
        src/toxicity/classifier.rs src/observability/classifier_metrics.rs \
        tests/unit_cost_meter.rs tests/unit_classifier.rs
git commit -m 'style(cost-guard): cargo fmt (#206)'
```

- [ ] **Step 4: Sanity-check the spec's defining numbers**

Confirm in code review that: backstop on by default (`from_env` unset → 500); only explicit `0` disables; first call cannot trip; over-ceiling returns `CostCeilingExceeded` and issues no HTTP. These are all covered by the tests above — this step is a final read-through, not new code.

---

## Out of scope (do NOT implement here)

- Loop-level "stop enqueuing"/whole-scan abort optimization → decouple project.
- Warm-window metering → contingent on the decouple's burst shape.
- Any change to classification concurrency, the sweep, or the two-stage ONNX filter.

## Notes for the executor

- The meter is shared (`Arc`) and the client derives `Clone`; cloning the client shares the one meter/clock — correct (all clones of a scan's client meter the same scan).
- `?` on `arm_and_check()` works because `CostCeilingExceeded` implements `std::error::Error` (via `thiserror`), so anyhow converts it. The downcast in the client test confirms the type survives.
- Do not add the backstop to the internal `RunPodError` retry enum — the check is *outside* the backon retry loop (at the top of `classify`), so it is inherently non-retryable. Adding it to `RunPodError` would be wrong (that enum is the retry classifier).
