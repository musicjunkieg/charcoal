// TF-IDF keyword extraction implementation.
//
// Uses the `keyword_extraction` crate to extract keywords from a set of posts,
// then clusters co-occurring keywords into human-readable topic groups.
//
// Each post is treated as a separate document for IDF computation — words that
// appear in every post get downweighted, while words that are distinctive to
// certain posts get boosted. This is exactly what we want for topic detection.

use std::sync::LazyLock;

use anyhow::Result;
use keyword_extraction::tf_idf::{TfIdf, TfIdfParams};
use stop_words::{get, LANGUAGE};
use tracing::info;

/// Pre-compiled regex patterns for post cleaning. Using LazyLock ensures each
/// pattern is compiled exactly once (on first use) rather than on every call
/// to clean_post(). With 500 posts this avoids ~1500 redundant compilations.
static URL_PATTERN: LazyLock<regex_lite::Regex> =
    LazyLock::new(|| regex_lite::Regex::new(r"https?://\S+").unwrap());
static MENTION_PATTERN: LazyLock<regex_lite::Regex> =
    LazyLock::new(|| regex_lite::Regex::new(r"@[\w.]+").unwrap());
static WHITESPACE_PATTERN: LazyLock<regex_lite::Regex> =
    LazyLock::new(|| regex_lite::Regex::new(r"\s+").unwrap());

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

        // Pre-process posts: normalize unicode, strip URLs, expand contractions
        let cleaned: Vec<String> = posts.iter().map(|p| clean_post(p)).collect();

        // Build stop words list: English defaults + social media extras
        let mut stop_words: Vec<String> = get(LANGUAGE::English);
        stop_words.extend(extra_stop_words().into_iter().map(String::from));

        // Run TF-IDF with each post as a separate document.
        // The library handles tokenization and scoring.
        let params = TfIdfParams::UnprocessedDocuments(&cleaned, &stop_words, None);
        let tfidf = TfIdf::new(params);

        // Get the top keywords with their scores, filtering out junk
        let ranked: Vec<(String, f32)> = tfidf
            .get_ranked_word_scores(self.top_n_keywords * 2) // grab extra to filter from
            .into_iter()
            .filter(|(word, _)| is_meaningful_keyword(word))
            .take(self.top_n_keywords)
            .collect();

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
        let clusters = cluster_keywords(&ranked, &cleaned, self.max_clusters);

        Ok(TopicFingerprint {
            clusters,
            post_count: posts.len() as u32,
        })
    }
}

/// Clean a post for TF-IDF analysis.
///
/// Normalizes smart quotes, strips URLs/mentions/hashtags, lowercases,
/// and removes non-alphabetic noise. This dramatically improves keyword
/// quality on real social media text.
fn clean_post(text: &str) -> String {
    let mut cleaned = text.to_string();

    // Normalize smart quotes and other unicode punctuation to ASCII
    cleaned = cleaned
        .replace(['\u{201C}', '\u{201D}'], "\"") // " "
        .replace(['\u{2018}', '\u{2019}'], "'")  // ' '
        .replace(['\u{2014}', '\u{2013}', '\u{2026}'], " "); // em/en dash, ellipsis

    // Expand common contractions so the real words survive stop word filtering
    let contractions = [
        ("don't", "do not"), ("doesn't", "does not"), ("didn't", "did not"),
        ("can't", "cannot"), ("won't", "will not"), ("wouldn't", "would not"),
        ("couldn't", "could not"), ("shouldn't", "should not"),
        ("isn't", "is not"), ("aren't", "are not"), ("wasn't", "was not"),
        ("weren't", "were not"), ("hasn't", "has not"), ("haven't", "have not"),
        ("hadn't", "had not"), ("i'm", "i am"), ("i've", "i have"),
        ("i'll", "i will"), ("i'd", "i would"), ("it's", "it is"),
        ("that's", "that is"), ("there's", "there is"), ("they're", "they are"),
        ("they've", "they have"), ("they'll", "they will"),
        ("we're", "we are"), ("we've", "we have"), ("we'll", "we will"),
        ("you're", "you are"), ("you've", "you have"), ("you'll", "you will"),
        ("who's", "who is"), ("what's", "what is"),
    ];
    let lower = cleaned.to_lowercase();
    for (contraction, expansion) in &contractions {
        if lower.contains(contraction) {
            // Case-insensitive replace
            cleaned = cleaned
                .to_lowercase()
                .replace(contraction, expansion);
        }
    }

    // Strip URLs (http/https links)
    cleaned = URL_PATTERN.replace_all(&cleaned, " ").to_string();

    // Strip @mentions
    cleaned = MENTION_PATTERN.replace_all(&cleaned, " ").to_string();

    // Remove non-letter characters (keep spaces and basic letters)
    cleaned = cleaned
        .chars()
        .map(|c| if c.is_alphabetic() || c == ' ' { c } else { ' ' })
        .collect();

    // Collapse multiple spaces
    cleaned = WHITESPACE_PATTERN.replace_all(&cleaned, " ").trim().to_string();

    cleaned
}

