// Weighted Jaccard similarity for topic overlap scoring.
//
// Compares two topic fingerprints by looking at their keyword weight vectors.
// For each keyword in either fingerprint, we take the minimum weight from both
// sides and the maximum weight from both sides. The overlap score is:
//
//   sum(min(weight_a, weight_b)) / sum(max(weight_a, weight_b))
//
// This gives 0.0 for no overlap and 1.0 for identical profiles. Keywords that
// are important to the protected user (high weight) matter more than minor ones.

use std::collections::{HashMap, HashSet};

use super::fingerprint::TopicFingerprint;

/// Compute the weighted Jaccard similarity between two fingerprints.
///
/// Returns a score from 0.0 (no overlap) to 1.0 (identical topic profiles).
pub fn weighted_jaccard(fp_a: &TopicFingerprint, fp_b: &TopicFingerprint) -> f64 {
    let weights_a = fp_a.keyword_weights();
    let weights_b = fp_b.keyword_weights();

    jaccard_from_weights(&weights_a, &weights_b)
}

/// Compute weighted Jaccard from raw keyword weight maps.
///
/// Separated from `weighted_jaccard` so it can be used with ad-hoc weight maps
/// (e.g., from a single account's posts without full clustering).
pub fn jaccard_from_weights(
    weights_a: &HashMap<String, f64>,
    weights_b: &HashMap<String, f64>,
) -> f64 {
    // Union of all keywords from both sides
    let all_keys: HashSet<&String> = weights_a.keys().chain(weights_b.keys()).collect();

    if all_keys.is_empty() {
        return 0.0;
    }

    let mut min_sum = 0.0;
    let mut max_sum = 0.0;

    for key in all_keys {
        let a = weights_a.get(key).copied().unwrap_or(0.0);
        let b = weights_b.get(key).copied().unwrap_or(0.0);
        min_sum += a.min(b);
        max_sum += a.max(b);
    }

    if max_sum == 0.0 {
        0.0
    } else {
        min_sum / max_sum
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topics::fingerprint::{TopicCluster, TopicFingerprint};

    fn make_fp(keywords_and_weights: &[(&str, f64)]) -> TopicFingerprint {
        let clusters: Vec<TopicCluster> = keywords_and_weights
            .iter()
            .map(|(kw, w)| TopicCluster {
                label: kw.to_string(),
                keywords: vec![kw.to_string()],
                weight: *w,
            })
            .collect();
        TopicFingerprint {
            clusters,
            post_count: 100,
        }
    }

    #[test]
    fn test_identical_fingerprints() {
        let fp = make_fp(&[("fat", 0.3), ("queer", 0.2), ("dei", 0.15)]);
        let score = weighted_jaccard(&fp, &fp);
        assert!(
            (score - 1.0).abs() < 0.001,
            "Identical fingerprints should score ~1.0, got {score}"
        );
    }

    #[test]
    fn test_no_overlap() {
        let fp_a = make_fp(&[("fat", 0.3), ("queer", 0.2)]);
        let fp_b = make_fp(&[("sports", 0.4), ("gaming", 0.3)]);
        let score = weighted_jaccard(&fp_a, &fp_b);
        assert!(
            score < 0.001,
            "Non-overlapping fingerprints should score ~0.0, got {score}"
        );
    }

    #[test]
    fn test_partial_overlap() {
        let fp_a = make_fp(&[("fat", 0.3), ("queer", 0.2), ("dei", 0.15)]);
        let fp_b = make_fp(&[("fat", 0.2), ("gaming", 0.3), ("dei", 0.1)]);
        let score = weighted_jaccard(&fp_a, &fp_b);
        // Should be moderate — some shared topics
        assert!(score > 0.0, "Should have some overlap");
        assert!(score < 1.0, "Should not be identical");
    }

    #[test]
    fn test_empty_fingerprints() {
        let fp = make_fp(&[]);
        let score = weighted_jaccard(&fp, &fp);
        assert!(
            score.abs() < 0.001,
            "Empty fingerprints should score 0.0, got {score}"
        );
    }
}
