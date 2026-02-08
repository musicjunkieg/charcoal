// Profile builder — orchestrates scoring for a single account.
//
// Given a target account, this module:
// 1. Fetches their recent posts
// 2. Runs toxicity scoring on those posts
// 3. Builds their topic fingerprint
// 4. Computes topic overlap with the protected user
// 5. Calculates the combined threat score
// 6. Returns a complete AccountScore ready for storage

use anyhow::Result;
use tracing::info;

use crate::bluesky::posts::{self, Post};
use crate::db::models::{AccountScore, ToxicPost};
use crate::scoring::threat::{self, ThreatWeights};
use crate::topics::fingerprint::TopicFingerprint;
use crate::topics::overlap;
use crate::topics::tfidf::TfIdfExtractor;
use crate::topics::traits::TopicExtractor;
use crate::toxicity::traits::ToxicityScorer;

use bsky_sdk::BskyAgent;

/// Build a complete threat profile for a single account.
///
/// This is the core scoring function. It fetches the target's posts,
/// scores them for toxicity, extracts their topics, and computes the
/// combined threat score against the protected user's fingerprint.
pub async fn build_profile(
    agent: &BskyAgent,
    scorer: &dyn ToxicityScorer,
    target_handle: &str,
    target_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
) -> Result<AccountScore> {
    // Step 1: Fetch the target's recent posts (50 from API, keep up to 20 after filtering)
    let target_posts = posts::fetch_recent_posts(agent, target_handle, 20).await?;

    if target_posts.len() < 5 {
        info!(
            handle = target_handle,
            post_count = target_posts.len(),
            "Insufficient posts for reliable scoring"
        );
        return Ok(AccountScore {
            did: target_did.to_string(),
            handle: target_handle.to_string(),
            toxicity_score: None,
            topic_overlap: None,
            threat_score: None,
            threat_tier: Some("Insufficient Data".to_string()),
            posts_analyzed: target_posts.len() as u32,
            top_toxic_posts: vec![],
            scored_at: String::new(),
        });
    }

    // Step 2: Score posts for toxicity
    let post_texts: Vec<String> = target_posts.iter().map(|p| p.text.clone()).collect();
    let toxicity_results = scorer.score_batch(&post_texts).await?;

    // Calculate average toxicity
    let avg_toxicity: f64 = if toxicity_results.is_empty() {
        0.0
    } else {
        toxicity_results.iter().map(|r| r.toxicity).sum::<f64>() / toxicity_results.len() as f64
    };

    // Collect the top 3 most toxic posts as evidence
    let mut scored_posts: Vec<(&Post, f64)> = target_posts
        .iter()
        .zip(toxicity_results.iter().map(|r| r.toxicity))
        .collect();
    scored_posts.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let top_toxic_posts: Vec<ToxicPost> = scored_posts
        .iter()
        .take(3)
        .map(|(post, score)| ToxicPost {
            text: post.text.clone(),
            toxicity: *score,
            uri: post.uri.clone(),
        })
        .collect();

    // Step 3: Build the target's topic fingerprint
    let topic_extractor = TfIdfExtractor {
        top_n_keywords: 30,
        max_clusters: 5,
    };
    let target_fingerprint = topic_extractor.extract(&post_texts)?;

    // Step 4: Compute topic overlap with the protected user
    let topic_overlap = overlap::weighted_jaccard(protected_fingerprint, &target_fingerprint);

    // Step 5: Compute the combined threat score
    let (threat_score, tier) = threat::compute_threat_score(avg_toxicity, topic_overlap, weights);

    info!(
        handle = target_handle,
        toxicity = format!("{:.2}", avg_toxicity),
        overlap = format!("{:.2}", topic_overlap),
        threat = format!("{:.1}", threat_score),
        tier = tier.as_str(),
        posts = target_posts.len(),
        "Scored account"
    );

    Ok(AccountScore {
        did: target_did.to_string(),
        handle: target_handle.to_string(),
        toxicity_score: Some(avg_toxicity),
        topic_overlap: Some(topic_overlap),
        threat_score: Some(threat_score),
        threat_tier: Some(tier.to_string()),
        posts_analyzed: target_posts.len() as u32,
        top_toxic_posts,
        scored_at: String::new(),
    })
}
