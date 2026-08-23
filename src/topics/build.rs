//! The single fingerprint build path (#297): embedding-first assembly with
//! keyword-only degradation, persisted as one atomic bundle (#302). Lives
//! outside `src/web` because the CLI `charcoal fingerprint` must build
//! without the `web` feature.

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::config::Config;
use crate::db::models::ClusterCentroid;
use crate::db::Database;
use crate::topics::clustering::{build_clustered_fingerprint, ClusteringParams};
use crate::topics::embeddings::normalized_mean_embedding;
use crate::topics::fingerprint::TopicFingerprint;
use crate::topics::tfidf::{clean_for_embedding, TfIdfExtractor};
use crate::topics::traits::TopicExtractor;
use crate::toxicity::download::{embedding_files_present, embedding_model_dir};

/// Aligned originals + their embeddings (empty-cleaned posts already filtered).
pub struct EmbeddedPosts {
    pub original_texts: Vec<String>,
    pub embeddings: Vec<Vec<f64>>,
}

/// Everything one fingerprint generation persists.
pub struct FingerprintArtifacts {
    pub fingerprint: TopicFingerprint,
    pub mean_embedding: Option<Vec<f64>>,
    pub clusters: Vec<ClusterCentroid>,
}

/// Assemble one fingerprint generation. With embeddings: cluster in
/// embedding space and label per-cluster (the #297 fix). Without: today's
/// TF-IDF keyword fingerprint — same degradation as before, still atomic.
///
/// Pure assembly, no I/O — the fetch/embed steps live in
/// `build_user_fingerprint`, which keeps this testable without models or a
/// network.
pub fn assemble_fingerprint(
    post_texts: &[String],
    embedded: Option<&EmbeddedPosts>,
) -> Result<FingerprintArtifacts> {
    match embedded {
        Some(e) if !e.embeddings.is_empty() => {
            let (fingerprint, post_clusters) = build_clustered_fingerprint(
                &e.original_texts,
                &e.embeddings,
                post_texts.len() as u32,
                &ClusteringParams::default(),
            )?;
            let clusters = post_clusters
                .iter()
                .map(|c| ClusterCentroid {
                    centroid: c.centroid.clone(),
                    post_count: c.members.len() as u32,
                })
                .collect();
            Ok(FingerprintArtifacts {
                fingerprint,
                mean_embedding: Some(normalized_mean_embedding(&e.embeddings)),
                clusters,
            })
        }
        _ => {
            let extractor = TfIdfExtractor::default();
            let fingerprint = extractor.extract(post_texts)?;
            Ok(FingerprintArtifacts {
                fingerprint,
                mean_embedding: None,
                clusters: Vec::new(),
            })
        }
    }
}

