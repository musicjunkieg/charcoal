//! Contextual scoring orchestration.
//!
//! Finds the best text pairs between a target account and the protected
//! user for NLI scoring, using embedding similarity to match posts.

use crate::topics::embeddings::cosine_similarity_embeddings;

/// Find the N most similar posts from target to the user embedding.
/// Returns (post_text, similarity) sorted by similarity descending.
pub fn find_most_similar_posts(
    user_embedding: &[f64],
    target_posts: &[(String, Vec<f64>)],
    top_n: usize,
) -> Vec<(String, f64)> {
    let mut scored: Vec<(String, f64)> = target_posts
        .iter()
        .map(|(text, emb)| {
            let sim = cosine_similarity_embeddings(user_embedding, emb);
            (text.clone(), sim)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_n);
    scored
}
