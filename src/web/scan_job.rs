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
use chrono::{DateTime, Utc};
use futures::{FutureExt, StreamExt};
use tracing::{debug, error, info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::config::Config;
use crate::db::Database;
use crate::scoring::behavioral::detect_pile_on_participants;
use crate::scoring::threat::ThreatWeights;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::onnx::OnnxToxicityScorer;
use crate::toxicity::traits::ToxicityScorer;

// The fingerprint build path moved to `crate::topics::build` (#297) so the
// CLI (`charcoal fingerprint`, built without the `web` feature) can share it.
// Re-exported here so admin.rs and this module's own callers keep resolving
// `scan_job::build_user_fingerprint` unchanged.
pub use crate::topics::build::build_user_fingerprint;

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

/// How a full scan ended, for bookkeeping. `amplification::run` returns
/// `Ok((events, scored, degraded))` for cost-capped and partially skipped
/// scans alike, so `Ok` alone says nothing about completion (V2-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanCompletion {
    /// Every candidate scored; staging drained to `done`; zero persisted skips.
    Complete,
    /// Reached `done` but `n` accounts were skipped at some point in this
    /// staged run (persisted in `scan_skips`, which survives a resume).
    CompleteWithSkips { n: i64 },
    /// Reached `done` but the skip count could not be read: fulfilled for the
    /// cooldown, never clean, never proof (V5-01).
    CompleteUnverified,
    /// Cost-capped or interrupted: staging left at burst/finalize, re-run to resume.
    Resumable,
}

impl ScanCompletion {
    /// The user's request was carried out — verified or not. Drives the
    /// cooldown marker and clears the full obligation (V6-01). Only
    /// `Complete` additionally proves the revision.
    pub fn fulfilled(&self) -> bool {
        !matches!(self, ScanCompletion::Resumable)
    }

    /// Severity for folding a drain's outcome into the run's own (V6-01):
    /// Resumable > CompleteUnverified > CompleteWithSkips > Complete. Skip
    /// counts add.
    pub fn worst(self, other: ScanCompletion) -> ScanCompletion {
        use ScanCompletion::*;
        match (self, other) {
            (Resumable, _) | (_, Resumable) => Resumable,
            (CompleteUnverified, _) | (_, CompleteUnverified) => CompleteUnverified,
            (CompleteWithSkips { n: a }, CompleteWithSkips { n: b }) => {
                CompleteWithSkips { n: a + b }
            }
            (CompleteWithSkips { n }, Complete) | (Complete, CompleteWithSkips { n }) => {
                CompleteWithSkips { n }
            }
            (Complete, Complete) => Complete,
        }
    }
}

impl From<ScanCompletion> for crate::db::FinishCompletion {
    fn from(c: ScanCompletion) -> crate::db::FinishCompletion {
        use crate::db::FinishCompletion as F;
        match c {
            ScanCompletion::Complete => F::Complete,
            ScanCompletion::CompleteWithSkips { .. } => F::CompleteWithSkips,
            ScanCompletion::CompleteUnverified => F::CompleteUnverified,
            ScanCompletion::Resumable => F::Resumable,
        }
    }
}

/// `scan_state` key holding what a full scan remembers about a refresh drain
/// it performed earlier in the same owed run (V7-02).
///
/// The value is written and interpreted by `drain_then_run` (Task 9), which
/// owns the drain. It is named here because [`record_full_scan_completion`] is
/// what RETIRES it: a fulfilled run has consumed whatever drain it carried, so
/// the key is deleted in the same transaction as the cooldown marker. A failed
/// or resumable attempt leaves it in place — the obligation it belongs to is
/// still owed.
///
/// The key exists at all because the evidence it summarises (`scan_skips`, the
/// refresh's staging) is wiped by the full scan's own fresh start
/// (`scan_phases/mod.rs:195`), and the run may be resumed by another process.
pub const CARRIED_KEY: &str = "full_carried_completion";

/// Pure classification from the pipeline result, the `scan_phase` marker
/// after the run, and the skip count. `Err` is not a completion at all and
/// is handled by the caller.
pub fn classify_full_scan(
    degraded: bool,
    scan_phase: Option<&str>,
    skipped: Option<i64>,
) -> ScanCompletion {
    // The marker is authoritative (V4-02): staging left at burst/finalize is
    // unfinished work whatever the summary's flag says, and an unreadable
    // marker is not proof of completion either. The skip count is PERSISTED
    // state (V5-01): `scan_skips` is cleared only at a fresh start, so a
    // resumed invocation that saw no new error still carries the earlier
    // skips — the flag alone would erase them.
    match (scan_phase, skipped, degraded) {
        (Some("burst") | Some("finalize") | None, _, _) => ScanCompletion::Resumable,
        (Some("done"), Some(n), _) if n > 0 => ScanCompletion::CompleteWithSkips { n },
        (Some("done"), Some(_), false) => ScanCompletion::Complete,
        // Degraded with no recorded skip: something else went wrong (decode
        // sentinels, #355). Fulfilled, not clean.
        (Some("done"), Some(_), true) => ScanCompletion::CompleteWithSkips { n: 0 },
        (Some("done"), None, _) => ScanCompletion::CompleteUnverified,
        (Some(_), _, _) => ScanCompletion::Resumable, // gather or unknown: not finished
    }
}

/// What a scan future hands back to `run_under_slot`, so the durable queue
/// outcome (`scan_queue.completion`) keeps the distinction between a
/// fulfilled request and an interrupted attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanReport {
    pub completion: crate::db::FinishCompletion,
}

