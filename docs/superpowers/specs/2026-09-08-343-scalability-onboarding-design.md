# #343 — Scalability & onboarding design

**Status:** approved in brainstorm 2026-09-07/08, awaiting written-spec review
**Issues:** #343 (this), folds in #344 (scores never re-scored), #182 (no 429
handling), unblocks #342 (scheduled sweeps). Follow-ups: #135 (tier
calibration, gets a soot-vs-Bluesky sample from Phase 4).
**Deciduous:** goal 773 → decisions 775, 777, 778, 783, 784; observations
774, 776, 779, 785; options 780–782.

## 0. The question, and the answer in one paragraph

Bryan asked what it takes to onboard strangers fast and concurrently, and
where web, workers and Postgres should live once Railway stops being
cheapest. The answer: the scan is **half local CPU inference, half Bluesky
I/O**, and both halves are spent redundantly — every user re-fetches and
re-classifies the same community. A **shared, user-independent cache** of
account feeds and post classifications removes most of both halves for
every user after the first; a **token-bucket Bluesky client** and a
**logical web/worker split** make ten concurrent scans safe on one Railway
replica; **score expiry plus a top-tier refresh job** closes #344 and gives
#342 its scheduler; and **soot** later replaces the Bluesky gather entirely.
Hosting stays on Railway at roughly today's bill; a worker on the Hetzner
soot box is kept as a gated option, not a plan.

## 1. Where the time goes today (measured 2026-09-07, staging, post region fix)

One cold scan of Bryan's account: **13 m 42 s** for 934 candidates
(0.88 s/candidate; 2.0 s/candidate on 2026-08-07 before the Postgres region
move, so every earlier timing is stale).

| Phase | Wall | Notes |
|---|---|---|
| Enumeration (Constellation + follows) | 27 s | Bluesky/Constellation I/O |
| Gather (fetch + clean-pass + stage-1 ONNX) | **10 m 16 s** | 3 618 worker-seconds over 8 workers ≈ 5.9 effective. Fetch 39 %, clean-pass 7 %, stage-1 ONNX 41 % (`inference_pct` 47) |
| Burst (classifier, 887 posts) | 2 m 14 s | RunPod CoPE-B |
| Finalize (569 accounts scored) | 45 s | DB writes, 5 ms each |

Memory: 0.7 GB idle, 1.9 GB peak. vCPU during the gather is **unmeasured**
(Railway's metrics API reports 0.012 max during a known 10-minute gather —
unusable; Phase 0 fixes this).

**Measured 2026-09-13 (Phase 0, staging `593c40d`; runbooks
`docs/runbooks/343-phase0-session-experiment.md` and `…-phase1-hit-rate.md`,
deciduous 846–858):**

