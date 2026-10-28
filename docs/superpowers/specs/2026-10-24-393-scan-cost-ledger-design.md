# Scan cost ledger + alpha spend guardrails (#393, #389, #385)

Status: approved in conversation 2026-10-24; awaiting spec review.

## Why

Alpha onboarding is the priority, and every new user drives paid RunPod GPU
work. Two questions have no answer today:

1. **Would we notice a runaway?** RunPod auto-pay is on, so nothing bounds
   spend. The 2026-10-02 incident billed $193 over 16 days before anyone saw it.
2. **What does a user cost?** `ScanCostMeter` integrates GPU worker-seconds for
   every scan, uses it only to trip the $5 per-scan ceiling, and throws the
   number away. Bryan's target — onboarding a new user under $2.00 — cannot be
   checked.

Context measured 2026-10-24: the last 21 days billed $8.44 on 6 days, none
since 10-16. Endpoint `f39lf61pc762a8` runs up to 10 workers, 60 s idle
timeout, ~$3.29/worker-hour — a worst-case burn near $33/hour. Alpha budget
(Bryan): ~$100/month is fine.

## Decision

Option C of three (A: console only; B: in-app billing-API watcher; C: console +
in-app cost ledger). B was rejected because it needs a billing-scoped RunPod
key stored in Railway — the place that leaked on 2026-10-21. The prod key is
deliberately endpoint-only.

## Part 1 — RunPod console (Bryan, no code)

- Turn **auto-pay off**; keep the balance around **$60**. The balance becomes
  the hard ceiling on any runaway.
- Enable the **low-balance alert at $25**. The email then means "~$35 spent
  since the last top-up" — about ten days at budget pace. Earlier than that
  is the alarm.
