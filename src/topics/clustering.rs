//! Greedy agglomerative clustering of post embeddings (centroid linkage over
//! cosine). Deterministic by construction — no RNG — so fingerprints are
//! reproducible and the tests can assert exact structure. (#297)

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
        Self {
            merge_threshold: 0.60,
            max_clusters: 12,
        }
    }
}

/// Greedy agglomerative clustering, centroid linkage over cosine.
///
/// Deterministic: pairs are compared with a fixed tie-break (lowest index
/// pair wins), so the same input always produces the same clusters — no RNG,
/// which keeps fingerprints reproducible and tests exact. O(n³) worst case
/// (up to n-1 merges × O(n²) pair scan per merge); n ≤ 500 on the build path
/// keeps runtime sub-second. Scan-time cost is untouched.
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

    // Prune noise clusters. If pruning would delete everything (max_size < min_size),
    // keep every cluster tied for the largest size: a fragmented history keeps its
    // several small topics rather than one arbitrary survivor (decision 2026-08-21).
    // This ensures a valid fingerprint always has at least one topic.
    let min_size = 2usize.max((0.02 * n as f64).ceil() as usize);
    let max_size = clusters.iter().map(|(m, _)| m.len()).max().unwrap_or(0);
    let effective_min = if max_size < min_size {
        max_size
    } else {
        min_size
    };
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
            PostCluster {
                centroid,
                members,
                weight,
            }
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
        let params = ClusteringParams {
            merge_threshold: 0.60,
            max_clusters: 3,
        };
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
        let norm: f64 = clusters[0]
            .centroid
            .iter()
            .map(|v| v * v)
            .sum::<f64>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-9);
    }

    #[test]
    fn all_below_min_size_keeps_every_tied_largest_cluster() {
        // 5 mutually-orthogonal singleton topics: min_size = max(2, ceil(0.1)) = 2,
        // every cluster is size 1, so the tied-largest fallback keeps ALL of them
        // (decision 2026-08-21: a fragmented history keeps its several small
        // topics rather than one arbitrary survivor).
        let embs: Vec<Vec<f64>> = (0..5)
            .map(|g| {
                let mut v = vec![0.0; crate::topics::embeddings::EMBEDDING_DIM];
                v[g * 40] = 1.0;
                v
            })
            .collect();
        let clusters = cluster_embeddings(&embs, &ClusteringParams::default());
        assert_eq!(clusters.len(), 5);
        let total_weight: f64 = clusters.iter().map(|c| c.weight).sum();
        assert!((total_weight - 1.0).abs() < 1e-9);
    }

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
}
