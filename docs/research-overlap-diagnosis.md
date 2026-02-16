# Topic Overlap Scoring: Diagnostic Analysis

## Executive Summary

After scanning 426 accounts, Charcoal's maximum topic overlap score is 0.09 out of 1.0. The topic overlap dimension is effectively non-functional. This document identifies five compounding problems that produce this result, with concrete math showing why the current approach cannot yield meaningful scores.

The root causes, in order of severity:

1. **Exact keyword matching guarantees near-zero intersection** between two independently-built TF-IDF vocabularies
2. **Weight dilution** across 60-90 keywords makes even successful matches contribute almost nothing
3. **The Jaccard denominator penalizes vocabulary size**, so larger fingerprints produce lower scores
4. **Asymmetric corpus sizes** (500 vs. 20 posts) produce fundamentally different keyword distributions
5. **Cluster structure is discarded** at comparison time, losing the only abstraction that could bridge vocabularies

---

## Finding 1: The 500-vs-20 Post Asymmetry

### How the two fingerprints are built

| Property | Protected user | Target account |
|---|---|---|
| Posts analyzed | 500 | 20 |
| `top_n_keywords` | 60 | 30 |
| `max_clusters` | 10 | 5 |
| Keywords per cluster | ~6 avg | ~6 avg |
| Source file | `main.rs` line 152 (default extractor) | `profile.rs` lines 89-92 |

The protected user's fingerprint uses `TfIdfExtractor::default()`, which extracts 60 keywords into 10 clusters from 500 posts. Each target account uses a custom config that extracts 30 keywords into 5 clusters from 20 posts.

### Why 20 posts fundamentally changes what TF-IDF produces

TF-IDF scores are computed as `TF(term, doc) * IDF(term, corpus)`, where IDF = `log(N / df)` and `N` is the number of documents and `df` is the document frequency (how many documents contain the term).

With 500 posts:
- IDF has a wide dynamic range: `log(500/1) = 6.2` down to `log(500/250) = 0.7`
- Common-across-many-posts words get low IDF, rare distinctive words get high IDF
- The top 60 keywords are genuinely distinctive topic markers

With 20 posts:
- IDF has a compressed range: `log(20/1) = 3.0` down to `log(20/10) = 0.7`
- A word appearing in just 2 out of 20 posts gets `log(20/2) = 2.3` — almost as high as the rarest term
- The ranking is much noisier; idiosyncratic words from a few posts can dominate
- The top 30 keywords are more likely to be incidental vocabulary than stable topic markers

The critical insight: **TF-IDF on 20 documents does not produce a reliable topic fingerprint. It produces a noisy word list heavily influenced by whatever happened to be in those specific 20 posts.**

### Does fetching more posts help?

Increasing from 20 to 50 posts would improve IDF discrimination somewhat (`log(50/1) = 3.9`, giving slightly more range). But it would not fix the fundamental problem — the extracted keywords would still be a different set of words from the protected user's keywords, because TF-IDF surfaces **distinctive** vocabulary, and two different people talking about the same topic use different distinctive words.

For example, Bryan posting about fat liberation might surface keywords like "fatphobia", "diet", "stigma", "liberation". Someone else posting about the same topic might surface "weight", "body", "health", "size". Same topic, zero keyword overlap.

More posts would help marginally by stabilizing the keyword ranking, but it does not solve the vocabulary mismatch problem.

---

## Finding 2: The Exact-Match Problem

### The core flaw

The weighted Jaccard comparison (`overlap.rs`) iterates over the union of keywords from both fingerprints and requires **identical string matches** to register any overlap:

```rust
for key in all_keys {
    let a = weights_a.get(key).copied().unwrap_or(0.0);
    let b = weights_b.get(key).copied().unwrap_or(0.0);
    min_sum += a.min(b);
    max_sum += a.max(b);
}
```

If keyword "fatphobia" exists in fingerprint A but not fingerprint B, it contributes `min(w, 0.0) = 0.0` to the numerator and `max(w, 0.0) = w` to the denominator. It actively **hurts** the score by inflating the denominator.

### How severe is this?

Consider the topic spaces Bryan is active in:

- **Fat liberation**: Keywords might be "fatphobia", "diet", "stigma", "liberation", "weight", "body", "size"
- A hostile account in the same space might produce: "obesity", "health", "overweight", "BMI", "calories", "fitness"

These are clearly about the same topic. They share **zero** keywords. The overlap score for this pair is 0.0.

This is not an edge case. It is the **expected** outcome. Two independently-built TF-IDF vocabularies from different corpora on the same topic will share very few exact keywords, because TF-IDF specifically surfaces words that are distinctive **within each corpus**. The distinctive words within Bryan's 500 posts are not the same distinctive words within someone else's 20 posts, even when both people are discussing identical subjects.

### Quantifying the expected intersection rate