/// The cooldown anchor. Written for Complete, CompleteWithSkips AND
/// CompleteUnverified — every completion [`ScanCompletion::fulfilled`] admits
/// (V6-01): the user's request was carried out, and skipped or unverifiable
/// accounts are per-account gaps the retry covers. Never for Resumable.
/// Best-effort: a scan that completed must not be reported failed because a
/// marker write failed.
///
/// The same call records the run's duration as the ETA median's sample
/// (#344 F1) — see [`Database::finish_full_scan_state`].
///
/// `pub`, like `run_under_slot`, for the same reason: the V3-02 property —
/// the cooldown reads THIS marker and never the queue row's `done` status —
/// is only observable by composing this with the real slot lifecycle and the
/// real handler, which lives in `tests/web_scan_queue.rs`.
pub async fn record_full_scan_completion(
    db: &dyn Database,
    user_did: &str,
    claim_id: &str,
    completion: ScanCompletion,
) {
    if completion.fulfilled() {
        // Marker and carried-outcome delete together: a fulfilled run has
        // consumed whatever drain it carried (V7-02). `finish_full_scan_state`
        // is one `with_conn` transaction on SQLite / one transaction on Postgres,
        // fenced by `claim_id` so a superseded worker writes nothing (N2).
        if let Err(e) = db
            .finish_full_scan_state(
                user_did,
                &chrono::Utc::now().to_rfc3339(),
                CARRIED_KEY,
                claim_id,
            )
            .await
        {
            warn!(error = %format!("{e:#}"), "could not record last_full_scan_finished_at");
        }
    }
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
///
/// The `FinishCompletion` is what the durable `scan_queue.completion` records
/// (#344): a scan that reported itself only `Resumable` finishes `done` like
/// any other, and the row is the only place that difference survives a
/// restart. An `Err` or a panic is `Failed` — no completion was reported at
/// all, and the full-scan obligation must stay owed.
fn classify(
    finished: std::thread::Result<anyhow::Result<ScanReport>>,
) -> (SlotExit, Option<String>, crate::db::FinishCompletion) {
    match finished {
        Ok(Ok(report)) => (SlotExit::Completed, None, report.completion),
        Ok(Err(e)) => (
            SlotExit::Failed,
            Some(format!("{e:#}")),
            crate::db::FinishCompletion::Failed,
        ),
        Err(payload) => (
            SlotExit::Panicked,
            Some(format!(
                "Background scan panicked: {}",
                crate::pipeline::scan_phases::panic_message(&payload)
            )),
            crate::db::FinishCompletion::Failed,
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
    F: std::future::Future<Output = anyhow::Result<ScanReport>>,
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

    let (exit, error_text, completion) = loop {
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
                    // Never written: the release below finds no matching row,
                    // by construction (the fencing token is dead).
                    crate::db::FinishCompletion::Failed,
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
    crate::web::admitter::release_and_log(
        &db,
        &user_did,
        &slot.claim_id,
        completion,
        error_text.as_deref(),
    )
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
///
/// `kind` comes off the claim, and [`run_claim`] decides from it which pipeline
/// actually runs — a `refresh` row must never execute the full scan.
pub(crate) fn launch_scan(
    state: &crate::web::AppState,
    user_did: String,
    actor_handle: String,
    kind: crate::db::ScanKind,
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

        run_claim(
            kind,
            db.clone(),
            scan_manager.clone(),
            user_did,
            slot,
            live,
            crate::web::admitter::HEARTBEAT_INTERVAL,
            || run_scan(config, db, models, scan_manager, &did, &handle, &claim_id),
        )
        .await;
    });
}

/// The error a `refresh` claim finishes with until Task 9 lands the runner.
///
/// Named so the guard below and its test say the same thing, and so the
/// message an operator sees in `scan_queue.last_error` names the missing piece
/// rather than describing a scan failure that never happened.
fn refresh_runner_missing(user_did: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "no refresh runner is built into this binary yet (#344 Task 9) — the queued refresh \
         for {user_did} was not executed; it will be retried, and a full scan is unaffected"
    )
}

/// Run one admitted claim as the kind it was admitted as (#344 F1).
///
/// The guard is here rather than at the admitter because this is the last
/// point a claim can still be diverted: however a `refresh` row got into the
/// queue — the tick, a manual enqueue, a migration — it must not execute
/// `run_scan`. A full scan under a refresh claim would write the full-scan
/// cooldown marker and an ETA duration sample against a refresh row, which is
/// exactly what N4 forbids, and would clear a full-scan obligation the refresh
/// never fulfilled.
///
/// The refusal is loud and terminal rather than silent or looping: dropping the
/// row would leave the user's schedule advanced with nothing to show for it,
/// and re-queueing it would spin. Finishing the claim `failed` releases the
/// slot, records the reason durably, and lets the hourly retry pick the user up
/// once a runner exists.
///
/// `full` is a closure so the full pipeline is never even constructed for a
/// refresh claim — which is the property the test asserts.
#[allow(clippy::too_many_arguments)] // the slot lifecycle's own arguments, one call site
pub(crate) async fn run_claim<R, Fut>(
    kind: crate::db::ScanKind,
    db: Arc<dyn Database>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: String,
    slot: QueueSlot,
    live: crate::web::admitter::LiveScanGuard,
    heartbeat_interval: std::time::Duration,
    full: R,
) -> SlotExit
where
    R: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<ScanReport>>,
{
    match kind {
        crate::db::ScanKind::Full => {
            run_under_slot(
                full(),
                db,
                scan_manager,
                user_did,
                slot,
                live,
                heartbeat_interval,
            )
            .await
        }
        crate::db::ScanKind::Refresh => {
            error!(
                user_did,
                "a refresh claim was admitted but this binary has no refresh runner — \
                 finishing it as failed rather than running a full scan under it"
            );
            let refusal = refresh_runner_missing(&user_did);
            run_under_slot(
                async move { Err(refusal) },
                db,
                scan_manager,
                user_did,
                slot,
                live,
                heartbeat_interval,
            )
            .await
        }
    }
}

/// Rebuild the protected user's fingerprint when it's older than this.
/// Matches the scan-staleness tiering cadence (Normal = 14 days). (#296,
/// spike #295 defect 10 — updated_at was recorded but never consulted.)
const FINGERPRINT_MAX_AGE_DAYS: i64 = 14;

/// True when a fingerprint's `updated_at` (both backends emit
/// `YYYY-MM-DD HH:MM:SS` UTC) is more than FINGERPRINT_MAX_AGE_DAYS old.
/// Unparseable timestamps count as STALE: a successful rebuild rewrites
/// `updated_at` via the DB clock, so a one-off corrupt row self-heals after
/// one rebuild, while treating it as fresh would keep an old fingerprint on
/// the fresh path forever. A rebuild-per-scan only persists under systemic
/// timestamp-format drift, which the warn below makes loud — and the
/// stale-rebuild-failure path already falls back gracefully. (#303)
pub fn fingerprint_is_stale(updated_at: &str, now: chrono::NaiveDateTime) -> bool {
    match chrono::NaiveDateTime::parse_from_str(updated_at, "%Y-%m-%d %H:%M:%S") {
        Ok(built) => {
            now.signed_duration_since(built) > chrono::Duration::days(FINGERPRINT_MAX_AGE_DAYS)
        }
        Err(_) => {
            warn!(
                updated_at,
                "Unparseable fingerprint timestamp; treating as stale and rebuilding"
            );
            true
        }
    }
}

/// A stored generation needs a format rebuild when the embedding path ran
/// (mean embedding exists) but the per-topic centroid rows don't match the
/// fingerprint JSON: zero rows = pre-#297 legacy, count mismatch = pre-#302
/// divergence. Keyword-only fingerprints (no embedding) are NOT legacy —
/// treating them as such would rebuild-loop every scan on model-less
/// deployments. (#297)
pub fn fingerprint_needs_rebuild_for_format(
    has_embedding: bool,
    centroid_rows: usize,
    json_clusters: usize,
) -> bool {
    has_embedding && centroid_rows != json_clusters
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
    completion: crate::db::FinishCompletion,
) -> anyhow::Result<ScanReport> {
    record_scan_outcome(scan_manager, user_did, claim_id, &result).await;
    // Discard only the success tuple — `record_scan_outcome` has already
    // rendered it into the user-visible message. The `Err` must survive, and
    // the classified completion rides out to the durable queue row (#344).
    result.map(|_| ScanReport { completion })
}

/// Phase 0 (#343): one number per scan, read from scan_state, not logs.
/// Best-effort — a failure to record a diagnostic must not fail the scan.
async fn record_observed_rate_limit(db: &dyn Database, user_did: &str, observed: Option<u64>) {
    if let Some(limit) = observed {
        if let Err(e) = db
            .set_scan_state(user_did, "bluesky_ratelimit_limit", &limit.to_string())
            .await
        {
            tracing::warn!(error = %e, "could not record bluesky_ratelimit_limit");
        }
    } else {
        // No RateLimit-Limit header was ever observed this scan — either every
        // request omitted it or every value failed to parse. Either way the
        // Phase 0 pass number depends on this measurement, so its absence from
        // scan_state must be diagnosable rather than silent. One line per scan,
        // not per request.
        tracing::warn!(
            user_did,
            "no RateLimit-Limit header observed during this scan; bluesky_ratelimit_limit will be missing from scan_state"
        );
    }
}

/// What the full scan's inner run produces once it has gone as far as it can.
///
/// `completion` is CLASSIFIED (V2-05) — the pipeline returns `Ok` for a
/// cost-capped run too — so this is the only thing the bookkeeping boundary
/// needs to decide what the attempt earned.
///
/// `pub(crate)` like [`run_scan_with`], the only thing that consumes it —
/// nothing outside the crate constructs or reads one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FullScanRun {
    pub events: usize,
    pub scored: usize,
    pub completion: ScanCompletion,
}

/// Everything a full scan writes about ITSELF, as opposed to about its
/// results: the cooldown marker and the refresh schedule.
///
/// A trait rather than three direct calls so the wrapper's single scheduling
/// site can be driven — and counted — without a database's worth of fixture,
/// which is what makes "exactly one retry per failed attempt, and no other
/// scheduling call" an assertable property.
#[async_trait::async_trait]
pub trait FullScanBookkeeping: Send + Sync {
    /// Does this attempt still own the user's queue row? (#344 F2)
    ///
    /// The marker is fenced inside its own transaction, but the refresh
    /// schedule lives on `users` and cannot be fenced by a `WHERE` clause on
    /// `scan_queue`, so the wrapper asks once and skips ALL of its writes when
    /// the answer is no.
    async fn owns_claim(&self, user_did: &str, claim_id: &str) -> bool;
    /// Best-effort: the marker (+ ETA sample + carried-key delete), written
    /// only for fulfilled completions and only under this claim.
    async fn record_completion(&self, user_did: &str, claim_id: &str, completion: ScanCompletion);
    async fn schedule_success(&self, user_did: &str, now: DateTime<Utc>);
    async fn schedule_retry(&self, user_did: &str, now: DateTime<Utc>);
}

/// The production bookkeeping: Task 5's marker plus the refresh schedulers.
pub struct DbFullScanBookkeeping {
    db: Arc<dyn Database>,
    /// The cadence a success writes. Injected at construction so
    /// `schedule_after_success` never reads a process-global variable from
    /// inside the code under test (#344 F3).
    refresh_interval: std::time::Duration,
}

impl DbFullScanBookkeeping {
    pub fn new(db: Arc<dyn Database>, refresh_interval: std::time::Duration) -> Self {
        Self {
            db,
            refresh_interval,
        }
    }
}

#[async_trait::async_trait]
impl FullScanBookkeeping for DbFullScanBookkeeping {
    async fn owns_claim(&self, user_did: &str, claim_id: &str) -> bool {
        match self.db.scan_claim_is_current(user_did, claim_id).await {
            Ok(current) => current,
            Err(e) => {
                // Fail closed. An unreadable queue row is not evidence of
                // ownership, and the cost of guessing wrong is clobbering a
                // successor's schedule with this attempt's. Skipping costs the
                // user a deadline they get back on the next tick — they are
                // still due by the revision clause, or by their owed full work.
                warn!(
                    user_did,
                    error = %format!("{e:#}"),
                    "could not confirm the scan claim still owns this row — skipping the \
                     completion bookkeeping"
                );
                false
            }
        }
    }

    async fn record_completion(&self, user_did: &str, claim_id: &str, completion: ScanCompletion) {
        record_full_scan_completion(self.db.as_ref(), user_did, claim_id, completion).await;
    }

    async fn schedule_success(&self, user_did: &str, now: DateTime<Utc>) {
        crate::web::refresh::schedule_after_success(
            self.db.as_ref(),
            user_did,
            now,
            self.refresh_interval,
        )
        .await;
    }

    async fn schedule_retry(&self, user_did: &str, now: DateTime<Utc>) {
        crate::web::refresh::schedule_retry(self.db.as_ref(), user_did, now).await;
    }
}

/// ONE scheduling site for the full scan (V5-02).
///
/// `run` is the entire inner scan, so an early `?` anywhere in it arrives
/// here as `Err` and still gets its retry — which is the defect this shape
/// exists to remove: a scan that failed while building its scorer used to
/// return before any scheduling code ran, and the user's owed full scan was
/// never retried.
///
/// * complete → marker + nightly + proof
/// * complete-with-skips / unverified → marker + hourly retry (the request
///   was fulfilled, but it is not proof of the revision)
/// * resumable, or ANY error → hourly retry, obligation kept, no marker, no
///   proof
///
/// The deadline is anchored on the attempt's END (V6-02): `clock` is read
/// once *after* `run` returns, so a ninety-minute failed attempt still gets a
/// full hour of backoff. The start instant is telemetry only.
///
/// `clock: &(dyn Fn() -> DateTime<Utc> + Sync)` and not a bare `&dyn Fn`:
/// the future holds this borrow across `.await` and `launch_scan` hands it to
/// `tokio::spawn`, which needs `Send` (V7-01).
pub(crate) async fn run_scan_with<R, Fut>(
    scan_manager: Arc<RwLock<ScanManager>>,
    books: &dyn FullScanBookkeeping,
    clock: &(dyn Fn() -> DateTime<Utc> + Sync),
    user_did: &str,
    claim_id: &str,
    run: R,
) -> anyhow::Result<ScanReport>
where
    R: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<FullScanRun>>,
{
    let started = clock(); // telemetry only — never a scheduling anchor (V6-02)
    let outcome = run().await;
    // Read the clock AFTER the attempt: a retry is "an hour after this attempt
    // ended", not "an hour after it began" (V6-02).
    let now = clock();
    debug!(
        user_did,
        attempt_secs = (now - started).num_seconds(),
        "full scan attempt finished"
    );
    let completion = match &outcome {
        Ok(r) => Some(r.completion),
        Err(_) => None,
    };
    // One ownership check for all three writes (#344 F2). `record_completion`
    // fences itself, but `schedule_success`/`schedule_retry` write `users`,
    // which no fence on `scan_queue` can cover: a zombie whose lease lapsed
    // would otherwise move `next_refresh_at` and `refreshed_generation` for a
    // user its successor now owns — and being the slow one, its write lands
    // last. `finish_scan` below is still called: its writes are fenced on the
    // claim of their own accord, and the caller must learn the outcome either
    // way.
    if books.owns_claim(user_did, claim_id).await {
        if let Some(c) = completion {
            books.record_completion(user_did, claim_id, c).await; // no-op for Resumable
        }
        match completion {
            Some(ScanCompletion::Complete) => books.schedule_success(user_did, now).await,
            // CompleteWithSkips / CompleteUnverified: fulfilled (marker written,
            // obligation cleared by finish), not clean — retry, no proof.
            // Resumable and Err: obligation kept, retry.
            _ => books.schedule_retry(user_did, now).await,
        }
    } else {
        warn!(
            user_did,
            "the scan's claim no longer owns the queue row — no marker, no ETA sample and \
             no refresh schedule written; the successor owns this user's bookkeeping"
        );
    }
    let (result, finish) = match outcome {
        Ok(r) => (
            Ok((r.events, r.scored, r.completion != ScanCompletion::Complete)),
            r.completion.into(),
        ),
        Err(e) => (Err(e), crate::db::FinishCompletion::Failed),
    };
    finish_scan(&scan_manager, user_did, claim_id, result, finish).await
}

/// The full scan, wrapped so every exit reaches ONE scheduling site (V5-02).
///
/// Everything that can fail — scorer construction, the fingerprint rebuild
/// and its abort arm, discovery, the pipeline, classification — happens
/// inside `run_scan_inner`, so an early `?` anywhere in it lands in
/// [`run_scan_with`] as `Err` and still gets its retry. The slot lifecycle
/// only finishes the row; it never schedules.
async fn run_scan(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    models: Arc<ScanModels>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: &str,
    actor_handle: &str,
    claim_id: &str,
) -> anyhow::Result<ScanReport> {
    let books = DbFullScanBookkeeping::new(
        Arc::clone(&db),
        crate::web::refresh::refresh_deadline_interval(),
    );
    run_scan_with(
        Arc::clone(&scan_manager),
        &books,
        &chrono::Utc::now,
        user_did,
        claim_id,
        || {
            run_scan_inner(
                config,
                db,
                models,
                scan_manager,
                user_did,
                actor_handle,
                claim_id,
            )
        },
    )
    .await
}

async fn run_scan_inner(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    models: Arc<ScanModels>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: &str,
    actor_handle: &str,
    claim_id: &str,
) -> anyhow::Result<FullScanRun> {
    // Phase 1: toxicity scorer — loaded once at boot (#257), shared via Arc::clone.
    set_progress(
        &scan_manager,
        user_did,
        claim_id,
        WebScanPhase::LoadingModels,
        "Loading toxicity model…",
    )
    .await;

    // #343 Phase 1 (CodeRabbit, PR #118): nothing else deletes from the shared
    // cache tables — no user_did means delete_user_data skips them — so bound
    // them here, once per scan. Best-effort by construction: a cache is an
    // optimisation, and a failed sweep must never abort a scan.
    crate::db::cache_retention::evict_stale_cache_best_effort(db.as_ref()).await;

    // #343 Phase 1: stage-1 / clean-pass ONNX scores are cached by text hash
    // across users. Hit/miss counts are persisted at the end of the scan.
    let onnx_cache_stats = Arc::new(crate::observability::cache_stats::CacheStats::default());
    let primary_scorer: Box<dyn ToxicityScorer> =
        Box::new(crate::toxicity::cached::CachedToxicityScorer::new(
            Box::new(Arc::clone(&models.toxicity)),
            Arc::clone(&db),
            crate::toxicity::onnx::ONNX_MODEL_ID,
            Arc::clone(&onnx_cache_stats),
        ));

    // Wrap in the two-stage scorer. ONNX runs as a clean-pass filter
    // (< 0.10 = cleared); posts at or above the threshold are sent to the
    // configured Stage-2 classifier (CHARCOAL_CLASSIFIER) for a binary verdict.
    // The classifier is required — build_from_env errors (and the scan fails
    // loudly) if unconfigured; there is no silent ONNX-only fallback.
    // #343 Phase 1: stage-2 verdicts are cached by (text hash, model, policy).
    let classifier_cache_stats = Arc::new(crate::observability::cache_stats::CacheStats::default());
    let classifier: Arc<dyn crate::toxicity::classifier::ToxicityClassifier> =
        Arc::new(crate::toxicity::cached_classifier::CachedClassifier::new(
            crate::toxicity::classifier::build_from_env()?,
            Arc::clone(&db),
            Arc::clone(&classifier_cache_stats),
        ));
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

    // Load the stored generation up front: fingerprint JSON, mean embedding,
    // and per-topic centroid rows. These are three independent reads, NOT one
    // transactional snapshot — a concurrent `save_fingerprint_bundle` (the CLI
    // or the admin pre-seed, neither of which the scan queue serializes) can
    // commit between any two of them. What the single load buys is a coherent
    // DECISION: the rebuild check below and the scoring further down see the
    // same three values, so they cannot disagree with each other. A torn read
    // costs at worst one unnecessary rebuild or one scan against mixed
    // generations; the next scan re-reads and self-heals.
    let stored = db.get_fingerprint(user_did).await?;
    let mut protected_embedding = db.get_embedding(user_did).await?;
    let mut protected_centroid_rows = db.get_topic_centroids(user_did).await?;

    // An unreadable stored fingerprint is treated as absent rather than
    // aborting the scan: a rebuild rewrites it, so the row self-heals. (The
    // pre-#297 code parsed it with `?` and failed the whole scan.)
    let stored_fingerprint: Option<TopicFingerprint> =
        stored
            .as_ref()
            .and_then(|(json, _, _)| match serde_json::from_str(json) {
                Ok(fp) => Some(fp),
                Err(e) => {
                    warn!(error = %e, "Stored fingerprint JSON is unreadable; rebuilding");
                    None
                }
            });

    // Rebuild when absent/unreadable, when older than 14 days (#296), or when
    // the stored generation predates the clustered format (#297).
    let needs_rebuild = match (&stored, &stored_fingerprint) {
        (Some((_, _, updated_at)), Some(parsed)) => {
            fingerprint_is_stale(updated_at, chrono::Utc::now().naive_utc())
                || fingerprint_needs_rebuild_for_format(
                    protected_embedding.is_some(),
                    protected_centroid_rows.len(),
                    parsed.clusters.len(),
                )
        }
        _ => true,
    };

    let fingerprint: TopicFingerprint = if !needs_rebuild {
        stored_fingerprint.expect("a fresh, well-formed fingerprint was just parsed")
    } else {
        // Absent or stale: (re)build. On a stale-rebuild failure fall
        // back to the stale fingerprint rather than failing the scan —
        // stale data beats no scan.
        let is_rebuild = stored_fingerprint.is_some();
        set_progress(
            &scan_manager,
            user_did,
            claim_id,
            WebScanPhase::Fingerprint,
            if is_rebuild {
                // Covers both triggers — age (#296) and stored-format
                // upgrade (#297) — so the copy no longer claims a reason.
                "Refreshing your topic fingerprint…"
            } else {
                "Building your topic fingerprint from recent posts…"
            },
        )
        .await;

        match build_user_fingerprint(&config, &*db, user_did, actor_handle).await {
            Ok(()) => {
                // `delete_user_data` or a concurrent rebuild can remove the row
                // between the save and this read. Propagate instead of
                // panicking the worker, which would surface to the user as
                // "Background scan panicked" rather than a real error.
                let (json, _, _) = db.get_fingerprint(user_did).await?.ok_or_else(|| {
                    anyhow::anyhow!("fingerprint row vanished immediately after a successful save")
                })?;
                // The bundle save replaced the embedding and the centroid rows
                // atomically (#302) — re-read both or scoring would compare
                // candidates against the previous generation's vectors.
                protected_embedding = db.get_embedding(user_did).await?;
                protected_centroid_rows = db.get_topic_centroids(user_did).await?;
                serde_json::from_str(&json)?
            }
            Err(e) if is_rebuild => {
                warn!(error = %e, "Fingerprint refresh failed; using stale fingerprint");
                stored_fingerprint.expect("checked is_rebuild")
            }
            Err(e) => return Err(e),
        }
    };

    // Scoring wants bare vectors; the row's post_count is fingerprint metadata.
    let protected_topic_centroids: Vec<Vec<f64>> = protected_centroid_rows
        .iter()
        .map(|c| c.centroid.clone())
        .collect();

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
        Some(&protected_topic_centroids),
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

    record_observed_rate_limit(db.as_ref(), user_did, client.observed_rate_limit()).await;

    if let Err(e) = crate::observability::cache_stats::record_cache_stats(
        db.as_ref(),
        user_did,
        "onnx",
        &onnx_cache_stats,
    )
    .await
    {
        tracing::warn!(error = %e, "could not record onnx cache stats");
    }

    if let Err(e) = crate::observability::cache_stats::record_cache_stats(
        db.as_ref(),
        user_did,
        "classifier",
        &classifier_cache_stats,
    )
    .await
    {
        tracing::warn!(error = %e, "could not record classifier cache stats");
    }

    // Completion is CLASSIFIED, never inferred from `Ok` (V2-05): the pipeline
    // returns `Ok` for a cost-capped run too. The `scan_phase` marker says
    // whether the staging actually drained, and `scan_skips` — which survives
    // a burst/finalize resume — says whether this scan's coverage has holes
    // even when the resuming invocation itself reported none (V5-01).
    //
    // A failed count read is `None`, not 0: unverifiable is a distinct answer
    // from clean, and `classify_full_scan` keeps it that way.
    let (events, scored, degraded) = result?;
    let phase = db.get_scan_state(user_did, "scan_phase").await?;
    let skipped = db.count_scan_skips(user_did).await.ok();
    Ok(FullScanRun {
        events,
        scored,
        completion: classify_full_scan(degraded, phase.as_deref(), skipped),
    })
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
            async {
                Ok(ScanReport {
                    completion: crate::db::FinishCompletion::Complete,
                })
            },
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
        let literal =
            std::panic::catch_unwind(|| -> anyhow::Result<ScanReport> { panic!("static cause") });
        let (exit, text, completion) = classify(literal);
        assert_eq!(completion, crate::db::FinishCompletion::Failed);
        assert_eq!(exit, SlotExit::Panicked);
        assert!(
            text.as_deref().is_some_and(|t| t.contains("static cause")),
            "{text:?}"
        );

        // `panic!("{}", …)` and `unwrap()` on an Err — a String payload.
        let formatted = std::panic::catch_unwind(|| -> anyhow::Result<ScanReport> {
            panic!("formatted {}", "cause")
        });
        let (exit, text, _) = classify(formatted);
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
                std::future::pending::<anyhow::Result<ScanReport>>(),
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
                crate::db::FinishCompletion::Failed,
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
            finish_scan(
                &mgr,
                DID,
                &claim_id,
                Ok((7, 42, false)),
                crate::db::FinishCompletion::Complete,
            ),
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

    /// F1: a `refresh` claim must never execute the full scan, however its row
    /// got into the queue. Until Task 9 lands the runner it finishes `failed`
    /// with a message naming the missing piece — and, critically, the full
    /// pipeline is never even constructed, so none of the full scan's
    /// bookkeeping (the cooldown marker, the ETA duration sample) can be
    /// written against a refresh row (N4).
    #[tokio::test]
    async fn a_refresh_claim_never_runs_the_full_scan() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};

        let db = test_db();
        db.upsert_user(DID, "slot.h").await.expect("user");
        db.enqueue_refresh_scan(DID).await.expect("enqueue refresh");
        let claim = db
            .claim_next_scan(1, LEASE_SECS)
            .await
            .expect("claim")
            .expect("a queued row exists");
        assert_eq!(claim.kind, crate::db::ScanKind::Refresh);
        let (tx, mut wake_rx) = tokio::sync::mpsc::channel(4);
        let mgr = manager_with_running_scan(&claim.claim_id);

        let constructed = Arc::new(AtomicBool::new(false));
        let flag = constructed.clone();
        let exit = run_claim(
            claim.kind,
            db.clone(),
            mgr,
            DID.to_string(),
            QueueSlot {
                claim_id: claim.claim_id.clone(),
                wake: tx,
            },
            live_guard(),
            Duration::from_millis(10),
            || {
                flag.store(true, SeqCst);
                async {
                    Ok(ScanReport {
                        completion: crate::db::FinishCompletion::Complete,
                    })
                }
            },
        )
        .await;

        assert!(
            !constructed.load(SeqCst),
            "the full pipeline must not even be built for a refresh claim"
        );
        assert_eq!(exit, SlotExit::Failed);
        let row = db
            .list_scan_queue()
            .await
            .expect("queue")
            .into_iter()
            .find(|r| r.user_did == DID)
            .expect("row exists");
        assert_eq!(row.status, "failed");
        assert!(
            row.last_error
                .as_deref()
                .unwrap_or_default()
                .contains("no refresh runner"),
            "the recorded reason must name the missing runner: {:?}",
            row.last_error
        );
        // N4: nothing about the FULL scan was recorded against this row.
        assert_eq!(
            db.get_scan_state(DID, "last_full_scan_finished_at")
                .await
                .expect("scan_state read"),
            None,
            "no cooldown marker"
        );
        assert_eq!(
            db.get_scan_state(DID, crate::db::traits::LAST_FULL_SCAN_DURATION_KEY)
                .await
                .expect("scan_state read"),
            None,
            "no ETA duration sample"
        );
        assert!(
            wake_rx.try_recv().is_ok(),
            "the slot was released and the admitter woken"
        );
    }
}

