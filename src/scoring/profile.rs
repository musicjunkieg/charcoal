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
use tracing::{info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::posts::{self, Post};
use crate::db::models::{AccountScore, ToxicPost};
use crate::scoring::behavioral;
use crate::scoring::nli::NliScorer;
use crate::scoring::threat::{self, ThreatWeights};
use crate::topics::embeddings::{self, SentenceEmbedder};
use crate::topics::fingerprint::TopicFingerprint;
use crate::topics::overlap;
use crate::topics::tfidf::TfIdfExtractor;
use crate::topics::traits::TopicExtractor;
use crate::toxicity::traits::{ToxicityResult, ToxicityScorer};

/// Build a complete threat profile for a single account.
///
/// This is the core scoring function. It fetches the target's posts,
/// scores them for toxicity, extracts their topics, and computes the
/// combined threat score against the protected user's fingerprint.
///
/// When `embedder` and `protected_embedding` are provided, topic overlap
/// is computed using sentence embeddings (semantic similarity). Otherwise,
/// falls back to TF-IDF keyword cosine similarity.
#[allow(clippy::too_many_arguments)]
pub async fn build_profile(
    client: &PublicAtpClient,
    scorer: &dyn ToxicityScorer,
    target_handle: &str,
    target_did: &str,
    protected_fingerprint: &TopicFingerprint,
    weights: &ThreatWeights,
    embedder: Option<&SentenceEmbedder>,
    protected_embedding: Option<&[f64]>,
    median_engagement: f64,
    pile_on_dids: &std::collections::HashSet<String>,
    nli_scorer: Option<&NliScorer>,
    protected_posts_with_embeddings: Option<&[(String, Vec<f64>)]>,
    direct_pairs: Option<&[(String, String)]>,
    data_dir: Option<&std::path::Path>,
) -> Result<AccountScore> {
    // Step 1: Fetch the target's recent posts (up to 50 for stable TF-IDF fingerprints)
    let target_posts = posts::fetch_recent_posts(client, target_handle, 50).await?;

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
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
        });
    }

    // Step 2: Score posts for toxicity
    let post_texts: Vec<String> = target_posts.iter().map(|p| p.text.clone()).collect();
    let toxicity_results = scorer.score_batch(&post_texts).await?;

    // Calculate weighted toxicity that emphasizes hostile intent over profanity.
    //
    // The raw `toxicity` score treats all toxicity equally — but "fuck yeah,
    // fat liberation!" (high obscene, low identity_attack) is very different
    // from "fat people are disgusting" (high identity_attack, high insult).
    // We weight the categories to surface genuine hostility:
    //   identity_attack: 0.35 — directly targets people for who they are
    //   insult:          0.25 — hostile personal attacks
    //   threat:          0.25 — threatening language
    //   severe_toxicity: 0.10 — extreme toxicity signal
    //   profanity:       0.05 — swearing alone is not hostility
    let avg_toxicity: f64 = if toxicity_results.is_empty() {
        0.0
    } else {
        let sum: f64 = toxicity_results.iter().map(weighted_toxicity).sum();
        sum / toxicity_results.len() as f64
    };

    // Collect the top 3 most toxic posts as evidence.
    // Sort by weighted_toxicity (which drives the threat score) so the evidence
    // shown to the user matches what actually determined the tier — not the raw
    // model toxicity score, which can be misleading for ally-style profanity.
    let mut scored_posts: Vec<(&Post, f64)> = target_posts
        .iter()
        .zip(toxicity_results.iter().map(weighted_toxicity))
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

    // Step 3: Compute topic overlap with the protected user.
    //
    // Prefer sentence embeddings when available — they capture semantic
    // similarity ("fatphobia" ≈ "obesity") that keyword matching misses.
    // Fall back to TF-IDF keyword cosine when the embedding model isn't loaded.
    let topic_overlap = if let (Some(emb), Some(protected_emb)) = (embedder, protected_embedding) {
        // Embedding path: embed target's posts, average, compare
        let target_embeddings = emb.embed_batch(&post_texts).await?;
        let target_mean = embeddings::mean_embedding(&target_embeddings);
        embeddings::cosine_similarity_embeddings(protected_emb, &target_mean)
    } else {
        // Fallback: TF-IDF keyword cosine similarity
        let topic_extractor = TfIdfExtractor {
            top_n_keywords: 40,
            max_clusters: 7,
        };
        let target_fingerprint = topic_extractor.extract(&post_texts)?;
        overlap::cosine_similarity(protected_fingerprint, &target_fingerprint)
    };

    // Step 4b: Compute behavioral signals
    let quote_count = target_posts.iter().filter(|p| p.is_quote).count();
    let quote_ratio = behavioral::compute_quote_ratio(quote_count, target_posts.len());

    let (reply_count, reply_total) = match posts::fetch_reply_ratio(client, target_handle).await {
        Ok(counts) => counts,
        Err(e) => {
            warn!(
                handle = target_handle,
                error = %e,
                "Reply ratio fetch failed, defaulting to 0"
            );
            (0, 0)
        }
    };
    let reply_ratio = behavioral::compute_reply_ratio(reply_count, reply_total);

    let avg_engagement = behavioral::compute_avg_engagement(&target_posts);
    let pile_on = pile_on_dids.contains(target_did);

    // Step 5: Compute context score via NLI
    //
    // Two modes:
    // - Direct pairs (amplifiers): NLI-score the actual event texts
    // - Inferred pairs (followers): find top 3 most similar posts by embedding
    let context_score = if let Some(nli) = nli_scorer {
        if let Some(pairs) = direct_pairs {
            // Mode A: Direct pairs — score each real interaction
            if pairs.is_empty() {
                None
            } else {
                let mut pair_scores = Vec::new();
                for (original, response) in pairs {
                    match nli.score_pair(original, response).await {
                        Ok((score, hypothesis_scores)) => {
                            pair_scores.push(score);
                            if let Some(dir) = data_dir {
                                crate::scoring::nli_audit::log_nli_audit(
                                    &crate::scoring::nli_audit::NliAuditEntry {
                                        timestamp: chrono::Utc::now().to_rfc3339(),
                                        target_did: target_did.to_string(),
                                        target_handle: target_handle.to_string(),
                                        pair_type: "direct".to_string(),
                                        original_text: original.to_string(),
                                        response_text: response.to_string(),
                                        hypothesis_scores,
                                        hostility_score: score,
                                        similarity: None,
                                    },
                                    Some(dir),
                                );
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "NLI scoring failed for direct pair");
                        }
                    }
                }
                crate::scoring::nli::avg_context_score(&pair_scores)
            }
        } else if let (Some(emb), Some(user_posts)) = (embedder, protected_posts_with_embeddings) {
            // Mode B: Inferred pairs — embedding-matched (existing logic)
            if user_posts.is_empty() {
                None
            } else {
                match emb.embed_batch(&post_texts).await {
                    Ok(target_embeddings) => {
                        let target_with_emb: Vec<(String, Vec<f64>)> = post_texts
                            .iter()
                            .zip(target_embeddings.into_iter())
                            .map(|(text, emb)| (text.clone(), emb))
                            .collect();

                        let user_mean: Vec<f64> = {
                            let dim = user_posts[0].1.len();
                            let mut mean = vec![0.0; dim];
                            for (_, emb) in user_posts {
                                for (i, v) in emb.iter().enumerate() {
                                    mean[i] += v;
                                }
                            }
                            let n = user_posts.len() as f64;
                            mean.iter_mut().for_each(|v| *v /= n);
                            mean
                        };
                        let top_target_posts = crate::scoring::context::find_most_similar_posts(
                            &user_mean,
                            &target_with_emb,
                            3,
                        );

                        let mut pair_scores = Vec::new();
                        for (target_text, similarity) in &top_target_posts {
                            let target_emb = target_with_emb
                                .iter()
                                .find(|(t, _)| t == target_text)
                                .map(|(_, e)| e.as_slice());

                            let user_text = target_emb.and_then(|emb| {
                                crate::scoring::context::find_best_matching_user_post(
                                    emb, user_posts,
                                )
                            });

                            let original = user_text.as_deref().unwrap_or("");
                            if original.is_empty() {
                                continue;
                            }

                            match nli.score_pair(original, target_text).await {
                                Ok((score, hypothesis_scores)) => {
                                    pair_scores.push(score);
                                    if let Some(dir) = data_dir {
                                        crate::scoring::nli_audit::log_nli_audit(
                                            &crate::scoring::nli_audit::NliAuditEntry {
                                                timestamp: chrono::Utc::now().to_rfc3339(),
                                                target_did: target_did.to_string(),
                                                target_handle: target_handle.to_string(),
                                                pair_type: "inferred".to_string(),
                                                original_text: original.to_string(),
                                                response_text: target_text.to_string(),
                                                hypothesis_scores,
                                                hostility_score: score,
                                                similarity: Some(*similarity),
                                            },
                                            Some(dir),
                                        );
                                    }
                                }
                                Err(e) => {
                                    warn!(error = %e, "NLI scoring failed for inferred pair");
                                }
                            }
                        }
                        crate::scoring::nli::avg_context_score(&pair_scores)
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to embed target posts for NLI");
                        None
                    }
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // Step 6: Apply scoring formula in spec order:
    //   1. raw_score = tox * 70 * (1 + overlap * 1.5)
    //   2. score_with_behavioral = raw_score * behavioral_boost (via gate)
    //   3. context_multiplier = 1.0 + (context_score * 0.5)
    //   4. final_score = score_with_behavioral * context_multiplier
    let (raw_score, _) = threat::compute_threat_score(avg_toxicity, topic_overlap, weights);

    let (score_with_behavioral, benign_gate) = behavioral::apply_behavioral_modifier_contextual(
        raw_score,
        quote_ratio,
        reply_ratio,
        pile_on,
        avg_engagement,
        median_engagement,
        context_score,
    );

    let context_multiplier = match context_score {
        Some(ctx) => 1.0 + (ctx * 0.5),
        None => 1.0,
    };
    let final_score = (score_with_behavioral * context_multiplier).clamp(0.0, 100.0);

    let tier = crate::db::models::ThreatTier::from_score(final_score);

    let behavioral_boost = behavioral::compute_behavioral_boost(quote_ratio, reply_ratio, pile_on);
    let signals = behavioral::BehavioralSignals {
        quote_ratio,
        reply_ratio,
        avg_engagement,
        pile_on,
        benign_gate,
        behavioral_boost,
    };
    let signals_json = serde_json::to_string(&signals)?;

    info!(
        handle = target_handle,
        toxicity = format!("{:.2}", avg_toxicity),
        overlap = format!("{:.2}", topic_overlap),
        context = format!("{:?}", context_score),
        raw_score = format!("{:.1}", raw_score),
        threat = format!("{:.1}", final_score),
        tier = tier.as_str(),
        quote_ratio = format!("{:.2}", quote_ratio),
        reply_ratio = format!("{:.2}", reply_ratio),
        benign_gate = benign_gate,
        behavioral_boost = format!("{:.2}", behavioral_boost),
        posts = target_posts.len(),
        "Scored account"
    );

    Ok(AccountScore {
        did: target_did.to_string(),
        handle: target_handle.to_string(),
        toxicity_score: Some(avg_toxicity),
        topic_overlap: Some(topic_overlap),
        threat_score: Some(final_score),
        threat_tier: Some(tier.to_string()),
        posts_analyzed: target_posts.len() as u32,
        top_toxic_posts,
        scored_at: String::new(),
        behavioral_signals: Some(signals_json),
        context_score,
        graph_distance: None,
    })
}

/// Compute a weighted toxicity score from individual category scores.
///
/// The raw model `toxicity` score treats all categories equally, but for
/// threat detection we care much more about identity attacks, insults, and
/// threats than about profanity. An ally who says "fuck yeah, fat liberation!"
/// scores high on obscene/profanity but low on identity_attack — they should
/// NOT be flagged as toxic.
///
/// Falls back to the raw toxicity score if category attributes are missing
/// (e.g. when using a scorer that doesn't provide breakdowns).
fn weighted_toxicity(result: &ToxicityResult) -> f64 {
    let attrs = &result.attributes;

    // If we don't have category breakdowns, fall back to raw score
    let identity_attack = match attrs.identity_attack {
        Some(v) => v,
        None => return result.toxicity,
    };

    let insult = attrs.insult.unwrap_or(0.0);
    let threat = attrs.threat.unwrap_or(0.0);
    let severe = attrs.severe_toxicity.unwrap_or(0.0);
    let profanity = attrs.profanity.unwrap_or(0.0);

    identity_attack * 0.35 + insult * 0.25 + threat * 0.25 + severe * 0.10 + profanity * 0.05
}
