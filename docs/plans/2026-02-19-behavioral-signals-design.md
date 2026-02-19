# Behavioral Signals Design

**Issue:** #54 — Behavioral signals: reply ratio, quote ratio, pile-on detection
**Date:** 2026-02-19
**Status:** Approved

## Problem

The current scoring formula uses two inputs: toxicity (ONNX model) and topic
overlap (sentence embeddings). This misses behavioral patterns that distinguish
threats from allies:

- An ally who posts about the same topics as the protected user gets a moderate
  score due to high overlap, even though their behavior is clearly non-threatening
- A hostile account that writes carefully-worded posts (low toxicity) but
  habitually quote-dunks strangers is scored Low

Behavioral signals fill this gap in both directions: dampening scores for
benign accounts and amplifying scores for hostile patterns.

## Signals Collected

Four behavioral signals per scored account:

| Signal | Source | Computation |
|--------|--------|-------------|
| Quote ratio | Existing post fetch (embed type detection) | Posts with `embed.record` or `embed.recordWithMedia` / total posts. Range 0.0-1.0. |
| Reply ratio | New API call (`getAuthorFeed` unfiltered, 50-post sample) | Reply posts / total posts in sample. Range 0.0-1.0. |
| Avg engagement received | Existing post data (`like_count + repost_count`) | Mean engagement across fetched posts. |
| Pile-on participation | `amplification_events` table post-processing | 5+ distinct amplifiers on same protected post within 24-hour sliding window. Boolean. |

### Reply ratio implementation

Currently `fetch_recent_posts` uses the `posts_no_replies` filter. For reply
ratio, make a second `getAuthorFeed` call with `posts_and_author_threads` filter
(includes replies). One page of up to 50 posts. Count posts with a `reply`
field vs total. This gives a statistically useful ratio estimate without
fetching the entire post history.

### Quote detection

The AT Protocol `PostView` includes an `embed` field. When a post quotes
another, the embed contains a `record` or `recordWithMedia` variant. Add an
`is_quote: bool` field to the `Post` struct by checking the embed type during
post parsing.

## Scoring: Gate + Multiplier Hybrid

### Benign Gate (false-positive dampener)

An account is behaviorally benign when ALL conditions are true:

- Quote ratio < 0.15
- Reply ratio < 0.30
- Not flagged in any pile-on
- Average engagement received > median (computed across all scored accounts)

When benign gate triggers: **score capped at 12.0** (maximum Watch tier, below
Elevated threshold of 15.0).

Rationale: allies who post about the same topics as the protected user often
have high topic overlap and occasionally moderate toxicity scores (e.g., posts
about contentious topics that the model flags). The gate prevents them from
being classified as Elevated or High.

### Hostile Multiplier (threat amplifier)

For accounts that don't pass the benign gate:

```
behavioral_boost = 1.0
  + (quote_ratio * 0.20)        // max +0.20 if 100% quotes
  + (reply_ratio * 0.15)        // max +0.15 if 100% replies
  + (pile_on ? 0.15 : 0.0)     // +0.15 if pile-on participant
```

Range: 1.0 (no hostile signals) to 1.5 (extreme: all quotes, all replies,
pile-on participant).

### Combined Formula

```
raw_score = tox * 70 * (1 + overlap * 1.5)

if behaviorally_benign:
    score = min(raw_score, 12.0)
else:
    score = raw_score * behavioral_boost
```

The existing overlap gate (< 0.15 → capped at 25) still applies first.

### Backward Compatibility

When no behavioral data exists (e.g., accounts scored before this feature):
- behavioral_boost = 1.0 (no change)
- benign gate not applied
- Existing 139 tests pass unchanged

## Pile-on Detection

### Algorithm

1. Group `amplification_events` by `original_post_uri`
2. For each group, sort by `detected_at` timestamp
3. Sliding 24-hour window: if 5+ distinct `amplifier_did` values fall within
   any 24-hour window, flag as pile-on
4. All amplifiers in that window get `pile_on = true`

### Why 5+ threshold

Positive amplification is real — 2-3 accounts quoting the same post is normal
engagement. 5+ distinct accounts within 24 hours is unusual enough to warrant
flagging, especially combined with individual toxicity scores.

## Data Storage

### Schema migration (v2 → v3)

Add one column to `account_scores`:

```sql
ALTER TABLE account_scores ADD COLUMN behavioral_signals TEXT;
```

The column stores a JSON object:

```json
{
  "quote_ratio": 0.35,
  "reply_ratio": 0.45,
  "avg_engagement": 12.5,
  "pile_on": true,
  "benign_gate": false,
  "behavioral_boost": 1.22
}
```

## Testing Strategy

### Unit tests — behavioral signal computation

- Quote ratio: 0 quotes → 0.0, all quotes → 1.0, mixed → correct ratio
- Reply ratio: same pattern
- Behavioral boost: verify range 1.0-1.5
- Benign gate: all four conditions required
- Benign gate cap: score capped at 12.0
- Edge cases: 0 posts, insufficient data, threshold boundaries

### Unit tests — pile-on detection

- 4 events in 24h → no pile-on
- 5 events in 24h → pile-on detected
- 5 events across 48h → no pile-on (outside window)
- Same amplifier twice → counts as 1 (deduplicated by DID)

### Real-world persona scenarios

- **The Quote-Dunker**: 80% quote ratio, moderate toxicity, high overlap →
  behavioral boost ~1.16, pushed from Watch to Elevated
- **The Supportive Ally**: 5% quote ratio, low toxicity, high overlap →
  benign gate triggers, capped at Watch despite high overlap
- **The Pile-On Participant**: Part of 7-account pile-on, moderate individual
  toxicity → pile-on flag + boost, elevated threat tier
- **The Lurker Reposter**: Low post count, mostly reposts (not quotes), low
  engagement received → doesn't trigger benign gate (low engagement), moderate
  boost from behavior patterns

### Composition tests

- Full pipeline: benign → gate → capped score
- Full pipeline: hostile → boost → elevated score
- Full pipeline: hostile + pile-on → max boost
- High toxicity + benign behavior → gate prevents High tier
