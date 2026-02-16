// Unit tests for topic extraction and overlap functions.
//
// Tests isolated pure functions: TopicFingerprint::keyword_weights edge cases,
// cosine_from_weights numerical edge cases, and TfIdfExtractor::extract
// invariant properties.

use std::collections::HashMap;

use charcoal::topics::fingerprint::{TopicCluster, TopicFingerprint};
use charcoal::topics::overlap::{cosine_from_weights, cosine_similarity};
use charcoal::topics::tfidf::TfIdfExtractor;
use charcoal::topics::traits::TopicExtractor;

// ============================================================
// TopicFingerprint::keyword_weights — edge cases
// ============================================================

#[test]
fn keyword_weights_empty_clusters() {
    let fp = TopicFingerprint {
        clusters: vec![],
        post_count: 0,
    };
    assert!(fp.keyword_weights().is_empty());
}

#[test]
fn keyword_weights_single_cluster_single_keyword() {
    let fp = TopicFingerprint {
        clusters: vec![TopicCluster {
            label: "test".to_string(),
            keywords: vec!["fat".to_string()],
            weight: 0.8,
        }],
        post_count: 10,
    };
    let w = fp.keyword_weights();
    assert!((w["fat"] - 0.8).abs() < 0.001);
}

#[test]
fn keyword_weights_distributed_evenly() {
    // 3 keywords sharing a cluster weight of 0.9
    let fp = TopicFingerprint {
        clusters: vec![TopicCluster {
            label: "test".to_string(),
            keywords: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            weight: 0.9,
        }],
        post_count: 10,
    };
    let w = fp.keyword_weights();
    assert!((w["a"] - 0.3).abs() < 0.001);
    assert!((w["b"] - 0.3).abs() < 0.001);
    assert!((w["c"] - 0.3).abs() < 0.001);
}

#[test]
fn keyword_weights_accumulates_across_clusters() {
    // Same keyword in two clusters — weights should add
    let fp = TopicFingerprint {
        clusters: vec![
            TopicCluster {
                label: "A".to_string(),
                keywords: vec!["shared".to_string()],
                weight: 0.3,
            },
            TopicCluster {
                label: "B".to_string(),
                keywords: vec!["shared".to_string()],
                weight: 0.2,
            },
        ],
        post_count: 10,
    };
    let w = fp.keyword_weights();
    assert!(
        (w["shared"] - 0.5).abs() < 0.001,
        "Shared keyword should accumulate weights: got {}",
        w["shared"]
    );
}

#[test]
fn keyword_weights_empty_keywords_vec_produces_empty_map() {
    // Cluster exists but has no keywords — nothing added to map
    let fp = TopicFingerprint {
        clusters: vec![TopicCluster {
            label: "empty".to_string(),
            keywords: vec![],
            weight: 0.5,
        }],
        post_count: 10,
    };
    assert!(fp.keyword_weights().is_empty());
}

#[test]
fn keyword_weights_zero_weight_cluster() {
    let fp = TopicFingerprint {
        clusters: vec![TopicCluster {
            label: "zero".to_string(),
            keywords: vec!["a".to_string(), "b".to_string()],
            weight: 0.0,
        }],
        post_count: 10,
    };
    let w = fp.keyword_weights();
    assert_eq!(w["a"], 0.0);
    assert_eq!(w["b"], 0.0);
}

// ============================================================
// cosine_from_weights — numerical edge cases
// ============================================================

#[test]
fn cosine_one_empty_one_nonempty() {
    let empty: HashMap<String, f64> = HashMap::new();
    let nonempty: HashMap<String, f64> = [("fat".to_string(), 0.5)].into();
    assert_eq!(cosine_from_weights(&empty, &nonempty), 0.0);
    assert_eq!(cosine_from_weights(&nonempty, &empty), 0.0);
}

#[test]
fn cosine_both_empty() {
    let empty: HashMap<String, f64> = HashMap::new();
    assert_eq!(cosine_from_weights(&empty, &empty), 0.0);
}

