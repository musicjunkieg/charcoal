# #343 Phase 2 — score expiry + nightly refresh (deploy runbook)

**What shipped:** schema v18, score expiry, the scoring *revision* stamp, and
a nightly background refresh that re-scores each user's High/Elevated accounts
shortly before their scores expire. Chainlink #344, spec §4.4.

**Who this is for:** whoever is deploying. It is written to be followed under
deploy pressure, top to bottom, with the actual command at every step and a
plain statement of what a correct reading looks like.

**Pass numbers (spec §6, Phase 2):** deploy → every pre-existing row hidden and
`tier_counts.expired` equals the row count; the first ticks enqueue a refresh
for every user with scores; each refresh re-scores exactly that user's
High/Elevated set; no `legacy` row at or above Elevated remains after one
refresh per user; an interrupted refresh resumes itself on its retry; a user's
full-scan request during a refresh runs afterwards; a `charcoal migrate`
rehearsal on a copy of prod preserves row counts and provenance.
**The former ≥ 80 % refresh feed-cache hit rate is withdrawn** (deciduous 878)
and replaced by the functional test in §6 — do not reinstate it.

**Everything below is read from the database.** Railway logs are not part of
the pass/fail; they are only ever corroboration.

**If this deploy looks wrong and you want to undo it, go to §2b first.**
Redeploying the previous binary is *not* a rollback here — on Postgres it boots
green and then fails every score write two hours later. §2b explains why and
gives you the three things you can actually do instead.

## Vocabulary (read this once)

- **Scoring revision** — the string stamped on every score row. It is
  `SCORING_GENERATION` (a hand-maintained date in `src/scoring/generation.rs`)
  joined with the identity of every model the binary loads, e.g.
  `2026-09-13|onnx=detoxify-unbiased-toxic-roberta-quantized|emb=all-MiniLM-L6-v2|nli=nli-deberta-v3-xsmall-fp32`.
  Rows stamped anything else are hidden.
- **`legacy`** — the revision migration v18 writes onto rows that were scored
  before revisions existed. It is never equal to the current revision, so
  every pre-existing row is hidden the moment v18 runs. This is intended.
- **Fresh** — `scoring_generation = <current revision> AND valid_until > now()`.
  Everything a user sees is filtered by exactly this predicate.
- **Fulfilled vs. proven** — a full scan that finished (even with skipped
  accounts, even when the skip count could not be read) *fulfils* the user's
  request: it writes the cooldown marker and clears the obligation. Only a
  *clean* completion *proves* the revision and earns the next nightly slot.

## How to run the SQL in here

Production:

```bash
railway run -s Postgres -e production -- sh -c 'psql "$DATABASE_PUBLIC_URL" -tAc "SELECT 1"'
```

Staging is the same with `-e staging`. Railway prints its own noise on stdout;
filter for the line you want or ignore anything containing `postgres://`.
Throughout this document `psql "…" -tAc "<SQL>"` is shorthand for that whole
wrapper. Use single quotes on the outside, double quotes on the inside — this
project's shell breaks on heredocs.

---

## 0. Pre-deploy checklist

### 0.1 `CHARCOAL_COPE_B_POLICY_VERSION` — highest priority

**This is the one that stops a deploy.** The Stage-2 classifier (CoPE-B on
RunPod) runs *outside* the Charcoal binary, so Charcoal cannot read its
identity the way it reads the ONNX models it loads itself. As of this branch, a
score may only be published from verdicts whose recorded producer is one this
binary recognises. If Charcoal's idea of the endpoint's policy version and the
endpoint's actual `POLICY_VERSION` differ by so much as a character, **every
Stage-2 verdict is foreign evidence**: every account is re-gathered once and
then skipped, no scores are written, and every refresh fails.

This is loud, not silent — a web-triggered scan, and a refresh with work to do,
probes the endpoint before it gathers anything and refuses to start, naming both
values (the CLI `scan`/`sweep` commands do not probe) — but a refused scan is
still a failed deploy day. **The variable is unset in production today, and nobody on
this branch knows the endpoint's value.** Read both sides before deploying.

Read what the endpoint serves (the environment variable on the RunPod endpoint
itself, in the RunPod console under the endpoint's template, or from its worker
env):

```bash
# RunPod console → Serverless → your CoPE-B endpoint → Template → Environment
# Variables → POLICY_VERSION.  Copy the value verbatim, including case.
```

Read what Charcoal is told:

```bash
railway variables -s charcoal-web -e production --json | grep -i COPE_B_POLICY
```

Empty output means unset, which Charcoal reads as `policy-unknown`.

**They must be identical.** If they are not, set it before deploying:

```bash
railway variables -s charcoal-web -e production --set CHARCOAL_COPE_B_POLICY_VERSION=<the endpoint value> --skip-deploys
```

If the endpoint's *policy itself* changed and not just its label, also do §0.2.

**Setting it correctly still throws the Stage-2 verdict cache away — once, and
unavoidably.** The classifier cache is keyed on the classifier's `model_id`
*and* its `policy_version` (`src/toxicity/cached_classifier.rs:69-73` on the
read, `:116` on the write). On this branch the RunPod client reports
`cope_b_expected_policy()` as that `policy_version`
(`src/toxicity/runpod_cope_b.rs:663-670`); before this branch it reported the
literal `"policy-unknown"` (same method on `staging`). So every verdict already
in the cache is filed under `policy-unknown`, no read will ever match it again,
and every text has to be classified afresh. **There is no value you can pick
that avoids this** — keeping `policy-unknown` is precisely what the probe above
now refuses to start with. Nothing is deleted: the orphaned rows simply age out
after 90 days (`SCORE_RETENTION`, `src/db/cache_retention.rs:25`). What it
costs on deploy day is in §2 — read that before you deploy, because it lands in
the same window as the re-scoring.

### 0.2 `SCORING_GENERATION` — do you need to bump it?

`SCORING_GENERATION` is a constant in `src/scoring/generation.rs`, not a knob.
Changing it is a code change, deliberately: two replicas disagreeing about the
revision would hide each other's scores.

**You do not need to bump it for a model swap.** The identity of every model
the binary loads — toxicity, embedding, NLI — is composed into the revision
automatically, so swapping one expires every stored score by itself.

**Bump it by hand for:**

- the threat formula or its weights (`src/scoring/threat.rs`)
- the topic-overlap maths or the fingerprint JSON format (`src/topics/`)
- a scoring-policy change (tier thresholds, abstention rules)
- **a Stage-2 classifier model or policy change** (CoPE-B / Zentropi)