Given two vocabularies of size 60 and 30, drawn from a shared topic domain of perhaps 200 relevant words (being generous), the expected number of shared keywords by chance is approximately `60 * 30 / 200 = 9` in a best-case scenario where both people have perfect topic focus. In practice, social media posts span many topics, the effective vocabulary pool is much larger (thousands of words), and the overlap drops to 1-5 keywords — which is exactly what we observe (a max score of 0.09 implies roughly 2-4 matching keywords with tiny weights).

---

## Finding 3: The Weight Distribution Problem

### How weights are distributed

The `keyword_weights()` method in `fingerprint.rs` distributes each cluster's weight evenly across its keywords:

```rust
let per_keyword = cluster.weight / cluster.keywords.len().max(1) as f64;
```

For the protected user's fingerprint (60 keywords, 10 clusters, weights summing to 1.0):
- Average cluster weight: 0.10
- Average keywords per cluster: 6
- Average per-keyword weight: `0.10 / 6 = 0.0167`

For a target account's fingerprint (30 keywords, 5 clusters, weights summing to 1.0):
- Average cluster weight: 0.20
- Average keywords per cluster: 6
- Average per-keyword weight: `0.20 / 6 = 0.0333`

### Why this crushes overlap scores

When two fingerprints share a keyword, the contribution to the numerator is `min(weight_a, weight_b)`. With the weights above, a single match contributes approximately `min(0.0167, 0.0333) = 0.0167` to the numerator.

But **every non-matching keyword** contributes its full weight to the denominator via `max(weight_a, 0.0) = weight_a`.

The denominator includes the full weight mass of both fingerprints minus the overlap. Since both fingerprints have weights summing to 1.0, and the matching keywords represent a tiny fraction, the denominator is approximately 2.0 minus the small overlap.

---

## Finding 4: Concrete Math Example

### Setup

**Protected user fingerprint**: 60 keywords across 10 clusters, weights summing to 1.0.
- Average per-keyword weight: ~0.0167

**Target account fingerprint**: 30 keywords across 5 clusters, weights summing to 1.0.
- Average per-keyword weight: ~0.0333

**Shared keywords**: 3 (an optimistic real-world case)

### Calculation

For the 3 matching keywords, assume the protected user's weight is ~0.0167 and the target's weight is ~0.0333 (these are averages; actual values vary but this illustrates the scale):

```
Numerator (sum of mins):
  3 matching keywords * min(0.0167, 0.0333) = 3 * 0.0167 = 0.0501

Denominator (sum of maxes):
  3 matching keywords * max(0.0167, 0.0333) = 3 * 0.0333  = 0.0999
  57 unmatched protected keywords * 0.0167                  = 0.9519
  27 unmatched target keywords * 0.0333                     = 0.8991

  Total denominator = 0.0999 + 0.9519 + 0.8991             = 1.9509

Score = 0.0501 / 1.9509 = 0.0257
```

**Result: 0.026** — even with 3 shared keywords, the score is about 2.6%.

### What about 5 shared keywords?

```
Numerator: 5 * 0.0167 = 0.0835
Denominator: (5 * 0.0333) + (55 * 0.0167) + (25 * 0.0333)
           = 0.1665 + 0.9185 + 0.8325
           = 1.9175
Score = 0.0835 / 1.9175 = 0.0436
```

**Result: 0.044** — five shared keywords only gets to 4.4%.

### What would it take to reach 0.50?

Working backward: for score = 0.50, we need `min_sum / max_sum = 0.50`, meaning `min_sum = 0.50 * max_sum`. This essentially requires the fingerprints to share the majority of their weight mass. With independent TF-IDF extraction, this is mathematically unreachable.

Even if we imagined a perfect scenario where all 30 target keywords match 30 of the protected user's keywords (a complete vocabulary overlap), and the matching keywords happen to carry the heaviest weights:

```
Best case: 30 keywords match
Numerator: 30 * 0.0167 = 0.501   (all mins come from protected user's smaller weights)
Denominator: (30 * 0.0333) + (30 * 0.0167) = 0.999 + 0.501 = 1.500
Score = 0.501 / 1.500 = 0.334
```

Even **perfect vocabulary overlap** only yields 0.33 because the two fingerprints have different weight scales. The Jaccard denominator always includes the larger weight for each keyword, and the target account's keywords are weighted ~2x heavier (0.0333 vs 0.0167) because they have fewer keywords.

---

## Finding 5: Cluster Structure Is Discarded

### What happens at comparison time

The fingerprint stores rich cluster structure: groups of related keywords with a shared weight. But `keyword_weights()` flattens this to a `HashMap<String, f64>` of individual keywords, each carrying a fraction of the cluster weight.

This means:
- The cluster label "white / black / woke" with weight 0.19 and 6 keywords becomes 6 entries of ~0.032 each
- The semantic grouping (these words co-occur and represent a topic) is lost
- Two accounts whose clusters cover the same topics but use different words within those topics score 0.0

