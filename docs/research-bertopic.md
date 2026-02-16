# Research: BERTopic and Semantic Embeddings for Topic Overlap Scoring

**Date:** 2026-02-16
**Context:** Charcoal's topic overlap scores are universally very low (max 0.09 out
of 1.0 across 426 scored accounts). This research evaluates BERTopic and semantic
embedding approaches as potential replacements for the current TF-IDF + weighted
Jaccard similarity system.

---

## 1. What BERTopic Actually Does

BERTopic is a Python library for topic modeling created by Maarten Grootendorst. It
discovers what topics exist in a collection of documents and assigns each document to
a topic. The pipeline has five main steps, each designed to be swappable and modular.

### The Pipeline, Step by Step

**Step 1 -- Turn text into numbers (Sentence Embeddings).** Each document (in our
case, a social media post) gets converted into a dense vector of numbers -- typically
384 or 768 numbers long. These numbers encode the *meaning* of the text, not just
which words appear. The default model is `all-MiniLM-L6-v2`, which produces 384-
dimensional vectors. Two posts about the same topic will have vectors that point in
similar directions, even if they use completely different words.

**Step 2 -- Squish the dimensions down (UMAP).** 384 dimensions is too many for
clustering algorithms to work well -- this is called the "curse of dimensionality."
UMAP (Uniform Manifold Approximation and Projection) reduces the vectors down to
around 5 dimensions while preserving the relationships between them. Think of it like
taking a 3D globe and flattening it onto a 2D map -- you lose some detail, but nearby
countries stay nearby. UMAP is better than simpler methods like PCA because it
preserves both local neighborhoods *and* the broader structure of the data.

**Step 3 -- Group similar documents together (HDBSCAN).** HDBSCAN (Hierarchical
Density-Based Spatial Clustering of Applications with Noise) finds natural groupings
in the reduced data. Unlike K-means, which requires you to specify how many clusters
to find, HDBSCAN discovers the number of clusters on its own. It also has the useful
property of labeling some documents as "noise" rather than forcing every document
into a cluster -- this avoids polluting topic groups with irrelevant posts.

**Step 4 -- Find what makes each group unique (Bag-of-Words).** All documents in
each cluster are merged into one mega-document. Then a word frequency count is
performed on each mega-document. This produces a bag-of-words representation at the
*cluster* level, not the document level.

**Step 5 -- Extract topic labels (class-based TF-IDF).** Instead of regular TF-IDF
(which compares word importance across individual documents), BERTopic uses
*class-based* TF-IDF (c-TF-IDF), which compares word importance across clusters.
Words that are common in one cluster but rare in others become the topic keywords.
This produces readable topic labels like "fat, liberation, body, stigma" for a
cluster of posts about fat liberation.

**Step 6 (optional) -- Polish the labels.** The c-TF-IDF keywords can be further
refined using LLMs, KeyBERT, or other representation models.

### Why It Works Better Than Plain TF-IDF

The key insight is that steps 1-3 use *semantic meaning* to group documents, while
steps 4-5 use *word frequency* only for labeling those groups. Traditional TF-IDF
alone can only find overlap when two people use the same literal words. BERTopic's
embeddings understand that "body autonomy," "fat liberation," and "weight stigma" are
all part of the same conversation, even without any shared keywords.

---

## 2. Rust Availability

### BERTopic Itself: No Rust Port Exists

There is no Rust implementation of BERTopic. The library is Python-only. However,
BERTopic is modular -- each of its five steps is independent, and Rust crates exist
for most of the components.

### Component-by-Component Rust Ecosystem

**Sentence Embeddings (Step 1) -- GOOD availability:**

- **`fastembed` crate** (by the Qdrant team): The best option. A Rust library for
  generating vector embeddings locally using ONNX Runtime. Supports models including
  `all-MiniLM-L6-v2`, `all-MiniLM-L12-v2`, `bge-small-en-v1.5`, and
  `nomic-embed-text-v1`. Also supports quantized model variants (int8) for smaller
  model sizes. Downloads models automatically on first use.
  **Version concern:** fastembed currently pins its ort dependency to `=2.0.0-rc.10`,
  while Charcoal uses `ort 2.0.0-rc.11`. These are exact-version pins and would
  conflict. This would need to be resolved by either downgrading Charcoal's ort to
  rc.10 or waiting for fastembed to update. Alternatively, the embedding model could
  be loaded directly via ort without fastembed as a dependency.

- **Direct `ort` usage:** Since Charcoal already uses `ort 2.0.0-rc.11` for toxicity
  scoring, we could load a sentence-transformer ONNX model directly using the same
  infrastructure. This avoids any dependency conflict. We would need to handle
  tokenization (already have `tokenizers 0.22`) and mean-pooling of the output
  ourselves, but both are straightforward.