The last one is the trap. The classifier lives outside the binary, so its
identity *cannot* be composed into the revision — there is nothing to read at
build time. A new CoPE-B policy that changes verdicts, with `SCORING_GENERATION`
unchanged, leaves thousands of scores that look current and were produced under
a policy that no longer exists. Bump it.

To bump: edit the `SCORING_GENERATION` constant to today's date, commit, deploy.
Then expect §2 and §3 to replay in full — everything is hidden, everyone is due.

### 0.3 Record the pre-deploy tier counts

You need these numbers to check §2 and §3. Run before deploying:

```sql
SELECT user_did,
       COUNT(*)                                            AS rows_total,
       COUNT(*) FILTER (WHERE threat_score >= 15)          AS candidates,
       COUNT(*) FILTER (WHERE threat_tier = 'High')        AS tier_high,
       COUNT(*) FILTER (WHERE threat_tier = 'Elevated')    AS tier_elevated
  FROM account_scores
 GROUP BY user_did
 ORDER BY rows_total DESC;
```

Write down `rows_total` and `candidates` per user.

- `rows_total` is the number that should appear as `expired` immediately after
  the deploy (§2).
- `candidates` is the number the refresh should re-score for that user (§3).
  Note the predicate: the refresh selects by **score** —
  `threat_score >= ThreatTier::ELEVATED_MIN`, which is `15.0`
  (`src/db/models.rs:244`) — not by the stored tier string, deliberately, so
  the candidate set cannot drift from the tier definition. The two `tier_*`
  columns are there only so you can reconcile against `GET /api/status`, which
  *does* bucket by the stored string. `High` starts at **35**, not 25; the
  "Threat tiers" table in the README said 25 for a long time and was wrong.

### 0.4 Know that the migration blocks boot

Migration v18 is a **full-table rewrite that runs inside the boot transaction**,
before the web service accepts traffic. It adds two columns to `account_scores`,
backfills `valid_until` on every row, then runs `SET NOT NULL` (which makes
Postgres verify every row), and creates `idx_account_scores_user_score`
**non-concurrently** (which takes a write lock on the table for the duration).

At current row counts this is **under a second**. Measured 2026-09-17:
`account_scores` holds 5,427 rows (5 MB) in production and 61,330 rows (55 MB)
on staging, and the v18 statements run as one transaction on a local
61,330-row table padded to 101 MB took 499 ms end to end. The lock is taken
by the first `ALTER TABLE … ADD COLUMN`, not only by the index build, so moving
just the index to `CONCURRENTLY` would not shorten it. It is worth knowing anyway, for two
reasons: if the deploy appears to hang at boot, this is the first thing to look
at; and the moment `account_scores` is large (tens of millions of rows) this
step becomes a maintenance window, not a deploy step — at which point the
index creation should move to `CREATE INDEX CONCURRENTLY` outside the
transaction. Nothing in this deploy needs that yet.

A migration that *fails* is the good case, not the bad one: the transaction
rolls back, boot fails, and the old container keeps serving the unmigrated
database with nothing to undo. §2b spells that out — along with why redeploying
the old binary *after* a successful migration is a different and much worse
situation.

### 0.5 Rolling deploys and the revision

Railway ships a revision change as a single-replica deploy. During the seconds
in which both binaries are alive, the old one can still stamp its in-flight
scan's rows with the *old* revision. Those rows are hidden by the new binary
and re-scored by the refresh job if they are High/Elevated. Staged work the old
binary left behind carries the old revision in `scan_state.scan_run_generation`
and is discarded on the next run. This is expected and needs no action — it is
recorded here so that a handful of `legacy`-adjacent rows right after a deploy
does not read as a defect.

---

## 1. Before deploy: pre-deploy High/Elevated counts per user

This is §0.3. Do not skip it — §3 has no meaning without it.

---

## 2. Deploy → everything is hidden

Deploy, wait for the service to report healthy, then:

```sql
SELECT scoring_generation, COUNT(*)
  FROM account_scores
 GROUP BY scoring_generation;
```

**Correct reading:** one row, `legacy`, with a count equal to the total
`rows_total` from §0.3. Every pre-existing score is now hidden.

Then check the API for one user (any session-authenticated browser, dev tools →
Network, or a `curl` carrying the session cookie):

```
GET /api/status
```

**Correct reading:** `tier_counts.high`, `.elevated`, `.watch`, `.low` and
`.total` are all `0`, and `tier_counts.expired` equals that user's
`rows_total`. `expired` is deliberately *not* part of `total` — `total` counts
what is currently being shown, `expired` explains what is not.

**This is the expected state, not an outage.** The dashboard will show zeros
with an "expired" count next to them until §3 finishes. Every row is still in
the database; nothing was deleted. Say so before anyone else notices.

Two more things are true immediately after this deploy, and both look like bugs
if you are not expecting them:

- **Some users get one free scan past the cooldown.** The 24-hour cooldown no
  longer reads the queue row's `done` status; it reads only
  `scan_state.last_full_scan_finished_at`, a key nothing had ever written
  before this deploy. v18 backfills it from every queue row that is already
  `done` with a `finished_at`, which covers most users — but a user whose scan
  was still queued or running at deploy time has no marker, and their next
  click is admitted immediately. One extra scan per such user, once. Check who
  is exposed with:

```sql
SELECT q.user_did, q.status FROM scan_queue q
 WHERE NOT EXISTS (SELECT 1 FROM scan_state s
                    WHERE s.user_did = q.user_did
                      AND s.key = 'last_full_scan_finished_at');
```

- **The queue ETA is null until one full scan completes post-deploy.** The ETA
  median is computed from per-user duration samples written alongside the
  cooldown marker, and no post-deploy scan has written one yet. It also now
  samples every *fulfilled* full attempt — including ones that completed with
  skips or whose skip count could not be verified — not only clean ones, so the
  median is slightly more pessimistic than it used to be. That is deliberate: a
  scan that took 90 minutes and skipped three accounts still took 90 minutes.

### 2a. Deploy day is the most expensive day this feature has

Two costs land in the same window, and they multiply rather than overlap.

1. **Every High/Elevated row in the database is re-scored at once.** The first
   ticks (§3) make every user with scores due, 25 at a time, and each refresh
   re-scores that user's whole candidate set. That is the feature working; §0.3
   told you the number (`candidates`, summed across users).

