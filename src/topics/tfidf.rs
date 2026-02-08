// TF-IDF keyword extraction implementation.
//
// Uses the `keyword_extraction` crate to extract keywords from a set of posts,
// then clusters co-occurring keywords into human-readable topic groups.
//
// Each post is treated as a separate document for IDF computation — words that
// appear in every post get downweighted, while words that are distinctive to
// certain posts get boosted. This is exactly what we want for topic detection.

use anyhow::Result;
use keyword_extraction::tf_idf::{TfIdf, TfIdfParams};
use stop_words::{get, LANGUAGE};
use tracing::info;

use super::fingerprint::{TopicCluster, TopicFingerprint};
use super::traits::TopicExtractor;

/// TF-IDF based topic extractor — the default for the MVP.
///
/// Zero API calls, runs locally, no cost. Can be swapped for an
/// embeddings-based approach later via the TopicExtractor trait.
pub struct TfIdfExtractor {
    /// How many top keywords to extract before clustering
    pub top_n_keywords: usize,
    /// How many topic clusters to produce in the fingerprint
    pub max_clusters: usize,
}

impl Default for TfIdfExtractor {
    fn default() -> Self {
        Self {
            top_n_keywords: 60,
            max_clusters: 10,
        }
    }
}

impl TopicExtractor for TfIdfExtractor {
    fn extract(&self, posts: &[String]) -> Result<TopicFingerprint> {
        if posts.is_empty() {
            anyhow::bail!("No posts to analyze — cannot build a topic fingerprint");
        }

        // Get English stop words from the stop-words crate
        let stop_words: Vec<String> = get(LANGUAGE::English);

        // Run TF-IDF with each post as a separate document.
        // The library handles tokenization, stop word removal, and scoring.
        let params = TfIdfParams::UnprocessedDocuments(posts, &stop_words, None);
        let tfidf = TfIdf::new(params);

        // Get the top keywords with their scores
        let ranked: Vec<(String, f32)> = tfidf.get_ranked_word_scores(self.top_n_keywords);

        if ranked.is_empty() {
            anyhow::bail!(
                "TF-IDF produced no keywords from {} posts — posts may be too short or uniform",
                posts.len()
            );
        }

        info!(
            keywords = ranked.len(),
            top_keyword = &ranked[0].0,
            top_score = ranked[0].1,
            "Extracted TF-IDF keywords"
        );

        // Cluster keywords into topic groups using simple co-occurrence.
        // Two keywords belong to the same cluster if they frequently appear
        // in the same posts.
        let clusters = cluster_keywords(&ranked, posts, self.max_clusters);

        Ok(TopicFingerprint {
            clusters,
            post_count: posts.len() as u32,
        })
    }
}

