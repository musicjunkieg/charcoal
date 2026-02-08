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
            let bar = format!(
                "[{}{}]",
                "=".repeat(filled),
                " ".repeat(empty)
            );

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
    pub fn keyword_weights(&self) -> std::collections::HashMap<String, f64> {
        let mut weights = std::collections::HashMap::new();
        for cluster in &self.clusters {
            // Each keyword in a cluster gets the cluster's weight,
            // distributed evenly among the keywords
            let per_keyword = cluster.weight / cluster.keywords.len().max(1) as f64;
            for keyword in &cluster.keywords {
                *weights.entry(keyword.clone()).or_insert(0.0) += per_keyword;
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
                    weight: 0.6,
                },
                TopicCluster {
                    label: "Other".to_string(),
                    keywords: vec!["c".to_string()],
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
}
