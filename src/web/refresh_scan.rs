//! The nightly refresh job (#343 §4.4, #344 Task 9).
//!
//! A refresh re-scores the accounts whose High/Elevated scores are expiring.
//! It shares the *scoring* half of a full scan — the same gather → burst →
//! finalize pipeline, the same models, the same evidence contract — and none
//! of the *discovery* half:
//!
//! - no Constellation amplification query, no like/reply detection,
//! - no follower expansion,
//! - no `classify_relationships` (the stored graph distance is reused),
//! - no fingerprint build or rebuild: a refresh that finds no fingerprint, or
//!   one built by another embedding model, **defers** and asks for a full
//!   scan instead (R03, V2-04). Rebuilding here would let the nightly job
//!   silently re-derive the protected user's topics at 3am.
//!
//! The candidate list comes from `list_refresh_candidates`, and every
//! per-candidate input (pile-on membership, direct NLI pairs, graph distance)
//! is read from what earlier scans already stored.
//!
//! Two invariants shape the rest of the module:
//!
//! **Errors are not absence (R05).** Every context load returns `Result`, and
//! a failure fails the whole run: no score is written, the existing row keeps
//! its own expiry, and the attempt is retried in an hour. A refresh that
//! quietly scored a High account with missing context would write a *lower*
//! score, stamp it current, and hide the real state until it expired.
//!
//! **Completion is explicit (V2-05).** `Ok` from the pipeline is not
//! completion. Only `Completed`/`NothingDue` prove the revision and earn the
//! nightly cadence; everything else retries within the hour and shows as
//! degraded. The full scan's cooldown marker is never written by a refresh.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::relationships::GraphDistance;
use crate::config::Config;
use crate::db::{Database, FinishCompletion, RefreshCandidate, ScanKind};
use crate::pipeline::scan_phases::{
    has_own_resumable_staging, CandidateInput, PhasedScanError, RunIdentity, ScanSummary,
};
use crate::topics::embeddings::EMBEDDING_MODEL_ID;
use crate::topics::fingerprint::TopicFingerprint;
use crate::web::scan_job::{finish_scan, set_progress, ScanManager, ScanReport, WebScanPhase};
use crate::web::scan_setup::{
    build_scan_scorers, embed_protected_posts, pile_on_dids, record_scan_cache_stats, ScanModels,
};

/// How far ahead of expiry a High/Elevated row becomes a refresh candidate.
///
/// Two days: the tick runs nightly, so a one-day horizon would leave a row
/// that expires shortly after tonight's run unrefreshed until tomorrow's.
pub const REFRESH_HORIZON_DAYS: i64 = 2;

/// Gather width for a refresh. The same literal the full scan uses today;
/// Phase 3 replaces both with one tuned value.
const REFRESH_GATHER_CONCURRENCY: usize = 8;

/// What a stored fingerprint bundle looks like to the refresh: the parsed
/// fingerprint, the mean embedding, the per-topic centroids, and the id of the
/// embedding model that produced the vectors.
///
/// Aliased because it travels through a trait method signature and a 4-tuple
/// spelled out there reads as noise.
pub type StoredFingerprint = (
    TopicFingerprint,
    Option<Vec<f64>>,
    Vec<Vec<f64>>,
    Option<String>,
);

/// Why a refresh declined to run this time. None of these are errors: the work
/// is simply not this job's to do right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferReason {
    /// A full scan left resumable staging. It drains its own; the refresh waits.
    FullScanResumable,
    /// No fingerprint at all — a refresh never builds one.
    NoFingerprint,
    /// The stored vectors came from another embedding model, so scoring against
    /// them would compare across generations. A refresh never rebuilds.
    IncompatibleFingerprint,
}

impl DeferReason {
    fn label(self) -> &'static str {
        match self {
            DeferReason::FullScanResumable => "full_scan_resumable",
            DeferReason::NoFingerprint => "no_fingerprint",
            DeferReason::IncompatibleFingerprint => "incompatible_fingerprint",
        }
    }
}

/// How a refresh ended (V2-05). `Ok` from the pipeline is never "complete".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    /// Every candidate re-scored and the staging drained.
    Completed {
        candidates: usize,
        scored: usize,
    },
    /// Drained, but some accounts were skipped — they stay expired until the
    /// hourly retry picks them up. The successful writes are kept.
    CompletedWithSkips {
        candidates: usize,
        scored: usize,
        skipped: usize,
    },
    /// Drained to `done` but the skip count could not be read (V5-01):
    /// fulfilled, never proof.
    CompletedUnverified {
        candidates: usize,
        scored: usize,
    },
    /// Nothing was due. A real answer, and a proof of the revision: there is
    /// nothing stale left for this user.
    NothingDue,
    Deferred(DeferReason),
    /// Cost cap or transient interruption: markers left, owned by this kind.
    Resumable,
}

/// What an outcome does to the schedule, the proof and the status (V2-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bookkeeping {
    /// Stamp `refreshed_generation` and take the nightly cadence.
    pub prove_revision: bool,
    /// Try again in [`crate::web::refresh::REFRESH_RETRY_HOURS`].
    pub retry: bool,
    /// Ask for a follow-up full scan on this refresh's own queue row.
    pub request_full: bool,
    /// Show as degraded, so partial work does not read as clean.
    pub degraded: bool,
}

impl RefreshOutcome {
    /// Stable `scan_state.refresh_last_outcome` value; the runbook greps it.
    pub fn label(&self) -> String {
        match self {
            RefreshOutcome::Completed { .. } => "completed".to_string(),
            RefreshOutcome::CompletedWithSkips { .. } => "completed_with_skips".to_string(),
            RefreshOutcome::CompletedUnverified { .. } => "completed_unverified".to_string(),
            RefreshOutcome::NothingDue => "nothing_due".to_string(),
            RefreshOutcome::Deferred(r) => format!("deferred:{}", r.label()),
            RefreshOutcome::Resumable => "resumable".to_string(),
        }
    }

    pub fn scored(&self) -> usize {
        match self {
            RefreshOutcome::Completed { scored, .. }
            | RefreshOutcome::CompletedWithSkips { scored, .. }
            | RefreshOutcome::CompletedUnverified { scored, .. } => *scored,
            _ => 0,
        }
    }

    pub fn bookkeeping(&self) -> Bookkeeping {
        match self {
            // The only two that prove the revision: all the work that was due
            // was done, or there was none.
            RefreshOutcome::Completed { .. } | RefreshOutcome::NothingDue => Bookkeeping {
                prove_revision: true,
                retry: false,
                request_full: false,
                degraded: false,
            },
            // Fulfilled but incomplete, or interrupted: the skipped High rows
            // are still expired candidates, so the hourly retry picks them up.
            RefreshOutcome::CompletedWithSkips { .. }
            | RefreshOutcome::CompletedUnverified { .. }
            | RefreshOutcome::Resumable
            | RefreshOutcome::Deferred(DeferReason::FullScanResumable) => Bookkeeping {
                prove_revision: false,
                retry: true,
                request_full: false,
                degraded: true,
            },
            // A refresh cannot build or rebuild a fingerprint, so retrying
            // alone would loop forever: ask for the full scan that can.
            RefreshOutcome::Deferred(
                DeferReason::NoFingerprint | DeferReason::IncompatibleFingerprint,
            ) => Bookkeeping {
                prove_revision: false,
                retry: true,
                request_full: true,
                degraded: true,
            },
        }
    }

    /// What the durable queue row records. A refresh never reports `Complete`
    /// for work it deferred or left staged — the distinction has to survive a
    /// restart, which is what the row is for.
    pub fn finish_completion(&self) -> FinishCompletion {
        match self {
            RefreshOutcome::Completed { .. } | RefreshOutcome::NothingDue => {
                FinishCompletion::Complete
            }
            RefreshOutcome::CompletedWithSkips { .. } => FinishCompletion::CompleteWithSkips,
            RefreshOutcome::CompletedUnverified { .. } => FinishCompletion::CompleteUnverified,
            RefreshOutcome::Deferred(_) | RefreshOutcome::Resumable => FinishCompletion::Resumable,
        }
    }
}

