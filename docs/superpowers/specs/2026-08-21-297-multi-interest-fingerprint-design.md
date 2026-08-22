# Design: Multi-interest topic fingerprint (#297) + atomic fingerprint persistence (#302)

**Date:** 2026-08-21
**Issues:** chainlink #297 (feature), #302 (persistence fix, folded in by design)
**Source research:** spike #295, `docs/research/topic-fingerprint-spike-2026-08.md` §3 BETTER tier (local, git-ignored)
**Branch:** `feat/297-multi-interest-fingerprint` → `staging`

## Problem

Topic overlap today is a cosine between two single mean-pooled 384-dim centroids.
A multi-topic protected user (fat liberation + a cappella + Atlassian + …)
averages into one vector that sits in *nobody's* topic region, so niche co-topic
accounts and unrelated accounts can score similarly (spike defect #1). The rich
`TopicCluster` keyword structure is discarded on the embedding path — the only
path that matters for scoring.

Separately (#302, CodeRabbit Major deferred off PR #101): `save_fingerprint` and
`save_embedding` are two independent statements. An embedding-save failure is
warn-only and leaves a fresh fingerprint JSON beside a stale/absent embedding
for up to 14 days. Generations can mix.

## Decisions taken (with Bryan, 2026-08-21)

1. **Defer #298** (bge-small-en-v1.5 swap). Clustering ships on the existing
   all-MiniLM-L6-v2 so only one variable changes at a time; a model swap would
   invalidate every stored embedding and add a third in-flight overlap
   distribution shift.
2. **Shadow compare** for recalibration. The new max-over-topics overlap is the
   live score; the old mean-centroid overlap is stored beside it in the DB.
   Gate/tier thresholds stay numerically unchanged at ship time; #135
   recalibrates later from the recorded pairs, after distributions converge.
3. **Approach: embedding-first** ("BERTopic-lite"). Topics are defined by
   clustering in embedding space; TF-IDF keywords become per-cluster labels.
   Rejected: minimal bolt-on (keeps the parallel-universe defect), and
   keyword-clusters-as-topics (semantic quality capped by keyword matching).
4. **Candidate side unchanged** in this build. Candidates keep one centroid;
   the residual candidate-side dilution (max-over-candidate-posts) is filed as
   **#304**, out of scope here.

## 1. Fingerprint build (embedding-first)

`build_user_fingerprint` (src/web/scan_job.rs:549) becomes the single build
path; the hand-duplicated CLI copy in `src/main.rs:235-288` is deduplicated to
call it. Admin pre-seed already delegates.

New flow:

1. Fetch ≤500 posts; `clean_for_embedding` each; embed each post; L2-normalize
   per post (existing code, `src/topics/embeddings.rs`).
2. **Cluster** the post vectors: greedy agglomerative with centroid linkage
   over cosine. Every post starts as its own cluster; repeatedly merge the pair of
   clusters with the highest centroid cosine; stop when the best pair falls
   below the **merge threshold** or the count reaches the **cluster cap**.
   Deterministic — no RNG, no seed; same input → same clusters.
3. **Prune noise**: clusters with fewer than `max(2, ceil(0.02 × posts))`
   members are dropped from scoring (their posts still contribute to the mean
   centroid). Survivors are the K topic centroids; each centroid is the
   normalized mean of its member vectors; `weight` = member share of all
   clustered posts, normalized to sum 1.0 across survivors.
4. **Label** each surviving cluster by running the existing TF-IDF extraction
   (`TfIdfExtractor`) over only that cluster's posts; top ≤6 keywords populate
   `TopicCluster { label, keywords, keyword_scores, weight }` — the JSON shape
   is unchanged, so serde compatibility holds. Keyword ranking is by score, so
   the seed-first positional assumption in discovery still holds.
5. The **single normalized-mean centroid is still computed and stored** exactly
   as today (shadow baseline + legacy degrade path).
6. Everything persists via one atomic `save_fingerprint_bundle` (§3).

**Parameters** (fields on the extractor/builder config, not buried constants):

| Parameter | Default | Status |
|---|---|---|
| `cluster_merge_threshold` | 0.60 cosine | provisional — validated against Bryan's real history before merge |
| `max_topic_clusters` | 12 | provisional, same validation |
| min cluster size | `max(2, 2% of posts)` | provisional, same validation |

Degradation: if the embedder is unavailable, or all posts clean to empty (the
#296 empty-batch guard), build the pure TF-IDF keyword fingerprint as today —
saved as a bundle with no embedding and no cluster rows.

Complexity note: agglomerative at n=500 is O(n²·K) with 384-dim cosines —
sub-second on the build path, which runs once per rebuild, not per scan.
Scan-time cost is unchanged (candidates still get one centroid).

## 2. Scoring, shadow compare, rebuild trigger

**Overlap semantics (Stage 2)**: `overlap = max_i cosine(candidate_centroid,
topic_i)` over the protected user's topic centroids. Pure max — **not**
weight-multiplied (a 5%-of-posts topic is still fully the user's topic; weight
already gated noise via pruning). Negative cosines are preserved (opposition
signal, per #296).

Unchanged:
- Stage 1 keyword-scale gate and `keyword_gate_threshold` (0.05) — never used
  embeddings (src/scoring/profile.rs:255-418).
- Stage 2 three-way fallback chain (precomputed candidate centroid →
  embed-at-finalize → keyword cosine with `overlap_is_keyword_scale`)
  (profile.rs:589-644).
- `overlap_gate_threshold` 0.15 numeric value (recalibration deferred to #135).
- Candidate centroid computation everywhere (gather.rs, staging.rs blob,
  finalize.rs).

**Shadow compare**: wherever the new overlap is computed from topic centroids,
also compute legacy overlap = cosine vs the stored mean centroid, and persist
it to a new nullable `account_scores.overlap_legacy` column. DB, not logs —
scan auditing here is DB-based. Null when no embedding path was available.
The live score, gates, and tiers use only the new overlap.

**Legacy format & rebuild trigger**: a fingerprint with zero `topic_clusters`
rows is legacy-format. `fingerprint_is_stale` gains a companion check: absent
OR stale OR legacy-format triggers `build_user_fingerprint` on the next scan
(scan_job.rs:797-837), with the existing fall-back-to-stale-data behavior on
rebuild failure. Until rebuilt, a legacy user's overlap degrades to the
single-centroid cosine (max over the one-element set {mean centroid}) —
numerically identical to pre-#297 behavior, so nothing breaks mid-fleet.
Deploy effect mirrors the #296 rollout: one amortized 500-post rebuild per
protected user on first scan.

## 3. Persistence & schema (#302)

### New table `topic_clusters`

| Column | Postgres | SQLite |
|---|---|---|
| `user_did` | TEXT, FK → `topic_fingerprint(user_did)` ON DELETE CASCADE | TEXT |
| `cluster_index` | INTEGER (0-based; row i ↔ JSON `clusters[i]`) | INTEGER |
| `centroid` | `vector(384)` (f32, pgvector) | TEXT (JSON array of f64) |
| `post_count` | INTEGER | INTEGER |
| PK | `(user_did, cluster_index)` | same |

Only surviving (post-prune) clusters get rows; `cluster_index` counts them in
JSON order. Label/keywords/weight stay in the fingerprint JSON.

### `save_fingerprint_bundle` (new trait method)

The `Database` trait exposes no transaction handle by design; atomicity is
expressed as coarse internally-transactional methods (precedent:
`delete_user_data`, queries.rs:937 "one transaction for the whole sequence").

```rust
async fn save_fingerprint_bundle(
    &self,
    user_did: &str,
    fingerprint_json: &str,
    post_count: u32,
    embedding: Option<&[f64]>,
    clusters: &[ClusterCentroid],   // { centroid: Vec<f64>, post_count: u32 }
) -> Result<()>;
```

Both backends, one transaction: upsert the `topic_fingerprint` row (JSON,
post_count, `embedding_vector` in the same statement — no separate
order-dependent UPDATE), DELETE the user's old `topic_clusters` rows, INSERT
the new ones, `updated_at` bumped exactly once by the upsert, preserving the
`YYYY-MM-DD HH:MM:SS` text contract `fingerprint_is_stale` parses. Postgres via
`self.pool.begin()`; SQLite via `unchecked_transaction()` (both existing
patterns). Centroids and embedding validated finite before save; `embedding:
None` with empty `clusters` is the legal keyword-only bundle.

### Trait/reader changes

- New `get_topic_centroids(&self, user_did) -> Result<Vec<ClusterCentroid>>`,
  ordered by `cluster_index`. Empty ⇒ legacy format.
- `save_fingerprint` / `save_embedding` **remain** on the trait (used by
  `charcoal migrate` and tests) but all build paths use the bundle.
- `charcoal migrate` (SQLite→Postgres transfer) learns to copy `topic_clusters`
  rows.
- `delete_user_data`: Postgres relies on the FK cascade; SQLite adds an
  explicit DELETE inside its existing transaction.
- Defensive read check: if centroid row count ≠ JSON cluster count (possible
  only for DBs written by a pre-#302 binary), treat as legacy format → rebuild.

### Migration 0013 (both backends)

One migration: create `topic_clusters`; add nullable
`account_scores.overlap_legacy REAL` (`DOUBLE PRECISION` on PG). Postgres file
`0013_topic_clusters.sql` **self-records** `INSERT INTO schema_version (version)
VALUES (13) ON CONFLICT DO NOTHING;` and is registered in the `include_str!`
list (postgres.rs:119-163). SQLite migration v13 in schema.rs wraps its
multiple statements in explicit `BEGIN;`/`COMMIT;`. The sqlite.rs expected-
tables assertion (~:626) is updated.

## 4. Discovery

No code change to `extract_search_keywords`
(src/discovery/topic_search.rs:20-40): clusters are still sorted by weight and
keywords are still ranked-first, so it now yields one search keyword per
*semantic topic* automatically. Existing discovery tests verify the contract.

## 5. Error handling summary

| Failure | Behavior |
|---|---|
| Embedder load fails | keyword-only bundle (atomic), warn — same UX as today |
| All posts clean to empty | same as above (#296 guard) |
| 1 post / identical posts | single cluster; max-over-one ≡ old behavior |
| Non-finite centroid/embedding values | reject before save (like #296 keyword-score validation) |
| Bundle transaction fails mid-way | rollback; previous complete generation intact |
| Cluster rows ≠ JSON clusters | treated as legacy → rebuild next scan |
| Rebuild fails on stale/legacy | fall back to existing stored fingerprint (existing behavior) |

## 6. Testing (TDD — tests first)

- **Clustering unit** (new module, synthetic 384-dim vectors): determinism;
  merges above threshold, stops below; cluster cap respected; noise pruning;
  weights sum to 1.0; single-post; empty input errors.
- **Coarseness-fix proof**: fixture user with two well-separated synthetic
  topics; candidate near topic B only. Assert max-over-topics is high where
  mean-centroid cosine is low. This is the reason-for-existence test.
- **Overlap semantics**: max over set; negative cosine preserved; empty
  centroid set degrades to mean-centroid cosine.
- **Per-cluster labeling**: keywords derive from each cluster's own posts;
  serde roundtrip of `TopicFingerprint` with legacy JSON (no clusters table).
- **Persistence**: bundle roundtrip SQLite (inline) + Postgres
  (tests/db_postgres.rs — run against local `charcoal_test`; these silently
  no-op without `DATABASE_URL`, so the check is only valid on a live DB);
  atomic rollback leaves nothing behind; keyword-only bundle; delete_user_data
  clears rows; migrate transfers rows; migration 0013 creates table + records
  version 13.
- **Scan integration**: legacy-format triggers rebuild; `overlap_legacy`
  written to `account_scores`; golden/composition tests updated where overlap
  values shift; sqlite expected-tables assertion.
- **Discovery**: per-topic keyword extraction contract.
- **Full model-gated run**: `CHARCOAL_MODEL_DIR=./models cargo test --features
  web -- --show-output` with the exact `grep -E "^\s*SKIP:"` showing zero.
- **Real-data validation** (manual, pre-merge): rebuild Bryan's fingerprint
  locally via `charcoal fingerprint`; clusters should recover recognizable
  topics (fat liberation, a cappella, Atlassian, …). Tune the three
  provisional parameters here if clusters degenerate (all-in-one or shattered).

## Out of scope

- #298 bge-small eval/swap (deferred; clean follow-up once clustering lands)
- #299 adversarial anchors (BEST tier; gated on #259 privacy)
- #304 candidate-side max-over-posts overlap (filed this session)
- #135 threshold recalibration (waits for distribution convergence; consumes
  the `overlap_legacy` shadow data this build records)
- Fingerprint viewer UI (#131/#132) — the per-topic labels feed it later