2. **None of that work can be served from the Stage-2 verdict cache.** The
   cache is keyed by the classifier's `policy_version`, and this deploy changes
   what that string is — see §0.1. Every pre-existing cached verdict is now
   filed under a key nothing reads, so every one of those re-scores is a live
   RunPod (or Zentropi) call rather than a table read.

So the classifier bill on deploy day is not "the usual nightly refresh". It is
a full re-score of every candidate with a cold cache, all in the first hours.
Know the number before you deploy, not after the invoice.

If the spend is the thing that has gone wrong, the brake is **§2b Option 1** —
turn the nightly job off. It stops new work being claimed; it does not undo
anything and does not bring hidden scores back.

---

## 2b. Rollback — read this before you undo anything

### The instinctive move is the harmful one

> **Redeploying the previous binary is not a rollback on Postgres.** The old
> container boots green, passes its health check, serves reads normally — and
> then fails *every score write* at the end of the next scan, about two hours
> in, after the user has waited for the whole gather.

Three facts produce that, and they are worth reading rather than just obeying:

1. Migration v18 makes `account_scores.valid_until` `NOT NULL` **with no
   default**, and drops the default off `scoring_generation`
   (`migrations/postgres/0018_score_expiry.sql:32-37`). Its header comment says
   why, in as many words: *"a writer that forgets the stamp must fail, not
   write 'legacy'."*
2. **The pre-v18 binary is exactly that writer.** Its `upsert_account_score`
   INSERT names neither column — check it yourself with
   `git show staging:src/db/postgres.rs` and read the column list around line
   452. Every row it tries to write violates the not-null constraint.
3. **Nothing stops the old binary starting on the newer schema.** The migration
   runner iterates its *own* embedded list, skips anything above its max
   version, and treats an already-recorded version as done
   (`src/db/postgres.rs:391-404`). An old binary has no v18 in its list, so it
   never considers it at all — it returns `Ok` and boots.

There is no guard, no version check, and no error at startup. The failure is
silent until the first score write.

**This is Postgres only**, which means production and staging. SQLite keeps
both defaults (`src/db/schema.rs:633-634` — `scoring_generation` still
`DEFAULT 'legacy'`, `valid_until` still nullable), so a local SQLite checkout
will not reproduce it. Do not take a clean local rollback as evidence.

### The safe case: a migration that *fails* needs no rollback

Migrations 2 and up run inside a single transaction
(`src/db/postgres.rs:411-417`). If v18 raises, the transaction rolls back,
`db::open()` returns the error, boot fails, and Railway keeps the previous
container serving the **unmigrated** database. Nothing is half-applied.

Confirm it with:

```bash
railway run -s Postgres -e production -- sh -c 'psql "$DATABASE_PUBLIC_URL" -tAc "SELECT MAX(version) FROM schema_version"'
```

**Correct reading:** `17`. The old schema is intact, the old binary is
consistent with it, and there is nothing to undo. Fix the migration and deploy
again.

If that returns `18`, the migration committed and you are in the situation the
rest of this section is about.

### Reading the current scoring revision (you will need it, and nothing prints it)

`scoring_revision()` (`src/scoring/generation.rs:70-72`) is
`SCORING_GENERATION` joined with the three model ids, e.g.
`2026-09-13|onnx=detoxify-unbiased-toxic-roberta-quantized|emb=all-MiniLM-L6-v2|nli=nli-deberta-v3-xsmall-fp32`.

**There is no `charcoal` subcommand that prints it** (check `enum Commands` in
`src/main.rs`), and no routine log line carries it. The single log that
mentions it is a `warn!` on a staged-blob generation mismatch
(`src/pipeline/scan_phases/finalize.rs:109-115`) — you cannot summon that on
demand. So read it off the database, which is the authoritative copy anyway:

```sql
SELECT scoring_generation, COUNT(*)
  FROM account_scores
 GROUP BY 1
 ORDER BY 2 DESC;
```

**Correct reading, any time after the new binary has written one score:** two
rows — `legacy` with the bulk of them, and one value of the `…|onnx=…|emb=…`
shape. That second value is the current revision. Copy it verbatim, pipes and
all.

If no scan has run yet under the new binary there is no row to read, and the
running system cannot tell you. Compose it by hand from the deployed commit:
`SCORING_GENERATION` (`src/scoring/generation.rs`), `ONNX_MODEL_ID`
(`src/toxicity/onnx.rs`), `EMBEDDING_MODEL_ID` (`src/topics/embeddings.rs`),
`NLI_MODEL_ID` (`src/scoring/nli.rs`), joined exactly as `compose_revision`
does. Getting it wrong is not destructive — it just hides the rows again — but
check it against a real row as soon as one exists.

### Option 1 — turn the nightly refresh off (preferred; reversible; safe)

**Use it when:** the refresh stampede is the problem — the classifier bill,
a saturated endpoint, a queue that will not drain. §2a is the stampede.

```bash
railway variables -s charcoal-web -e production --set CHARCOAL_REFRESH_INTERVAL_HOURS=0
```

`0` or `off` disables the tick; unset means 24 hours
(`src/web/refresh.rs:19-46`). This triggers a redeploy — that is fine and is
what you want, since the knob is read at boot.

**Correct result:** once the new container is healthy, the refresh tick stops
claiming users. Check that nothing new is being queued, twice, a minute apart:

```sql
SELECT kind, status, COUNT(*) FROM scan_queue GROUP BY kind, status;
```

**Correct reading:** the `refresh` / `queued` count is not larger on the second
reading than the first. Refreshes that were already running are *not* killed;
they finish normally.

**What this does not do — say it out loud before someone assumes otherwise:**
it brings nothing back. With refreshes off, every hidden row stays hidden and
nothing re-scores it on its own. A user gets their tier list back only by
running a full scan themselves. This is a brake on new work, not a rollback.
If you leave it off, put it in the deploy notes with a date and an owner — §7
item 6 is the same warning from the other direction.

### Option 2 — un-hide the existing scores in place (last resort)

**Use it when:** the empty dashboards are themselves the user-visible outage
and you cannot wait for refreshes to repopulate them. This is the only option
that brings tier lists back without re-scoring anything.

**Understand what you are doing first.** This is a deliberate lie to the
freshness system. The scores are not re-computed; you are stamping old numbers
with the current revision so the fresh predicate stops hiding them. They were
produced by the previous formula and the previous models, and the entire point
of this feature is that such scores should *not* be presented as current. You
are choosing a known-wrong number over a blank page, on purpose, temporarily.

Get the current revision string (previous section), then:

```sql
UPDATE account_scores
   SET scoring_generation = '<the current revision, verbatim>',
       valid_until = NOW() + CASE scoring_confidence
                               WHEN 'high' THEN INTERVAL '14 days'
                               WHEN 'low'  THEN INTERVAL '3 days'
                               ELSE INTERVAL '7 days'
                             END
 WHERE scoring_generation = 'legacy';
```

The `CASE` mirrors what a real score write uses — `staleness_days_for_label`
(`src/db/models.rs:93-127`): high 14 days, low 3 days, everything else
including NULL 7 days — so these rows expire on the schedule they would have
had rather than all at once. **Keep the `WHERE` clause**: it leaves rows the
new binary genuinely wrote alone.

**Correct result:**

```sql
SELECT scoring_generation, COUNT(*) FROM account_scores GROUP BY 1;
```

one row, the current revision, with a count equal to the `rows_total` you wrote
down in §0.3 — and `GET /api/status` showing `tier_counts.expired` back to `0`
with the pre-deploy tier counts restored.

**Then close the loop.** Leave the refresh job **on** so High/Elevated accounts
get genuinely re-scored within their window, or ask each affected user to run a
full scan. Write down what you did, to which rows, and when — otherwise the
next person reading `scoring_generation` will believe those numbers are fresh.

### Option 3 — actually roll the binary back (most dangerous)

**Use it only when** the new binary is broken in a way Options 1 and 2 do not
contain. **Take a database backup first.** The schema must be reversed
*before* the old container starts, and both halves have to hold.

```sql
ALTER TABLE account_scores ALTER COLUMN valid_until DROP NOT NULL;
ALTER TABLE account_scores ALTER COLUMN scoring_generation SET DEFAULT 'legacy';
DELETE FROM schema_version WHERE version = 18;
```

What each line is for:

- **Line 1** is the one that stops the old binary's score writes from failing.
  It is the not-null constraint that breaks it, not the column's existence.
- **Line 2** restores the default v18 dropped, so the old INSERT — which names
  neither column — has something to write into `scoring_generation`.
- **Line 3** removes v18's self-recorded version row. Without it, a later
  redeploy of the *new* binary sees version 18 already recorded, skips the
  migration (`src/db/postgres.rs:396-404`), and you are left running the new
  code against a `valid_until` that is nullable and a `scoring_generation` that
  silently defaults to `legacy` — exactly the state v18 exists to prevent.

**Do not drop the columns.** They are ignored by the old binary and re-used by
the new one. The contents of `valid_until` cannot be recovered except by
re-scoring everything.

**Correct result:**

```sql
SELECT MAX(version) FROM schema_version;
SELECT column_name, is_nullable, column_default
  FROM information_schema.columns
 WHERE table_name = 'account_scores'
   AND column_name IN ('valid_until', 'scoring_generation');
```

**Correct reading:** max version `17`; `valid_until` with `is_nullable = YES`;
`scoring_generation` with `column_default = 'legacy'::text`. Only then redeploy
the previous image.

Afterwards the old binary writes rows with `scoring_generation = 'legacy'` and
`valid_until = NULL` — which is exactly the shape v18 backfills when you
eventually roll forward again.

**What this does not reverse, deliberately:** everything else v18 added.
`scan_queue.kind` / `full_requested_at` / `completion`, `users.next_refresh_at`
/ `refreshed_generation` / `refresh_attempted_generation`,
`topic_fingerprint.embedding_model_id`, the `scan_state.last_full_scan_finished_at`
backfill and the `idx_account_scores_user_score` index all stay. They are
purely additive, the old binary ignores them, and removing them would only
create work for the roll-forward. Leave them.

---

## 3. First tick → a refresh for everyone

The refresh tick runs on the existing scan admitter loop, every **30 seconds**.
It claims at most **25 due users per tick**, in one transaction.

Within 30 seconds of the deploy:

```sql
SELECT user_did, kind, status, enqueued_at
  FROM scan_queue
 WHERE kind = 'refresh'
 ORDER BY enqueued_at;
```

**Correct reading:** a `kind = 'refresh'` row for every user who has score rows
— in batches of 25, so with more than 25 users this takes more than one tick.
Users get refresh rows because their `refresh_attempted_generation` is NULL
(v18 leaves it so) and therefore differs from the current revision; there is no
one-off timestamp backfill.

As each refresh finishes, check its bookkeeping:

```sql
SELECT key, value FROM scan_state
 WHERE user_did = '<DID>'
   AND key IN ('refresh_last_run_id', 'refresh_last_outcome',
               'refresh_last_run_at', 'refresh_candidates', 'refresh_scored',
               'refresh_feed_cache_hits', 'refresh_feed_cache_misses',
               'refresh_feed_cache_applicable')
 ORDER BY key;
```

**Correct reading:**

| Key | Expected |
|---|---|
| `refresh_last_outcome` | `completed` |
| `refresh_last_run_id` | the queue row's `claim_id` for that run |
| `refresh_candidates` | that user's pre-deploy High + Elevated count (§0.3) |
| `refresh_scored` | `refresh_candidates` minus that user's rows in `scan_skips` |
| `refresh_feed_cache_applicable` | `1` (it had candidates) |

The full set of `refresh_last_outcome` values, so you can read whatever you get:

| Value | Meaning | What happens next |
|---|---|---|
| `completed` | every due candidate re-scored | proves the revision, next run in 24 h |
| `nothing_due` | there was nothing stale | also proves the revision |
| `completed_with_skips` | finished, some accounts skipped | retry in 1 h, reads as degraded |
| `completed_unverified` | finished, the skip count could not be read | retry in 1 h, never proof |
| `deferred:full_scan_resumable` | a *full* scan's staged work is sitting there | retry in 1 h; the full scan drains it |
| `deferred:no_fingerprint` | no topic fingerprint | retry in 1 h **and** request a full scan |
| `deferred:incompatible_fingerprint` | fingerprint built by another embedding model | same |
| `resumable` | cost-capped or interrupted mid-run | retry in 1 h, resumes its own staging |

Then the revision proof and the headline check:

```sql
SELECT did, refreshed_generation, refresh_attempted_generation, next_refresh_at
  FROM users ORDER BY did;

SELECT COUNT(*) FROM account_scores
 WHERE scoring_generation = 'legacy' AND threat_score >= 15;
```

**Correct reading:** every user's `refreshed_generation` is the current
revision, `next_refresh_at` is about 24 hours out, and the `legacy` count at or
above Elevated is **zero**. Low and Watch rows stay `legacy` forever unless the
account re-engages — that is decision 777, not an omission.

