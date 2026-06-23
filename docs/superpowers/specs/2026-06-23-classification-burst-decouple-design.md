# Classification Burst Decouple — Design Spec

**Issue:** chainlink #208 (blocks/enables the cost-guard goal in #206; related latency follow-on #207)
**Status:** Design — awaiting spec review + user approval
**Date:** 2026-06-23

## Problem

A full onboarding scan (~5,300 accounts, ~6h wall-clock in the Phase 6.7 staging
gate) calls the RunPod CoPE-B classifier **inline, per account**, interleaved
with slow Bluesky API fetches. RunPod bills GPU **worker uptime**, not
per-classification, and the `ScanCostMeter` (chainlink #206) meters wall-clock
from the first classification call onward. Because those calls are dribbled
across the entire multi-hour scan, the meter's "elapsed × rate" estimate keys
off the whole ~6h span instead of the few minutes of *actual* GPU busy-time
(~$0.08 modeled). The result: the cost ceiling overcounts and cannot be set to a
value that reflects real spend without also capping legitimate long scans.

The fix is structural: **concentrate all classification into one contiguous
burst** so the meter's wall-clock assumption becomes true by construction, the
GPU is used efficiently (high-concurrency batching, fewer cold starts), and the
work is crash-resilient.

## Goals

- Make the existing `ScanCostMeter` (#206) **honest with zero meter-code
  changes** — by ensuring RunPod calls happen in one tight, contiguous window.
- Improve GPU efficiency: classify at high concurrency in a dedicated burst
  rather than 4-wide interleaved across the scan.
- Make a scan **crash-/402-resumable** via a durable, DB-staged work queue.
- Make **re-scans cheap** by skipping fresh accounts before any fetch.

## Non-Goals (explicit)

- **Onboarding wall-clock reduction** (chainlink #207). The burst fixes cost
  *accounting*, not total scan time — that time is dominated by Phase A Bluesky
  I/O, not GPU calls. Parallelizing Phase A fetches, progressive/streamed
  results, etc. are deferred to #207. This design keeps the phase boundaries
  clean so that work is a natural follow-on, not blocked.
- Any change to the meter's *accounting* model (unnecessary — the burst makes it
  correct).
- Any change to scoring formulas, thresholds, the two-stage ONNX filter, or UI.

## Architecture

Classification funnels through a single chokepoint today: both scan paths — the
**sweep** (`src/pipeline/sweep.rs`, followers + second-degree) and
**amplification** (`src/pipeline/amplification.rs`, quote/repost amplifiers) —
score accounts via `src/scoring/profile.rs::score_account`, the only function
that calls the classifier (`classify_batch_with_contexts`, profile.rs:247). The
restructure splits `score_account` along the classification seam into three
phases:

```
┌─ PHASE A · GATHER (I/O-bound, NO RunPod) ──────────────────┐
│  entry gate: skip accounts where is_score_stale == false    │
│              (re-scans skip fetch + classify entirely)      │
│  per surviving account (sweep + amplification):             │
│    fetch sample posts + parent posts                        │
│    run local ONNX Stage-1 clean-pass filter (free)          │
│    enqueue per-post rows → classification_queue             │
│      · ONNX-clean post → status = done (clean verdict)      │
│      · ONNX survivor   → status = pending                   │
│    stash per-account scoring inputs → scan_account_input    │
└─────────────────────────────────────────────────────────────┘
                          ↓   (all network I/O finished)
┌─ PHASE B · BURST (the ONLY RunPod window) ─────────────────┐
│  loop:                                                       │
│    batch = fetch_pending_classifications(user, BURST_BATCH)  │
│    if empty break                                            │
│    verdicts = batch.classify via buffer_unordered(CONC)     │
│    record_classification_verdicts(user, verdicts)           │
│  → worker warms once, stays hot → meter wall-clock ≈ cost   │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─ PHASE C · FINALIZE (local compute, no re-fetch) ──────────┐
│  per account whose rows are ALL done:                       │
│    read verdicts + stashed input                            │
│    reply-weighted tox rate → behavioral → context/NLI       │
│      → graph-distance → tier                                │
│    upsert_account_score  (incremental, per account)         │
└─────────────────────────────────────────────────────────────┘
```

**Properties:**

- **Backend-agnostic** — Phase B bursts regardless of `runpod`/`zentropi`; it
  just *happens* to make the RunPod meter honest and helps Zentropi throughput.
- **Crash-resilient** — the queue is the durable checkpoint.
- **#207-ready** — Phase A is a clean, self-contained I/O pass.

**Main cost:** `score_account` splits into a gather half and a finalize half,
and both call sites (sweep + amplification) restructure from "stream-and-score"
into "gather-all → burst → finalize-all."

## Data model (schema v9)

Two new tables, behind the `Database` trait (SQLite via `src/db/schema.rs`,
Postgres via a new numbered file in `migrations/postgres/`).

### `classification_queue` — flat, per-post (drives the burst)

| column | purpose |
|---|---|
| `user_did TEXT` | protected-user isolation (matches every other table) |
| `account_did TEXT` | which scanned account this post belongs to |
| `post_uri TEXT` | the post being classified |
| `text TEXT` | the envelope text sent to the classifier |
| `context_text TEXT NULL` | parent text for replies (`[Parent]/[Reply]` envelope) |
| `post_kind TEXT` | `original` / `reply` / `quote` (for reply-weighting in Phase C) |
| `onnx_score REAL` | Stage-1 clean-pass score (evidence sorting + audit) |
| `status TEXT` | `pending` (needs RunPod) / `done` (clean *or* classified) |
| `toxic_token INTEGER NULL` | verdict, filled by Phase B (or pre-filled clean in A) |
| `confidence REAL NULL` | verdict confidence |
| `model_id TEXT NULL`, `policy_version TEXT NULL` | verdict provenance |

**Primary key `(user_did, account_did, post_uri)`** → enqueue is an upsert,
making Phase A idempotent (crash-restart re-walks and re-upserts the same rows,
no duplicates). Index on `(user_did, status)` for the burst's pending scan.

### `scan_account_input` — per-account blob (drives Phase C)

`(user_did, account_did)` PK + a serialized JSON payload of everything Phase C
needs and would otherwise re-fetch (engagement metrics, reply/quote structure,
embeddings, parent texts, graph-distance inputs). JSONB on Postgres, TEXT-JSON
on SQLite. **Stash, not re-fetch** — re-fetching reintroduces the Bluesky I/O
cost (#207) and makes Phase C non-deterministic (the feed changes between A and
C).

### `scan_state.phase` (new column)

`gather` / `burst` / `finalize` / `done` marker so resume can skip
already-finished phases instead of re-walking.

### Lifecycle (resume without a scan_id)

- **Fresh scan:** no staging rows → Phase A populates both tables.
- **Resume after crash/402:** rows exist → idempotent re-entry; the `phase`
  marker skips finished phases.
- **Clean finish:** delete the user's rows in both staging tables.

## `Database` trait surface

~10 new methods, batch-oriented (the burst writes thousands of verdicts and
SQLite serializes on a `Mutex`). All land in `SqliteDatabase`, `PgDatabase`, and
any test double.

**Phase A:**
- `enqueue_classifications(user_did, &[QueueRow])` — batch upsert per-post rows.
- `stash_account_input(user_did, account_did, &AccountInput)` — upsert blob.
- `set_scan_phase(user_did, phase)` — advance the `scan_state.phase` marker.

**Phase B:**
- `fetch_pending_classifications(user_did, limit)` — pull a `pending` batch.
- `record_classification_verdicts(user_did, &[Verdict])` — batch mark done + store.

**Phase C:**
- `list_scan_accounts(user_did)` — distinct accounts staged this scan.
- `fetch_account_verdicts(user_did, account_did)` — all rows for one account.
- `fetch_account_input(user_did, account_did)` — the stashed blob.

**Lifecycle / observability:**
- `get_scan_phase(user_did)` — resume entry point.
- `count_pending_classifications(user_did)` — burst progress.
- `clear_scan_staging(user_did)` — delete both tables' rows (clean finish / fresh start).

The pipeline code only ever touches `Arc<dyn Database>`, so the phase functions
stay backend-agnostic.

## Execution mechanics

**Phase A entry gate (re-scan win):** `is_score_stale(user_did, did,
RESCAN_WINDOW_DAYS)` *before* fetching — fresh accounts are skipped entirely (no
fetch, no enqueue). Reuses the existing `account_scores.scored_at` +
`idx_scores_age` index; no new schema. (Today's sweep already gates scoring on a
7-day staleness window at `sweep.rs:110`; this moves the gate ahead of the
fetch.)

**Phase B burst loop** (see Architecture). `BURST_CONCURRENCY` is a new knob,
higher for the dedicated burst than today's interleaved `ZENTROPI_CONCURRENCY=4`
(H100 handles ~32; Zentropi stays conservative). One tight loop → worker warms
once → the #206 `ScanCostMeter` rides along **unchanged** and now meters an
honest, contiguous window.

## Error handling — graceful + resumable (reuses #206's path)

- `CostCeilingExceeded` (or a live RunPod 402) is non-retryable → the in-flight
  row stays `pending`, the burst stops, `scan_state.phase` stays `burst`, the
  scan is marked *degraded*. A later run resumes from the leftover `pending`
  rows. The cost ceiling thus doubles as a clean resume boundary.
- A per-row classify error leaves that row `pending` (not fatal to the scan).
- **Phase C finalizes only accounts whose rows are all `done`;** accounts with
  leftover `pending` rows are skipped this run and completed on resume.
  Incremental `upsert_account_score` per account preserves today's
  crash-resilience.

## Testing

- **Unit (per phase, fake `Database` + `StubClassifier`):** enqueue idempotency
  (double-gather → no dups), staleness skip, burst resume (`pending`
  re-select), ceiling-trip leaves rows `pending`, Phase C aggregation.
- **Golden / behavior-preserving:** Phase C output must match today's
  `score_account` result for the same inputs (the restructure must not change
  scores).
- **End-to-end:** one small scan across all three phases with the stub.
- **Postgres:** the new trait methods covered in the `--features postgres` suite.
- Full suite green under `cargo test --features web`; clippy clean across
  `--features web` / default / `--features postgres`.

## Observability

Phase-transition banners; burst progress via `count_pending_classifications`;
reuse the existing `scan_cost_*` / classifier metrics.

## Rollout

Schema v9 auto-migrates on `db::open()` (existing migration path). The change is
internal to the scan pipeline — no API/UI change. Validated by a staging gate
re-run (the same vehicle as #195/#196): confirm backend=runpod-cope-b, the burst
window is contiguous, the meter estimate tracks real spend, and re-scans skip
fresh accounts.
