// Background scan job — runs the full scan pipeline when triggered via POST /api/scan.
//
// The three ONNX models (toxicity, embedding, NLI) are loaded once at boot into
// `AppState::models` and shared by every scan via `Arc::clone` (#257). They used
// to load per scan so they weren't held in memory while idle; concurrency makes
// that cost linear in concurrent scans (~500MB each since #231's fp32 NLI
// export), so they stay resident instead.
//
// Only one scan can run at a time; POST /api/scan returns 409 if one is already active.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use anyhow::Context;
use futures::{FutureExt, StreamExt};
use tracing::{error, info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::config::Config;
use crate::db::Database;
use crate::scoring::behavioral::detect_pile_on_participants;
use crate::scoring::threat::ThreatWeights;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::download::{embedding_files_present, embedding_model_dir};
use crate::toxicity::onnx::OnnxToxicityScorer;
use crate::toxicity::traits::ToxicityScorer;

/// The three ONNX models a scan needs, loaded once and shared.
///
/// These used to load per scan so they were not held while idle. Concurrency
/// (#257) makes that cost linear in concurrent scans — ~500MB each since #231
/// moved NLI to the fp32 export — so they move to `AppState` and stay resident.
/// Accepted trade: production idles ~0.776GB today and ~1.28GB after, which
/// raises the metered RAM floor (#188) even on days nobody scans.
pub struct ScanModels {
    pub toxicity: Arc<OnnxToxicityScorer>,
    pub embedder: Arc<crate::topics::embeddings::SentenceEmbedder>,
    pub nli: Arc<crate::scoring::nli::NliScorer>,
}

impl ScanModels {
    /// Load all three models from `model_dir`.
    ///
    /// Fails hard rather than degrading: a server that cannot score is not
    /// usefully up, and production auto-downloads missing models before this
    /// point. The per-scan path used to degrade when a model was absent; at
    /// boot that would hide a broken deploy behind a green healthcheck.
    pub fn load(model_dir: &std::path::Path) -> anyhow::Result<Self> {
        let toxicity = OnnxToxicityScorer::load(model_dir)
            .context("failed to load the toxicity model at boot")?;
        let embedder = crate::topics::embeddings::SentenceEmbedder::load(
            &crate::toxicity::download::embedding_model_dir(model_dir),
        )
        .context("failed to load the embedding model at boot")?;
        let nli = crate::scoring::nli::NliScorer::load(model_dir)
            .context("failed to load the NLI model at boot")?;

        Ok(Self {
            toxicity: Arc::new(toxicity),
            embedder: Arc::new(embedder),
            nli: Arc::new(nli),
        })
    }
}

/// Manages per-user scan status with a global one-at-a-time gate.
pub struct ScanManager {
    statuses: HashMap<String, ScanStatus>,
    fingerprint_building: HashSet<String>,
    any_running: bool,
}

impl Default for ScanManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ScanManager {
    pub fn new() -> Self {
        Self {
            statuses: HashMap::new(),
            fingerprint_building: HashSet::new(),
            any_running: false,
        }
    }

    /// Register a scan the admission queue has already approved (#257).
    ///
    /// Deliberately skips the `any_running` gate: `claim_next_scan` enforced
    /// the concurrency cap in the database before this was reached, so
    /// re-checking a process-local bool here would refuse every concurrent
    /// scan the queue just legitimately admitted. The bool is still set so the
    /// legacy `POST /api/scan` path (which the next step of #257 replaces with
    /// an enqueue) keeps seeing a scan in flight rather than racing one.
    pub fn begin_admitted_scan(&mut self, user_did: &str) {
        self.any_running = true;
        self.statuses.insert(
            user_did.to_string(),
            ScanStatus {
                running: true,
                started_at: Some(chrono::Utc::now().to_rfc3339()),
                progress_message: "Starting scan...".to_string(),
                last_error: None,
                phase: WebScanPhase::Starting,
            },
        );
    }

    /// Atomically check the global gate and start a scan.
    pub fn try_start_scan(&mut self, user_did: &str) -> Result<(), String> {
        if self.any_running {
            // Scans are gated globally, not per-user — the conflict may be
            // another user's scan, so the message shouldn't imply it's theirs.
            return Err(
                "Another scan is already in progress on this server — scans run \
                 one at a time. Try again in a few minutes."
                    .to_string(),
            );
        }
        self.any_running = true;
        self.statuses.insert(
            user_did.to_string(),
            ScanStatus {
                running: true,
                started_at: Some(chrono::Utc::now().to_rfc3339()),
                progress_message: "Starting scan...".to_string(),
                last_error: None,
                phase: WebScanPhase::Starting,
            },
        );
        Ok(())
    }

    pub fn finish_scan(&mut self, user_did: &str) {
        self.any_running = false;
        if let Some(status) = self.statuses.get_mut(user_did) {
            status.running = false;
        }
    }

    pub fn get_status(&self, user_did: &str) -> Option<&ScanStatus> {
        self.statuses.get(user_did)
    }

    pub fn get_status_mut(&mut self, user_did: &str) -> Option<&mut ScanStatus> {
        self.statuses.get_mut(user_did)
    }

    pub fn is_any_running(&self) -> bool {
        self.any_running
    }

    #[allow(dead_code)]
    pub fn is_scan_running_for(&self, user_did: &str) -> bool {
        self.statuses.get(user_did).is_some_and(|s| s.running)
    }

    pub fn start_fingerprint_build(&mut self, user_did: &str) {
        self.fingerprint_building.insert(user_did.to_string());
    }

    pub fn finish_fingerprint_build(&mut self, user_did: &str) {
        self.fingerprint_building.remove(user_did);
    }

    pub fn is_fingerprint_building(&self, user_did: &str) -> bool {
        self.fingerprint_building.contains(user_did)
    }
}

/// Coarse phase of the background scan, exposed via GET /api/status so the
/// dashboard can render a step indicator instead of guessing from prose.
///
/// `Scoring` covers the whole phased pipeline (gather → burst → finalize);
/// the status handler refines it further from the `scan_phase` marker the
/// pipeline persists in `scan_state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WebScanPhase {
    /// No scan has run in this process lifetime.
    #[default]
    Idle,
    Starting,
    LoadingModels,
    Fingerprint,
    Discovering,
    Scoring,
    Done,
    Failed,
}

