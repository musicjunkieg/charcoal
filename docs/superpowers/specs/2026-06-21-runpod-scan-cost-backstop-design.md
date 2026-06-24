# RunPod Scan Cost Backstop — Design

- **Date:** 2026-06-21
- **Status:** Draft (brainstorming → spec review)
- **Issue:** chainlink #206 (blocks #196 prod cutover)
- **Branch:** `feat/cope-b-cost-guard`

## Problem

The Phase 6.7 staging gate (grimalkina re-scan, chainlink #195) revealed that a
single onboarding scan cost roughly **$10–20** — it exhausted the RunPod account
balance mid-scan (HTTP 402 Payment Required at ~4h40m in) — against a modelled
target of **< $1**.

Root cause: **RunPod bills GPU worker *uptime*, not classifications.** The H100
worker costs ~$3.29/hr whether it is saturated or idle-but-warm. The current
pipeline interleaves Stage-2 classification with a long, Bluesky-API-bound
follower sweep, so RunPod calls dribble in across the whole multi-hour scan and
the worker (with `idleTimeout=60s`) never scales to zero. We pay for hours of a
warm H100 doing minutes of actual work.

The intended guard, `CHARCOAL_SCAN_COST_CEILING_CENTS`, **does not exist in the
code.** The only cost code is an informational `estimate_cost_cents(backend,
elapsed_ms)` (`src/observability/classifier_metrics.rs:46`) that is logged but
never checked, and it meters *busy latency per call* against a stale $2.72/hr
A100 rate — which structurally undercounts the idle-but-warm uptime that
dominates real cost.

## Goal

Add a **cost backstop**: an in-process, conservative estimate of real RunPod
worker cost for the current scan, and a hard stop that prevents a single scan
from running away into disaster spend.

This is a **disaster brake, not a budget.** It is deliberately generous (~$5
default) so normal scans complete; it exists to prevent the $20+ event we just
hit, not to enforce the < $1 target. The < $1 target is the job of the separate
**cost-reduction** work (decouple collection from a concentrated classification
burst), which is explicitly out of scope here.

### Non-goals (deferred to the cost-reduction / decouple project)

- **Loop-level "stop enqueuing" / early-abort optimization.** Halting the
  amplifier / follower / sweep loops early once capped is only an *efficiency*
  win (post-trip RunPod calls are already free — see Enforcement), and those
  loops are exactly what the decouple rewrites. Building loop hooks now means
  throwing them away. Deferred.
- **"Warm-window" metering.** Elapsed-since-first-call (below) is accurate for
  today's interleaved pipeline and for a single-burst decouple. *If* the
  decouple ends up *waved* (collect → burst → collect → burst), the worker
  sleeps between waves and elapsed would overcount; only then do we switch to
  metering that counts only time within `idleTimeout` of a call. That decision
  depends on the burst shape, which is designed in the decouple project.

## Design

Everything in scope is **architecture-independent** — it is reused unchanged by
the decoupled pipeline. All RunPod traffic flows through one client, so the
enforcement boundary does not move when the loops are rewritten.

### 1. Trigger / scope

The backstop is active **only when the configured classifier backend is
RunPod** (`CHARCOAL_CLASSIFIER=runpod`). It is a no-op for Zentropi, whose
third-party per-call billing has a different cost model and no self-hosted
worker-uptime problem. This keeps the Zentropi path completely untouched.

### 2. `ScanCostMeter`

A small, cloneable handle (`Arc` internally) created once per scan and shared
with the RunPod client. Fields are immutable after construction except the
first-call clock:

- `started_at: OnceLock<Instant>` — the **first-RunPod-call instant**. `OnceLock`
  gives lock-free, set-exactly-once arming and lock-free reads, which is required
  because the `buffer_unordered(8)` loops mean multiple in-flight `classify`
  calls race to arm and read it. Do **not** use `Mutex<Option<Instant>>` or a
  plain field — `OnceLock::get_or_init` is the contract.
- `rate_cents_per_hour: u32` and `ceiling_cents: u32` — immutable, set at
  construction from config.

**Arming order (must be exactly this):** on each `classify`, the client calls the
meter to **arm-then-check**: (1) `get_or_init` the clock (first call sets `t0 =
Instant::now()`; later calls are no-ops), then (2) evaluate the trip predicate.
Because the first-ever call arms with `elapsed = 0`, it can never trip (given a
non-zero ceiling — see §3) — desirable, and a direct consequence of arm-then-check.
A check-then-arm ordering, or arming at construction, would change behavior and
is wrong.

**Disabled fast-path:** if `ceiling_cents == 0`, the meter reports "not over"
without arming or evaluating the predicate (backstop disabled; see §5).

**Estimate + test seam:** the core is `fn estimated_cents_at(&self, now: Instant)
-> u32` = `(now - t0).as_secs_f64() / 3600.0 × rate_cents_per_hour`, returning `0`
before arming. The public `is_over(&self) -> bool` passes `Instant::now()` and
applies the `ceiling == 0` disabled guard + §3 predicate. Tests drive
`estimated_cents_at` / the predicate with an injected `now`, so they never sleep.

This metering is **deliberately conservative**: it assumes the worker stays warm
continuously from the first call onward, which is ~true under today's
interleaved pipeline (calls rarely gap beyond `idleTimeout` during active
scanning) and *exactly* true under a single-burst decouple (no idle gaps). A
backstop should err toward stopping runaway cost, so over-counting on the rare
long idle gap is acceptable and safe.

Time source: `std::time::Instant` (monotonic). No wall-clock, no `Date::now`.

### 3. `over_ceiling` — pure decision function

```rust
/// Pure, I/O-free. Unit-tested in isolation.
fn over_ceiling(elapsed_secs: f64, rate_cents_per_hour: u32, ceiling_cents: u32) -> bool
```

Returns `true` when `elapsed_secs / 3600.0 * rate_cents_per_hour as f64 >=
ceiling_cents as f64`. Pure arithmetic only; it is **always called with
`ceiling_cents > 0`**. The "disabled when ceiling is 0" semantics (§5) are
**not** this function's job — the meter short-circuits `ceiling == 0` to "not
over" *before* calling this predicate (§2 disabled fast-path). This avoids the
trap that `0 >= 0` would otherwise trip immediately. The meter feeds it
`elapsed_secs` from the monotonic clock.

The **trip decision is made solely by this `f64` predicate**. The meter's
`estimated_cents_at -> u32` result is for display/logging only and never gates
the trip, so its integer truncation can never disagree with the boundary.

### 4. Enforcement — at the per-call boundary (the hard guarantee)

Enforcement lives **inside the RunPod client, immediately before issuing each
request** — not at loop boundaries.

- Before each RunPod call, the client consults the meter. If `over_ceiling`, it
  returns a new typed, **non-retryable** error `CostCeilingExceeded` instead of
  calling out.
- This rides **the exact code path the live 402 already exercised** —
  `pipeline::amplification` logs "Failed to score follower, skipping" and
  continues (graceful, no crash, no panic). The 402 (`RunPodError`,
  non-retryable) proved this path in production during the staging gate.

Why per-call rather than per-loop: enforcement granularity must match
cost-accrual granularity. A single account's classification can take 1–2 min,
and the follower/sweep loops run `buffer_unordered(8)`, so loop-boundary checks
can overshoot by `~8 × per-account-cost`. The per-call check is **structural**
(all RunPod traffic flows through the client — it cannot be forgotten in a new
loop) and bounds overshoot to the handful of in-flight calls finishing after the
trip = pennies, independent of how long any loop runs.

Post-trip behaviour on today's pipeline: the scan keeps collecting followers and
ONNX-filtering them, while every RunPod call fast-fails for free (the worker
scales to zero). This is the same degraded-but-free completion the 402
produced — correct and safe, just not yet optimal (the loop early-stop that
makes it *also* stop wasting collection time is the deferred optimization).

### 5. Configuration

- `CHARCOAL_SCAN_COST_CEILING_CENTS` — backstop ceiling in cents. The backstop
  is **on by default**. Resolution is explicit:
  - **unset** → default **500** ($5, enabled).
  - **malformed / non-numeric** → default **500** + a `WARN` (never fail the scan
    over a typo'd cost knob).
  - explicit **`0`** → backstop **disabled** (the only way to turn it off; the
    meter still records for observability).
  - any other positive integer → that exact ceiling.
- `CHARCOAL_GPU_COST_CENTS_PER_HOUR` — GPU rate, default **329** (observed H100
  $3.29/hr; conservatively covers the H200 fallback, which is in the same range).
  Unset or malformed → default 329 + `WARN`.

Note the two distinct paths: *unset* and *malformed* both resolve to the
enabled default (500), whereas only an *explicit `0`* disables — so a
mis-typed value fails safe (backstop stays on), and disabling is always
deliberate. Both vars are parsed once at scan start.

### 6. Observability

- On trip: a `WARN` — `scan cost-capped: est ~$X.XX after Ymin (ceiling $Z)`
  with the scan/user identifiers already in the tracing span. Because up to 8
  concurrent in-flight calls can each observe the trip under
  `buffer_unordered(8)`, emit this **once** via a dedicated `AtomicBool`/`Once`
  on the meter (the same one-shot discipline as arming), so the cap is reported
  a single time rather than up to 8 times. Per-call `CostCeilingExceeded` errors
  still flow normally; only the summary WARN is de-duplicated.
- The stale `estimate_cost_cents` $2.72/hr busy-latency constant
  (`classifier_metrics.rs:46`) is updated for the **RunPod branch only** — it
  reads the same configurable `rate_cents_per_hour` the meter uses, so
  informational per-call logs match the guard's rate. **Non-RunPod branches of
  that function are unchanged** (Zentropi / ONNX keep returning `0` / their
  existing per-call values). The function's signature and call sites are
  otherwise untouched; this is a constant-source change, not a refactor.

## Error handling

- `CostCeilingExceeded` is a new non-retryable variant alongside the existing
  RunPod error taxonomy (`src/toxicity/runpod_cope_b.rs`), surfaced through the
  same `Result` the callers already handle. No new call-site error handling is
  introduced — it reuses the skip-and-continue path.
- Malformed config → default + warn, never a hard failure.
- Meter on a backend that is not RunPod → inert (never constructed / always
  reports disabled).

## Testing

- **`over_ceiling` (pure):** below / at / above the ceiling; zero elapsed. Pin
  the boundary exactly: at `ceiling=500`, `rate=329`, the predicate flips at
  `elapsed_secs ≥ 500 × 3600 / 329 = 5471.13s` (~91.2 min). Assert `false` at
  5471s and `true` at 5472s to lock the `>=` semantics against off-by-one. (Do
  **not** test `ceiling == 0` here — that path never reaches this function.)
- **`ScanCostMeter`:** `estimated_cents_at` returns 0 before arming; the first
  `classify`/arm sets the clock and a 0-elapsed check does not trip; estimate
  grows with injected `now` (no sleeping); `is_over` with `ceiling_cents == 0`
  returns `false` regardless of elapsed (disabled fast-path); the trip-WARN
  fires at most once across repeated over-ceiling observations.
- **Client enforcement:** with a meter already over the ceiling, `classify`
  returns `CostCeilingExceeded` and issues **no** HTTP request (wiremock asserts
  zero hits); the error is non-retryable (no backon retries). With a meter under
  the ceiling, the call proceeds normally.
- No live-GPU dependency; all tests run in the standard `--features web` suite.

## Out of scope (explicit)

- Loop-level stop-enqueuing / whole-scan early abort → cost-reduction / decouple
  project.
- Warm-window metering → contingent on the decouple's burst shape.
- Any change to classification concurrency, the sweep, or the two-stage ONNX
  filter.

## Why this is safe to build before the decouple

The meter, the pure predicate, the per-call enforcement, the typed error, and
the config are all pipeline-agnostic and are inherited verbatim by the decoupled
architecture. The only decouple-dependent decision (metering model) is
explicitly deferred with a defined trigger (waved bursts). Nothing here is
throwaway.