/// #344 V2-05/V5-01/V6-01: how a full scan's ending is classified, and what
/// a classification is allowed to claim. These are pure — no models, no
/// network — because the whole point is that `Ok(..)` from the pipeline is
/// not evidence of completion.
#[cfg(test)]
mod completion_tests {
    use super::*;

    use crate::db::schema::create_tables;
    use crate::db::sqlite::SqliteDatabase;
    use crate::db::traits::LAST_FULL_SCAN_DURATION_KEY;
    use crate::scoring::generation::scoring_revision;

    fn test_db() -> Arc<dyn Database> {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
        create_tables(&conn).expect("schema");
        Arc::new(SqliteDatabase::new(conn))
    }

    /// Put `did` in the state `record_full_scan_completion` is called from:
    /// a running full-scan row this worker holds the claim on. Required since
    /// the bookkeeping is fenced by that claim (N2).
    async fn claimed(db: &Arc<dyn Database>, did: &str) -> String {
        db.enqueue_scan(did).await.expect("enqueue");
        db.claim_next_scan(8, 60)
            .await
            .expect("claim")
            .expect("a queued row exists")
            .claim_id
    }

    /// F2: the ownership probe the wrapper's fence reads. False for a row that
    /// does not exist, a row nobody holds, and a row someone else holds —
    /// the same three cases `finish_full_scan_state` refuses to write under.
    #[tokio::test]
    async fn claim_ownership_is_false_unless_this_claim_holds_the_row() {
        let db = test_db();
        db.upsert_user("did:plc:own", "own.h").await.unwrap();
        assert!(
            !db.scan_claim_is_current("did:plc:own", "claim-1")
                .await
                .unwrap(),
            "no row at all"
        );
        // Queued, so claim_id is still NULL.
        db.enqueue_scan("did:plc:own").await.unwrap();
        assert!(
            !db.scan_claim_is_current("did:plc:own", "claim-1")
                .await
                .unwrap(),
            "unclaimed"
        );
        let claim = claimed(&db, "did:plc:own").await;
        assert!(db
            .scan_claim_is_current("did:plc:own", &claim)
            .await
            .unwrap());
        assert!(
            !db.scan_claim_is_current("did:plc:own", "someone-else")
                .await
                .unwrap(),
            "a foreign claim"
        );
    }

