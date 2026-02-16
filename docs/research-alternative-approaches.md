# Alternative Approaches to Topic Overlap Scoring: Research Report

**Charcoal Project** | February 16, 2026

---

## Problem Statement

Charcoal's topic overlap scoring uses TF-IDF keyword extraction with weighted
Jaccard similarity to compare a protected user's topic profile against other
Bluesky accounts. In testing against 426 accounts, every single score came
back under 0.10 -- effectively zero discrimination between accounts that
clearly share topic areas and those that do not.

The root causes are:

1. **Vocabulary mismatch**: TF-IDF operates on exact single-word tokens. An
   account posting about "fat liberation" and another posting "fat acceptance
   is a lie" share the word "fat" but their other keywords diverge. The
   protected user might use "liberation" while an adversary uses "acceptance"
   or "obesity" -- different words, same topic space.

2. **Weighted Jaccard is wrong for sparse vectors**: Jaccard similarity
   measures set overlap. When two keyword weight maps have hundreds of keys
   each and only a handful overlap, the denominator (union of all keys)
   dwarfs the numerator (intersection). This structurally compresses scores
   toward zero.

3. **Independent IDF corpora**: The protected user's IDF is computed over
   their own posts. Each target account's IDF is computed over their own
   posts. Two people posting about identical topics will produce different
   keyword rankings because their IDF denominators are different document
   sets.

4. **Single-word tokens miss multi-word concepts**: "Fat liberation," "body
   positivity," "diet culture," "trans rights," "queer identity" -- these are
   all multi-word phrases that lose meaning when split into individual tokens.

This report evaluates six alternative approaches and recommends a path forward.

---

## Table of Contents

