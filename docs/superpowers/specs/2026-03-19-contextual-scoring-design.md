# Phase 1.75: Contextual Scoring & Feedback Mechanism

**Date:** 2026-03-19
**Status:** Draft
**Epic:** #101 — Improve scoring to detect contextual toxicity
**Related issues:** #102, #103, #104

## Problem Statement

Charcoal's current scoring evaluates accounts by analyzing their posts in
isolation — individual post toxicity averaged across recent output, combined
with topic overlap and behavioral signals. This misses the most important
harassment signal: **how someone engages with the protected user's specific
content**.

Current state:
- 138 scored accounts, 0 High/Elevated, 2 Watch
- Only quotes and reposts detected as engagement (4 quotes, 31 reposts)
- No contextual pair analysis (original post + response together)
- No ground truth labels from the protected user
- No way to measure scoring accuracy

The three primary harassment vectors on Bluesky are:
1. **Quote-posts** — hostile commentary broadcasting your post to a new audience
2. **Drive-by replies** — non-followers showing up in your mentions with hostility
3. **Screenshot posts** — sharing images of your posts to evade link detection

Current scoring only detects vector 1 (and reposts). Vector 2 is undetected.
Vector 3 is future work (#64).

Additionally, the system only scores followers of quoters/reposters. The
original architecture envisions scoring the **second-degree network of anyone
who engages with your content** — because engagement is the broadcast
mechanism that exposes your posts to potential harassers.

## Design Decisions

### Approach: Zero-Shot NLI Cross-Encoder

**Chosen:** Use a pre-trained DeBERTa-v3-xsmall NLI model (quantized ONNX,
~87MB) to score interaction pairs without any training or fine-tuning.

**Alternatives considered:**
- Multi-model ensemble (NLI + irony detector + Detoxify) — more complex,
  diminishing returns, defer unless NLI alone insufficient
- Fine-tune on SRQ dataset — requires Python training pipeline, overkill
  until zero-shot is proven insufficient

**Fallback:** If zero-shot NLI accuracy is poor after user feedback reveals
systematic errors, fine-tune the same DeBERTa-v3-xsmall architecture on the
SRQ dataset (5,200+ labeled quote/reply pairs, CC-BY 4.0, available on
Zenodo) plus accumulated Bluesky-native labeled data.

**Model:** `Xenova/nli-deberta-v3-xsmall` quantized ONNX from HuggingFace.
Same `ort` + `tokenizers` stack already used for Detoxify and embeddings.
512-token max input fits two Bluesky posts (300 chars each) comfortably.

**Tokenizer:** DeBERTa uses a SentencePiece-based tokenizer (different from
the RoBERTa tokenizer used by Detoxify). Download `tokenizer.json` from
`Xenova/nli-deberta-v3-xsmall` alongside the ONNX model. The `tokenizers`
crate (0.22) supports SentencePiece tokenizers via `tokenizer.json` format.

### Hostility Detection, Not Stance Detection

The NLI model detects **hostile engagement patterns**, not mere disagreement.
Disagreement is healthy; ad hominems, contempt, mockery, and misrepresentation
are the threat signals.

Hypothesis templates for NLI inference:

| Hypothesis | Detects |
|---|---|
| "The second text attacks or mocks the author of the first text" | Direct hostility, ad hominem |
| "The second text dismisses the first text with contempt" | Contempt, eye-rolling |
| "The second text misrepresents what the first text says" | Strawmanning, goalpost moving |
| "The second text respectfully disagrees with the first text" | Good-faith disagreement (NOT threat) |
| "The second text supports or agrees with the first text" | Ally signal (lowers threat) |

Contextual hostility score derived as:
```
hostile_signal = max(attack_score, contempt_score, misrepresent_score)
supportive_signal = max(good_faith_disagree_score * 0.5, support_score * 0.8)
hostility = clamp(hostile_signal - supportive_signal, 0.0, 1.0)
```

The subtraction ensures supportive signals reduce hostility but cannot produce
negative scores. A post that is simultaneously flagged as both attacking and
supporting will have its hostility reduced but not eliminated — the hostile
signal takes precedence when both are present, since genuine support rarely
co-occurs with genuine attack in the same text. Edge cases will be surfaced
through user feedback labels.

### Label Categories: 4-Tier Matching Predicted Tiers

User labels: `High`, `Elevated`, `Watch`, `Safe` — directly mapping to the
system's predicted threat tiers. This enables straightforward accuracy
computation (predicted tier vs. user label) without any mapping logic.

## Architecture

### Account Discovery — Who Gets Scored

The pipeline expands from "score followers of quoters/reposters" to "score
the second-degree network of anyone who engages with the protected user's
content."

**Engagement types and detection sources:**

| Engagement | How It Exposes Content | Detection Source | Status |
|---|---|---|---|
| Quote-post | Broadcasts to quoter's followers with commentary | Constellation backlinks | Existing |
| Repost | Broadcasts to reposter's followers silently | Constellation backlinks | Existing |
| Like | Surfaces in "Liked by people you follow" feeds | Constellation backlinks (`app.bsky.feed.like:subject.uri`) OR public API `getLikes` endpoint as fallback | **New** |
| Reply (non-follower) | Visible in thread; replier's followers see activity | `getPostThread` + filter non-followers | **New** |

**For each engagement, scoring targets:**
1. The engager themselves (toxicity + overlap + behavioral + contextual)
2. The engager's followers — the second-degree network now exposed to the
   protected user's content

**Drive-by reply detection:** Any reply from an account the protected user
does NOT follow. Following is the consent signal. Non-follower replies are
candidates for contextual scoring. Requires:
- **Follows list cache:** Fetch the protected user's follows via paginated
  `app.bsky.graph.getFollows` and cache in a `user_follows` table (or in
  memory). Refresh once per scan (follows lists change slowly). A user with
  1,000 follows requires ~10 paginated API calls at 100 per page.
- **Reply thread fetching:** For each of the protected user's recent posts,
  call `app.bsky.feed.getPostThread` with `depth=1` to get direct replies.
- **Filter:** Exclude replies from accounts in the follows cache. Remaining
  replies are drive-by candidates.

**Like detection verification:** Constellation's backlink index supports
querying likes via source path `app.bsky.feed.like:subject.uri`. If
Constellation does not index likes (to be verified during Phase 0 staging
setup), fall back to the public API `app.bsky.feed.getLikes` endpoint, which
returns likers for a given post URI. This is paginated but rate-limited, so
we may need to sample high-engagement posts rather than fetching all likes on
all posts.

### Scoring Pipeline — Contextual Pair Integration

**Current formula:**
```
threat_score = toxicity * 70 * (1 + overlap * 1.5)
// then modified by behavioral gate/boost
```

**Updated formula (when pair data exists):**
```
pair_weight = 0.4
blended_toxicity = toxicity * (1 - pair_weight) + context_score * pair_weight
threat_score = blended_toxicity * 70 * (1 + overlap * 1.5)
// then modified by behavioral gate/boost (see note below)
```

When no pair data exists (most second-degree accounts initially), the formula
falls back to the current behavior unchanged.

**Benign gate interaction:** The existing benign gate (caps scores at 12.0 for
accounts with low quote ratio, low reply ratio, no pile-on, and above-median
engagement) must be **bypassed when context_score is high** (>= 0.5). An
account that looks benign in isolation but is hostile in direct interactions
with the protected user's content is exactly the type of concern troll this
system is designed to catch. The benign gate should only apply when no
contextual evidence contradicts it.

**Pair sources at each scoring level:**

| Account Type | Pair Source | Data Volume |
|---|---|---|
| Direct engager (quote/reply) | Their response + protected user's post | ALL interactions with protected user's content |
| Direct engager (like/repost) | Their most topic-relevant posts + protected user's similar posts | Top 3-5 by embedding similarity |
| Second-degree (follower of engager) | Their most topic-relevant posts + protected user's similar posts | Top 3-5 by embedding similarity |

For direct engagers with multiple interactions, use the **max** hostility
score across all pairs. One hostile quote-dunk is sufficient signal.

For accounts with inferred pairs (no direct interaction), find the account's
posts with highest embedding similarity to the protected user's fingerprint,
pair each with the protected user's closest matching post, and run NLI.

### Data Volume Increase

| Signal | Current | After Phase 1.75 |
|---|---|---|
| Engagement types detected | 2 (quote, repost) | 4 (+ like, reply) |
| Interaction pairs per engager | 1 (quote text only) | All interactions |
| Inferred pairs per 2nd-degree account | 0 | Up to 5 |
| NLI inferences per scan | 0 | Hundreds to thousands |

The NLI model runs locally (no API rate limits). Memory and CPU impact on
Railway will be validated in the staging environment.

## Schema Changes (v5)

### Modify `amplification_events`

Add columns to store the protected user's original post text (for pair
display and scoring) and the NLI context score:

```sql
ALTER TABLE amplification_events ADD COLUMN original_post_text TEXT;
ALTER TABLE amplification_events ADD COLUMN context_score REAL;
```

Event types expand from `'quote'` and `'repost'` to include `'like'` and
`'reply'`.

**Rust trait change required:** The `Database` trait method
`insert_amplification_event` in `src/db/traits.rs` gains two parameters:
`original_post_text: Option<&str>` and `context_score: Option<f64>`. Both
`SqliteDatabase` and `PgDatabase` implementations must be updated. The
`AmplificationEvent` struct in `src/db/models.rs` gains matching fields.

### New `inferred_pairs` table

Inferred pairs (topic-matched posts for second-degree accounts and
likers/reposters) are stored separately from real amplification events to
avoid polluting pile-on detection, engagement counts, and dashboard displays.

```sql
CREATE TABLE inferred_pairs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_did TEXT NOT NULL,
    target_did TEXT NOT NULL,          -- the account being scored
    target_post_text TEXT NOT NULL,    -- their topic-relevant post
    target_post_uri TEXT NOT NULL,
    user_post_text TEXT NOT NULL,      -- protected user's matched post
    user_post_uri TEXT NOT NULL,
    similarity REAL NOT NULL,          -- embedding similarity that matched them
    context_score REAL,                -- NLI hostility score
    created_at TEXT NOT NULL
);

CREATE INDEX idx_inferred_pairs_target
    ON inferred_pairs(user_did, target_did);
```

