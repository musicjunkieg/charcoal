# NLI Context Scoring Pipeline Redesign

**Date:** 2026-03-20
**Status:** Approved
**Issue:** #102
**Branch:** TBD (feat/nli-scoring-redesign)

## Problem

The NLI contextual scoring infrastructure is built (model loading, inference,
DB schema, UI) but incompletely wired. Amplifiers (accounts that quote/reply
to the protected user) are never scored via `build_profile()` — they only have
event-level data. Followers get no NLI scoring at all. The context_score uses
max instead of average, and doesn't factor into the final threat score as a
multiplier.

## Goals

1. Score amplifiers themselves via `build_profile()` with direct text pairs
2. Score high-signal followers with NLI using inferred (embedding-matched) pairs
3. Use average context_score across all pairs per account
4. Wire context_score into threat formula as a multiplier
5. Show context_score in the review queue and account detail UI

## Design

### Pipeline Flow

```
SCAN START
  ├─ Load toxicity model, embedding model, NLI model
  ├─ Build/load topic fingerprint + protected user post embeddings
  ├─ Fetch amplification events (quotes, reposts, likes, replies)
  │
  ├─ PHASE A: Record events + NLI score direct pairs
  │   └─ For each quote/reply with both texts: NLI score → context_score on event
  │
  ├─ PHASE B: Score amplifiers
  │   ├─ Collect unique amplifier DIDs from events
  │   ├─ For each amplifier: build_profile() with NLI scorer
  │   │   ├─ Direct pairs: gather all event texts for this amplifier
  │   │   ├─ NLI score each pair, average → context_score
  │   │   ├─ Threat formula: raw * behavioral_boost * context_multiplier
  │   │   └─ upsert_account_score() → amplifier now has a tier
  │
  ├─ PHASE C: Score followers (single call, conditional NLI)
  │   ├─ build_profile() runs once per follower (fetches posts, scores
  │   │   toxicity, computes overlap — all the expensive work happens once)
  │   ├─ After computing raw_score, check threshold:
  │   │   If raw_score >= 8.0 (Watch threshold):
  │   │   ├─ Run NLI on top 3 inferred pairs (embedding similarity)
  │   │   ├─ Average NLI scores → context_score
  │   │   └─ Re-compute final_score with context_multiplier
  │   └─ If raw_score < 8.0: skip NLI, context_score = None
  │   └─ upsert_account_score() with whatever was computed
  │
SCAN COMPLETE
```

### Context Score Aggregation

- **Amplifiers (direct pairs):** Average NLI hostility score across ALL event
  pairs for the account. If someone quoted the protected user 8 times, all 8
  interactions contribute to the average. This captures engagement patterns,
  not just worst-case moments.

