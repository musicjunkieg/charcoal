# Scan admission: a durable queue with bounded concurrency

**Issue:** #257 (under #256, production go-live readiness)
**Date:** 2026-08-05
**Status:** Design approved, implementation plan pending
**Blocked by:** #182 (Bluesky 429/backoff) — see Dependencies

## Problem

`src/web/scan_job.rs:50` gates scan admission on a single process-wide bool:

```rust
pub fn try_start_scan(&mut self, user_did: &str) -> Result<(), String> {
    if self.any_running {
        // Scans are gated globally, not per-user — the conflict may be
        // another user's scan, so the message shouldn't imply it's theirs.
        return Err("Another scan is already in progress on this server ...")
```

One scan at a time for the entire server. `POST /api/scan` returns **409** to everyone else.

That was the right call for a single-user tool and the comment shows it was deliberate. It does not survive open signup (decided 2026-08-05, #256). A production scan of 595 accounts took 22 minutes; #207 records larger accounts at hours. So the second person to sign up is told to "try again in a few minutes" and may be told that all day. There is no queue behind the refusal — just a closed door.

With one user this is invisible. On day one of a public launch it *is* the product.

**This is not #52.** That issue covers ONNX inference concurrency. This is the admission gate — a different layer. But they are coupled in one direction, which the Constraints section explains.

## Goals

1. No user is ever refused because someone else is scanning.
2. A waiting user sees where they are and roughly how long it will be.
3. Two scans can run at once without exhausting memory or breaching Bluesky's tolerance.
4. A deploy — which happens on every merge — does not lose queued or running work.

## Non-goals

Deliberately excluded to keep this shippable:

- **Per-user quotas and cooldowns** (#258). Adjacent and also a launch blocker, but a separate policy concern. This design leaves a clean seam for it: quota is a check at enqueue time.
- **Notification when your turn arrives.** Polling `GET /api/status` is the existing mechanism and stays.
- **Priority tiers.** FIFO by `enqueued_at`.
- **External worker processes.** Scans stay in-process. The worker-pool question stays parked with the infra-scaling spike.

## Constraints discovered during design

These shaped the design and are recorded because they are not obvious from the code.

### Models load per-scan, so concurrency multiplies memory

`scan_job.rs:292` (toxicity), `:336` (embedder) and `:372` (NLI) each load a fresh model per scan, deliberately — the module doc says *"the scan loads the toxicity scorer and embedder fresh each time it runs, so startup stays fast and the scorer isn't held in memory while idle."*

Since #231 made the NLI export fp32, that is roughly **500 MB of models per concurrent scan** (284 NLI + 126 toxicity + 90 embedding). Production peak RAM is 1.11 GB total.

So the global gate is not only a leftover — it is implicitly protecting memory. Lifting it without addressing model ownership puts ~1 GB of models in RAM before any work happens.

### Concurrent inference is not available, and does not matter here

`ort` 2.0.0-rc.11 takes `&mut self` on both `Session::run` and `Session::run_async`, which is why all three scorers already hold `Arc<Mutex<Session>>`. There is no safe path to parallel inference on one session. `Session` is `unsafe impl Send + Sync`, so ORT's C-level session is genuinely thread-safe and the restriction is the Rust binding being conservative — but working around a binding's safety contract with `unsafe` is not something this design does.

It matters less than it would elsewhere, because **Phase A is dominated by network wait.** #207 breaks the pre-burst window down as amplification ~37m + sweep ~10m + gather ~80m, and gather runs 8 accounts in flight (`scan_job.rs:649` → `gather_concurrency` → `buffer_unordered(8)` at `mod.rs:400`). Per candidate account that is a 25-post Stage-1 `getAuthorFeed` sample (`gather.rs:239`), a 50-post sample if the account does not early-exit (`:262`), and a batched `getPosts` for reply parents — each a cursor-paginated round trip to `public.api.bsky.app`.

**Phase A is not purely I/O, and an earlier draft of this spec wrongly said it was.** `gather.rs:107/141/153/161/177` run the local ONNX clean pass over each account's sample inside the same phase. So gather is fetch *and* inference, and two concurrent scans **will** contend on the shared inference mutex — the contention is real, not theoretical.

The design still holds: the majority of per-account time is the round trip, so throughput should improve meaningfully. But it will be **less than a clean 2×**, and the shape of that curve is unmeasured. This is also why the default is 2 rather than 3 — with a serialized clean pass, each additional concurrent scan buys progressively less.

Sharing one model instance buys the memory headroom; the concurrency that matters happens in the network wait that surrounds the inference.

## Dependencies

### #182 must land first — blocking

`src/constellation/client.rs:22`:

```rust
/// Concurrency for the discovery backlink fetches (#213). Kept low on purpose:
/// Constellation publishes no rate limit and `PublicAtpClient` has no backoff
/// (#182). Do not raise past ~8 until #182 lands.
const DISCOVERY_CONCURRENCY: usize = 8;
```

`PublicAtpClient` still has no 429 or backoff handling. Two concurrent scans mean **16 simultaneous Bluesky requests**, not 8 — the code guards this limit *within* a scan, and concurrency breaches it *across* scans through a door the constant cannot see.

Being rate-limited is not graceful here: with no backoff a 429 surfaces as a fetch failure, and #236 shows failures during gather can cost an entire account.

#182 is therefore a hard dependency, and its `low` priority was set before open signup was on the table. It is small — `backon` is already a dependency.

## Design

Three changes, each small alone.

### 1. Models move to `AppState`, loaded once at boot

The #52 slice this needs, and no more. `OnnxToxicityScorer`, `SentenceEmbedder` and `NliScorer` load in `web::serve()` and live as `Arc<…>` on `AppState`. The three per-scan load sites become clones. Their existing `Arc<Mutex<Session>>` is untouched — this changes ownership and lifetime, not concurrency semantics.

Memory goes from N × 500 MB to 500 MB flat.

**Model load failure is fatal at boot.** Today models load optionally and the scan degrades when one is absent. At boot the server fails to start instead: a server that cannot score is not usefully up, and production already auto-downloads missing models before this point.

**Accepted cost.** This reverses the "not held in memory while idle" decision. Production idles at 0.776 GB today; always-resident models make that ~1.28 GB. Trivial against the 32 GB ceiling, but Railway meters RAM continuously, so it raises the monthly floor even on days nobody scans. Relevant to #188.

### 2. A `scan_queue` table replaces the `any_running` bool

Schema **v11**. Admission becomes a database question — "how many rows are `running`?" — rather than a process-local bool.

```sql
CREATE TABLE scan_queue (
  user_did      TEXT PRIMARY KEY,
  status        TEXT NOT NULL,      -- 'queued' | 'running' | 'done' | 'failed'
  enqueued_at   TIMESTAMPTZ NOT NULL,
  started_at    TIMESTAMPTZ,
  finished_at   TIMESTAMPTZ,
  lease_expires TIMESTAMPTZ,
  last_error    TEXT
);
```

`user_did` as primary key makes enqueue idempotent for free — a double-click cannot double-book, which matters more when strangers are using it.

`lease_expires` is what makes this survive the deploy cadence. A running scan heartbeats; on boot the admitter reclaims any `running` row whose lease has lapsed and returns it to `queued`. Combined with #208's `scan_phase`, the reclaimed scan **resumes** rather than restarting, so nobody re-pays for completed work.

Position is `COUNT(*) WHERE status='queued' AND enqueued_at < mine`. ETA is position × a rolling median of `finished_at - started_at`, which becomes measurable for the first time because those columns now exist.

**The Postgres migration must self-record its version** (`INSERT INTO schema_version … ON CONFLICT DO NOTHING`). The runner does not do it, and a migration that skips it re-runs forever.

### 3. An admitter loop

One background task per process. Wakes on enqueue, on scan completion, and on a 30-second timer as a backstop. While `running_count < CHARCOAL_SCAN_CONCURRENCY`, it claims the oldest queued row, marks it `running` with a fresh lease, and calls `launch_scan` with the `Arc`'d models.

`CHARCOAL_SCAN_CONCURRENCY` defaults to **2**, env-tunable, mirroring the existing `CHARCOAL_BURST_CONCURRENCY` convention.

## Lifecycle

```
POST /api/scan
   └─> UPSERT scan_queue (user_did PK, status='queued')     ← idempotent
       └─> 202 { status: "queued", position: 3, eta_seconds: 4200 }

admitter  (on enqueue | on completion | every 30s)
   └─> while running_count < CHARCOAL_SCAN_CONCURRENCY:
         claim oldest 'queued' → status='running', lease_expires = now + 2min
         └─> launch_scan(...) with Arc'd models from AppState

running scan
   └─> heartbeat: lease_expires = now + 2min, every 30s
   └─> on exit (ok | err | panic): status='done'|'failed', notify admitter

on boot
   └─> reclaim: status='running' AND lease_expires < now  →  'queued'
```

### Dual backend, and its expiry date

Claiming uses `FOR UPDATE SKIP LOCKED` on Postgres — the standard safe-dequeue idiom. SQLite has no equivalent, so its implementation claims inside a write transaction, which is sufficient because SQLite is the single-process local and CLI backend.

**Correction (2026-08-06, Task 4 review).** `SKIP LOCKED` alone does **not** enforce the cap, and an earlier draft of this section wrongly implied it did. The claim reads `COUNT(*) WHERE status='running'` to check the limit, that count takes no lock, and the pool runs at READ COMMITTED — so two admitters both read the pre-claim count, both pass the guard, and both admit. `SKIP LOCKED` guarantees they take *different rows*, which is precisely why it cannot bound the total: it makes the over-admission clean rather than preventing it. Reproduced against a live instance at cap 1 — both sessions saw `running_seen = 0` and `running_after` ended at 2.

The cap is therefore enforced by a **transaction-level advisory lock** (`pg_advisory_xact_lock`) taken before the count, which serializes admitters against each other and nothing else. `SKIP LOCKED` stays — redundant but harmless. `SERIALIZABLE` would also work but pushes retry handling onto every caller.

**#263 will delete the SQLite implementation.** Bryan's decision (2026-08-05): *"at this point we should just rip out SQLite — we're never going back."* This design deliberately does **not** block on that. #257 is a launch blocker; #263 is cleanup, and it forces every contributor to run Postgres locally. The SQLite queue implementation here is intentionally minimal — no `SKIP LOCKED`, no multi-consumer concerns — because it is expected to be deleted rather than maintained.

### A property worth naming

Because admission is arbitrated by the database rather than a process-local bool, this design is **correct with more than one replica** — but only with the advisory lock above. Without it the property is claimed and not held, which is exactly the trap Task 4 fell into. That correctness is not a goal in itself; it means scaling out later does not require redesigning admission.

It also is not only a multi-replica concern. A single replica over-admits too, as soon as two callers claim concurrently — the admitter loop and any start-now path — because sqlx hands each task its own pooled connection.

## Error handling

| Failure | Behaviour |
|---|---|
| Scan returns `Err` | `status='failed'`, `last_error` recorded, slot released |
| Scan panics | existing `AssertUnwindSafe` catch in `launch_scan` → as above |
| Server killed mid-scan | lease lapses → re-queued on boot → resumes via `scan_phase` |
| Model load fails at boot | fail fast; the server does not start |
| DB unreachable at enqueue | `503`; never a silent drop |
| User double-clicks Scan | PK conflict → returns current position, no second row |

**The invariant the implementation must not violate: every exit path releases the slot.** The lease is the backstop for paths that cannot run cleanup — a SIGKILL, an OOM, a Railway redeploy.

## API and UX

`POST /api/scan` stops returning "server busy".

**Correction (2026-08-06, Task 6).** An earlier draft of this line said *"`409` survives only for 'you already have a scan queued or running'"*, which contradicted two other statements in this same spec: the idempotency rationale above ("a double-click cannot double-book") and the error table's *"PK conflict → returns current position, no second row"*. The implemented behavior follows those two: **the endpoint never returns 409.** A repeat request from a user who is already queued or running is idempotent — `202` with their current position and ETA. That is both friendlier for the double-click case the table describes and the only reading consistent with `user_did` being the primary key.

`GET /api/status` gains a queue block, and `WebScanPhase` gains a `Queued` variant:

```json
{
  "phase": "queued",
  "queue": { "position": 3, "eta_seconds": 4200, "enqueued_at": "2026-08-05T18:22:00Z" }
}
```

`ScanProgress.svelte` then shows **"You're 3rd in line — about 70 minutes"** where it currently shows an error toast. Whoever implements this will be editing that component, which is also where #248 (`prefers-reduced-motion`) and #249 (contrast) land — worth doing together.

## Testing

Following the project's TDD mandate: tests first, and a test that cannot fail is not a test.

**Unit**
- admission arithmetic (`running_count < limit`)
- position calculation across queued rows
- ETA from rolling median duration
- lease-expiry predicate

**Postgres integration** (production runs Postgres; a SQLite-only test proves nothing about the deployed path)
- concurrent enqueue never admits past the limit — and this test must be
  **genuinely concurrent** (`tokio::join!` / `JoinSet` of N > cap claims,
  each on its own pooled connection), then assert
  `COUNT(*) WHERE status='running' <= cap`. Sequential `.await`s on one
  connection never contend a row, so `SKIP LOCKED` never executes and the
  test passes against an implementation that over-admits. This is not
  hypothetical: the Task 4 cap test was written that way and missed the
  defect above. Write it, watch it fail, then fix.
- idempotent enqueue leaves exactly one row
- boot reclaim moves expired `running` rows to `queued`
- a reclaimed scan resumes from `scan_phase` rather than restarting

**Regression for the actual bug**
- two users, second is **queued** rather than refused. This test must fail against today's `any_running` gate — verify that before trusting it.

**Negative control**
- with the cap set to 1, assert a second scan does *not* start; with the cap at 2, assert it does. A queue test that passes at every cap value is measuring nothing.

## Measurement task (do this first)

The concurrency default rests on an estimate, not data. Before tuning it, instrument Phase A to record the split it depends on.

**Instrument `gather_account` to emit, per account:**

- `fetch_ms` — time in `fetch_sample` / `fetch_parent_posts` (the `getAuthorFeed` and `getPosts` round trips)
- `clean_pass_ms` — time in `onnx_clean_pass`
- `total_ms`

Aggregate per scan and log once at Phase A completion, alongside the existing phase banner. This follows #61's precedent of a latency split (RunPod `delayTime` vs `executionTime`), which is what made the burst phase tunable.

**What it answers.** If `clean_pass_ms` is under ~10% of `total_ms`, concurrency 2 should approach 2× and 3 is worth trying. If it is 30%+, the shared mutex is the ceiling and raising concurrency past 2 buys little — in which case the real lever is a session pool for the toxicity model specifically, or moving the clean pass out of gather.

**Do it before the queue lands, not after.** It is a handful of `Instant::now()` calls and one log line, it needs no queue, and it can run on the existing single-flight path — so the number is available when the concurrency default is chosen rather than being guessed and revisited. Tracked as #264.

## Open questions

None blocking. Three settled during design and recorded here so they are not silently revisited:

- **Fail-fast on model load** rather than degrade — approved 2026-08-05.
- **Default concurrency 2**, not 3 — conservative given the memory floor, the Bluesky pressure #182 addresses, and the unmeasured mutex contention above. Revisit once #264 reports the fetch/inference split.
- **Phase A is a mixed workload**, not pure I/O. Corrected 2026-08-05 after tracing the call sites; the original claim came from taking #207's title at face value.
