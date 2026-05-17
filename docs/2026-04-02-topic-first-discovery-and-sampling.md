# Topic-First Discovery & Adaptive Sampling

**Date:** 2026-04-02
**Status:** Proposed

## Core Problem

Charcoal’s primary goal (per SPEC.md) is **predictive defense**: finding
accounts that share the protected user’s topic fingerprint, are behaviorally
hostile in replies/quotes generally, and haven’t found the protected user
yet. The current sweep pipeline (`sweep.rs`) approaches this graph-first —
walk followers-of-followers, then filter by topic overlap. This is
backwards. For 500 followers × 200 followers-of-followers, that’s
~100,000 accounts to discover before filtering, and 90%+ are topically
irrelevant. It’s expensive and wasteful.

Two additional problems compound this:

1. **Sampling the wrong posts.** `fetch_recent_posts` uses the
   `posts_no_replies` filter, which explicitly excludes replies — the exact
   posts where hostile behavior manifests. The toxicity profile is built
   from original timeline posts, which are the least informative signal for
   predicting whether someone will harass in replies.
1. **Fixed sample size.** Every account gets 50 posts scored regardless of
   whether the signal is clear at 10 posts or ambiguous at 50. No adaptive
   stopping, no confidence tracking.

## Architecture Pivot

### Three-layer discovery (replaces current sweep)

**Layer 1 — Topic-first discovery (primary, the new sweep):**
Search for accounts active in the protected user’s topic clusters using
`app.bsky.feed.searchPosts` with keywords extracted from the topic
fingerprint. Collect author DIDs, deduplicate against already-scored
accounts, score new ones. This finds hostile accounts in the topical
neighborhood directly — no graph walk required.

Run on a cadence (daily), cycling through topic clusters. Each cycle
searches 3-5 topic keywords, collects up to 200-300 new author DIDs,
and scores them. This replaces the bulk of `sweep.rs`.

**Layer 2 — Threat-graph expansion (secondary, targeted):**
When an account scores High or Elevated, fan out to *their* followers.
The insight: hostile accounts cluster. A person who follows three
known-High accounts is more likely to be a threat than a random
second-degree follower. This is a refinement pass, not primary discovery.

Only expand from accounts scored High or Elevated (maybe 20-50 accounts
total), not all of the protected user’s followers. Much cheaper than the
current full sweep.

**Layer 3 — Firehose monitoring (real-time tripwire):**
Subscribe to the Bluesky Jetstream (filtered firehose) and watch for
replies to, quotes of, and mentions of the protected user’s posts. When
an unscored stranger appears, score them on demand. If they’re already
scored High, alert immediately.

This catches anyone Layers 1-2 missed at the moment of collision. It
can’t do predictive defense (the collision already happened), but it
closes the gap.

**Bootstrap (one-time cold start):**
Scan the last 2-4 weeks of amplification events (existing
`amplification.rs` pipeline). Score the amplifiers and their immediate
context. This seeds the database before steady-state layers take over.

### Why this ordering matters

The current pipeline treats amplification events (people who already
collided) as the primary signal and the sweep as secondary background
work. The SPEC’s actual priority is reversed: the sweep (finding threats
before collision) is the core product. Amplification events are useful
mainly as signal sources — someone who quote-dunked you has followers
who think like them, and those followers are the predictive defense
targets.

## Sampling Changes

### Fetch replies, not just original posts

**Current:** `fetch_recent_posts` → `posts_no_replies` filter → scores
original timeline posts for toxicity. A separate `fetch_reply_ratio`
call counts replies but discards the reply text.

**New:** Make the reply-inclusive call (`posts_with_replies` or
`posts_and_author_threads`) the primary fetch. From the same API
response, extract:

- Reply posts (have `reply` field set) — score these for toxicity
- Original posts — use for topic fingerprinting and baseline toxicity
- Reply parent URIs — batch-fetch via `getPosts` (up to 25 URIs/call)
  to form context pairs for Zentropi or NLI scoring

