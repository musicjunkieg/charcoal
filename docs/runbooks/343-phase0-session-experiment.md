# #343 Phase 0 — ONNX session experiment (staging)

**Question:** does a pool of N ONNX sessions cut gather wall time by ≥ 25 %
on the same candidate set, or does the single mutexed session stay and the
CPU headroom go to more concurrent scans instead? (Spec §6, Phase 0 item 3.)

**Read this first:** the Phase 1 caches ship in the same PR. A second scan
of the same account is served from `account_feed_snapshots` and
`onnx_scores`, which makes its gather wall meaningless for this experiment.
Every run below starts from a **cold cache** — truncate first.

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

3. Trigger a scan for `$DID` from the dashboard and wait for
   `scan_queue.status = 'done'`.
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
     `stage1_onnx_ms`, `inference_pct` — **`total_ms` is the gather wall.**
   - search `cpu_cores_busy` → one sample per minute; note the median.

## Runs

| N | gather `total_ms` | `stage1_onnx_ms` | median `cpu_cores_busy` | RSS (Railway metrics) |
|---|---|---|---|---|
| 1 | | | | |
| 4 | | | | |
| 1 (repeat) | | | | |

Run N=1 twice so run-to-run noise is visible; Bluesky latency varies.

## Decision

- N=4 `total_ms` ≤ 0.75 × mean of the two N=1 runs → keep the pool, set
  `CHARCOAL_ONNX_SESSIONS=4` on staging, record the numbers in spec §1.
- Otherwise → leave the default at 1, record the numbers, and note in the
  spec that CPU headroom goes to scan concurrency (Phase 3).

Either way, file the result as a deciduous outcome under node 788 and put
the table above in the PR body.