/// Turn a stored candidate row into a pipeline candidate.
pub fn to_candidate(
    row: &RefreshCandidate,
    pairs: Vec<(String, String)>,
    pile_on: &HashSet<String>,
) -> CandidateInput {
    CandidateInput {
        account_did: row.did.clone(),
        account_handle: row.handle.clone(),
        is_pile_on: pile_on.contains(&row.did),
        // `None`, not `Some(vec![])`: finalize reads `direct_pairs.is_some()`
        // as "this is an amplifier", and an amplifier with nothing to say
        // skips NLI entirely. An account with no stored pairs is a follower.
        direct_pairs: if pairs.is_empty() { None } else { Some(pairs) },
        // An unparseable stored value is `None` — no distance multiplier —
        // rather than a guess.
        graph_distance: row
            .graph_distance
            .as_deref()
            .and_then(GraphDistance::from_str),
    }
}

/// Everything the pipeline needs, once the context has been loaded.
#[derive(Debug)]
pub struct RefreshPlan {
    pub fingerprint: TopicFingerprint,
    pub protected_embedding: Option<Vec<f64>>,
    pub centroids: Vec<Vec<f64>>,
    pub protected_posts: Vec<(String, Vec<f64>)>,
    pub median_engagement: f64,
    pub candidates: Vec<CandidateInput>,
}

/// The fallible context loads, behind a trait so the failure path is testable
/// without models (R05, the V2 mandatory test).
///
/// Every method returns `Result` on purpose: the production implementations
/// wrap network and database calls whose failure must fail the run, and a
/// helper that returned an empty `Vec` for "the AppView is down" is exactly
/// the shape R05 exists to forbid.
#[async_trait]
pub trait RefreshContextSource: Send + Sync {
    /// `Ok(None)` = this user has no fingerprint (a real answer: defer).
    async fn fingerprint(&self, user_did: &str) -> anyhow::Result<Option<StoredFingerprint>>;
    async fn candidates(&self, user_did: &str) -> anyhow::Result<Vec<RefreshCandidate>>;
    async fn protected_posts_embeddings(
        &self,
        actor_handle: &str,
    ) -> anyhow::Result<Vec<(String, Vec<f64>)>>;
    async fn pile_on(&self, user_did: &str) -> anyhow::Result<HashSet<String>>;
    async fn direct_pairs(
        &self,
        user_did: &str,
        amplifier_did: &str,
    ) -> anyhow::Result<Vec<(String, String)>>;
    async fn median_engagement(&self, user_did: &str) -> anyhow::Result<f64>;
}

/// Everything before the pipeline, through the injectable boundary.
///
/// `Err` = required context could not be loaded (R05) — the caller fails the
/// run and writes nothing. `Ok(Err(reason))` = defer, which is not a failure.
pub async fn prepare_refresh(
    ctx: &dyn RefreshContextSource,
    user_did: &str,
    actor_handle: &str,
) -> anyhow::Result<Result<RefreshPlan, DeferReason>> {
    // 1. Fingerprint: read, never rebuilt. Missing or incompatible ⇒ defer and
    //    ask for a full scan (R03, V2-04 — the full scan's rebuild_decision
    //    enforces compatibility; this is only the request).
    let Some((fingerprint, protected_embedding, centroids, model_id)) =
        ctx.fingerprint(user_did).await?
    else {
        return Ok(Err(DeferReason::NoFingerprint));
    };
    // A keyword-only fingerprint has no vectors to be incompatible with.
    if protected_embedding.is_some() && model_id.as_deref() != Some(EMBEDDING_MODEL_ID) {
        warn!(
            user_did,
            ?model_id,
            "fingerprint embeddings are from another model — deferring to a full scan"
        );
        return Ok(Err(DeferReason::IncompatibleFingerprint));
    }
    // 2. Candidates from the table.
    let rows = ctx.candidates(user_did).await?;
    // 3. Required context (R05): any failure here is an Err. Missing context
    //    would systematically LOWER every High/Elevated score we are about to
    //    overwrite — and stamp the result current.
    let protected_posts = ctx.protected_posts_embeddings(actor_handle).await?;
    let pile_on = ctx.pile_on(user_did).await?;
    let median_engagement = ctx.median_engagement(user_did).await?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in &rows {
        let pairs = ctx.direct_pairs(user_did, &row.did).await?;
        candidates.push(to_candidate(row, pairs, &pile_on));
    }
    Ok(Ok(RefreshPlan {
        fingerprint,
        protected_embedding,
        centroids,
        protected_posts,
        median_engagement,
        candidates,
    }))
}

/// Everything a refresh writes about ITSELF, behind a trait so the failure
/// paths are testable without a database that fails on cue (V3-05).
#[async_trait]
pub trait RefreshBookkeeping: Send + Sync {
    /// Does this attempt still own the user's queue row? (#344 F2)
    ///
    /// The refresh schedule lives on `users` and cannot be fenced by a `WHERE`
    /// clause on `scan_queue`, so the wrapper asks once and skips ALL of its
    /// writes when the answer is no — exactly as `run_scan_with` does.
    async fn owns_claim(&self, user_did: &str, claim_id: &str) -> bool;
    /// Zero the eight `refresh_*` keys and record `refresh_last_run_id` (R08).
    async fn reset_markers(&self, user_did: &str, claim_id: &str) -> anyhow::Result<()>;
    async fn record_outcome(
        &self,
        user_did: &str,
        label: &str,
        at: DateTime<Utc>,
    ) -> anyhow::Result<()>;
    async fn request_full(&self, user_did: &str) -> anyhow::Result<()>;
    /// Best-effort by contract: logs and counts a failure, never returns it.
    /// `now` is the attempt's END instant, from the injected clock (V6-02).
    async fn schedule_success(&self, user_did: &str, now: DateTime<Utc>);
    async fn schedule_retry(&self, user_did: &str, now: DateTime<Utc>);
}

/// The production bookkeeping.
pub struct DbBookkeeping {
    db: Arc<dyn Database>,
    /// The cadence a success writes. Injected at construction so nothing here
    /// reads a process-global variable from inside the code under test (F3).
    refresh_interval: std::time::Duration,
}

impl DbBookkeeping {
    pub fn new(db: Arc<dyn Database>, refresh_interval: std::time::Duration) -> Self {
        Self {
            db,
            refresh_interval,
        }
    }
}

/// The eight per-run `refresh_*` keys, reset at the START of every run so a
/// previous run's numbers can never be read as this one's (R08).
const REFRESH_RUN_KEYS_ZEROED: [&str; 5] = [
    "refresh_candidates",
    "refresh_scored",
    "refresh_feed_cache_hits",
    "refresh_feed_cache_misses",
    // `1` only once we know candidates > 0; until then the feed cache has
    // measured nothing, and "0 hits of 0" must not read as a 0% hit rate.
    "refresh_feed_cache_applicable",
];

#[async_trait]
impl RefreshBookkeeping for DbBookkeeping {
    async fn owns_claim(&self, user_did: &str, claim_id: &str) -> bool {
        match self.db.scan_claim_is_current(user_did, claim_id).await {
            Ok(current) => current,
            Err(e) => {
                // Fail closed, for the same reason the full scan does: an
                // unreadable queue row is not evidence of ownership, and the
                // cost of guessing wrong is clobbering a successor's schedule.
                warn!(
                    user_did,
                    error = %format!("{e:#}"),
                    "could not confirm the refresh claim still owns this row — skipping the \
                     completion bookkeeping"
                );
                false
            }
        }
    }