### New `user_labels` table

```sql
CREATE TABLE user_labels (
    user_did TEXT NOT NULL,
    target_did TEXT NOT NULL,
    label TEXT NOT NULL,        -- 'high', 'elevated', 'watch', 'safe'
    labeled_at TEXT NOT NULL,
    notes TEXT,
    PRIMARY KEY (user_did, target_did)
);
```

One label per account per protected user. Upsert behavior: `INSERT ... ON
CONFLICT(user_did, target_did) DO UPDATE SET label=excluded.label,
labeled_at=excluded.labeled_at, notes=excluded.notes`. SQLite and Postgres
both support this syntax.

### New `Database` trait methods

```rust
// User labels
async fn upsert_user_label(
    &self, user_did: &str, target_did: &str, label: &str, notes: Option<&str>
) -> Result<()>;
async fn get_user_label(
    &self, user_did: &str, target_did: &str
) -> Result<Option<UserLabel>>;
async fn get_unlabeled_accounts(
    &self, user_did: &str, limit: i64
) -> Result<Vec<AccountScore>>;
async fn get_accuracy_metrics(
    &self, user_did: &str
) -> Result<AccuracyMetrics>;

// Inferred pairs
async fn insert_inferred_pair(
    &self, user_did: &str, target_did: &str,
    target_post_text: &str, target_post_uri: &str,
    user_post_text: &str, user_post_uri: &str,
    similarity: f64, context_score: Option<f64>
) -> Result<i64>;
async fn get_inferred_pairs(
    &self, user_did: &str, target_did: &str
) -> Result<Vec<InferredPair>>;
```

### Modify `account_scores`

```sql
ALTER TABLE account_scores ADD COLUMN context_score REAL;
```

