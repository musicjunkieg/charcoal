// Topic-first discovery — find accounts via searchPosts by topic keywords.
//
// The primary discovery mechanism for predictive defense. Instead of walking
// the follower graph (expensive, mostly irrelevant), search for posts about
// the protected user's topics and extract author DIDs.

use anyhow::{Context, Result};
use std::collections::HashSet;
use tracing::{debug, info};

use crate::bluesky::client::PublicAtpClient;
use crate::topics::fingerprint::TopicFingerprint;

/// Extract top N search keywords from a topic fingerprint.
///
/// Takes the first keyword from each cluster, sorted by cluster weight
/// (highest weight first). Clusters represent the user's primary topic
/// areas, and the first keyword in each is the most representative term.
/// Filters out very short keywords (< 3 chars).
pub fn extract_search_keywords(fingerprint: &TopicFingerprint, top_n: usize) -> Vec<String> {
    let mut clusters_sorted: Vec<_> = fingerprint.clusters.iter().collect();
    clusters_sorted.sort_by(|a, b| {
        b.weight
            .partial_cmp(&a.weight)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    clusters_sorted
        .iter()
        .flat_map(|cluster| {
            cluster
                .keywords
                .iter()
                .filter(|k| k.chars().count() >= 3)
                .take(1) // Top keyword per cluster
        })
        .take(top_n)
        .cloned()
        .collect()
}

/// Deduplicate author DIDs against already-scored accounts.
pub fn deduplicate_dids(raw_dids: &[String], already_scored: &HashSet<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    raw_dids
        .iter()
        .filter(|did| !already_scored.contains(did.as_str()) && seen.insert((*did).clone()))
        .cloned()
        .collect()
}

/// Search for posts matching a keyword and extract unique author DIDs.
///
/// Uses `app.bsky.feed.searchPosts` to find posts about a given topic,
/// then collects the author DIDs. Handles pagination up to max_results.
pub async fn search_posts_for_authors(
    client: &PublicAtpClient,
    query: &str,
    max_results: usize,
) -> Result<Vec<String>> {
    use atrium_api::app::bsky::feed::search_posts;

    let mut author_dids = Vec::new();
    let mut cursor: Option<String> = None;
    let limit = max_results.min(100).to_string();

    loop {
        let mut params: Vec<(&str, &str)> = vec![("q", query), ("limit", &limit)];
        if let Some(ref c) = cursor {
            params.push(("cursor", c));
        }

        let output: search_posts::Output = client
            .xrpc_get("app.bsky.feed.searchPosts", &params)
            .await
            .with_context(|| format!("searchPosts failed for query: {}", query))?;

        for post_view in &output.posts {
            author_dids.push(post_view.author.did.as_str().to_string());
        }

        debug!(
            query,
            page_results = output.posts.len(),
            total_authors = author_dids.len(),
            "searchPosts page"
        );

        if author_dids.len() >= max_results {
            break;
        }

        cursor = output.cursor.clone();
        if cursor.is_none() || output.posts.is_empty() {
            break;
        }
    }

    info!(
        query,
        unique_authors = author_dids.len(),
        "Collected author DIDs from search"
    );

    Ok(author_dids)
}

/// Run a topic-first discovery cycle.
///
/// Searches for posts matching top keywords from the fingerprint,
/// deduplicates against already-scored accounts, and returns new
/// author DIDs to score.
pub async fn discover_by_topic(
    client: &PublicAtpClient,
    fingerprint: &TopicFingerprint,
    already_scored: &HashSet<String>,
    keywords_per_cycle: usize,
    results_per_keyword: usize,
) -> Result<Vec<String>> {
    let keywords = extract_search_keywords(fingerprint, keywords_per_cycle);

    info!(keywords = ?keywords, "Running topic-first discovery cycle");

    let mut all_dids = Vec::new();
    for keyword in &keywords {
        match search_posts_for_authors(client, keyword, results_per_keyword).await {
            Ok(dids) => all_dids.extend(dids),
            Err(e) => {
                tracing::warn!(keyword, error = %e, "searchPosts failed, skipping keyword");
            }
        }
    }

    let new_dids = deduplicate_dids(&all_dids, already_scored);
    info!(
        raw = all_dids.len(),
        new = new_dids.len(),
        "Topic discovery: found new accounts to score"
    );

    Ok(new_dids)
}