/// Additional stop words for social media text.
///
/// The standard English stop word list misses many common social media words
/// and fragments that aren't meaningful for topic detection.
fn extra_stop_words() -> Vec<&'static str> {
    vec![
        // Common social media / conversational words
        "just", "like", "really", "actually", "literally", "basically",
        "pretty", "also", "even", "still", "much", "way", "thing",
        "things", "lot", "lot's", "gonna", "gotta", "wanna",
        "yeah", "yes", "no", "oh", "ok", "okay", "lol", "lmao",
        "hey", "hi", "hello", "thanks", "thank", "please",
        // Pronouns / determiners the base list might miss
        "something", "anything", "everything", "nothing",
        "someone", "anyone", "everyone", "nobody",
        "here", "there", "where", "when", "how", "why", "what",
        "this", "that", "these", "those", "every", "each",
        // Verbs too common to be meaningful
        "get", "got", "getting", "make", "made", "making",
        "go", "going", "went", "gone", "come", "coming", "came",
        "know", "known", "knowing", "think", "thought", "thinking",
        "see", "seen", "seeing", "look", "looking", "looked",
        "want", "wanted", "wanting", "need", "needed", "needing",
        "say", "said", "saying", "tell", "told", "telling",
        "take", "took", "taking", "give", "gave", "giving",
        "feel", "felt", "feeling", "keep", "kept", "keeping",
        "let", "put", "try", "trying", "tried",
        "use", "used", "using", "work", "working", "worked",
        "call", "called", "find", "found",
        // Time words
        "now", "today", "time", "always", "never", "already",
        "often", "sometimes", "usually", "ever",
        // Quantity / degree
        "many", "more", "most", "less", "few", "very",
        "too", "enough", "really", "quite",
        // Other noise
        "new", "old", "good", "bad", "big", "great",
        "first", "last", "next", "back", "right", "long",
        "own", "same", "different", "able", "whole",
        "well", "away", "sure", "kind", "sort",
        // Bluesky-specific
        "post", "thread", "quote", "repost", "reply",
        "follow", "block", "mute", "feed",
        // Generic social/conversational words that survive other filters
        "people", "person", "folks", "guys", "everyone",
        "understand", "believe", "agree", "disagree",
        "love", "hate", "hope", "wish", "care",
        "point", "part", "case", "fact", "idea",
        "talk", "talking", "talked", "read", "reading",
        "write", "writing", "wrote", "start", "stop",
        "happen", "happened", "happening", "change", "changed",
        "means", "mean", "meant", "true", "real",
        "live", "life", "world", "place", "home",
        "day", "week", "month", "year", "years",
        "hard", "easy", "small", "high", "low",
        "best", "worst", "better", "worse",
        "stuff", "bit", "ones", "isn", "don",
    ]
}

/// Check if a keyword is meaningful enough to include in the fingerprint.
///
/// Filters out single characters, pure numbers, and other junk that
/// survives stop word filtering.
fn is_meaningful_keyword(word: &str) -> bool {
    // Must be at least 3 characters
    if word.len() < 3 {
        return false;
    }
    // Must contain at least one letter
    if !word.chars().any(|c| c.is_alphabetic()) {
        return false;
    }
    // Skip pure numbers
    if word.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    true
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
    let keywords: Vec<&str> = ranked.iter().map(|(w, _)| w.as_str()).collect();

    // For each post, record which keywords appear in it (word boundary aware)
    let post_keywords: Vec<Vec<usize>> = posts
        .iter()
        .map(|post| {
            let lower = post.to_lowercase();
            let words: Vec<&str> = lower.split_whitespace().collect();
            keywords
                .iter()
                .enumerate()
                .filter(|(_, kw)| words.contains(kw))
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

    let total_score: f32 = ranked.iter().map(|(_, s)| s).sum();

    for seed_idx in 0..n {
        if clusters.len() >= max_clusters {
            break;
        }
        if assigned[seed_idx] {
            continue;
        }

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

    clusters.sort_by(|a, b| b.weight.partial_cmp(&a.weight).unwrap_or(std::cmp::Ordering::Equal));

    clusters
}

/// Generate a human-readable label from a cluster's top keywords.
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

    #[test]
    fn test_clean_post() {
        let raw = "Don\u{2019}t let them tell you \u{201C}fat is unhealthy\u{201D} https://example.com/link @someone.bsky.social #bodypositivity";
        let cleaned = clean_post(raw);
        assert!(!cleaned.contains("https"));
        assert!(!cleaned.contains("@"));
        assert!(!cleaned.contains('\u{201C}'));
        assert!(!cleaned.contains('\u{2019}'));
        // Contraction should be expanded
        assert!(cleaned.contains("not"));
    }

    #[test]
    fn test_is_meaningful_keyword() {
        assert!(is_meaningful_keyword("fat"));
        assert!(is_meaningful_keyword("liberation"));
        assert!(!is_meaningful_keyword("a"));
        assert!(!is_meaningful_keyword("42"));
        assert!(!is_meaningful_keyword(""));
    }
}