impl WebScanPhase {
    /// snake_case string used in the /api/status JSON.
    pub fn as_str(&self) -> &'static str {
        match self {
            WebScanPhase::Idle => "idle",
            WebScanPhase::Starting => "starting",
            WebScanPhase::LoadingModels => "loading_models",
            WebScanPhase::Fingerprint => "fingerprint",
            WebScanPhase::Discovering => "discovering",
            WebScanPhase::Scoring => "scoring",
            WebScanPhase::Done => "done",
            WebScanPhase::Failed => "failed",
        }
    }
}

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
    /// Which coarse stage the scan is in.
    pub phase: WebScanPhase,
}

use tokio::sync::RwLock;

/// Update the live phase + progress message for a user's scan.
/// Takes the write lock briefly; a no-op if the user has no status entry.
async fn set_progress(
    scan_manager: &Arc<RwLock<ScanManager>>,
    user_did: &str,
    phase: WebScanPhase,
    message: &str,
) {
    let mut mgr = scan_manager.write().await;
    if let Some(s) = mgr.get_status_mut(user_did) {
        s.phase = phase;
        s.progress_message = message.to_string();
    }
}

/// The `scan_queue` slot a scan is running under (#257).
///
/// `None` at the call sites that still start a scan directly; the admitter
/// always supplies one.
pub struct QueueSlot {
    /// Fencing token from `claim_next_scan`. Required to heartbeat or release
    /// the row, so a worker whose lease lapsed cannot touch its successor's.
    pub claim_id: String,
    /// Wake channel for the admitter — pinged the moment this scan finishes so
    /// the next queued user starts immediately rather than on the 30s tick.
    pub wake: tokio::sync::mpsc::Sender<()>,
}

/// How a scan running under a queue slot ended.
///
/// Named rather than inferred from a `Result` because the four exits are the
/// whole point of `run_under_slot` and each one has to be independently
/// assertable — "every exit path releases the slot" is not a property a test
/// can check if the test cannot say which exit it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotExit {
    /// The scan future returned `Ok`.
    Completed,
    /// The scan future returned `Err`.
    Failed,
    /// The scan future panicked; the unwind was caught.
    Panicked,
    /// The lease lapsed mid-scan. The slot belongs to a successor now, so the
    /// scan future was dropped and nothing of this worker's was written.
    Abandoned,
}

