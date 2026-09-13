# #343 Phase 1 — shared-cache pass test (staging)

**Pass (spec §6, Phase 1):** a second staging account whose community
overlaps the first shows a feed-snapshot hit rate ≥ 50 % **and** a scan
wall ≤ 60 % of the cold baseline (13 m 42 s → ≤ 8 m). Hit rate < 20 % →
re-cost before Phase 2.

Everything below is read from the DB. Railway logs are not part of the
pass/fail.

## Accounts

- **A** — the baseline account (the one scanned on 2026-09-07).
- **B** — an account that follows / is followed by much of A's community.
  Both must exist in `users` (they have logged in to staging at least once).

**Pick B by measured overlap, not by feel.** The feed cache is keyed by
candidate DID, so B's hit rate is bounded above by the share of B's
candidates that A's run just fetched. Estimate it from the last scans
before spending 20 minutes:

```sql
WITH a AS (SELECT did FROM account_scores
            WHERE user_did = '<A>' AND scored_at >= '<A run date>')
SELECT u.handle, count(*) AS n,
       round(100.0 * count(*) FILTER (WHERE s.did IN (SELECT did FROM a))
             / count(*), 1) AS pct
  FROM account_scores s JOIN users u ON u.did = s.user_did
 WHERE s.user_did <> '<A>' GROUP BY 1 ORDER BY 3 DESC;
```

On 2026-09-13 the best existing staging user scored 7.9 % — nobody who
shares Bryan's community has signed up on staging, so the ≥ 50 % claim
cannot be tested until one does.

**Triggering a scan without the dashboard.** The admin endpoint
(`POST /api/admin/scan/{did}`) runs exactly this statement; from psql it
is the same thing and needs no browser session:

```sql
INSERT INTO scan_queue (user_did, status, enqueued_at)
VALUES ('<DID>', 'queued', NOW())
ON CONFLICT (user_did) DO UPDATE
  SET status = 'queued', enqueued_at = NOW(), started_at = NULL,
      finished_at = NULL, lease_expires = NULL, last_error = NULL,
      claim_id = NULL
  WHERE scan_queue.status IN ('done', 'failed');
```

The admitter ticks every 30 s and claims it.

**The 7-day freshness filter changes A's population.** Candidates scored
for the same user in the last 7 days are dropped before the gather
(`get_fresh_scored_dids(user, 7)`, #344). A's 2026-09-07 scan was 6 days
old on 2026-09-13, so the "cold" A run saw 469 candidates, not 934. Either
accept the halved population (the cache still cold-starts correctly) or
delete A's recent `account_scores` rows first:

```sql
DELETE FROM account_scores WHERE user_did = '<A>' AND scored_at >= '<date>';
```

## Steps

1. Cold the cache once so A's run is a true baseline:

   ```sql
   TRUNCATE account_feed_snapshots, onnx_scores, classifier_verdicts;
   ```

   Automatic eviction will not interfere with this. Every scan starts by
   deleting feed snapshots older than 7 days and scores/verdicts older than
   90 days; an A/B run finished within a day writes nothing old enough to
   qualify, so the only thing that empties these tables mid-run is the
   TRUNCATE above.

2. Scan **A**. Wait for `scan_queue.status = 'done'`. Record its wall and
   its cache counters. The **feed** counter should be ≈ 0 hits on a cold
   cache — it counts once per candidate (see `CachedPostFetcher`), so a
   truly cold run has nothing to hit. The **onnx** counter WILL show hits
   even on a cold cache: originals scored during stage 1 are re-scored in
   the clean pass, which is an intra-scan hit, not a cross-scan one. Record
   A's onnx rate as the intra-scan floor — it is not evidence the cache
   failed to cold-start.

   ```sql
   SELECT finished_at::timestamptz - started_at::timestamptz AS wall
     FROM scan_queue WHERE user_did = '<A>' ORDER BY enqueued_at DESC LIMIT 1;
   SELECT key, value FROM scan_state
    WHERE user_did = '<A>' AND key LIKE '%_cache_%' ORDER BY key;
   ```

3. Scan **B**. Same two queries with `<B>`.
4. Compute for B:

   - feed hit rate = `feed_cache_hits / (feed_cache_hits + feed_cache_misses)`
   - onnx hit rate = `onnx_cache_hits / (onnx_cache_hits + onnx_cache_misses)`
   - classifier hit rate = same shape
   - wall ratio = B's wall ÷ A's wall

5. Sanity-check the tables grew and hold no readable text:

   ```sql
   SELECT count(*) FROM account_feed_snapshots;
   SELECT count(*) FROM onnx_scores;
   SELECT count(*) FROM classifier_verdicts;
   SELECT text_sha256 FROM onnx_scores LIMIT 3;   -- 64 hex chars, nothing else
   ```

## Result

| | wall | feed hit rate | onnx hit rate | classifier hit rate |
|---|---|---|---|---|
| A (cold) | | ≈ 0 | (floor) | ≈ 0 |
| B (warm) | | | | |

For onnx and classifier, the intra-scan re-score inflates a raw B number
too, so compute the cross-scan rate as `(B − A) / (1 − A)`. The feed rate
needs no correction — it is already per-candidate.

**Pass:** feed hit rate ≥ 0.50 and B's wall ≤ 0.60 × A's wall.
**Re-cost:** feed hit rate < 0.20 — write the number into spec §4.1 and
stop before planning Phase 2.

Record the table as a deciduous outcome under node 788 and in the PR body.
Also note `bluesky_ratelimit_limit` from either account's `scan_state` in
spec §4.3 — Phase 3 needs it.

## Result — 2026-09-13 (staging `593c40d`, deciduous 846/849)

A = chaosgreml.in (469 candidates after the freshness filter), B =
brookie.blog (3 095 candidates; predicted overlap 7.9 %).

| | wall | candidates | feed hit rate | onnx hit rate | classifier hit rate |
|---|---|---|---|---|---|
| A (cold) | 4 m 35 s | 469 | 0 / 469 = 0 | 1 513 / 18 464 = 8.2 % (floor) | 0 / 407 = 0 |
| B (warm) | 18 m 55 s | 3 095 | 204 / 3 095 = **6.6 %** | 15.9 % raw → **8.4 %** cross-scan | 133 / 1 716 = **7.8 %** |

Per candidate: A 0.59 s, B 0.37 s. Cache after B: 3 355 snapshots,
101 446 onnx scores, 1 976 verdicts; `text_sha256` values are 64 hex chars.

**Verdict: re-cost band (< 20 %) — but read it correctly.** The hit rate
equals the measured community overlap almost exactly (6.6 % measured vs
7.9 % predicted), so the cache mechanics are doing what they should; the
number is low because no current staging user shares Bryan's community.
The ≥ 50 % pass needs a genuinely overlapping pair and stays untested. Do
not re-cost the design on this pair; re-run when such an account exists.

`bluesky_ratelimit_limit` was **not** recorded: `public.api.bsky.app`
returns no `RateLimit-*` headers at all (verified with a bare `curl`; the
responses come from a BunnyCDN edge with `cache-control: public,
max-age=30`). Spec §4.3's adaptive limiter has no header to read — 429s
are the only signal.