    #[test]
    fn fulfilled_covers_every_completion_but_resumable() {
        assert!(ScanCompletion::Complete.fulfilled());
        assert!(ScanCompletion::CompleteWithSkips { n: 3 }.fulfilled());
        assert!(ScanCompletion::CompleteUnverified.fulfilled());
        assert!(!ScanCompletion::Resumable.fulfilled());
    }

    #[test]
    fn worst_is_commutative_and_ordered_by_severity() {
        use ScanCompletion::*;
        let all = [
            Complete,
            CompleteWithSkips { n: 2 },
            CompleteUnverified,
            Resumable,
        ];
        for a in all {
            for b in all {
                assert_eq!(a.worst(b), b.worst(a), "worst({a:?}, {b:?}) is symmetric");
            }
            assert_eq!(a.worst(Resumable), Resumable, "Resumable absorbs {a:?}");
        }
        assert_eq!(
            CompleteUnverified.worst(CompleteWithSkips { n: 9 }),
            CompleteUnverified,
            "an unverifiable count beats a known one"
        );
        assert_eq!(
            CompleteWithSkips { n: 2 }.worst(CompleteWithSkips { n: 3 }),
            CompleteWithSkips { n: 5 },
            "skip counts add"
        );
        assert_eq!(
            CompleteWithSkips { n: 4 }.worst(Complete),
            CompleteWithSkips { n: 4 }
        );
        assert_eq!(Complete.worst(Complete), Complete);
    }