/// Classify how the (unwind-caught) scan future finished, with the text to
/// record against the queue row.
fn classify(finished: std::thread::Result<anyhow::Result<()>>) -> (SlotExit, Option<String>) {
    match finished {
        Ok(Ok(())) => (SlotExit::Completed, None),
        Ok(Err(e)) => (SlotExit::Failed, Some(format!("{e:#}"))),
        Err(_) => (
            SlotExit::Panicked,
            Some("Background scan panicked".to_string()),
        ),
    }
}

/// Run a scan under its `scan_queue` slot, releasing the slot on every exit.
///
/// Split out of `launch_scan` so it can be driven by a dummy future: the real
/// pipeline needs ~500MB of ONNX models, which would make every test of this
/// composition model-gated (and therefore silently skippable). The composition
/// — the `select!` between scan and heartbeat, the abort-on-finish, the
/// release, the wake — is exactly where the binding constraint lives.
///
/// `slot` is `None` for the call sites that still start a scan directly; they
/// get the panic-catching and the status write but no lease.
pub async fn run_under_slot<F>(
    scan: F,
    db: Arc<dyn Database>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: String,
    slot: Option<QueueSlot>,
    heartbeat_interval: std::time::Duration,
) -> SlotExit
where
    F: std::future::Future<Output = anyhow::Result<()>>,
{
    let scan = AssertUnwindSafe(scan).catch_unwind();
    tokio::pin!(scan);

    let (exit, error_text) = match &slot {
        None => classify(scan.await),
        Some(slot) => {
            // Hold the lease for as long as the scan runs. If the heartbeat
            // ever reports the claim lost, this worker has been superseded:
            // another process reclaimed the row and may already be scanning
            // this user, so the scan is abandoned rather than left burning GPU
            // budget on a slot it no longer owns.
            let mut heartbeat = tokio::spawn(crate::web::admitter::heartbeat_until_lost(
                db.clone(),
                user_did.clone(),
                slot.claim_id.clone(),
                heartbeat_interval,
            ));

            loop {
                tokio::select! {
                    finished = &mut scan => {
                        heartbeat.abort();
                        break classify(finished);
                    }
                    // Match the JOIN result, don't just observe that the task
                    // ended: a `JoinError` is the heartbeat TASK dying, not the
                    // lease lapsing. Treating the two alike would abort a
                    // two-hour scan and report "the slot was reassigned" when
                    // in fact this worker still holds it.
                    beat = &mut heartbeat => match beat {
                        // heartbeat_until_lost only ever returns because the
                        // claim is gone.
                        Ok(_lost) => break (
                            SlotExit::Abandoned,
                            Some("scan lease lapsed — the queue slot was reassigned".to_string()),
                        ),
                        Err(e) => {
                            error!(
                                user_did,
                                error = %e,
                                "the heartbeat task died — restarting it; the scan keeps \
                                 running and still holds its slot"
                            );
                            // Respawning cannot hot-loop: heartbeat_until_lost
                            // sleeps a full interval before it can fail again.
                            heartbeat = tokio::spawn(crate::web::admitter::heartbeat_until_lost(
                                db.clone(),
                                user_did.clone(),
                                slot.claim_id.clone(),
                                heartbeat_interval,
                            ));
                        }
                    },
                }
            }
        }
    };

    match exit {
        // run_scan already wrote its own terminal status (Done, or Failed with
        // the pipeline's own error) before returning.
        SlotExit::Completed => {}
        SlotExit::Failed | SlotExit::Panicked => {
            let detail = error_text.clone().unwrap_or_default();
            error!(error = %detail, "Background scan failed");
            let mut mgr = scan_manager.write().await;
            mgr.finish_scan(&user_did);
            if let Some(status) = mgr.get_status_mut(&user_did) {
                status.last_error = Some(detail);
                status.progress_message = "Scan failed — see server logs".to_string();
                status.phase = WebScanPhase::Failed;
            }
        }
        SlotExit::Abandoned => {
            // Deliberately NOT touching the ScanManager. The lease lapsed, the
            // row was reclaimed, and a successor may already have called
            // `begin_admitted_scan` for this same user — writing `Failed` here
            // would overwrite that live entry and make /api/status report a
            // running scan as failed. The successor owns the user's status now.
            //
            // The cost of being conservative: if no successor has started yet,
            // the user's status keeps saying "running" until one does. A stale
            // label beats clobbering a live scan.
            warn!(
                user_did,
                "scan abandoned — its lease lapsed and the slot was reassigned; \
                 leaving the status entry to whoever holds the slot now"
            );
        }
    }

    // Release the queue slot on every exit — success, error, caught panic, and
    // abandonment all land here. Done after the status update so the next
    // admitted scan cannot observe this user mid-transition. On abandonment the
    // release is a no-op by construction: the fencing token no longer matches,
    // so `release_and_log` reports Lost and changes nothing.
    if let Some(slot) = &slot {
        crate::web::admitter::release_and_log(
            &db,
            &user_did,
            &slot.claim_id,
            error_text.as_deref(),
        )
        .await;
        // try_send, not send: a full channel already has a wake pending, so
        // dropping this one loses nothing, and a closed channel only means the
        // admitter is gone (shutdown). Neither is worth blocking on.
        let _ = slot.wake.try_send(());
    }

    exit
}