This eliminates the separate `fetch_reply_ratio` call (reply ratio
computed from the same data) and gets the actual reply text for
toxicity scoring.

**Changes to `src/bluesky/posts.rs`:**

Add a new `PostSample` return type:

```rust
pub struct PostSample {
    /// Original posts (not replies, not quotes)
    pub originals: Vec<Post>,
    /// Reply posts with parent URI for context pair fetching
    pub replies: Vec<ReplyPost>,
    /// Quote posts with quoted URI
    pub quotes: Vec<Post>,
    /// Computed reply ratio (replies / total non-repost posts)
    pub reply_ratio: f64,
    /// Computed quote ratio (quotes / total non-repost posts)
    pub quote_ratio: f64,
    /// Total non-repost posts seen (denominator for ratios)
    pub total_posts: usize,
}

pub struct ReplyPost {
    pub post: Post,
    /// AT URI of the post being replied to
    pub parent_uri: String,
}
```

Add `fetch_posts_with_replies()` that returns `PostSample`. Keep
`fetch_recent_posts()` available for backward compatibility (protected
user’s fingerprint building still uses original posts only).

### Target account fingerprinting: originals-first with reply fallback

Topic overlap is the gating signal in the threat formula. Without a
target’s topic fingerprint, there’s no overlap score, and the
multiplicative formula (`tox * 70 * (1 + overlap * 1.5)`) collapses
to a raw toxicity ranking with no topical relevance. The overlap gate
(< 0.15 → capped at 25.0) can’t fire. Target fingerprinting is not
optional.

The question is which posts to fingerprint from, now that the fetch
returns both originals and replies via `PostSample`.

**Originals are better for fingerprinting.** An original post is a
*chosen* topic — the person decided to talk about this unprompted.
A reply like “you’re wrong and this is harmful” tells you the person
is hostile but not what topics they care about. Worse, replies inherit
the topic of the parent post, so fingerprinting from replies partially
captures the topics of *the people they’re arguing with*, not the
target’s own topical interests. That’s noise.

**But some accounts are reply-heavy.** An account that’s 90% replies
and 10% originals might have 4 original posts in a 3-month window.
TF-IDF needs enough documents for meaningful keyword extraction, and
the sentence embedding mean vector is unstable with < 10 posts. For
those accounts, originals-only fingerprinting produces garbage.

**The rule:**

```
if originals.len() >= 15:
    fingerprint from originals only
elif originals.len() + replies.len() >= 15:
    fingerprint from all posts (originals + replies)
    set fingerprint_quality = Degraded
else:
    fingerprint from all posts
    set fingerprint_quality = Unreliable
```

The 15-post threshold is the minimum for stable TF-IDF keyword
extraction and a non-degenerate embedding mean. Below it, the cosine
similarity is noisy enough that overlap-based gating becomes
unreliable.

**`fingerprint_quality` flag:** Store alongside the overlap score so
downstream logic can account for confidence. An `Unreliable` overlap
score should not trigger a strong gate decision in either direction —
don’t confidently gate someone out (could miss a threat) or
confidently amplify (could false-positive). For `Unreliable` accounts,
widen the tier boundary uncertainty or flag for re-scoring when more
data is available.

**Changes to `build_profile`:**

```rust
// Partition posts for different consumers
let fingerprint_posts: Vec<String> = if sample.originals.len() >= 15 {
    sample.originals.iter().map(|p| p.text.clone()).collect()
} else {
    // Fall back to all posts for fingerprinting
    sample.originals.iter()
        .chain(sample.replies.iter().map(|r| &r.post))
        .chain(sample.quotes.iter())
        .map(|p| p.text.clone())
        .collect()
};

let toxicity_posts: Vec<String> = sample.replies.iter()
    .map(|r| r.post.text.clone())
    .chain(sample.quotes.iter().map(|p| p.text.clone()))
    .chain(sample.originals.iter().map(|p| p.text.clone()))
    .collect();
```