---

## 4. The human still wins

Two cases. Both are about the same guarantee: a background job never costs a
user their place or their scan.

**(a) Click Scan while a refresh is *queued* (not yet running).**

```sql
SELECT kind, status, enqueued_at, full_requested_at
  FROM scan_queue WHERE user_did = '<DID>';
```

Note `enqueued_at`. Click Scan in the dashboard. Re-run the query.

**Correct reading:** `kind` has flipped to `full`, `enqueued_at` is
**unchanged** (the user keeps their place in line — queue order is FIFO across
kinds), and `full_requested_at` is now set.

**(b) Click Scan while a refresh is *running*.**

The API returns `202` with `queued: "after_refresh"` in the body. Then:

```sql
SELECT kind, status, full_requested_at FROM scan_queue WHERE user_did = '<DID>';
```

**Correct reading:** while the refresh runs, `status = 'running'`,
`kind = 'refresh'`, and `full_requested_at` is set — the request is written
down durably, so it survives a restart. When the refresh finishes, the same row
becomes `status = 'queued'`, `kind = 'full'`. The user does not click again.

**Cooldown.** In both cases the 24-hour cooldown is measured from
`scan_state.last_full_scan_finished_at`, never from a queue row's `done`
status:

```sql
SELECT value FROM scan_state
 WHERE user_did = '<DID>' AND key = 'last_full_scan_finished_at';
```

A finished refresh must not change it.

---

## 5. Interruption and resume

This proves a cost-capped refresh resumes its own work rather than starting
over.

Set the cost ceiling low enough to trip during the burst phase.
`CHARCOAL_SCAN_COST_CEILING_CENTS` is parsed in `src/toxicity/cost_meter.rs`
(unset or malformed = 500 cents) and enforced by the burst loop in
`src/pipeline/scan_phases/burst.rs`, which stops, keeps every score it already
wrote, and returns `CostCapped`:

```bash
railway variables -s charcoal-web -e staging --set CHARCOAL_SCAN_COST_CEILING_CENTS=1
```

`--set` without `--skip-deploys` redeploys the service, which is what you want
here — the process reads the ceiling from its environment, so it only takes
effect on a restart. **Wait for that deploy to finish before the next step**, or
the tick you force will be served by the old process with the old ceiling.

Force a tick for one user:

```sql
UPDATE users SET next_refresh_at = NOW() WHERE did = '<DID>';
```

Wait for the run to stop, then read:

```sql
SELECT key, value FROM scan_state
 WHERE user_did = '<DID>'
   AND key IN ('scan_phase', 'scan_run_kind', 'scan_run_generation',
               'refresh_last_outcome', 'refresh_candidates');
SELECT next_refresh_at FROM users WHERE did = '<DID>';
```

**Correct reading:** `scan_phase = burst`, `scan_run_kind = refresh`,
`scan_run_generation` = the current revision, `refresh_last_outcome =
resumable`, and `next_refresh_at` ≈ now + 1 hour (the retry cadence, not the
nightly one). `refresh_candidates` is the size of the staged set.

Now raise the cap back and force the tick again:

```bash
railway variables -s charcoal-web -e staging --set CHARCOAL_SCAN_COST_CEILING_CENTS=<normal>
```
```sql
UPDATE users SET next_refresh_at = NOW() WHERE did = '<DID>';
```

**Correct reading:** the same claim resumes at the burst phase — there is no
second gather, so the run is much faster than the first attempt — and ends
`refresh_last_outcome = completed` with `refresh_scored` equal to the
`refresh_candidates` you recorded. A refresh only ever resumes staging marked
`scan_run_kind = refresh` at the current revision; anything else it defers
(a full scan's leftovers) or discards (another revision's).

---

## 6. Feed cache — functional, not a rate

**There is no hit-rate threshold.** The ≥ 80 % target was withdrawn (deciduous
878, spec §6 Phase 2) because the arithmetic does not support it: a High score
is valid for 14 days and enters the 2-day refresh horizon about 12 days after
it was written, while feed snapshots live 24 hours. In steady state a refresh
can therefore only hit the cache for candidates who happened to be fetched in
the last day. That is the design, not a defect — the refresh job is **not** the
cache's main beneficiary; concurrent onboarding scans are.

**Do not raise `SNAPSHOT_TTL` to move this number.** A longer TTL means
scoring accounts on staler feeds, which is the opposite of what expiry exists
to fix.

What is checked instead is that the cache is *wired up*: three cases.

**(a) Warm — a hit is possible and happens.** Run a full scan for a user. Within
24 hours (while the snapshots are still alive), make their top-tier scores due:

```sql
UPDATE account_scores SET valid_until = NOW()
 WHERE user_did = '<DID>' AND threat_score >= 15;
UPDATE users SET next_refresh_at = NOW() WHERE did = '<DID>';
```

**Correct reading:** `refresh_feed_cache_applicable = 1` and
`refresh_feed_cache_hits >= refresh_feed_cache_misses` for those candidates.

**(b) Cold — a miss is possible and happens.**

**Staging only.** This empties the shared feed cache for *every* user, so the
next scans pay full fetch cost. It is safe (the cache is an optimisation and
rebuilds itself) but it is not something to do in production to satisfy a
runbook step.

```sql
DELETE FROM account_feed_snapshots;
UPDATE users SET next_refresh_at = NOW() WHERE did = '<DID>';
```

**Correct reading:** `refresh_feed_cache_hits = 0`. This is the negative
control: it proves (a) measured something real rather than always reporting a
hit.

**(c) No-op — nothing due.** Force a tick with nothing expiring.

**Correct reading:** `refresh_last_outcome = nothing_due`,
`refresh_feed_cache_applicable = 0`, and both counters `0`. The `applicable`
flag exists so "0 hits out of 0 candidates" is never read as a 0 % hit rate.

**Steady-state hit share over the first two weeks:** _(record the number here
after the first fortnight in production; no threshold attaches to it)_

| Date | User | `refresh_candidates` | hits | misses | share |
|---|---|---|---|---|---|
| | | | | | |

---

## 7. Revision change procedure

When you change the scoring revision (by swapping a model, or by bumping
`SCORING_GENERATION` per §0.2), this is what happens and what to watch.

1. **An in-binary model swap needs no bump.** The revision changes by itself.
2. **A classifier model or policy change needs a manual bump** — see §0.2. It
   is a deploy checklist item because the classifier's identity is outside the
   binary and cannot be composed in.