/// Launch the scan pipeline in a background tokio task.
/// Returns immediately. Callers poll `scan_manager` to track progress.
pub fn launch_scan(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    models: Arc<ScanModels>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: String,
    actor_handle: String,
    slot: Option<QueueSlot>,
) {
    tokio::spawn(async move {
        // Borrowed by the scan future, so they stay put while `user_did` moves
        // into run_under_slot.
        let did = user_did.clone();
        let handle = actor_handle;
        let scan = run_scan(
            config,
            db.clone(),
            models,
            scan_manager.clone(),
            &did,
            &handle,
        );

        run_under_slot(
            scan,
            db,
            scan_manager,
            user_did,
            slot,
            crate::web::admitter::HEARTBEAT_INTERVAL,
        )
        .await;
    });
}

/// Build a topic fingerprint and embeddings for a user.
/// Fetches their recent posts, runs TF-IDF, and computes MiniLM embeddings.
/// Used by both the scan pipeline (auto-fingerprint) and the admin pre-seed handler.
pub async fn build_user_fingerprint(
    config: &Config,
    db: &dyn Database,
    user_did: &str,
    handle: &str,
) -> anyhow::Result<()> {
    info!("No fingerprint found for {user_did}, building automatically");

    let client = PublicAtpClient::new(&config.public_api_url)?;
    let fp_posts = crate::bluesky::posts::fetch_recent_posts(&client, handle, 500).await?;
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

    // Compute and save sentence embedding if the embedding model is available
    let embed_dir = embedding_model_dir(&config.model_dir);
    if embedding_files_present(&config.model_dir) {
        match tokio::task::spawn_blocking(move || {
            crate::topics::embeddings::SentenceEmbedder::load(&embed_dir)
        })
        .await
        {
            Ok(Ok(embedder)) => match embedder.embed_batch(&post_texts).await {
                Ok(post_embeddings) => {
                    let mean_emb = crate::topics::embeddings::mean_embedding(&post_embeddings);
                    if let Err(e) = db.save_embedding(user_did, &mean_emb).await {
                        warn!(error = %e, "Failed to save embedding during fingerprint build");
                    } else {
                        info!("Sentence embedding computed and saved");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "embed_batch failed during fingerprint build");
                }
            },
            Ok(Err(e)) => {
                warn!(error = %e, "Embedding model failed to load during fingerprint build");
            }
            Err(e) => {
                warn!(error = %e, "spawn_blocking panicked loading embedder during fingerprint build");
            }
        }
    }

    Ok(())
}