Stores the NLI-derived contextual hostility score (max across all pairs for
this account). Null when no pair data exists.

**Rust struct change required:** `AccountScore` in `src/db/models.rs` gains
`context_score: Option<f64>`. Update `upsert_account_score` implementations,
`get_ranked_threats` queries, and web API JSON serialization in
`src/web/handlers/accounts.rs`.

### Postgres migration

Create `migrations/postgres/0005_contextual_scoring.sql` following the
existing migration file pattern. Use `DOUBLE PRECISION` instead of `REAL` for
score columns (Postgres convention). The SQLite migration goes in
`src/db/schema.rs` as `migrate_v4_to_v5()`.

## Feedback Mechanism

### User-Facing Acknowledgment

When a user labels an account differently from the predicted tier, the system
acknowledges the discrepancy at three levels:

**1. Immediate (on label action):**
```
You: Safe  |  Charcoal predicted: Elevated
"Got it — we'll use your label to improve future scoring."
```

**2. Persistent (on account detail view):**
```
@handle — Safe (your label)
Charcoal scored: 18.4 (Elevated) — overridden by your assessment
```

The user's label is authoritative. The algorithm is learning from them.

**3. Aggregate (on accuracy dashboard):**
```
Accuracy: 71% (predicted tier matches your label)
Overscored: 12 accounts (you said Safe, we said Watch+)
Underscored: 3 accounts (you said High, we said Low)
```

Shows patterns in where the system is wrong.

### Inline Labeling

On the existing account detail view, add a row of 4 buttons:

```
[ High ] [ Elevated ] [ Watch ] [ Safe ]
```

- Clicking saves immediately (no confirmation — low friction)
- Selected button stays highlighted
- Pre-highlights current label if one exists
- Optional text field for notes

### Triage Review Queue

New `/review` page showing unlabeled accounts sorted by threat score
descending (highest predicted threats are most valuable to label).

Each card shows:
- Handle + avatar
- Current predicted tier + score
- Top toxic posts
- Interaction pairs (if any): protected user's original post + their response
- The 4 label buttons

After labeling, next account loads automatically. Progress indicator shows
how many accounts have been labeled out of total.

### Accuracy Metrics

Computed from `user_labels` joined against `account_scores`:
- Overall accuracy (predicted tier == user label)
- Confusion matrix by tier
- Most common error patterns (e.g., "allies in topic space overscored")

Displayed on the main dashboard once 20+ labels exist.

## Deployment Strategy

### Railway Staging Environment

This is a large change (new model, schema migration, expanded pipeline, new
UI). All work deploys to a staging environment first.

**Setup:**
- Create `staging` environment on the existing Railway charcoal project
- Own Postgres instance, own persistent volume, own domain
  (`staging.charcoal.watch` or Railway-generated)
- Deploys from `feat/contextual-scoring` branch
- Same Dockerfile, different env vars
- Fresh database (no data migration from production)

**Validation in staging:**
- ONNX model downloads succeed on Railway hardware
- 3 models fit in memory (Detoxify ~126MB + embeddings ~90MB + NLI ~87MB)
- Schema migration v5 runs cleanly
- OAuth flow works with staging redirect URI
- Full scan completes: engagement detection -> follower expansion -> NLI scoring
- Feedback UI is functional
- Bryan and testers can log in and start labeling accounts

**Production cutover:**
- Merge `feat/contextual-scoring` -> `main`
- Production auto-deploys, runs schema migration v5 on startup
- Delete or retain staging environment

### Post-Deploy Infrastructure Evaluation

After observing how Phase 1.75 performs on Railway (memory usage, scan
duration, model inference time, cost), evaluate whether Railway remains the
right infrastructure provider or whether alternatives (Fly.io, dedicated VPS,
etc.) would better serve the expanded workload. This is a decision point
informed by real performance data, not a predetermined migration.

## Implementation Phases

### Phase 0 — Staging Environment (before any code changes)
1. Create Railway staging environment
2. Configure branch deployment from `feat/contextual-scoring`
3. Verify staging deploys and runs current codebase correctly