/// Group keywords into topic clusters based on co-occurrence in posts.
///
/// Strategy: for each pair of keywords, count how often they appear in the
/// same post. Then greedily build clusters by starting with the highest-scored
/// keyword and pulling in its most co-occurring neighbors.
fn cluster_keywords(
    ranked: &[(String, f32)],
    posts: &[String],
    max_clusters: usize,
) -> Vec<TopicCluster> {
    // Build a co-occurrence matrix: for each keyword pair, how many posts
    // contain both keywords?
    let keywords: Vec<&str> = ranked.iter().map(|(w, _)| w.as_str()).collect();

    // For each post, record which keywords appear in it
    let post_keywords: Vec<Vec<usize>> = posts
        .iter()
        .map(|post| {
            let lower = post.to_lowercase();
            keywords
                .iter()
                .enumerate()
                .filter(|(_, kw)| lower.contains(*kw))
                .map(|(i, _)| i)
                .collect()
        })
        .collect();

    // Count co-occurrences
    let n = keywords.len();
    let mut cooccurrence = vec![vec![0u32; n]; n];
    for pk in &post_keywords {
        for &i in pk {
            for &j in pk {
                if i != j {
                    cooccurrence[i][j] += 1;
                }
            }
        }
    }

    // Greedy clustering: start from the highest-scored unclustered keyword,
    // pull in its top co-occurring keywords that aren't yet assigned
    let mut assigned = vec![false; n];
    let mut clusters = Vec::new();

    // Total score for normalization
    let total_score: f32 = ranked.iter().map(|(_, s)| s).sum();

    for seed_idx in 0..n {
        if assigned[seed_idx] || clusters.len() >= max_clusters {
            break;
        }

        // Start a new cluster with this keyword as seed
        assigned[seed_idx] = true;
        let mut cluster_indices = vec![seed_idx];
        let mut cluster_score = ranked[seed_idx].1;

        // Find the top co-occurring unassigned keywords
        let mut candidates: Vec<(usize, u32)> = (0..n)
            .filter(|&i| !assigned[i] && cooccurrence[seed_idx][i] > 0)
            .map(|i| (i, cooccurrence[seed_idx][i]))
            .collect();
        candidates.sort_by(|a, b| b.1.cmp(&a.1));

        // Pull in up to 5 related keywords per cluster
        for (idx, _count) in candidates.into_iter().take(5) {
            assigned[idx] = true;
            cluster_score += ranked[idx].1;
            cluster_indices.push(idx);
        }

        let cluster_keywords: Vec<String> = cluster_indices
            .iter()
            .map(|&i| ranked[i].0.clone())
            .collect();

        // Generate a label from the top 2-3 keywords
        let label = generate_cluster_label(&cluster_keywords);

        let weight = if total_score > 0.0 {
            (cluster_score / total_score) as f64
        } else {
            0.0
        };

        clusters.push(TopicCluster {
            label,
            keywords: cluster_keywords,
            weight,
        });
    }

    // Normalize weights so they sum to 1.0
    let weight_sum: f64 = clusters.iter().map(|c| c.weight).sum();
    if weight_sum > 0.0 {
        for cluster in &mut clusters {
            cluster.weight /= weight_sum;
        }
    }

    // Sort by weight descending
    clusters.sort_by(|a, b| b.weight.partial_cmp(&a.weight).unwrap_or(std::cmp::Ordering::Equal));

    clusters
}

/// Generate a human-readable label from a cluster's top keywords.
///
/// Takes the first 2-3 keywords and joins them with " / ". In a more
/// sophisticated version, this could use an LLM to generate better labels.
fn generate_cluster_label(keywords: &[String]) -> String {
    let label_words: Vec<&str> = keywords.iter().take(3).map(|s| s.as_str()).collect();
    label_words.join(" / ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_basic() {
        let extractor = TfIdfExtractor {
            top_n_keywords: 20,
            max_clusters: 5,
        };

        let posts = vec![
            "Fat liberation is a civil rights movement that challenges weight stigma and diet culture".to_string(),
            "The body positivity community continues to fight against fatphobia in healthcare".to_string(),
            "Trans rights are human rights and queer identity deserves celebration".to_string(),
            "Community governance requires trust accountability and transparent moderation".to_string(),
            "Building inclusive spaces means centering marginalized voices in decision making".to_string(),
            "Weight stigma in medical settings causes real harm to fat patients seeking care".to_string(),
            "Queer joy is resistance and trans visibility matters in public discourse".to_string(),
            "DEI programs face backlash but equity work remains essential for justice".to_string(),
            "Atlassian Forge development requires understanding the app platform deeply".to_string(),
            "Community moderation is cybernetics applied to social systems governance".to_string(),
        ];

        let fingerprint = extractor.extract(&posts).unwrap();

        assert!(!fingerprint.clusters.is_empty());
        assert!(fingerprint.clusters.len() <= 5);
        assert_eq!(fingerprint.post_count, 10);

        // Weights should sum to approximately 1.0
        let weight_sum: f64 = fingerprint.clusters.iter().map(|c| c.weight).sum();
        assert!((weight_sum - 1.0).abs() < 0.01, "Weights sum to {weight_sum}");
    }

    #[test]
    fn test_extract_empty_fails() {
        let extractor = TfIdfExtractor::default();
        let result = extractor.extract(&[]);
        assert!(result.is_err());
    }
}