async fn run_scan(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    models: Arc<ScanModels>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: &str,
    actor_handle: &str,
) -> anyhow::Result<()> {
    // Phase 1: toxicity scorer — loaded once at boot (#257), shared via Arc::clone.
    set_progress(
        &scan_manager,
        user_did,
        WebScanPhase::LoadingModels,
        "Loading toxicity model…",
    )
    .await;

    let primary_scorer: Box<dyn ToxicityScorer> = Box::new(Arc::clone(&models.toxicity));

    // Wrap in the two-stage scorer. ONNX runs as a clean-pass filter
    // (< 0.10 = cleared); posts at or above the threshold are sent to the
    // configured Stage-2 classifier (CHARCOAL_CLASSIFIER) for a binary verdict.
    // The classifier is required — build_from_env errors (and the scan fails
    // loudly) if unconfigured; there is no silent ONNX-only fallback.
    let classifier = crate::toxicity::classifier::build_from_env()?;
    info!(
        backend = classifier.name(),
        "Stage-2 toxicity classifier loaded — two-stage scoring enabled"
    );
    // Scan-start banner metric so log aggregation can attribute which backend
    // produced this scan's verdicts.
    crate::observability::classifier_metrics::record_backend_selected(classifier.name());

    // Concrete scorer (not boxed as `dyn`): the phased scan pipeline (#208)
    // needs the `TwoStageToxicityScorer`'s inherent `classifier()` accessor and
    // its `CleanPassScorer` impl, both of which a `dyn ToxicityScorer` erases.
    let scorer = crate::toxicity::ensemble::TwoStageToxicityScorer::new(primary_scorer, classifier);

    // Phase 2: embedding model — loaded once at boot, shared via Arc::clone.
    //
    // Kept `Option`-shaped downstream (always `Some` now that boot fail-fast
    // guarantees presence — #257) so the pipeline signature and the
    // `embedder.is_some()` / `as_deref()` call sites below didn't need to
    // change shape.
    set_progress(
        &scan_manager,
        user_did,
        WebScanPhase::LoadingModels,
        "Loading embedding model…",
    )
    .await;

    let embedder = Some(Arc::clone(&models.embedder));

    // Phase 2b: NLI model — loaded once at boot, shared via Arc::clone.
    set_progress(
        &scan_manager,
        user_did,
        WebScanPhase::LoadingModels,
        "Loading NLI model…",
    )
    .await;

    let nli_scorer = Some(Arc::clone(&models.nli));

    // Phase 3: load or build topic fingerprint
    //
    // For web users there is no CLI step — if no fingerprint exists yet,
    // we build one automatically from the user's recent posts.
    set_progress(
        &scan_manager,
        user_did,
        WebScanPhase::Fingerprint,
        "Loading topic fingerprint…",
    )
    .await;

    let client = PublicAtpClient::new(&config.public_api_url)?;

    let fingerprint: TopicFingerprint = match db.get_fingerprint(user_did).await? {
        Some((json, _, _)) => serde_json::from_str(&json)?,
        None => {
            // Auto-fingerprint: fetch posts, run TF-IDF, compute embeddings, save to DB.
            // build_user_fingerprint handles the full pipeline including embeddings.
            set_progress(
                &scan_manager,
                user_did,
                WebScanPhase::Fingerprint,
                "Building your topic fingerprint from recent posts…",
            )
            .await;

            build_user_fingerprint(&config, &*db, user_did, actor_handle).await?;

            // Load the fingerprint we just built
            let (json, _, _) = db
                .get_fingerprint(user_did)
                .await?
                .expect("Fingerprint was just saved");
            serde_json::from_str(&json)?
        }
    };

    let protected_embedding = db.get_embedding(user_did).await?;

    // Build per-post embeddings for follower NLI inferred pair matching.
    // Each protected post gets its own embedding so followers' posts can be
    // matched to the closest protected post for NLI pair scoring.
    let protected_posts_with_embeddings: Option<Vec<(String, Vec<f64>)>> =
        if embedder.is_some() && nli_scorer.is_some() {
            let pp_texts: Vec<String> =
                crate::bluesky::posts::fetch_recent_posts(&client, actor_handle, 50)
                    .await
                    .unwrap_or_default()
                    .iter()
                    .map(|p| p.text.clone())
                    .collect();

            if let Some(ref emb) = embedder {
                match emb.embed_batch(&pp_texts).await {
                    Ok(embeddings) => Some(pp_texts.into_iter().zip(embeddings).collect()),
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
    set_progress(
        &scan_manager,
        user_did,
        WebScanPhase::Discovering,
        "Fetching amplification events…",
    )
    .await;

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
    set_progress(
        &scan_manager,
        user_did,
        WebScanPhase::Discovering,
        "Detecting likes via Constellation…",
    )
    .await;
    let like_events = constellation.find_likers(&post_uris).await;
    info!(
        like_count = like_events.len(),
        "Constellation likes detected"
    );
    events.extend(like_events);

    // Fetch reply threads and detect drive-by replies
    set_progress(
        &scan_manager,
        user_did,
        WebScanPhase::Discovering,
        "Detecting drive-by replies…",
    )
    .await;
    let follows_set = crate::bluesky::replies::fetch_follows_set(&client, user_did)
        .await
        .unwrap_or_default();
    // Fetch each post's replies concurrently (was one serial round-trip per
    // post, #213), then process in post order so the emitted events are
    // byte-identical to the old serial loop. Concurrency capped low —
    // `PublicAtpClient` has no backoff yet (#182).
    const REPLY_FETCH_CONCURRENCY: usize = 8;
    let mut fetched_replies: Vec<(usize, String, _)> = futures::stream::iter(0..posts.len())
        .map(|i| {
            let uri = posts[i].uri.clone();
            let client = &client;
            async move {
                let result = crate::bluesky::replies::fetch_replies_to_post(client, &uri).await;
                (i, uri, result)
            }
        })
        .buffer_unordered(REPLY_FETCH_CONCURRENCY)
        .collect()
        .await;
    fetched_replies.sort_by_key(|(i, _, _)| *i);

    for (_, post_uri, reply_result) in fetched_replies {
        match reply_result {
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
                            original_post_uri: Some(post_uri.clone()),
                            amplifier_post_uri: uri.clone(),
                            indexed_at: String::new(),
                        });
                    }
                }
            }
            Err(e) => {
                warn!(uri = post_uri, error = %e, "Failed to fetch replies");
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
    // Distinct amplifier accounts behind the events — computed here (rather
    // than in Phase 5b where it used to live) so the progress message below
    // reports the real amplifier count, not the event count.
    let amplifier_did_set: std::collections::HashSet<String> =
        events.iter().map(|e| e.amplifier_did.clone()).collect();
    let amplifier_count = amplifier_did_set.len();

    // Phase 5: behavioral context
    set_progress(
        &scan_manager,
        user_did,
        WebScanPhase::Discovering,
        &format!("Scoring followers of {amplifier_count} amplifiers…"),
    )
    .await;

    let median_engagement = db.get_median_engagement(user_did).await?;
    let pile_on_refs = db.get_events_for_pile_on(user_did).await?;
    let pile_on_dids: HashSet<String> = detect_pile_on_participants(
        &pile_on_refs
            .iter()
            .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
            .collect::<Vec<_>>(),
    );

    // Phase 5b: classify social graph distance for all amplifiers
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

    // Phase 6: run amplification pipeline. From here until completion the
    // pipeline reports progress via the scan_state table (scan_phase marker +
    // classification counts), which GET /api/status reads to refine this phase.
    set_progress(
        &scan_manager,
        user_did,
        WebScanPhase::Scoring,
        "Scoring candidate accounts…",
    )
    .await;

    let weights = ThreatWeights::default();
    let result = crate::pipeline::amplification::run(
        &client,
        Some(&scorer),
        &db,
        user_did,
        &fingerprint,
        &weights,
        actor_handle,
        true, // analyze_followers
        50,   // max_followers_per_amplifier
        8,    // concurrency
        embedder.as_deref(),
        protected_embedding.as_deref(),
        events,
        median_engagement,
        &pile_on_dids,
        &original_text_cache,
        nli_scorer.as_deref(),
        protected_posts_with_embeddings.as_deref(),
        Some(config.data_dir()),
        &graph_distances,
    )
    .await;

    let mut mgr = scan_manager.write().await;
    mgr.finish_scan(user_did);

    match result {
        Ok((events, accounts, degraded)) => {
            info!(events, accounts, degraded, "Background scan completed");
            if let Some(s) = mgr.get_status_mut(user_did) {
                s.last_error = None;
                s.phase = WebScanPhase::Done;
                s.progress_message = if degraded {
                    format!(
                        "Completed (incomplete — cost-capped or accounts skipped, \
                         re-run to resume): {events} events, {accounts} accounts scored"
                    )
                } else {
                    format!("Completed: {events} events, {accounts} accounts scored")
                };
            }
        }
        Err(e) => {
            error!(error = %e, "Pipeline error");
            if let Some(s) = mgr.get_status_mut(user_did) {
                s.last_error = Some(e.to_string());
                s.phase = WebScanPhase::Failed;
                s.progress_message =
                    "Scan encountered an error — partial results may have been saved".to_string();
            }
        }
    }

    Ok(())
}

/// The slot lifecycle: every exit from `run_under_slot` must free the row it
/// claimed, and none of them may stomp a successor's state.
///
/// Driven by dummy futures against in-memory SQLite, so none of it is
/// model-gated — the composition these cover is the whole reason the fencing
/// token exists, and a model-gated test of it would silently skip.
#[cfg(test)]
mod slot_lifecycle_tests {
    use super::*;

    use std::time::Duration;

    use crate::db::schema::create_tables;
    use crate::db::sqlite::SqliteDatabase;
    use crate::web::admitter::LEASE_SECS;

    const DID: &str = "did:plc:slot";

    fn test_db() -> Arc<dyn Database> {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
        create_tables(&conn).expect("schema");
        Arc::new(SqliteDatabase::new(conn))
    }

    async fn status_of(db: &Arc<dyn Database>, did: &str) -> String {
        db.scan_queue_entry(did, 1)
            .await
            .expect("queue entry query")
            .expect("row exists")
            .status
    }

    /// Enqueue and claim `DID`, returning the slot plus the wake receiver so a
    /// test can assert the admitter was pinged.
    async fn held_slot(db: &Arc<dyn Database>) -> (QueueSlot, tokio::sync::mpsc::Receiver<()>) {
        db.enqueue_scan(DID).await.expect("enqueue");
        let claim = db
            .claim_next_scan(1, LEASE_SECS)
            .await
            .expect("claim")
            .expect("a queued row exists");
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        (
            QueueSlot {
                claim_id: claim.claim_id,
                wake: tx,
            },
            rx,
        )
    }

    fn manager_with_running_scan() -> Arc<RwLock<ScanManager>> {
        let mut mgr = ScanManager::new();
        mgr.begin_admitted_scan(DID);
        Arc::new(RwLock::new(mgr))
    }

    /// Exit 1 of 4 — Ok. The row goes to 'done' and the admitter is woken so the
    /// next queued user starts now rather than on the tick.
    #[tokio::test]
    async fn a_successful_scan_releases_its_slot() {
        let db = test_db();
        let (slot, mut wake_rx) = held_slot(&db).await;
        let mgr = manager_with_running_scan();

        let exit = run_under_slot(
            async { Ok(()) },
            db.clone(),
            mgr.clone(),
            DID.to_string(),
            Some(slot),
            Duration::from_millis(10),
        )
        .await;

        assert_eq!(exit, SlotExit::Completed);
        assert_eq!(status_of(&db, DID).await, "done", "the slot must be freed");
        assert!(
            wake_rx.try_recv().is_ok(),
            "finishing must wake the admitter, not leave the next user for the tick"
        );
    }

    /// Exit 2 of 4 — Err. The slot is freed too (holding it until the lease
    /// lapses would throttle the server for two minutes over one failure), and
    /// the failure is recorded in both the row and the status.
    #[tokio::test]
    async fn a_failed_scan_releases_its_slot() {
        let db = test_db();
        let (slot, mut wake_rx) = held_slot(&db).await;
        let mgr = manager_with_running_scan();

        let exit = run_under_slot(
            async { Err(anyhow::anyhow!("pipeline exploded")) },
            db.clone(),
            mgr.clone(),
            DID.to_string(),
            Some(slot),
            Duration::from_millis(10),
        )
        .await;

        assert_eq!(exit, SlotExit::Failed);
        assert_eq!(status_of(&db, DID).await, "failed");
        assert!(
            wake_rx.try_recv().is_ok(),
            "a failure must wake the admitter"
        );

        let mgr = mgr.read().await;
        let status = mgr.get_status(DID).expect("status entry");
        assert!(!status.running);
        assert_eq!(status.phase, WebScanPhase::Failed);
        assert!(status
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("pipeline exploded")));
    }

    /// Exit 3 of 4 — panic. The unwind must be caught rather than killing the
    /// task before the release: an uncaught panic leaks the slot until the
    /// lease lapses.
    #[tokio::test]
    async fn a_panicking_scan_releases_its_slot() {
        let db = test_db();
        let (slot, mut wake_rx) = held_slot(&db).await;
        let mgr = manager_with_running_scan();

        let exit = run_under_slot(
            async { panic!("boom inside the pipeline") },
            db.clone(),
            mgr.clone(),
            DID.to_string(),
            Some(slot),
            Duration::from_millis(10),
        )
        .await;

        assert_eq!(exit, SlotExit::Panicked);
        assert_eq!(status_of(&db, DID).await, "failed");
        assert!(wake_rx.try_recv().is_ok(), "a panic must wake the admitter");
        assert_eq!(
            mgr.read().await.get_status(DID).expect("status").phase,
            WebScanPhase::Failed
        );
    }

    /// Exit 4 of 4 — the lease is lost mid-scan.
    ///
    /// The zombie must abandon a scan that would otherwise never end, and must
    /// leave BOTH the successor's queue row and the successor's status entry
    /// alone. Writing `Failed` here is what would make /api/status report a
    /// running scan as failed.
    #[tokio::test]
    async fn a_lost_lease_abandons_without_clobbering_the_successor() {
        let db = test_db();

        // Zombie claims with an already-expired lease; the row is reclaimed and
        // re-claimed, so the zombie's token no longer owns it.
        db.enqueue_scan(DID).await.expect("enqueue");
        let zombie = db
            .claim_next_scan(1, -1)
            .await
            .expect("claim")
            .expect("a queued row exists");
        assert_eq!(db.reclaim_expired_scans().await.expect("reclaim"), 1);
        let successor = db
            .claim_next_scan(1, LEASE_SECS)
            .await
            .expect("claim")
            .expect("the reclaimed row is queued again");
        assert_ne!(zombie.claim_id, successor.claim_id);

        // The successor has registered its own running scan for this user.
        let mgr = manager_with_running_scan();
        let (wake_tx, _wake_rx) = tokio::sync::mpsc::channel(4);

        // A scan future that never finishes: only the heartbeat can end this.
        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            run_under_slot(
                std::future::pending::<anyhow::Result<()>>(),
                db.clone(),
                mgr.clone(),
                DID.to_string(),
                Some(QueueSlot {
                    claim_id: zombie.claim_id,
                    wake: wake_tx,
                }),
                Duration::from_millis(10),
            ),
        )
        .await
        .expect("a lost lease must end the scan on its own");

        assert_eq!(exit, SlotExit::Abandoned);
        assert_eq!(
            status_of(&db, DID).await,
            "running",
            "the successor's row must be untouched"
        );

        let mgr = mgr.read().await;
        let status = mgr.get_status(DID).expect("status entry");
        assert!(
            status.running,
            "the zombie must not mark the successor's scan finished"
        );
        assert_eq!(
            status.phase,
            WebScanPhase::Starting,
            "the zombie must not overwrite the successor's phase with Failed"
        );
        assert!(status.last_error.is_none());
    }
}

#[cfg(test)]
mod model_sharing_tests {
    use super::*;

    /// Two scans must share one model instance, not load their own. This is the
    /// memory precondition for concurrency: per-scan loading costs ~500MB each
    /// (284 fp32 NLI + 126 toxicity + 90 embedding) against a 1.11GB prod peak.
    #[test]
    fn scan_models_are_shared_by_arc_not_cloned() {
        let base = crate::toxicity::download::resolve_model_dir();
        if !crate::toxicity::download::nli_files_present(&base) {
            eprintln!("SKIP: models not present at {}", base.display());
            return;
        }
        let models = Arc::new(ScanModels::load(&base).expect("load models"));
        let a = Arc::clone(&models);
        let b = Arc::clone(&models);

        // Same allocation behind both handles — a clone would be a second load.
        assert!(
            Arc::ptr_eq(&a.nli, &b.nli),
            "concurrent scans must share one NLI instance"
        );
        assert!(Arc::ptr_eq(&a.toxicity, &b.toxicity));
        assert!(Arc::ptr_eq(&a.embedder, &b.embedder));
        assert_eq!(Arc::strong_count(&models), 3, "models + a + b");
    }
}