3. **Deploy single-replica** (Railway's default). §0.5 covers the overlap.
4. **§2 and §3 replay automatically.** Every row is hidden; every user becomes
   due because `refresh_attempted_generation` no longer matches the current
   revision; the ticks work through them 25 at a time.
5. **A failed first attempt retries hourly, not every tick.** The tick stamps
   `refresh_attempted_generation` when it *attempts*, so a user is not
   re-selected 30 seconds later; `refreshed_generation` is only written by work
   that actually completed. Watch it with:

```sql
SELECT did, next_refresh_at, refresh_attempted_generation, refreshed_generation
  FROM users ORDER BY next_refresh_at NULLS FIRST;
```

   A user whose `refresh_attempted_generation` is current but whose
   `refreshed_generation` is not, with `next_refresh_at` an hour out, is a
   failed attempt waiting for its retry — that is the system working.

6. **If refreshes are disabled** (`CHARCOAL_REFRESH_INTERVAL_HOURS=0` or `off`)
   the lists stay hidden until users re-engage. Nothing brings scores back on
   its own. If you disable the job as part of a revision change, say so in the
   deploy notes — otherwise the empty dashboards have no explanation and no end
   date.

### 7b. Completion bookkeeping

Three checks that the cooldown marker means what it says. The marker
(`scan_state.last_full_scan_finished_at`) is now the *only* cooldown anchor.

**(i) A cost-capped full scan starts no cooldown.** Set
`CHARCOAL_SCAN_COST_CEILING_CENTS` low, run a full scan, let it cost-cap.

**Correct reading:** `last_full_scan_finished_at` is **unchanged**, the queue
row records `completion = 'resumable'`, `full_requested_at` is still set, and
the user's next click is admitted immediately — no 429.

`scan_queue.completion` is the durable record of how an attempt ended. The
five values, and what each one earns:

| `completion` | Cooldown marker | Clears the full-scan obligation | Proves the revision |
|---|---|---|---|
| `complete` | yes | yes | yes |
| `complete_with_skips` | yes | yes | no |
| `complete_unverified` | yes | yes | no |
| `resumable` | no | no | no |
| `failed` | no | no | no |

**(ii) An interrupted drain keeps the obligation.** Request a full scan while a
refresh is running (§4b), then cost-cap the resulting drain.

**Correct reading:** the queue row still ends up `queued` / `kind = 'full'`, the
cooldown is not started by the drain, and no further click is needed. A drain
alone never completes a user's full-scan request — only the run's own gather
reaching `done` does.

**(iii) One skipped account still fulfils the request.** Cause one account to
be skipped in an otherwise complete refresh (a feed fetch returning 4xx — use a
deactivated or suspended account as the fixture).

**Correct reading:** `refresh_last_outcome = completed_with_skips`,
`users.refreshed_generation` **unchanged** (not proof), `next_refresh_at` ≈ now
+ 1 hour, and the other accounts' new scores are present. Check the skip:

```sql
SELECT account_did, phase, error, skipped_at FROM scan_skips
 WHERE user_did = '<DID>';
```

---

## 8. Index plans

The refresh candidate query is the one new hot read this phase adds. These
plans are the record that it is indexed and that the index earns its place;
they were captured during implementation (Task 7, commit `3a51da5`) and are
reproduced in full here so nothing is lost with the scratch notes.

**Environment.** PostgreSQL 17.10 (Homebrew) on aarch64-apple-darwin25.4.0,
database `charcoal_test`, idle machine (an earlier pass under concurrent
`cargo test` load showed an 18 ms outlier that disappeared on an idle box).

**Fixture.** Two throwaway owner DIDs under the prefix `did:plc:idxm_`, 6 000
rows total, seeded by a scratch script:

- `did:plc:idxm_usera` — 3 000 rows, the steady-state workload: 2 940 at the
  current revision / 60 `legacy`; 90 expiring inside the 2-day horizon, the
  rest valid for 20 days; 360 rows clear the `threat_score >= 15` floor, of
  which 150 are due.
- `did:plc:idxm_userb` — 3 000 rows, the revision-bump workload: all 3 000
  stamped `legacy`, all valid for 20 days, so every one of the 360
  score-qualifying rows is a candidate and the residual filter rejects nothing.

`ANALYZE account_scores` was run after the load. Afterwards
`DELETE FROM account_scores WHERE user_did LIKE 'did:plc:idxm_%'` removed all
6 000 rows and `ANALYZE` was re-run; the table was verified back at 0 rows and
the 72-test `db_postgres` suite passed afterwards.

**Statement.** Exactly the one `src/db/postgres.rs` sends, issued through
`PREPARE`/`EXECUTE` so the plan is the one the extended-query protocol (sqlx)
produces:

```sql
PREPARE rc (text, float8, int, text) AS
SELECT did, handle, graph_distance FROM account_scores
 WHERE user_did = $1
   AND threat_score >= $2
   AND (scoring_generation <> $4
        OR valid_until <= NOW() + make_interval(days => $3))
 ORDER BY threat_score DESC, did;
```

bound with `($owner, 15.0, 2, scoring_revision())`. Each plan below was taken
after two warm-up executions.

**Headline:** the planner picks `idx_account_scores_user_score` on its own — it
does **not** seq-scan, so the `enable_seqscan = off` fallback was not needed to
demonstrate usability; it is recorded anyway and produces the identical plan.
Every case is sub-millisecond, far below the 50 ms threshold at which a partial
index would have been warranted. **No partial index is needed.**

### PLAN A1 — user A (95 % fresh / 3 % expiring / 2 % legacy), `enable_seqscan = on` (default)

```
 Sort  (cost=177.94..178.41 rows=186 width=53) (actual time=0.471..0.482 rows=150 loops=1)
   Sort Key: threat_score DESC, did
   Sort Method: quicksort  Memory: 35kB
   Buffers: shared hit=45
   ->  Bitmap Heap Scan on account_scores  (cost=15.93..170.93 rows=186 width=53) (actual time=0.060..0.332 rows=150 loops=1)
         Recheck Cond: ((user_did = 'did:plc:idxm_usera'::text) AND (threat_score >= '15'::double precision))
         Filter: ((scoring_generation <> '2026-09-13|onnx=detoxify-unbiased-toxic-roberta-quantized|emb=all-MiniLM-L6-v2|nli=nli-deberta-v3-xsmall-fp32'::text) OR (valid_until <= (now() + '2 days'::interval)))
         Rows Removed by Filter: 210
         Heap Blocks: exact=40
         Buffers: shared hit=45
         ->  Bitmap Index Scan on idx_account_scores_user_score  (cost=0.00..15.88 rows=360 width=0) (actual time=0.046..0.047 rows=360 loops=1)
               Index Cond: ((user_did = 'did:plc:idxm_usera'::text) AND (threat_score >= '15'::double precision))
               Buffers: shared hit=5
 Planning Time: 0.144 ms
 Execution Time: 0.517 ms
```

### PLAN B1 — user B (3 000 rows all `legacy`), `enable_seqscan = on` (default)

```
 Sort  (cost=177.94..178.41 rows=186 width=53) (actual time=0.771..0.791 rows=360 loops=1)
   Sort Key: threat_score DESC, did
   Sort Method: quicksort  Memory: 50kB
   Buffers: shared hit=51
   ->  Bitmap Heap Scan on account_scores  (cost=15.93..170.93 rows=186 width=53) (actual time=0.078..0.231 rows=360 loops=1)
         Recheck Cond: ((user_did = 'did:plc:idxm_userb'::text) AND (threat_score >= '15'::double precision))
         Filter: ((scoring_generation <> '2026-09-13|onnx=detoxify-unbiased-toxic-roberta-quantized|emb=all-MiniLM-L6-v2|nli=nli-deberta-v3-xsmall-fp32'::text) OR (valid_until <= (now() + '2 days'::interval)))
         Heap Blocks: exact=47
         Buffers: shared hit=51
         ->  Bitmap Index Scan on idx_account_scores_user_score  (cost=0.00..15.88 rows=360 width=0) (actual time=0.067..0.067 rows=360 loops=1)
               Index Cond: ((user_did = 'did:plc:idxm_userb'::text) AND (threat_score >= '15'::double precision))
               Buffers: shared hit=4
 Planning Time: 0.180 ms
 Execution Time: 0.835 ms
```

### PLAN A2 — user A, `SET enable_seqscan = off`

```
 Sort  (cost=177.94..178.41 rows=186 width=53) (actual time=0.343..0.352 rows=150 loops=1)
   Sort Key: threat_score DESC, did
   Sort Method: quicksort  Memory: 35kB
   Buffers: shared hit=45
   ->  Bitmap Heap Scan on account_scores  (cost=15.93..170.93 rows=186 width=53) (actual time=0.042..0.241 rows=150 loops=1)
         Recheck Cond: ((user_did = 'did:plc:idxm_usera'::text) AND (threat_score >= '15'::double precision))
         Filter: ((scoring_generation <> '2026-09-13|onnx=detoxify-unbiased-toxic-roberta-quantized|emb=all-MiniLM-L6-v2|nli=nli-deberta-v3-xsmall-fp32'::text) OR (valid_until <= (now() + '2 days'::interval)))
         Rows Removed by Filter: 210
         Heap Blocks: exact=40
         Buffers: shared hit=45
         ->  Bitmap Index Scan on idx_account_scores_user_score  (cost=0.00..15.88 rows=360 width=0) (actual time=0.033..0.033 rows=360 loops=1)
               Index Cond: ((user_did = 'did:plc:idxm_usera'::text) AND (threat_score >= '15'::double precision))
               Buffers: shared hit=5
 Planning Time: 0.126 ms
 Execution Time: 0.376 ms
```

### PLAN B2 — user B, `SET enable_seqscan = off`

```
 Sort  (cost=177.94..178.41 rows=186 width=53) (actual time=1.132..1.167 rows=360 loops=1)
   Sort Key: threat_score DESC, did
   Sort Method: quicksort  Memory: 50kB
   Buffers: shared hit=51
   ->  Bitmap Heap Scan on account_scores  (cost=15.93..170.93 rows=186 width=53) (actual time=0.037..0.220 rows=360 loops=1)
         Recheck Cond: ((user_did = 'did:plc:idxm_userb'::text) AND (threat_score >= '15'::double precision))
         Filter: ((scoring_generation <> '2026-09-13|onnx=detoxify-unbiased-toxic-roberta-quantized|emb=all-MiniLM-L6-v2|nli=nli-deberta-v3-xsmall-fp32'::text) OR (valid_until <= (now() + '2 days'::interval)))
         Heap Blocks: exact=47
         Buffers: shared hit=51
         ->  Bitmap Index Scan on idx_account_scores_user_score  (cost=0.00..15.88 rows=360 width=0) (actual time=0.028..0.028 rows=360 loops=1)
               Index Cond: ((user_did = 'did:plc:idxm_userb'::text) AND (threat_score >= '15'::double precision))
               Buffers: shared hit=4
 Planning Time: 0.077 ms
 Execution Time: 1.232 ms
```

### Counterfactual — the same query with the index dropped

The plans above show the index is *used*; this shows it is *load-bearing*.
Taken inside `BEGIN; DROP INDEX idx_account_scores_user_score; …; ROLLBACK;`
(Postgres DDL is transactional; `pg_indexes` was queried afterwards and the
index is still present). The revision literal is elided in this capture:

```
 Sort  (cost=274.84..275.31 rows=186 width=53) (actual time=1.171..1.247 rows=360 loops=1)
   Sort Key: threat_score DESC, did
   Sort Method: quicksort  Memory: 50kB
   Buffers: shared hit=124
   ->  Bitmap Heap Scan on account_scores  (cost=46.83..267.83 rows=186 width=53) (actual time=0.191..0.750 rows=360 loops=1)
         Recheck Cond: (user_did = 'did:plc:idxm_userb'::text)
         Filter: ((threat_score >= '15'::double precision) AND ((scoring_generation <> '2026-09-13|onnx=…|nli=…'::text) OR (valid_until <= (now() + '2 days'::interval))))
         Rows Removed by Filter: 2640
         Heap Blocks: exact=114
         Buffers: shared hit=118
         ->  Bitmap Index Scan on idx_scores_age  (cost=0.00..46.78 rows=3000 width=0) (actual time=0.076..0.077 rows=3000 loops=1)
               Index Cond: (user_did = 'did:plc:idxm_userb'::text)
               Buffers: shared hit=4
 Planning Time: 0.533 ms
 Execution Time: 1.294 ms
```

Without it the planner falls back to `idx_scores_age` (the `user_did` prefix
only), reads all 3 000 of the user's rows and discards 2 640 at the filter,
with 124 shared buffers against 51 and 114 heap blocks against 47. At 3 000
rows per user the wall-clock difference is fractions of a millisecond; the
*ratio* is the point, and it is why the index earns its place before the table
gets large.

### SQLite plan (same shape)

`EXPLAIN QUERY PLAN` over the real `REFRESH_CANDIDATES_SQL`:

```
SEARCH account_scores USING INDEX idx_account_scores_user_score (user_did=? AND threat_score>?)
USE TEMP B-TREE FOR LAST TERM OF ORDER BY
```

Same shape as Postgres: an index seek on `(user_did, threat_score)` with the
expiry/revision predicate applied as a residual filter, and a sort for the
`did` tie-break only. That plan is asserted by a test
(`candidate_query_uses_the_user_score_index`), so a future edit that drops the
index or makes the predicate unindexable fails in CI.

**One caveat on the Postgres captures.** They are *custom* plans — literals
inlined, only two warm-ups. sqlx keeps prepared statements alive, so a
long-lived connection can switch to the *generic* plan after five executions of
the same statement. If this query ever misbehaves in production, re-capture
after six or more executions on one connection before concluding the index is
at fault.

### 8b. Suite isolation

The Postgres test suite's destructive migration fixtures touch only the
dedicated `*_migrations` database and are serialized by a process mutex plus a
Postgres session advisory lock. The evidence that this holds under the suite's
normal parallelism is ten consecutive full runs:

```bash
for i in $(seq 10); do
  DATABASE_URL=postgres://$USER@localhost/charcoal_test \
  DATABASE_URL_MIGRATIONS=postgres://$USER@localhost/charcoal_test_migrations \
    cargo test --all-targets --features postgres || break
done
```

Ten runs came back clean during Task 9. **Known-narrow residual flake shape:**
a test whose user is "due" for a refresh can have its tick batch limit eaten by
*other* tests' due users in the same database, because the batch is bounded at
25 users globally rather than per test. Ten consecutive clean runs make this a
watch item, not an open defect. If a tick-related Postgres test fails once and
passes on a re-run, that is the shape to suspect first.

---

## 9. Migration rehearsal (`charcoal migrate`)

`charcoal migrate` now copies score rows *verbatim* rather than through the
ranked-threats query — which would have silently dropped NULL-score rows and,
after this phase, every hidden row too. Rehearse it on a copy of the prod
SQLite database before trusting it.

```bash
createdb charcoal_migrate_rehearsal
CHARCOAL_DB_PATH=backups/<a copy of the prod sqlite file> \
  cargo run --features postgres -- migrate \
  --database-url postgres://$USER@localhost/charcoal_migrate_rehearsal
```

Compare source and destination:

```bash
sqlite3 backups/<copy>.db 'SELECT COUNT(*), COUNT(*) FILTER (WHERE threat_score IS NULL), MIN(scored_at), MAX(valid_until) FROM account_scores;'
psql charcoal_migrate_rehearsal -tAc 'SELECT COUNT(*), COUNT(*) FILTER (WHERE threat_score IS NULL), MIN(scored_at), MAX(valid_until) FROM account_scores;'
```

**Correct reading:** the two counts match, including the NULL-score count.
Timestamps match to the second — SQLite stores whole seconds in its column
form, so a Postgres → SQLite copy truncates; SQLite → Postgres does not lose
anything.

Now the NULL/malformed expiry conversion. Count them in the source:

```bash
sqlite3 backups/<copy>.db 'SELECT COUNT(*) FROM account_scores WHERE datetime(valid_until) IS NULL;'
```

That counts both NULL and unparseable values — `datetime()` returns NULL for
both, which is also why both read as *expired* everywhere in SQLite. Then:

```bash
psql charcoal_migrate_rehearsal -tAc 'SELECT COUNT(*) FROM account_scores WHERE valid_until = scored_at;'
```

**Correct reading:** the same number. Postgres cannot hold a malformed
timestamp and its column is `NOT NULL`, so those rows land as
`valid_until = scored_at` — expired the instant they were scored. Never
renewed, never dropped. The raw malformed text is logged, not stored.

**Run the migrate a second time against the same destination.**

**Correct reading:** nothing changes — same counts, same timestamps. Importing
never renews an expiry, so a repeated migrate is a no-op rather than a quiet
extension of everybody's validity.

---

## Known gaps

These are known and deliberate. They are listed so nobody spends deploy day
debugging them.

1. **`GET /api/status` reports "scan running" during a nightly refresh**
   (#365). The status payload carries no run *kind*, so a background refresh is
   indistinguishable from a scan the user started. The progress *message* is
   refresh-specific; the `scan_running` flag and its surrounding copy are not.
   A user may see "scanning" without having asked for a scan. Fix is a `kind`
   field on `ScanQueueEntry` plus refresh-specific copy.
2. **A refresh's completion line still says "0 events."** A refresh discovers
   nothing — it reads candidates from the table — so "Refresh complete: 0
   events, 12 accounts scored" is accurate but reads as a failure. Copy
   decision, pairs with #365.
3. **Reads outside the tier list still show expired scores as current.** The
   account-detail page and the typeahead search do not apply the fresh
   predicate, so an account whose score is hidden from the tier list can still
   be looked up and shown with that score, undated. This is out of this plan's
   scope, and it is the one place where a user could act on a stale number
   without being told it is stale. Worth filing before this reaches more than a
   handful of users.
4. **The Postgres suite has no CI coverage.** `.github/workflows/ci.yml` runs
   `cargo test --features web` only. Everything Postgres-specific in this phase
   — the v18 migration, the conditional queue write, the advisory locks, the
   candidate query — is gated solely by a local run. Run `VERIFY_PG` locally
   before merging anything that touches `src/db/postgres.rs` or
   `migrations/postgres/`.
5. **The drain's production wiring is compile-checked only.** `drain_then_run`
   is tested end to end against the real phased pipeline, but the call site that
   builds the real dependencies needs ~500 MB of models and so is not covered by
   a test. §7b(ii) of this runbook is where it gets exercised for real.
6. **The classifier probe adds one endpoint round trip per scan start**, on
   both kinds. It is the FlashBoot warm-up the RunPod client was written to
   absorb, so it is not new work in spirit — but it is a new request on the
   critical path of every scan, and it is the call site to remember if RunPod
   cold-start latency is ever the thing being tuned.
7. **The probe only covers the RunPod backend.** Zentropi's identity is also
   operator-declared (`ZENTROPI_LABELER_VERSION_ID`) and its probe is a no-op, so
   a Zentropi deployment discovers a policy mismatch at finalize rather than at
   scan start.