- **Followers (inferred pairs):** Average NLI hostility score across the top 3
  most similar posts (by embedding cosine similarity between the follower's
  posts and the protected user's posts). Each follower post is matched to the
  closest protected user post to form a real text pair for NLI scoring.

- **Function:** Replace `max_context_score_opt()` with `avg_context_score()`
  that returns `Option<f64>` (None if no pairs scored).

### Threat Score Formula

**Why multiplicative instead of blending:** The previous approach (60% toxicity
+ 40% context) treated context as a *replacement* for toxicity signal. This is
wrong — an account with 0.0 toxicity and 1.0 context_score would get a nonzero
threat score, which doesn't make sense. Context should *amplify* existing threat
signals, not create them from nothing. A multiplicative approach ensures that
context only boosts accounts that already show toxicity + overlap signal.

Current formula:
```
raw_score = tox * 70 * (1 + overlap * 1.5)
```

New formula — order of operations:
```
Step 1: raw_score = tox * 70 * (1 + overlap * 1.5)
Step 2: raw_with_behavioral = raw_score * behavioral_boost
Step 3: context_multiplier = 1.0 + (context_score * 0.5)   // range: 1.0–1.5
Step 4: final_score = raw_with_behavioral * context_multiplier
```

Behavioral boost applies first (amplifies based on engagement patterns), then
context multiplier applies last (amplifies based on NLI hostility evidence).

Effects at each context level:
- context_score = 0.0 (benign): multiplier 1.0x — no change
- context_score = 0.5 (moderate hostility): multiplier 1.25x
- context_score = 1.0 (extreme hostility): multiplier 1.5x

Example scenarios:
- Watch-borderline (raw 7.9) + extreme context (1.0) → 7.9 * 1.5 = 11.85 (Watch)
- Elevated-borderline (raw 14.5) + moderate context (0.5) → 14.5 * 1.25 = 18.1 (Elevated)
- Low-tier ally (raw 2.0) + extreme context (1.0) → 2.0 * 1.5 = 3.0 (still Low)
- Zero toxicity (raw 0.0) + any context → 0.0 (still Low, context cannot create threat)

### Benign Gate Bypass

Unchanged: accounts with `context_score >= 0.5` skip the benign gate even if
behavioral signals look ally-like. This is an intentional sharp threshold — the
0.5 cutoff means "more likely hostile than not in context" which is a meaningful
semantic boundary from the NLI model. The gate bypass is a safety mechanism for
concern trolls, not a scoring adjustment, so it does not need to scale smoothly
with the multiplier.

### Direct Pairs for Amplifiers

When `build_profile()` is called for an amplifier, it receives the amplifier's
event text pairs (original_post_text + amplifier_text) gathered from the
event processing loop. These are passed as a new parameter
`direct_pairs: Option<&[(String, String)]>` — when present, NLI scores these
instead of using embedding-based inferred pairs.

**Edge case: no direct pairs.** An amplifier may have no text pairs (e.g., all
their events were likes/reposts with no quote or reply text). In this case,
`direct_pairs` is `Some(&[])` (empty slice), context_score is None, and the
threat formula falls back to `context_multiplier = 1.0` (no context boost).
The amplifier still gets a full AccountScore from toxicity + overlap scoring.

### Inferred Pairs for Followers

When `build_profile()` is called for a follower with NLI enabled (pass 2),
it uses the existing `protected_posts_with_embeddings` parameter. For each of
the follower's top 3 most similar posts (by embedding), it finds the closest
matching protected user post via `find_best_matching_user_post()` and forms a
text pair for NLI scoring.

### NLI Audit Logging

Every NLI scoring call emits a structured log entry via `tracing::info!` so it
appears in Railway's log dashboard and is queryable via log filters. Each entry
includes:

- `timestamp` — when the scoring happened
- `target_did` — the account being scored
- `target_handle` — human-readable handle
- `pair_type` — "direct" (from event text) or "inferred" (from embedding match)
- `original_text` — the protected user's post text
- `response_text` — the target's post text
- `hypothesis_scores` — all 5 scores: attack, contempt, misrepresent,
  good_faith_disagree, support
- `hostility_score` — the final computed hostility (0.0–1.0)
- `similarity` — embedding cosine similarity (inferred pairs only)

In addition to tracing output, `score_pair()` returns the full
`HypothesisScores` struct alongside the hostility score so callers can log
or store the breakdown.

**Audit file rotation:** NLI audit entries are also appended to a JSONL file
at `{data_dir}/nli-audit.jsonl` on the Railway persistent volume (`/data`).
Every 30 days, the active file is rotated: uploaded to a Railway Bucket
(S3-compatible object storage) as `nli-audit-{YYYY-MM-DD}.jsonl`, then the
local file is truncated and a new one started. The rotation check happens at
scan start — if the file's first entry is older than 30 days, rotate before
appending. Railway Buckets provide durable long-term storage independent of
the volume lifecycle.

## Files Changed

### Modified
| File | Changes |
|------|---------|
| `src/pipeline/amplification.rs` | Add Phase B amplifier scoring loop after event recording. Collect unique amplifier DIDs, gather their event pairs, call `build_profile()` with NLI + direct pairs. |
| `src/scoring/profile.rs` | Accept `direct_pairs` parameter. When present, NLI-score direct pairs instead of inferred pairs. Use `avg_context_score()`. |
| `src/scoring/threat.rs` | Replace `compute_threat_score_contextual()` with context_multiplier approach: `raw * behavioral_boost * context_multiplier`. |
| `src/scoring/nli.rs` | Replace `max_context_score_opt()` with `avg_context_score()`. |
| `src/scoring/behavioral.rs` | Update `apply_behavioral_modifier_contextual()` to use new formula. Benign gate bypass unchanged. |
| `src/web/scan_job.rs` | Compute and pass `protected_posts_with_embeddings` to pipeline for inferred pairs. |
| `src/main.rs` | Update `build_profile()` call sites with new `direct_pairs` parameter. |
| `src/pipeline/sweep.rs` | Update `build_profile()` call site with new parameter. |
| `src/scoring/nli.rs` | Return `HypothesisScores` from `score_pair()`. Add audit logging with full hypothesis breakdown. Add JSONL file append + 30-day rotation. |

### Not Changed
- Database schema (context_score fields already exist on account_scores and events)
- Frontend (context_score already conditionally displayed in review queue)
- Tier thresholds (High >= 35, Elevated >= 15, Watch >= 8, Low < 8)
- Event recording, like/reply detection, Constellation queries

## Testing Strategy

- Unit tests for `avg_context_score()` replacing max
- Unit tests for context_multiplier formula at boundary values
- Unit tests for two-pass follower scoring (verify NLI only triggers above threshold)
- Existing 307 tests must continue passing (no regressions)
- Manual validation on Railway staging with a scan
