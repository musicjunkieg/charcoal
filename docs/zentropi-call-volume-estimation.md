# Estimating Zentropi call volume across the Bluesky network

This document describes the `estimate` tool: what it's for, how it works, the
math behind it, and the design decisions made along the way. It lives behind
the `estimate` Cargo feature and is built as a separate binary:

```bash
cargo run --features estimate --bin estimate -- --help
```

## The question it answers

> How many Zentropi API calls does a single protected account generate, and
> what does that look like across the whole network?

For one account the answer is "it depends" — Zentropi is a *second-stage*
classifier that only fires on posts which survive a first-stage ONNX filter, and
the dominant cost comes from follower fan-out that scales with how much hostile
amplification an account attracts. A single person is a sample of one and can't
be averaged up to a network. This tool estimates the network number empirically
instead of guessing.

## Where Zentropi calls actually come from

Tracing the scan pipeline, Zentropi is invoked from exactly one place:

- `TwoStageToxicityScorer::classify_post` (`src/toxicity/ensemble.rs`), reached
  via `classify_batch_with_contexts`, and only for posts whose ONNX score is at
  or above `ONNX_CLEAN_THRESHOLD` (0.10). Posts below that are "cleared" with no
  Zentropi call.

That method is called from `scoring::profile::build_profile`, which the
amplification pipeline runs on:

1. **Amplifiers** — every distinct account that quoted/reposted/replied to/liked
   the protected user's recent posts.
2. **Followers of quote/reply amplifiers** — up to 50 each. Reposts and likes do
   *not* trigger follower analysis.

### The cost model

Let:

- `A` = distinct amplifiers
- `Q` = distinct **quote/reply** amplifiers (the only ones triggering follower
  fan-out)
- `f` = followers scored per quote/reply amplifier (capped at 50)
- `p̄` = average posts per scored account that clear the ONNX 0.10 gate (0–50)

Then, roughly:

```
Zentropi calls ≈ p̄ × (A + Q·f)
```

The `Q·f` term dominates — one quote/reply amplifier drags in up to 50 follower
scans. So the single most important per-account number is **Q**, and because
engagement is power-law distributed, the network total is driven by a small
viral/heavily-targeted tail. That shapes the entire estimation strategy: measure
`Q` cheaply for everyone, and reweight so the tail counts exactly as much as it
really does.

> Note: this models a scan with **NLI disabled**. With NLI on, followers scoring
> above the Watch threshold get re-scored in a second `build_profile` pass that
> re-runs the toxicity classification, roughly doubling their Zentropi calls. The
> tool's numbers are therefore a **lower bound** for an NLI-enabled deployment.

## Pipeline architecture

The tool is a five-stage pipeline. Stages 1–3 use only free, public, read-only
APIs (no Zentropi calls, no third-party content sent anywhere). Stage 4 runs the
real scoring code with a counting scorer that still makes no Zentropi calls.

```
1. Harvest      firehose + topic search → candidate DIDs        (free)
2. Filter       getProfiles viability gates                     (free)
3. Stratify     Constellation backlinks → Q → engagement strata (free)
4. Dry-run      real build_profile + CountingScorer → counts    (free, needs ONNX)
5. Aggregate    per-stratum distribution, reweighted estimate   (pure)
```

| Stage | Module(s) |
|-------|-----------|
| Harvest | `discovery/jetstream.rs`, `discovery/seeds.rs`, `discovery/candidate.rs` |
| Filter | `discovery/profile_filter.rs` |
| Stratify | `discovery/engagement.rs` |
| Dry-run | `discovery/counting_scorer.rs`, `discovery/dry_run.rs` |
| Aggregate | `discovery/aggregate.rs` |
| Driver | `src/bin/estimate.rs` |

### Stage 1 — Harvest candidate accounts

The population that matters is *protected users* — people who'd run Charcoal —
not random Bluesky accounts. That population self-selects toward higher
engagement and the sensitive topic areas, so we harvest from two complementary
sources and union them:

- **Jetstream firehose** (`jetstream.rs`): an activity-weighted sample of
  accounts posting right now. High-volume posters appear more often, which is the
  right bias for capacity planning. Pure parse/sample core
  (`extract_post_author`, `AuthorSampler`) plus a thin WebSocket shell.
- **Topic-keyword search** (`seeds.rs`): authors active in the sensitive topic
  areas from `SPEC.md`, found by searching `app.bsky.feed.searchPosts` for a
  fixed seed keyword set. Thin wrapper over the existing
  `topic_search::search_posts_for_authors`.