- **`candle` framework** (by Hugging Face): A minimalist ML framework for Rust that
  supports BERT-based models natively without ONNX. More control but significantly
  more work to set up compared to ONNX-based approaches. Would add a large new
  dependency.

- **`rust-bert` crate:** Provides transformer pipelines in Rust, including sentence
  embeddings. Uses either `tch-rs` (PyTorch bindings) or ONNX Runtime. Heavier
  dependency than fastembed or direct ort usage.

**Dimensionality Reduction (Step 2) -- EXISTS but may not be needed:**

- **`fast-umap` crate:** A Rust UMAP implementation with configurable parameters.
- **`annembed` crate:** Another Rust dimension reduction library in the UMAP style.
- **`linfa-reduction` crate:** Part of the linfa ML ecosystem, offers PCA and other
  reduction techniques.

**Clustering (Step 3) -- GOOD availability:**

- **`hdbscan` crate** (v0.10.1): Pure Rust HDBSCAN implementation. Supports varying
  density clusters, noise detection, configurable distance metrics. Generic over
  floating point types. This is a mature, well-documented crate.
- **`petal-clustering` crate:** Provides DBSCAN, HDBSCAN, and OPTICS in Rust.

**Cosine Similarity -- Trivial to implement:**

Computing cosine similarity between two vectors is roughly 10 lines of Rust. No
crate is needed, though `ndarray` provides vectorized operations if performance
matters.

---

## 3. Architecture Fit for Charcoal

### What We Already Have

Charcoal's current architecture is well-positioned for an embedding-based approach:

- **`ort 2.0.0-rc.11`:** Already set up for ONNX model inference (used for the
  toxicity model). Loading a second ONNX model is straightforward.
- **`tokenizers 0.22`:** Already handles tokenization. The same crate tokenizes input
  for sentence-transformer models.
- **`TopicExtractor` trait:** The existing trait (`src/topics/traits.rs`) defines a
  clean interface for swapping implementations. A new `EmbeddingExtractor` could
  implement this trait.
- **`TopicFingerprint` struct:** The current data model stores clusters with keywords
  and weights. This could be extended to also store an embedding vector (or the
  embedding could replace the keyword-based approach entirely).

### Two Possible Architectures

**Option A: Full BERTopic Pipeline (Embeddings + UMAP + HDBSCAN + c-TF-IDF)**

This would replicate BERTopic's full pipeline in Rust:
1. Generate embeddings for all posts using an ONNX sentence-transformer model
2. Reduce dimensions with UMAP (via `fast-umap` or `annembed`)
3. Cluster with HDBSCAN (via `hdbscan` crate)
4. Extract topic labels with c-TF-IDF (custom implementation)

Pros: Produces labeled topic clusters, which are nice for human-readable reports.
Cons: Much more complex. Three additional dependencies. Requires enough posts per
account to form meaningful clusters (HDBSCAN typically wants 50+ data points to work
well). Social media posts are very short documents, which clustering algorithms
struggle with.

**Option B: Direct Embedding Comparison (Embeddings + Cosine Similarity)**

This skips UMAP, HDBSCAN, and c-TF-IDF entirely:
1. Generate embeddings for all posts from both the protected user and each candidate
2. Compute an aggregate "topic vector" per account (average of all post embeddings)
3. Compare account vectors using cosine similarity

Pros: Much simpler. Only one new model to add. Cosine similarity on averaged
embeddings works extremely well for "are these two people talking about the same
stuff?" comparisons. No minimum post count needed.
Cons: No labeled topic clusters in the output (though the existing TF-IDF keywords
could still be used for labeling).

### Why Option B Fits Better

BERTopic is designed for a different problem: "What are the topics in this large
corpus?" Charcoal's problem is simpler: "Does person A talk about similar things
as person B?" For this pairwise similarity question, directly comparing embedding
vectors is both simpler and more effective than running a full topic modeling
pipeline. The clustering and labeling steps of BERTopic add complexity without adding
value to the similarity score.

---

## 4. Practical Considerations

### Model Size

| Model | Format | Size | Embedding Dims |
|---|---|---|---|
| all-MiniLM-L6-v2 | ONNX (fp32) | ~90 MB | 384 |
| all-MiniLM-L6-v2 | ONNX (fp16) | ~45 MB | 384 |
| all-MiniLM-L6-v2 | ONNX (int8 quantized) | ~23 MB | 384 |
| all-MiniLM-L12-v2 | ONNX (fp32) | ~130 MB | 384 |
| bge-small-en-v1.5 | ONNX (fp32) | ~130 MB | 384 |
| Current toxicity model | ONNX | ~126 MB | N/A |

