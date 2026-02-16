// Cosine similarity for topic overlap scoring.
//
// Compares two topic fingerprints by treating their keyword weight maps as
// sparse vectors and computing cosine similarity:
//
//   dot(A, B) / (||A|| * ||B||)
//
// Unlike weighted Jaccard, cosine similarity is insensitive to the magnitude
// of the vectors — it measures the angle between them. This means a fingerprint
// built from 500 posts and one built from 20 posts can still produce meaningful
// overlap scores, as long as the keywords that DO overlap point in the same
// direction.
//
// Returns 0.0 for no overlap and 1.0 for identical topic profiles.

use std::collections::HashMap;

use super::fingerprint::TopicFingerprint;

/// Compute the cosine similarity between two fingerprints.
///
/// Returns a score from 0.0 (no overlap) to 1.0 (identical topic profiles).
pub fn cosine_similarity(fp_a: &TopicFingerprint, fp_b: &TopicFingerprint) -> f64 {
    let weights_a = fp_a.keyword_weights();
    let weights_b = fp_b.keyword_weights();

    cosine_from_weights(&weights_a, &weights_b)
}

/// Compute cosine similarity from raw keyword weight maps.
///
/// Treats each map as a sparse vector over the union of all keywords.
/// Separated from `cosine_similarity` so it can be used with ad-hoc weight
/// maps (e.g., from a single account's posts without full clustering).
pub fn cosine_from_weights(
    weights_a: &HashMap<String, f64>,
    weights_b: &HashMap<String, f64>,
) -> f64 {
    if weights_a.is_empty() || weights_b.is_empty() {
        return 0.0;
    }

    // Dot product: only non-zero where both vectors have the same keyword
    let dot: f64 = weights_a
        .iter()
        .filter_map(|(key, &a)| weights_b.get(key).map(|&b| a * b))
        .sum();

    // Magnitudes
    let mag_a: f64 = weights_a.values().map(|v| v * v).sum::<f64>().sqrt();
    let mag_b: f64 = weights_b.values().map(|v| v * v).sum::<f64>().sqrt();

    let denominator = mag_a * mag_b;
    if denominator == 0.0 {
        0.0
    } else {
        (dot / denominator).clamp(0.0, 1.0)
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
        let score = cosine_similarity(&fp, &fp);
        assert!(
            (score - 1.0).abs() < 0.001,
            "Identical fingerprints should score ~1.0, got {score}"
        );
    }

    #[test]
    fn test_no_overlap() {
        let fp_a = make_fp(&[("fat", 0.3), ("queer", 0.2)]);
        let fp_b = make_fp(&[("sports", 0.4), ("gaming", 0.3)]);
        let score = cosine_similarity(&fp_a, &fp_b);
        assert!(
            score < 0.001,
            "Non-overlapping fingerprints should score ~0.0, got {score}"
        );
    }

    #[test]
    fn test_partial_overlap() {
        let fp_a = make_fp(&[("fat", 0.3), ("queer", 0.2), ("dei", 0.15)]);
        let fp_b = make_fp(&[("fat", 0.2), ("gaming", 0.3), ("dei", 0.1)]);
        let score = cosine_similarity(&fp_a, &fp_b);
        // Cosine similarity should be moderate — shared keywords point
        // in similar directions even though magnitudes differ
        assert!(score > 0.1, "Should have meaningful overlap, got {score}");
        assert!(score < 1.0, "Should not be identical");
    }

    #[test]
    fn test_empty_fingerprints() {
        let fp = make_fp(&[]);
        let score = cosine_similarity(&fp, &fp);
        assert!(
            score.abs() < 0.001,
            "Empty fingerprints should score 0.0, got {score}"
        );
    }

    #[test]
    fn test_cosine_insensitive_to_magnitude() {
        // Same keywords, different magnitudes — cosine should still be ~1.0
        let fp_a = make_fp(&[("fat", 0.5), ("queer", 0.3), ("dei", 0.2)]);
        let fp_b = make_fp(&[("fat", 0.1), ("queer", 0.06), ("dei", 0.04)]);
        let score = cosine_similarity(&fp_a, &fp_b);
        assert!(
            (score - 1.0).abs() < 0.001,
            "Proportional weights should score ~1.0, got {score}"
        );
    }
}