/// Build a topic fingerprint for a user and persist it atomically.
/// Used by the scan pipeline (auto-fingerprint), admin pre-seed, and the CLI.
pub async fn build_user_fingerprint(
    config: &Config,
    db: &dyn Database,
    user_did: &str,
    handle: &str,
) -> Result<()> {
    info!("Building topic fingerprint for {user_did}");

    let client = PublicAtpClient::new(&config.public_api_url)?;
    let fp_posts = crate::bluesky::posts::fetch_recent_posts(&client, handle, 500).await?;
    if fp_posts.is_empty() {
        anyhow::bail!(
            "No posts found — Charcoal needs posting history to build a topic fingerprint."
        );
    }
    let post_texts: Vec<String> = fp_posts.iter().map(|p| p.text.clone()).collect();

    // Embed when the model is on disk; every failure below degrades to the
    // keyword-only bundle rather than failing the build — a keyword
    // fingerprint beats no fingerprint, and the staleness clock retries in
    // 14 days anyway.
    let embedded: Option<EmbeddedPosts> = if embedding_files_present(&config.model_dir) {
        let embed_dir = embedding_model_dir(&config.model_dir);
        match tokio::task::spawn_blocking(move || {
            crate::topics::embeddings::SentenceEmbedder::load(&embed_dir)
        })
        .await
        {
            Ok(Ok(embedder)) => {
                // Keep originals aligned with their cleaned forms so cluster
                // members map back to real posts for TF-IDF labeling.
                let aligned: Vec<(String, String)> = post_texts
                    .iter()
                    .map(|t| (t.clone(), clean_for_embedding(t)))
                    .filter(|(_, c)| !c.is_empty())
                    .collect();
                if aligned.is_empty() {
                    // URLs/mentions-only corpus (#301): no embeddings, no
                    // zero-centroid poisoning — keyword-only bundle below.
                    warn!("All posts cleaned to empty; building keyword-only fingerprint");
                    None
                } else {
                    let cleaned: Vec<String> = aligned.iter().map(|(_, c)| c.clone()).collect();
                    match embedder.embed_batch(&cleaned).await {
                        Ok(embeddings) => Some(EmbeddedPosts {
                            original_texts: aligned.into_iter().map(|(o, _)| o).collect(),
                            embeddings,
                        }),
                        Err(e) => {
                            warn!(error = %e, "embed_batch failed during fingerprint build");
                            None
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Embedding model failed to load during fingerprint build");
                None
            }
            Err(e) => {
                warn!(error = %e, "spawn_blocking panicked loading embedder during fingerprint build");
                None
            }
        }
    } else {
        None
    };

    // Clustering plus one TF-IDF pass per cluster is seconds-scale CPU work at
    // n=500, and this function runs inside the scan future. Left on the runtime
    // worker it would block every other task on that thread — including the
    // queue heartbeat in `run_under_slot`, whose lease would then look lapsed.
    // `assemble_fingerprint` itself stays sync so it remains unit-testable.
    let fallback_post_texts = post_texts.clone();
    let assembled =
        tokio::task::spawn_blocking(move || assemble_fingerprint(&post_texts, embedded.as_ref()))
            .await
            .context("fingerprint assembly task panicked")?;
    let artifacts = match assembled {
        Ok(artifacts) => artifacts,
        Err(error) => {
            // Same contract as every other embedding failure on this path
            // (#297 spec degradation table): a keyword fingerprint beats no
            // fingerprint. A keyword-only assembly failure still propagates.
            // The retry is a full TF-IDF pass over ≤500 posts — CPU work that
            // belongs on a blocking thread for the same heartbeat reason as
            // the primary attempt above.
            warn!(error = %error, "Fingerprint assembly failed; building keyword-only fingerprint");
            tokio::task::spawn_blocking(move || assemble_fingerprint(&fallback_post_texts, None))
                .await
                .context("keyword-only fingerprint assembly task panicked")??
        }
    };
    let json = serde_json::to_string(&artifacts.fingerprint)?;
    db.save_fingerprint_bundle(
        user_did,
        &json,
        artifacts.fingerprint.post_count,
        artifacts.mean_embedding.as_deref(),
        &artifacts.clusters,
    )
    .await?;
    info!(
        post_count = artifacts.fingerprint.post_count,
        topics = artifacts.clusters.len(),
        embedded = artifacts.mean_embedding.is_some(),
        "Topic fingerprint built and saved atomically"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_without_embeddings_is_keyword_only() {
        let posts: Vec<String> = vec![
            "fat liberation organizing and community care work".into(),
            "community organizing for fat liberation activists".into(),
            "care work and community organizing in fat spaces".into(),
        ];
        let artifacts = assemble_fingerprint(&posts, None).unwrap();
        assert!(artifacts.mean_embedding.is_none());
        assert!(artifacts.clusters.is_empty());
        assert!(!artifacts.fingerprint.clusters.is_empty()); // TF-IDF clusters
        assert_eq!(artifacts.fingerprint.post_count, 3);
    }

    #[test]
    fn assemble_with_embeddings_produces_matching_clusters_and_mean() {
        let posts: Vec<String> = vec![
            "fat liberation is about body autonomy and acceptance".into(),
            "fat acceptance and body liberation communities organizing".into(),
            "body autonomy fat liberation acceptance politics".into(),
            "choral rehearsal techniques for a cappella arrangements".into(),
            "arranging a cappella voicings for choral rehearsal".into(),
            "rehearsal warmups for a cappella choral singers".into(),
        ];
        let mut embs: Vec<Vec<f64>> = (0..3)
            .map(|i| {
                let mut v = vec![0.0; crate::topics::embeddings::EMBEDDING_DIM];
                v[0] = 1.0;
                v[1] = i as f64 * 0.01;
                v
            })
            .collect();
        embs.extend((0..3).map(|i| {
            let mut v = vec![0.0; crate::topics::embeddings::EMBEDDING_DIM];
            v[100] = 1.0;
            v[101] = i as f64 * 0.01;
            v
        }));
        let embedded = EmbeddedPosts {
            original_texts: posts.clone(),
            embeddings: embs,
        };
        let artifacts = assemble_fingerprint(&posts, Some(&embedded)).unwrap();
        assert_eq!(artifacts.clusters.len(), 2);
        assert_eq!(artifacts.fingerprint.clusters.len(), 2); // JSON ↔ rows 1:1
        let mean = artifacts.mean_embedding.unwrap();
        assert_eq!(mean.len(), crate::topics::embeddings::EMBEDDING_DIM);
        // Cluster post_counts reflect membership.
        assert_eq!(
            artifacts.clusters.iter().map(|c| c.post_count).sum::<u32>(),
            6
        );
    }
}
