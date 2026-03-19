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
hostility = max(attack_score, contempt_score, misrepresent_score)
            - good_faith_disagree_score * 0.5
            - support_score * 0.8
```

Clamped to 0.0–1.0.

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
| Like | Surfaces in "Liked by people you follow" feeds | Constellation backlinks (likes path) | **New** |
| Reply (non-follower) | Visible in thread; replier's followers see activity | `getPostThread` + filter non-followers | **New** |

**For each engagement, scoring targets:**
1. The engager themselves (toxicity + overlap + behavioral + contextual)
2. The engager's followers — the second-degree network now exposed to the
   protected user's content

**Drive-by reply detection:** Any reply from an account the protected user
does NOT follow. Following is the consent signal. Non-follower replies are
candidates for contextual scoring. Requires fetching the protected user's
follows list and the reply threads on their recent posts.

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
// then modified by behavioral gate/boost
```

When no pair data exists (most second-degree accounts initially), the formula
falls back to the current behavior unchanged.

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

Event types expand from `'quote'` and `'repost'` to include `'like'`,
`'reply'`, and `'inferred'` (for topic-matched pairs on second-degree
accounts).

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

One label per account per protected user. Updatable (relabeling overwrites).

### Modify `account_scores`

```sql
ALTER TABLE account_scores ADD COLUMN context_score REAL;
```

Stores the NLI-derived contextual hostility score. Null when no pair data
exists.

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

3. **Scan duration:** Hundreds to thousands of NLI inferences per scan will
   increase scan time. Need to measure in staging and potentially add
   concurrency or batching.

4. **Like volume:** Likes are far more common than quotes/reposts. Scoring
   every liker's followers could be expensive. May need to sample or
   prioritize likers with high toxicity/topic overlap.

5. **Infrastructure fit:** After staging validation, evaluate whether Railway's
   resource limits and pricing model suit the expanded workload, or whether a
   different provider would be more appropriate.