#[test]
fn cosine_all_zero_weights() {
    let a: HashMap<String, f64> = [("a".to_string(), 0.0), ("b".to_string(), 0.0)].into();
    let b: HashMap<String, f64> = [("a".to_string(), 0.0)].into();
    // Magnitude is 0 -> denominator < EPSILON -> returns 0.0
    assert_eq!(cosine_from_weights(&a, &b), 0.0);
}

#[test]
fn cosine_negative_weights_clamped_to_zero() {
    // Negative dot product gets clamped to 0.0
    let a: HashMap<String, f64> = [("x".to_string(), 1.0)].into();
    let b: HashMap<String, f64> = [("x".to_string(), -1.0)].into();
    assert_eq!(
        cosine_from_weights(&a, &b),
        0.0,
        "Negative cosine should be clamped to 0.0"
    );
}

#[test]
fn cosine_single_shared_keyword_is_one() {
    let a: HashMap<String, f64> = [("x".to_string(), 0.7)].into();
    let b: HashMap<String, f64> = [("x".to_string(), 0.3)].into();
    let result = cosine_from_weights(&a, &b);
    assert!(
        (result - 1.0).abs() < 0.001,
        "Single shared keyword (same direction) should be ~1.0, got {result}"
    );
}

#[test]
fn cosine_orthogonal_vectors() {
    let a: HashMap<String, f64> = [("x".to_string(), 1.0)].into();
    let b: HashMap<String, f64> = [("y".to_string(), 1.0)].into();
    assert_eq!(
        cosine_from_weights(&a, &b),
        0.0,
        "Orthogonal vectors should have zero similarity"
    );
}

#[test]
fn cosine_is_symmetric() {
    let a: HashMap<String, f64> = [("x".to_string(), 0.5), ("y".to_string(), 0.3)].into();
    let b: HashMap<String, f64> = [("x".to_string(), 0.2), ("z".to_string(), 0.8)].into();
    let ab = cosine_from_weights(&a, &b);
    let ba = cosine_from_weights(&b, &a);
    assert!(
        (ab - ba).abs() < 1e-10,
        "Cosine should be symmetric: {ab} vs {ba}"
    );
}

#[test]
fn cosine_very_small_weights() {
    let a: HashMap<String, f64> = [("x".to_string(), 1e-100)].into();
    let b: HashMap<String, f64> = [("x".to_string(), 1e-100)].into();
    // Magnitude could be near f64::EPSILON — should not panic
    let result = cosine_from_weights(&a, &b);
    // Either returns 0.0 (below EPSILON) or ~1.0 if magnitude is large enough
    assert!((0.0..=1.0).contains(&result));
}

#[test]
fn cosine_large_sparse_vectors() {
    // Many keywords but only one shared — should still compute correctly
    let mut a: HashMap<String, f64> = HashMap::new();
    let mut b: HashMap<String, f64> = HashMap::new();
    for i in 0..100 {
        a.insert(format!("a_kw_{i}"), 0.01);
        b.insert(format!("b_kw_{i}"), 0.01);
    }
    // Add one shared keyword
    a.insert("shared".to_string(), 0.5);
    b.insert("shared".to_string(), 0.5);

    let result = cosine_from_weights(&a, &b);
    assert!(result > 0.0, "Should have some overlap via 'shared'");
    assert!(result < 1.0, "Should not be identical");
}

// ============================================================
// cosine_similarity via TopicFingerprint (convenience wrapper)
// ============================================================

#[test]
fn cosine_similarity_one_empty_fp() {
    let empty = TopicFingerprint {
        clusters: vec![],
        post_count: 0,
    };
    let nonempty = TopicFingerprint {
        clusters: vec![TopicCluster {
            label: "test".to_string(),
            keywords: vec!["fat".to_string()],
            weight: 0.5,
        }],
        post_count: 10,
    };
    assert_eq!(cosine_similarity(&empty, &nonempty), 0.0);
    assert_eq!(cosine_similarity(&nonempty, &empty), 0.0);
}

// ============================================================
// TfIdfExtractor::extract — invariant properties
// ============================================================

