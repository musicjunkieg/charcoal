# Multi-Interest Topic Fingerprint (#297 + #302) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cluster the protected user's post embeddings into K weighted topic centroids, make overlap = max-over-topics, and persist fingerprint JSON + embedding + cluster rows in one transaction per backend.

**Architecture:** Embedding-first ("BERTopic-lite"): greedy agglomerative clustering with centroid linkage over cosine defines topics; per-cluster TF-IDF supplies human-readable labels. The single normalized-mean centroid is still stored as the shadow-compare baseline (`account_scores.overlap_legacy`) and the legacy degrade path. Persistence goes through one new internally-transactional trait method, `save_fingerprint_bundle` (#302).

**Tech Stack:** Rust, rusqlite (SQLite), sqlx-core/sqlx-postgres + pgvector (Postgres), ort/MiniLM embeddings (existing), chrono.

**Spec:** `docs/superpowers/specs/2026-08-21-297-multi-interest-fingerprint-design.md` — read it before starting any task.

## Global Constraints

- Branch: `feat/297-multi-interest-fingerprint` (exists). NEVER commit to `staging`/`main`. Push after each task commit.
- Rust style: `?` propagation, `anyhow::Result`, no `.unwrap()` outside tests, comments explain *why*.
- Git: stage files EXPLICITLY by name (never `git add -A`/`.`); NEVER use heredoc (`<<EOF`) in any shell command — single-quoted multi-line strings only.
- Tests: run with `CHARCOAL_MODEL_DIR=./models` and `--features web` where noted; model-gated tests silently pass without it. Bash tool calls that run `cargo test` MUST set an explicit `timeout` of 600000 ms (the 120 s default auto-backgrounds and stalls subagents).
- Postgres tests: `DATABASE_URL=postgres://$USER@localhost/charcoal_test` (role is your OS user, not `charcoal`).
- Postgres migrations MUST self-record: `INSERT INTO schema_version (version) VALUES (13) ON CONFLICT DO NOTHING;`
- `updated_at` text contract: both backends emit `YYYY-MM-DD HH:MM:SS` (UTC); `fingerprint_is_stale` parses exactly this.
- Provisional parameters (validated in Task 10, do not hardcode elsewhere): `merge_threshold = 0.60`, `max_clusters = 12`, min cluster size `max(2, ceil(0.02 × n))`.
- Deciduous: log an `action` node (with `--commit HEAD`) after each task's commit and link it to goal node 503's chain (`deciduous link 515 <new> -r "..."` chain-style).

---

### Task 1: Clustering module (`src/topics/clustering.rs`)

**Files:**
- Create: `src/topics/clustering.rs`
- Modify: `src/topics/mod.rs` (add `pub mod clustering;`)
- Test: inline `#[cfg(test)]` in `src/topics/clustering.rs`

**Interfaces:**
- Consumes: `crate::topics::embeddings::cosine_similarity_embeddings(a: &[f64], b: &[f64]) -> f64`, `EMBEDDING_DIM: usize = 384`.
- Produces (used by Tasks 3, 8):
  - `pub struct PostCluster { pub centroid: Vec<f64>, pub members: Vec<usize>, pub weight: f64 }`
  - `pub struct ClusteringParams { pub merge_threshold: f64, pub max_clusters: usize }` with `Default` = `{ 0.60, 12 }`
  - `pub fn cluster_embeddings(embeddings: &[Vec<f64>], params: &ClusteringParams) -> Vec<PostCluster>`
  - Guarantees: deterministic; zero-vector inputs skipped; surviving clusters pruned to size ≥ `max(2, (0.02 * n).ceil())` (n = non-zero inputs) unless that would leave zero clusters, in which case the largest cluster survives; `members` are indices into the ORIGINAL `embeddings` slice; weights over survivors sum to 1.0; clusters sorted by weight descending, ties by smallest first member index; centroids are L2-normalized.

- [ ] **Step 1: Write the failing tests**

Create `src/topics/clustering.rs` with the module doc, empty stubs NOT included — tests first. Put this at the bottom of the new file (the top will hold the implementation in Step 3; for this step, create the file with only a doc comment and the test module, referring to not-yet-written items so it fails to compile — that is the expected failure):

```rust
//! Greedy agglomerative clustering of post embeddings (centroid linkage over
//! cosine). Deterministic by construction — no RNG — so fingerprints are
//! reproducible and the tests can assert exact structure. (#297)

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a unit vector with 1.0 at `axis`, plus `noise` at `axis + 1`.
    /// Two vectors sharing an axis have cosine ≈ 1; orthogonal axes ≈ 0.
    fn vec_on_axis(axis: usize, noise: f64) -> Vec<f64> {
        let mut v = vec![0.0; crate::topics::embeddings::EMBEDDING_DIM];
        v[axis] = 1.0;
        v[axis + 1] = noise;
        v
    }

    #[test]
    fn two_well_separated_topics_form_two_clusters() {
        // 5 posts near axis 0, 4 posts near axis 100 — far below any sane
        // merge threshold across groups, far above it within groups.
        let mut embs: Vec<Vec<f64>> = (0..5).map(|i| vec_on_axis(0, i as f64 * 0.01)).collect();
        embs.extend((0..4).map(|i| vec_on_axis(100, i as f64 * 0.01)));
        let clusters = cluster_embeddings(&embs, &ClusteringParams::default());
        assert_eq!(clusters.len(), 2);
        // Sorted by weight desc: 5-member cluster first.
        assert_eq!(clusters[0].members.len(), 5);
        assert_eq!(clusters[1].members.len(), 4);
        assert!((clusters[0].weight - 5.0 / 9.0).abs() < 1e-9);
        assert!((clusters[1].weight - 4.0 / 9.0).abs() < 1e-9);
    }

    #[test]
    fn deterministic_same_input_same_output() {
        let mut embs: Vec<Vec<f64>> = (0..6).map(|i| vec_on_axis(0, i as f64 * 0.02)).collect();
        embs.extend((0..6).map(|i| vec_on_axis(50, i as f64 * 0.02)));
        let a = cluster_embeddings(&embs, &ClusteringParams::default());
        let b = cluster_embeddings(&embs, &ClusteringParams::default());
        assert_eq!(a.len(), b.len());
        for (ca, cb) in a.iter().zip(b.iter()) {
            assert_eq!(ca.members, cb.members);
            assert_eq!(ca.centroid, cb.centroid);
        }
    }

    #[test]
    fn respects_max_clusters_cap() {
        // 20 mutually orthogonal singleton groups of 2 (axes 0,20,40,…) would
        // naturally stay 20 clusters; the cap forces merges down to 3.
        let mut embs = Vec::new();
        for g in 0..20 {
            embs.push(vec_on_axis(g * 19, 0.0));
            embs.push(vec_on_axis(g * 19, 0.001));
        }
        let params = ClusteringParams { merge_threshold: 0.60, max_clusters: 3 };
        let clusters = cluster_embeddings(&embs, &params);
        assert!(clusters.len() <= 3, "got {} clusters", clusters.len());
    }

    #[test]
    fn small_clusters_are_pruned_and_weights_renormalized() {
        // 10 posts on axis 0, 1 lone post on axis 100. min size = max(2, ceil(0.22)) = 2,
        // so the singleton is pruned and the big cluster's weight renormalizes to 1.0.
        let mut embs: Vec<Vec<f64>> = (0..10).map(|i| vec_on_axis(0, i as f64 * 0.01)).collect();
        embs.push(vec_on_axis(100, 0.0));
        let clusters = cluster_embeddings(&embs, &ClusteringParams::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].members.len(), 10);
        assert!((clusters[0].weight - 1.0).abs() < 1e-9);
    }

    #[test]
    fn single_post_yields_single_cluster() {
        // One post: pruning would leave zero clusters, so the largest survives.
        let embs = vec![vec_on_axis(0, 0.0)];
        let clusters = cluster_embeddings(&embs, &ClusteringParams::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].members, vec![0]);
        assert!((clusters[0].weight - 1.0).abs() < 1e-9);
    }

    #[test]
    fn zero_vectors_are_skipped() {
        let embs = vec![
            vec![0.0; crate::topics::embeddings::EMBEDDING_DIM], // zero — skipped
            vec_on_axis(0, 0.0),
            vec_on_axis(0, 0.01),
        ];
        let clusters = cluster_embeddings(&embs, &ClusteringParams::default());
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0].members, vec![1, 2]); // original indices preserved
    }

    #[test]
    fn empty_input_yields_no_clusters() {
        let clusters = cluster_embeddings(&[], &ClusteringParams::default());
        assert!(clusters.is_empty());
    }

    #[test]
    fn centroids_are_l2_normalized() {
        let embs: Vec<Vec<f64>> = (0..4).map(|i| vec_on_axis(0, i as f64 * 0.05)).collect();
        let clusters = cluster_embeddings(&embs, &ClusteringParams::default());
        let norm: f64 = clusters[0].centroid.iter().map(|v| v * v).sum::<f64>().sqrt();
        assert!((norm - 1.0).abs() < 1e-9);
    }
}
```

Add `pub mod clustering;` to `src/topics/mod.rs` (read it first; place alphabetically among the existing `pub mod` lines).

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib topics::clustering 2>&1 | tail -20` (timeout 600000)
Expected: COMPILE ERROR — `cluster_embeddings`, `ClusteringParams` not found. That is the failing state.

- [ ] **Step 3: Write the implementation**

Above the test module in `src/topics/clustering.rs`:

```rust
use crate::topics::embeddings::cosine_similarity_embeddings;

/// One semantic topic: the normalized mean of its member post vectors.
#[derive(Debug, Clone)]
pub struct PostCluster {
    /// L2-normalized centroid of the member vectors.
    pub centroid: Vec<f64>,
    /// Indices into the original `embeddings` slice passed to
    /// `cluster_embeddings` — callers use these to find each cluster's posts
    /// for TF-IDF labeling.
    pub members: Vec<usize>,
    /// Share of clustered (non-zero, surviving) posts. Sums to 1.0.
    pub weight: f64,
}

/// Tuning knobs for the greedy agglomerative pass. Provisional defaults per
/// the #297 spec — validated against Bryan's real posting history before
/// merge, revisited by #135 recalibration.
#[derive(Debug, Clone)]
pub struct ClusteringParams {
    /// Stop merging when the best remaining pair's centroid cosine falls
    /// below this (unless still over `max_clusters`).
    pub merge_threshold: f64,
    /// Hard cap on surviving clusters; forces merges below the threshold.
    pub max_clusters: usize,
}

impl Default for ClusteringParams {
    fn default() -> Self {
        Self { merge_threshold: 0.60, max_clusters: 12 }
    }
}

/// Greedy agglomerative clustering, centroid linkage over cosine.
///
/// Deterministic: pairs are compared with a fixed tie-break (lowest index
/// pair wins), so the same input always produces the same clusters — no RNG,
/// which keeps fingerprints reproducible and tests exact. O(n² · K) with
/// n ≤ 500 on the build path only; scan-time cost is untouched.
pub fn cluster_embeddings(embeddings: &[Vec<f64>], params: &ClusteringParams) -> Vec<PostCluster> {
    // Working state: each cluster = (member indices, unnormalized sum of
    // NORMALIZED member vectors). Normalizing members first means the
    // centroid direction reflects what posts say, not how long they are —
    // the same reasoning as normalized_mean_embedding (#296).
    let mut clusters: Vec<(Vec<usize>, Vec<f64>)> = Vec::new();
    for (i, emb) in embeddings.iter().enumerate() {
        let norm: f64 = emb.iter().map(|v| v * v).sum::<f64>().sqrt();
        if norm < f64::EPSILON {
            continue; // zero vector (empty text) — same skip as the mean path
        }
        clusters.push((vec![i], emb.iter().map(|v| v / norm).collect()));
    }
    if clusters.is_empty() {
        return Vec::new();
    }
    let n = clusters.len();

    // Merge loop: find the best pair by centroid cosine; merge while the best
    // is at/above threshold, or unconditionally while over the cap.
    loop {
        if clusters.len() <= 1 {
            break;
        }
        let mut best: Option<(usize, usize, f64)> = None;
        for a in 0..clusters.len() {
            for b in (a + 1)..clusters.len() {
                let sim = cosine_similarity_embeddings(&clusters[a].1, &clusters[b].1);
                // Strict > keeps the FIRST (lowest-index) best pair on ties —
                // the determinism guarantee.
                if best.map(|(_, _, s)| sim > s).unwrap_or(true) {
                    best = Some((a, b, sim));
                }
            }
        }
        let Some((a, b, sim)) = best else { break };
        let over_cap = clusters.len() > params.max_clusters;
        if sim < params.merge_threshold && !over_cap {
            break;
        }
        // Merge b into a: append members, sum the normalized vectors.
        let (b_members, b_sum) = clusters.swap_remove(b);
        clusters[a].0.extend(b_members);
        for (i, v) in b_sum.iter().enumerate() {
            clusters[a].1[i] += v;
        }
    }

    // Prune noise clusters. If pruning would delete everything (e.g. a
    // 1-post corpus), keep the largest so a valid fingerprint always has at
    // least one topic.
    let min_size = 2usize.max((0.02 * n as f64).ceil() as usize);
    let max_size = clusters.iter().map(|(m, _)| m.len()).max().unwrap_or(0);
    let effective_min = if max_size < min_size { max_size } else { min_size };
    clusters.retain(|(members, _)| members.len() >= effective_min);

    let total: f64 = clusters.iter().map(|(m, _)| m.len() as f64).sum();
    let mut out: Vec<PostCluster> = clusters
        .into_iter()
        .map(|(mut members, sum)| {
            members.sort_unstable();
            let norm: f64 = sum.iter().map(|v| v * v).sum::<f64>().sqrt();
            let centroid = if norm < f64::EPSILON {
                sum
            } else {
                sum.iter().map(|v| v / norm).collect()
            };
            let weight = members.len() as f64 / total;
            PostCluster { centroid, members, weight }
        })
        .collect();

    // Weight descending; ties broken by first member index for determinism.
    out.sort_by(|x, y| {
        y.weight
            .partial_cmp(&x.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| x.members[0].cmp(&y.members[0]))
    });
    out
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib topics::clustering 2>&1 | tail -20` (timeout 600000)
Expected: `test result: ok. 8 passed`

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy --features web --all-targets 2>&1 | tail -5` — fix any warnings in the new file.

```bash
git add src/topics/clustering.rs src/topics/mod.rs
git commit -m 'feat(297): deterministic greedy agglomerative clustering of post embeddings'
git push -u origin feat/297-multi-interest-fingerprint
```

---

### Task 2: Max-over-topics overlap (`src/topics/embeddings.rs`)

**Files:**
- Modify: `src/topics/embeddings.rs` (add function after `cosine_similarity_embeddings`, ~line 280)
- Test: inline in `src/topics/embeddings.rs`'s existing `#[cfg(test)]` module (find it at the bottom of the file; if none exists, add one)

**Interfaces:**
- Consumes: `cosine_similarity_embeddings` (same file).
- Produces (used by Task 9):
  - `pub fn max_topic_overlap(topic_centroids: &[Vec<f64>], candidate: &[f64]) -> Option<f64>` — `None` when `topic_centroids` is empty; otherwise the maximum cosine (sign preserved, range [-1, 1]).

- [ ] **Step 1: Write the failing tests**

Add to the test module in `src/topics/embeddings.rs`:

```rust
#[test]
fn max_topic_overlap_takes_the_best_topic() {
    let mut topic_a = vec![0.0; EMBEDDING_DIM];
    topic_a[0] = 1.0;
    let mut topic_b = vec![0.0; EMBEDDING_DIM];
    topic_b[100] = 1.0;
    let mut candidate = vec![0.0; EMBEDDING_DIM];
    candidate[100] = 1.0; // exactly topic B
    let overlap = max_topic_overlap(&[topic_a, topic_b], &candidate).unwrap();
    assert!((overlap - 1.0).abs() < 1e-9);
}

#[test]
fn max_topic_overlap_beats_smeared_mean_for_niche_topic() {
    // The coarseness fix, demonstrated (#297's reason to exist): a candidate
    // sitting exactly on ONE of two orthogonal topics scores ~1.0 against
    // max-over-topics but only ~0.71 against the smeared mean centroid.
    let mut topic_a = vec![0.0; EMBEDDING_DIM];
    topic_a[0] = 1.0;
    let mut topic_b = vec![0.0; EMBEDDING_DIM];
    topic_b[100] = 1.0;
    let mut candidate = vec![0.0; EMBEDDING_DIM];
    candidate[100] = 1.0;

    let mean = normalized_mean_embedding(&[topic_a.clone(), topic_b.clone()]);
    let legacy = cosine_similarity_embeddings(&mean, &candidate);
    let multi = max_topic_overlap(&[topic_a, topic_b], &candidate).unwrap();
    assert!(multi > legacy + 0.2, "multi {multi} should beat legacy {legacy}");
}

#[test]
fn max_topic_overlap_preserves_negative_sign() {
    // Opposition is signal (#296 defect 5): a candidate pointing AWAY from
    // every topic must stay negative, not clamp to zero.
    let mut topic = vec![0.0; EMBEDDING_DIM];
    topic[0] = 1.0;
    let mut candidate = vec![0.0; EMBEDDING_DIM];
    candidate[0] = -1.0;
    let overlap = max_topic_overlap(&[topic], &candidate).unwrap();
    assert!((overlap - (-1.0)).abs() < 1e-9);
}

#[test]
fn max_topic_overlap_empty_topics_is_none() {
    let candidate = vec![1.0; EMBEDDING_DIM];
    assert!(max_topic_overlap(&[], &candidate).is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib max_topic_overlap 2>&1 | tail -10` (timeout 600000)
Expected: COMPILE ERROR — `max_topic_overlap` not found.

- [ ] **Step 3: Implement**

After `cosine_similarity_embeddings` in `src/topics/embeddings.rs`:

```rust
/// Overlap between a candidate centroid and a SET of protected topic
/// centroids: the best (maximum) cosine across topics. Answers "is this
/// account near ANY of my topics?" — the actual threat question — instead of
/// "is it near my average?" (#297, spike #295 defect 1).
///
/// Pure max, deliberately NOT weighted by topic weight: a topic that is 5%
/// of someone's posting is still fully theirs; noise clusters were already
/// pruned at build time. Sign is preserved (opposition signal, #296).
/// Returns `None` for an empty topic set so callers can degrade to the
/// legacy mean-centroid path (pre-#297 fingerprints).
pub fn max_topic_overlap(topic_centroids: &[Vec<f64>], candidate: &[f64]) -> Option<f64> {
    topic_centroids
        .iter()
        .map(|topic| cosine_similarity_embeddings(topic, candidate))
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib max_topic_overlap 2>&1 | tail -10` (timeout 600000)
Expected: `4 passed`

- [ ] **Step 5: Commit**

```bash
git add src/topics/embeddings.rs
git commit -m 'feat(297): max-over-topics overlap with legacy degrade to None'
git push
```

---

### Task 3: Embedding-first fingerprint assembly (`src/topics/clustering.rs`)

**Files:**
- Modify: `src/topics/clustering.rs` (add function + tests)

**Interfaces:**
- Consumes: `cluster_embeddings`, `ClusteringParams`, `PostCluster` (Task 1); `TfIdfExtractor { top_n_keywords, max_clusters }` + `TopicExtractor::extract(&self, posts: &[String]) -> Result<TopicFingerprint>` (`src/topics/tfidf.rs:34-47`, `src/topics/traits.rs:13`); `TopicFingerprint { clusters, post_count }`, `TopicCluster { label, keywords, keyword_scores, weight }` (`src/topics/fingerprint.rs:12-34`).
- Produces (used by Task 8):
  - `pub fn build_clustered_fingerprint(post_texts: &[String], embeddings: &[Vec<f64>], total_post_count: u32, params: &ClusteringParams) -> anyhow::Result<(TopicFingerprint, Vec<PostCluster>)>`
  - Guarantees: `post_texts` and `embeddings` are parallel slices (same length — caller aligns them; error if not); the returned `TopicFingerprint.clusters[i]` corresponds 1:1 (same order) with the returned `Vec<PostCluster>[i]`; every centroid is finite; labels come from per-cluster TF-IDF with fallback label `topic N` and empty keywords when extraction fails on a tiny cluster.

- [ ] **Step 1: Write the failing tests**

Append to the test module in `src/topics/clustering.rs`:

```rust
#[test]
fn clustered_fingerprint_labels_each_topic_from_its_own_posts() {
    // Two topic groups with distinctive vocabulary. Embeddings are synthetic
    // (axis-separated); TF-IDF labeling runs on the real texts.
    let texts: Vec<String> = vec![
        "fat liberation is about body autonomy and fat acceptance".into(),
        "fat acceptance and body liberation communities organizing".into(),
        "body autonomy fat liberation acceptance politics".into(),
        "choral rehearsal techniques for a cappella arrangements".into(),
        "arranging a cappella voicings for choral rehearsal".into(),
        "rehearsal warmups for a cappella choral singers".into(),
    ];
    let mut embs: Vec<Vec<f64>> = (0..3).map(|i| {
        let mut v = vec![0.0; crate::topics::embeddings::EMBEDDING_DIM];
        v[0] = 1.0; v[1] = i as f64 * 0.01; v
    }).collect();
    embs.extend((0..3).map(|i| {
        let mut v = vec![0.0; crate::topics::embeddings::EMBEDDING_DIM];
        v[100] = 1.0; v[101] = i as f64 * 0.01; v
    }));

    let (fp, clusters) =
        build_clustered_fingerprint(&texts, &embs, 6, &ClusteringParams::default()).unwrap();
    assert_eq!(fp.clusters.len(), 2);
    assert_eq!(clusters.len(), 2);
    assert_eq!(fp.post_count, 6);
    // JSON cluster i ↔ PostCluster i, weights match.
    for (jc, pc) in fp.clusters.iter().zip(clusters.iter()) {
        assert!((jc.weight - pc.weight).abs() < 1e-9);
        assert!(!jc.label.is_empty());
    }
    // Each topic's keywords come from ITS posts, not the other topic's.
    let all_kw_0 = fp.clusters[0].keywords.join(" ");
    let all_kw_1 = fp.clusters[1].keywords.join(" ");
    assert_ne!(all_kw_0, all_kw_1);
}

#[test]
fn clustered_fingerprint_rejects_mismatched_slices() {
    let texts = vec!["one post".to_string()];
    let embs: Vec<Vec<f64>> = Vec::new();
    assert!(build_clustered_fingerprint(&texts, &embs, 1, &ClusteringParams::default()).is_err());
}

#[test]
fn clustered_fingerprint_survives_label_extraction_failure() {
    // A cluster whose posts are stop-words-only makes TF-IDF extraction bail;
    // the cluster keeps a fallback label instead of killing the build.
    let texts: Vec<String> = vec![
        "the and but or so".into(),
        "and the or but so".into(),
        "or so and the but".into(),
    ];
    let embs: Vec<Vec<f64>> = (0..3).map(|i| {
        let mut v = vec![0.0; crate::topics::embeddings::EMBEDDING_DIM];
        v[0] = 1.0; v[1] = i as f64 * 0.01; v
    }).collect();
    let (fp, clusters) =
        build_clustered_fingerprint(&texts, &embs, 3, &ClusteringParams::default()).unwrap();
    assert_eq!(clusters.len(), 1);
    assert_eq!(fp.clusters[0].label, "topic 1");
    assert!(fp.clusters[0].keywords.is_empty());
    assert!(fp.clusters[0].keyword_scores.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib clustered_fingerprint 2>&1 | tail -10` (timeout 600000)
Expected: COMPILE ERROR — `build_clustered_fingerprint` not found.

- [ ] **Step 3: Implement**

Add above the test module in `src/topics/clustering.rs`:

```rust
use crate::topics::fingerprint::{TopicCluster, TopicFingerprint};
use crate::topics::tfidf::TfIdfExtractor;
use crate::topics::traits::TopicExtractor;

/// Assemble a multi-interest fingerprint: cluster the post embeddings, then
/// label each cluster by running TF-IDF over only that cluster's own posts
/// (BERTopic-lite). JSON cluster i corresponds 1:1 with returned PostCluster
/// i — the persistence layer relies on that ordering (#297/#302).
pub fn build_clustered_fingerprint(
    post_texts: &[String],
    embeddings: &[Vec<f64>],
    total_post_count: u32,
    params: &ClusteringParams,
) -> anyhow::Result<(TopicFingerprint, Vec<PostCluster>)> {
    anyhow::ensure!(
        post_texts.len() == embeddings.len(),
        "post_texts ({}) and embeddings ({}) must be parallel slices",
        post_texts.len(),
        embeddings.len(),
    );

    let clusters = cluster_embeddings(embeddings, params);

    // One TF-IDF pass per cluster, over that cluster's posts only. A single
    // output cluster per pass: we want ranked keywords for a label, not
    // sub-clustering. Small keyword budget — labels, not a parallel universe.
    let labeler = TfIdfExtractor { top_n_keywords: 12, max_clusters: 1 };

    let mut json_clusters = Vec::with_capacity(clusters.len());
    for (i, cluster) in clusters.iter().enumerate() {
        let member_texts: Vec<String> = cluster
            .members
            .iter()
            .map(|&m| post_texts[m].clone())
            .collect();
        // Tiny or stop-word-only clusters can make TF-IDF bail; a fingerprint
        // with an unlabeled topic beats no fingerprint, so degrade per-cluster.
        let (label, keywords, keyword_scores) = match labeler.extract(&member_texts) {
            Ok(fp) if !fp.clusters.is_empty() => {
                let c = &fp.clusters[0];
                (c.label.clone(), c.keywords.clone(), c.keyword_scores.clone())
            }
            _ => (format!("topic {}", i + 1), Vec::new(), Vec::new()),
        };
        json_clusters.push(TopicCluster {
            label,
            keywords,
            keyword_scores,
            weight: cluster.weight,
        });
    }

    // Finite-value guard, same discipline as #296's keyword-score validation:
    // a NaN centroid would poison every future cosine against this user.
    for cluster in &clusters {
        anyhow::ensure!(
            cluster.centroid.iter().all(|v| v.is_finite()),
            "non-finite value in cluster centroid",
        );
    }

    Ok((
        TopicFingerprint { clusters: json_clusters, post_count: total_post_count },
        clusters,
    ))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib clustered_fingerprint 2>&1 | tail -10` (timeout 600000)
Expected: `3 passed`

- [ ] **Step 5: Commit**

```bash
git add src/topics/clustering.rs
git commit -m 'feat(297): assemble multi-interest fingerprint with per-cluster TF-IDF labels'
git push
```

---

### Task 4: Migration 0013, both backends

**Files:**
- Create: `migrations/postgres/0013_topic_clusters.sql`
- Modify: `src/db/schema.rs` (append v13 after the v12 block ending near line 430)
- Modify: `src/db/postgres.rs:157-165` (registry: add the 0013 tuple after 0012)
- Modify: `src/db/sqlite.rs:623-631` (`test_trait_table_count`: 12 → 13, update comment)
- Test: inline in `src/db/schema.rs` tests (if present) / `src/db/sqlite.rs`

**Interfaces:**
- Produces: table `topic_clusters (user_did, cluster_index, centroid, post_count)` PK `(user_did, cluster_index)`; column `account_scores.overlap_legacy` (REAL / DOUBLE PRECISION, nullable). Tasks 5, 6, 9 depend on these exact names.

- [ ] **Step 1: Write the failing SQLite test**

In `src/db/sqlite.rs`, first update `test_trait_table_count` (~line 623): change `assert_eq!(count, 12);` to `assert_eq!(count, 13);` and extend the comment's table list with `topic_clusters` and `= 13 tables (v13)`. Then add alongside it:

```rust
#[tokio::test]
async fn test_migration_v13_creates_topic_clusters_and_overlap_legacy() {
    let db = test_db().await;
    let conn = db.conn.lock().await;
    let has_table: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='topic_clusters'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(has_table, "topic_clusters table missing");
    let has_col: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('account_scores') WHERE name='overlap_legacy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(has_col, "account_scores.overlap_legacy missing");
}
```

(Check how sibling tests access the connection — if `db.conn` is private to the module the test lives in the same file, so direct field access works; mirror whatever `test_trait_table_count` does.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web test_trait_table_count test_migration_v13 2>&1 | tail -10` (timeout 600000)
Expected: FAIL — count is 12, table missing.

- [ ] **Step 3: Write both migrations**

Append to `src/db/schema.rs` after the v12 `run_migration` block (match the established `run_migration(conn, N, |c| { ... })` style used by v11/v12 — read those first):

```rust
    // v13 (#297/#302): per-topic centroid rows + the shadow-compare column.
    // No FK on SQLite — topic_fingerprint was rebuilt via a rename in v4 and
    // rusqlite FK enforcement is off by default; deletes are handled
    // explicitly inside delete_user_data's transaction instead.
    run_migration(conn, 13, |c| {
        c.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS topic_clusters (
                 user_did TEXT NOT NULL,
                 cluster_index INTEGER NOT NULL,
                 centroid TEXT NOT NULL,
                 post_count INTEGER NOT NULL,
                 PRIMARY KEY (user_did, cluster_index)
             );
             ALTER TABLE account_scores ADD COLUMN overlap_legacy REAL;
             COMMIT;",
        )?;
        Ok(())
    })?;
```

(If `run_migration`'s closure signature differs, adapt — read the v12 block. The explicit `BEGIN;/COMMIT;` matters: `execute_batch` does not auto-wrap, per the comment at schema.rs:106-110.)

Create `migrations/postgres/0013_topic_clusters.sql`:

```sql
-- #297/#302: one row per protected-user topic centroid, plus the
-- shadow-compare column for #135 recalibration data.
--
-- cluster_index is 0-based and corresponds 1:1 with clusters[i] in the
-- fingerprint JSON — save_fingerprint_bundle writes both in one transaction,
-- so they cannot diverge (#302).

CREATE TABLE IF NOT EXISTS topic_clusters (
    user_did TEXT NOT NULL REFERENCES topic_fingerprint(user_did) ON DELETE CASCADE,
    cluster_index INTEGER NOT NULL,
    centroid vector(384) NOT NULL,
    post_count INTEGER NOT NULL,
    PRIMARY KEY (user_did, cluster_index)
);

ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS overlap_legacy DOUBLE PRECISION;

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (13) ON CONFLICT DO NOTHING;
```

Register it in `src/db/postgres.rs` after the 0012 tuple (~line 164):

```rust
                (
                    13,
                    include_str!("../../migrations/postgres/0013_topic_clusters.sql"),
                ),
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features web test_trait_table_count test_migration_v13 2>&1 | tail -10` (timeout 600000)
Expected: PASS both.

Also compile the postgres feature: `cargo check --features postgres 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add src/db/schema.rs src/db/sqlite.rs src/db/postgres.rs migrations/postgres/0013_topic_clusters.sql
git commit -m 'feat(297): migration 0013 - topic_clusters table + account_scores.overlap_legacy'
git push
```

---

### Task 5: `save_fingerprint_bundle` + `get_topic_centroids` — trait, SQLite impl, delete path

**Files:**
- Modify: `src/db/models.rs` (add `ClusterCentroid` struct near `AccountScore`)
- Modify: `src/db/traits.rs:183-200` (two new methods in the "Topic fingerprint" block)
- Modify: `src/db/queries.rs` (new functions after `get_embedding` ~line 144; extend `delete_user_data` ~line 936)
- Modify: `src/db/sqlite.rs` (trait impls near the existing fingerprint impls ~line 68)
- Test: inline in `src/db/sqlite.rs` alongside `test_trait_fingerprint_roundtrip` (~line 502)

**Interfaces:**
- Consumes: table `topic_clusters` (Task 4).
- Produces (used by Tasks 6, 7, 8, 9):

```rust
// src/db/models.rs
/// One stored topic centroid. Label/keywords/weight live in the fingerprint
/// JSON (clusters[i] ↔ cluster_index i); this is only what scoring needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterCentroid {
    pub centroid: Vec<f64>,
    pub post_count: u32,
}

// src/db/traits.rs — in the Topic fingerprint block
/// Persist a complete fingerprint generation atomically: JSON, mean
/// embedding, and per-topic centroid rows in ONE transaction, bumping
/// updated_at exactly once. `embedding: None` with empty `clusters` is the
/// legal keyword-only bundle (embedder unavailable). (#302)
async fn save_fingerprint_bundle(
    &self,
    user_did: &str,
    fingerprint_json: &str,
    post_count: u32,
    embedding: Option<&[f64]>,
    clusters: &[ClusterCentroid],
) -> Result<()>;

/// Load stored topic centroids ordered by cluster_index. Empty = legacy
/// (pre-#297) or keyword-only fingerprint.
async fn get_topic_centroids(&self, user_did: &str) -> Result<Vec<ClusterCentroid>>;
```

- [ ] **Step 1: Write the failing tests**

Add to the test module in `src/db/sqlite.rs` (mirror the setup used by `test_trait_fingerprint_roundtrip` at ~502 — same `test_db()` helper):

```rust
#[tokio::test]
async fn test_bundle_roundtrip_full() {
    let db = test_db().await;
    let clusters = vec![
        crate::db::models::ClusterCentroid { centroid: vec![0.5; 384], post_count: 30 },
        crate::db::models::ClusterCentroid { centroid: vec![-0.25; 384], post_count: 12 },
    ];
    let emb = vec![0.125; 384];
    db.save_fingerprint_bundle(TEST_USER, "{\"clusters\":[],\"post_count\":42}", 42, Some(&emb), &clusters)
        .await
        .unwrap();

    let (json, count, updated_at) = db.get_fingerprint(TEST_USER).await.unwrap().unwrap();
    assert_eq!(count, 42);
    assert!(json.contains("post_count"));
    // updated_at honors the staleness parser's format contract.
    assert!(chrono::NaiveDateTime::parse_from_str(&updated_at, "%Y-%m-%d %H:%M:%S").is_ok());

    let stored_emb = db.get_embedding(TEST_USER).await.unwrap().unwrap();
    assert_eq!(stored_emb.len(), 384);
    let stored = db.get_topic_centroids(TEST_USER).await.unwrap();
    assert_eq!(stored, clusters); // order = cluster_index
}

#[tokio::test]
async fn test_bundle_replaces_previous_generation_completely() {
    let db = test_db().await;
    let three = vec![
        crate::db::models::ClusterCentroid { centroid: vec![0.1; 384], post_count: 5 },
        crate::db::models::ClusterCentroid { centroid: vec![0.2; 384], post_count: 6 },
        crate::db::models::ClusterCentroid { centroid: vec![0.3; 384], post_count: 7 },
    ];
    db.save_fingerprint_bundle(TEST_USER, "{}", 18, None, &three).await.unwrap();
    let two = vec![
        crate::db::models::ClusterCentroid { centroid: vec![0.9; 384], post_count: 9 },
        crate::db::models::ClusterCentroid { centroid: vec![0.8; 384], post_count: 8 },
    ];
    db.save_fingerprint_bundle(TEST_USER, "{}", 17, None, &two).await.unwrap();
    let stored = db.get_topic_centroids(TEST_USER).await.unwrap();
    assert_eq!(stored.len(), 2, "old generation's third row must not survive");
    assert_eq!(stored, two);
}

#[tokio::test]
async fn test_bundle_keyword_only_is_legal() {
    let db = test_db().await;
    db.save_fingerprint_bundle(TEST_USER, "{}", 10, None, &[]).await.unwrap();
    assert!(db.get_embedding(TEST_USER).await.unwrap().is_none());
    assert!(db.get_topic_centroids(TEST_USER).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_bundle_rejects_non_finite_and_preserves_previous_generation() {
    let db = test_db().await;
    let good = vec![crate::db::models::ClusterCentroid { centroid: vec![0.5; 384], post_count: 3 }];
    db.save_fingerprint_bundle(TEST_USER, "{\"gen\":1}", 3, None, &good).await.unwrap();

    let bad = vec![crate::db::models::ClusterCentroid { centroid: vec![f64::NAN; 384], post_count: 4 }];
    assert!(db.save_fingerprint_bundle(TEST_USER, "{\"gen\":2}", 4, None, &bad).await.is_err());

    // Generation 1 intact, in full: JSON and clusters both from gen 1.
    let (json, count, _) = db.get_fingerprint(TEST_USER).await.unwrap().unwrap();
    assert!(json.contains("gen\":1"));
    assert_eq!(count, 3);
    assert_eq!(db.get_topic_centroids(TEST_USER).await.unwrap(), good);
}

#[tokio::test]
async fn test_delete_user_data_clears_topic_clusters() {
    let db = test_db().await;
    let clusters = vec![crate::db::models::ClusterCentroid { centroid: vec![0.5; 384], post_count: 3 }];
    db.save_fingerprint_bundle(TEST_USER, "{}", 3, None, &clusters).await.unwrap();
    db.delete_user_data(TEST_USER).await.unwrap();
    assert!(db.get_topic_centroids(TEST_USER).await.unwrap().is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web test_bundle test_delete_user_data_clears_topic_clusters 2>&1 | tail -10` (timeout 600000)
Expected: COMPILE ERROR — trait methods and `ClusterCentroid` not found.

- [ ] **Step 3: Implement**

1. Add `ClusterCentroid` to `src/db/models.rs` (code in Interfaces above).
2. Add the two trait methods to `src/db/traits.rs` (code in Interfaces above; add `use` for `ClusterCentroid` following how `AccountScore` is imported there).
3. In `src/db/queries.rs`, after `get_embedding` (~line 144):

```rust
/// Persist a complete fingerprint generation in one transaction (#302).
/// `unchecked_transaction` for the same reason as delete_user_data: these
/// free functions take &Connection, and rusqlite's transaction() needs &mut.
pub fn save_fingerprint_bundle(
    conn: &Connection,
    user_did: &str,
    fingerprint_json: &str,
    post_count: u32,
    embedding: Option<&str>, // pre-serialized JSON array, like save_embedding
    clusters: &[crate::db::models::ClusterCentroid],
) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, embedding_vector, updated_at)
         VALUES (?1, ?2, ?3, ?4, datetime('now'))
         ON CONFLICT(user_did) DO UPDATE SET
            fingerprint_json = ?2,
            post_count = ?3,
            embedding_vector = ?4,
            updated_at = datetime('now')",
        params![user_did, fingerprint_json, post_count, embedding],
    )?;
    tx.execute(
        "DELETE FROM topic_clusters WHERE user_did = ?1",
        params![user_did],
    )?;
    for (i, cluster) in clusters.iter().enumerate() {
        let centroid_json = serde_json::to_string(&cluster.centroid)?;
        tx.execute(
            "INSERT INTO topic_clusters (user_did, cluster_index, centroid, post_count)
             VALUES (?1, ?2, ?3, ?4)",
            params![user_did, i as i64, centroid_json, cluster.post_count],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Load topic centroids in cluster_index order. Empty = legacy/keyword-only.
pub fn get_topic_centroids(
    conn: &Connection,
    user_did: &str,
) -> Result<Vec<crate::db::models::ClusterCentroid>> {
    let mut stmt = conn.prepare(
        "SELECT centroid, post_count FROM topic_clusters
         WHERE user_did = ?1 ORDER BY cluster_index",
    )?;
    let rows = stmt.query_map(params![user_did], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, u32>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (json, post_count) = row?;
        out.push(crate::db::models::ClusterCentroid {
            centroid: serde_json::from_str(&json)?,
            post_count,
        });
    }
    Ok(out)
}
```

4. In `delete_user_data` (`src/db/queries.rs:936`), add inside the existing transaction, next to the other per-user deletes (read the function; put it adjacent to the topic_fingerprint delete if one exists, otherwise with the data-table deletes):

```rust
    tx.execute(
        "DELETE FROM topic_clusters WHERE user_did = ?1",
        params![user_did],
    )?;
```

5. In `src/db/sqlite.rs`, implement both trait methods next to the existing fingerprint methods (~line 68), following the established lock-then-delegate shape. Validation lives HERE (backend-agnostic checks before any I/O — the Postgres impl repeats it):

```rust
    async fn save_fingerprint_bundle(
        &self,
        user_did: &str,
        fingerprint_json: &str,
        post_count: u32,
        embedding: Option<&[f64]>,
        clusters: &[ClusterCentroid],
    ) -> Result<()> {
        validate_bundle(embedding, clusters)?;
        let embedding_json = embedding.map(serde_json::to_string).transpose()?;
        let conn = self.conn.lock().await;
        queries::save_fingerprint_bundle(
            &conn,
            user_did,
            fingerprint_json,
            post_count,
            embedding_json.as_deref(),
            clusters,
        )
    }

    async fn get_topic_centroids(&self, user_did: &str) -> Result<Vec<ClusterCentroid>> {
        let conn = self.conn.lock().await;
        queries::get_topic_centroids(&conn, user_did)
    }
```

6. Add the shared validator to `src/db/traits.rs` (free function below the trait, exported for both backends):

```rust
/// Reject bundles that would poison future cosines: every stored float must
/// be finite. Runs before any I/O so a bad build can never split a
/// generation. (#296 discipline, applied to #302.)
pub fn validate_bundle(
    embedding: Option<&[f64]>,
    clusters: &[ClusterCentroid],
) -> Result<()> {
    if let Some(emb) = embedding {
        anyhow::ensure!(
            emb.iter().all(|v| v.is_finite()),
            "non-finite value in mean embedding",
        );
    }
    for (i, cluster) in clusters.iter().enumerate() {
        anyhow::ensure!(
            cluster.centroid.iter().all(|v| v.is_finite()),
            "non-finite value in centroid of cluster {i}",
        );
    }
    Ok(())
}
```

Import `validate_bundle` and `ClusterCentroid` in `src/db/sqlite.rs` following the file's existing `use` style.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features web test_bundle test_delete_user_data test_trait_fingerprint 2>&1 | tail -10` (timeout 600000)
Expected: all PASS (new tests plus the pre-existing roundtrips untouched).

- [ ] **Step 5: Commit**

```bash
git add src/db/models.rs src/db/traits.rs src/db/queries.rs src/db/sqlite.rs
git commit -m 'feat(302): save_fingerprint_bundle + get_topic_centroids, SQLite backend'
git push
```

---

### Task 6: Postgres backend for the bundle

**Files:**
- Modify: `src/db/postgres.rs` (impls next to `save_fingerprint`/`save_embedding` at ~271-311)
- Test: `tests/db_postgres.rs` (alongside `test_pg_fingerprint_roundtrip` ~line 98)

**Interfaces:**
- Consumes: `ClusterCentroid`, `validate_bundle` (Task 5); table `topic_clusters` (Task 4).
- Produces: Postgres `Database` impl parity. f64→f32 conversion matches `save_embedding` (pgvector is f32); `get_topic_centroids` returns f64 (lossy roundtrip is pre-existing behavior on the mean embedding — tests must compare with tolerance, not equality).

- [ ] **Step 1: Write the failing tests**

Add to `tests/db_postgres.rs`, following the file's existing `database_url()` early-return pattern EXACTLY (these no-op without a live DB — that is the established, documented behavior):

```rust
#[tokio::test]
async fn test_pg_bundle_roundtrip_and_replacement() {
    let Some(url) = database_url() else { return };
    let db = fresh_db(&url).await; // mirror however sibling tests build/clean their db + user row
    let clusters = vec![
        charcoal::db::models::ClusterCentroid { centroid: vec![0.5; 384], post_count: 30 },
        charcoal::db::models::ClusterCentroid { centroid: vec![-0.25; 384], post_count: 12 },
    ];
    let emb = vec![0.125; 384];
    db.save_fingerprint_bundle(TEST_USER, "{}", 42, Some(&emb), &clusters).await.unwrap();

    let stored = db.get_topic_centroids(TEST_USER).await.unwrap();
    assert_eq!(stored.len(), 2);
    // pgvector stores f32 — compare with tolerance, same as the mean-embedding tests.
    for (s, c) in stored.iter().zip(clusters.iter()) {
        assert_eq!(s.post_count, c.post_count);
        for (a, b) in s.centroid.iter().zip(c.centroid.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    // Replacement drops the old generation's extra rows.
    let one = vec![charcoal::db::models::ClusterCentroid { centroid: vec![0.9; 384], post_count: 9 }];
    db.save_fingerprint_bundle(TEST_USER, "{}", 9, None, &one).await.unwrap();
    let stored = db.get_topic_centroids(TEST_USER).await.unwrap();
    assert_eq!(stored.len(), 1);
    // None embedding leaves the column NULL for this generation.
    assert!(db.get_embedding(TEST_USER).await.unwrap().is_none());
}

#[tokio::test]
async fn test_pg_delete_user_cascades_topic_clusters() {
    let Some(url) = database_url() else { return };
    let db = fresh_db(&url).await;
    let clusters = vec![charcoal::db::models::ClusterCentroid { centroid: vec![0.5; 384], post_count: 3 }];
    db.save_fingerprint_bundle(TEST_USER, "{}", 3, None, &clusters).await.unwrap();
    db.delete_user_data(TEST_USER).await.unwrap();
    assert!(db.get_topic_centroids(TEST_USER).await.unwrap().is_empty());
}
```

(Adapt `fresh_db`/`TEST_USER` to whatever helpers the file actually defines — read it first. Note: `save_fingerprint_bundle(…, None, &one)` overwriting the embedding to NULL is intentional: the bundle IS the generation; a keyword-only rebuild replaces everything.)

- [ ] **Step 2: Run to verify failure**

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --features postgres --test db_postgres test_pg_bundle test_pg_delete_user_cascades 2>&1 | tail -10` (timeout 600000)
Expected: COMPILE ERROR (methods missing from PG impl). If the database is missing: `createdb charcoal_test` first.

- [ ] **Step 3: Implement in `src/db/postgres.rs`**

Next to `save_embedding` (~line 293):

```rust
    async fn save_fingerprint_bundle(
        &self,
        user_did: &str,
        fingerprint_json: &str,
        post_count: u32,
        embedding: Option<&[f64]>,
        clusters: &[ClusterCentroid],
    ) -> Result<()> {
        crate::db::traits::validate_bundle(embedding, clusters)?;
        // One transaction for the whole generation (#302): fingerprint row
        // (JSON + embedding + updated_at in one upsert), then cluster rows.
        let mut tx = self.pool.begin().await?;
        let vector = embedding.map(|e| {
            let floats: Vec<f32> = e.iter().map(|&v| v as f32).collect();
            pgvector::Vector::from(floats)
        });
        sqlx_core::query::query(
            "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, embedding_vector, updated_at)
             VALUES ($1, $2, $3, $4, NOW())
             ON CONFLICT(user_did) DO UPDATE SET
                fingerprint_json = $2,
                post_count = $3,
                embedding_vector = $4,
                updated_at = NOW()",
        )
        .bind(user_did)
        .bind(fingerprint_json)
        .bind(i32::try_from(post_count).context("post_count exceeds i32 range")?)
        .bind(vector)
        .execute(&mut *tx)
        .await?;
        sqlx_core::query::query("DELETE FROM topic_clusters WHERE user_did = $1")
            .bind(user_did)
            .execute(&mut *tx)
            .await?;
        for (i, cluster) in clusters.iter().enumerate() {
            let floats: Vec<f32> = cluster.centroid.iter().map(|&v| v as f32).collect();
            sqlx_core::query::query(
                "INSERT INTO topic_clusters (user_did, cluster_index, centroid, post_count)
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(user_did)
            .bind(i as i32)
            .bind(pgvector::Vector::from(floats))
            .bind(i32::try_from(cluster.post_count).context("cluster post_count exceeds i32 range")?)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn get_topic_centroids(&self, user_did: &str) -> Result<Vec<ClusterCentroid>> {
        let rows = sqlx_core::query::query(
            "SELECT centroid, post_count FROM topic_clusters
             WHERE user_did = $1 ORDER BY cluster_index",
        )
        .bind(user_did)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                let vector: pgvector::Vector = r.get(0);
                Ok(ClusterCentroid {
                    centroid: vector.to_vec().iter().map(|&v| v as f64).collect(),
                    post_count: r.get::<i32, _>(1) as u32,
                })
            })
            .collect()
    }
```

(Match the file's existing imports for `ClusterCentroid`; `Row::get` usage mirrors `get_fingerprint` at ~313.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --features postgres --test db_postgres 2>&1 | tail -10` (timeout 600000)
Expected: all PASS including the two new tests and the pre-existing roundtrips. This run is only meaningful with the DATABASE_URL set — verify the two new test names appear as `ok`, not just the suite summary ("verify the population can fail").

- [ ] **Step 5: Commit**

```bash
git add src/db/postgres.rs tests/db_postgres.rs
git commit -m 'feat(302): Postgres save_fingerprint_bundle + get_topic_centroids'
git push
```

---

### Task 7: `charcoal migrate` transfers cluster rows

**Files:**
- Modify: `src/main.rs` (`Commands::Migrate` arm, ~line 1123-1170 — the fingerprint/embedding transfer block at ~1160-1167)

**Interfaces:**
- Consumes: `get_fingerprint`, `get_embedding`, `get_topic_centroids`, `save_fingerprint_bundle` (Tasks 5-6).
- Produces: SQLite→Postgres transfer carries the full generation.

- [ ] **Step 1: Rewrite the transfer block**

Read the current block first (it calls `save_fingerprint` then conditionally `save_embedding` per user). Replace the fingerprint+embedding portion with one bundle call so the transferred generation is atomic on the PG side too:

```rust
                if let Some((json, post_count, _)) = sqlite_db.get_fingerprint(&did).await? {
                    let embedding = sqlite_db.get_embedding(&did).await?;
                    let clusters = sqlite_db.get_topic_centroids(&did).await?;
                    pg_db
                        .save_fingerprint_bundle(
                            &did,
                            &json,
                            post_count,
                            embedding.as_deref(),
                            &clusters,
                        )
                        .await?;
                }
```

Preserve whatever per-user println/progress output the block currently emits (read before writing; keep the style). Note the transferred `updated_at` is re-stamped to NOW() by the bundle — acceptable: a migrated fingerprint is at worst marked fresher than it was, and the 14-day staleness clock restarts.

- [ ] **Step 2: Verify it compiles + existing tests pass**

Run: `cargo check --features postgres 2>&1 | tail -5` then `cargo test --features web unit_admin 2>&1 | tail -5` (timeout 600000)
Expected: clean check; admin tests pass.

- [ ] **Step 3: Commit**

```bash
git add src/main.rs
git commit -m 'feat(302): charcoal migrate transfers fingerprint generation as one bundle'
git push
```

---

### Task 8: Embedding-first build path, shared by web scan + CLI

**Files:**
- Create: `src/topics/build.rs`
- Modify: `src/topics/mod.rs` (add `pub mod build;`)
- Modify: `src/web/scan_job.rs:546-627` (delete the old `build_user_fingerprint`; re-export the new one)
- Modify: `src/main.rs:230-294` (`Commands::Fingerprint` arm: delegate to the shared builder)
- Test: inline in `src/topics/build.rs`

**Why this shape:** `src/web` is behind the `web` feature (`src/lib.rs:20-21`) and the CLI `charcoal fingerprint` must build without it, so the shared logic lives in `src/topics/build.rs` (everything it needs — `PublicAtpClient`, `fetch_recent_posts`, `SentenceEmbedder`, `toxicity::download` helpers, `Database` — is feature-independent). `scan_job.rs` re-exports it so `src/web/handlers/admin.rs:241` keeps compiling unchanged. This resolves the spec's "dedupe main.rs" requirement.

**Interfaces:**
- Consumes: `build_clustered_fingerprint`, `ClusteringParams` (Tasks 1/3); `save_fingerprint_bundle`, `ClusterCentroid` (Task 5); `normalized_mean_embedding`, `clean_for_embedding`, `TfIdfExtractor`, `fetch_recent_posts`, `SentenceEmbedder`, `embedding_model_dir`, `embedding_files_present` (all existing).
- Produces (used by Task 9 and existing callers):

```rust
// src/topics/build.rs
/// Aligned originals + their embeddings (empty-cleaned posts already filtered).
pub struct EmbeddedPosts {
    pub original_texts: Vec<String>,
    pub embeddings: Vec<Vec<f64>>,
}

/// Everything one fingerprint generation persists.
pub struct FingerprintArtifacts {
    pub fingerprint: TopicFingerprint,
    pub mean_embedding: Option<Vec<f64>>,
    pub clusters: Vec<crate::db::models::ClusterCentroid>,
}

/// Pure assembly (no I/O): clustered fingerprint when embeddings are
/// available, keyword-only TF-IDF fingerprint when not.
pub fn assemble_fingerprint(
    post_texts: &[String],
    embedded: Option<&EmbeddedPosts>,
) -> anyhow::Result<FingerprintArtifacts>;

/// Fetch → embed (if model present) → assemble → save as ONE bundle (#302).
/// The single build path for web scans, admin pre-seed, and the CLI.
pub async fn build_user_fingerprint(
    config: &Config,
    db: &dyn Database,
    user_did: &str,
    handle: &str,
) -> anyhow::Result<()>;
```

- [ ] **Step 1: Write the failing tests for `assemble_fingerprint`**

Create `src/topics/build.rs` with the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_without_embeddings_is_keyword_only() {
        let posts: Vec<String> = vec![
            "fat liberation organizing and community care work".into(),
            "community organizing for fat liberation activists".into(),
            "care work and community organizing in fat spaces".into(),
        ];
        let artifacts = assemble_fingerprint(&posts, None).unwrap();
        assert!(artifacts.mean_embedding.is_none());
        assert!(artifacts.clusters.is_empty());
        assert!(!artifacts.fingerprint.clusters.is_empty()); // TF-IDF clusters
        assert_eq!(artifacts.fingerprint.post_count, 3);
    }

    #[test]
    fn assemble_with_embeddings_produces_matching_clusters_and_mean() {
        let posts: Vec<String> = vec![
            "fat liberation is about body autonomy and acceptance".into(),
            "fat acceptance and body liberation communities organizing".into(),
            "body autonomy fat liberation acceptance politics".into(),
            "choral rehearsal techniques for a cappella arrangements".into(),
            "arranging a cappella voicings for choral rehearsal".into(),
            "rehearsal warmups for a cappella choral singers".into(),
        ];
        let mut embs: Vec<Vec<f64>> = (0..3).map(|i| {
            let mut v = vec![0.0; crate::topics::embeddings::EMBEDDING_DIM];
            v[0] = 1.0; v[1] = i as f64 * 0.01; v
        }).collect();
        embs.extend((0..3).map(|i| {
            let mut v = vec![0.0; crate::topics::embeddings::EMBEDDING_DIM];
            v[100] = 1.0; v[101] = i as f64 * 0.01; v
        }));
        let embedded = EmbeddedPosts { original_texts: posts.clone(), embeddings: embs };
        let artifacts = assemble_fingerprint(&posts, Some(&embedded)).unwrap();
        assert_eq!(artifacts.clusters.len(), 2);
        assert_eq!(artifacts.fingerprint.clusters.len(), 2); // JSON ↔ rows 1:1
        let mean = artifacts.mean_embedding.unwrap();
        assert_eq!(mean.len(), crate::topics::embeddings::EMBEDDING_DIM);
        // Cluster post_counts reflect membership.
        assert_eq!(artifacts.clusters.iter().map(|c| c.post_count).sum::<u32>(), 6);
    }
}
```

Add `pub mod build;` to `src/topics/mod.rs`.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib topics::build 2>&1 | tail -10` (timeout 600000)
Expected: COMPILE ERROR.

- [ ] **Step 3: Implement `src/topics/build.rs`**

```rust
//! The single fingerprint build path (#297): embedding-first assembly with
//! keyword-only degradation, persisted as one atomic bundle (#302). Lives
//! outside `src/web` because the CLI `charcoal fingerprint` must build
//! without the `web` feature.

use anyhow::Result;
use tracing::{info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::config::Config;
use crate::db::models::ClusterCentroid;
use crate::db::traits::Database;
use crate::topics::clustering::{build_clustered_fingerprint, ClusteringParams};
use crate::topics::embeddings::normalized_mean_embedding;
use crate::topics::fingerprint::TopicFingerprint;
use crate::topics::tfidf::{clean_for_embedding, TfIdfExtractor};
use crate::topics::traits::TopicExtractor;
use crate::toxicity::download::{embedding_files_present, embedding_model_dir};

pub struct EmbeddedPosts {
    pub original_texts: Vec<String>,
    pub embeddings: Vec<Vec<f64>>,
}

pub struct FingerprintArtifacts {
    pub fingerprint: TopicFingerprint,
    pub mean_embedding: Option<Vec<f64>>,
    pub clusters: Vec<ClusterCentroid>,
}

/// Assemble one fingerprint generation. With embeddings: cluster in
/// embedding space and label per-cluster (the #297 fix). Without: today's
/// TF-IDF keyword fingerprint — same degradation as before, still atomic.
pub fn assemble_fingerprint(
    post_texts: &[String],
    embedded: Option<&EmbeddedPosts>,
) -> Result<FingerprintArtifacts> {
    match embedded {
        Some(e) if !e.embeddings.is_empty() => {
            let (fingerprint, post_clusters) = build_clustered_fingerprint(
                &e.original_texts,
                &e.embeddings,
                post_texts.len() as u32,
                &ClusteringParams::default(),
            )?;
            let clusters = post_clusters
                .iter()
                .map(|c| ClusterCentroid {
                    centroid: c.centroid.clone(),
                    post_count: c.members.len() as u32,
                })
                .collect();
            Ok(FingerprintArtifacts {
                fingerprint,
                mean_embedding: Some(normalized_mean_embedding(&e.embeddings)),
                clusters,
            })
        }
        _ => {
            let extractor = TfIdfExtractor::default();
            let fingerprint = extractor.extract(&post_texts.to_vec())?;
            Ok(FingerprintArtifacts {
                fingerprint,
                mean_embedding: None,
                clusters: Vec::new(),
            })
        }
    }
}

/// Build a topic fingerprint for a user and persist it atomically.
/// Used by the scan pipeline (auto-fingerprint), admin pre-seed, and the CLI.
pub async fn build_user_fingerprint(
    config: &Config,
    db: &dyn Database,
    user_did: &str,
    handle: &str,
) -> Result<()> {
    info!("Building topic fingerprint for {user_did}");

    let client = PublicAtpClient::new(&config.public_api_url)?;
    let fp_posts = crate::bluesky::posts::fetch_recent_posts(&client, handle, 500).await?;
    if fp_posts.is_empty() {
        anyhow::bail!(
            "No posts found — Charcoal needs posting history to build a topic fingerprint."
        );
    }
    let post_texts: Vec<String> = fp_posts.iter().map(|p| p.text.clone()).collect();

    // Embed when the model is on disk; every failure below degrades to the
    // keyword-only bundle rather than failing the build — a keyword
    // fingerprint beats no fingerprint, and the staleness clock retries in
    // 14 days anyway.
    let embedded: Option<EmbeddedPosts> = if embedding_files_present(&config.model_dir) {
        let embed_dir = embedding_model_dir(&config.model_dir);
        match tokio::task::spawn_blocking(move || {
            crate::topics::embeddings::SentenceEmbedder::load(&embed_dir)
        })
        .await
        {
            Ok(Ok(embedder)) => {
                // Keep originals aligned with their cleaned forms so cluster
                // members map back to real posts for TF-IDF labeling.
                let aligned: Vec<(String, String)> = post_texts
                    .iter()
                    .map(|t| (t.clone(), clean_for_embedding(t)))
                    .filter(|(_, c)| !c.is_empty())
                    .collect();
                if aligned.is_empty() {
                    // URLs/mentions-only corpus (#301): no embeddings, no
                    // zero-centroid poisoning — keyword-only bundle below.
                    warn!("All posts cleaned to empty; building keyword-only fingerprint");
                    None
                } else {
                    let cleaned: Vec<String> =
                        aligned.iter().map(|(_, c)| c.clone()).collect();
                    match embedder.embed_batch(&cleaned).await {
                        Ok(embeddings) => Some(EmbeddedPosts {
                            original_texts: aligned.into_iter().map(|(o, _)| o).collect(),
                            embeddings,
                        }),
                        Err(e) => {
                            warn!(error = %e, "embed_batch failed during fingerprint build");
                            None
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Embedding model failed to load during fingerprint build");
                None
            }
            Err(e) => {
                warn!(error = %e, "spawn_blocking panicked loading embedder during fingerprint build");
                None
            }
        }
    } else {
        None
    };

    let artifacts = assemble_fingerprint(&post_texts, embedded.as_ref())?;
    let json = serde_json::to_string(&artifacts.fingerprint)?;
    db.save_fingerprint_bundle(
        user_did,
        &json,
        artifacts.fingerprint.post_count,
        artifacts.mean_embedding.as_deref(),
        &artifacts.clusters,
    )
    .await?;
    info!(
        post_count = artifacts.fingerprint.post_count,
        topics = artifacts.clusters.len(),
        embedded = artifacts.mean_embedding.is_some(),
        "Topic fingerprint built and saved atomically"
    );
    Ok(())
}
```

(Check the actual import paths — e.g. `PublicAtpClient`'s module and whether `clean_for_embedding` is `pub` — against `src/web/scan_job.rs`'s current imports, and mirror them. `assemble_fingerprint`'s `&post_texts.to_vec()` is only needed if `extract` takes `&[String]` — it does (`src/topics/traits.rs:13`), so pass `post_texts` directly; drop the `.to_vec()`.)

- [ ] **Step 4: Replace the old build path**

1. In `src/web/scan_job.rs`, DELETE the whole old `build_user_fingerprint` (lines ~546-627) and add near the other `use` statements:

```rust
pub use crate::topics::build::build_user_fingerprint;
```

`fingerprint_is_stale` and `FINGERPRINT_MAX_AGE_DAYS` stay in scan_job.rs (web-only staleness policy). Verify `src/web/handlers/admin.rs:241` still resolves (it calls through scan_job — the re-export covers it).

2. In `src/main.rs` `Commands::Fingerprint` (lines ~230-294): replace the whole hand-rolled sequence (fetch → extract → display → save_fingerprint → embed → save_embedding) with:

```rust
            println!("Building topic fingerprint from your recent posts...");
            if !charcoal::toxicity::download::embedding_files_present(&config.model_dir) {
                println!(
                    "{}",
                    "Tip: Run `charcoal download-model` to enable semantic topic overlap.".dimmed()
                );
            }
            charcoal::topics::build::build_user_fingerprint(
                &config,
                &*db,
                &did,
                &config.bluesky_handle,
            )
            .await?;
            let (json, _, _) = db
                .get_fingerprint(&did)
                .await?
                .expect("fingerprint was just saved");
            let fingerprint: charcoal::topics::fingerprint::TopicFingerprint =
                serde_json::from_str(&json)?;
            fingerprint.display();
            println!(
                "{}",
                "Fingerprint saved. Review the topics above — do they look accurate?".bold()
            );
```

(Keep the surrounding arm structure — read the arm first; only the body between the opening `println!` and the closing brace changes. `expect` here is CLI-terminal and just-written — acceptable.)

- [ ] **Step 5: Run tests**

Run: `cargo test --lib topics::build 2>&1 | tail -10` then `CHARCOAL_MODEL_DIR=./models cargo test --features web scan_job 2>&1 | tail -10` (timeout 600000)
Expected: new tests PASS; scan_job staleness tests still PASS; whole tree compiles under `cargo check` and `cargo check --features web`.

- [ ] **Step 6: Commit**

```bash
git add src/topics/build.rs src/topics/mod.rs src/web/scan_job.rs src/main.rs
git commit -m 'feat(297): embedding-first build path shared by web scan and CLI, atomic bundle save'
git push
```

---

### Task 9: Scan wiring — centroids into scoring, shadow column, legacy rebuild trigger

**Files:**
- Modify: `src/web/scan_job.rs` (~797-840: legacy-format trigger + centroid load)
- Modify: `src/pipeline/scan_phases/finalize.rs` (~58-75 params, ~195: thread centroids)
- Modify: `src/scoring/profile.rs` (`score_from_sample` signature ~462-486; overlap block ~584-644; `AccountScore` construction near the end of the function)
- Modify: `src/db/models.rs` (`AccountScore` + `overlap_legacy` field)
- Modify: `src/db/queries.rs` (`upsert_account_score` ~149; the row-mapper for reads)
- Modify: `src/db/postgres.rs` (same two)
- Test: `tests/unit_profile.rs`, inline in `src/web/scan_job.rs`

**Interfaces:**
- Consumes: `max_topic_overlap` (Task 2), `get_topic_centroids` (Task 5), column `overlap_legacy` (Task 4).
- Produces:
  - `AccountScore` gains `pub overlap_legacy: Option<f64>` (after `topic_overlap`).
  - `score_from_sample` gains `protected_topic_centroids: Option<&[Vec<f64>]>` immediately after `protected_embedding` (`src/scoring/profile.rs:470`).
  - Semantics: live `topic_overlap` = max-over-topics when centroids are non-empty, else the old mean-centroid cosine; `overlap_legacy` = mean-centroid cosine whenever the embedding path ran, `None` on the keyword-scale path.

- [ ] **Step 1: Write the failing tests**

In `tests/unit_profile.rs` (read the file's existing helpers first — `score_from_sample_uses_precomputed_target_embedding_without_an_embedder` shows how to call `score_from_sample` with a `PostSample` fixture; copy its setup):

```rust
#[tokio::test]
async fn multi_topic_centroids_use_max_and_record_legacy_shadow() {
    // Protected user has two orthogonal topics; the candidate's precomputed
    // centroid sits exactly on topic B. Live overlap must be ~1.0 (max over
    // topics); overlap_legacy must be the much lower mean-centroid cosine.
    let dim = charcoal::topics::embeddings::EMBEDDING_DIM;
    let mut topic_a = vec![0.0; dim];
    topic_a[0] = 1.0;
    let mut topic_b = vec![0.0; dim];
    topic_b[100] = 1.0;
    let mean = charcoal::topics::embeddings::normalized_mean_embedding(
        &[topic_a.clone(), topic_b.clone()],
    );
    let candidate = topic_b.clone();
    let centroids = vec![topic_a, topic_b];

    let score = /* call score_from_sample exactly like the precomputed-path
       test does, but passing:
         protected_embedding: Some(&mean),
         protected_topic_centroids: Some(&centroids),
         precomputed_target_embedding: Some(&candidate)  */;

    let overlap = score.topic_overlap.unwrap();
    assert!(overlap > 0.99, "live overlap should be max-over-topics, got {overlap}");
    let legacy = score.overlap_legacy.unwrap();
    assert!(legacy < 0.85, "legacy shadow should be the smeared mean cosine, got {legacy}");
    assert!(overlap > legacy);
}

#[tokio::test]
async fn empty_centroids_degrade_to_legacy_behavior() {
    // Pre-#297 fingerprint: no centroid rows. Live overlap == legacy overlap
    // == mean-centroid cosine; shadow still recorded.
    /* same setup, but protected_topic_centroids: None */
    let overlap = score.topic_overlap.unwrap();
    let legacy = score.overlap_legacy.unwrap();
    assert!((overlap - legacy).abs() < 1e-9);
}
```

(These are the two REQUIRED behaviors; write them as real tests using the file's existing fixture helpers — the comment blocks above mark exactly which arguments differ from the existing precomputed-path test. Add `overlap_legacy: None` to any `AccountScore` literal fixtures the compiler flags across the test suite.)

In `src/web/scan_job.rs`'s inline test module (~1598), add:

```rust
#[test]
fn legacy_format_forces_rebuild() {
    // Embedding present but zero centroid rows = pre-#297 generation.
    assert!(fingerprint_needs_rebuild_for_format(true, 0, 3));
    // Keyword-only fingerprint (no embedding) is NOT legacy — no rebuild loop.
    assert!(!fingerprint_needs_rebuild_for_format(false, 0, 3));
    // Row/JSON mismatch (pre-#302 divergence) rebuilds.
    assert!(fingerprint_needs_rebuild_for_format(true, 2, 3));
    // Healthy clustered generation: no rebuild.
    assert!(!fingerprint_needs_rebuild_for_format(true, 3, 3));
}
```

- [ ] **Step 2: Run to verify failure**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test unit_profile multi_topic empty_centroids 2>&1 | tail -10` and `cargo test --features web legacy_format_forces_rebuild 2>&1 | tail -5` (timeout 600000)
Expected: COMPILE ERRORS (new param, new field, new helper missing).

- [ ] **Step 3: Implement, in this order (compiler-driven)**

1. **`src/db/models.rs`**: add to `AccountScore` after `topic_overlap` (line 15):

```rust
    /// Shadow-compare value (#297): what overlap WOULD have been under the
    /// pre-#297 single-mean-centroid formula. Recorded so #135 can
    /// recalibrate gates/tiers from real paired data. None on the keyword
    /// fallback path (no embeddings involved).
    pub overlap_legacy: Option<f64>,
```

2. **`src/db/queries.rs` `upsert_account_score` (~149)**: add `overlap_legacy` to the column list, `?N` placeholders, the `DO UPDATE SET` clause, and the `params![]` — keep the existing ordering style. Update the row-mapping read function(s) in the same file (find them: `rg -n "topic_overlap" src/db/queries.rs`) to read the new column. Same in **`src/db/postgres.rs`** (`rg -n "topic_overlap" src/db/postgres.rs`).

3. **`src/scoring/profile.rs`**: add the parameter at line ~475:

```rust
    protected_topic_centroids: Option<&[Vec<f64>]>,
```

Replace the overlap block (~584-644) so each embedding-scale arm computes `(live, legacy)`:

```rust
    // Step 3: Compute topic overlap with the protected user.
    //
    // Live overlap = max over the protected user's topic centroids (#297) —
    // "is this account near ANY of my topics?". The pre-#297 mean-centroid
    // cosine is computed alongside as `overlap_legacy` (shadow compare) so
    // #135 can recalibrate from real paired data. Legacy fingerprints (no
    // centroid rows) degrade to the mean-centroid cosine for BOTH values.
    let topic_centroids = protected_topic_centroids.filter(|c| !c.is_empty());
    let (topic_overlap, overlap_is_keyword_scale, overlap_legacy) =
        if let (Some(precomputed), Some(protected_emb)) =
            (precomputed_target_embedding, protected_embedding)
        {
            let legacy = embeddings::cosine_similarity_embeddings(protected_emb, precomputed);
            let live = topic_centroids
                .and_then(|tc| embeddings::max_topic_overlap(tc, precomputed))
                .unwrap_or(legacy);
            (live, false, Some(legacy))
        } else {
            let embedded_overlap =
                if let (Some(emb), Some(protected_emb)) = (embedder, protected_embedding) {
                    let embed_texts: Vec<String> = fingerprint_posts
                        .iter()
                        .map(|t| crate::topics::tfidf::clean_for_embedding(t))
                        .filter(|t| !t.is_empty())
                        .collect();
                    if embed_texts.is_empty() {
                        None
                    } else {
                        let target_embeddings = emb.embed_batch(&embed_texts).await?;
                        let target_mean =
                            embeddings::normalized_mean_embedding(&target_embeddings);
                        let legacy = embeddings::cosine_similarity_embeddings(
                            protected_emb,
                            &target_mean,
                        );
                        let live = topic_centroids
                            .and_then(|tc| embeddings::max_topic_overlap(tc, &target_mean))
                            .unwrap_or(legacy);
                        Some((live, legacy))
                    }
                } else {
                    None
                };

            match embedded_overlap {
                Some((live, legacy)) => (live, false, Some(legacy)),
                None => {
                    let topic_extractor = TfIdfExtractor::default();
                    let target_fingerprint = topic_extractor.extract(&fingerprint_posts)?;
                    (
                        overlap::cosine_similarity(protected_fingerprint, &target_fingerprint),
                        true,
                        None, // keyword scale — no embedding shadow exists
                    )
                }
            }
        };
```

Preserve the existing explanatory comments where they still apply (the precomputed-path #213 comment, the empty-clean #301 comment) — merge, don't drop. Then set `overlap_legacy` in the `AccountScore` literal at the end of the function (find `topic_overlap:` there).

4. **Thread the parameter**: `rg -n "score_from_sample" src/ tests/` and add the argument at every call site. In `src/pipeline/scan_phases/finalize.rs` add a `protected_topic_centroids: Option<&[Vec<f64>]>` parameter beside `protected_embedding` (line ~70) and pass it through at line ~195. Follow `protected_embedding`'s ownership pattern upward: wherever a caller loads `db.get_embedding(user_did)`, also load

```rust
    let protected_centroid_rows = db.get_topic_centroids(user_did).await?;
    let protected_topic_centroids: Vec<Vec<f64>> =
        protected_centroid_rows.iter().map(|c| c.centroid.clone()).collect();
```

and pass `Some(&protected_topic_centroids)` (or `None` where `protected_embedding` is `None`). Existing tests that call these functions get `None`.

5. **Legacy rebuild trigger** in `src/web/scan_job.rs`: add next to `fingerprint_is_stale`:

```rust
/// A stored generation needs a format rebuild when the embedding path ran
/// (mean embedding exists) but the per-topic centroid rows don't match the
/// fingerprint JSON: zero rows = pre-#297 legacy, count mismatch = pre-#302
/// divergence. Keyword-only fingerprints (no embedding) are NOT legacy —
/// treating them as such would rebuild-loop every scan on model-less
/// deployments. (#297)
pub fn fingerprint_needs_rebuild_for_format(
    has_embedding: bool,
    centroid_rows: usize,
    json_clusters: usize,
) -> bool {
    has_embedding && centroid_rows != json_clusters
}
```

Wire it into the load block (~797): after fetching `db.get_fingerprint`, also fetch `db.get_embedding` and `db.get_topic_centroids` (they're needed below anyway — reorder so they're loaded once, before the match), parse the candidate JSON, and route to the rebuild arm when `fingerprint_is_stale(..) || fingerprint_needs_rebuild_for_format(embedding.is_some(), centroids.len(), parsed.clusters.len())`. Keep the existing behaviors: rebuild-failure falls back to the stored fingerprint, absent fingerprint hard-fails on build error. After a successful rebuild, RE-load embedding + centroids (the bundle just replaced them). NOTE: a pre-#297 fingerprint's JSON clusters are keyword clusters, so `centroid_rows (0) != json_clusters (>0)` correctly flags it; a post-#297 generation always has equal counts by construction (Task 3 guarantee).

- [ ] **Step 4: Run the full affected suites**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test unit_profile --test unit_scoring --test unit_scan_phases 2>&1 | tail -15` then `CHARCOAL_MODEL_DIR=./models cargo test --features web 2>&1 | tail -5` (timeout 600000)
Expected: all PASS. Golden/composition tests (`tests/composition.rs`, `tests/golden_build_profile.rs`) may assert overlap values — if any fail, update expectations ONLY where the new max-over-topics semantics explain the shift, and say so in the commit message.

- [ ] **Step 5: Commit**

```bash
git add src/db/models.rs src/db/queries.rs src/db/postgres.rs src/scoring/profile.rs src/pipeline/scan_phases/finalize.rs src/web/scan_job.rs tests/unit_profile.rs
git commit -m 'feat(297): max-over-topics live overlap, overlap_legacy shadow, legacy-format rebuild'
git push
```

(Stage any additional files the parameter-threading touched — list them explicitly by name.)

---

### Task 10: Full verification + real-data validation + changelog

**Files:**
- Modify: `CHANGELOG.md` (new entry at top, matching the file's existing entry style — read it first)
- No other source changes expected; this task is gates.

- [ ] **Step 1: Format + lint, all feature combos**

Run (each; timeout 600000):
- `cargo fmt --all -- --check` (fix with `cargo fmt --all` if needed)
- `cargo clippy --features web --all-targets 2>&1 | tail -5`
- `cargo clippy --features postgres --all-targets 2>&1 | tail -5`
- `cargo clippy --no-default-features --features sqlite --all-targets 2>&1 | tail -5`
Expected: zero warnings.

- [ ] **Step 2: Full model-gated suite, zero skips**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:"` (timeout 600000)
Expected: EMPTY output (the grep matches the `SKIP:` sentinel exactly — no `-i`). Then confirm the suite itself passed: `CHARCOAL_MODEL_DIR=./models cargo test --features web 2>&1 | tail -3`.

- [ ] **Step 3: Postgres suite against live DB**

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres 2>&1 | tail -5` (timeout 600000)
Expected: all PASS. Confirm the Task 6 test names ran (not silently no-opped): rerun with `-- --show-output 2>&1 | grep test_pg_bundle`.

- [ ] **Step 4: Real-data validation (STOP — report to Bryan, do not tune alone)**

Run: `cargo run --features web -- fingerprint` (with the local `.env` — this rebuilds Bryan's real fingerprint via the new path).
Report the resulting topic list verbatim in the session. ACCEPTANCE: clusters recover recognizable distinct topics (fat liberation / a cappella / Atlassian / AI etc.), no single mega-cluster, no shattered 12-way split of one topic. If degenerate, adjust `merge_threshold` (±0.05 steps) / `max_clusters` in `ClusteringParams::default()` WITH Bryan's sign-off, re-run, and record the chosen values in a deciduous observation node.

- [ ] **Step 5: Changelog + close-out commit**

Add the CHANGELOG entry (handwritten — never `chainlink issue close` without `--no-changelog`). Cover: multi-interest fingerprint, max-over-topics overlap, shadow compare column, atomic bundle (#302), migration 0013, deploy effect (first scan per user rebuilds).

```bash
git add CHANGELOG.md
git commit -m 'docs(297): changelog for multi-interest fingerprint + atomic persistence'
git push
```

Then log the deciduous `outcome` node (link to the Task 8/9 action nodes) and leave issues #297/#302 OPEN — closing happens after PR review/merge per the PR-loop feedback memory.

---

## Post-plan notes for the orchestrator

- PR: open against `staging` when all tasks are green (`gh pr create --base staging`), body via `--body-file` (never heredoc). The PR loop ends at **CodeRabbit APPROVED**, not green checks; sweep review BODIES for outside-diff findings; deferred findings need their threads resolved.
- Expect CI clippy (1.98) to possibly flag files this branch never touched (toolchain drift, #178-adjacent) — fix forward in a `fix(297)` commit.
- Deploy effect on staging merge: every protected user's next scan triggers a format rebuild (legacy-format trigger). Expected and amortized — same pattern as #296.
- #298 (bge-small), #299 (adversarial anchors), #304 (candidate-side dilution), #135 (recalibration using `overlap_legacy` data) are explicitly OUT of this branch.
