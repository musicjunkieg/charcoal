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
score accounts via `src/scoring/profile.rs::build_profile` (profile.rs:38), the
only function that reaches the RunPod classifier (via
`classify_batch_with_contexts`, profile.rs:247 →
`classify_batch`→`classify_post`→`self.classifier.classify`). The restructure
splits `build_profile` along the classification seam into three phases.

Two pre-existing structures in the amplification path are *not* RunPod calls and
must be placed deliberately (see "Amplification path specifics" below): the
event-recording loop (`amplification.rs` ~77–194: `scorer.score_with_context`,
which is **ONNX-only** — verified at ensemble.rs:216 it delegates to
`self.primary`, never the RunPod classifier — plus `nli.score_pair` and
`insert_amplification_event`), and the adaptive **two-pass `build_profile`**
(amplification.rs:355+: pass 1 without NLI, pass 2 with NLI only if pass-1
`raw_score >= 8.0`).

```
┌─ PHASE A · GATHER (I/O-bound, NO RunPod) ──────────────────┐
│  entry gate: skip accounts already scored within window     │
│              (is_score_stale / get_all_scored_dids)         │
│  per surviving account — the EXISTING 2-stage sampler:      │
│   Stage 1 (25 posts): account-level ONNX + TF-IDF overlap   │
│     · <5 posts         → finalize "Insufficient Data" (write)│
│     · clean+irrelevant → finalize "Low" / conf=low   (write)│
│     · else             → Stage 2                            │
│   Stage 2 (50 posts): per-post ONNX clean-pass              │
│     · clean post → enqueue row status=done (clean verdict)  │
│     · survivor   → enqueue row status=pending               │
│     stash per-account inputs → scan_account_input           │
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

### Phase A internalizes the two-stage adaptive sampler

`build_profile` is **not** "fetch → classify → finalize." It contains an
account-level adaptive sampler (profile.rs:55–187) that Phase A must reproduce
exactly, or the restructure silently changes both cost and outputs:

- **Stage 1 (25 posts):** fetch 25 posts, run ONNX on originals+quotes (replies
  excluded — unreliable without parent context) and compute TF-IDF topic
  overlap. Two terminal outcomes here **never call the classifier**:
  - `total_posts < 5` → `AccountScore` tier **"Insufficient Data"**.
  - `should_early_exit_stage1` (all ONNX clean AND overlap below
    `overlap_gate_threshold`) → tier **"Low"**, `toxicity_score=0`,
    `scoring_confidence="low"`. This catches ~50–60% of sweep accounts.
- **Stage 2 (50 posts):** only accounts that survive Stage 1 fetch the larger
  sample and run the full pipeline — and *this* is where the per-post ONNX
  clean-pass (`ONNX_CLEAN_THRESHOLD`, ensemble.rs:120) and the RunPod classifier
  live.

**Mapping to phases:** Phase A runs the whole sampler. The two Stage-1 terminal
outcomes (Insufficient Data, early-exit Low) need no classification, so Phase A
**finalizes them directly** via `upsert_account_score` and does *not* enqueue
them. Only **Stage-2 survivors** enqueue per-post rows + stash account inputs,
and only they are finalized in Phase C. (So "Phase C writes all scores" is
refined: Phase A writes the no-classification terminal outcomes; Phase C writes
the survivors once their verdicts land.) Note the two account-level ONNX uses are
distinct: the Stage-1 *early-exit* check (25-post, originals+quotes solo) vs. the
Stage-2 *per-post* clean-pass that splits `done`/`pending` — Phase A must
preserve both, not collapse them.

**Properties:**

- **Backend-agnostic** — Phase B bursts regardless of `runpod`/`zentropi`; it
  just *happens* to make the RunPod meter honest and helps Zentropi throughput.
- **Crash-resilient** — the queue is the durable checkpoint.
- **#207-ready** — Phase A is a clean, self-contained I/O pass.

**Main cost:** `build_profile` splits into a gather half and a finalize half,
and both call sites (sweep + amplification) restructure from "stream-and-score"
into "gather-all → burst → finalize-all."

### Amplification path specifics

The amplification path has two structures the sweep path does not; both are
handled explicitly so the burst window stays the sole RunPod window:

1. **Event-recording loop (Phase A).** `amplification.rs` ~77–194 records each
   amplification event: `scorer.score_with_context` (ONNX-only — does *not* hit
   RunPod), `nli.score_pair` (local ONNX), and `insert_amplification_event`.
   This is pre-classification, I/O- and local-compute-only, so it folds into
   **Phase A** unchanged — it neither calls RunPod nor breaks the contiguous
   burst. (The per-event `score_with_context` / `insert_amplification_event`
   work is orthogonal to the per-account `build_profile` classification that
   defers to the burst.)

2. **Adaptive two-pass `build_profile` collapses into Phase C.** Today
   (amplification.rs:355+) the follower path runs `build_profile` *twice*: pass 1
   without NLI, then pass 2 *with* NLI only if pass-1 `raw_score >= 8.0`. That
   structure exists because the toxicity score must be known before deciding
   whether the (more expensive, **local**) NLI pass is worth running. In the
   phased model the score is not known until verdicts land — so the gate cannot
   fire in Phase A. Resolution: **the two-pass logic moves wholesale into Phase
   C**, where verdicts exist. Phase C aggregates the toxicity rate → computes
   `raw_score` → if `>= 8.0`, runs the local NLI pass → final tier. NLI is local
   ONNX (no RunPod cost), so running it in Phase C is free of the burst concern.
   Net effect: classification happens **once** (in the burst) instead of twice,
   and the `>= 8.0` NLI gate is preserved exactly. The golden test (see Testing)
   guards that final scores are unchanged. Phase A must stash whatever the NLI
   pass needs in `scan_account_input` so Phase C runs NLI without re-fetching.
   Note pass 2 differs from pass 1 by **two** arguments, not one
   (amplification.rs:414): the NLI scorer + inferred pairs (`nli_ref`,
   `ppwe_ref`) **and** `data_dir` (NLI audit logging). Phase C's NLI pass must
   pass `data_dir` too, or audit-log output regresses.

## Data model (schema v9)

The v8→v9 migration adds **two new tables** (`classification_queue`,
`scan_account_input`). The phase marker is **not** a new column — it reuses the
existing **key/value `scan_state` table** (`(user_did, key, value)`, PK
`(user_did, key)`) under `key='scan_phase'`, via the existing
`set_scan_state`/`get_scan_state` trait methods. Behind the `Database` trait
(SQLite via `src/db/schema.rs`, Postgres via a new numbered file in
`migrations/postgres/` — current max is `0008_fingerprint_scoring.sql`, so
`0009_*`).

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
needs and would otherwise re-fetch. This must cover **every** `build_profile`
Stage-2 argument that isn't a verdict, specifically: the 50-post sample
(originals/replies/quotes + reply/quote structure), parent texts, embeddings,
graph-distance, **`median_engagement`**, **`pile_on_dids` membership** (the
behavioral gate, profile.rs:329–330), and for amplifiers the **`direct_pairs`**
(amplification.rs:228–241) the context score needs. JSONB on Postgres, TEXT-JSON
on SQLite. **Stash, not re-fetch** — re-fetching reintroduces the Bluesky I/O
cost (#207) and makes Phase C non-deterministic (the feed changes between A and
C).

**Forward-compat (deploy-mid-scan safety).** The blob carries a
`schema_version` tag. Because the whole point is crash/deploy resilience, a
deploy can land between Phase A (write) and Phase C (read) with a changed
payload shape. Policy: **on deserialize failure or a version mismatch, discard
the stale staging rows for that account and re-gather it** (drop its
`scan_account_input` + `classification_queue` rows, re-run Phase A for that
account). Re-gather is the safe fallback — correctness over the saved I/O for
the rare straddling-deploy case. A bumped `schema_version` is therefore part of
any change to the blob's shape (called out in the plan's checklist).

### Phase marker (existing `scan_state` key/value, not a new column)

A `scan_state` row `key='scan_phase'`, value one of `gather` / `burst` /
`finalize` / `done`, so resume can skip already-finished phases instead of
re-walking. `scan_state` is a key/value table, so this needs **no schema change**
and **no new trait methods** — `set_scan_state(user_did, "scan_phase", v)` /
`get_scan_state(user_did, "scan_phase")` already exist.

### Lifecycle (resume without a scan_id)

- **Fresh scan:** no staging rows → Phase A populates both tables.
- **Resume after crash/402:** rows exist → idempotent re-entry; the `phase`
  marker skips finished phases.
- **Clean finish:** delete the user's rows in both staging tables.

**Required invariant: at most one in-flight scan per `user_did`.** The "resume
without a scan_id" design depends on this — the staging tables are keyed by
`user_did` with no run identifier, so two concurrent scans for the same user
would corrupt each other's queue / blob / phase marker. This invariant is
**already enforced — more strongly than required** — by `ScanJobManager`:
`try_start_scan` (`src/web/scan_job.rs:50`) trips on a **global** `any_running`
flag, allowing **one scan at a time across all users** (not just per-user), and
returns an error the web/admin scan-launch handlers surface as HTTP 409.
(`is_scan_running_for` at scan_job.rs:87 carries a now-stale `#[allow(dead_code)]`
— it is actually called at `src/web/handlers/admin.rs:275`, which only blocks
*user deletion* during a scan, not scan launch — so it is not the launch gate;
if the plan touches that fn it should drop the stale attribute.) The CLI is
single-operator and inherits the invariant by usage.
Note this means a *single* scan, even one that internally runs both Mode 1
(amplification) and Mode 2 (sweep), shares one staging set for the user — which
is correct (they're one logical onboarding). The spec assumes Mode 1 and Mode 2
run **sequentially within one scan**, not as two concurrently-launched scans;
the plan must preserve that. As a defensive backstop, a fresh scan that finds a
stale phase marker but is *not* resuming calls `clear_scan_staging` first.

## `Database` trait surface

~10 new methods, batch-oriented (the burst writes thousands of verdicts and
SQLite serializes on a `Mutex`). All land in `SqliteDatabase`, `PgDatabase`, and
any test double.

**Phase A:**
- `enqueue_classifications(user_did, &[QueueRow])` — batch upsert per-post rows.
- `stash_account_input(user_did, account_did, &AccountInput)` — upsert blob.
- (phase marker uses the existing `set_scan_state(user_did, "scan_phase", …)`)

**Phase B:**
- `fetch_pending_classifications(user_did, limit)` — pull a `pending` batch.
- `record_classification_verdicts(user_did, &[Verdict])` — batch mark done + store.

**Phase C:**
- `list_scan_accounts(user_did)` — distinct accounts staged this scan.
- `fetch_account_verdicts(user_did, account_did)` — all rows for one account.
- `fetch_account_input(user_did, account_did)` — the stashed blob.

**Lifecycle / observability:**
- (resume entry point reads the existing `get_scan_state(user_did, "scan_phase")`)
- `count_pending_classifications(user_did)` — burst progress.
- `clear_scan_staging(user_did)` — delete both tables' rows (clean finish / fresh start).

The pipeline code only ever touches `Arc<dyn Database>`, so the phase functions
stay backend-agnostic.

## Execution mechanics

**Phase A entry gate (re-scan win):** skip fresh accounts *before* fetching (no
fetch, no enqueue), reusing the existing `account_scores.scored_at` +
`idx_scores_age` index; no new schema. The exact skip predicate differs by
entry point, matching each path's existing dedup:

- **`sweep::run`** (graph-walk) already gates scoring on `is_score_stale(user_did,
  did, 7)` at `sweep.rs:110` — move that gate ahead of the fetch.
- **`sweep::run_topic_first`** dedups via `get_all_scored_dids` (`sweep.rs:204`),
  not `is_score_stale`. The entry gate there reuses that same dedup set (a freshly
  scored DID is skipped before fetch). The plan must apply the gate to **both**
  sweep variants, not just `run`.

Either way the principle is identical: an account already scored within the
window is skipped before any Bluesky I/O.

**Phase B burst loop** (see Architecture). `BURST_CONCURRENCY` is a new knob,
higher for the dedicated burst than today's interleaved `ZENTROPI_CONCURRENCY=4`
(H100 handles ~32; Zentropi stays conservative). One tight loop → worker warms
once → the #206 `ScanCostMeter` rides along **unchanged** and now meters an
honest, contiguous window.

*Verified against the meter's arming semantics:* `ScanCostMeter::arm_and_check`
arms `started_at` (a `OnceLock`) on the first classification call and meters
elapsed from there (cost_meter.rs). Concentrating all RunPod calls into Phase B
makes `started_at` fire at burst start, so the metered wall-clock equals the
burst window — no meter-code change required, only the call-site restructure.

## Error handling — graceful + resumable (reuses #206's path)

- `CostCeilingExceeded` (or a live RunPod 402) is non-retryable → the burst
  stops accepting new work. Because Phase B classifies a batch via
  `buffer_unordered(BURST_CONCURRENCY)`, up to `BURST_CONCURRENCY` calls may be
  in flight when the ceiling trips; any row whose verdict was not recorded stays
  `pending` (verdicts are written only on success). `scan_state.phase` stays
  `burst` and the scan is marked *degraded*. A later run resumes from the
  leftover `pending` rows. The cost ceiling thus doubles as a clean resume
  boundary.
- A per-row classify error leaves that row `pending` (not fatal to the scan).
- **Phase C finalizes only accounts whose rows are all `done`;** accounts with
  leftover `pending` rows are skipped this run and completed on resume.
  Incremental `upsert_account_score` per account preserves today's
  crash-resilience.

## Testing

- **Unit (per phase, fake `Database` + `StubClassifier`):** enqueue idempotency
  (double-gather → no dups), staleness skip, burst resume (`pending`
  re-select), ceiling-trip leaves rows `pending`, Phase C aggregation.
- **Golden / behavior-preserving (write FIRST, TDD):** capture today's
  `build_profile` output across representative accounts — including each Stage-1
  terminal outcome (Insufficient Data, early-exit Low/conf=low) and the
  amplification two-pass NLI gate (`raw_score >= 8.0`) — and assert the phased
  pipeline reproduces them exactly. This golden test must exist *before* the
  restructure, so behavior preservation is enforced by construction, not checked
  after the fact. The plan's **first chunk** is reproducing the two-stage
  adaptive sampler in Phase A under this test.
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