The protected user’s fingerprint is always built from originals only
(unchanged — you want to know what *they* talk about, and they have
enough posts to make this work). This change only affects target
account fingerprinting.

### Reply-weighted toxicity

**Current:** `avg_toxicity` averages continuous ONNX scores across all
50 posts equally. This is doubly wrong: it treats all posts the same
regardless of type, and the underlying ONNX scores are unreliable for
identity-term-bearing content.

**New:** Compute toxicity rates from Zentropi binary labels, weighted
by post type:

```
reply_tox_rate = zentropi_toxic_replies / total_replies
original_tox_rate = zentropi_toxic_originals / total_originals
weighted_tox = reply_tox_rate * 0.7 + original_tox_rate * 0.3
```

Someone whose replies are flagged toxic 40% of the time but whose
original posts are flagged 0% of the time is much more concerning than
someone flagged 24% across all posts uniformly. The reply-weighted
rate surfaces the former, a flat rate buries them.

Fall back to flat rate (`zentropi_toxic / total_posts`) when there are
fewer than 5 replies/quotes (insufficient interactive data to weight).

### Adaptive sequential sampling

**Current:** `build_profile` always fetches 50 posts and scores all of
them. Every account costs the same.

**New:** Three-stage sampling with early stopping:

**Stage 1 (25 posts):** Fetch one page. Score with ONNX (free). Compute
preliminary topic overlap. ONNX is only used for early *exit* on
obviously clean accounts — it cannot be used to identify toxic accounts
due to identity-term keyword triggering.

- If topic overlap < gate threshold (0.15) AND all posts score ONNX
  < 0.10 → **stop, classify Low, confidence: low**. The account
  discusses different topics and contains no flagged language at all.
  This exits the cleanest ~50-60% of accounts in the sweep.
- If topic overlap >= 0.15 OR any posts score ONNX >= 0.10 → proceed
  to Stage 2 (where Zentropi makes toxicity decisions).
- There is no “clearly toxic” early signal from Stage 1 — ONNX high
  scores are unreliable, so you cannot shortcut to “strong signal”
  based on ONNX alone.

**Stage 2 (50 posts cumulative):** Fetch second page. Score all posts
with ONNX; send everything scoring >= 0.10 to Zentropi for binary
classification. Compute toxicity rate from Zentropi labels only.
Re-compute topic overlap with larger sample.

- Most accounts resolve here. If toxicity rate and overlap clearly place
  them in a tier (not within ±5 points of a tier boundary) → **stop,
  confidence: standard**.
- If near a tier boundary (within ±5 points of 8.0 Watch, 15.0
  Elevated, or 40.0 High thresholds) → proceed to Stage 3.

**Stage 3 (100+ posts):** Only for borderline accounts. Fetch additional
pages. Add Zentropi context-pair scoring for replies. Add NLI for
matched pairs. This is the current “two-pass NLI promotion” logic,
extended.

**Add `scoring_confidence` to `AccountScore`:**

```rust
pub enum ScoringConfidence {
    /// < 25 posts analyzed, early exit
    Low,
    /// 25-50 posts, standard sampling
    Standard,
    /// 50+ posts, full analysis with context pairs
    High,
}
```

Store in the DB. Use to prioritize re-scoring: `Low` confidence accounts
get re-scored sooner (3 days instead of 7).

### Context pair formation and ONNX → Zentropi triage

Zentropi returns a binary label: 1 (toxic) or 0 (not toxic). It does
not return a continuous score.

**Critical constraint: ONNX is only reliable for low scores.**

The unbiased-toxic-roberta model has a well-documented keyword
triggering problem: posts containing identity terms (fat, queer, trans,
gay, etc.) score near 1.0 regardless of intent. “Fuck yeah, fat
liberation!” and “fat people are disgusting” both score ~0.95+. The
model’s own creators at Unitary AI confirm this persists “even in the
unbiased model.” Academic research (QueerReclaimLex, 2024) shows F1
scores as low as 0.24 on reclaimed queer language. The score
distribution is bimodal — posts cluster near 0 and near 1, with very
little in the 0.2-0.6 range that would represent genuine uncertainty.