    /// Args are `(degraded, scan_phase, skipped)`.
    #[test]
    fn classification_reads_the_marker_and_the_persisted_skip_count() {
        use ScanCompletion::*;
        let cases: &[(bool, Option<&str>, Option<i64>, ScanCompletion)] = &[
            (false, Some("done"), Some(0), Complete),
            (true, Some("done"), Some(3), CompleteWithSkips { n: 3 }),
            // V5-01: a clean RESUME over skips an earlier invocation persisted.
            // The flag describes this attempt; `scan_skips` describes the scan.
            (false, Some("done"), Some(2), CompleteWithSkips { n: 2 }),
            // Degraded with no recorded skip: something else went wrong
            // (decode sentinels, #355). Fulfilled, not clean.
            (true, Some("done"), Some(0), CompleteWithSkips { n: 0 }),
            // V5-01: the count could not be read — fulfilled, never proof.
            (false, Some("done"), None, CompleteUnverified),
            (true, Some("done"), None, CompleteUnverified),
            // V4-02: unfinished staging is resumable whatever the flag says.
            (true, Some("burst"), Some(0), Resumable),
            (true, None, Some(0), Resumable),
            (false, Some("burst"), Some(0), Resumable),
            (false, Some("finalize"), Some(0), Resumable),
            (false, None, Some(0), Resumable),
            (false, Some("burst"), None, Resumable),
            (false, Some("gather"), Some(0), Resumable),
        ];
        for (degraded, phase, skipped, expected) in cases {
            assert_eq!(
                classify_full_scan(*degraded, *phase, *skipped),
                *expected,
                "classify_full_scan({degraded}, {phase:?}, {skipped:?})"
            );
        }
    }

    #[test]
    fn the_durable_completion_mirrors_the_classification() {
        use crate::db::FinishCompletion as F;
        assert_eq!(F::from(ScanCompletion::Complete), F::Complete);
        assert_eq!(
            F::from(ScanCompletion::CompleteWithSkips { n: 1 }),
            F::CompleteWithSkips,
            "the durable row records THAT there were skips, not how many"
        );
        assert_eq!(
            F::from(ScanCompletion::CompleteUnverified),
            F::CompleteUnverified
        );
        assert_eq!(F::from(ScanCompletion::Resumable), F::Resumable);
    }

    /// V6-01: the cooldown anchor is written for every fulfilled completion —
    /// including one that could not verify itself — and never for a resumable
    /// attempt, which the user must be able to retry at once.
    #[tokio::test]
    async fn the_marker_is_written_for_fulfilment_only() {
        for (completion, expected) in [
            (ScanCompletion::Complete, true),
            (ScanCompletion::CompleteWithSkips { n: 2 }, true),
            (ScanCompletion::CompleteUnverified, true),
            (ScanCompletion::Resumable, false),
        ] {
            let db = test_db();
            let claim = claimed(&db, "did:plc:m").await;
            record_full_scan_completion(db.as_ref(), "did:plc:m", &claim, completion).await;
            assert_eq!(
                db.get_scan_state("did:plc:m", "last_full_scan_finished_at")
                    .await
                    .unwrap()
                    .is_some(),
                expected,
                "{completion:?}"
            );
        }
    }

    /// V7-02: the carried drain outcome is retired by the SAME call that
    /// anchors the cooldown, and kept by an attempt that is still owed.
    /// (`drain_then_run`, which writes the value, is Task 9's; the key's
    /// retirement is this task's.)
    #[tokio::test]
    async fn a_fulfilled_run_consumes_the_carried_outcome_and_a_resumable_one_keeps_it() {
        let db = test_db();
        let claim = claimed(&db, "did:plc:g").await;
        let carried = format!("{}|skips|1", scoring_revision());
        db.set_scan_state("did:plc:g", CARRIED_KEY, &carried)
            .await
            .unwrap();
        record_full_scan_completion(db.as_ref(), "did:plc:g", &claim, ScanCompletion::Resumable)
            .await;
        assert_eq!(
            db.get_scan_state("did:plc:g", CARRIED_KEY).await.unwrap(),
            Some(carried),
            "a resumable attempt keeps it — the run is still owed"
        );
        record_full_scan_completion(
            db.as_ref(),
            "did:plc:g",
            &claim,
            ScanCompletion::CompleteWithSkips { n: 1 },
        )
        .await;
        assert_eq!(
            db.get_scan_state("did:plc:g", CARRIED_KEY).await.unwrap(),
            None
        );
        assert!(db
            .get_scan_state("did:plc:g", "last_full_scan_finished_at")
            .await
            .unwrap()
            .is_some());
    }

    /// The marker write and the carried-key delete are ONE transaction, and
    /// the delete is scoped to that key alone.
    #[tokio::test]
    async fn finishing_full_scan_state_leaves_every_other_key_alone() {
        let db = test_db();
        let claim = claimed(&db, "did:plc:k").await;
        db.set_scan_state("did:plc:k", CARRIED_KEY, "whatever")
            .await
            .unwrap();
        db.set_scan_state("did:plc:k", "scan_phase", "done")
            .await
            .unwrap();
        db.finish_full_scan_state(
            "did:plc:k",
            "2026-09-14T00:00:00+00:00",
            CARRIED_KEY,
            &claim,
        )
        .await
        .unwrap();
        assert_eq!(
            db.get_scan_state("did:plc:k", "last_full_scan_finished_at")
                .await
                .unwrap()
                .as_deref(),
            Some("2026-09-14T00:00:00+00:00")
        );
        assert_eq!(
            db.get_scan_state("did:plc:k", CARRIED_KEY).await.unwrap(),
            None
        );
        assert_eq!(
            db.get_scan_state("did:plc:k", "scan_phase")
                .await
                .unwrap()
                .as_deref(),
            Some("done"),
            "the delete is scoped to one key"
        );
    }