    async fn reset_markers(&self, user_did: &str, claim_id: &str) -> anyhow::Result<()> {
        self.db
            .set_scan_state(user_did, "refresh_last_run_id", claim_id)
            .await?;
        self.db
            .set_scan_state(user_did, "refresh_last_outcome", "running")
            .await?;
        self.db
            .set_scan_state(user_did, "refresh_last_run_at", "")
            .await?;
        for key in REFRESH_RUN_KEYS_ZEROED {
            self.db.set_scan_state(user_did, key, "0").await?;
        }
        Ok(())
    }

    async fn record_outcome(
        &self,
        user_did: &str,
        label: &str,
        at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        self.db
            .set_scan_state(user_did, "refresh_last_outcome", label)
            .await?;
        self.db
            .set_scan_state(user_did, "refresh_last_run_at", &at.to_rfc3339())
            .await
    }

    async fn request_full(&self, user_did: &str) -> anyhow::Result<()> {
        self.db.request_full_after_refresh(user_did).await
    }

    async fn schedule_success(&self, user_did: &str, now: DateTime<Utc>) {
        crate::web::refresh::schedule_after_success(
            self.db.as_ref(),
            user_did,
            now,
            self.refresh_interval,
        )
        .await
    }

    async fn schedule_retry(&self, user_did: &str, now: DateTime<Utc>) {
        crate::web::refresh::schedule_retry(self.db.as_ref(), user_did, now).await
    }
}

/// The run. EVERY fallible step — marker reset, scorer/client construction,
/// context loads, the pipeline — happens inside ONE captured outcome, so a
/// failure anywhere reaches exactly one scheduling site (V3-05). A refresh
/// that died while building its scorer used to return before any scheduling
/// code ran, and the user's expiring scores were never retried.
///
/// `setup` builds both the context source and the pipeline, so a construction
/// failure is a failed refresh with a retry rather than a bare slot failure.
///
/// `clock` is read once AFTER the attempt (V6-02): a ninety-minute failed
/// attempt still gets a full hour of backoff. `&(dyn Fn() -> DateTime<Utc> +
/// Sync)` and not a bare `&dyn Fn` because the future holds this borrow across
/// `.await` and the caller hands it to `tokio::spawn` (V7-01).
#[allow(clippy::too_many_arguments)] // one call site; every argument is a seam
pub(crate) async fn run_refresh_with<S, F, Fut>(
    scan_manager: Arc<RwLock<ScanManager>>,
    books: &dyn RefreshBookkeeping,
    clock: &(dyn Fn() -> DateTime<Utc> + Sync),
    user_did: &str,
    actor_handle: &str,
    claim_id: &str,
    setup: S,
) -> anyhow::Result<ScanReport>
where
    S: FnOnce() -> anyhow::Result<(Box<dyn RefreshContextSource>, F)>,
    F: FnOnce(RefreshPlan) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<ScanSummary>>,
{
    let started = clock(); // telemetry only (V6-02)
    let outcome: anyhow::Result<RefreshOutcome> = async {
        books
            .reset_markers(user_did, claim_id)
            .await
            .context("resetting refresh markers")?;
        let (ctx, pipeline) = setup().context("refresh setup")?;
        let plan = match prepare_refresh(ctx.as_ref(), user_did, actor_handle).await? {
            Ok(plan) => plan,
            Err(reason) => return Ok(RefreshOutcome::Deferred(reason)),
        };
        let candidates = plan.candidates.len();
        // Ownership is enforced inside run_phased_scan (R02). `NothingDue` is
        // decided AFTER it on purpose: a refresh with no candidates still has
        // to resume its own leftover staging.
        let summary = match pipeline(plan).await {
            Ok(s) => s,
            Err(e)
                if matches!(
                    e.downcast_ref::<PhasedScanError>(),
                    Some(PhasedScanError::OwnedByOtherKind(ScanKind::Full))
                ) =>
            {
                return Ok(RefreshOutcome::Deferred(DeferReason::FullScanResumable));
            }
            Err(e) => return Err(e),
        };
        Ok(classify_refresh(candidates, &summary))
    }
    .await;

    // Read the clock AFTER the attempt: the retry deadline and the recorded
    // run time both describe when this attempt ENDED (V6-02).
    let now = clock();
    debug!(
        user_did,
        attempt_secs = (now - started).num_seconds(),
        "refresh attempt finished"
    );
    let (result, label, books_for) = match &outcome {
        Ok(o) => {
            let b = o.bookkeeping();
            // A refresh discovers nothing, so the "events" half of the status
            // line is always zero — `finish_scan`'s label is what keeps that
            // from reading as a failed scan.
            (Ok((0, o.scored(), b.degraded)), o.label(), Some(b))
        }
        Err(e) => (Err(anyhow::anyhow!("{e:#}")), "failed".to_string(), None),
    };

    // One ownership check for every write below (#344 F2). A worker whose
    // lease lapsed would otherwise move `next_refresh_at` and stamp an outcome
    // for a user its successor now owns — and, being the slow one, its write
    // lands last. `finish_scan` is still called: its writes are fenced on the
    // claim of their own accord, and the caller must learn the outcome anyway.
    if books.owns_claim(user_did, claim_id).await {
        // Record before scheduling so an operator reading `scan_state` sees the
        // outcome that produced the schedule. If the database is down for THIS
        // write it is down for the schedule too: log + count, never pretend.
        if let Err(e) = books.record_outcome(user_did, &label, now).await {
            error!(user_did, error = %format!("{e:#}"), "could not record the refresh outcome");
            crate::observability::refresh_metrics::record_bookkeeping_failure();
        }
        // Exactly one scheduling site. The slot lifecycle (`run_under_slot`)
        // does NOT schedule; it only finishes the queue row.
        match books_for {
            Some(b) if b.prove_revision => books.schedule_success(user_did, now).await,
            Some(b) => {
                if b.request_full {
                    if let Err(e) = books.request_full(user_did).await {
                        warn!(error = %format!("{e:#}"), "could not request the follow-up full scan");
                    }
                }
                books.schedule_retry(user_did, now).await;
            }
            None => books.schedule_retry(user_did, now).await,
        }
    } else {
        warn!(
            user_did,
            "the refresh's claim no longer owns the queue row — no outcome and no schedule \
             written; the successor owns this user's bookkeeping"
        );
    }

    let completion = match &outcome {
        Ok(o) => o.finish_completion(),
        Err(_) => FinishCompletion::Failed,
    };
    finish_scan(
        &scan_manager,
        user_did,
        claim_id,
        result,
        completion,
        "Refresh complete",
    )
    .await
}

/// Pure: the candidate count plus the pipeline's summary → the outcome.
///
/// Delegates to the full scan's classifier so the two kinds cannot drift on
/// what "done" means: the `scan_phase` marker is authoritative about whether
/// staging drained, and the PERSISTED skip count — which survives a resume —
/// is authoritative about whether the coverage has holes (V5-01).
pub fn classify_refresh(candidates: usize, summary: &ScanSummary) -> RefreshOutcome {
    use crate::web::scan_job::ScanCompletion;
    match crate::web::scan_job::classify_full_scan(
        summary.degraded,
        summary.final_phase.as_deref(),
        summary.skipped,
    ) {
        ScanCompletion::Resumable => RefreshOutcome::Resumable,
        ScanCompletion::CompleteWithSkips { n } => RefreshOutcome::CompletedWithSkips {
            candidates,
            scored: summary.accounts_scored,
            skipped: n as usize,
        },
        ScanCompletion::CompleteUnverified => RefreshOutcome::CompletedUnverified {
            candidates,
            scored: summary.accounts_scored,
        },
        ScanCompletion::Complete if candidates == 0 => RefreshOutcome::NothingDue,
        ScanCompletion::Complete => RefreshOutcome::Completed {
            candidates,
            scored: summary.accounts_scored,
        },
    }
}