This means ONNX high scores are *not* trustworthy. An “ambiguous band”
triage (0.10-0.40) would catch almost no posts because the model
pushes identity-term-bearing text to the ceiling with false confidence.
The actual hard cases (ally vs. hostile use of identity terms) sit at
0.9+ looking “confident.”

**ONNX is a clean-pass filter, not a toxicity detector.**

A post scoring 0.03 genuinely doesn’t contain hostile language or
identity terms. That’s a trustworthy signal. A post scoring 0.85 could
be an ally or an attacker. ONNX’s only reliable role is clearing posts
that are obviously benign.

**The corrected triage flow for a single account:**

1. ONNX scores all posts. Returns continuous 0.0-1.0 per post.
1. Posts with ONNX < 0.10 → **clearly clean.** Skip Zentropi. This is
   the cost savings — for accounts outside identity-adjacent topics
   (tech, cooking, sports), most posts land here.
1. Posts with ONNX >= 0.10 → **send to Zentropi.** All of them. ONNX
   high scores are not trustworthy, so there is no “clearly toxic”
   band. Zentropi’s policy prompt understands context, reclaimed
   language, and supportive-vs-hostile intent. It makes the actual call.
- Replies → send as `(parent_text, reply_text)` pairs. Batch-fetch
  parent posts via `getPosts` (up to 25 URIs/call).
- Originals → send as solo posts. Zentropi’s policy prompt frames
  toxicity as “conversation content” between participants. Solo
  original posts not directed at anyone may return 0, which is
  correct — non-conversational posts are genuinely lower-signal
  for harassment prediction.
- Zentropi returns 1 or 0 for each.
1. **Toxicity rate** = `zentropi_toxic_count / total_posts`

ONNX does **not** contribute to the toxic count. It only subtracts
from the pool that Zentropi needs to evaluate. The toxicity rate is
driven entirely by Zentropi’s binary decisions. This rate is the
continuous signal that feeds the threat formula where `avg_toxicity`
currently goes.

**Cost implications:** For accounts in identity-adjacent topic spaces,
potentially 40-60% of posts go to Zentropi (ONNX flags identity terms
at > 0.10 routinely). For accounts outside that space, the ONNX filter
saves the majority of API calls (70-90% of posts score < 0.10). This
is more Zentropi traffic than an ambiguous-band design, but the
alternative — trusting ONNX high scores — produces systematic false
positives on exactly the communities Charcoal is designed to protect.

**If Zentropi’s free tier can’t absorb the volume**, the fallback is
self-hosting CoPE (open weights on HuggingFace, 9B params, quantizes
to ~6GB) or Llama Guard 4 12B on the Mac Studio via Ollama. Test
Zentropi’s throughput before committing to this architecture.

**`weighted_toxicity()` becomes unnecessary.** The category weighting
(identity_attack 0.35, insult 0.25, profanity 0.05) was a patch for
a model that can’t distinguish intent — downweighting profanity to
protect allies. Zentropi’s policy prompt can distinguish intent, so
the patch is no longer needed. Remove `weighted_toxicity()` from the
scoring path once Zentropi is integrated. Keep it only as a fallback
for ONNX-only mode (when Zentropi is unavailable).

**Reply-weighted toxicity rate:**

```
reply_tox_rate = zentropi_toxic_replies / total_replies
original_tox_rate = zentropi_toxic_originals / total_originals
weighted_tox = reply_tox_rate * 0.7 + original_tox_rate * 0.3
```

Fall back to flat rate when fewer than 5 replies (as before).

**Single policy prompt, not categories.** Zentropi’s policy prompt
already covers combative, belittling, dismissive, insulting,
patronizing, passive-aggressive, and threatening content within a
single binary decision. The exclusions section (legitimate criticism,
humor, academic discussion, third-party discussions) handles the
ally-profanity problem that the ONNX weighted_toxicity formula was
designed to solve. Splitting into per-category prompts would multiply
API calls by N for information the scoring pipeline doesn’t use yet.

