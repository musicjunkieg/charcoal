// TopicFingerprint — the structured representation of what someone talks about.
//
// A fingerprint is a list of topic clusters, each with a label, a set of
// keywords, and a weight indicating how prominent that topic is in the
// person's posting history.

use colored::Colorize;
use serde::{Deserialize, Serialize};

/// A complete topic fingerprint for an account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicFingerprint {
    /// Ranked list of topic clusters (highest weight first)
    pub clusters: Vec<TopicCluster>,
    /// Total number of posts analyzed to build this fingerprint
    pub post_count: u32,
}

/// A single topic cluster — a group of related keywords with a label.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopicCluster {
    /// Human-readable label for this topic area
    pub label: String,
    /// The keywords that make up this cluster, in descending score order
    pub keywords: Vec<String>,
    /// TF-IDF scores aligned with `keywords`. Empty for fingerprints
    /// serialized before #296 — `keyword_weights()` falls back to a
    /// uniform split so stored JSON keeps working.
    #[serde(default)]
    pub keyword_scores: Vec<f64>,
    /// Normalized weight (0.0 to 1.0) representing how much of the person's
    /// posting is about this topic
    pub weight: f64,
}

impl TopicFingerprint {
    /// Display the fingerprint as a formatted bar chart in the terminal.
    ///
    /// This is the output Bryan sees when running `charcoal fingerprint` —
    /// it should be scannable and help him validate whether the system
    /// understands his topic profile correctly.
    pub fn display(&self) {
        println!(
            "\n{}",
            format!(
                "=== Your Topic Fingerprint (based on {} recent posts) ===",
                self.post_count
            )
            .bold()
        );
        println!();

        let bar_width: usize = 20;

        for (i, cluster) in self.clusters.iter().enumerate() {
            // Build the bar: filled portion + empty portion
            let filled = (cluster.weight * bar_width as f64).round() as usize;
            let empty = bar_width.saturating_sub(filled);
            let bar = format!("[{}{}]", "=".repeat(filled), " ".repeat(empty));

            // Color the bar based on weight
            let colored_bar = if cluster.weight >= 0.25 {
                bar.bright_green()
            } else if cluster.weight >= 0.10 {
                bar.bright_yellow()
            } else {
                bar.bright_blue()
            };

            println!(
                "  {:>2}. {:<40} {} {:.2}",
                i + 1,
                cluster.label.bold(),
                colored_bar,
                cluster.weight
            );

            // Show the keywords below the bar
            let keywords_str = cluster.keywords.join(", ");
            println!("      Keywords: {}", keywords_str.dimmed());
            println!();
        }
    }