### What the cluster structure could have offered

If comparison happened at the cluster level — matching clusters by some similarity metric rather than requiring identical keywords — the system could recognize that a cluster about {"fatphobia", "diet", "stigma"} and a cluster about {"obesity", "weight", "health"} are about related topics. The cluster structure was designed to capture this abstraction, but it is thrown away at the one point where it matters most.

---

## Finding 6: The Overlap Gate Makes It Worse

The threat scoring formula in `threat.rs` applies a gate:

```rust
overlap_gate_threshold: 0.05,
gate_max_score: 25.0,
```

Any account with topic overlap below 0.05 gets capped at a threat score of 25.0, regardless of toxicity. Given that the maximum observed overlap is 0.09, the vast majority of accounts (those scoring below 0.05) are gated. Even genuinely hostile accounts that talk about exactly the same topics as Bryan are being gated because the overlap measurement is broken.

The gate is a good idea conceptually — it prevents purely hostile-but-irrelevant accounts from dominating the report. But when the overlap metric itself is broken, the gate becomes a universal score suppressor.

---

## Summary of Compounding Effects

The problems multiply rather than add. Here is how they cascade:

1. **20 posts** produces a noisy, unstable keyword list (Finding 1)
2. **Exact matching** requires identical strings from independently-built vocabularies (Finding 2)
3. Together, (1) and (2) yield ~2-4 matching keywords out of 60+30 = 90 total
4. **Weight dilution** means each of those 2-4 matches contributes ~0.017 to the numerator (Finding 3)
5. **The Jaccard denominator** includes all ~87 non-matching keywords, inflating the denominator to ~1.95 (Finding 4)
6. **Result**: even optimistic scenarios produce scores around 0.03-0.05
7. **The gate** at 0.05 then suppresses threat scores for the majority (Finding 6)

The fundamental issue is architectural: **weighted Jaccard over independently-extracted TF-IDF keywords is the wrong similarity metric for this problem**. It conflates vocabulary similarity with topical similarity, and these are very different things.

---

## What Would a Fix Look Like? (Directional, Not Prescriptive)

This section sketches directions, not implementations.

### Option A: Embeddings-based overlap
Replace keyword matching with semantic vector comparison. Embed each account's posts (or their TF-IDF keywords) into a dense vector space using a sentence embedding model. Cosine similarity between vectors captures topical proximity regardless of specific word choice. "Fatphobia" and "obesity" would land near each other in embedding space.

**Tradeoff**: Requires an embedding model (could be local via ONNX, or an API call). Adds a dependency, but the `TopicExtractor` trait was designed for exactly this swap.

### Option B: Shared vocabulary comparison
Instead of building independent fingerprints, score each target account's posts against the protected user's keyword list. Count how often the protected user's 60 keywords appear in the target's posts, weighted by importance. This avoids the vocabulary mismatch entirely because you are measuring "does this person use Bryan's distinctive words?" rather than "do two independent keyword extractions happen to produce the same words?"

**Tradeoff**: Simpler to implement, but still requires exact word matching. Would not catch semantic synonyms.

### Option C: Hybrid approach
Keep TF-IDF for the protected user's fingerprint (it works well with 500 posts). For each target account, skip independent fingerprint extraction. Instead, compute a simple "topic relevance score" by checking how many of the protected user's keywords appear in the target's raw post text, weighted by keyword importance. This is essentially Option B with no new dependencies.

**Tradeoff**: Loses the ability to characterize what the target talks about independently, but gains dramatically better overlap measurement.

### Option D: Increase post count + use lemmatization
Fetch 50-100 posts per account and apply lemmatization (reducing words to root forms: "fatphobia" and "fatphobic" become the same stem). This would increase keyword intersection rates.

**Tradeoff**: Helps at the margins but does not fix the fundamental vocabulary mismatch. "Obesity" still does not lemmatize to "fat".

---

## Appendix: Key Code Locations

| Component | File | Lines |
|---|---|---|
| Protected user fingerprint extraction | `/home/sprite/charcoal/src/main.rs` | 143-154 |
| Target account fingerprint extraction | `/home/sprite/charcoal/src/scoring/profile.rs` | 89-93 |
| TF-IDF keyword extraction and clustering | `/home/sprite/charcoal/src/topics/tfidf.rs` | 38-86 |
| Keyword weight flattening | `/home/sprite/charcoal/src/topics/fingerprint.rs` | 86-97 |
| Weighted Jaccard similarity | `/home/sprite/charcoal/src/topics/overlap.rs` | 30-56 |
| Threat score formula and gate | `/home/sprite/charcoal/src/scoring/threat.rs` | 38-55 |
| Threat tier thresholds | `/home/sprite/charcoal/src/db/models.rs` | 63-69 |