The int8 quantized version of `all-MiniLM-L6-v2` at 23 MB is remarkably small and
retains 95%+ similarity fidelity compared to the full-precision model. Even the
full-precision version at 90 MB is comparable to the toxicity model Charcoal already
downloads. A `charcoal download-model` command that grabs both models would total
roughly 150-220 MB depending on which embedding model variant we choose.

### Inference Speed on CPU

Sentence-transformer models like all-MiniLM-L6-v2 process roughly 50 sentences per
second on a single CPU thread. With batching and ONNX Runtime optimizations, this can
reach 100-200 sentences per second. For Charcoal's typical workload (analyzing a few
hundred posts per candidate account), this means:

- 100 posts from a candidate account: ~1-2 seconds to embed
- 50 posts from the protected user: ~0.5-1 second (done once, cached)
- 426 candidate accounts at 100 posts each: ~7-14 minutes total

This is acceptable for a CLI batch tool. The embeddings should be cached in the
database after first computation, so subsequent runs would skip the embedding step
for already-analyzed accounts.

### Memory Requirements

The all-MiniLM-L6-v2 model uses approximately 100-150 MB of RAM when loaded. Since
Charcoal already loads the toxicity model (similar size), peak memory would roughly
double. Both models could be loaded sequentially rather than simultaneously to keep
peak memory under control -- embed first, then score toxicity, rather than doing both
at once.

### Storage for Embeddings

Each post embedding is 384 float32 values = 1,536 bytes. For 100 posts per account
across 426 accounts, that's roughly 65 MB of embedding data in the database. An
aggregate (averaged) embedding per account is just 1,536 bytes each, which is
negligible. Storing per-post embeddings is optional but would allow more sophisticated
analysis later.

---

## 5. How This Would Improve Topic Overlap Scoring

### The Current Problem

Charcoal's current approach works like this:
1. Run TF-IDF to extract keywords from the protected user's posts (e.g., "fat,"
   "liberation," "stigma," "queer," "trans," "governance")
2. Run TF-IDF to extract keywords from a candidate's posts
3. Compare the two keyword sets using weighted Jaccard similarity

The maximum overlap score across 426 accounts was 0.09 out of 1.0. This is
artificially low because weighted Jaccard requires *exact keyword matches*. Two
people can be deeply engaged in the same discourse while using almost entirely
different vocabulary.

### Why Keyword Matching Fails

Consider these two posts:

- Protected user: "Fat liberation is a civil rights movement"
- Candidate: "Fat acceptance is a dangerous lie being pushed on society"

With TF-IDF keyword matching:
- Protected user's keywords might be: "fat," "liberation," "civil," "rights,"
  "movement"
- Candidate's keywords might be: "fat," "acceptance," "dangerous," "lie," "pushed,"
  "society"
- Overlap: only "fat" matches. Jaccard score: very low.

These two people are talking about *exactly the same topic* -- but the keyword overlap
is minimal because they use different words. The TF-IDF approach sees them as
unrelated.

### Why Embeddings Fix This

Sentence embedding models like all-MiniLM-L6-v2 are trained on hundreds of millions
of sentence pairs where humans labeled whether two sentences are about the same thing.
The model learns that:

- "fat liberation" and "fat acceptance" are about the same topic (body politics)
- "civil rights movement" and "dangerous lie being pushed on society" are about
  the same topic (social movements and reactions to them)
- "liberation" and "acceptance" are semantically close in this context

When both posts are converted to 384-dimensional vectors, the cosine similarity
between them will be high (likely 0.6-0.8) because the model understands they are
about the same subject matter. This is true even though the *intent* is opposite
(supportive vs. hostile) -- and that's exactly what we want. Charcoal's job is to
detect topic overlap, not sentiment. Sentiment is handled separately by the toxicity
scorer. Someone who talks about the same topics as the protected user AND is toxic
about those topics is exactly the threat profile Charcoal is designed to detect.

### Why the Same Topic, Different Stance Case Matters

This is the critical scenario for Charcoal:

- The protected user posts supportively about fat liberation
- A hostile account posts mockingly about fat acceptance
- These accounts have HIGH topic overlap (both discussing body politics) and the
  hostile account has HIGH toxicity scores
- Combined, this correctly identifies a threat

With keyword matching, this threat goes undetected because the overlap score is near
zero. With embeddings, the overlap score correctly reflects that both accounts are
engaged in the same discourse, and the toxicity scorer catches the hostile intent.

### What About Allies?