The time to split into categories is when building user-facing
configuration (“warn me about threats but I’m okay with sarcasm”).
That’s a Zentropi-side change (multiple policy prompts) layered on
top of a working detection system, not a prerequisite for one.

This replaces the ONNX + Groq ensemble. ONNX stays as a clean-pass
filter only. Zentropi handles all toxicity classification decisions.
Groq Safeguard is removed.

The NLI cross-encoder (DeBERTa-v3-xsmall) continues to serve its
distinct role: inferring the *relationship type* between texts (attack,
contempt, support, etc.) for the context_score used in the threat
formula. It does not replace Zentropi and Zentropi does not replace it.

## Bug Fix: Context Score Double Application

**Current behavior in `build_profile`:**

1. `apply_behavioral_modifier_contextual()` — if `context_score >= 0.5`,
   bypasses the benign gate (allows raw_score × behavioral_boost through)
1. `context_multiplier = 1.0 + (context_score * 0.5)` — multiplies the
   result again by up to 1.5×

For a concern troll with context_score = 0.8 and raw_score = 20:

- Gate bypass lets 20 × 1.1 (behavioral_boost) = 22 through
- Context multiplier: 22 × 1.4 = 30.8

The gate bypass and the multiplier are both doing “amplify concern
trolls” but they compound in a way that isn’t documented or intentional.

**Fix:** The gate bypass is binary (context >= 0.5 → bypass). The
multiplier should only apply to accounts that were NOT eligible for the
benign gate in the first place. If the gate was bypassed due to context,
the context has already done its work — don’t multiply again.

```rust
// In build_profile:
let (score_with_behavioral, benign_gate, gate_was_bypassed) =
    apply_behavioral_modifier_contextual(...);

// Only apply context multiplier if gate wasn't relevant
let context_multiplier = match (context_score, gate_was_bypassed) {
    (Some(ctx), false) => 1.0 + (ctx * 0.5),  // normal: context boosts
    (Some(_), true) => 1.0,  // gate bypass already handled context
    (None, _) => 1.0,
};
```

Alternatively, reduce the multiplier range to 0.0-0.25 when the gate
was bypassed, so context still contributes but doesn’t double-count.

## Implementation Phases

These are ordered by value delivered, not by code dependency. Each phase
is independently shippable.

### Phase 1: Fix the sampling (highest impact, no new dependencies)

Change what posts get fetched and how toxicity is computed. This
improves every scoring path — amplification, sweep, and future firehose.

1. Add `PostSample` / `ReplyPost` types and `fetch_posts_with_replies()`
1. Update `build_profile` to use reply-inclusive fetch
1. Route originals vs replies to fingerprinting vs toxicity scoring
   (originals-first fingerprinting with reply fallback at < 15 originals)
1. Add `fingerprint_quality` field to `AccountScore`
1. Implement reply-weighted toxicity averaging
1. Fix context score double application
1. Remove separate `fetch_reply_ratio` call (now derived from PostSample)

### Phase 2: Adaptive sampling (cost reduction)

Add early stopping to `build_profile`. Requires Phase 1 (needs the
reply-inclusive data to make good early decisions).

1. Add `ScoringConfidence` enum and field to `AccountScore`
1. Implement three-stage sampling in `build_profile`
1. Update `is_score_stale` to use confidence-aware staleness (Low = 3
   days, Standard = 7, High = 14)
1. Update sweep pipeline to use aggressive early-stop variant

### Phase 3: Topic-first discovery (the architectural pivot)

Replace the graph-first sweep with topic-search-based discovery. Can
ship independently of Phases 1-2 (uses existing `build_profile`).

1. Add `src/discovery/topic_search.rs` — wraps `searchPosts` API,
   extracts author DIDs, deduplicates against scored accounts
