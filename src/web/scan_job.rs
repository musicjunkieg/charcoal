// Background scan job — runs the full scan pipeline when triggered via POST /api/scan.
//
// The scan loads the toxicity scorer and embedder fresh each time it runs,
// so startup stays fast and the scorer isn't held in memory while idle.
//
// Only one scan can run at a time; POST /api/scan returns 409 if one is already active.

use std::collections::HashSet;
use std::sync::Arc;

use tracing::{error, info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::config::Config;
use crate::db::Database;
use crate::scoring::behavioral::detect_pile_on_participants;
use crate::scoring::threat::ThreatWeights;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::download::{
    embedding_files_present, embedding_model_dir, model_files_present,
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
) {
    tokio::spawn(async move {
        if let Err(e) = run_scan(config, db, scan_status.clone()).await {
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
) -> anyhow::Result<()> {
    // Phase 1: load toxicity scorer
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Loading toxicity model…".to_string();
    }

    let scorer: Box<dyn ToxicityScorer> = if model_files_present(&config.model_dir) {
        match OnnxToxicityScorer::load(&config.model_dir) {
            Ok(s) => Box::new(s),
            Err(e) => anyhow::bail!(
                "Failed to load ONNX model: {e}. Run `charcoal download-model` first."
            ),
        }
    } else {
        anyhow::bail!("ONNX model files not found. Run `charcoal download-model` first.");
    };

    // Phase 2: load topic fingerprint
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Loading topic fingerprint…".to_string();
    }

    let fingerprint: TopicFingerprint = match db.get_fingerprint().await? {
        Some((json, _, _)) => serde_json::from_str(&json)?,
        None => anyhow::bail!("No fingerprint found. Run `charcoal fingerprint` first."),
    };

    // Phase 3: load embedding model (optional — falls back to TF-IDF)
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Loading embedding model…".to_string();
    }

    let embed_dir = embedding_model_dir(&config.model_dir);
    let embedder = if embedding_files_present(&config.model_dir) {
        match crate::topics::embeddings::SentenceEmbedder::load(&embed_dir) {
            Ok(e) => {
                info!("Embedding model loaded");
                Some(e)
            }
            Err(e) => {
                warn!(error = %e, "Embedding model failed to load, using TF-IDF fallback");
                None
            }
        }
    } else {
        None
    };

    let protected_embedding = match db.get_embedding().await {
        Ok(Some(v)) => Some(v),
        _ => None,
    };

    // Phase 4: fetch amplification events from Constellation
    {
        let mut s = scan_status.write().await;
        s.progress_message = "Fetching amplification events…".to_string();
    }

    let client = PublicAtpClient::new(&config.public_api_url)?;
    let constellation =
        crate::constellation::client::ConstellationClient::new(&config.constellation_url)?;

    let posts =
        crate::bluesky::posts::fetch_recent_posts(&client, &config.bluesky_handle, 50).await?;
    let post_uris: Vec<String> = posts.iter().map(|p| p.uri.clone()).collect();

    let mut events = constellation.find_amplification_events(&post_uris).await;

    // Resolve DIDs to handles
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

    // Deduplicate by amplifier_post_uri
    let mut seen = HashSet::new();
    events.retain(|e| seen.insert(e.amplifier_post_uri.clone()));
    let event_count = events.len();

    // Phase 5: behavioral context
    {
        let mut s = scan_status.write().await;
        s.progress_message = format!("Scoring followers of {event_count} amplifiers…");
    }

    let median_engagement = db.get_median_engagement().await.unwrap_or(0.0);
    let pile_on_refs = db.get_events_for_pile_on().await.unwrap_or_default();
    let pile_on_dids: HashSet<String> = detect_pile_on_participants(
        &pile_on_refs
            .iter()
            .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
            .collect::<Vec<_>>(),
    );

    // Phase 6: run amplification pipeline
    let weights = ThreatWeights::default();
    let result = crate::pipeline::amplification::run(
        &client,
        scorer.as_ref(),
        &db,
        &fingerprint,
        &weights,
        &config.bluesky_handle,
        true, // analyze_followers
        50,   // max_followers_per_amplifier
        8,    // concurrency
        embedder.as_ref(),
        protected_embedding.as_deref(),
        events,
        median_engagement,
        &pile_on_dids,
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
