// Background scan job — runs the full scan pipeline when triggered via POST /api/scan.
//
// The scan loads the toxicity scorer and embedder fresh each time it runs,
// so startup stays fast and the scorer isn't held in memory while idle.
//
// Only one scan can run at a time; POST /api/scan returns 409 if one is already active.

use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::FutureExt;
use tracing::{error, info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::config::Config;
use crate::db::Database;
use crate::scoring::behavioral::detect_pile_on_participants;
use crate::scoring::threat::ThreatWeights;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::download::{
    embedding_files_present, embedding_model_dir, model_files_present, nli_files_present,
};
use crate::toxicity::onnx::OnnxToxicityScorer;
use crate::toxicity::traits::ToxicityScorer;

/// Live status of the background scan, exposed via GET /api/status.
#[derive(Debug, Clone, Default)]
pub struct ScanStatus {
    /// True while a scan is in progress.
    pub running: bool,
    /// ISO 8601 timestamp of when the current/last scan started.
    pub started_at: Option<String>,
    /// Human-readable progress message updated as phases complete.
    pub progress_message: String,
    /// Error message from the last scan, if it failed.
    pub last_error: Option<String>,
}

use tokio::sync::RwLock;

/// Launch the scan pipeline in a background tokio task.
/// Returns immediately. Callers poll `scan_status.running` to track progress.
pub fn launch_scan(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    scan_status: Arc<RwLock<ScanStatus>>,
    user_did: String,
    actor_handle: String,
) {
    tokio::spawn(async move {
        let result = AssertUnwindSafe(run_scan(
            config,
            db,
            scan_status.clone(),
            &user_did,
            &actor_handle,
        ))
        .catch_unwind()
        .await
        .unwrap_or_else(|_| Err(anyhow::anyhow!("Background scan panicked")));
        if let Err(e) = result {
            error!(error = %e, "Background scan failed");
            let mut status = scan_status.write().await;
            status.running = false;
            status.last_error = Some(e.to_string());
            status.progress_message = "Scan failed — see server logs".to_string();
        }
    });
}

async fn run_scan(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    scan_status: Arc<RwLock<ScanStatus>>,
    user_did: &str,
    actor_handle: &str,
) -> anyhow::Result<()> {
    // Phase 1: load toxicity scorer
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Loading toxicity model…".to_string();
    }

    let scorer: Box<dyn ToxicityScorer> = if model_files_present(&config.model_dir) {
        let model_dir = config.model_dir.clone();
        // OnnxToxicityScorer::load is synchronous blocking I/O — offload to avoid
        // stalling the async runtime while the model is read from disk.
        let loaded = tokio::task::spawn_blocking(move || OnnxToxicityScorer::load(&model_dir))
            .await
            .map_err(|e| anyhow::anyhow!("spawn_blocking panicked loading ONNX model: {e}"))??;
        Box::new(loaded)
    } else {
        anyhow::bail!("ONNX model files not found. Run `charcoal download-model` first.");
    };

    // Phase 2: load embedding model (optional — falls back to TF-IDF)
    //
    // Loaded early so it can be reused for both auto-fingerprint embedding
    // (if needed) and amplifier scoring in the pipeline.
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Loading embedding model…".to_string();
    }

    let embed_dir = embedding_model_dir(&config.model_dir);
    let embedder = if embedding_files_present(&config.model_dir) {
        // SentenceEmbedder::load is synchronous blocking I/O — offload to avoid
        // stalling the async runtime while the model is read from disk.
        match tokio::task::spawn_blocking(move || {
            crate::topics::embeddings::SentenceEmbedder::load(&embed_dir)
        })
        .await
        {
            Ok(Ok(e)) => {
                info!("Embedding model loaded");
                Some(e)
            }
            Ok(Err(e)) => {
                warn!(error = %e, "Embedding model failed to load, using TF-IDF fallback");
                None
            }
            Err(e) => {
                warn!(error = %e, "spawn_blocking panicked loading embedder, using TF-IDF fallback");
                None
            }
        }
    } else {
        None
    };

    // Phase 2b: load NLI model (optional — falls back gracefully if unavailable)
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Loading NLI model…".to_string();
    }

    let nli_scorer = if nli_files_present(&config.model_dir) {
        let model_dir = config.model_dir.clone();
        match tokio::task::spawn_blocking(move || crate::scoring::nli::NliScorer::load(&model_dir))
            .await
        {
            Ok(Ok(scorer)) => {
                info!("NLI cross-encoder model loaded");
                Some(scorer)
            }
            Ok(Err(e)) => {
                warn!(error = %e, "NLI model failed to load, context scoring disabled");
                None
            }
            Err(e) => {
                warn!(error = %e, "spawn_blocking panicked loading NLI model");
                None
            }
        }
    } else {
        info!("NLI model not found, context scoring disabled");
        None
    };

    // Phase 3: load or build topic fingerprint
    //
    // For web users there is no CLI step — if no fingerprint exists yet,
    // we build one automatically from the user's recent posts.
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Loading topic fingerprint…".to_string();
    }

    let client = PublicAtpClient::new(&config.public_api_url)?;

    let fingerprint: TopicFingerprint = match db.get_fingerprint(user_did).await? {
        Some((json, _, _)) => serde_json::from_str(&json)?,
        None => {
            // Auto-fingerprint: fetch posts, run TF-IDF, save to DB
            {
                let mut s = scan_status.write().await;
                s.progress_message =
                    "Building your topic fingerprint from recent posts…".to_string();
            }
            info!("No fingerprint found for {user_did}, building automatically");

            let fp_posts =
                crate::bluesky::posts::fetch_recent_posts(&client, actor_handle, 500).await?;
            if fp_posts.is_empty() {
                anyhow::bail!(
                    "No posts found — Charcoal needs posting history to build a topic fingerprint."
                );
            }

            let post_texts: Vec<String> = fp_posts.iter().map(|p| p.text.clone()).collect();
            let extractor = crate::topics::tfidf::TfIdfExtractor::default();
            let fp = crate::topics::traits::TopicExtractor::extract(&extractor, &post_texts)?;

            let json = serde_json::to_string(&fp)?;
            db.save_fingerprint(user_did, &json, fp.post_count).await?;
            info!(
                post_count = fp.post_count,
                clusters = fp.clusters.len(),
                "Topic fingerprint built and saved"
            );

            // Compute and save sentence embedding using the already-loaded embedder
            if let Some(ref embedder) = embedder {
                {
                    let mut s = scan_status.write().await;
                    s.progress_message = "Computing sentence embeddings…".to_string();
                }
                match embedder.embed_batch(&post_texts).await {
                    Ok(post_embeddings) => {
                        let mean_emb = crate::topics::embeddings::mean_embedding(&post_embeddings);
                        if let Err(e) = db.save_embedding(user_did, &mean_emb).await {
                            warn!(error = %e, "Failed to save embedding during auto-fingerprint");
                        } else {
                            info!("Sentence embedding computed and saved");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "embed_batch failed during auto-fingerprint, using TF-IDF fallback");
                    }
                }
            }

            fp
        }
    };

    let protected_embedding = db.get_embedding(user_did).await?;

    // Build per-post embeddings for follower NLI inferred pair matching.
    // Each protected post gets its own embedding so followers' posts can be
    // matched to the closest protected post for NLI pair scoring.
    let protected_posts_with_embeddings: Option<Vec<(String, Vec<f64>)>> = if embedder.is_some()
        && nli_scorer.is_some()
    {
        let pp_texts: Vec<String> =
            crate::bluesky::posts::fetch_recent_posts(&client, actor_handle, 50)
                .await
                .unwrap_or_default()
                .iter()
                .map(|p| p.text.clone())
                .collect();

        if let Some(ref emb) = embedder {
            match emb.embed_batch(&pp_texts).await {
                Ok(embeddings) => Some(pp_texts.into_iter().zip(embeddings.into_iter()).collect()),
                Err(e) => {
                    warn!(error = %e, "Failed to embed protected posts for NLI pairs");
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // Phase 4: fetch amplification events from Constellation
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Fetching amplification events…".to_string();
    }

    let constellation =
        crate::constellation::client::ConstellationClient::new(&config.constellation_url)?;

    let posts = crate::bluesky::posts::fetch_recent_posts(&client, actor_handle, 50).await?;
    let post_uris: Vec<String> = posts.iter().map(|p| p.uri.clone()).collect();

    // Build a cache of original post text keyed by URI — avoids redundant fetches
    // when multiple events reference the same protected post.
    let original_text_cache: std::collections::HashMap<String, String> = posts
        .iter()
        .map(|p| (p.uri.clone(), p.text.clone()))
        .collect();

    let mut events = constellation.find_amplification_events(&post_uris).await;

    // Also fetch likes via Constellation backlinks
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Detecting likes via Constellation…".to_string();
    }
    let like_events = constellation.find_likers(&post_uris).await;
    info!(
        like_count = like_events.len(),
        "Constellation likes detected"
    );
    events.extend(like_events);

    // Fetch reply threads and detect drive-by replies
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Detecting drive-by replies…".to_string();
    }
    let follows_set = crate::bluesky::replies::fetch_follows_set(&client, user_did)
        .await
        .unwrap_or_default();
    for post in &posts {
        match crate::bluesky::replies::fetch_replies_to_post(&client, &post.uri).await {
            Ok(replies) => {
                let reply_dids: Vec<String> =
                    replies.iter().map(|(did, _, _)| did.clone()).collect();
                let drive_by_dids = crate::bluesky::replies::filter_drive_by_replies_excluding_self(
                    &reply_dids,
                    &follows_set,
                    user_did,
                );
                // Create events for drive-by replies
                for (did, _text, uri) in &replies {
                    if drive_by_dids.contains(did) {
                        events.push(crate::bluesky::amplification::AmplificationNotification {
                            event_type: "reply".to_string(),
                            amplifier_did: did.clone(),
                            amplifier_handle: did.clone(), // resolved below
                            original_post_uri: Some(post.uri.clone()),
                            amplifier_post_uri: uri.clone(),
                            indexed_at: String::new(),
                        });
                    }
                }
            }
            Err(e) => {
                warn!(uri = post.uri, error = %e, "Failed to fetch replies");
            }
        }
    }

    // Resolve DIDs to handles for all event types
    let unresolved_dids: Vec<String> = events
        .iter()
        .filter(|e| e.amplifier_handle.starts_with("did:"))
        .map(|e| e.amplifier_did.clone())
        .collect();
    if !unresolved_dids.is_empty() {
        if let Ok(resolved) =
            crate::bluesky::profiles::resolve_dids_to_handles(&client, &unresolved_dids).await
        {
            for event in &mut events {
                if let Some(handle) = resolved.get(&event.amplifier_did) {
                    event.amplifier_handle = handle.clone();
                }
            }
        }
    }

    // Deduplicate: by amplifier_post_uri for quotes/replies, by (did, post_uri) for likes
    let mut seen_uris = HashSet::new();
    let mut seen_likes = HashSet::new();
    events.retain(|e| {
        if e.event_type == "like" {
            seen_likes.insert((e.amplifier_did.clone(), e.original_post_uri.clone()))
        } else {
            seen_uris.insert(e.amplifier_post_uri.clone())
        }
    });
    let event_count = events.len();

    // Phase 5: behavioral context
    {
        let mut s = scan_status.write().await;
        s.progress_message = format!("Scoring followers of {event_count} amplifiers…");
    }

    let median_engagement = db.get_median_engagement(user_did).await?;
    let pile_on_refs = db.get_events_for_pile_on(user_did).await?;
    let pile_on_dids: HashSet<String> = detect_pile_on_participants(
        &pile_on_refs
            .iter()
            .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
            .collect::<Vec<_>>(),
    );

    // Phase 5b: classify social graph distance for all amplifiers
    let amplifier_did_set: std::collections::HashSet<String> =
        events.iter().map(|e| e.amplifier_did.clone()).collect();
    let graph_distances = if !amplifier_did_set.is_empty() {
        let did_refs: Vec<&str> = amplifier_did_set.iter().map(|s| s.as_str()).collect();
        crate::bluesky::relationships::classify_relationships(&client, user_did, &did_refs)
            .await
            .unwrap_or_default()
    } else {
        std::collections::HashMap::new()
    };
    info!(
        classified = graph_distances.len(),
        "Classified amplifier graph distances"
    );

    // Phase 6: run amplification pipeline
    let weights = ThreatWeights::default();
    let result = crate::pipeline::amplification::run(
        &client,
        scorer.as_ref(),
        &db,
        user_did,
        &fingerprint,
        &weights,
        actor_handle,
        true, // analyze_followers
        50,   // max_followers_per_amplifier
        8,    // concurrency
        embedder.as_ref(),
        protected_embedding.as_deref(),
        events,
        median_engagement,
        &pile_on_dids,
        &original_text_cache,
        nli_scorer.as_ref(),
        protected_posts_with_embeddings.as_deref(),
        Some(config.data_dir()),
        &graph_distances,
    )
    .await;

    let mut status = scan_status.write().await;
    status.running = false;
    status.last_error = None;

    match result {
        Ok((events, accounts)) => {
            info!(events, accounts, "Background scan completed");
            status.progress_message =
                format!("Completed: {events} events, {accounts} accounts scored");
        }
        Err(e) => {
            error!(error = %e, "Pipeline error");
            status.last_error = Some(e.to_string());
            status.progress_message =
                "Scan encountered an error — partial results may have been saved".to_string();
        }
    }

    Ok(())
}
