# Architecture Correction — Read Before Implementing

> **Status (2026-04-27):** Phases 1a, 1b, 2, 3, and 5 are landed on
> `feat/topic-first-discovery`. Topic-first sampling, reply-inclusive
> scoring, adaptive sampling, ONNX-clean-filter + Zentropi binary
> classifier, and the binary toxicity rate are in production code. Phase 4
> (firehose monitoring) remains deferred.
>
> **Priority:** This addendum corrects architectural assumptions in the
> existing codebase. Read this before working on any scoring, sampling,
> or discovery changes.

## The primary goal is predictive defense, not reactive detection

Per SPEC.md, Charcoal’s core value is finding accounts that will
probably harass the protected user **before they arrive**. People who
have already collided (amplification events, replies, quotes) are
secondary — the protected user can already see and block them. The
amplification pipeline is a *signal source* (someone who quote-dunked
you has followers who think like them), not the primary product.

This means the sweep/discovery pipeline is the most important code
path, not the amplification pipeline.

## Three things the current code gets wrong

### 1. Sampling the wrong posts

`fetch_recent_posts` in `src/bluesky/posts.rs` uses the
`posts_no_replies` filter. This builds a toxicity profile from original
timeline posts, which is the **least** informative sample for predicting
reply harassment. A person can post wholesome original content and be
vicious in replies.

The fix: fetch with `posts_with_replies` or `posts_and_author_threads`
as the primary call. Partition the response into replies (have `reply`
field) and originals. Score reply text for toxicity (weighted 70%) and
original text (weighted 30%). Batch-fetch parent posts via `getPosts`
to form context pairs. Remove the separate `fetch_reply_ratio` call —
reply ratio is derived from the same data.

**Target fingerprinting still matters.** Topic overlap is the gating
signal — without it the threat formula collapses to raw toxicity. Build
the target’s fingerprint from originals (chosen topics, not inherited
from whoever they’re arguing with). Fall back to including replies only
when there are fewer than 15 originals (reply-heavy accounts). Track
`fingerprint_quality` (Normal/Degraded/Unreliable) so downstream
scoring can account for noisy overlap. The protected user’s fingerprint
is always originals-only (unchanged).

### 2. Discovery is graph-first when it should be topic-first

`sweep.rs` walks followers-of-followers, then filters by topic overlap.
This fetches ~100K accounts to find maybe 500 relevant ones. Flip it:
use `app.bsky.feed.searchPosts` with keywords from the topic
fingerprint to find accounts posting about the same topics, then check
if they’re hostile. Topic-first discovery finds the “hasn’t collided
but probably will” pool directly.

Graph expansion still has value, but only as a targeted secondary pass
from known-hostile accounts (High/Elevated tier), not from all of the
protected user’s followers.

### 3. Fixed 50-post sample wastes budget

`build_profile` fetches 50 posts for every account. Most accounts
resolve with 25 (clearly clean + irrelevant topic). Implement
three-stage adaptive sampling: 25 → 50 → 100+ posts, with early
exit when ONNX scores are all < 0.10 and topic overlap is below
the gate threshold. Track `ScoringConfidence` (Low/Standard/High) on
`AccountScore` and re-score Low-confidence accounts sooner.

## Known bug: context score double application

In `src/scoring/profile.rs`, `context_score` both bypasses the benign
gate (in `apply_behavioral_modifier_contextual`) AND gets applied as a
multiplier (`1.0 + ctx * 0.5`). For concern trolls, this compounds in
a way that isn’t documented or intentional. The gate bypass should
consume the context signal — don’t multiply again on top of it.

## Scoring pipeline layering (target state)

```
ONNX model (free, local) — CLEAN-PASS FILTER ONLY
  → scores all posts, returns continuous 0.0-1.0
  → ONLY reliable for low scores (< 0.10 = genuinely clean)
  → high scores are NOT trustworthy — keyword triggering on identity
    terms means "fuck yeah fat liberation" and "fat people are
    disgusting" both score ~0.95+
  → posts < 0.10: cleared, skip Zentropi (cost savings)
  → posts >= 0.10: ALL sent to Zentropi (no "clearly toxic" band)

Zentropi CoPE (free tier, policy-steerable)
  → BINARY classifier: returns 1 (toxic) or 0 (not toxic)
  → sees everything ONNX scores >= 0.10 (not just ambiguous posts)
  → replies sent as (parent_text, reply_text) pairs
  → originals sent as solo posts
  → replaces Groq Safeguard (unsustainable cost)
  → single policy prompt, not per-category (split later for user config)
  → if free tier can't absorb volume: self-host CoPE (9B, open weights)
    or Llama Guard 4 12B on Mac Studio

Toxicity rate (the continuous signal for the threat formula)
  → zentropi_toxic_count / total_posts
  → ONNX does NOT contribute to the toxic count
  → this is what feeds the threat formula where avg_toxicity currently goes
  → weighted_toxicity() is removed (was a patch for ONNX's inability
    to distinguish intent; Zentropi's policy prompt handles this)

NLI cross-encoder (DeBERTa-v3-xsmall, local ONNX)
  → relationship inference (attack/contempt/support/etc.)
  → feeds context_score in threat formula
  → distinct from toxicity classification, does NOT replace Zentropi
```

## Discovery pipeline layering (target state)

```
Topic-first search (primary)
  → searchPosts by topic keywords from fingerprint
  → finds hostile accounts in topical neighborhood
  → daily cadence, cycling topic clusters

Threat-graph expansion (secondary)
  → fan out from High/Elevated accounts only
  → hostile accounts cluster, their followers are higher-signal

Firehose monitoring (real-time tripwire)
  → Jetstream WebSocket, filtered to protected user's DID
  → score-on-arrival for unscored strangers
  → instant alert for already-scored High accounts

Cold start bootstrap (one-time)
  → 2-4 week amplification scan to seed database
  → then steady-state layers take over
```

## Design doc

Full design, rationale, sampling math, implementation phases, and
testing strategy: `docs/plans/2026-04-02-topic-first-discovery-and-sampling.md`

## Upcoming: Shared scoring schema migration

The current `account_scores` table duplicates expensive data (toxicity
rate, embeddings, behavioral signals) per protected user. A schema
split into shared `account_profiles` and per-user
`user_threat_assessments` is needed before multi-user launch. Separate
design doc forthcoming. In the meantime: **do not tightly couple
toxicity computation with per-user threat scoring in new code.** Keep
them as distinct steps in `build_profile` so the split is clean.