- Accepted tradeoff: a manual top-up every 2–3 weeks. If the balance hits $0,
  Stage-2 calls fail and the existing backoff/circuit breaker (#387) stops
  them retrying at full rate; scans fail rather than spend.

## Part 2 — Scan cost ledger (code)

### Data: new table `scan_costs` (schema v20, both backends)

One row per scan run — a first/full scan or a per-user refresh — whether it
succeeded or failed.

| column | type | meaning |
|---|---|---|
| `id` | integer / bigserial PK | |
| `user_did` | text, not null | whose scan |
| `kind` | text, not null, `'full'` or `'refresh'` | |
| `outcome` | text, not null, `'completed'`, `'paused'` or `'failed'` | `paused` = returned without error but did not finish (cost ceiling or interruption); the resumed run writes its own row. |
| `started_at` | text (RFC 3339), not null | |
| `finished_at` | text (RFC 3339), not null | |
| `classifications` | integer, not null | Stage-2 classifier calls that reached the backend (cache misses) |
| `classifier_cache_hits` | integer, not null | verdicts served from the cache |
| `metered_worker_secs` | real / double, not null | the meter's worker-seconds integral |
| `peak_in_flight` | integer, not null | most concurrent in-flight requests seen (capped at workers max) |
| `est_cents` | integer, not null | `metered_worker_secs` × rate, rounded to the nearest cent (truncating would record every small refresh as 0) |
| `rate_cents_per_hour` | integer, not null | rate used, so old rows stay interpretable if it changes |

Index on `(user_did, started_at)` and on `started_at`.

Postgres migration `migrations/postgres/0020_scan_costs.sql` must self-record
version 20 (`INSERT INTO schema_version … ON CONFLICT DO NOTHING`). SQLite
gets the matching `migrate_v20` in `src/db/schema.rs`.

**Retention:** rows are tiny (one per scan). Kept indefinitely for now; user
deletion (`delete_user_data`, both backends) must delete the user's rows.

### Why a table, not `scan_state`

`scan_state` is a per-user key/value store that keeps only the latest value.
Per-user totals and day-by-day comparison with RunPod billing need history.

### Getting the number out

Today `build_from_env()` creates the meter inside the RunPod client, and the
client is then wrapped in `CachedClassifier`, so nothing can read the meter
afterwards.

- `ScanCostMeter` gains `peak_in_flight()` (tracked in `Acc::enter`) and a
  `snapshot()` returning `{ worker_secs, peak_in_flight, est_cents,
  rate_cents_per_hour }`. The existing trip logic is unchanged.
- `build_from_env()` is split: `build_from_env_with_meter(meter)` takes the
  meter; `build_from_env()` keeps its behaviour by creating one. `None` meter
  for non-RunPod backends (Zentropi bills per request, not per GPU-second —
  its rows record `0` worker-seconds and `0` cents).
- `build_scan_scorers` creates the meter, passes it in, and returns it in
  `ScanScorers` beside the existing `classifier_stats`.
- At scan end — **on both success and failure paths** — the scan job and the
  refresh path call one helper, `record_scan_cost(db, user_did, kind,
  outcome, started_at, &meter, &classifier_stats)`, which writes the row and
  emits one `info!` log line:
  `scan cost: user=<did> kind=refresh outcome=completed classified=41
  cache_hits=12 gpu_secs=38.2 peak=3 est_cents=3`.
- A failure to write the row is logged at `warn!` and never fails the scan
  (the cost row is bookkeeping, not part of the result).

### The estimate is a lower bound — and says so

The meter counts time our requests are in flight. RunPod also bills each
worker's 60 s idle tail and its cold start. For small refreshes the real bill
can be several times the estimate. The ledger stores the raw meter value plus
`peak_in_flight` so the true ratio can be fitted later against billing
(Part 3). Every UI label reads **"est., excludes idle time"**. No idle-tail
fudge factor is added now — it would be a guess presented as data.

### Not covered (by design)

Spend outside a scan — e.g. a classifier identity probe that wakes a worker
and then refuses (the 2026-10-02 incident class). There is no scan to
attribute it to. Parts 1 and 3 cover it.

Runs that fail before any classifier work also write no row: a full scan
whose scorers fail to build (`build_scan_scorers` is synchronous and makes
no network call), and a refresh refused by the circuit breaker or failing
its marker reset, setup or context load. These spend no GPU time, and the
scan lifecycle and refresh markers already record them as failed. A row per
refused refresh would add one zero-cost row per user per tick during an
outage. (Codex review, 2026-10-24: considered and declined.)

### Where it shows

- **Railway logs:** the per-scan line above.
- **Admin page** (`/admin`, existing admin-only route): a "GPU cost" section.
  - Totals for the last 7 and 30 days (sum of `est_cents`).
  - Per user: full-scan cost to date (all `kind='full'` rows), refreshes to
    date (sum of `kind='refresh'`), total, and last scan time.
  - A user whose full-scan cost to date is over **$2.00** is flagged.
  - Served by a new admin-only endpoint `GET /api/admin/costs`, behind the
    same admin check as the other admin endpoints.

### Tests (written first)

- Meter: `peak_in_flight` tracks concurrency and respects the workers cap;
  `snapshot()` matches `estimated_cents()`.
- DB (SQLite unit + Postgres integration): insert/list round trip; per-user
  and windowed aggregates; `delete_user_data` removes the rows; migration v20
  applies on a v19 database and is idempotent.
- Scan-path wiring is verified on staging (plan Task 8): the scan body needs
  live Bluesky and models, which the unit suites fake.
- A DB write failure on the cost row does not fail the scan.
- Endpoint: non-admins get 403; aggregates and the $2.00 flag are correct.
- Frontend (vitest): `formatCents` formatting and the null case. The
  section's rendering is checked on staging.

## Part 3 — Weekly billing check (follow-up, separate issue)

A once-a-week read of RunPod serverless billing by day, run from the Mac
mini with the local full-access key (never stored in Railway). It reports:

- days billed in the week vs days with a `scan_costs` row — the signature of
  the 2026-10-02 leak was every day billed with no scans;
- billed dollars vs summed `est_cents`, which calibrates the lower-bound
  estimate.

Out of scope for this spec; filed after Part 2 ships.

## Out of scope

Per-user spend limits or refusing scans over budget; the per-tick probe fix
(#386); refresh concurrency caps (#388); PostHog (#390).