`candidate.rs::merge_candidates` deduplicates the two DID lists and records
provenance (`firehose` / `topic` / `both`).

### Stage 2 — Filter to viable accounts

`profile_filter.rs` fetches each candidate's profile via batched
`app.bsky.actor.getProfiles` (25/request) and drops accounts that can't be
scored. The filter is deliberately **conservative**: its job is to remove
*non-viable* accounts (bots, eggs, deactivated), **not** low-engagement ones — a
real account with zero amplification is a legitimate low-end data point, not
noise. Defaults: `min_posts = 5` (Charcoal treats fewer as "Insufficient Data");
follower and account-age gates off.

### Stage 3 — Engagement stratification

`engagement.rs` measures each candidate's cost driver for free: it fetches their
recent posts and queries Constellation backlinks for quote/repost events (and
optionally likes and drive-by replies), then computes:

- `A` = distinct amplifiers across all event types
- `Q` = distinct quote/reply amplifiers (the fan-out driver)

`assign_stratum(Q)` buckets candidates into `none / low / medium / high / viral`.
Bucketing matters because the final estimate samples *within* strata and
reweights — so the expensive tail gets resolution instead of being averaged
away.

### Stage 4 — Instrumented dry-run

`counting_scorer.rs` defines `CountingScorer`, a `ToxicityScorer` **decorator**
over the real ONNX scorer. It implements the trait so it drops into
`build_profile` unchanged, replicates the exact two-stage gate (reusing the
shared `ONNX_CLEAN_THRESHOLD` and `format_parent_reply`), and increments an
atomic counter where a Zentropi call *would* happen — without making it.

`dry_run.rs` drives the real `build_profile` (the actual Zentropi call site) over
exactly the accounts a scan would score: amplifiers plus deduplicated followers
of quote/reply amplifiers. Per-candidate counts come from a before/after
snapshot of the shared counter. It also reports the **ONNX clean-pass rate** and
posts-classified, the upstream quantities that determine `p̄`.

### Stage 5 — Aggregation and reweighting

`aggregate.rs` rolls per-candidate counts into a network estimate: per-stratum
mean / median / p90 / p99 / max, a single **population-reweighted expected calls
per candidate**, and an optional projected network total.

The reweighting is the statistical heart of the tool:

```
expected_calls_per_candidate = Σ_stratum  population_share(stratum) × sample_mean(stratum)
```

If you supply `--population-weights` (the true stratum shares from a large free
Stage-3 run), the per-stratum means from a smaller — possibly tail-oversampled —
dry-run sample get corrected back to reality. Without weights, it falls back to
the sample's own distribution, in which case the expected value is just the plain
sample mean. Strata that carry population weight but had no dry-run samples are
surfaced as a downward-bias warning rather than silently dropped.

## The intended workflow

1. **Stratify the population (free, broad).** Run Stages 1–3 over a large harvest
   to learn the true stratum shares:
   ```bash
   cargo run --features estimate --bin estimate -- \
     --firehose-target 5000 --per-keyword 200 --json > population.json
   ```
   Tally the `stratum` field to get counts per stratum.

2. **Dry-run a sample (costlier, tail-oversampled).** Run Stage 4 on a sample,
   deliberately including viral/high accounts for resolution. Requires the ONNX
   model (`charcoal download-model`):
   ```bash
   cargo run --features estimate --bin estimate -- \
     --dry-run --firehose-target 300 \
     --population-weights "none=4200,low=650,medium=110,high=30,viral=10" \
     --population-size 5000
   ```

3. **Read the estimate.** The output gives per-stratum distributions, the
   reweighted expected calls/candidate, and the projected network total — with
   the median and tail visible, which is the honest answer rather than a
   misleading single average.

### Initial scan vs. steady state

The initial scan is the expensive one — everything is "stale" and gets scored.
Re-scans skip any account scored within 7 days (`is_score_stale(..., 7)`), so
ongoing load is much lower. The dry-run models an initial scan (a fresh run with
no prior scores). Keep that distinction in mind when sizing a token budget:
project the initial-scan number across onboarding, and a smaller recurring number
for steady state.

## Key CLI flags