1. Add `src/discovery/threat_expansion.rs` — given High/Elevated DIDs,
   fetch their followers for targeted scoring
1. Refactor `sweep.rs` to use topic-first discovery as primary path
1. Keep graph walk as optional fallback (`--sweep-mode=graph` flag)

### Phase 4: Firehose monitoring (real-time layer)

Add Jetstream WebSocket subscription for real-time collision detection.

1. Add `src/firehose/jetstream.rs` — WebSocket client filtered to
   `app.bsky.feed.post` collection
1. Filter for posts referencing protected user’s DID in reply/embed
1. Score-on-arrival for unscored strangers
1. Alert pathway for already-scored High accounts

### Phase 5: Zentropi integration (classification upgrade)

Replace ONNX + Groq ensemble with ONNX clean-pass filter + Zentropi
binary classification. ONNX only clears obviously-clean posts (< 0.10);
everything else goes to Zentropi. ONNX high scores are NOT trustworthy
due to keyword triggering on identity terms.

1. Add `src/toxicity/zentropi.rs` — wraps Zentropi API, returns
   `BinaryToxicityResult { is_toxic: bool }` (not `ToxicityScorer`
   trait, which assumes continuous scores)
1. Add ONNX clean-pass filter: posts scoring < 0.10 are cleared
   without Zentropi. Everything >= 0.10 goes to Zentropi.
1. Send replies as `(parent_text, reply_text)` pairs to Zentropi;
   originals as solo posts
1. Compute toxicity rate from Zentropi labels only:
   `zentropi_toxic_count / total_posts`
1. Remove Groq Safeguard dependency (`groq_safeguard.rs`, ensemble’s
   Groq path)
1. Remove `weighted_toxicity()` from scoring path (keep as fallback
   for ONNX-only mode when Zentropi is unavailable)
1. Test Zentropi free tier throughput — if insufficient, fall back to
   self-hosted CoPE (9B, open weights) or Llama Guard 4 12B on Mac
   Studio

## Testing Strategy

### Sampling tests

- `fetch_posts_with_replies` returns correctly partitioned `PostSample`
- Reply-weighted toxicity: 12/30 replies toxic (rate 0.40), 0/20
  originals toxic (rate 0.0) → weighted = 0.40 * 0.7 + 0.0 * 0.3 =
  0.28 (not 0.24 that flat rate of 12/50 gives)
- Fallback to flat rate when < 5 replies
- Stage 1 early exit: 0 toxic posts + low overlap → Low tier, stops
- Stage 2 resolution: clear signal → stops, borderline → continues
- Confidence levels stored and retrieved correctly

### Target fingerprinting tests

- 20 originals + 30 replies → fingerprint from 20 originals only
- 10 originals + 40 replies → fingerprint from all 50 posts,
  `fingerprint_quality = Degraded`
- 3 originals + 8 replies → fingerprint from all 11 posts,
  `fingerprint_quality = Unreliable`
- 0 originals + 25 replies → fingerprint from 25 replies,
  `fingerprint_quality = Unreliable`
- Verify originals-only fingerprint has higher cosine similarity to
  ground truth (use known-topic test accounts) than mixed fingerprint
- Protected user fingerprint always uses originals only (unchanged)

### Context score fix tests

- Gate bypass + no multiplier double-count: concern troll score is
  lower than current (quantify expected delta)
- Non-gate-eligible accounts still get full context multiplier
- Zero context score: no change from current behavior

### Topic-first discovery tests

- `searchPosts` keyword extraction from topic fingerprint
- Deduplication against existing scored accounts
- Threat expansion: only expands from High/Elevated, not all accounts

### Zentropi triage tests

- ONNX 0.05 → clearly clean, not sent to Zentropi, not counted
- ONNX 0.03 → clearly clean, not sent to Zentropi, not counted
- ONNX 0.15 → sent to Zentropi (NOT treated as “mildly toxic”)
- ONNX 0.85 → sent to Zentropi (NOT treated as “clearly toxic” —
  could be keyword triggering on identity terms)