Someone who posts supportively about fat liberation would also have high topic overlap
*and* low toxicity scores. The combined scoring formula would correctly identify them
as a non-threat. Topic overlap alone was never meant to be a threat signal -- it's
the combination with toxicity that matters.

---

## 6. Recommendation

### Do NOT implement full BERTopic. Use direct embedding comparison instead.

BERTopic's full pipeline (embeddings + UMAP + HDBSCAN + c-TF-IDF) is designed for
corpus-level topic discovery: "What are all the topics in these 100,000 documents?"
That is not Charcoal's problem.

Charcoal's problem is pairwise account similarity: "Does this candidate account
talk about the same things as the protected user?" For this, directly comparing
averaged embedding vectors with cosine similarity is simpler, faster, and equally
effective.

### Recommended Approach

1. **Add a sentence-transformer ONNX model** (all-MiniLM-L6-v2, quantized int8
   variant at ~23 MB) to the existing model download infrastructure.

2. **Generate post embeddings using the existing `ort` crate.** No new ONNX runtime
   dependency needed. Use the existing `tokenizers` crate for tokenization. Handle
   mean-pooling in Rust (straightforward vector arithmetic).

3. **Compute per-account topic vectors** by averaging all post embeddings for each
   account. Store these in the database.

4. **Replace weighted Jaccard** with cosine similarity between account-level embedding
   vectors. The `TopicExtractor` trait and `TopicFingerprint` struct can be extended
   to carry embedding data alongside the existing keyword data.

5. **Keep TF-IDF keywords for display.** The existing TF-IDF extraction is still
   useful for human-readable topic labels in reports. It just should not be used for
   the similarity *score*.

### Why This Works for Charcoal Specifically

- **No new heavy dependencies.** Uses `ort` and `tokenizers` already in the project.
- **Small model footprint.** The int8 quantized model is only 23 MB.
- **Fast enough for CLI.** 50+ sentences/second on CPU is fine for batch analysis.
- **Solves the actual problem.** Cosine similarity on semantic embeddings will
  produce overlap scores in the 0.3-0.8 range for accounts discussing the same
  topics, compared to the current 0.0-0.09 range from keyword matching.
- **Architecturally clean.** Implements the existing `TopicExtractor` trait. Can
  coexist with TF-IDF as a fallback.
- **No minimum data requirement.** Unlike HDBSCAN clustering, cosine similarity
  works fine even with 5-10 posts per account.

### What We Skip (and Why That Is Fine)

| BERTopic Step | Needed? | Reason |
|---|---|---|
| Sentence Embeddings | YES | Core of the improvement |
| UMAP | NO | Only needed before HDBSCAN clustering |
| HDBSCAN | NO | We are comparing accounts, not discovering topics |
| c-TF-IDF | NO | Existing TF-IDF keywords work for labels |

### Migration Path

This can be implemented incrementally:
1. Add the embedding model download to `charcoal download-model`
2. Implement an `EmbeddingExtractor` behind the `TopicExtractor` trait
3. Store embeddings in the database alongside existing fingerprints
4. Switch the overlap score from Jaccard to cosine similarity
5. Keep TF-IDF keywords for report display

The existing TF-IDF path remains as a fallback for users who do not want to download
the embedding model (similar to how the Perspective API is a fallback for toxicity
scoring).

---

## Sources

- [BERTopic - The Algorithm](https://maartengr.github.io/BERTopic/algorithm/algorithm.html)
- [BERTopic Documentation](https://maartengr.github.io/BERTopic/index.html)
- [sentence-transformers/all-MiniLM-L6-v2 on Hugging Face](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2)
- [Xenova/all-MiniLM-L6-v2 ONNX variants](https://huggingface.co/Xenova/all-MiniLM-L6-v2)
- [fastembed-rs on GitHub](https://github.com/Anush008/fastembed-rs)
- [fastembed-rs on crates.io](https://crates.io/crates/fastembed)
- [hdbscan crate on crates.io](https://crates.io/crates/hdbscan)
- [fast-umap crate on crates.io](https://crates.io/crates/fast-umap)
- [rust-bert on GitHub](https://github.com/guillaume-be/rust-bert)
- [candle framework on GitHub](https://github.com/huggingface/candle)
- [ort crate on crates.io](https://crates.io/crates/ort)
- [Pinecone: Advanced Topic Modeling with BERTopic](https://www.pinecone.io/learn/bertopic/)
- [Speeding up Inference -- Sentence Transformers docs](https://sbert.net/docs/sentence_transformer/usage/efficiency.html)
- [ONNX Optimization of Sentence Transformer Models](https://medium.com/@TheHaseebHassan/onnx-optimization-of-sentence-transformer-pytorch-models-e24bdbed9696)