fn sample_posts() -> Vec<String> {
    vec![
        "Fat liberation is a civil rights movement that challenges weight stigma and diet culture"
            .to_string(),
        "Trans rights are human rights and queer identity deserves celebration not erasure"
            .to_string(),
        "Community governance requires trust accountability and transparent moderation practices"
            .to_string(),
        "DEI programs face backlash but equity work remains essential for justice and inclusion"
            .to_string(),
        "Atlassian Forge development requires understanding the app platform deeply".to_string(),
        "Weight stigma in medical settings causes real harm to fat patients seeking care"
            .to_string(),
        "Queer joy is resistance and trans visibility matters in public discourse and media"
            .to_string(),
        "Building inclusive spaces means centering marginalized voices in decision making"
            .to_string(),
    ]
}

#[test]
fn tfidf_weights_sum_to_one() {
    let extractor = TfIdfExtractor {
        top_n_keywords: 30,
        max_clusters: 5,
    };
    let fp = extractor.extract(&sample_posts()).unwrap();
    let weight_sum: f64 = fp.clusters.iter().map(|c| c.weight).sum();
    assert!(
        (weight_sum - 1.0).abs() < 0.01,
        "Cluster weights should sum to ~1.0, got {weight_sum}"
    );
}

#[test]
fn tfidf_clusters_sorted_by_weight_descending() {
    let extractor = TfIdfExtractor {
        top_n_keywords: 30,
        max_clusters: 5,
    };
    let fp = extractor.extract(&sample_posts()).unwrap();
    for window in fp.clusters.windows(2) {
        assert!(
            window[0].weight >= window[1].weight,
            "Clusters should be sorted descending: {} >= {}",
            window[0].weight,
            window[1].weight
        );
    }
}

#[test]
fn tfidf_cluster_labels_and_keywords_nonempty() {
    let extractor = TfIdfExtractor {
        top_n_keywords: 30,
        max_clusters: 5,
    };
    let fp = extractor.extract(&sample_posts()).unwrap();
    for cluster in &fp.clusters {
        assert!(
            !cluster.label.is_empty(),
            "Cluster label should not be empty"
        );
        assert!(!cluster.keywords.is_empty(), "Cluster should have keywords");
    }
}

#[test]
fn tfidf_post_count_matches_input() {
    let extractor = TfIdfExtractor {
        top_n_keywords: 30,
        max_clusters: 5,
    };
    let posts = sample_posts();
    let fp = extractor.extract(&posts).unwrap();
    assert_eq!(fp.post_count, posts.len() as u32);
}

#[test]
fn tfidf_respects_max_clusters() {
    let extractor = TfIdfExtractor {
        top_n_keywords: 30,
        max_clusters: 3,
    };
    let fp = extractor.extract(&sample_posts()).unwrap();
    assert!(
        fp.clusters.len() <= 3,
        "Should not exceed max_clusters=3, got {}",
        fp.clusters.len()
    );
}

#[test]
fn tfidf_duplicate_posts_does_not_panic() {
    let extractor = TfIdfExtractor {
        top_n_keywords: 10,
        max_clusters: 3,
    };
    let posts = vec!["Fat liberation activism healthcare stigma".to_string(); 10];
    // All-identical posts produce poor TF-IDF — should either succeed
    // with a fingerprint or return an error, but never panic
    let result = extractor.extract(&posts);
    match result {
        Ok(fp) => assert_eq!(fp.post_count, 10),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("no keywords") || msg.contains("No posts"),
                "Unexpected error: {msg}"
            );
        }
    }
}

#[test]
fn tfidf_empty_posts_errors() {
    let extractor = TfIdfExtractor::default();
    let result = extractor.extract(&[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("No posts"));
}

#[test]
fn tfidf_all_keywords_are_meaningful() {
    // Every keyword in every cluster should be at least 3 chars
    // and contain alphabetic characters
    let extractor = TfIdfExtractor {
        top_n_keywords: 40,
        max_clusters: 7,
    };
    let fp = extractor.extract(&sample_posts()).unwrap();
    for cluster in &fp.clusters {
        for keyword in &cluster.keywords {
            assert!(
                keyword.len() >= 3,
                "Keyword '{}' too short (< 3 chars)",
                keyword
            );
            assert!(
                keyword.chars().any(|c| c.is_alphabetic()),
                "Keyword '{}' has no alphabetic chars",
                keyword
            );
        }
    }
}