/// Pure: should this refresh pay the classifier-identity probe round trip?
///
/// It must when there is anything fresh to gather (`candidates > 0`) OR when
/// a prior attempt left this refresh's OWN staging behind at a resumable
/// phase — that staging still bursts through the classifier below even
/// though nothing new was gathered this tick. A genuinely empty tick
/// (`NothingDue`: no candidates, no owned staging) skips the probe entirely,
/// since nothing downstream will call the classifier — a `RunPod` `warm_up`
/// round trip is otherwise paid every night for a run that scores nothing
/// (#344 F6).
pub(crate) fn refresh_should_probe(candidates: usize, has_own_resumable_staging: bool) -> bool {
    candidates > 0 || has_own_resumable_staging
}

/// The refresh's candidate list, re-derived from stored state alone.
///
/// Used by the full scan's drain (`scan_job::drain_then_run`): the refresh
/// whose staging is being drained is gone, so the drain rebuilds the candidate
/// list it would have had. Finalize does not need candidates at all — only
/// `recover_account`'s bounded re-gather does, and an account no longer in the
/// list is a documented skip.
pub(crate) async fn refresh_drain_candidates(
    db: &Arc<dyn Database>,
    user_did: &str,
) -> anyhow::Result<Vec<CandidateInput>> {
    let rows = db
        .list_refresh_candidates(user_did, REFRESH_HORIZON_DAYS)
        .await?;
    let pile_on = pile_on_dids(db.as_ref(), user_did).await?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in &rows {
        let pairs =
            crate::pipeline::amplification::direct_pairs_for(db, user_did, &row.did).await?;
        candidates.push(to_candidate(row, pairs, &pile_on));
    }
    Ok(candidates)
}

/// Task 8's helpers behind the boundary.
struct LiveRefreshContext {
    db: Arc<dyn Database>,
    client: Arc<PublicAtpClient>,
    models: Arc<ScanModels>,
}

#[async_trait]
impl RefreshContextSource for LiveRefreshContext {
    async fn fingerprint(&self, user_did: &str) -> anyhow::Result<Option<StoredFingerprint>> {
        let Some((json, _, _)) = self.db.get_fingerprint(user_did).await? else {
            return Ok(None);
        };
        // An unreadable fingerprint is an ERROR here, not "absent": treating it
        // as absent would defer and request a full scan, which is the right
        // shape — but the row is corrupt, not missing, and saying so is what
        // makes it fixable. The run fails and retries in an hour.
        let fingerprint: TopicFingerprint =
            serde_json::from_str(&json).context("stored fingerprint is unreadable")?;
        let embedding = self.db.get_embedding(user_did).await?;
        let centroids = self
            .db
            .get_topic_centroids(user_did)
            .await?
            .iter()
            .map(|c| c.centroid.clone())
            .collect();
        let model_id = self.db.fingerprint_embedding_model(user_did).await?;
        Ok(Some((fingerprint, embedding, centroids, model_id)))
    }

    async fn candidates(&self, user_did: &str) -> anyhow::Result<Vec<RefreshCandidate>> {
        self.db
            .list_refresh_candidates(user_did, REFRESH_HORIZON_DAYS)
            .await
    }

    async fn protected_posts_embeddings(
        &self,
        actor_handle: &str,
    ) -> anyhow::Result<Vec<(String, Vec<f64>)>> {
        embed_protected_posts(&self.client, &self.models.embedder, actor_handle).await
    }

    async fn pile_on(&self, user_did: &str) -> anyhow::Result<HashSet<String>> {
        pile_on_dids(self.db.as_ref(), user_did).await
    }

    async fn direct_pairs(
        &self,
        user_did: &str,
        amplifier_did: &str,
    ) -> anyhow::Result<Vec<(String, String)>> {
        crate::pipeline::amplification::direct_pairs_for(&self.db, user_did, amplifier_did).await
    }

    async fn median_engagement(&self, user_did: &str) -> anyhow::Result<f64> {
        self.db.get_median_engagement(user_did).await
    }
}