    /// N2: the bookkeeping is fenced by the claim. A worker whose lease
    /// lapsed — its row reclaimed and handed to a successor — writes NOTHING:
    /// no cooldown anchor for a request it no longer owns, no ETA sample from
    /// the successor's clock, and no retirement of a drain outcome the
    /// successor still owes.
    #[tokio::test]
    async fn a_superseded_worker_records_no_completion() {
        let db = test_db();
        db.enqueue_scan("did:plc:zombie").await.unwrap();
        // A lease that is already expired, so the reclaim below is the real
        // per-pass reclaim rather than a hand-edited row.
        let stale = db
            .claim_next_scan(8, -1)
            .await
            .unwrap()
            .expect("a queued row exists")
            .claim_id;
        db.set_scan_state("did:plc:zombie", CARRIED_KEY, "carried")
            .await
            .unwrap();
        assert_eq!(db.reclaim_expired_scans().await.unwrap(), 1);
        let successor = db
            .claim_next_scan(8, 60)
            .await
            .unwrap()
            .expect("the successor claims the re-queued row")
            .claim_id;
        assert_ne!(successor, stale);

        record_full_scan_completion(
            db.as_ref(),
            "did:plc:zombie",
            &stale,
            ScanCompletion::Complete,
        )
        .await;
        assert_eq!(
            db.get_scan_state("did:plc:zombie", "last_full_scan_finished_at")
                .await
                .unwrap(),
            None,
            "no cooldown anchor from a worker that lost its claim"
        );
        assert_eq!(
            db.get_scan_state("did:plc:zombie", LAST_FULL_SCAN_DURATION_KEY)
                .await
                .unwrap(),
            None,
            "no ETA sample from the successor's started_at"
        );
        assert_eq!(
            db.get_scan_state("did:plc:zombie", CARRIED_KEY)
                .await
                .unwrap()
                .as_deref(),
            Some("carried"),
            "the successor's obligation is not retired by its predecessor"
        );

        // The successor, holding the real claim, writes all three.
        record_full_scan_completion(
            db.as_ref(),
            "did:plc:zombie",
            &successor,
            ScanCompletion::Complete,
        )
        .await;
        assert!(db
            .get_scan_state("did:plc:zombie", "last_full_scan_finished_at")
            .await
            .unwrap()
            .is_some());
        assert_eq!(
            db.get_scan_state("did:plc:zombie", CARRIED_KEY)
                .await
                .unwrap(),
            None
        );
    }

    /// N4: the ETA median describes FULL scans. A refresh finishing cleanly
    /// runs the whole queue lifecycle but never calls the full-scan
    /// bookkeeping, so it records no duration sample — a refresh is a
    /// fraction of a full scan's work, and one folded into the median would
    /// quote every queued user an ETA they cannot get.
    #[tokio::test]
    async fn a_refresh_completion_records_no_duration_sample() {
        let db = test_db();
        db.enqueue_refresh_scan("did:plc:r").await.unwrap();
        let claim = db.claim_next_scan(8, 60).await.unwrap().unwrap();
        assert_eq!(claim.kind, crate::db::ScanKind::Refresh);
        db.finish_queued_scan(
            "did:plc:r",
            &claim.claim_id,
            crate::db::FinishCompletion::Complete,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            db.get_scan_state("did:plc:r", LAST_FULL_SCAN_DURATION_KEY)
                .await
                .unwrap(),
            None,
            "a refresh duration must never reach the full-scan ETA median"
        );
        assert_eq!(
            db.get_scan_state("did:plc:r", "last_full_scan_finished_at")
                .await
                .unwrap(),
            None,
            "and it anchors no cooldown either"
        );
    }

    #[tokio::test]
    async fn deleting_an_absent_scan_state_key_is_not_an_error() {
        let db = test_db();
        db.delete_scan_state("did:plc:absent", CARRIED_KEY)
            .await
            .expect("absent is the state the caller wanted");
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
    fn unparseable_timestamp_is_treated_as_stale() {
        // A malformed timestamp must trigger the rebuild path: a successful
        // rebuild rewrites updated_at via the DB clock, so a one-off corrupt
        // row self-heals after one rebuild. Treating it as fresh would keep
        // an old fingerprint on the fresh path FOREVER, silently bypassing
        // the 14-day refresh. (#303, CodeRabbit PR #101; supersedes the
        // original #296 plan decision.)
        assert!(fingerprint_is_stale("not a date", now()));
        assert!(fingerprint_is_stale("", now()));
    }

    #[test]
    fn legacy_format_forces_rebuild() {
        // Embedding present but zero centroid rows = pre-#297 generation.
        assert!(fingerprint_needs_rebuild_for_format(true, 0, 3));
        // Keyword-only fingerprint (no embedding) is NOT legacy — no rebuild loop.
        assert!(!fingerprint_needs_rebuild_for_format(false, 0, 3));
        // Row/JSON mismatch (pre-#302 divergence) rebuilds.
        assert!(fingerprint_needs_rebuild_for_format(true, 2, 3));
        // Healthy clustered generation: no rebuild.
        assert!(!fingerprint_needs_rebuild_for_format(true, 3, 3));
    }
}

/// `record_observed_rate_limit`'s two branches — the `Some` persistence path
/// and the `None` warn-only path — were split out of `run_scan` (review
/// finding, PR round 2) specifically so both are unit-testable without
/// running a whole scan.
#[cfg(test)]
mod rate_limit_persistence_tests {
    use super::*;

    use std::sync::{Mutex, PoisonError};

    use crate::db::schema::create_tables;
    use crate::db::sqlite::SqliteDatabase;

    const DID: &str = "did:plc:ratelimit";

    fn test_db() -> Arc<dyn Database> {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
        create_tables(&conn).expect("schema");
        Arc::new(SqliteDatabase::new(conn))
    }

    /// Collects every event's rendered field set, keyed by field name. Mirrors
    /// the `DbWriteSpans` layer in `tests/unit_actions_runner.rs` — same
    /// callsite-interest-caching quirk applies (see the comment on
    /// `try_init()` below), so the pattern is copied rather than reinvented.
    #[derive(Clone, Default)]
    struct CapturedEvents(Arc<Mutex<Vec<String>>>);

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CapturedEvents {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *event.metadata().level() != tracing::Level::WARN {
                return;
            }
            struct MessageOnly(Option<String>);
            impl tracing::field::Visit for MessageOnly {
                fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
                    if f.name() == "message" {
                        self.0 = Some(format!("{v:?}"));
                    }
                }
            }
            let mut msg = MessageOnly(None);
            event.record(&mut msg);
            if let Some(m) = msg.0 {
                self.0
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(m);
            }
        }
    }

    #[tokio::test]
    async fn observed_rate_limit_is_persisted_to_scan_state() {
        let db = test_db();

        record_observed_rate_limit(db.as_ref(), DID, Some(3000)).await;

        assert_eq!(
            db.get_scan_state(DID, "bluesky_ratelimit_limit")
                .await
                .expect("scan_state read"),
            Some("3000".to_string())
        );
    }

    #[tokio::test]
    async fn missing_rate_limit_warns_once_and_writes_nothing() {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let db = test_db();

        // tracing caches each callsite's interest the first time ANY thread
        // hits it, against whichever subscriber is the *global* default at
        // that moment. Without this, a parallel test run can cache this
        // warn!() site as never-interested before this test's scoped
        // subscriber is installed, and the event never reaches our layer.
        // A bare global Registry pins the cache to "always"; it drops events
        // itself and the scoped layer below still sees only this thread's
        // events, since `set_default` only affects the current thread and
        // `#[tokio::test]` is a current-thread runtime.
        let _ = tracing_subscriber::registry().try_init();
        let captured = CapturedEvents::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let _guard = tracing::subscriber::set_default(subscriber);

        record_observed_rate_limit(db.as_ref(), DID, None).await;

        drop(_guard);

        let seen = captured
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(seen.len(), 1, "expected exactly one warn event: {seen:?}");
        assert!(
            seen[0].contains("no RateLimit-Limit header observed"),
            "unexpected warn message: {seen:?}"
        );

        assert_eq!(
            db.get_scan_state(DID, "bluesky_ratelimit_limit")
                .await
                .expect("scan_state read"),
            None,
            "the None branch must not write a scan_state row"
        );
    }
}