1. [Approach 1: Sentence Embeddings via ONNX](#approach-1-sentence-embeddings-via-onnx)
2. [Approach 2: Improved Keyword Methods](#approach-2-improved-keyword-methods)
3. [Approach 3: Hashtag and Entity Analysis](#approach-3-hashtag-and-entity-analysis)
4. [Approach 4: Zero-Shot Topic Classification](#approach-4-zero-shot-topic-classification)
5. [Approach 5: Hybrid Multi-Signal Approach](#approach-5-hybrid-multi-signal-approach)
6. [Approach 6: What Existing Tools Use](#approach-6-what-existing-tools-use)
7. [Comparison Matrix](#comparison-matrix)
8. [Ranked Recommendations](#ranked-recommendations)

---

## Approach 1: Sentence Embeddings via ONNX

### How It Works

Instead of extracting keywords from posts, embed each post as a dense vector
in a high-dimensional space (384 dimensions for MiniLM-class models). Posts
about similar topics land near each other in this space, even when they use
completely different words. "Fat liberation is a civil rights movement" and
"The obesity acceptance movement is dangerous" would produce vectors with
meaningful cosine similarity because the model learned during training that
these sentences exist in the same semantic neighborhood.

The process:

1. **Embed each post** through a sentence-transformer ONNX model, producing a
   384-dimensional float vector per post.
2. **Compute an account-level centroid** by averaging all post vectors for an
   account. This gives one vector that represents "what this account talks
   about."
3. **Compare centroids** using cosine similarity: `dot(A, B) / (|A| * |B|)`.
   Cosine similarity naturally produces scores from -1.0 to 1.0, with most
   relevant comparisons falling in the 0.3-0.9 range -- far more
   discriminating than the current 0.00-0.10 range.

### Model Options

**all-MiniLM-L6-v2** (recommended):
- 22M parameters, 6 transformer layers
- 384-dimensional output vectors
- Max sequence length: 256 tokens (fine for social media posts)
- ONNX quantized (INT8): ~23 MB model file
- ONNX FP16: ~45 MB model file
- Inference: ~5-15ms per post on CPU
- Pre-exported ONNX available at `onnx-models/all-MiniLM-L6-v2-onnx` on
  HuggingFace, and also at `Xenova/all-MiniLM-L6-v2`

**intfloat/multilingual-e5-small**:
- 118M parameters, 12 layers
- 384-dimensional output
- Better accuracy on benchmarks, 5x slower
- Supported by `fastembed` crate

**BAAI/bge-small-en-v1.5** (default in fastembed):
- Similar size to MiniLM
- Strong retrieval performance
- Well-tested in the Rust ecosystem

### Rust Implementation Path

There are two integration strategies:

**Option A: Use the `fastembed` crate (recommended)**

The `fastembed` crate (v5.9.0, Apache 2.0) is a Rust library purpose-built
for generating embeddings locally. Critical detail: it depends on
`ort =2.0.0-rc.11` and `tokenizers ^0.22.0` -- the exact same versions
Charcoal already uses. This means no dependency conflicts.

```
fastembed = "5.9"
```

The API is straightforward:

- `TextEmbedding::try_new(InitOptions::new(EmbeddingModel::AllMiniLML6V2))`
  to load the model
- `model.embed(documents, None)` to generate embeddings
- Returns `Vec<Vec<f32>>` -- one vector per document

fastembed handles model downloading, caching, tokenization, ONNX inference,
and mean pooling internally. It supports 20+ models including all-MiniLM-L6-v2,
BGE-small, E5-small, and Nomic-embed. It also adds `ndarray` and
`safetensors` as dependencies, but no heavy additions beyond what Charcoal
already carries.

**Option B: Direct ONNX with `ort` + `tokenizers`**

Use the crates Charcoal already has. This requires:

1. Downloading the ONNX model file and tokenizer.json
2. Tokenizing input with the `tokenizers` crate
3. Running inference through `ort::Session`
4. Applying mean pooling to token embeddings manually (the ONNX model outputs
   token-level embeddings, not sentence embeddings)
5. L2-normalizing the result

This is more work but adds zero new dependencies. The mean pooling step is
approximately 10 lines of code: multiply each token embedding by its
attention mask, sum across tokens, divide by the mask sum.

### Accuracy Assessment

This approach directly solves the vocabulary mismatch problem. Sentence
embeddings capture semantic meaning, not lexical overlap. An account posting
supportively about "fat liberation" and an adversary posting "fat acceptance
is delusional" would both produce vectors near the "body politics" region of
embedding space. Their cosine similarity would likely be 0.4-0.7 -- clearly
distinguishable from an account posting about Rust programming (which might
score 0.0-0.1).

The critical test case -- "fat liberation" vs "fat acceptance is a lie" --
would work correctly because sentence transformers are trained on semantic
similarity tasks and understand that these sentences are about the same topic
regardless of stance.

One limitation: embedding similarity does not distinguish between supportive
and hostile engagement with a topic. An ally and an adversary posting about
the same topic will have similar embeddings. This is actually acceptable for
Charcoal's architecture because topic overlap is combined with toxicity
scoring -- the toxicity signal distinguishes allies from threats.

### Resource Cost

- Model download: 23-45 MB (one-time, comparable to the existing ~126 MB
  toxicity model)
- Memory: ~100-200 MB at runtime (model loaded into memory)
- Inference: ~5-15ms per post, or ~100-300ms for 20 posts per account
- For 426 accounts at 20 posts each: ~40-130 seconds total embedding time

### Complexity

Using `fastembed`: Low-medium. The crate handles the hard parts. Main work
is integrating it into the `TopicExtractor` trait, computing centroids, and
writing cosine similarity. Likely 1-2 sessions of work.

Using raw `ort`: Medium. Need to handle tokenization, inference, pooling, and
normalization manually, but the patterns are identical to the existing
toxicity scorer. Likely 2-3 sessions.

---

## Approach 2: Improved Keyword Methods

### 2a: N-grams (Bigrams and Trigrams)

Instead of splitting "fat liberation movement" into three separate tokens,
extract bigrams ("fat liberation", "liberation movement") and trigrams ("fat
liberation movement") as units.

**Rust feasibility**: The `keyword_extraction` crate that Charcoal already
uses includes a YAKE algorithm (feature flag `yake`) that extracts keyphrases
with configurable n-gram size (defaults to 1-3 grams). Enabling the `yake`
feature on the existing dependency would provide n-gram support immediately.

There is also a standalone `yake-rust` crate and a purpose-built
`tfidf_sparsevec` crate that implements the full pipeline: text -> stemming ->
bigrams -> TF-IDF -> sparse vector -> cosine distance.

**Accuracy**: Moderate improvement. "Fat liberation" as a bigram would match
better between accounts. But it still fails on paraphrasing -- "body
autonomy" and "fat liberation" are topically related but share zero n-grams.

**Complexity**: Low. Enable a feature flag, adjust extraction parameters.

### 2b: Cosine Similarity on TF-IDF Vectors

Replace weighted Jaccard with cosine similarity on the raw TF-IDF vectors.
This is the single highest-impact change that could be made to the existing
approach with minimal effort.

**Why it helps**: Weighted Jaccard divides `sum(min)` by `sum(max)`. When the
union of keywords is large (which it always is -- two different people use
different words), the denominator dominates. Cosine similarity instead
measures the angle between the two vectors, which is robust to the number of
dimensions. Two accounts that share 5 keywords out of 200 total will get a
low Jaccard score but a meaningful cosine score, because cosine only considers
the dimensions where both vectors are non-zero.

Academic research confirms this: studies comparing cosine similarity and
Jaccard for text matching consistently find cosine outperforms Jaccard when
applied to TF-IDF vectors, particularly for documents of different lengths
(which is exactly the case here -- the protected user has 100+ posts analyzed,
target accounts have 20).

**Rust feasibility**: Cosine similarity on two `HashMap<String, f64>` maps
is ~15 lines of code. The `rnltk` crate also provides it, as does
`tf-idf-vectorizer`. But hand-rolling it is trivial: dot product divided by
the product of L2 norms.

**Accuracy**: Would likely move scores from the 0.00-0.10 range to
0.05-0.40 -- a meaningful improvement but still limited by vocabulary
mismatch.

**Complexity**: Very low. Replace one function.

### 2c: Shared Vocabulary / Unified IDF Corpus

Currently, each account's TF-IDF is computed independently. The protected
user's posts form one corpus (determining IDF weights), and each target
account's posts form separate corpora. This means the same word gets different
weights for different accounts, making comparison unreliable.

The fix: combine the protected user's posts with each target account's posts
into a single corpus, then compute TF-IDF over that combined corpus. Or
better: use the protected user's posts as the fixed IDF reference, and score
each target account's posts against that reference vocabulary.

**Accuracy**: Moderate improvement. Words that are distinctive to the
protected user would be properly weighted when scoring other accounts.

**Complexity**: Medium. Requires restructuring how TF-IDF is computed. The
`keyword_extraction` crate's `TfIdfParams::UnprocessedDocuments` expects a
single document set; would need to build a combined set and then split results.

### 2d: Fuzzy / Partial Keyword Matching

Use string similarity (Levenshtein distance, Jaro-Winkler) to match keywords
that are similar but not identical: "fatphobia" and "fatphobic," "trans" and
"transgender," "queer" and "queerness."

**Rust feasibility**: The `strsim` crate provides Levenshtein, Jaro-Winkler,
and other string distance metrics. The `ngrammatic` crate provides n-gram
based fuzzy matching.

**Accuracy**: Marginal improvement. Handles morphological variants but does
not solve the fundamental semantic gap ("body autonomy" vs "fat liberation").

**Complexity**: Low-medium. Add fuzzy matching to the overlap computation.

### Overall Assessment of Keyword Improvements

Combining 2a + 2b + 2c would produce a meaningfully better keyword-based
system: n-gram keyphrases, cosine similarity, unified IDF corpus. This
combination would likely move scores from 0.00-0.10 to 0.10-0.50 for
topically related accounts. However, it still cannot bridge semantic gaps
where the same topic is discussed using entirely different vocabulary.

---

## Approach 3: Hashtag and Entity Analysis

### How It Works

Bluesky posts can contain hashtags as rich text facets. The AT Protocol
stores hashtags in the post's `facets` array as
`app.bsky.richtext.facet#tag` objects with a `tag` attribute. Unlike
keywords, hashtags are intentional self-categorization by the poster.

The approach:

1. **Extract hashtag facets** from each post's structured data (not parsing
   the text, but reading the `facets` field from the post record).
2. **Build a hashtag frequency profile** for each account: which hashtags
   they use, how often.
3. **Compare hashtag profiles** using Jaccard or cosine similarity.

### Rust Feasibility

Charcoal already fetches post data through `bsky-sdk`. The post record
includes a `facets` field. Extracting tag facets from the existing data
structures would require deserializing the facet objects from the post's
`Unknown` record type, filtering for `#tag` type features, and collecting
the tag strings. This is primarily a data extraction task, not an ML task.

### Accuracy Assessment

**Strengths**:
- Hashtags are high-signal. Someone using #FatLiberation, #BodyPositivity,
  #TransRights is unambiguously in that topic space.
- Exact matching works perfectly -- hashtags are standardized labels.
- No model needed, no inference cost.

**Weaknesses**:
- **Most Bluesky posts do not use hashtags**. Unlike Instagram or Twitter,
  Bluesky's culture does not heavily emphasize hashtag use. Many accounts will
  have zero or very few hashtags, producing empty profiles.
- **Adversaries rarely self-categorize honestly**. Someone quote-posting to
  mock fat liberation content is unlikely to tag their post
  #FatLiberation. They might use no hashtags at all, or use adversarial tags
  like #CommonSense.
- **Low coverage**: Useful when present, but absent too often to be a
  primary signal.

### Entity Extraction Alternative

Instead of (or in addition to) hashtags, extract named entities from post
text: mentions of specific people, organizations, movements, events. The
`ner` feature of models like BERT can identify entities, but this adds
significant ML complexity. A simpler approach would be to maintain a
curated list of topic-relevant entities and check for their presence.

### Resource Cost

Minimal. No model, no inference. Just data structure parsing.

### Complexity

Low for hashtag extraction. Medium for entity extraction with a curated list.
High for ML-based named entity recognition.

### Verdict

Hashtag analysis is a useful supplementary signal but cannot be a primary
topic overlap method due to low coverage. Best used as a component in a
hybrid approach (see Approach 5).

---

## Approach 4: Zero-Shot Topic Classification

### How It Works

Use a Natural Language Inference (NLI) model to classify each post into
predefined topic categories without any training data. The model takes a
post as a "premise" and a hypothesis like "This post is about fat liberation
and body politics" and returns a probability that the hypothesis is true.

Define 10-15 topic categories relevant to the protected user (derived from
their fingerprint), then classify each post from each account into those
categories. Compare the resulting category distributions between accounts.

### Model Options

**facebook/bart-large-mnli** (the standard):
- 406M parameters
- Trained on MultiNLI (393K premise-hypothesis pairs)
- Produces entailment/contradiction/neutral probabilities
- Model size: ~1.6 GB (FP32), ~400 MB (quantized ONNX)
- Inference: ~100-500ms per classification per label on CPU
- Available in ONNX via `navteca/bart-large-mnli`
- Available in `rust-bert` natively

**MoritzLaurer/DeBERTa-v3-xsmall-mnli-fever-anli-ling-binary** (smaller):
- 22M backbone parameters (1/4 of RoBERTa-base)
- Trained on MultiNLI + Fever-NLI + ANLI (764K pairs)
- Binary (entailment vs not-entailment) -- simpler output
- Would need ONNX conversion

**MoritzLaurer/DeBERTa-v3-base-mnli-fever-anli** (best accuracy):
- 86M parameters
- Trained on 764K NLI pairs
- Highest accuracy of the DeBERTa zero-shot family
- Available on HuggingFace, would need ONNX export

### Rust Feasibility

The `rust-bert` crate has native zero-shot classification support with
BART-large-mnli as the default model. However, `rust-bert` uses `tch-rs`
(LibTorch bindings), which adds a ~2 GB dependency on LibTorch. This is a
heavy addition.

Alternatively, an ONNX-converted model could be loaded with `ort`, using
Charcoal's existing infrastructure. However, zero-shot classification
requires a specific inference pattern: the model must be run once per
candidate label per post (premise-hypothesis pairs), making it N times
slower than single-pass models where N is the number of topic labels.

### Accuracy Assessment

**Strengths**:
- Directly answers "is this post about fat liberation?" rather than
  inferring it from keyword overlap.
- Category distributions are easy to compare and interpret.
- Can define categories that capture the protected user's exact topic areas.
- Handles paraphrasing naturally -- "body autonomy," "fat acceptance," and
  "weight stigma" would all classify into the same body politics category.

**Weaknesses**:
- **Requires predefined categories**: The CLAUDE.md notes that the protected
  user "cannot fully enumerate their own topic areas." Zero-shot
  classification with fixed labels creates the same rigidity problem.
  New topics would require adding new labels.
- **Multiplicative inference cost**: With 10 labels and 20 posts per account,
  that is 200 NLI inferences per account. At 100-500ms each on CPU, that is
  20-100 seconds per account, or 2.4-12 hours for 426 accounts. This is
  prohibitively slow.
- **BART-large is big**: 400 MB-1.6 GB model file, ~2 GB RAM at inference.
  Combined with the existing toxicity model, this could push memory
  requirements past what is practical for a CLI tool.

### Resource Cost

- Model download: 400 MB - 1.6 GB
- Memory: ~1-2 GB at runtime
- Inference: 20-100 seconds per account on CPU (unacceptable for batch
  processing)
- For 426 accounts: 2.4-12 hours

### Complexity

High. Requires either adding `rust-bert` (heavy dependency) or manually
implementing NLI inference patterns with ONNX. Label set design requires
iteration. The multiplicative inference cost may require GPU infrastructure
to be practical.

### Verdict

Theoretically appealing but practically infeasible for Charcoal's MVP. The
inference cost alone is a dealbreaker for batch processing hundreds of
accounts on CPU. Could be reconsidered for future versions with GPU
infrastructure and real-time monitoring (where you classify one account at a
time rather than batch).

---

## Approach 5: Hybrid Multi-Signal Approach

### How It Works

Combine multiple weak signals into a stronger composite topic overlap score:

1. **Embedding similarity** (Approach 1): Semantic understanding of topic
   proximity.
2. **Keyword/keyphrase overlap** (Approach 2, improved): Lexical signal that
   catches exact terminology matches.
3. **Hashtag overlap** (Approach 3): High-confidence signal when available.

Weight the signals based on availability and confidence:

```
topic_overlap = w1 * embedding_similarity
              + w2 * keyphrase_cosine_similarity
              + w3 * hashtag_jaccard_similarity (if hashtags present)
```

Where the weights might be something like w1=0.6, w2=0.3, w3=0.1 (tuned
empirically). If no hashtags are available, redistribute: w1=0.7, w2=0.3.

### Why Hybrid is Better Than Any Single Method

Academic research on topic detection in social media consistently finds that
hybrid approaches outperform single-signal methods:

- A 2019 study in *Measurement and Control* proposed combining word
  embeddings with topic models for social media topic detection, finding
  that the combination captured both semantic and lexical relations.
- Research on user profiling with word embeddings (Alekseev & Nikolenko,
  2017) found that embedding-based profiles significantly augmented
  traditional keyword-based user profiles.
- The BERTopic library, widely used for production topic modeling, internally
  combines sentence embeddings (for clustering) with c-TF-IDF (for topic
  description) -- validating the embedding + keyword hybrid.
- A 2025 study using semantic similarity to measure communication strategy
  echo used embedding cosine similarity as the primary signal with keyword
  analysis as validation.

For Charcoal specifically:

- **Embeddings catch paraphrasing**: "body autonomy" and "fat liberation"
  map to similar vectors even though they share no keywords.
- **Keywords catch exact terminology**: When an adversary uses the exact same
  term ("fat") as the protected user, keyword overlap catches it even if
  their overall embedding centroids diverge (because most of their posts are
  about other topics).
- **Hashtags catch intentional categorization**: When present, hashtags are
  the highest-confidence signal available.

### Rust Feasibility

All three signals use different computation methods, each already addressed
above. The combination is just weighted arithmetic -- trivial to implement
once the individual signals exist.

### Accuracy Assessment

This is the most robust approach. It degrades gracefully: if hashtags are
absent, the other two signals carry the load. If an account posts very short
messages that embed poorly, keyword overlap still works. The failure modes of
each individual method are compensated by the others.

### Resource Cost

Dominated by the embedding computation (see Approach 1). Keyword extraction
and hashtag parsing add negligible overhead.

### Complexity

Medium-high overall, but decomposable: implement embedding similarity first
(highest impact), then add keyword improvements, then add hashtag analysis.
Each piece can ship independently.

---

## Approach 6: What Existing Tools Use

### Bluesky Ecosystem Tools

**Ozone (Bluesky's official labeling tool)**:
- Ozone is a web interface for human moderators to apply labels to content.
  It does not do automated topic detection. Labels are applied manually or
  through external AI tools that feed into Ozone via the labeler API.
- Bluesky's own moderation uses "people, AI, or a combination of both" for
  content classification, but the specific AI methods are not documented.
- Community labelers (listed at bluesky-labelers.io) use various approaches
  including AI classification, but implementation details are per-labeler.

**Gomoderate**:
- A Go CLI tool for automated Bluesky moderation. It works by importing
  block lists from trusted accounts and bulk-muting users. No topic analysis
  -- it is purely trust-graph based ("if accounts I trust blocked this
  person, I should mute them too").

**Bluesky Block List Proposals**:
- Community proposals for Bluesky block lists focus on social graph analysis
  (who blocks whom, concentration of block power) rather than topic
  similarity. No topic-based blocking tools were found in the Bluesky
  open-source ecosystem.

### Academic and Research Approaches

**BERTopic (Python, widely cited)**:
- The state-of-the-art topic modeling library combines sentence-transformer
  embeddings, UMAP dimensionality reduction, HDBSCAN clustering, and
  c-TF-IDF for topic description.
- This validates the embedding + TF-IDF hybrid approach. However, BERTopic
  is Python-only and designed for clustering a single corpus, not comparing
  two users' topic profiles.
- Charcoal does not need the full BERTopic pipeline (UMAP, HDBSCAN) because
  we are not discovering topics from scratch -- we are comparing two known
  profiles.

**Word Embeddings for User Profiling (Alekseev & Nikolenko, 2017)**:
- Showed that aggregating word embeddings from a user's posts creates
  effective user-level topic profiles for comparison. This is essentially
  what Approach 1 proposes -- averaging post embeddings into an account-level
  centroid.

**ClusTop (2022)**:
- A clustering-based topic model that uses community detection on a graph
  where nodes are words/phrases and edges represent co-occurrence or
  embedding similarity. This is more complex than Charcoal needs but
  validates combining lexical (co-occurrence) and semantic (embedding)
  signals.

### Content Moderation Industry

**General pattern**: Modern content moderation tools (NapoleonCat,
CommentGuard, StatusBrew, etc.) classify content into categories using
LLM-based classifiers or fine-tuned models. They do not publish their topic
similarity algorithms. Most focus on classifying individual pieces of content
rather than comparing user profiles.

**Observation**: No widely-used open-source tool was found that does exactly
what Charcoal does -- compare two social media users' topic profiles for
overlap as a component of threat scoring. Charcoal's use case (predictive
threat detection via topic + toxicity combination) appears to be novel.
The closest analogy is BERTopic-style topic modeling applied per-user, which
is what the embedding centroid approach effectively does.

---

## Comparison Matrix

| Approach | Accuracy (Semantic) | Accuracy (Lexical) | Rust Feasibility | Model Size | Inference Speed | Implementation Work |
|----------|--------------------|--------------------|------------------|------------|-----------------|---------------------|
| **1. Sentence Embeddings** | Excellent | N/A | Excellent (`fastembed` crate) | 23-45 MB | ~5-15ms/post | Low-Medium (1-2 sessions) |
| **2a. N-grams** | Poor | Good | Excellent (existing crate) | None | Instant | Low (enable feature flag) |
| **2b. Cosine on TF-IDF** | Poor | Moderate | Trivial | None | Instant | Very Low (replace 1 function) |
| **2c. Shared IDF** | Poor | Good | Moderate | None | Instant | Medium (restructure TF-IDF) |
| **2d. Fuzzy matching** | Poor | Moderate | Good (`strsim` crate) | None | Fast | Low-Medium |
| **3. Hashtags** | N/A | Exact | Good (parse facets) | None | Instant | Low |
| **4. Zero-Shot Classification** | Excellent | N/A | Difficult | 400 MB-1.6 GB | 100-500ms/label/post | High |
| **5. Hybrid (1+2+3)** | Excellent | Good | Good | 23-45 MB | ~5-15ms/post + trivial | Medium-High (phased) |

---

## Ranked Recommendations

### Rank 1: Sentence Embeddings via fastembed (Approach 1)

**This should be the primary replacement.**

Rationale:
- Directly solves the core problem (vocabulary mismatch / semantic gap)
- The `fastembed` crate uses `ort =2.0.0-rc.11` and `tokenizers ^0.22.0`,
  exactly matching Charcoal's existing dependency versions -- zero conflicts
- all-MiniLM-L6-v2 at 23 MB (INT8 ONNX) is smaller than the existing
  toxicity model (126 MB)
- 5-15ms per post inference means 426 accounts can be processed in minutes,
  not hours
- The `TopicExtractor` trait already exists as an abstraction point -- a new
  `EmbeddingExtractor` implementation slots in cleanly
- Cosine similarity on centroids produces scores in a useful 0.0-0.9 range
  instead of the current 0.00-0.10 range
- Proven approach: this is what BERTopic, semantic search engines, and
  production RAG systems use for measuring semantic similarity

Implementation plan:
1. Add `fastembed = "5.9"` to Cargo.toml
2. Implement a new `EmbeddingExtractor` struct behind the `TopicExtractor`
   trait (or a new `TopicComparer` trait if the abstraction needs changing)
3. Add a `charcoal download-embeddings-model` command (or extend
   `download-model`)
4. Compute account-level centroid embeddings
5. Replace weighted Jaccard with cosine similarity
6. Tune/validate against known topically-similar and topically-different
   accounts

### Rank 2: Replace Jaccard with Cosine Similarity (Approach 2b)

**This should be done immediately regardless of other changes.**

Rationale:
- Highest impact-to-effort ratio of any change
- Takes 30 minutes to implement
- Does not require any new dependencies
- Even if embeddings are added later, having cosine similarity as the
  comparison function is the correct choice for any vector-based comparison
- Removes the structural compression-to-zero problem in the current scoring

This is a prerequisite fix that should ship before any larger architectural
change.

### Rank 3: Hybrid Approach with N-grams (Approach 5 = 1 + 2a + 2b + 3)

**Build toward this incrementally after Rank 1 and Rank 2 ship.**

Rationale:
- Embedding similarity alone might miss cases where exact terminology overlap
  is the strongest signal
- N-gram keyphrases ("fat liberation" as a unit) complement embeddings by
  catching lexical precision
- Hashtag overlap, when available, is the highest-confidence signal
- Academic research consistently validates hybrid over single-signal approaches
- Can be built incrementally: ship embeddings first, add keyword improvements
  later, add hashtag extraction when convenient

Suggested weights (starting point, tune empirically):
- Embedding similarity: 0.65
- Keyphrase cosine similarity (with n-grams): 0.25
- Hashtag overlap: 0.10 (when available; redistribute to embeddings when not)

### Rank 4: N-gram Keyphrase Extraction (Approach 2a)

**Low effort, moderate value as part of the hybrid.**

Enable the `yake` feature on the existing `keyword_extraction` crate to
extract multi-word keyphrases. Replace single-word TF-IDF keywords with
YAKE keyphrases in the fingerprint. This can be done independently of the
embedding work and improves the keyword signal for the hybrid approach.

### Rank 5: Hashtag Extraction (Approach 3)

**Worth doing but low priority due to low coverage.**

Parse hashtag facets from Bluesky post data. Useful as a supplementary signal
in the hybrid approach but cannot stand alone because most accounts do not use
hashtags consistently.

### Not Recommended for MVP: Zero-Shot Classification (Approach 4)

The inference cost (100-500ms per label per post, multiplied across 10+
labels and 20 posts per account) makes this impractical for batch processing
on CPU. The model size (400 MB-1.6 GB) is also problematic. Revisit when
Charcoal has GPU infrastructure and real-time per-account processing.

---

## Implementation Sequence

| Step | What | Dependencies | Estimated Effort |
|------|------|-------------|-----------------|
| 1 | Replace Jaccard with cosine similarity | None | 30 minutes |
| 2 | Add `fastembed`, implement embedding-based topic comparison | Step 1 | 1-2 sessions |
| 3 | Enable YAKE n-gram keyphrases | None (parallel with step 2) | 1 session |
| 4 | Combine signals into hybrid score | Steps 2 + 3 | 1 session |
| 5 | Add hashtag facet extraction | None (parallel) | 1 session |
| 6 | Integrate hashtag signal into hybrid | Steps 4 + 5 | 30 minutes |

Steps 1 through 4 are the critical path. Steps 5 and 6 can be deferred.

---

## Sources

### Models and Libraries
- [sentence-transformers/all-MiniLM-L6-v2](https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2) -- Sentence-transformer model
- [Xenova/all-MiniLM-L6-v2](https://huggingface.co/Xenova/all-MiniLM-L6-v2) -- ONNX-exported variant with quantized models
- [onnx-models/all-MiniLM-L6-v2-onnx](https://huggingface.co/onnx-models/all-MiniLM-L6-v2-onnx) -- ONNX model
- [fastembed-rs](https://github.com/Anush008/fastembed-rs) -- Rust embedding library (Apache 2.0)
- [fastembed on crates.io](https://crates.io/crates/fastembed) -- v5.9.0
- [fastembed docs.rs](https://docs.rs/fastembed/latest/fastembed/) -- API documentation
- [facebook/bart-large-mnli](https://huggingface.co/facebook/bart-large-mnli) -- Zero-shot classification model
- [MoritzLaurer/DeBERTa-v3-base-mnli-fever-anli](https://huggingface.co/MoritzLaurer/DeBERTa-v3-base-mnli-fever-anli) -- NLI model
- [MoritzLaurer/DeBERTa-v3-xsmall-mnli-fever-anli-ling-binary](https://huggingface.co/MoritzLaurer/DeBERTa-v3-xsmall-mnli-fever-anli-ling-binary) -- Small NLI model

### Rust Crates
- [keyword_extraction](https://crates.io/crates/keyword_extraction) -- TF-IDF, RAKE, TextRank, YAKE
- [yake-rust](https://crates.io/crates/yake-rust) -- Standalone YAKE implementation
- [tfidf_sparsevec](https://github.com/SextantAI/tfidf_sparsevec) -- Bigram TF-IDF with cosine distance
- [rnltk](https://lib.rs/crates/rnltk) -- Rust NLP toolkit with TF-IDF and cosine similarity
- [tf-idf-vectorizer](https://docs.rs/tf-idf-vectorizer) -- TF-IDF vectorizer crate
- [ngrammatic](https://github.com/compenguy/ngrammatic) -- N-gram fuzzy matching
- [rust-bert](https://docs.rs/rust-bert/latest/rust_bert/pipelines/zero_shot_classification/) -- Zero-shot classification in Rust

### Bluesky / AT Protocol
- [Bluesky rich text facets](https://docs.bsky.app/docs/advanced-guides/post-richtext) -- Hashtag facet documentation
- [app.bsky.richtext.facet schema](https://github.com/bluesky-social/atproto/blob/main/lexicons/app/bsky/richtext/facet.json) -- Facet JSON schema
- [Bluesky moderation architecture](https://docs.bsky.app/blog/blueskys-moderation-architecture)
- [Ozone labeling tool](https://github.com/bluesky-social/ozone)
- [Gomoderate](https://github.com/thepudds/gomoderate) -- Bluesky bulk moderation CLI (Go)

### Research
- [Word Embedding Topic Model for Social Media (2019)](https://journals.sagepub.com/doi/full/10.1177/0020294019865750)
- [Word Embeddings for User Profiling (Alekseev & Nikolenko, 2017)](https://www.semanticscholar.org/paper/Word-Embeddings-for-User-Profiling-in-Online-Social-Alekseev-Nikolenko/5dae4d279de3a0130a32bd45d7c1efcae6f2e4e9)
- [ClusTop: Clustering-based topic model using word networks and embeddings (2022)](https://journalofbigdata.springeropen.com/articles/10.1186/s40537-022-00585-4)
- [Semantic Similarity for Strategic Communications (2025)](https://epjdatascience.springeropen.com/articles/10.1140/epjds/s13688-025-00538-w)
- [BERTopic: Topic Modeling with BERT](https://maartengr.github.io/BERTopic/algorithm/algorithm.html)
- [Cosine vs Jaccard Similarity for Text (ScienceDirect)](https://www.sciencedirect.com/science/article/pii/S1877050924003971)
- [Cosine vs Jaccard pros and cons (LinkedIn)](https://www.linkedin.com/advice/3/what-pros-cons-using-cosine-similarity-vs)
- [Optimizing Sentence Transformer Inference (Philschmid)](https://www.philschmid.de/optimize-sentence-transformers)