/// Production entry: everything fallible is inside `setup`.
///
/// Returns the same `ScanReport` as `run_scan`, so `run_claim`'s two arms have
/// one future type and `run_under_slot` finishes the queue row with the
/// refresh's own completion (V4-04).
pub(crate) async fn run_refresh(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    models: Arc<ScanModels>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: &str,
    actor_handle: &str,
    claim_id: &str,
) -> anyhow::Result<ScanReport> {
    let books = DbBookkeeping::new(
        Arc::clone(&db),
        crate::web::refresh::refresh_deadline_interval(),
    );
    let uid = user_did.to_string();

    // #344 F3: the admitter creates the status entry and nothing writes to it
    // until `finish_scan`, so without these two calls a user watching the
    // dashboard through an hour-long nightly refresh sees "Starting…" the whole
    // time — indistinguishable from a stuck scan. Reported here rather than
    // inside `run_refresh_with` because the progress vocabulary is the
    // production runner's, not the seam's: the tests drive that seam with
    // fake pipelines that have no phases to report.
    set_progress(
        &scan_manager,
        user_did,
        claim_id,
        WebScanPhase::Fingerprint,
        "Loading topic fingerprint for the nightly refresh…",
    )
    .await;

    let progress_manager = Arc::clone(&scan_manager);
    let progress_claim = claim_id.to_string();
    let setup = move || -> anyhow::Result<(Box<dyn RefreshContextSource>, _)> {
        let scorers = build_scan_scorers(&models, &db)?;
        let client = Arc::new(PublicAtpClient::new(&config.public_api_url)?);
        let ctx: Box<dyn RefreshContextSource> = Box::new(LiveRefreshContext {
            db: Arc::clone(&db),
            client: Arc::clone(&client),
            models: Arc::clone(&models),
        });
        let pipeline = move |plan: RefreshPlan| async move {
            let candidates = plan.candidates.len();
            // #344 F5/F6: probe the Stage-2 classifier identity before this
            // refresh gathers anything — but only when something will
            // actually call it. A `NothingDue` tick (no fresh candidates AND
            // nothing of this refresh's own left staged) makes zero
            // classifier calls, so paying a RunPod `warm_up` round trip for
            // it every night is pure waste; a zero-candidate tick that still
            // owns resumable staging (a prior attempt cut off mid-burst)
            // bursts below regardless, so it still needs the probe.
            // `has_own_resumable_staging` is the same ownership read
            // `run_phased_scan` performs on entry, read here without its
            // mutating fallbacks.
            let has_staging = has_own_resumable_staging(&db, &uid, RunIdentity::refresh()).await?;
            if refresh_should_probe(candidates, has_staging) {
                // A mismatched Stage-2 policy fails the run (retry in an
                // hour, no score written) instead of re-gathering every due
                // account and then skipping it.
                crate::web::scan_setup::probe_classifier_identity(&scorers).await?;
            }
            db.set_scan_state(&uid, "refresh_candidates", &candidates.to_string())
                .await?;
            // Minor 5: `candidates_total` is the denominator GET /api/status
            // renders. `amplification::run` writes it for a full scan and
            // `run_candidates` does not, so without this the dashboard showed
            // the PREVIOUS full scan's denominator against this refresh's
            // progress — two runs' numbers in one fraction.
            db.set_scan_state(&uid, "candidates_total", &candidates.to_string())
                .await?;
            // The feed cache measured nothing when nothing was due, and a
            // "0 % hit rate" from an empty run is worse than no number (R08).
            db.set_scan_state(
                &uid,
                "refresh_feed_cache_applicable",
                if candidates == 0 { "0" } else { "1" },
            )
            .await?;
            // From here the pipeline reports through the `scan_phase` marker
            // and the classification counts, exactly as the full scan does —
            // GET /api/status refines this message from them.
            set_progress(
                &progress_manager,
                &uid,
                &progress_claim,
                WebScanPhase::Scoring,
                &format!("Re-scoring {candidates} accounts whose scores are expiring…"),
            )
            .await;
            let weights = crate::scoring::threat::ThreatWeights::default();
            let summary = crate::pipeline::amplification::run_candidates(
                &client,
                &scorers.scorer,
                &db,
                &uid,
                &plan.candidates,
                &plan.fingerprint,
                &weights,
                Some(&models.embedder),
                plan.protected_embedding.as_deref(),
                Some(&plan.centroids),
                Some(&models.nli),
                Some(&plan.protected_posts),
                Some(config.data_dir()),
                plan.median_engagement,
                REFRESH_GATHER_CONCURRENCY,
                RunIdentity::refresh(),
                "refresh_feed",
            )
            .await?;
            db.set_scan_state(&uid, "refresh_scored", &summary.accounts_scored.to_string())
                .await?;
            record_scan_cache_stats(db.as_ref(), &uid, &scorers).await;
            info!(
                user_did = %uid,
                candidates,
                scored = summary.accounts_scored,
                "refresh pipeline finished"
            );
            Ok(summary)
        };
        Ok((ctx, pipeline))
    };
    run_refresh_with(
        scan_manager,
        &books,
        &chrono::Utc::now,
        user_did,
        actor_handle,
        claim_id,
        setup,
    )
    .await
}
#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};

    use crate::db::schema::create_tables;
    use crate::db::sqlite::SqliteDatabase;
    use crate::db::{Database, ScanKind};
    use crate::scoring::generation::scoring_revision;
    use crate::web::scan_job::ScanManager;

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

    #[test]
    fn candidates_keep_graph_distance_pairs_and_pile_on() {
        let row = RefreshCandidate {
            did: "did:plc:a".into(),
            handle: "a.handle".into(),
            graph_distance: Some("Follows you".into()),
        };
        let pile_on: HashSet<String> = ["did:plc:a".to_string()].into_iter().collect();
        let c = to_candidate(&row, vec![("o".into(), "r".into())], &pile_on);
        assert_eq!(c.account_did, "did:plc:a");
        assert!(c.is_pile_on);
        assert_eq!(c.graph_distance, Some(GraphDistance::InboundFollow));
        assert_eq!(
            c.direct_pairs,
            Some(vec![("o".to_string(), "r".to_string())])
        );
    }

    /// No stored pairs ⇒ follower path (`direct_pairs: None`), NOT
    /// `Some(vec![])`, which finalize would treat as an amplifier with nothing
    /// to say and skip NLI entirely.
    #[test]
    fn no_pairs_means_follower_mode_and_unparseable_distance_is_none() {
        let row = RefreshCandidate {
            did: "did:plc:b".into(),
            handle: "b.handle".into(),
            graph_distance: Some("not a distance".into()),
        };
        let c = to_candidate(&row, vec![], &HashSet::new());
        assert_eq!(c.direct_pairs, None);
        assert_eq!(c.graph_distance, None);
    }

    /// #344 F6: a nothing-due tick (no candidates, nothing of its own left
    /// staged) is the ONLY case that skips the probe — every other
    /// combination pays it, including the zero-candidate-but-own-staging
    /// case that still bursts below.
    #[test]
    fn probe_is_skipped_only_when_nothing_is_due() {
        assert!(
            !refresh_should_probe(0, false),
            "nothing due: no candidates, no owned staging — no classifier call is coming"
        );
        assert!(
            refresh_should_probe(1, false),
            "candidates present: the burst below will call the classifier"
        );
        assert!(
            refresh_should_probe(0, true),
            "own leftover staging still bursts even with zero fresh candidates"
        );
        assert!(refresh_should_probe(3, true));
    }

    #[test]
    fn outcome_labels_are_stable_scan_state_values() {
        assert_eq!(RefreshOutcome::NothingDue.label(), "nothing_due");
        assert_eq!(
            RefreshOutcome::Deferred(DeferReason::FullScanResumable).label(),
            "deferred:full_scan_resumable"
        );
        assert_eq!(
            RefreshOutcome::Deferred(DeferReason::NoFingerprint).label(),
            "deferred:no_fingerprint"
        );
        assert_eq!(
            RefreshOutcome::Deferred(DeferReason::IncompatibleFingerprint).label(),
            "deferred:incompatible_fingerprint"
        );
        assert_eq!(RefreshOutcome::Resumable.label(), "resumable");
        assert_eq!(
            RefreshOutcome::Completed {
                candidates: 3,
                scored: 3
            }
            .label(),
            "completed"
        );
        assert_eq!(
            RefreshOutcome::CompletedWithSkips {
                candidates: 3,
                scored: 2,
                skipped: 1
            }
            .label(),
            "completed_with_skips"
        );
        assert_eq!(
            RefreshOutcome::CompletedUnverified {
                candidates: 3,
                scored: 3
            }
            .label(),
            "completed_unverified"
        );
    }

    /// V2-05: only complete work proves the revision; everything else retries
    /// and shows as degraded; missing/incompatible fingerprints additionally
    /// request a full scan, because a refresh never rebuilds one.
    #[test]
    fn bookkeeping_follows_the_completion_contract() {
        let b = |o: RefreshOutcome| o.bookkeeping();
        let ok = b(RefreshOutcome::Completed {
            candidates: 1,
            scored: 1,
        });
        assert!(ok.prove_revision && !ok.retry && !ok.request_full && !ok.degraded);
        let none = b(RefreshOutcome::NothingDue);
        assert!(none.prove_revision && !none.retry && !none.degraded);
        let skips = b(RefreshOutcome::CompletedWithSkips {
            candidates: 3,
            scored: 2,
            skipped: 1,
        });
        assert!(!skips.prove_revision && skips.retry && !skips.request_full && skips.degraded);
        let unver = b(RefreshOutcome::CompletedUnverified {
            candidates: 3,
            scored: 3,
        });
        assert!(!unver.prove_revision && unver.retry && !unver.request_full && unver.degraded);
        let res = b(RefreshOutcome::Resumable);
        assert!(!res.prove_revision && res.retry && res.degraded);
        let full = b(RefreshOutcome::Deferred(DeferReason::FullScanResumable));
        assert!(!full.prove_revision && full.retry && !full.request_full);
        for r in [
            DeferReason::NoFingerprint,
            DeferReason::IncompatibleFingerprint,
        ] {
            let d = b(RefreshOutcome::Deferred(r));
            assert!(d.retry && d.request_full && !d.prove_revision);
        }
    }

    /// A refresh NEVER writes the full scan's bookkeeping, whatever it ends as.
    #[test]
    fn refresh_completions_map_to_the_queue_row() {
        use crate::db::FinishCompletion as F;
        assert_eq!(
            RefreshOutcome::Completed {
                candidates: 1,
                scored: 1
            }
            .finish_completion(),
            F::Complete
        );
        assert_eq!(RefreshOutcome::NothingDue.finish_completion(), F::Complete);
        assert_eq!(
            RefreshOutcome::CompletedWithSkips {
                candidates: 1,
                scored: 0,
                skipped: 1
            }
            .finish_completion(),
            F::CompleteWithSkips
        );
        assert_eq!(
            RefreshOutcome::CompletedUnverified {
                candidates: 1,
                scored: 1
            }
            .finish_completion(),
            F::CompleteUnverified
        );
        assert_eq!(RefreshOutcome::Resumable.finish_completion(), F::Resumable);
        assert_eq!(
            RefreshOutcome::Deferred(DeferReason::NoFingerprint).finish_completion(),
            F::Resumable
        );
    }

    /// Counting bookkeeping stub: records what the run asked for, can fail
    /// `reset_markers` on cue, and otherwise delegates to the database so the
    /// assertions below read real rows.
    struct CountingBooks {
        inner: DbBookkeeping,
        fail_reset: bool,
        retries: AtomicUsize,
        successes: AtomicUsize,
    }

    impl CountingBooks {
        fn new(db: Arc<dyn Database>, fail_reset: bool) -> Self {
            Self {
                inner: DbBookkeeping::new(db, std::time::Duration::from_secs(24 * 3600)),
                fail_reset,
                retries: 0.into(),
                successes: 0.into(),
            }
        }
    }

    #[async_trait]
    impl RefreshBookkeeping for CountingBooks {
        async fn owns_claim(&self, u: &str, c: &str) -> bool {
            self.inner.owns_claim(u, c).await
        }
        async fn reset_markers(&self, u: &str, c: &str) -> anyhow::Result<()> {
            if self.fail_reset {
                anyhow::bail!("scan_state write failed")
            } else {
                self.inner.reset_markers(u, c).await
            }
        }
        async fn record_outcome(&self, u: &str, l: &str, at: DateTime<Utc>) -> anyhow::Result<()> {
            self.inner.record_outcome(u, l, at).await
        }
        async fn request_full(&self, u: &str) -> anyhow::Result<()> {
            self.inner.request_full(u).await
        }
        async fn schedule_success(&self, u: &str, now: DateTime<Utc>) {
            self.successes.fetch_add(1, SeqCst);
            self.inner.schedule_success(u, now).await
        }
        async fn schedule_retry(&self, u: &str, now: DateTime<Utc>) {
            self.retries.fetch_add(1, SeqCst);
            self.inner.schedule_retry(u, now).await
        }
    }

    /// A High row, a claimed refresh and a status entry — the state every
    /// failure-path test starts from.
    async fn refresh_in_flight(
        db: &Arc<dyn Database>,
    ) -> (
        String,
        Arc<RwLock<ScanManager>>,
        Vec<crate::db::models::StoredScore>,
    ) {
        db.upsert_user("did:plc:u", "u.h").await.unwrap();
        let mut high = crate::db::models::AccountScore::default_for_test("did:plc:high");
        high.threat_score = Some(60.0);
        high.threat_tier = Some("High".into());
        high.scoring_confidence = Some("high".into());
        db.upsert_account_score("did:plc:u", &high).await.unwrap();
        let before = db.export_scores("did:plc:u").await.unwrap();
        db.enqueue_refresh_scan("did:plc:u").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        let mgr = manager_with_running_scan("did:plc:u", &claim.claim_id);
        (claim.claim_id.clone(), mgr, before)
    }

    /// Shared assertions for every failure path: exactly one scheduling call
    /// and it is the retry; nothing written; outcome recorded; one-hour
    /// deadline; revision not proven; queue row completion = failed.
    async fn assert_failed_with_one_retry(
        db: &Arc<dyn Database>,
        books: &CountingBooks,
        before: &[crate::db::models::StoredScore],
        claim_id: &str,
    ) {
        let retries = books.retries.load(SeqCst);
        let successes = books.successes.load(SeqCst);
        assert_eq!(
            (retries, successes),
            (1, 0),
            "exactly one scheduling call, and it is the retry"
        );
        assert_eq!(
            db.export_scores("did:plc:u").await.unwrap(),
            before,
            "no score written or restamped"
        );
        assert_eq!(
            db.get_scan_state("did:plc:u", "refresh_last_outcome")
                .await
                .unwrap()
                .as_deref(),
            Some("failed")
        );
        let next = db
            .next_refresh_at("did:plc:u")
            .await
            .unwrap()
            .expect("retry scheduled");
        let next = chrono::DateTime::parse_from_rfc3339(&next).unwrap();
        let delta = next.signed_duration_since(chrono::Utc::now());
        assert!(
            delta > chrono::Duration::minutes(55) && delta <= chrono::Duration::hours(1),
            "one-hour retry, got {delta}"
        );
        assert_ne!(
            db.refreshed_generation("did:plc:u")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision()),
            "not proven"
        );
        // A refresh never writes the full scan's bookkeeping (N4).
        assert_eq!(
            db.get_scan_state("did:plc:u", "last_full_scan_finished_at")
                .await
                .unwrap(),
            None
        );
        db.finish_queued_scan(
            "did:plc:u",
            claim_id,
            crate::db::FinishCompletion::Failed,
            None,
        )
        .await
        .unwrap();
        let row = db
            .list_scan_queue()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.user_did == "did:plc:u")
            .unwrap();
        assert_eq!(row.completion, Some(crate::db::FinishCompletion::Failed));
    }

    struct Failing;

    #[async_trait]
    impl RefreshContextSource for Failing {
        async fn fingerprint(&self, _: &str) -> anyhow::Result<Option<StoredFingerprint>> {
            Ok(Some((
                TopicFingerprint {
                    clusters: vec![],
                    post_count: 0,
                },
                None,
                vec![],
                None,
            )))
        }
        async fn candidates(&self, _: &str) -> anyhow::Result<Vec<RefreshCandidate>> {
            Ok(vec![RefreshCandidate {
                did: "did:plc:high".into(),
                handle: "high.h".into(),
                graph_distance: None,
            }])
        }
        async fn protected_posts_embeddings(
            &self,
            _: &str,
        ) -> anyhow::Result<Vec<(String, Vec<f64>)>> {
            anyhow::bail!("getAuthorFeed: 502 from the AppView")
        }
        async fn pile_on(&self, _: &str) -> anyhow::Result<HashSet<String>> {
            Ok(HashSet::new())
        }
        async fn direct_pairs(&self, _: &str, _: &str) -> anyhow::Result<Vec<(String, String)>> {
            Ok(vec![])
        }
        async fn median_engagement(&self, _: &str) -> anyhow::Result<f64> {
            Ok(0.0)
        }
    }

    /// R05, mandatory (V2): a context load failure fails the run — no score is
    /// written, the pipeline is never entered, the outcome is recorded as
    /// failed, and the retry is scheduled. Runs without models.
    #[tokio::test]
    async fn missing_context_fails_the_refresh_without_writing() {
        let db = test_db();
        let (claim_id, mgr, before) = refresh_in_flight(&db).await;
        let books = CountingBooks::new(db.clone(), false);
        let result = run_refresh_with(
            mgr,
            &books,
            &chrono::Utc::now,
            "did:plc:u",
            "u.h",
            &claim_id,
            || {
                let ctx: Box<dyn RefreshContextSource> = Box::new(Failing);
                Ok((ctx, |_plan: RefreshPlan| async {
                    panic!("the pipeline must not run when context is missing")
                }))
            },
        )
        .await;
        assert!(result.is_err());
        assert_failed_with_one_retry(&db, &books, &before, &claim_id).await;
    }

    /// V3-05 (1): a scorer/client setup failure BEFORE context preparation gets
    /// the same lifecycle — failed, one retry, nothing written.
    #[tokio::test]
    async fn setup_failure_records_failed_and_schedules_the_retry() {
        let db = test_db();
        let (claim_id, mgr, before) = refresh_in_flight(&db).await;
        let books = CountingBooks::new(db.clone(), false);
        let result = run_refresh_with::<
            _,
            fn(RefreshPlan) -> std::future::Ready<anyhow::Result<ScanSummary>>,
            _,
        >(
            mgr,
            &books,
            &chrono::Utc::now,
            "did:plc:u",
            "u.h",
            &claim_id,
            || anyhow::bail!("CHARCOAL_CLASSIFIER is unset — build_from_env failed"),
        )
        .await;
        assert!(result.is_err());
        assert_failed_with_one_retry(&db, &books, &before, &claim_id).await;
    }

    /// V3-05 (2): a marker-initialisation failure with the database otherwise
    /// available gets the same lifecycle, and the setup never runs.
    #[tokio::test]
    async fn marker_reset_failure_records_failed_and_schedules_the_retry() {
        let db = test_db();
        let (claim_id, mgr, before) = refresh_in_flight(&db).await;
        let books = CountingBooks::new(db.clone(), true);
        let result = run_refresh_with::<
            _,
            fn(RefreshPlan) -> std::future::Ready<anyhow::Result<ScanSummary>>,
            _,
        >(
            mgr,
            &books,
            &chrono::Utc::now,
            "did:plc:u",
            "u.h",
            &claim_id,
            || panic!("setup must not run when the markers could not be reset"),
        )
        .await;
        assert!(result.is_err());
        assert_failed_with_one_retry(&db, &books, &before, &claim_id).await;
    }

    /// V6-02, refresh side: a 90-minute failed attempt is retried an hour after
    /// it ENDED, not after it began.
    #[tokio::test]
    async fn a_failed_refresh_is_retried_an_hour_after_it_ended_not_began() {
        use crate::web::refresh::REFRESH_RETRY_HOURS;
        let db = test_db();
        let (claim_id, mgr, before) = refresh_in_flight(&db).await;
        let books = CountingBooks::new(db.clone(), false);
        let t0 = chrono::Utc::now();
        let clock = std::sync::Arc::new(std::sync::Mutex::new(t0));
        let advance = clock.clone();
        let now_fn = {
            let c = clock.clone();
            move || *c.lock().unwrap()
        };
        let result = run_refresh_with::<
            _,
            fn(RefreshPlan) -> std::future::Ready<anyhow::Result<ScanSummary>>,
            _,
        >(
            mgr,
            &books,
            &now_fn,
            "did:plc:u",
            "u.h",
            &claim_id,
            move || {
                *advance.lock().unwrap() += chrono::Duration::minutes(90);
                anyhow::bail!("AppView unreachable for the whole attempt")
            },
        )
        .await;
        assert!(result.is_err());
        let end = t0 + chrono::Duration::minutes(90);
        let deadline = chrono::DateTime::parse_from_rfc3339(
            &db.next_refresh_at("did:plc:u").await.unwrap().unwrap(),
        )
        .unwrap();
        assert_eq!(
            deadline,
            end + chrono::Duration::hours(REFRESH_RETRY_HOURS as i64)
        );
        assert_eq!(db.export_scores("did:plc:u").await.unwrap(), before);
        db.finish_queued_scan(
            "did:plc:u",
            &claim_id,
            crate::db::FinishCompletion::Failed,
            Some("down"),
        )
        .await
        .unwrap();
        assert_eq!(
            crate::web::refresh::enqueue_due_refreshes(
                &db,
                end + chrono::Duration::minutes(30),
                std::time::Duration::from_secs(24 * 3600)
            )
            .await,
            0
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
        let row = db
            .list_scan_queue()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.user_did == "did:plc:u")
            .unwrap();
        assert_eq!(
            row.kind,
            ScanKind::Refresh,
            "no full obligation ⇒ retried as a refresh"
        );
    }

    struct Empty;

    #[async_trait]
    impl RefreshContextSource for Empty {
        async fn fingerprint(&self, _: &str) -> anyhow::Result<Option<StoredFingerprint>> {
            Ok(Some((
                TopicFingerprint {
                    clusters: vec![],
                    post_count: 0,
                },
                None,
                vec![],
                None,
            )))
        }
        async fn candidates(&self, _: &str) -> anyhow::Result<Vec<RefreshCandidate>> {
            Ok(vec![RefreshCandidate {
                did: "did:plc:a".into(),
                handle: "a.h".into(),
                graph_distance: None,
            }])
        }
        async fn protected_posts_embeddings(
            &self,
            _: &str,
        ) -> anyhow::Result<Vec<(String, Vec<f64>)>> {
            Ok(vec![])
        }
        async fn pile_on(&self, _: &str) -> anyhow::Result<HashSet<String>> {
            Ok(HashSet::new())
        }
        async fn direct_pairs(&self, _: &str, _: &str) -> anyhow::Result<Vec<(String, String)>> {
            Ok(vec![])
        }
        async fn median_engagement(&self, _: &str) -> anyhow::Result<f64> {
            Ok(0.0)
        }
    }

    /// A successful lookup that legitimately finds no pairs is NOT a failure
    /// (the other half of R05).
    #[tokio::test]
    async fn no_pairs_is_a_valid_plan() {
        let plan = prepare_refresh(&Empty, "did:plc:u", "u.h")
            .await
            .unwrap()
            .expect("not deferred");
        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(
            plan.candidates[0].direct_pairs, None,
            "follower path, not an error"
        );
    }

    /// A refresh never rebuilds a fingerprint: absent or built by another
    /// embedding model, it defers AND asks for a full scan (R03, V2-04).
    #[tokio::test]
    async fn an_absent_or_foreign_fingerprint_defers() {
        struct NoFingerprint;
        #[async_trait]
        impl RefreshContextSource for NoFingerprint {
            async fn fingerprint(&self, _: &str) -> anyhow::Result<Option<StoredFingerprint>> {
                Ok(None)
            }
            async fn candidates(&self, _: &str) -> anyhow::Result<Vec<RefreshCandidate>> {
                panic!("candidates must not be read once the fingerprint is missing")
            }
            async fn protected_posts_embeddings(
                &self,
                _: &str,
            ) -> anyhow::Result<Vec<(String, Vec<f64>)>> {
                panic!("no context is loaded for a deferred refresh")
            }
            async fn pile_on(&self, _: &str) -> anyhow::Result<HashSet<String>> {
                panic!("no context is loaded for a deferred refresh")
            }
            async fn direct_pairs(
                &self,
                _: &str,
                _: &str,
            ) -> anyhow::Result<Vec<(String, String)>> {
                panic!("no context is loaded for a deferred refresh")
            }
            async fn median_engagement(&self, _: &str) -> anyhow::Result<f64> {
                panic!("no context is loaded for a deferred refresh")
            }
        }
        assert_eq!(
            prepare_refresh(&NoFingerprint, "did:plc:u", "u.h")
                .await
                .unwrap()
                .unwrap_err(),
            DeferReason::NoFingerprint
        );

        struct ForeignEmbeddings;
        #[async_trait]
        impl RefreshContextSource for ForeignEmbeddings {
            async fn fingerprint(&self, _: &str) -> anyhow::Result<Option<StoredFingerprint>> {
                Ok(Some((
                    TopicFingerprint {
                        clusters: vec![],
                        post_count: 0,
                    },
                    Some(vec![0.1, 0.2]),
                    vec![],
                    Some("some-other-embedding-model".into()),
                )))
            }
            async fn candidates(&self, _: &str) -> anyhow::Result<Vec<RefreshCandidate>> {
                panic!("candidates must not be read once the fingerprint is incompatible")
            }
            async fn protected_posts_embeddings(
                &self,
                _: &str,
            ) -> anyhow::Result<Vec<(String, Vec<f64>)>> {
                panic!("no context is loaded for a deferred refresh")
            }
            async fn pile_on(&self, _: &str) -> anyhow::Result<HashSet<String>> {
                panic!("no context is loaded for a deferred refresh")
            }
            async fn direct_pairs(
                &self,
                _: &str,
                _: &str,
            ) -> anyhow::Result<Vec<(String, String)>> {
                panic!("no context is loaded for a deferred refresh")
            }
            async fn median_engagement(&self, _: &str) -> anyhow::Result<f64> {
                panic!("no context is loaded for a deferred refresh")
            }
        }
        assert_eq!(
            prepare_refresh(&ForeignEmbeddings, "did:plc:u", "u.h")
                .await
                .unwrap()
                .unwrap_err(),
            DeferReason::IncompatibleFingerprint
        );
    }

    /// V2-05 end to end: a deferred refresh records its reason, keeps the
    /// scores it did not touch, asks for the full scan it cannot do itself,
    /// and finishes the queue row `resumable` — not `complete`.
    #[tokio::test]
    async fn a_deferred_refresh_requests_the_full_scan_it_cannot_run() {
        struct NoFingerprint;
        #[async_trait]
        impl RefreshContextSource for NoFingerprint {
            async fn fingerprint(&self, _: &str) -> anyhow::Result<Option<StoredFingerprint>> {
                Ok(None)
            }
            async fn candidates(&self, _: &str) -> anyhow::Result<Vec<RefreshCandidate>> {
                unreachable!()
            }
            async fn protected_posts_embeddings(
                &self,
                _: &str,
            ) -> anyhow::Result<Vec<(String, Vec<f64>)>> {
                unreachable!()
            }
            async fn pile_on(&self, _: &str) -> anyhow::Result<HashSet<String>> {
                unreachable!()
            }
            async fn direct_pairs(
                &self,
                _: &str,
                _: &str,
            ) -> anyhow::Result<Vec<(String, String)>> {
                unreachable!()
            }
            async fn median_engagement(&self, _: &str) -> anyhow::Result<f64> {
                unreachable!()
            }
        }

        let db = test_db();
        let (claim_id, mgr, before) = refresh_in_flight(&db).await;
        let books = CountingBooks::new(db.clone(), false);
        let report = run_refresh_with(
            mgr,
            &books,
            &chrono::Utc::now,
            "did:plc:u",
            "u.h",
            &claim_id,
            || {
                let ctx: Box<dyn RefreshContextSource> = Box::new(NoFingerprint);
                Ok((ctx, |_plan: RefreshPlan| async {
                    panic!("the pipeline must not run for a deferred refresh")
                }))
            },
        )
        .await
        .expect("a deferral is not an error");

        assert_eq!(report.completion, crate::db::FinishCompletion::Resumable);
        assert_eq!(
            db.get_scan_state("did:plc:u", "refresh_last_outcome")
                .await
                .unwrap()
                .as_deref(),
            Some("deferred:no_fingerprint")
        );
        assert_eq!(
            (books.retries.load(SeqCst), books.successes.load(SeqCst)),
            (1, 0)
        );
        assert_eq!(db.export_scores("did:plc:u").await.unwrap(), before);
        assert_ne!(
            db.refreshed_generation("did:plc:u")
                .await
                .unwrap()
                .as_deref(),
            Some(scoring_revision()),
            "a deferral proves nothing"
        );
        // The follow-up full scan is owed, durably, on the refresh's own row.
        let row = db
            .list_scan_queue()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.user_did == "did:plc:u")
            .unwrap();
        assert!(
            row.full_requested_at.is_some(),
            "the refresh asked for the full scan it cannot run itself"
        );
        // …and finishing the row hands it over as a queued FULL scan.
        db.finish_queued_scan("did:plc:u", &claim_id, report.completion, None)
            .await
            .unwrap();
        let row = db
            .list_scan_queue()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.user_did == "did:plc:u")
            .unwrap();
        assert_eq!((row.status.as_str(), row.kind), ("queued", ScanKind::Full));
    }

    /// The eight `refresh_*` keys are (re)written at the START of every run, so
    /// a previous run's numbers can never be read as this one's (R08).
    #[tokio::test]
    async fn markers_are_reset_at_the_start_of_every_run() {
        let db = test_db();
        db.upsert_user("did:plc:u", "u.h").await.unwrap();
        for (k, v) in [
            ("refresh_candidates", "900"),
            ("refresh_scored", "900"),
            ("refresh_feed_cache_hits", "900"),
            ("refresh_feed_cache_misses", "900"),
            ("refresh_feed_cache_applicable", "1"),
            ("refresh_last_outcome", "completed"),
        ] {
            db.set_scan_state("did:plc:u", k, v).await.unwrap();
        }
        DbBookkeeping::new(db.clone(), std::time::Duration::from_secs(3600))
            .reset_markers("did:plc:u", "claim-42")
            .await
            .unwrap();
        for k in [
            "refresh_candidates",
            "refresh_scored",
            "refresh_feed_cache_hits",
            "refresh_feed_cache_misses",
            "refresh_feed_cache_applicable",
        ] {
            assert_eq!(
                db.get_scan_state("did:plc:u", k).await.unwrap().as_deref(),
                Some("0"),
                "{k} must not carry a previous run's number"
            );
        }
        assert_eq!(
            db.get_scan_state("did:plc:u", "refresh_last_outcome")
                .await
                .unwrap()
                .as_deref(),
            Some("running")
        );
        assert_eq!(
            db.get_scan_state("did:plc:u", "refresh_last_run_id")
                .await
                .unwrap()
                .as_deref(),
            Some("claim-42")
        );
    }

    /// A superseded worker writes no schedule and no outcome (#344 F2, the
    /// refresh twin of `run_scan_with`'s fence): its successor owns the user's
    /// bookkeeping now.
    #[tokio::test]
    async fn a_superseded_refresh_worker_writes_nothing() {
        let db = test_db();
        let (claim_id, mgr, before) = refresh_in_flight(&db).await;
        let books = CountingBooks::new(db.clone(), false);
        // The lease lapses and a successor reclaims the row.
        db.finish_queued_scan(
            "did:plc:u",
            &claim_id,
            crate::db::FinishCompletion::Failed,
            None,
        )
        .await
        .unwrap();
        db.enqueue_refresh_scan("did:plc:u").await.unwrap();
        let successor = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        assert_ne!(successor.claim_id, claim_id);

        let result = run_refresh_with::<
            _,
            fn(RefreshPlan) -> std::future::Ready<anyhow::Result<ScanSummary>>,
            _,
        >(
            mgr,
            &books,
            &chrono::Utc::now,
            "did:plc:u",
            "u.h",
            &claim_id,
            || anyhow::bail!("the AppView is down"),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(
            (books.retries.load(SeqCst), books.successes.load(SeqCst)),
            (0, 0),
            "no scheduling at all under a lost claim"
        );
        assert_eq!(db.next_refresh_at("did:plc:u").await.unwrap(), None);
        assert_eq!(db.export_scores("did:plc:u").await.unwrap(), before);
    }

    /// Completion is classified from persisted state, not from the invocation's
    /// own flag (V5-01), and a run with no candidates that drained cleanly is
    /// `NothingDue` — which still proves the revision.
    #[test]
    fn refresh_completion_is_classified_from_persisted_state() {
        let done = |degraded, skipped| ScanSummary {
            accounts_scored: 2,
            regathered: 0,
            degraded,
            final_phase: Some("done".into()),
            skipped,
        };
        assert_eq!(
            classify_refresh(3, &done(false, Some(0))),
            RefreshOutcome::Completed {
                candidates: 3,
                scored: 2
            }
        );
        assert_eq!(
            classify_refresh(0, &done(false, Some(0))),
            RefreshOutcome::NothingDue
        );
        // A clean resume over earlier persisted skips is NOT clean completion.
        assert_eq!(
            classify_refresh(3, &done(false, Some(1))),
            RefreshOutcome::CompletedWithSkips {
                candidates: 3,
                scored: 2,
                skipped: 1
            }
        );
        assert_eq!(
            classify_refresh(3, &done(false, None)),
            RefreshOutcome::CompletedUnverified {
                candidates: 3,
                scored: 2
            }
        );
        // Staging left behind is resumable whatever the flag says.
        for phase in [
            Some("burst".to_string()),
            Some("finalize".to_string()),
            None,
        ] {
            let summary = ScanSummary {
                accounts_scored: 0,
                regathered: 0,
                degraded: false,
                final_phase: phase.clone(),
                skipped: Some(0),
            };
            assert_eq!(classify_refresh(1, &summary), RefreshOutcome::Resumable);
        }
    }
}