| Flag | Stage | Meaning |
|------|-------|---------|
| `--firehose-target`, `--firehose-seconds`, `--jetstream-url` | 1 | Firehose sampling size/time/endpoint |
| `--per-keyword`, `--skip-firehose`, `--skip-topic` | 1 | Topic harvest size; disable a source |
| `--skip-filter`, `--min-posts`, `--min-followers`, `--min-account-age-days` | 2 | Viability gates |
| `--skip-engagement`, `--max-posts`, `--include-likes`, `--include-replies`, `--concurrency` | 3 | Engagement measurement |
| `--dry-run`, `--max-followers` | 4 | Run the counter (needs ONNX) |
| `--population-weights`, `--population-size` | 5 | Reweight + project |
| `--json` | all | Machine-readable output |

Environment: `PUBLIC_API_URL`, `CONSTELLATION_URL`, and `CHARCOAL_MODEL_DIR` are
read from the standard Charcoal config.

## Design decisions and rationale

- **Separate binary behind an `estimate` feature.** This is research/capacity
  tooling, not part of the product. Feature-gating keeps the WebSocket dependency
  (`tokio-tungstenite`) and all sampling code out of the default `charcoal` build
  and its dependency tree, matching how `web` and `postgres` are handled. The
  explicit `[[bin]]` with `required-features` stops a plain `cargo build` from
  compiling it.

- **Counting wrapper scorer, not an instrumented mode.** The counter is a
  decorator implementing the existing `ToxicityScorer` trait. This means **zero
  changes to production code** — the shipping `TwoStageToxicityScorer` is
  untouched, with no `if dry_run` branch that could ever fire in production. It's
  faithful by construction (it counts in exactly the one method that reaches
  Zentropi and delegates the ONNX-only paths straight through) and unit-testable
  with a mock primary, so no ONNX model or network is needed to verify it. The
  one tradeoff — duplicating the `< 0.10` gate — is mitigated by importing the
  *same* threshold constant and reply-envelope helper the real scorer uses, so
  they can't drift.

- **Drive the real `build_profile`, not a reimplementation.** Stage 4 runs the
  actual scoring function over the actual accounts a scan scores, substituting
  only the scorer. That captures Stage-1 early-exit, the reply-weighted
  classification, and follower fan-out exactly, instead of re-deriving them.

- **Free engagement stratification before any scoring.** Everything upstream of
  the Zentropi gate (Constellation backlinks, ONNX) is free. Measuring `Q` for
  the whole population costs nothing and is what makes principled reweighting
  possible.

- **Strata + reweighting instead of a single average.** Because cost is
  power-law, a plain mean is dominated by or blind to the tail. Stratifying and
  reweighting by true population shares is what lets a small dry-run sample
  produce an unbiased network number with the tail represented.

- **Conservative viability filter.** Filtering removes non-scoreable accounts,
  not low-engagement ones, so the engagement distribution — the thing we're
  trying to measure — stays unbiased.

- **Graceful degradation everywhere.** A firehose disconnect, a failed keyword
  search, or an unresolvable profile is logged and skipped, never fatal. A run
  always produces whatever it could gather.

- **Privacy posture.** Stages 1–3 are public reads. Stage 4 runs ONNX locally and
  the counting scorer **never sends any third-party content to Zentropi**. The
  tool is safe to run broadly because no user text leaves the machine for
  classification.

## Test coverage

The pure logic of every stage is unit-tested (run with
`cargo test --features estimate`):

- `jetstream` — event parsing, sampler dedup/ordering/target
- `seeds` — seed keyword invariants
- `candidate` — merge/provenance/dedup, serialization
- `profile_filter` — every gate and reject reason, age-gate edge cases
- `engagement` — stratum boundaries, A/Q computation, event tallies
- `counting_scorer` — gate boundary (at/below threshold), reply envelope path,
  ONNX-only paths don't count, snapshot deltas
- `dry_run` — option defaults, empty result
- `aggregate` — percentile interpolation, reweighting corrects oversampling,
  projection, unsampled-stratum warning

## Known limitations

- **Lower bound for NLI deployments** — see the cost-model note above.
- **Network-dependent** — Stages 1–4 require outbound access to Bluesky,
  Jetstream, and Constellation; Stage 4 also requires the ONNX model. They can't
  run in a network-restricted sandbox.
- **Reply detection is opt-in** (`--include-replies`) because it's API-heavy; by
  default `Q` counts quote amplifiers only, slightly undercounting fan-out.
- **Seed keywords are a fixed list**, not exhaustive — they're a sampling
  instrument, not a per-user fingerprint.