### Phase 1.75a — Data Foundation
4. Schema migration v5 (new columns, new table)
5. Store original post text on amplification events
6. Detect likes via Constellation backlinks
7. Detect drive-by replies via `getPostThread` + follows filter
8. Expand follower scoring to all engagement types

### Phase 1.75b — Contextual Scoring Model
9. Download and integrate NLI model (add to `charcoal download-model`)
10. Build NLI inference module: `(text_a, text_b)` -> hostility score
11. Integrate into scoring pipeline (blended formula)
12. Score inferred pairs for second-degree accounts (top 3-5 topic-relevant)

### Phase 1.75c — Feedback Mechanism
13. Backend API: label endpoints, review queue endpoint, accuracy metrics
14. Frontend: inline label buttons on account detail
15. Frontend: triage review queue page
16. Frontend: accuracy dashboard with discrepancy acknowledgment

## Testing Strategy

### Unit Tests
- NLI inference module: mock model outputs, verify hostility score computation
- Blended scoring formula: verify pair_weight blending, fallback when no pairs
- Hypothesis score combination: verify hostility derivation from entailment scores
- User label CRUD: insert, update, fetch, accuracy computation
- Drive-by reply filtering: followers excluded, non-followers included

### Integration Tests
- Schema migration v5: verify columns added, data preserved
- Amplification event storage with new fields (original_post_text, context_score)
- Like detection via Constellation
- Reply detection via getPostThread
- Label API endpoints (create, update, list, accuracy)

### End-to-End (Staging)
- Full scan with expanded engagement detection
- NLI scoring on real Bluesky post pairs
- Label accounts via UI and verify accuracy metrics update
- Memory usage under 3-model load on Railway

## Success Criteria

1. All four engagement types (quote, repost, like, reply) detected and stored
2. NLI contextual scores computed for all available interaction pairs
3. Scoring formula incorporates context_score when pair data exists
4. Users can label accounts via inline buttons and triage queue
5. Accuracy metrics displayed once 20+ labels collected
6. Discrepancy acknowledgment shown when user label differs from prediction
7. All changes validated in staging before production deployment
8. 232+ tests passing (existing + new), clippy clean

## Open Questions

1. **Hypothesis template tuning:** The exact wording of NLI hypotheses will
   significantly affect accuracy. Plan to iterate based on user feedback from
   staging. Start with the 5 templates listed above.

2. **Inferred pair quality:** Matching second-degree accounts' posts to the
   protected user's posts by embedding similarity may produce low-quality pairs
   if the topics are only loosely related. May need a minimum similarity
   threshold to avoid noise.

3. **Scan duration:** DeBERTa-v3-xsmall NLI inference on CPU is roughly
   50-100ms per pair. 1000 pairs = 50-100 seconds serialized. Use the
   existing `spawn_blocking` + `buffer_unordered` pattern (from the scoring
   pipeline) to run NLI inferences concurrently. Batch size TBD based on
   Railway CPU/memory — start with concurrency=4, measure in staging.

4. **Like volume:** Likes are far more common than quotes/reposts. Scoring
   every liker's followers could be expensive. May need to sample or
   prioritize likers with high toxicity/topic overlap.

5. **Infrastructure fit:** After staging validation, evaluate whether Railway's
   resource limits and pricing model suit the expanded workload, or whether a
   different provider would be more appropriate.

6. **Accuracy metrics granularity:** Exact-match accuracy (predicted tier ==
   user label) is coarse — "Watch" predicted vs. "Elevated" labeled is a
   near-miss, while "High" predicted vs. "Safe" labeled is a major failure.
   Consider adding mean tier distance alongside exact-match accuracy. The
   confusion matrix partially addresses this but a headline "weighted
   accuracy" number may be more informative.

7. **Phase parallelism:** Phase 1.75a (data foundation) and Phase 1.75c
   (feedback mechanism) are independent — the feedback UI only needs the
   `user_labels` table and existing `account_scores` data. These can be built
   in parallel across sessions if desired. Phase 1.75b (NLI model) depends
   on 1.75a for pair data.
