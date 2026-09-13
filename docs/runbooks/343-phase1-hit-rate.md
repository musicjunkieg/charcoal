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
  Both must be on the staging allowlist.

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