- **Inference cores.** One ONNX session drives **13–15 cores** during a
  gather (`cpu_cores_busy` median ≈ 13 on Bryan's account, ≈ 8 on a
  3 630-candidate account; Railway's metrics API now agrees, 15.3 peak). That
  is ort's intra-op parallelism on a single session, not eight workers each
  using one core — so "inference cores per worker" is ~1.7 and the 24 vCPU
  cap is one scan's headroom, not ten's.
- **`RateLimit-Limit`: none.** `public.api.bsky.app` sends no `RateLimit-*`
  headers (see 4.3).
- **Session pool: no.** N=4 sessions vs N=1 on the same 464-candidate set:
  gather wall 129 s vs 137 s / 120 s (N=1 twice) — a 0 % change against a
  25 % bar, for +1.5 GB RSS. Stage-1 ONNX worker-seconds do halve
  (172/213 → 90 s), but the gather is fetch-bound: `fetch_ms` is 46–84 % of
  worker time depending on the account, and larger accounts end with a
  **5–8 minute tail at ≈ 0 cores** waiting on a few straggler fetches.
  `CHARCOAL_ONNX_SESSIONS` stays at 1.
- **Freshness filter halves the population.** A rescan within 7 days drops
  every candidate scored last time (`get_fresh_scored_dids`, #344): Bryan's
  "cold" run saw 469 candidates, not 934. Per-candidate throughput on that
  set was 0.59 s (cold) and 0.37 s on a 3 095-candidate warm run.
- **Burst cold start.** The first RunPod burst of the session took 127 s;
  the next took 11 s. `scan_queue` wall is not a gather measure.

What this means for ten users at once, unshared: ~22 Bluesky requests/s from
one IP and ~20 cores of inference. With realistic community overlap (three
users share roughly a third of their candidates) that is ~7 req/s and ~6
cores, which one Railway Pro replica (24 vCPU cap) absorbs. The per-wave
metered cost is cents (17 400 vCPU-s ≈ $0.13 unshared). **The cost driver is
idle memory, not waves.**

## 2. Target

**Ten concurrent onboardings, each finished in ≤ 15 minutes**, without
tripping Bluesky rate limits and without degrading the web tier — at a total
hosting bill ≤ $60/month across production and staging (today ≈ $30).

## 3. Approach chosen

Three approaches were weighed (deciduous 780–783):

- **A. Shared cache + logical worker split** — cache feeds and
  classifications across users; add rate limiting; make the process role
  configurable. Cheapest, no new infrastructure, every phase independently
  measurable. **Chosen.**
- **B. Soot as candidate source** — replace the Bluesky gather with soot's
  `/export`. Largest win but gated on soot's full-network backfill finishing.
  **Kept as the final phase.**
- **C. Move workers off Railway now** — dropped: it moves cost without
  removing work, and re-creates #335 (cross-region DB writes).

## 4. Design

### 4.1 Shared cache (S1)

Three new tables, all **keyed by DID or by scored text — no `user_did`** —
because a post's toxicity is a property of the post, not of who is being
protected. Only topic overlap, graph distance and targeting are per-user,
and those stay in the existing `(user_did, account_did, …)` tables.

```
account_feed_snapshots
  did          TEXT PRIMARY KEY
  handle       TEXT NOT NULL
  posts_json   TEXT NOT NULL      -- Vec<FeedPost>, the ordered getAuthorFeed sample
  fetched_at   TIMESTAMP NOT NULL
  source       TEXT NOT NULL      -- 'bluesky' | 'soot'

onnx_scores                       -- stage-1 / clean-pass primary-scorer output
  text_sha256  TEXT NOT NULL      -- SHA-256 (hex) of the exact text the model saw
  model_id     TEXT NOT NULL      -- ONNX model identity
  score        REAL NOT NULL
  scored_at    TIMESTAMP NOT NULL
  PRIMARY KEY (text_sha256, model_id)

classifier_verdicts               -- burst (CoPE-B / stub) output
  text_sha256     TEXT NOT NULL
  model_id        TEXT NOT NULL
  policy_version  TEXT NOT NULL
  toxic_token     BOOLEAN NOT NULL
  confidence      REAL NOT NULL
  classified_at   TIMESTAMP NOT NULL
  PRIMARY KEY (text_sha256, model_id, policy_version)
```

**Why text hashes, not `(post_uri, cid)`** (amended 2026-09-08 while
planning): `Post` carries no cid, and the same post is scored as **two
different texts** — stage 1 scores the raw text, the clean pass scores the
`format_parent_reply(parent, reply)` envelope for replies. A post-URI key
would have to store both, and would silently serve a raw-text score for an
envelope lookup. The hash is the exact identity of what the model saw: a
missing parent produces a different envelope and therefore a different key,
which is the correct behaviour. It also stores **no readable post text** —
a privacy improvement over `posts_json`, which is the only place the text
itself is persisted.

- **Read-through as decorators.** The gather/burst scoring code does not
  change. `CachedPostFetcher` wraps the Bluesky feed source: if a snapshot
  exists with `fetched_at > now − SNAPSHOT_TTL` (24 h) and decodes, use it;
  otherwise fetch once (50 posts — removing today's 25-then-50 double
  fetch), upsert, and sample from it. `CachedToxicityScorer` wraps the ONNX
  scorer's `score_batch` with an `onnx_scores` lookup by hash; misses are
  scored and inserted. `CachedClassifier` wraps the classifier's
  `classify_batch` with a `classifier_verdicts` lookup by
  `(hash, model_id, policy_version)`; only successful verdicts are cached.
- **Per-user tables are populated from the cache**, never the other way
  round. `classification_queue` and `scan_account_input` keep their shape.
- **`delete_user_data` does not touch these tables** — they hold nothing
  about the protected user.
- **Migration v16**, both backends. Postgres migration self-records its
  version (`INSERT INTO schema_version … ON CONFLICT DO NOTHING`).
  **No backfill** — existing rows in `classification_queue` are not copied;
  the cache fills on the next scan.
- **Claim to verify:** the second user in a community sees ≥ 50 % of
  candidates hit the snapshot cache. Below 20 % the cache is not paying for
  its writes and the plan is re-costed before Phase 2.

### 4.2 Worker role and concurrency knobs (S2)

- `CHARCOAL_ROLE=web|worker|both`, default `both` (today's behaviour).
  `worker` runs the admitter and the refresh tick (4.4) and serves only
  `/healthz`; `web` serves the app and does not run the admitter. This is a
  config split, not a code split — one binary, one Docker image.
- Three semaphores, all process-global:
  - `gather_permits` — default 16, replaces the literal `8` at
    `src/web/scan_job.rs:1047`.
  - `bluesky_permits` — in-flight cap that backs the token bucket in 4.3.
  - `burst_permits` — default 16, shared across all running scans.
- `CHARCOAL_SCAN_CONCURRENCY` (`src/web/admitter.rs:58`) clamp becomes
  1..16, default 10.
- **ONNX sessions.** ort 2.0.0-rc.11's `Session::run` takes `&mut self`
  (`session/mod.rs:206`), so the `Arc<Mutex<Session>>` in
  `src/toxicity/onnx.rs`, `src/topics/embeddings.rs:32` and
  `src/scoring/nli.rs:107` is required, not optional. Parallel inference
  therefore means **N sessions**, at ~500 MB per set. Phase 0 decides this
  with a number (see 6).
- **#277 (LiveScans process-local)** stays accepted until there are ≥ 2
  worker replicas; at that point `scan_queue.running_on` records the
  replica and the admitter counts by row, not by in-memory set.

### 4.3 Bluesky rate-limit strategy (S3)

Facts: `public.api.bsky.app` publishes no numeric limit ("generous"); the
3 000 per 5 min figure in the docs is the PDS limit, not the AppView's.
**Corrected 2026-09-13:** the AppView returns **no** `RateLimit-*` headers
at all — verified with a bare `curl` (responses come from a BunnyCDN edge
with `cache-control: public, max-age=30`), and the Phase 0 scan logged
"no RateLimit-Limit header observed". The "adaptive" bullet below therefore
has nothing to read; the bucket is sized by hand and 429s are the only
feedback. The 30 s CDN cache also means repeated fetches of the same feed
inside that window never reach the origin.
`PublicAtpClient` (`src/bluesky/client.rs:74`) has **no 429 handling**
(#182); the only existing limiter is `src/toxicity/rate_limiter.rs`, used
by Perspective and Zentropi.

- **Shared token bucket** on the client: `CHARCOAL_BLUESKY_RPS` default 8,
  burst 16. Every scan draws from the same bucket, so ten scans cannot
  exceed what one replica's IP is allowed.
- **Adaptive:** read `RateLimit-*` on every response. When `Remaining` drops
  under 10 % of `Limit`, halve the refill rate until `Reset`. Log the
  observed `Limit` at INFO once per minute at most, and persist the last
  observed value in `scan_state` so Phase 0 gets its number for free.
- **429 (#182):** honour `Retry-After` or `RateLimit-Reset`; park the
  **whole bucket**, not just the failing call; retry up to 3× with jitter;
  after that surface a typed transient error into the #183 resumable path
  (the scan pauses and resumes, it does not abort).
- **Boundary condition:** at ~4 calls per unique candidate, 8 rps supports
  ~2 candidates/s ≈ 1 200 unique candidates per 10 minutes. A ten-user wave
  with no cache overlap needs ~9 000; **the wave target depends on the
  cache (4.1) or on soot (4.5), not on the bucket alone.** The bucket makes
  the wave safe; the cache makes it fast.
- Constellation is unchanged (its own bounded client, #235). Once soot is
  the source, Bluesky calls fall to ~1 per candidate (profile + follows).

### 4.4 Score expiry and refresh (S4 — closes #344, unblocks #342)

Today `sweep.rs:134` and `amplification.rs:312` call
`get_fresh_scored_dids(user_did, 7)`; `ScoringConfidence::staleness_days()`
(`src/db/models.rs:63`, 14/7/3 days) is dead in production, and a DB error
in the freshness read silently re-scores everything via
`.unwrap_or_default()`.

- `account_scores` gains `scoring_generation TEXT NOT NULL` and
  `valid_until TIMESTAMP NOT NULL`. `valid_until = scored_at +
  staleness_days()` at write time.
- **Fresh** = `valid_until > now() AND scoring_generation = <current>`.
  The current generation is a build-time constant bumped whenever the
  formula, the models or the policy change. `get_fresh_scored_dids` takes
  no `max_age_days` any more.
- The freshness read is a **hard error** — a scan that cannot read
  freshness does not run.
- Expired rows are **kept** but filtered out of tier lists; the UI shows
  "N expired" so a user can see why a list shrank.
- **Re-entry** is by re-engagement (the normal path) or by the **top-tier
  refresh job**: high and elevated rows with `valid_until < now() + 2 d`,
  read from `account_scores`, re-scored through the normal pipeline.
- `scan_queue.kind = 'full' | 'refresh'`; `users.next_refresh_at`. The
  refresh tick runs inside the `worker` role, reusing the admitter's `TICK`
  (`admitter.rs:53`) — **no second lock, no second loop**. Defaults:
  refresh nightly, full fortnightly.
- **Migration v17**: backfill `valid_until = scored_at + 14 d`,
  `scoring_generation = 'legacy'`. Legacy rows expire naturally and are
  refreshed by the job; nothing is deleted.

### 4.5 Candidate source trait and soot (S5)

```rust
#[async_trait]
pub trait CandidateSource: Send + Sync {
    /// Accounts that engaged with `user_did` since `since`.
    async fn engagers(&self, user_did: &str, since: DateTime<Utc>)
        -> Result<Vec<EngagementEvent>>;
    /// Recent posts authored by `did`, newest first, at most `limit`.
    async fn recent_posts(&self, did: &str, limit: usize)
        -> Result<AccountPosts>;
}
```

- `BlueskySource` wraps today's Constellation + `getAuthorFeed` path.
- `SootSource`: `engagers` = `/export?parent_did=<did>` ∪
  `/export?quoted_did=<did>` (replies targeting and quotes targeting);
  `recent_posts` = `/export?did=<did>&since=<now−90d>`, capped at 50.
- The cache (4.1) sits **above** the trait: a snapshot is a snapshot
  whichever source filled it (`source` column).
- `CHARCOAL_CANDIDATE_SOURCE=bluesky|soot|soot-then-bluesky`, default
  `bluesky`. In `soot-then-bluesky`, an account with fewer than `MIN_POSTS`
  (10) from soot falls back to Bluesky for that account.
- `SOOT_URL` / `SOOT_TOKEN` from env only, never logged. Expect ~150 ms RTT
  from us-west2 to the Hetzner box.
- Profiles and follows stay on Bluesky — soot indexes posts, not the
  social graph.
- **Calibration checkpoint** before switching the default: score 100
  candidates from both sources, diff tiers, file the result under #135.
- Caveats carried from soot's author: the date filter is the author-supplied
  `createdAt` (backdatable — fine for windows, not forensic); coverage is
  what has been ingested (complete per-DID history only after the
  full-network backfill); deleted content is absent by design.

## 5. Hosting matrix

Railway rates (pricing page, 2026-09-08): CPU $0.00000772/vCPU-s ≈
$20/vCPU-mo; memory $0.00000386/GB-s ≈ $10/GB-mo; volume ≈ $0.155/GB-mo;
egress $0.05/GB. Pro: $20 minimum with $20 usage credit; **24 vCPU / 24 GB
per replica**, up to 42 replicas.

| Option | Marginal $/mo (prod + staging) | Wave risk | Verdict |
|---|---|---|---|
| **A. Railway, `CHARCOAL_ROLE=both`** | +$0 idle, +$1–3 metered | Web shares the replica; sluggish under a wave, never stalled (24 vCPU headroom) | **Default for Phases 1–4.** Total ≈ $30–35 |
| B. Railway, separate `worker` service | +$15–20 (second process's ~0.7–1 GB idle × 2 envs + model volume) | None | Only if a measured wave degrades web p95. Config change, not code |
| C. Worker on the Hetzner soot box | +$0 (sunk $80) | **#335 again**: EU↔us-west2 ≈ 150 ms RTT ⇒ ~300 ms per row write; finalize alone +3 min. Shares CPU with soot's backfill; needs Postgres TCP proxy + TLS | **Gated**, see below |

**Gate for C** (all three): soot's full-network backfill is finished; the
pipeline writes in batches per phase (multi-row inserts), not per candidate;
the measured effective DB write cost from that box is < 50 ms per candidate.
Gate fails → stay on A. C is kept open because it also buys a second
Bluesky source IP and ~1 ms soot RTT.

Out of scope: moving Postgres off Railway (Bryan's constraint; it would also
drag web latency with it).

## 6. Phased plan

Each phase: its own `feat/*` branch → PR to `staging` (CodeRabbit
APPROVED) → soak → promotion PR to `main`. Each phase has a pass number
before the next starts.

**Phase 0 — Measure** (no product code beyond logging)
1. Sample the process's own CPU time once a minute during the gather and log
   it. *Pass:* inference cores per worker known to ±1.
2. Log the observed `RateLimit-Limit` once per scan. *Pass:* a number.
3. ONNX session experiment on a fixed 200-candidate sample: one mutexed
   session set vs N=4 session sets (≈ +1.5 GB). *Pass for N sessions:*
   gather wall drops ≥ 25 %; otherwise the mutex stays and CPU headroom is
   spent on more concurrent scans instead.

*Result 2026-09-13:* (1) ≈ 1.7 cores per worker, 13–15 per gather — done.
(2) No number: the AppView sends no header (4.3). (3) **Pool rejected**:
0 % gather-wall change on a 464-candidate set; default stays 1. Headroom
goes to Phase 3 concurrency. Numbers in §1 and the runbook.

**Phase 1 — Shared cache (4.1)**
*Pass:* second staging account with overlapping community: snapshot hit rate
≥ 50 % **and** scan wall ≤ 60 % of the cold baseline (13 m 42 s → ≤ 8 m).
Hit rate < 20 % → re-cost before Phase 2.

*Result 2026-09-13:* **untestable as specified — no overlapping pair
exists on staging.** Against the best available second account
(brookie.blog, predicted 7.9 % overlap) the feed hit rate was **6.6 %**,
onnx 8.4 % cross-scan, classifier 7.8 % — i.e. the cache hits exactly the
overlap that exists, which validates the mechanism but says nothing about
the ≥ 50 % claim. The re-cost clause is **not** triggered on this pair: it
assumed an overlapping community. Phase 2 may proceed; the pass test is
re-run when an account from Bryan's community is granted on staging.
Also learned: the same-user 7-day freshness filter and RunPod cold start
both distort wall comparisons — see the runbook.

**Phase 2 — Expiry + refresh (4.4)**
*Pass:* bump the generation on staging → every row expired and hidden; the
nightly refresh re-scores exactly the high/elevated set; no `legacy` row in
any tier list after one nightly.

**Phase 3 — Rate limiting + worker role (4.2, 4.3)**
*Pass — the #343 acceptance test:* ten gated staging accounts whose
communities overlap (the realistic onboarding case: friends invite friends)
enqueued together, with the cache holding at least one prior scan from that
community; all ten `done` in ≤ 15 min; zero 429s; web p95 unchanged.
Ten accounts with **no** overlap and a cold cache is not the target — 4.3's
arithmetic puts that at ~19 min on Bluesky alone, and Phase 4 is what
fixes it. If Phase 0's observed `RateLimit-Limit` supports it, raise
`CHARCOAL_BLUESKY_RPS` before running this test and record the value here.

**Phase 4 — Soot source (4.5)** — gated on the full-network backfill.
*Pass:* same ten-user wave ≤ 5 min; Bluesky calls per candidate ≈ 1;
calibration diff filed under #135.

**Phase 5 (optional) — Hetzner worker** — only if the §5 gate passes.

## 7. Testing rules for every phase

- TDD per the project mandate: failing test first, then code.
- Migrations get a fresh-DB test and an upgrade-from-v15 test on **both**
  backends (`cargo test --features web` and the Postgres run against
  `postgres://$USER@localhost/charcoal_test`).
- Every new env knob has a clamp/default test; every semaphore has a test
  that proves the cap holds under contention.
- Rate-limiter tests use a fake clock and a fake server returning
  `RateLimit-*` and 429 with `Retry-After`; assert the bucket parks as a
  whole.
- The ten-user wave is a **manual runbook** in this spec's plan, not a cargo
  test: which accounts, how to enqueue, what to read from `scan_queue` and
  `scan_skips` (not Railway logs).
- Model-gated tests run with `CHARCOAL_MODEL_DIR=./models` and are checked
  for zero `SKIP:` lines.

## 8. Not doing

- Hosting Postgres anywhere but Railway.
- A second admitter loop or a distributed lock for the refresh tick.
- Backfilling the cache from historical `classification_queue` rows.
- Cross-user score sharing — per-user tables stay per-user; only the
  user-independent layer is shared.
- Tuning ORT intra-op thread counts before Phase 0's measurement exists.