    /// Get the keyword weights as a flat map (keyword -> weight).
    /// Used for computing topic overlap between two accounts.
    ///
    /// Within a cluster, weight is distributed by TF-IDF score share, so the
    /// seed keyword outweighs its co-occurring tail. (#296, spike #295
    /// defect 6.) Falls back to a uniform split when scores are absent
    /// (legacy JSON) or malformed.
    pub fn keyword_weights(&self) -> std::collections::HashMap<String, f64> {
        let mut weights = std::collections::HashMap::new();
        for cluster in &self.clusters {
            let score_sum: f64 = cluster.keyword_scores.iter().sum();
            // Every score must be finite and non-negative: a stored JSON with
            // e.g. [-1.0, 2.0] sums positive but would mint negative keyword
            // weights, and NaN/inf poison the shares. Malformed → uniform.
            // (#301, CodeRabbit PR #101)
            let rank_weighted = cluster.keyword_scores.len() == cluster.keywords.len()
                && cluster
                    .keyword_scores
                    .iter()
                    .all(|s| s.is_finite() && *s >= 0.0)
                && score_sum.is_finite()
                && score_sum > f64::EPSILON;

            if rank_weighted {
                for (keyword, &score) in cluster.keywords.iter().zip(&cluster.keyword_scores) {
                    *weights.entry(keyword.clone()).or_insert(0.0) +=
                        cluster.weight * score / score_sum;
                }
            } else {
                let per_keyword = cluster.weight / cluster.keywords.len().max(1) as f64;
                for keyword in &cluster.keywords {
                    *weights.entry(keyword.clone()).or_insert(0.0) += per_keyword;
                }
            }
        }
        weights
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keyword_weights() {
        let fp = TopicFingerprint {
            clusters: vec![
                TopicCluster {
                    label: "Test Topic".to_string(),
                    keywords: vec!["a".to_string(), "b".to_string()],
                    keyword_scores: vec![],
                    weight: 0.6,
                },
                TopicCluster {
                    label: "Other".to_string(),
                    keywords: vec!["c".to_string()],
                    keyword_scores: vec![],
                    weight: 0.4,
                },
            ],
            post_count: 100,
        };

        let weights = fp.keyword_weights();
        assert!((weights["a"] - 0.3).abs() < 0.001);
        assert!((weights["b"] - 0.3).abs() < 0.001);
        assert!((weights["c"] - 0.4).abs() < 0.001);
    }

    #[test]
    fn test_keyword_weights_rank_weighted() {
        let fp = TopicFingerprint {
            clusters: vec![TopicCluster {
                label: "t".to_string(),
                keywords: vec!["seed".to_string(), "neighbor".to_string()],
                keyword_scores: vec![3.0, 1.0],
                weight: 0.8,
            }],
            post_count: 100,
        };
        let weights = fp.keyword_weights();
        // Cluster weight 0.8 split by score share: seed 3/4, neighbor 1/4
        assert!((weights["seed"] - 0.6).abs() < 1e-9);
        assert!((weights["neighbor"] - 0.2).abs() < 1e-9);
    }

    #[test]
    fn test_keyword_weights_uniform_fallback_for_legacy_json() {
        // A fingerprint serialized before keyword_scores existed
        let json =
            r#"{"clusters":[{"label":"t","keywords":["a","b"],"weight":0.6}],"post_count":10}"#;
        let fp: TopicFingerprint =
            serde_json::from_str(json).expect("legacy JSON must deserialize");
        let weights = fp.keyword_weights();
        assert!((weights["a"] - 0.3).abs() < 1e-9);
        assert!((weights["b"] - 0.3).abs() < 1e-9);
    }

    #[test]
    fn test_keyword_weights_negative_scores_fall_back_to_uniform() {
        // [-1.0, 2.0] sums positive but would mint a negative weight for
        // one keyword and an oversized weight for the other. Malformed
        // scores must take the uniform path. (#301, CodeRabbit PR #101)
        let fp = TopicFingerprint {
            clusters: vec![TopicCluster {
                label: "t".to_string(),
                keywords: vec!["a".to_string(), "b".to_string()],
                keyword_scores: vec![-1.0, 2.0],
                weight: 0.6,
            }],
            post_count: 10,
        };
        let weights = fp.keyword_weights();
        assert!((weights["a"] - 0.3).abs() < 1e-9);
        assert!((weights["b"] - 0.3).abs() < 1e-9);
    }

    #[test]
    fn test_keyword_weights_non_finite_scores_fall_back_to_uniform() {
        // NaN poisons every arithmetic comparison downstream; infinity
        // would zero out every other keyword's share. Both take uniform.
        for bad in [f64::NAN, f64::INFINITY] {
            let fp = TopicFingerprint {
                clusters: vec![TopicCluster {
                    label: "t".to_string(),
                    keywords: vec!["a".to_string(), "b".to_string()],
                    keyword_scores: vec![bad, 1.0],
                    weight: 0.6,
                }],
                post_count: 10,
            };
            let weights = fp.keyword_weights();
            assert!(
                (weights["a"] - 0.3).abs() < 1e-9 && (weights["b"] - 0.3).abs() < 1e-9,
                "score {bad} must trigger the uniform fallback"
            );
        }
    }

    #[test]
    fn test_keyword_weights_overflowing_sum_falls_back_to_uniform() {
        // Each individual score is finite (f64::MAX passes is_finite()), but
        // their sum overflows to +inf. That passes the per-element check and
        // score_sum > EPSILON, so without an explicit score_sum.is_finite()
        // guard every keyword's share (score / score_sum) evaluates to 0.0
        // instead of falling back to the documented uniform split. (#307,
        // CodeRabbit PR #103)
        let fp = TopicFingerprint {
            clusters: vec![TopicCluster {
                label: "t".to_string(),
                keywords: vec!["a".to_string(), "b".to_string()],
                keyword_scores: vec![f64::MAX, f64::MAX],
                weight: 0.6,
            }],
            post_count: 10,
        };
        let weights = fp.keyword_weights();
        assert!(
            (weights["a"] - 0.3).abs() < 1e-9 && (weights["b"] - 0.3).abs() < 1e-9,
            "an overflowing score_sum must trigger the uniform fallback, got {weights:?}"
        );
    }

    #[test]
    fn test_keyword_weights_mismatched_scores_fall_back_to_uniform() {
        let fp = TopicFingerprint {
            clusters: vec![TopicCluster {
                label: "t".to_string(),
                keywords: vec!["a".to_string(), "b".to_string()],
                keyword_scores: vec![1.0], // wrong length — defensive fallback
                weight: 0.6,
            }],
            post_count: 10,
        };
        let weights = fp.keyword_weights();
        assert!((weights["a"] - 0.3).abs() < 1e-9);
        assert!((weights["b"] - 0.3).abs() < 1e-9);
    }
}
