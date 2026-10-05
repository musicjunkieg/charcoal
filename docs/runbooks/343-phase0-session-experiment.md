# #343 Phase 0 — ONNX session experiment (staging)

**Question:** does a pool of N ONNX sessions cut gather wall time by ≥ 25 %
on the same candidate set, or does the single mutexed session stay and the
CPU headroom go to more concurrent scans instead? (Spec §6, Phase 0 item 3.)

**Read this first:** the Phase 1 caches ship in the same PR. A second scan
of the same account is served from `account_feed_snapshots` and
`onnx_scores`, which makes its gather wall meaningless for this experiment.
Every run below starts from a **cold cache** — truncate first.

**Second trap — the freshness filter.** Every candidate the previous run
scored is dropped from the next run of the *same* account for 7 days
(`get_fresh_scored_dids(user, 7)`, #344), so a naive rerun sees ~0
candidates. Between runs, delete the account's `account_scores` rows from
today (staging only) so all three runs walk the same candidate set:

```sql
DELETE FROM account_scores WHERE user_did = '<DID>' AND scored_at >= '<today>';
```

Rows older than 7 days stay filtered on every run, which is fine — the
set is identical across runs, which is all the experiment needs.

## Setup

1. Pick one staging account with ~200 candidates (the 2026-09-07 baseline
   account had 934; a smaller one keeps each run under ~4 min). Record its
   DID as `$DID`.
2. Staging Postgres shell (the web service's `DATABASE_URL` is
   `postgres.railway.internal`, unreachable locally — go through the
   Postgres service):

   ```
   railway run -s Postgres -e staging -- sh -c 'psql "$DATABASE_PUBLIC_URL"'
   ```

   Never paste the URL into a transcript; it carries credentials.

## One run

1. Cold the cache (staging only — this is destructive by design):

   ```sql
   TRUNCATE account_feed_snapshots, onnx_scores, classifier_verdicts;
   ```

2. Set the knob on the staging web service and wait for the redeploy:

   ```
   railway variables --set CHARCOAL_ONNX_SESSIONS=<N> -s charcoal-web -e staging
   ```

3. Trigger a scan for `$DID` — from the dashboard, the admin endpoint, or
   the equivalent `INSERT INTO scan_queue …` in the Phase 1 runbook — and
   wait for `scan_queue.status = 'done'`.
4. Record, from the DB:

   ```sql
   SELECT finished_at::timestamptz - started_at::timestamptz AS wall
     FROM scan_queue WHERE user_did = '<DID>' ORDER BY enqueued_at DESC LIMIT 1;
   SELECT key, value FROM scan_state
    WHERE user_did = '<DID>' AND key IN ('bluesky_ratelimit_limit', 'candidates_total');
   ```

5. Record, from the logs (these two are logged by design — spec §6 items
   1 and 2 say "log it"). Railway MCP `get_logs` with `search` alone (the
   `since` + `deployment_id` combination returns nothing):
   - search `Phase A timing split` → `total_ms`, `fetch_ms`,
     `stage1_onnx_ms`, `inference_pct`. **`total_ms` is NOT the gather
     wall** — `gather.rs` sums each account's elapsed time across all
     workers, so it is worker-seconds (830 s for a 137 s gather). The
     gather wall is `scan_queue.started_at` → the timestamp of that log
     line. Compare walls; use `total_ms / wall` as effective parallelism.
   - search `cpu_cores_busy` → one sample per minute; note the median.
   - Do not compare `scan_queue` walls between runs: the burst phase is a
     RunPod call whose cold start alone swung 127 s → 11 s between two
     otherwise identical runs on 2026-09-13.

## Runs

| N | gather wall | `total_ms` (worker-s) | `stage1_onnx_ms` | median `cpu_cores_busy` | RSS (Railway metrics) |
|---|---|---|---|---|---|
| 1 | | | | | |
| 4 | | | | | |
| 1 (repeat) | | | | | |

Run N=1 twice so run-to-run noise is visible; Bluesky latency varies.

## Decision

- N=4 gather wall ≤ 0.75 × mean of the two N=1 runs → keep the pool, set
  `CHARCOAL_ONNX_SESSIONS=4` on staging, record the numbers in spec §1.
- Otherwise → leave the default at 1, record the numbers, and note in the
  spec that CPU headroom goes to scan concurrency (Phase 3).

Either way, file the result as a deciduous outcome under node 788 and put
the table above in the PR body.

## Result — 2026-09-13 (staging `593c40d`, deciduous 846/856/857/858)

Account: chaosgreml.in, 464–469 candidates after the freshness filter
(same set all three runs; caches truncated and today's `account_scores`
deleted between runs). A first attempt on mxscottkernest.bsky.social hit
3 630 candidates and 36 min per run with inference at 10 % of worker time,
so it was abandoned after one run (deciduous 851).

| N | gather wall | `total_ms` (worker-s) | `fetch_ms` | `stage1_onnx_ms` | `inference_pct` | `cpu_cores_busy` | RSS after scan |
|---|---|---|---|---|---|---|---|
| 1 | 137 s | 830 s | 470 s | 172 s | 35 | 15.3, 9.1 | 2.2 GB |
| 4 | 129 s | 749 s | 462 s | 90 s | 26 | 20.1 | 3.8 GB |
| 1 (repeat) | 120 s | 792 s | 371 s | 213 s | 44 | 15.3 | 2.2 GB |

**Decision: the pool loses.** N=4 gather wall 129 s against a mean N=1 of
128.5 s (threshold ≤ 96 s). The pool does halve stage-1 ONNX
worker-seconds (172/213 → 90 s) for +1.5 GB RSS, but the gather is
fetch-bound and run-to-run noise (137 vs 120 s) is bigger than the effect.
`CHARCOAL_ONNX_SESSIONS` stays at 1 (set explicitly to `1` on staging).
One session already drives ~15 cores through ort's intra-op threads; that
headroom goes to scan concurrency in Phase 3.

Two things the experiment surfaced that matter more than the pool:

- **Idle tail.** Larger accounts (brookie.blog 3 095, mxscottkernest
  3 630) spent the last 5–8 minutes of the gather at `cpu_cores_busy` ≈
  0.02: a handful of straggler fetches with every worker idle. That tail is
  25–40 % of those gather walls.
- **Burst cold start.** The RunPod burst took 127 s on the first scan of
  the session and 11 s on the next: `scan_queue` wall is not a gather
  measure.