/// The full scan's bookkeeping boundary (#344 V5-02, V6-01, V6-02, V7-01).
///
/// Driven with a closure in place of the pipeline, so none of it is
/// model-gated: the property under test is "every exit reaches exactly one
/// scheduling site, and the deadline it sets is anchored on the attempt's
/// end", which has nothing to do with ONNX.
#[cfg(test)]
mod bookkeeping_tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    use crate::db::schema::create_tables;
    use crate::db::sqlite::SqliteDatabase;
    use crate::db::FinishCompletion;
    use crate::scoring::generation::scoring_revision;
    use crate::web::refresh::REFRESH_RETRY_HOURS;

    fn test_db() -> Arc<dyn Database> {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
        create_tables(&conn).expect("schema");
        Arc::new(SqliteDatabase::new(conn))
    }

    fn manager_with_running_scan(did: &str, claim_id: &str) -> Arc<RwLock<ScanManager>> {
        let mut mgr = ScanManager::new();
        mgr.begin_admitted_scan(did, claim_id);
        Arc::new(RwLock::new(mgr))
    }

    /// The real bookkeeping with a tally on top. Counting the calls is what
    /// makes "exactly one retry per failed attempt, and no other scheduling
    /// call" assertable — asserting only on the database cannot tell one
    /// write from three identical ones.
    struct CountingFullBooks {
        inner: DbFullScanBookkeeping,
        retries: AtomicUsize,
        successes: AtomicUsize,
        markers: AtomicUsize,
    }

    /// The nightly cadence, passed explicitly so no test reads — or writes —
    /// `CHARCOAL_REFRESH_INTERVAL_HOURS` (#344 F3).
    const TEST_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

    impl CountingFullBooks {
        fn new(db: Arc<dyn Database>) -> Self {
            Self {
                inner: DbFullScanBookkeeping::new(db, TEST_REFRESH_INTERVAL),
                retries: 0.into(),
                successes: 0.into(),
                markers: 0.into(),
            }
        }
    }

    #[async_trait::async_trait]
    impl FullScanBookkeeping for CountingFullBooks {
        // Delegated, not stubbed: the fence under test is the real database
        // one, driven by a real reclaim.
        async fn owns_claim(&self, user_did: &str, claim_id: &str) -> bool {
            self.inner.owns_claim(user_did, claim_id).await
        }

        async fn record_completion(
            &self,
            user_did: &str,
            claim_id: &str,
            completion: ScanCompletion,
        ) {
            // Only fulfilled completions write anything, so only those count
            // as markers.
            if completion.fulfilled() {
                self.markers.fetch_add(1, SeqCst);
            }
            self.inner
                .record_completion(user_did, claim_id, completion)
                .await;
        }

        async fn schedule_success(&self, user_did: &str, now: DateTime<Utc>) {
            self.successes.fetch_add(1, SeqCst);
            self.inner.schedule_success(user_did, now).await;
        }

        async fn schedule_retry(&self, user_did: &str, now: DateTime<Utc>) {
            self.retries.fetch_add(1, SeqCst);
            self.inner.schedule_retry(user_did, now).await;
        }
    }

    async fn queue_row(db: &Arc<dyn Database>, did: &str) -> crate::db::traits::ScanQueueRow {
        db.list_scan_queue()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.user_did == did)
            .expect("row exists")
    }

    /// V5-02: a scheduler-created owed full scan whose setup fails before any
    /// score write still gets exactly one hourly retry, keeps its obligation,
    /// writes no marker and proves nothing — and the tick honours THAT
    /// deadline, not the nightly one it set when it queued the job.
    #[tokio::test]
    async fn a_full_scan_setup_failure_schedules_the_hourly_retry_and_keeps_the_obligation() {
        let db = test_db();
        db.upsert_user("did:plc:owed", "owed.h").await.unwrap();
        let now = chrono::Utc::now();
        // The user asked for a full scan earlier; the tick queued the retry
        // and set the NIGHTLY deadline (as claim_and_enqueue_due_refreshes does).
        db.enqueue_scan("did:plc:owed").await.unwrap();
        db.schedule_refresh(
            "did:plc:owed",
            &(now + chrono::Duration::hours(24)).to_rfc3339(),
        )
        .await
        .unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        let obligation = queue_row(&db, "did:plc:owed")
            .await
            .full_requested_at
            .expect("the obligation is recorded at enqueue");
        let mgr = manager_with_running_scan("did:plc:owed", &claim.claim_id);
        let books = CountingFullBooks::new(db.clone());

        for failure in [
            "fingerprint rebuild required (IncompatibleModel) and failed — scan aborted before scoring",
            "CHARCOAL_CLASSIFIER is unset — build_from_env failed",
        ] {
            let result = run_scan_with(
                mgr.clone(),
                &books,
                &chrono::Utc::now,
                "did:plc:owed",
                &claim.claim_id,
                move || async move { anyhow::bail!("{failure}") },
            )
            .await;
            assert!(result.is_err());
        }
        assert_eq!(
            books.retries.load(SeqCst),
            2,
            "one retry per failed attempt — no other scheduling call"
        );
        assert_eq!(books.successes.load(SeqCst), 0);
        assert_eq!(books.markers.load(SeqCst), 0, "no completion marker");
        assert!(db
            .get_scan_state("did:plc:owed", "last_full_scan_finished_at")
            .await
            .unwrap()
            .is_none());
        assert_ne!(
            db.refreshed_generation("did:plc:owed")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision()),
            "no proof"
        );
        let row = queue_row(&db, "did:plc:owed").await;
        assert_eq!(
            row.full_requested_at.as_deref(),
            Some(obligation.as_str()),
            "obligation kept"
        );
        // The retry deadline REPLACED the nightly one.
        let next = chrono::DateTime::parse_from_rfc3339(
            &db.next_refresh_at("did:plc:owed").await.unwrap().unwrap(),
        )
        .unwrap();
        let delta = next.signed_duration_since(chrono::Utc::now());
        assert!(
            delta > chrono::Duration::minutes(55) && delta <= chrono::Duration::hours(1),
            "hourly, not nightly: {delta}"
        );
        // Finish the row as the slot would after the second failure, then tick.
        db.finish_queued_scan(
            "did:plc:owed",
            &claim.claim_id,
            FinishCompletion::Failed,
            Some("setup failed"),
        )
        .await
        .unwrap();
        assert_eq!(
            crate::web::refresh::enqueue_due_refreshes(
                &db,
                now + chrono::Duration::seconds(30),
                std::time::Duration::from_secs(24 * 3600)
            )
            .await,
            0
        );
        assert_eq!(
            crate::web::refresh::enqueue_due_refreshes(
                &db,
                now + chrono::Duration::hours(2),
                std::time::Duration::from_secs(24 * 3600)
            )
            .await,
            1
        );
        let row = queue_row(&db, "did:plc:owed").await;
        assert_eq!(
            (row.status.as_str(), row.kind),
            ("queued", crate::db::ScanKind::Full)
        );
    }

    /// A settable clock for the wrapper: the `run` closure advances it, so a
    /// ninety-minute attempt takes no wall time (V6-02).
    struct FakeClock(std::sync::Arc<std::sync::Mutex<DateTime<Utc>>>);

    impl FakeClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    /// V6-02: the retry deadline is attempt END + REFRESH_RETRY_HOURS, even
    /// when the attempt itself outlasts the retry window. Anchored on the
    /// start, a two-hour failure would be "due" the moment it gave up and the
    /// next tick would re-run it immediately, forever.
    #[tokio::test]
    async fn a_failed_full_scan_is_retried_an_hour_after_it_ended_not_began() {
        let db = test_db();
        db.upsert_user("did:plc:slow", "slow.h").await.unwrap();
        db.enqueue_scan("did:plc:slow").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        let mgr = manager_with_running_scan("did:plc:slow", &claim.claim_id);
        let books = CountingFullBooks::new(db.clone());
        let t0 = chrono::Utc::now();
        let clock = FakeClock(std::sync::Arc::new(std::sync::Mutex::new(t0)));
        let advance = clock.0.clone();
        let result = run_scan_with(
            mgr,
            &books,
            &|| clock.now(),
            "did:plc:slow",
            &claim.claim_id,
            move || async move {
                // the attempt takes 90 min
                *advance.lock().unwrap() += chrono::Duration::minutes(90);
                anyhow::bail!("classifier down for the whole attempt")
            },
        )
        .await;
        assert!(result.is_err());
        let end = t0 + chrono::Duration::minutes(90);
        let deadline = chrono::DateTime::parse_from_rfc3339(
            &db.next_refresh_at("did:plc:slow").await.unwrap().unwrap(),
        )
        .unwrap();
        assert_eq!(
            deadline,
            end + chrono::Duration::hours(REFRESH_RETRY_HOURS as i64),
            "anchored on the END of the attempt"
        );
        assert_eq!(books.retries.load(SeqCst), 1);
        db.finish_queued_scan(
            "did:plc:slow",
            &claim.claim_id,
            FinishCompletion::Failed,
            Some("down"),
        )
        .await
        .unwrap();
        // The tick honours that deadline.
        assert_eq!(
            crate::web::refresh::enqueue_due_refreshes(
                &db,
                end + chrono::Duration::minutes(30),
                std::time::Duration::from_secs(24 * 3600)
            )
            .await,
            0,
            "still inside the backoff"
        );
        assert_eq!(
            crate::web::refresh::enqueue_due_refreshes(
                &db,
                end + chrono::Duration::minutes(61),
                std::time::Duration::from_secs(24 * 3600)
            )
            .await,
            1
        );
        let row = queue_row(&db, "did:plc:slow").await;
        assert_eq!(
            (row.status.as_str(), row.kind),
            ("queued", crate::db::ScanKind::Full),
            "owed work retried as full"
        );
    }

    /// V6-01: an unverified completion is FULFILLED — marker written,
    /// obligation cleared, durable completion `complete_unverified` — but it
    /// is not proof: the retry is scheduled, the revision is not marked.
    #[tokio::test]
    async fn an_unverified_completion_is_fulfilled_but_not_proof() {
        let db = test_db();
        db.upsert_user("did:plc:unv", "unv.h").await.unwrap();
        db.enqueue_scan("did:plc:unv").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        let mgr = manager_with_running_scan("did:plc:unv", &claim.claim_id);
        let books = CountingFullBooks::new(db.clone());
        let report = run_scan_with(
            mgr,
            &books,
            &chrono::Utc::now,
            "did:plc:unv",
            &claim.claim_id,
            || async {
                Ok(FullScanRun {
                    events: 3,
                    scored: 3,
                    completion: ScanCompletion::CompleteUnverified,
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(report.completion, FinishCompletion::CompleteUnverified);
        assert_eq!(
            (
                books.markers.load(SeqCst),
                books.retries.load(SeqCst),
                books.successes.load(SeqCst)
            ),
            (1, 1, 0)
        );
        assert!(
            db.get_scan_state("did:plc:unv", "last_full_scan_finished_at")
                .await
                .unwrap()
                .is_some(),
            "cooldown anchored: the request was carried out"
        );
        assert_ne!(
            db.refreshed_generation("did:plc:unv")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision()),
            "no proof"
        );
        // The slot finishes the row from the report:
        db.finish_queued_scan("did:plc:unv", &claim.claim_id, report.completion, None)
            .await
            .unwrap();
        let row = queue_row(&db, "did:plc:unv").await;
        assert_eq!(row.completion, Some(FinishCompletion::CompleteUnverified));
        assert!(
            row.full_requested_at.is_none(),
            "obligation cleared — fulfilled"
        );
    }

    /// A clean completion is the only thing that proves the revision and
    /// earns the nightly cadence — the other half of the `Complete` arm.
    #[tokio::test]
    async fn a_clean_completion_proves_the_revision_and_schedules_the_nightly() {
        let db = test_db();
        db.upsert_user("did:plc:clean", "clean.h").await.unwrap();
        db.enqueue_scan("did:plc:clean").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        let mgr = manager_with_running_scan("did:plc:clean", &claim.claim_id);
        // No env mutation: the cadence comes from TEST_REFRESH_INTERVAL.
        let books = CountingFullBooks::new(db.clone());
        let report = run_scan_with(
            mgr,
            &books,
            &chrono::Utc::now,
            "did:plc:clean",
            &claim.claim_id,
            || async {
                Ok(FullScanRun {
                    events: 1,
                    scored: 1,
                    completion: ScanCompletion::Complete,
                })
            },
        )
        .await
        .unwrap();
        assert_eq!(report.completion, FinishCompletion::Complete);
        assert_eq!(
            (
                books.markers.load(SeqCst),
                books.retries.load(SeqCst),
                books.successes.load(SeqCst)
            ),
            (1, 0, 1),
            "one marker, one success, and NO retry"
        );
        assert_eq!(
            db.refreshed_generation("did:plc:clean")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision()),
            "proven"
        );
        let next = chrono::DateTime::parse_from_rfc3339(
            &db.next_refresh_at("did:plc:clean").await.unwrap().unwrap(),
        )
        .unwrap();
        let delta = next.signed_duration_since(chrono::Utc::now());
        assert!(
            delta > chrono::Duration::hours(23),
            "the nightly cadence, not the hourly retry: {delta}"
        );
    }

    /// F2: a worker whose lease lapsed writes NONE of the three — no marker,
    /// no ETA duration sample, no refresh schedule — and the successor's own
    /// writes survive it.
    ///
    /// The zombie is the slow one by definition, so its write lands last: an
    /// unfenced `schedule_success` would overwrite the successor's deadline
    /// and proof for a user it no longer owns. The mismatch is produced the
    /// way production produces it (claim → reclaim → successor claim), never
    /// by editing `claim_id` by hand.
    #[tokio::test]
    async fn a_superseded_worker_writes_no_marker_sample_or_schedule() {
        let db = test_db();
        db.upsert_user("did:plc:zombie", "zombie.h").await.unwrap();
        db.enqueue_scan("did:plc:zombie").await.unwrap();
        // An already-expired lease, so the reclaim below has something to take.
        let zombie = db.claim_next_scan(1, -1).await.unwrap().unwrap();
        assert_eq!(db.reclaim_expired_scans().await.unwrap(), 1);
        let successor = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        assert_ne!(zombie.claim_id, successor.claim_id);

        // The successor finishes first and writes all three.
        let mgr = manager_with_running_scan("did:plc:zombie", &successor.claim_id);
        let books = CountingFullBooks::new(db.clone());
        let t0 = chrono::Utc::now();
        run_scan_with(
            mgr.clone(),
            &books,
            &move || t0,
            "did:plc:zombie",
            &successor.claim_id,
            || async {
                Ok(FullScanRun {
                    events: 1,
                    scored: 1,
                    completion: ScanCompletion::Complete,
                })
            },
        )
        .await
        .unwrap();
        let marker = db
            .get_scan_state("did:plc:zombie", "last_full_scan_finished_at")
            .await
            .unwrap()
            .expect("the successor anchored the cooldown");
        let sample = db
            .get_scan_state(
                "did:plc:zombie",
                crate::db::traits::LAST_FULL_SCAN_DURATION_KEY,
            )
            .await
            .unwrap();
        let deadline = db.next_refresh_at("did:plc:zombie").await.unwrap();
        assert_eq!(
            deadline.as_deref(),
            Some((t0 + chrono::Duration::hours(24)).to_rfc3339()).as_deref(),
            "the successor scheduled the nightly"
        );

        // Now the zombie's attempt lands, an hour later.
        let late = t0 + chrono::Duration::hours(1);
        run_scan_with(
            mgr,
            &books,
            &move || late,
            "did:plc:zombie",
            &zombie.claim_id,
            || async {
                Ok(FullScanRun {
                    events: 9,
                    scored: 9,
                    completion: ScanCompletion::Complete,
                })
            },
        )
        .await
        .unwrap();

        assert_eq!(
            (
                books.markers.load(SeqCst),
                books.successes.load(SeqCst),
                books.retries.load(SeqCst)
            ),
            (1, 1, 0),
            "the zombie reached NO scheduling call — the successor's one of each is all there is"
        );
        assert_eq!(
            db.get_scan_state("did:plc:zombie", "last_full_scan_finished_at")
                .await
                .unwrap(),
            Some(marker),
            "cooldown anchor untouched"
        );
        assert_eq!(
            db.get_scan_state(
                "did:plc:zombie",
                crate::db::traits::LAST_FULL_SCAN_DURATION_KEY
            )
            .await
            .unwrap(),
            sample,
            "ETA sample untouched"
        );
        assert_eq!(
            db.next_refresh_at("did:plc:zombie").await.unwrap(),
            deadline,
            "the successor's deadline survived the zombie's later write"
        );
    }

    /// V7-01: the wrapper futures must be `Send` — `launch_scan` puts them
    /// inside `tokio::spawn`. A directly awaited test proves nothing about
    /// that; this compile-time check does, for the production clock type.
    #[test]
    fn wrapper_futures_are_send() {
        fn assert_send<F: Send>(_: &F) {}
        let db = test_db();
        let books = DbFullScanBookkeeping::new(db.clone(), TEST_REFRESH_INTERVAL);
        let mgr = manager_with_running_scan("did:plc:u", "c");
        let fut = run_scan_with(mgr, &books, &chrono::Utc::now, "did:plc:u", "c", || async {
            Ok(FullScanRun {
                events: 0,
                scored: 0,
                completion: ScanCompletion::Complete,
            })
        });
        assert_send(&fut);
        drop(fut);
    }
}