- ONNX 0.95 on “fuck yeah, fat liberation!” → sent to Zentropi, which
  returns 0 (not toxic). ONNX high score is not trusted.
- ONNX 0.95 on “fat people are disgusting” → sent to Zentropi, which
  returns 1 (toxic). Zentropi makes the actual call.
- Toxicity rate: Zentropi labels 8 posts toxic out of 50 total →
  rate = 0.16. ONNX scores do NOT contribute to the count.
- Reply pair formation: reply scoring >= 0.10 has parent URI → parent
  fetched via getPosts → pair sent to Zentropi
- Original scoring >= 0.10 (no parent) → sent as solo post to Zentropi
- `weighted_toxicity()` NOT used when Zentropi is active; only used
  as fallback in ONNX-only mode
- ONNX-only fallback mode: if Zentropi unavailable, fall back to
  current weighted_toxicity scoring with documented caveat about
  identity-term false positives

### Composition tests (persona scenarios)

- **The Hidden Hostile:** Zentropi labels 0/20 original posts as toxic,
  but 12/30 replies as toxic. Reply toxicity rate = 0.40, original rate
  = 0.0, weighted rate = 0.28. Current system (ONNX on originals only)
  scores them Low. New system catches them via reply-weighted toxicity.
- **The Topical Stranger:** Posts about fat liberation topics, high reply
  toxicity rate, zero follower connection to protected user. Current
  sweep might never find them (depends on graph path). Topic-first
  discovery finds them via searchPosts.
- **The Fat Liberation Ally:** Posts supportively about fat liberation.
  ONNX scores 0.90+ on most posts (keyword triggering on “fat”). All
  posts sent to Zentropi, which returns 0 (not toxic) for supportive
  content. Toxicity rate = 0.0. Would have been scored as extremely
  toxic under ONNX-trusting design. This is the core reason ONNX high
  scores cannot be trusted.
- **The Efficient Clean Account:** 25 posts, all ONNX < 0.10, overlap
  0.08. Stage 1 early exit — never touches Zentropi. Costs 25 ONNX
  inferences and 0 API calls.
- **The Borderline Concern Troll:** context_score 0.8, benign behavioral
  signals. Verify gate bypass fires but context multiplier doesn’t
  double-count.
- **The Reply-Heavy Account:** 4 original posts, 46 replies. Originals
  are about cooking. Replies are vicious attacks on fat liberation posts.
  Originals-only fingerprint would show zero topic overlap (cooking ≠ fat
  liberation) and miss the threat entirely. Reply-fallback fingerprint
  captures the topical proximity from reply targets. Verify
  `fingerprint_quality = Degraded` is set and overlap score reflects the
  reply topics.

## Future: Shared Scoring Schema (separate design doc)

**Do not design against the current `account_scores` schema for
multi-user scaling.** The current schema stores toxicity data and
threat scores on the same row with `user_did` as part of the composite
key. This means the expensive work (toxicity rate, behavioral signals,
topic embedding) is duplicated per protected user. The target-state
schema splits shared vs. per-user data:

- `account_profiles` (shared): did, toxicity_rate, behavioral_signals,
  topic_embedding, fingerprint_quality, posts_analyzed, scored_at
- `user_threat_assessments` (per-user): user_did, target_did,
  topic_overlap, threat_score, threat_tier, context_score,
  graph_distance, assessed_at

This separation is what makes multi-user scaling sublinear: onboarding
user #101 with 10,000 already-scored accounts requires 10,000 cosine
similarities (milliseconds) and zero new Zentropi calls. Topic-first
discovery also partially shares — users in overlapping topic spaces
benefit from each other’s sweeps.

**This migration has its own design doc (forthcoming).** Do not
implement the schema split as part of the phases described above.
But do avoid decisions that make the split harder — e.g., don’t
tightly couple toxicity computation with per-user threat scoring in
new code.