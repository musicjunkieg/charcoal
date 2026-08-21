// Background scan job — runs the full scan pipeline when triggered via POST /api/scan.
//
// The three ONNX models (toxicity, embedding, NLI) are loaded once at boot into
// `AppState::models` and shared by every scan via `Arc::clone` (#257). They used
// to load per scan so they weren't held in memory while idle; concurrency makes
// that cost linear in concurrent scans (~500MB each since #231's fp32 NLI
// export), so they stay resident instead.
//
// Scans are admitted through the `scan_queue` (#257), never started inline:
// POST /api/scan enqueues and returns 202 with a queue position, and the
// background admitter claims rows while the running count is under
// CHARCOAL_SCAN_CONCURRENCY. Nothing in this module can start a scan without a
// `QueueSlot`, which is what keeps the cap honest.

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

/// Per-user scan status, as the process sees it.
///
/// Admission is NOT decided here — that is the `scan_queue`'s job (#257).
/// There used to be an `any_running` bool acting as a process-global
/// one-at-a-time gate; it is gone, because with the queue as the admission
/// authority a process-local bool can only disagree with it. (It disagreed in
/// two directions: it refused an admin trigger whenever any admitted scan ran,
/// and the first of two concurrent scans to finish cleared it globally,
/// admitting an extra scan outside the cap.)
///
/// Every write to a user's status is fenced by the `claim_id` that owns the
/// entry (#274). A worker whose lease lapsed keeps running until its next
/// heartbeat notices — possibly never, if `heartbeat_scan` is the thing that is
/// erroring — and in that window it would otherwise write `Done`, `Failed`, and
/// every progress message straight over the entry of the successor that took
/// its slot. `begin_admitted_scan` is what revokes the zombie's write access:
/// it stamps the entry with the new claim, and every stale write is then a
/// no-op.
pub struct ScanManager {
    statuses: HashMap<String, ScanStatus>,
    fingerprint_building: HashSet<String>,
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
        }
    }

    /// Register a scan the admission queue has approved, taking ownership of
    /// this user's status entry for `claim_id` (#257, #274).
    ///
    /// Overwriting any previous entry is the point: the previous claim, if
    /// still executing somewhere, has been superseded and must stop writing
    /// here.
    pub fn begin_admitted_scan(&mut self, user_did: &str, claim_id: &str) {
        self.statuses.insert(
            user_did.to_string(),
            ScanStatus {
                running: true,
                started_at: Some(chrono::Utc::now().to_rfc3339()),
                progress_message: "Starting scan...".to_string(),
                last_error: None,
                phase: WebScanPhase::Starting,
                claim_id: claim_id.to_string(),
            },
        );
    }

    pub fn get_status(&self, user_did: &str) -> Option<&ScanStatus> {
        self.statuses.get(user_did)
    }

    /// Whether `claim_id` still owns this user's status entry.
    pub fn owns(&self, user_did: &str, claim_id: &str) -> bool {
        self.statuses
            .get(user_did)
            .is_some_and(|s| s.claim_id == claim_id)
    }

    /// Mutate a user's status, but only while `claim_id` owns it (#274).
    ///
    /// Returns false when the write was refused — either the user has no entry,
    /// or a successor claim took the slot and this worker is a zombie. Callers
    /// that care (the terminal write) can log it; `set_progress` does not, since
    /// a superseded scan producing progress is expected until it notices.
    pub fn update_owned(
        &mut self,
        user_did: &str,
        claim_id: &str,
        f: impl FnOnce(&mut ScanStatus),
    ) -> bool {
        match self.statuses.get_mut(user_did) {
            Some(status) if status.claim_id == claim_id => {
                f(status);
                true
            }
            _ => false,
        }
    }

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
    /// Enqueued, waiting for a slot (#257). Never written by the pipeline —
    /// GET /api/status derives it from the user's `scan_queue` row, which is
    /// the only thing that knows about a scan that has not started yet.
    Queued,
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
            WebScanPhase::Queued => "queued",
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
    /// The `scan_queue` claim that owns this entry (#274). Only the worker
    /// holding this fencing token may write to the status; see
    /// `ScanManager::update_owned`.
    pub claim_id: String,
}

use tokio::sync::RwLock;

/// Update the live phase + progress message for a user's scan.
///
/// Takes the write lock briefly. A no-op when `claim_id` no longer owns the
/// entry — a superseded worker keeps producing progress until its heartbeat
/// notices, and every one of those writes would otherwise land in the
/// successor's entry (#274).
async fn set_progress(
    scan_manager: &Arc<RwLock<ScanManager>>,
    user_did: &str,
    claim_id: &str,
    phase: WebScanPhase,
    message: &str,
) {
    scan_manager
        .write()
        .await
        .update_owned(user_did, claim_id, |s| {
            s.phase = phase;
            s.progress_message = message.to_string();
        });
}

/// The `scan_queue` slot a scan is running under (#257).
///
/// Not optional: every scan runs under a slot now, which is what makes
/// `CHARCOAL_SCAN_CONCURRENCY` an actual cap rather than a cap on one of
/// several admission paths.
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
///
/// The panic payload is unwrapped rather than discarded: this text is what
/// reaches BOTH `ScanStatus::last_error` and the durable `scan_queue` row, so
/// dropping it left a panicked scan with no recorded cause in either place.
/// `panic_message` is the pipeline's existing extractor, reused for exactly
/// the reason it was written.
fn classify(finished: std::thread::Result<anyhow::Result<()>>) -> (SlotExit, Option<String>) {
    match finished {
        Ok(Ok(())) => (SlotExit::Completed, None),
        Ok(Err(e)) => (SlotExit::Failed, Some(format!("{e:#}"))),
        Err(payload) => (
            SlotExit::Panicked,
            Some(format!(
                "Background scan panicked: {}",
                crate::pipeline::scan_phases::panic_message(&payload)
            )),
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
/// `live` is the in-process registration for this user (#273). Held for exactly
/// as long as the pipeline is executing and dropped on every exit, so the
/// admitter can tell "this user's row is queued again" from "this user's
/// pipeline is still running in this process".
pub async fn run_under_slot<F>(
    scan: F,
    db: Arc<dyn Database>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: String,
    slot: QueueSlot,
    live: crate::web::admitter::LiveScanGuard,
    heartbeat_interval: std::time::Duration,
) -> SlotExit
where
    F: std::future::Future<Output = anyhow::Result<()>>,
{
    let scan = AssertUnwindSafe(scan).catch_unwind();
    tokio::pin!(scan);

    // Hold the lease for as long as the scan runs. If the heartbeat ever
    // reports the claim lost, this worker has been superseded: another process
    // reclaimed the row and may already be scanning this user, so the scan is
    // abandoned rather than left burning GPU budget on a slot it no longer owns.
    let mut heartbeat = tokio::spawn(crate::web::admitter::heartbeat_until_lost(
        db.clone(),
        user_did.clone(),
        slot.claim_id.clone(),
        heartbeat_interval,
    ));

    let (exit, error_text) = loop {
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
    };

    match exit {
        // run_scan already wrote `Done` from inside the scan future.
        SlotExit::Completed => {}
        // Both of these — and the abandonment below — are fenced by the claim
        // (#274). If a successor already took this user's slot, `update_owned`
        // refuses the write rather than reporting the successor's live scan as
        // this worker's failure.
        SlotExit::Failed | SlotExit::Panicked => {
            let detail = error_text.clone().unwrap_or_default();
            error!(error = %detail, "Background scan failed");
            scan_manager
                .write()
                .await
                .update_owned(&user_did, &slot.claim_id, |status| {
                    status.running = false;
                    status.last_error = Some(detail);
                    // Keep a message the scan future already wrote for itself.
                    // A pipeline error now reaches this arm (it used to be
                    // swallowed into `Completed`), and `record_scan_outcome`
                    // says something more useful about it than this generic
                    // line does — notably that partial results were saved.
                    // Setup errors and panics never get that far, so they still
                    // need a message from here.
                    if status.phase != WebScanPhase::Failed {
                        status.progress_message = "Scan failed — see server logs".to_string();
                    }
                    status.phase = WebScanPhase::Failed;
                });
        }
        SlotExit::Abandoned => {
            warn!(
                user_did,
                "scan abandoned — its lease lapsed and the slot was reassigned"
            );
            // Recorded only if no successor has claimed the entry yet. When one
            // has, this is a no-op and the successor keeps its own live status;
            // when one has not, the user learns their scan stopped instead of
            // watching a "running" label that will never change.
            scan_manager
                .write()
                .await
                .update_owned(&user_did, &slot.claim_id, |status| {
                    status.running = false;
                    status.last_error = error_text.clone();
                    status.progress_message =
                        "Scan stopped — its slot was reassigned. Re-run to resume.".to_string();
                    status.phase = WebScanPhase::Failed;
                });
        }
    }

    // Free the in-process registration BEFORE the row is released.
    //
    // A row that still holds a slot is `running`, which `claim_next_scan` never
    // selects — so a registration held while the row is still ours is never a
    // hazard in either order. The hazard is on the other side: after
    // `release_and_log` returns, the row can be re-enqueued and reclaimed by a
    // concurrent admitter pass immediately, and if `live` were still held at
    // that instant, `try_register` would spuriously refuse the very scan that
    // is supposed to start. Dropping first closes that window instead of
    // opening it.
    drop(live);

    // Release the queue slot on every exit — success, error, caught panic, and
    // abandonment all land here. Done after the status update so the next
    // admitted scan cannot observe this user mid-transition. On abandonment the
    // release is a no-op by construction: the fencing token no longer matches,
    // so `release_and_log` reports Lost and changes nothing.
    crate::web::admitter::release_and_log(&db, &user_did, &slot.claim_id, error_text.as_deref())
        .await;

    // try_send, not send: a full channel already has a wake pending, so
    // dropping this one loses nothing, and a closed channel only means the
    // admitter is gone (shutdown). Neither is worth blocking on.
    let _ = slot.wake.try_send(());

    exit
}

/// Launch the scan pipeline in a background tokio task.
/// Returns immediately. Callers poll `scan_manager` to track progress.
///
/// `pub(crate)` and slot-mandatory on purpose (#257): the admitter is the only
/// caller, because a second admission path is a second way past the concurrency
/// cap. There is no `None` to pass any more.
pub(crate) fn launch_scan(
    state: &crate::web::AppState,
    user_did: String,
    actor_handle: String,
    slot: QueueSlot,
    live: crate::web::admitter::LiveScanGuard,
) {
    let config = state.config.clone();
    let db = state.db.clone();
    let models = state.models.clone();
    let scan_manager = state.scan_manager.clone();

    tokio::spawn(async move {
        // Borrowed by the scan future, so they stay put while `user_did` moves
        // into run_under_slot.
        let did = user_did.clone();
        let handle = actor_handle;
        let claim_id = slot.claim_id.clone();
        let scan = run_scan(
            config,
            db.clone(),
            models,
            scan_manager.clone(),
            &did,
            &handle,
            &claim_id,
        );

        run_under_slot(
            scan,
            db,
            scan_manager,
            user_did,
            slot,
            live,
            crate::web::admitter::HEARTBEAT_INTERVAL,
        )
        .await;
    });
}

/// Rebuild the protected user's fingerprint when it's older than this.
/// Matches the scan-staleness tiering cadence (Normal = 14 days). (#296,
/// spike #295 defect 10 — updated_at was recorded but never consulted.)
const FINGERPRINT_MAX_AGE_DAYS: i64 = 14;

/// True when a fingerprint's `updated_at` (both backends emit
/// `YYYY-MM-DD HH:MM:SS` UTC) is more than FINGERPRINT_MAX_AGE_DAYS old.
/// Unparseable timestamps count as fresh — a malformed row must not
/// trigger a rebuild on every scan.
pub fn fingerprint_is_stale(updated_at: &str, now: chrono::NaiveDateTime) -> bool {
    match chrono::NaiveDateTime::parse_from_str(updated_at, "%Y-%m-%d %H:%M:%S") {
        Ok(built) => {
            now.signed_duration_since(built) > chrono::Duration::days(FINGERPRINT_MAX_AGE_DAYS)
        }
        Err(_) => {
            warn!(
                updated_at,
                "Unparseable fingerprint timestamp; treating as fresh"
            );
            false
        }
    }
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
    info!("Building topic fingerprint for {user_did}");

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
            Ok(Ok(embedder)) => {
                let embed_texts: Vec<String> = post_texts
                    .iter()
                    .map(|t| crate::topics::tfidf::clean_for_embedding(t))
                    .filter(|t| !t.is_empty())
                    .collect();
                if embed_texts.is_empty() {
                    // Every post cleaned to empty (URLs/mentions only). A
                    // zero-vector centroid would silently zero all overlap
                    // comparisons — keep the previous embedding instead.
                    // (#301, CodeRabbit PR #101; near-unreachable in practice
                    // because TF-IDF extraction above bails first on such a
                    // corpus, but defense in depth is cheap here.)
                    warn!("All posts cleaned to empty; skipping embedding save");
                } else {
                    match embedder.embed_batch(&embed_texts).await {
                        Ok(post_embeddings) => {
                            let mean_emb = crate::topics::embeddings::normalized_mean_embedding(
                                &post_embeddings,
                            );
                            if let Err(e) = db.save_embedding(user_did, &mean_emb).await {
                                warn!(error = %e, "Failed to save embedding during fingerprint build");
                            } else {
                                info!("Sentence embedding computed and saved");
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "embed_batch failed during fingerprint build");
                        }
                    }
                }
            }
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

/// Write the pipeline's terminal status, fenced by the claim that owns the
/// entry (#274).
///
/// Split out of `run_scan` because this is the write that made #274 bite: it
/// happens INSIDE the scan future, before `run_under_slot` ever gets to
/// classify the exit, so a zombie whose pipeline finishes inside the window had
/// already stamped `Done` over its successor's live entry by the time the
/// `Completed` arm ran and (correctly) did nothing. Being a free function it is
/// also testable without ~500MB of ONNX models.
async fn record_scan_outcome(
    scan_manager: &Arc<RwLock<ScanManager>>,
    user_did: &str,
    claim_id: &str,
    result: &anyhow::Result<(usize, usize, bool)>,
) {
    let written = scan_manager
        .write()
        .await
        .update_owned(user_did, claim_id, |s| {
            s.running = false;
            match result {
                Ok((events, accounts, degraded)) => {
                    s.last_error = None;
                    s.phase = WebScanPhase::Done;
                    s.progress_message = if *degraded {
                        format!(
                            "Completed (incomplete — cost-capped or accounts skipped, \
                             re-run to resume): {events} events, {accounts} accounts scored"
                        )
                    } else {
                        format!("Completed: {events} events, {accounts} accounts scored")
                    };
                }
                Err(e) => {
                    s.last_error = Some(e.to_string());
                    s.phase = WebScanPhase::Failed;
                    s.progress_message =
                        "Scan encountered an error — partial results may have been saved"
                            .to_string();
                }
            }
        });

    match result {
        Ok((events, accounts, degraded)) => {
            info!(events, accounts, degraded, "Background scan completed")
        }
        Err(e) => error!(error = %e, "Pipeline error"),
    }

    if !written {
        warn!(
            user_did,
            "scan finished but its claim no longer owns the status entry — a \
             successor took this user's slot, so the result was not reported \
             over theirs"
        );
    }
}

/// End the scan future: record the pipeline's terminal status, then hand the
/// pipeline's own `Result` back to `run_under_slot` as the future's result.
///
/// The two halves have to happen together and in this order, which is the whole
/// reason this is one function rather than two statements at the end of
/// `run_scan`. `record_scan_outcome` is the *in-process* write (what the browser
/// polls); the returned `Result` is what `run_under_slot` classifies into a
/// `SlotExit`, and therefore what lands in the durable `scan_queue` row. Drop
/// the second half — as `run_scan` originally did by returning `Ok(())`
/// unconditionally — and a scan that errored two minutes in is stored as a
/// two-minute *successful* scan, which `scan_queue_entry` then folds into the
/// median it quotes every queued user as their ETA.
async fn finish_scan(
    scan_manager: &Arc<RwLock<ScanManager>>,
    user_did: &str,
    claim_id: &str,
    result: anyhow::Result<(usize, usize, bool)>,
) -> anyhow::Result<()> {
    record_scan_outcome(scan_manager, user_did, claim_id, &result).await;
    // Discard only the success tuple — `record_scan_outcome` has already
    // rendered it into the user-visible message. The `Err` must survive.
    result.map(|_| ())
}

async fn run_scan(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    models: Arc<ScanModels>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: &str,
    actor_handle: &str,
    claim_id: &str,
) -> anyhow::Result<()> {
    // Phase 1: toxicity scorer — loaded once at boot (#257), shared via Arc::clone.
    set_progress(
        &scan_manager,
        user_did,
        claim_id,
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
        claim_id,
        WebScanPhase::LoadingModels,
        "Loading embedding model…",
    )
    .await;

    let embedder = Some(Arc::clone(&models.embedder));

    // Phase 2b: NLI model — loaded once at boot, shared via Arc::clone.
    set_progress(
        &scan_manager,
        user_did,
        claim_id,
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
        claim_id,
        WebScanPhase::Fingerprint,
        "Loading topic fingerprint…",
    )
    .await;

    let client = PublicAtpClient::new(&config.public_api_url)?;

    let fingerprint: TopicFingerprint = match db.get_fingerprint(user_did).await? {
        Some((json, _, updated_at))
            if !fingerprint_is_stale(&updated_at, chrono::Utc::now().naive_utc()) =>
        {
            serde_json::from_str(&json)?
        }
        existing => {
            // Absent or stale: (re)build. On a stale-rebuild failure fall
            // back to the stale fingerprint rather than failing the scan —
            // stale data beats no scan.
            let is_rebuild = existing.is_some();
            set_progress(
                &scan_manager,
                user_did,
                claim_id,
                WebScanPhase::Fingerprint,
                if is_rebuild {
                    "Refreshing your topic fingerprint (older than 14 days)…"
                } else {
                    "Building your topic fingerprint from recent posts…"
                },
            )
            .await;

            match build_user_fingerprint(&config, &*db, user_did, actor_handle).await {
                Ok(()) => {
                    let (json, _, _) = db
                        .get_fingerprint(user_did)
                        .await?
                        .expect("Fingerprint was just saved");
                    serde_json::from_str(&json)?
                }
                Err(e) if is_rebuild => {
                    warn!(error = %e, "Fingerprint refresh failed; using stale fingerprint");
                    let (json, _, _) = existing.expect("checked is_rebuild");
                    serde_json::from_str(&json)?
                }
                Err(e) => return Err(e),
            }
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
        claim_id,
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
        claim_id,
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
        claim_id,
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
        claim_id,
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
        claim_id,
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

    finish_scan(&scan_manager, user_did, claim_id, result).await
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
    use crate::web::admitter::{LiveScans, LEASE_SECS};

    const DID: &str = "did:plc:slot";
    /// The claim the tests' ScanManager entry belongs to, unless a test is
    /// deliberately playing a superseded worker.
    const CLAIM: &str = "claim-under-test";

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
    async fn held_slot(
        db: &Arc<dyn Database>,
    ) -> (QueueSlot, String, tokio::sync::mpsc::Receiver<()>) {
        db.enqueue_scan(DID).await.expect("enqueue");
        let claim = db
            .claim_next_scan(1, LEASE_SECS)
            .await
            .expect("claim")
            .expect("a queued row exists");
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let claim_id = claim.claim_id.clone();
        (
            QueueSlot {
                claim_id: claim.claim_id,
                wake: tx,
            },
            claim_id,
            rx,
        )
    }

    fn manager_with_running_scan(claim_id: &str) -> Arc<RwLock<ScanManager>> {
        let mut mgr = ScanManager::new();
        mgr.begin_admitted_scan(DID, claim_id);
        Arc::new(RwLock::new(mgr))
    }

    /// A registration for DID, as the admitter would have handed the pipeline.
    fn live_guard() -> crate::web::admitter::LiveScanGuard {
        LiveScans::new()
            .try_register(DID)
            .expect("a fresh registry always registers")
    }

    /// Exit 1 of 4 — Ok. The row goes to 'done' and the admitter is woken so the
    /// next queued user starts now rather than on the tick.
    #[tokio::test]
    async fn a_successful_scan_releases_its_slot() {
        let db = test_db();
        let (slot, claim_id, mut wake_rx) = held_slot(&db).await;
        let mgr = manager_with_running_scan(&claim_id);

        let exit = run_under_slot(
            async { Ok(()) },
            db.clone(),
            mgr.clone(),
            DID.to_string(),
            slot,
            live_guard(),
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
        let (slot, claim_id, mut wake_rx) = held_slot(&db).await;
        let mgr = manager_with_running_scan(&claim_id);

        let exit = run_under_slot(
            async { Err(anyhow::anyhow!("pipeline exploded")) },
            db.clone(),
            mgr.clone(),
            DID.to_string(),
            slot,
            live_guard(),
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
        let (slot, claim_id, mut wake_rx) = held_slot(&db).await;
        let mgr = manager_with_running_scan(&claim_id);

        let exit = run_under_slot(
            async { panic!("boom inside the pipeline") },
            db.clone(),
            mgr.clone(),
            DID.to_string(),
            slot,
            live_guard(),
            Duration::from_millis(10),
        )
        .await;

        assert_eq!(exit, SlotExit::Panicked);
        assert_eq!(status_of(&db, DID).await, "failed");
        assert!(wake_rx.try_recv().is_ok(), "a panic must wake the admitter");
        let mgr = mgr.read().await;
        let status = mgr.get_status(DID).expect("status");
        assert_eq!(status.phase, WebScanPhase::Failed);
        // The payload, not just the fact of a panic. This used to record a
        // fixed "Background scan panicked" with the payload dropped, so the one
        // clue about the cause never reached the user or the queue row.
        assert!(
            status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("boom inside the pipeline")),
            "the panic message must survive: {:?}",
            status.last_error
        );
    }

    /// Both panic payload shapes must come through, and the same text is what
    /// `release_and_log` writes to the durable `scan_queue` row — the two sinks
    /// share `classify`'s second return value, so covering it here covers both.
    #[test]
    fn a_panic_payload_becomes_the_recorded_error() {
        // `panic!("literal")` — a &'static str payload.
        let literal = std::panic::catch_unwind(|| -> anyhow::Result<()> { panic!("static cause") });
        let (exit, text) = classify(literal);
        assert_eq!(exit, SlotExit::Panicked);
        assert!(
            text.as_deref().is_some_and(|t| t.contains("static cause")),
            "{text:?}"
        );

        // `panic!("{}", …)` and `unwrap()` on an Err — a String payload.
        let formatted =
            std::panic::catch_unwind(|| -> anyhow::Result<()> { panic!("formatted {}", "cause") });
        let (exit, text) = classify(formatted);
        assert_eq!(exit, SlotExit::Panicked);
        assert!(
            text.as_deref()
                .is_some_and(|t| t.contains("formatted cause")),
            "{text:?}"
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

        // The successor has registered its own running scan for this user,
        // which is what revokes the zombie's write access (#274).
        let mgr = manager_with_running_scan(&successor.claim_id);
        let (wake_tx, _wake_rx) = tokio::sync::mpsc::channel(4);

        // A scan future that never finishes: only the heartbeat can end this.
        let exit = tokio::time::timeout(
            Duration::from_secs(5),
            run_under_slot(
                std::future::pending::<anyhow::Result<()>>(),
                db.clone(),
                mgr.clone(),
                DID.to_string(),
                QueueSlot {
                    claim_id: zombie.claim_id,
                    wake: wake_tx,
                },
                live_guard(),
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

    /// #274 — the write `run_under_slot` never sees.
    ///
    /// The abandonment arm above only covers a zombie that is still hanging.
    /// A zombie whose pipeline COMPLETES inside the window writes its own
    /// terminal status from inside `run_scan`, long before `run_under_slot`
    /// classifies the exit — and then the `Completed` arm correctly does
    /// nothing, because the damage is already done. Same for every
    /// `set_progress` call in that window.
    ///
    /// So the fence has to be on the writes themselves, not on an exit arm.
    #[tokio::test]
    async fn a_zombie_cannot_write_its_own_outcome_over_the_successor() {
        let mgr = Arc::new(RwLock::new(ScanManager::new()));
        // The zombie was admitted first...
        mgr.write().await.begin_admitted_scan(DID, "zombie-claim");
        // ...then its lease lapsed and a successor took the slot.
        mgr.write()
            .await
            .begin_admitted_scan(DID, "successor-claim");

        // The zombie's pipeline is oblivious and keeps reporting.
        set_progress(
            &mgr,
            DID,
            "zombie-claim",
            WebScanPhase::Scoring,
            "zombie progress",
        )
        .await;
        // Then it finishes successfully — the exact case `SlotExit::Completed`
        // cannot defend against.
        record_scan_outcome(&mgr, DID, "zombie-claim", &Ok((7, 42, false))).await;

        let mgr = mgr.read().await;
        let status = mgr.get_status(DID).expect("status entry");
        assert!(
            status.running,
            "the zombie must not mark the successor's live scan finished"
        );
        assert_eq!(
            status.phase,
            WebScanPhase::Starting,
            "the zombie's Done must not land on the successor's entry"
        );
        assert_eq!(
            status.progress_message, "Starting scan...",
            "the zombie's progress must not land on the successor's entry"
        );
    }

    /// The other half of #274: the fence must not be so tight that a scan
    /// cannot report its own result. Same writes, still the owner.
    #[tokio::test]
    async fn the_owning_claim_still_writes_its_outcome() {
        let mgr = Arc::new(RwLock::new(ScanManager::new()));
        mgr.write().await.begin_admitted_scan(DID, CLAIM);

        set_progress(&mgr, DID, CLAIM, WebScanPhase::Scoring, "scoring…").await;
        assert_eq!(
            mgr.read().await.get_status(DID).expect("entry").phase,
            WebScanPhase::Scoring
        );

        record_scan_outcome(&mgr, DID, CLAIM, &Ok((7, 42, false))).await;
        let mgr = mgr.read().await;
        let status = mgr.get_status(DID).expect("status entry");
        assert!(!status.running);
        assert_eq!(status.phase, WebScanPhase::Done);
        assert!(status.progress_message.contains("42 accounts scored"));
    }

    /// A pipeline error from the owning claim still reaches the user.
    #[tokio::test]
    async fn the_owning_claim_reports_a_pipeline_error() {
        let mgr = Arc::new(RwLock::new(ScanManager::new()));
        mgr.write().await.begin_admitted_scan(DID, CLAIM);

        record_scan_outcome(
            &mgr,
            DID,
            CLAIM,
            &Err(anyhow::anyhow!("constellation unreachable")),
        )
        .await;

        let mgr = mgr.read().await;
        let status = mgr.get_status(DID).expect("status entry");
        assert!(!status.running);
        assert_eq!(status.phase, WebScanPhase::Failed);
        assert!(status
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("constellation unreachable")));
    }

    /// The pipeline's error has to *leave* the scan future, not merely be
    /// recorded in memory on the way out.
    ///
    /// Every other failure test in this module hands `run_under_slot` a
    /// hand-written `async { Err(...) }` — a shape the most consequential
    /// production failure never produced. `run_scan` returned `Ok(())`
    /// unconditionally after `record_scan_outcome`, so `SlotExit::Failed` was
    /// only ever reachable from the `?`s in the model/classifier/fingerprint
    /// setup above the pipeline. A pipeline that died mid-burst exited
    /// `Completed` and was written to `scan_queue` as `status='done'` with a
    /// NULL error.
    ///
    /// Driving the real tail of `run_scan` is what closes that gap, so this
    /// composes `finish_scan` exactly as `run_scan` does rather than faking
    /// its result.
    #[tokio::test]
    async fn a_pipeline_error_is_recorded_as_failed_not_done() {
        let db = test_db();
        let (slot, claim_id, _wake_rx) = held_slot(&db).await;
        let mgr = manager_with_running_scan(&claim_id);

        let exit = run_under_slot(
            finish_scan(
                &mgr,
                DID,
                &claim_id,
                Err(anyhow::anyhow!("constellation unreachable mid-burst")),
            ),
            db.clone(),
            mgr.clone(),
            DID.to_string(),
            slot,
            live_guard(),
            Duration::from_millis(10),
        )
        .await;

        assert_eq!(
            exit,
            SlotExit::Failed,
            "a pipeline error must classify as Failed — Completed is the exit \
             that records a clean 'done'"
        );
        assert_eq!(
            status_of(&db, DID).await,
            "failed",
            "an operator reading scan_queue must not see a clean 'done' for a \
             scan that died mid-burst"
        );

        {
            let mgr = mgr.read().await;
            let status = mgr.get_status(DID).expect("status entry");
            assert!(!status.running);
            assert_eq!(status.phase, WebScanPhase::Failed);
            assert!(status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("constellation unreachable mid-burst")));
            // The pipeline knows partial results were saved; the generic
            // "see server logs" line in the `Failed` arm does not, and must not
            // overwrite it now that a pipeline error reaches that arm.
            assert!(
                status.progress_message.contains("partial results"),
                "the pipeline's own terminal message must survive: {}",
                status.progress_message
            );
        }

        // The consequence that matters. `scan_queue_entry` medians
        // `finished_at - started_at` over `status='done'` rows to quote every
        // queued user an ETA. A scan that errored seconds in, filed as 'done',
        // is a seconds-long *successful* scan in that median — so the next
        // user is promised almost no wait for what is really an hour.
        db.enqueue_scan("did:plc:next-in-line")
            .await
            .expect("enqueue");
        let waiting = db
            .scan_queue_entry("did:plc:next-in-line", 1)
            .await
            .expect("queue entry query")
            .expect("row exists");
        assert_eq!(waiting.status, "queued");
        assert_eq!(
            waiting.eta_seconds, None,
            "no scan has ever COMPLETED, so there is no median to quote — a \
             failed scan counted as 'done' would fabricate one"
        );
    }

    /// The other half of the propagation fix: a pipeline that succeeded still
    /// ends `done`, so "propagate the error" cannot degenerate into "always
    /// report failure".
    #[tokio::test]
    async fn a_successful_pipeline_is_still_recorded_as_done() {
        let db = test_db();
        let (slot, claim_id, _wake_rx) = held_slot(&db).await;
        let mgr = manager_with_running_scan(&claim_id);

        let exit = run_under_slot(
            finish_scan(&mgr, DID, &claim_id, Ok((7, 42, false))),
            db.clone(),
            mgr.clone(),
            DID.to_string(),
            slot,
            live_guard(),
            Duration::from_millis(10),
        )
        .await;

        assert_eq!(exit, SlotExit::Completed);
        assert_eq!(status_of(&db, DID).await, "done");
        let mgr = mgr.read().await;
        let status = mgr.get_status(DID).expect("status entry");
        assert_eq!(status.phase, WebScanPhase::Done);
        assert!(status.last_error.is_none());
        assert!(status.progress_message.contains("42 accounts scored"));
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

#[cfg(test)]
mod fingerprint_staleness_tests {
    use super::*;

    fn now() -> chrono::NaiveDateTime {
        chrono::NaiveDateTime::parse_from_str("2026-08-20 12:00:00", "%Y-%m-%d %H:%M:%S").unwrap()
    }

    #[test]
    fn fresh_fingerprint_is_not_stale() {
        assert!(!fingerprint_is_stale("2026-08-19 12:00:00", now()));
    }

    #[test]
    fn fifteen_day_old_fingerprint_is_stale() {
        assert!(fingerprint_is_stale("2026-08-05 11:59:59", now()));
    }

    #[test]
    fn exactly_fourteen_days_is_not_stale() {
        // Boundary: staleness begins strictly AFTER 14 days.
        assert!(!fingerprint_is_stale("2026-08-06 12:00:00", now()));
    }

    #[test]
    fn unparseable_timestamp_is_treated_as_fresh() {
        // A malformed timestamp must not cause a rebuild loop on every scan.
        assert!(!fingerprint_is_stale("not a date", now()));
        assert!(!fingerprint_is_stale("", now()));
    }
}
