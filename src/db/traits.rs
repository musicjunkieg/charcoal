// Database trait — backend-agnostic async interface for all DB operations.
//
// Implementors: SqliteDatabase (wraps rusqlite), PgDatabase (wraps sqlx).
// All methods are async so both sync (rusqlite via Mutex) and native async
// (sqlx) backends fit behind a single interface.
//
// The trait mirrors the existing queries.rs function signatures, so switching
// from direct Connection usage to `Arc<dyn Database>` is a straightforward
// mechanical replacement in callers.

use anyhow::Result;
use async_trait::async_trait;

use super::models::{
    AccountScore, AccuracyMetrics, AmplificationEvent, ClusterCentroid, InferredPair,
    NewAmplificationEvent, StoredScore, UserLabel, UserRow,
};
use crate::pipeline::scan_phases::staging::{QueueRow, VerdictRow};

/// One user's candidate for the nightly refresh job (#344 Task 7) — a
/// High/Elevated account whose score is expiring, already expired, or
/// stamped with a superseded `scoring_generation`. `graph_distance` rides
/// along because the refresh runner needs it for the same reasons
/// `get_ranked_threats` does (display, prioritization) without a second
/// round-trip per candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshCandidate {
    pub did: String,
    pub handle: String,
    pub graph_distance: Option<String>,
}

/// One account dropped from a scan, with the reason (#226).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanSkip {
    pub account_did: String,
    /// Pipeline phase the skip happened in — "gather", "finalize", …
    pub phase: String,
    /// Full anyhow source chain (`{e:#}`), stored verbatim.
    pub error: String,
    pub skipped_at: String,
}

/// A user's position in the scan queue (#257).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanQueueEntry {
    pub user_did: String,
    /// "queued" | "running" | "done" | "failed"
    pub status: String,
    /// 1-based position among queued rows; 0 when running or finished.
    pub position: i64,
    /// `ceil(position / concurrency_limit) x rolling median scan duration` —
    /// the cap divides the wait, so a plain `position x median` overstates it
    /// by up to the cap factor. None while the status is anything but
    /// "queued" (a running scan's remaining time is unknown, not zero) and
    /// None until enough scans have finished to have a median.
    ///
    /// The median is sampled from the per-user `scan_state` key
    /// [`LAST_FULL_SCAN_DURATION_KEY`], written by `finish_full_scan_state`
    /// when a full scan is fulfilled — **not** from `scan_queue`. There is one
    /// queue row per user and a refresh enqueue rewrites its `kind`,
    /// `started_at` and `finished_at`, so a median read from the queue would
    /// lose every user's full-scan sample the first night the refresh job runs
    /// and stay empty forever after (#344 F1). `scan_state` rows are only ever
    /// added to, so the sample survives the refresh.
    pub eta_seconds: Option<i64>,
    pub enqueued_at: String,
}

/// Per-user `scan_state` key holding the whole-second duration of that user's
/// most recent fulfilled full scan. The population the ETA median is drawn
/// from; see [`ScanQueueEntry::eta_seconds`].
pub const LAST_FULL_SCAN_DURATION_KEY: &str = "last_full_scan_duration_secs";

/// Whole seconds between a claimed full scan's `started_at` and the instant it
/// fulfilled, or `None` when there is no trustworthy sample to record.
///
/// `None` for an absent `started_at` (nothing to measure from), for timestamps
/// that do not parse, and for a negative span (clock skew across a restart).
/// A fabricated sample would skew the ETA every queued user is quoted, whereas
/// no sample merely means this attempt taught the median nothing.
///
/// Shared by both backends so they cannot disagree about what a duration is.
pub(crate) fn full_scan_duration_secs(started_at: Option<&str>, finished_at: &str) -> Option<i64> {
    let started = chrono::DateTime::parse_from_rfc3339(started_at?).ok()?;
    let finished = chrono::DateTime::parse_from_rfc3339(finished_at).ok()?;
    let secs = (finished - started).num_seconds();
    (secs >= 0).then_some(secs)
}

/// Median of the raw `scan_state` duration values both backends hand in.
///
/// Lives here, like [`eta_seconds`], so the two cannot drift: averaging the
/// two middle values on an even sample is what Postgres's
/// `PERCENTILE_CONT(0.5)` did while the median was computed in SQL, and what
/// the SQLite side has always done in Rust.
///
/// An unparseable value is warned about and skipped rather than failing the
/// read: an ETA is an estimate shown next to a queue position, and one corrupt
/// row must not take the page down. `None` on an empty sample, so a fresh
/// install reports no ETA instead of fabricating one.
pub(crate) fn median_scan_duration_secs<I: IntoIterator<Item = String>>(values: I) -> Option<f64> {
    let mut durations: Vec<f64> = Vec::new();
    for raw in values {
        match raw.parse::<f64>() {
            Ok(v) if v.is_finite() => durations.push(v),
            _ => tracing::warn!(
                value = %raw,
                key = LAST_FULL_SCAN_DURATION_KEY,
                "scan_state holds an unparseable full-scan duration — ignoring it"
            ),
        }
    }
    if durations.is_empty() {
        return None;
    }
    // `total_cmp`, not `partial_cmp().unwrap()`: the values are finite by the
    // filter above, and a total order needs no unwrap to say so.
    durations.sort_by(f64::total_cmp);
    let mid = durations.len() / 2;
    Some(if durations.len().is_multiple_of(2) {
        (durations[mid - 1] + durations[mid]) / 2.0
    } else {
        durations[mid]
    })
}

/// Shared ETA formula for `ScanQueueEntry::eta_seconds` (#257).
///
/// Lives here rather than in either backend so the two cannot drift.
///
/// - Only a "queued" row has a meaningful ETA. A *running* scan's remaining
///   time is unknown, and `position` is forced to 0 for non-queued rows, so
///   computing anyway would report `Some(0)` — telling a user watching their
///   own running scan "0 seconds remaining".
/// - `concurrency_limit` scans drain in parallel, so the wait is
///   `ceil(position / limit)` batches, not `position` of them. Charcoal scans
///   run 22 minutes to 2 hours, so at cap 2 the uncorrected formula tells a
///   position-4 user roughly twice their real wait.
pub(crate) fn eta_seconds(
    status: &str,
    position: i64,
    concurrency_limit: usize,
    median_secs: Option<f64>,
) -> Option<i64> {
    if status != "queued" {
        return None;
    }
    let median = median_secs?;
    // A cap of 0 admits nothing, so there is no ETA to give.
    let limit = i64::try_from(concurrency_limit).unwrap_or(i64::MAX).max(0);
    if limit == 0 {
        return None;
    }
    // Manual ceiling division: i64::div_ceil is still unstable. position is a
    // COUNT(*) so it is never negative.
    let batches = (position.max(0) + limit - 1) / limit;
    Some((median * batches as f64) as i64)
}

#[cfg(test)]
mod eta_tests {
    use super::eta_seconds;

    #[test]
    fn only_queued_rows_get_an_eta() {
        // position is forced to 0 for non-queued rows, so without the status
        // gate these would all report Some(0) — "0 seconds remaining".
        for status in ["running", "done", "failed"] {
            assert_eq!(eta_seconds(status, 0, 2, Some(600.0)), None, "{status}");
        }
    }

    #[test]
    fn the_cap_divides_the_wait() {
        assert_eq!(eta_seconds("queued", 4, 1, Some(600.0)), Some(2400));
        assert_eq!(eta_seconds("queued", 4, 2, Some(600.0)), Some(1200));
        // Ceiling, not floor: position 4 at cap 3 is still two batches.
        assert_eq!(eta_seconds("queued", 4, 3, Some(600.0)), Some(1200));
        assert_eq!(eta_seconds("queued", 4, 4, Some(600.0)), Some(600));
    }

    #[test]
    fn no_median_and_no_capacity_mean_no_eta() {
        assert_eq!(eta_seconds("queued", 3, 2, None), None);
        assert_eq!(eta_seconds("queued", 3, 0, Some(600.0)), None);
    }
}

/// What a `scan_queue` row asks the admitter to run (#343 §4.4).
///
/// `Full` is the scan the user triggers. `Refresh` re-scores only this
/// user's High/Elevated rows that are about to expire or predate the current
/// scoring generation — candidates come from `account_scores`, never from
/// the network. Both run under the same claim/lease/fencing machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    Full,
    Refresh,
}

impl ScanKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ScanKind::Full => "full",
            ScanKind::Refresh => "refresh",
        }
    }

    /// Not `std::str::FromStr`: the callers want `Option` so they can attach
    /// the offending value with `.with_context(...)`, and `FromStr::Err` would
    /// force an error type that carries nothing useful here. Paired with
    /// [`ScanKind::as_str`] as a round-trip, which is the only contract the
    /// database columns need.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "full" => Some(ScanKind::Full),
            "refresh" => Some(ScanKind::Refresh),
            _ => None,
        }
    }
}

/// What `enqueue_scan` did, so the handler can tell the user the truth
/// (#344 R09): a request made while a refresh is running is not dropped —
/// it is recorded on the row and runs when the refresh finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// A new queued full row, or a queued refresh upgraded in place.
    Queued,
    /// A full row was already queued; nothing changed.
    AlreadyQueued,
    /// A full scan is running; nothing changed.
    AlreadyRunning,
    /// A refresh is running; `full_requested_at` recorded (or already was).
    QueuedAfterRefresh,
}

/// How a scan ended, recorded durably on the queue row (#344 V2-05).
///
/// `status` alone cannot carry this: an interrupted full scan and a clean one
/// both finish `done`, and the cooldown, the ETA median and the full-scan
/// obligation each need to tell them apart. The first three variants are
/// *fulfilment* — the user's request was carried out, verified or not — and
/// only [`FinishCompletion::Complete`] is clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishCompletion {
    Complete,
    CompleteWithSkips,
    CompleteUnverified,
    Resumable,
    Failed,
}

impl FinishCompletion {
    pub fn as_str(&self) -> &'static str {
        match self {
            FinishCompletion::Complete => "complete",
            FinishCompletion::CompleteWithSkips => "complete_with_skips",
            FinishCompletion::CompleteUnverified => "complete_unverified",
            FinishCompletion::Resumable => "resumable",
            FinishCompletion::Failed => "failed",
        }
    }

    /// `Option` rather than `std::str::FromStr` — see [`ScanKind::from_str`].
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "complete" => Some(FinishCompletion::Complete),
            "complete_with_skips" => Some(FinishCompletion::CompleteWithSkips),
            "complete_unverified" => Some(FinishCompletion::CompleteUnverified),
            "resumable" => Some(FinishCompletion::Resumable),
            "failed" => Some(FinishCompletion::Failed),
            _ => None,
        }
    }

    /// The user's full-scan request was carried out (V6-01). Clears
    /// `full_requested_at`; the other two leave the work owed.
    pub fn fulfils_full_request(&self) -> bool {
        matches!(
            self,
            FinishCompletion::Complete
                | FinishCompletion::CompleteWithSkips
                | FinishCompletion::CompleteUnverified
        )
    }
}

/// A successful claim on a queued scan (#257).
///
/// `claim_id` is a fencing token minted by the claim. `heartbeat_scan` and
/// `finish_queued_scan` require it, so a worker whose lease lapsed — its row
/// already reclaimed and handed to someone else — cannot free or extend the
/// new owner's slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanClaim {
    pub user_did: String,
    pub claim_id: String,
    /// Which pipeline the claimant must run (#344). Carried on the claim
    /// rather than re-read afterwards: between the claim and a second read
    /// the row can be upgraded, and the worker must run the kind it was
    /// admitted as.
    pub kind: ScanKind,
}

/// One `scan_queue` row, as the admin dashboard needs to display it (#288).
///
/// Distinct from `ScanQueueEntry`, which answers "where am *I*?" for one user
/// and carries an ETA. This is the row-level view: every column an operator
/// needs to tell a failed scan from one that never ran, with no ETA because
/// the dashboard shows a table rather than a single user's wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanQueueRow {
    pub user_did: String,
    /// "queued" | "running" | "done" | "failed"
    pub status: String,
    /// 1-based position among queued rows; 0 for any other status.
    pub position: i64,
    /// RFC3339, like every other timestamp on this trait.
    pub enqueued_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub last_error: Option<String>,
    /// Which pipeline this row runs (#344). Legacy rows read `Full`, which is
    /// what the v18 column default backfilled them to.
    pub kind: ScanKind,
    /// RFC3339 instant a full scan was first asked for and not yet delivered,
    /// or None when nothing is owed (#344 R09). Survives a refresh handover,
    /// a resumable attempt and a process restart.
    pub full_requested_at: Option<String>,
    /// How the last attempt ended. None on a row that has never finished, and
    /// on rows written before v18.
    pub completion: Option<FinishCompletion>,
}

/// How many rows are waiting versus occupying a slot (#257).
///
/// `claim_next_scan` answers "the queue is empty" and "every slot is taken"
/// with the same `Ok(None)`, so the admitter cannot tell an idle server from a
/// wedged one without counting for itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanQueueDepth {
    /// Rows waiting for a slot.
    pub queued: usize,
    /// Rows holding a slot. Counts every `running` row regardless of lease, so
    /// it matches what `claim_next_scan` compares against the cap.
    pub running: usize,
}

/// One `access_requests` row (#309).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRequestRow {
    pub did: String,
    pub handle: String,
    /// "pending" | "allowed" | "denied"
    pub status: String,
    /// RFC3339, like every other timestamp on this trait.
    pub requested_at: String,
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
}

/// One `account_feed_snapshots` row (#343 §4.1): an account's recent feed as
/// fetched, so the next scan that meets this account can skip Bluesky.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedSnapshot {
    pub did: String,
    pub handle: String,
    /// `serde_json` of `Vec<bluesky::posts::FeedPost>`.
    pub posts_json: String,
    /// RFC3339, like every other timestamp on this trait.
    pub fetched_at: String,
    /// "bluesky" today; "soot" once Phase 4 lands.
    pub source: String,
}

/// One `onnx_scores` row minus the key columns the caller already holds.
#[derive(Debug, Clone, PartialEq)]
pub struct OnnxScoreRow {
    /// Lowercase hex SHA-256 of the exact text the model scored.
    pub text_sha256: String,
    pub score: f64,
}

/// One `classifier_verdicts` row minus the key columns the caller holds.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierVerdictRow {
    pub text_sha256: String,
    pub toxic_token: bool,
    pub confidence: f64,
}

/// One write-scoped OAuth grant per user (#315). Every `*_enc` column is an
/// AES-256-GCM blob produced by `web::actions::crypto::TokenCrypto`; the DB
/// layer never sees plaintext. `access_expires_at` is unix seconds; the two
/// timestamps are RFC3339 like every other table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OauthSessionRow {
    pub user_did: String,
    pub pds_url: String,
    pub scope: String,
    pub access_token_enc: Vec<u8>,
    pub refresh_token_enc: Vec<u8>,
    pub dpop_key_enc: Vec<u8>,
    pub access_expires_at: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// One user request (#315): a tier-wide mute, a single block, an undo, or a
/// retry. `source` is free text for the log: `tier:High`, `single`,
/// `undo:<batch_id>`, `retry:<batch_id>`.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionBatchRow {
    pub id: i64,
    pub user_did: String,
    pub kind: String,
    pub source: String,
    pub requested: i64,
    pub status: String,
    pub error: Option<String>,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// One target account within a batch (#315). `record_uri` is the
/// `app.bsky.graph.block` record Charcoal itself created — the ONLY thing an
/// undo is allowed to delete. `score_at_action`/`tier_at_action` are snapshots
/// so the log explains itself after later rescans.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionRow {
    pub id: i64,
    pub batch_id: i64,
    pub user_did: String,
    pub target_did: String,
    pub kind: String,
    pub status: String,
    pub record_uri: Option<String>,
    pub undo_of: Option<i64>,
    pub error: Option<String>,
    pub score_at_action: Option<f64>,
    pub tier_at_action: Option<String>,
    pub applied_at: Option<String>,
    pub undone_at: Option<String>,
}

/// Input for `create_action_batch`; the DB assigns ids and `pending`.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAction {
    pub target_did: String,
    pub kind: String,
    pub undo_of: Option<i64>,
    pub score_at_action: Option<f64>,
    pub tier_at_action: Option<String>,
}

/// The slice of `account_scores` the actions feature needs: enough to
/// validate targets, snapshot score/tier, and compute tier drift.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoreSnapshot {
    pub did: String,
    pub handle: String,
    pub threat_score: Option<f64>,
    pub threat_tier: Option<String>,
}

#[async_trait]
pub trait Database: Send + Sync {
    // --- Lifecycle ---

    /// Count the number of user-created tables in the database.
    async fn table_count(&self) -> Result<i64>;

    // --- User management ---

    /// Create or update a user record (DID + handle).
    async fn upsert_user(&self, did: &str, handle: &str) -> Result<()>;

    /// Look up a user's handle by DID. Returns None if the user is not registered.
    async fn get_user_handle(&self, did: &str) -> Result<Option<String>>;

    // --- Scan state ---

    /// Get a scan state value by key for a specific user (e.g., "notifications_cursor").
    async fn get_scan_state(&self, user_did: &str, key: &str) -> Result<Option<String>>;

    /// Set a scan state value (upsert) for a specific user.
    async fn set_scan_state(&self, user_did: &str, key: &str, value: &str) -> Result<()>;

    /// Remove a single scan state key. Absent keys are not an error — the
    /// callers use this to retract a marker whose presence is the signal, and
    /// "already gone" is the state they wanted.
    async fn delete_scan_state(&self, user_did: &str, key: &str) -> Result<()>;

    /// Record that a full scan was carried out, in ONE transaction (#344
    /// V7-02): write `last_full_scan_finished_at` (the cooldown anchor), write
    /// [`LAST_FULL_SCAN_DURATION_KEY`] (the ETA sample, derived in the same
    /// transaction from the queue row's own `started_at` — omitted when there
    /// is none), and delete `carried_key` (the drain outcome this run has now
    /// consumed).
    ///
    /// Two calls would leave a window where the cooldown has started but the
    /// carried outcome is still there to taint an unrelated later scan — or,
    /// the other way round, the evidence is gone while the run is still owed.
    ///
    /// Fenced by `claim_id` (#344 N2): all three writes describe the attempt
    /// that holds the row, so a worker whose lease lapsed writes **nothing** —
    /// it must not sample a successor's `started_at`, start a cooldown the
    /// successor has not earned, or retire a drain outcome the successor still
    /// owes. A row that is absent, unclaimed, or claimed by someone else is
    /// logged and skipped; it is not an error, because the scan itself really
    /// did finish.
    async fn finish_full_scan_state(
        &self,
        user_did: &str,
        finished_at_rfc3339: &str,
        carried_key: &str,
        claim_id: &str,
    ) -> Result<()>;

    /// Get all scan state key-value pairs for a specific user. Used by the
    /// migration command to transfer all keys without a hardcoded list.
    async fn get_all_scan_state(&self, user_did: &str) -> Result<Vec<(String, String)>>;

    // --- Topic fingerprint ---

    /// Store the topic fingerprint for a specific user.
    async fn save_fingerprint(
        &self,
        user_did: &str,
        fingerprint_json: &str,
        post_count: u32,
    ) -> Result<()>;

    /// Store a user's mean embedding vector.
    async fn save_embedding(&self, user_did: &str, embedding: &[f64]) -> Result<()>;

    /// Load the stored fingerprint JSON, post count, and updated_at timestamp for a user.
    async fn get_fingerprint(&self, user_did: &str) -> Result<Option<(String, u32, String)>>;

    /// Load the stored embedding vector for a user (if one exists).
    async fn get_embedding(&self, user_did: &str) -> Result<Option<Vec<f64>>>;

    /// Persist a complete fingerprint generation atomically: JSON, mean
    /// embedding, and per-topic centroid rows in ONE transaction, bumping
    /// updated_at exactly once. `embedding: None` with empty `clusters` is the
    /// legal keyword-only bundle (embedder unavailable). (#302)
    ///
    /// `embedding_model_id` (#344) records which model produced `embedding` —
    /// callers pass `Some(EMBEDDING_MODEL_ID)` when `embedding.is_some()`,
    /// else `None` (keyword-only fingerprint). Read back by
    /// `fingerprint_embedding_model` to gate input compatibility (R03).
    async fn save_fingerprint_bundle(
        &self,
        user_did: &str,
        fingerprint_json: &str,
        post_count: u32,
        embedding: Option<&[f64]>,
        embedding_model_id: Option<&str>,
        clusters: &[ClusterCentroid],
    ) -> Result<()>;

    /// Load stored topic centroids ordered by cluster_index. Empty = legacy
    /// (pre-#297) or keyword-only fingerprint.
    async fn get_topic_centroids(&self, user_did: &str) -> Result<Vec<ClusterCentroid>>;

    /// The stored embedding model id for a user's fingerprint (#344). `None`
    /// means a keyword-only fingerprint, or a pre-v18 row that predates the
    /// column — either way, no vector to compare against `EMBEDDING_MODEL_ID`.
    async fn fingerprint_embedding_model(&self, user_did: &str) -> Result<Option<String>>;

    // --- Account scores ---

    /// Save or update an account's scores for a specific user.
    async fn upsert_account_score(&self, user_did: &str, score: &AccountScore) -> Result<()>;

    /// Get all scored accounts above a minimum score for a user, ranked by threat score descending.
    async fn get_ranked_threats(&self, user_did: &str, min_score: f64)
        -> Result<Vec<AccountScore>>;

    /// Check if an account's score is fresh for a user (#344). Freshness is
    /// `scoring_generation == scoring_revision() AND valid_until > now` — NOT
    /// a simple age check any more. A missing row is stale. This read is a
    /// hard error for callers (spec §4.4): a DB blip must not silently widen
    /// the re-score set or, worse, silently narrow it.
    async fn is_score_stale(&self, user_did: &str, did: &str) -> Result<bool>;

    /// Return the DIDs whose score is fresh (see `is_score_stale`) — the
    /// complement of `is_score_stale` over a whole user's scores, in one
    /// query. Discovery loops fetch this once and test membership in memory
    /// instead of one `is_score_stale` round-trip per candidate (#213).
    async fn get_fresh_scored_dids(&self, user_did: &str) -> Result<Vec<String>>;

    /// Count rows that are NOT fresh for a user (#344) — hidden from
    /// `get_ranked_threats` but never deleted. Includes legacy, expired,
    /// NULL and malformed `valid_until` rows.
    async fn count_expired(&self, user_did: &str) -> Result<i64>;

    /// Every `account_scores` row for the user, verbatim — never filtered by
    /// freshness (#344 R01). For `charcoal migrate` and any future export
    /// path; the presentation queries (`get_ranked_threats`, counts) are the
    /// wrong tool for this because they hide expired/legacy rows.
    async fn export_scores(&self, user_did: &str) -> Result<Vec<StoredScore>>;

    /// Write a row exactly as `export_scores` produced it: `scored_at`,
    /// `scoring_generation` and `valid_until` are taken from `row`, never
    /// from the clock — unlike `upsert_account_score`, which is the live
    /// scoring path and always stamps from now. Idempotent.
    async fn import_score(&self, user_did: &str, row: &StoredScore) -> Result<()>;

    /// The nightly refresh job's candidate source (#344 Task 7): rows are
    /// eligible by score (`ELEVATED_MIN`), eligible when expiring within
    /// `horizon_days`, already expired, NULL/malformed expiry (SQLite), or
    /// stamped with an old `scoring_generation` — the complement of the
    /// fresh predicate, not a copy of it. NULL-score rows never qualify
    /// (the `>=` comparison is false against NULL). Most dangerous first:
    /// `threat_score DESC, did`.
    async fn list_refresh_candidates(
        &self,
        user_did: &str,
        horizon_days: i64,
    ) -> Result<Vec<RefreshCandidate>>;

    // --- Amplification events ---

    /// Record a new amplification event for a user and return its ID.
    #[allow(clippy::too_many_arguments)]
    async fn insert_amplification_event(
        &self,
        user_did: &str,
        event_type: &str,
        amplifier_did: &str,
        amplifier_handle: &str,
        original_post_uri: &str,
        amplifier_post_uri: Option<&str>,
        amplifier_text: Option<&str>,
        original_post_text: Option<&str>,
        context_score: Option<f64>,
    ) -> Result<i64>;

    /// Insert many amplification events for a user, collapsing what used to
    /// be one network round-trip per event (359 events ≈ 2m16s of a 28m scan,
    /// chainlink #216) into a small, fixed number of round-trips regardless
    /// of batch size.
    ///
    /// Returns the number of rows inserted. Backend behavior differs:
    /// Postgres uses `UNNEST`, which binds 8 arrays plus a single scalar
    /// (user_did) and is a genuine single round-trip at any batch size.
    /// SQLite runs one transaction but chunks
    /// into multi-row `INSERT`s of 100 rows each (bind-parameter limits), so
    /// large batches issue multiple statements within that one transaction.
    ///
    /// Rows MUST be inserted in slice order so auto-increment ids ascend in
    /// input order — downstream evidence output and tests depend on it.
    /// An empty slice is a no-op returning `Ok(0)`.
    async fn insert_amplification_events_batch(
        &self,
        user_did: &str,
        events: &[NewAmplificationEvent],
    ) -> Result<usize>;

    /// Get recent amplification events for a user, ordered by detection time descending.
    async fn get_recent_events(
        &self,
        user_did: &str,
        limit: u32,
    ) -> Result<Vec<AmplificationEvent>>;

    /// Get amplification events for pile-on detection for a specific user.
    /// Returns (amplifier_did, original_post_uri, detected_at) tuples.
    async fn get_events_for_pile_on(&self, user_did: &str)
        -> Result<Vec<(String, String, String)>>;

    /// Get all amplification events for a specific amplifier DID.
    async fn get_events_by_amplifier(
        &self,
        user_did: &str,
        amplifier_did: &str,
    ) -> Result<Vec<AmplificationEvent>>;

    /// Insert an amplification event for a user, preserving its original detected_at timestamp.
    /// Used only by the migrate command so historical events keep their real timestamps
    /// instead of all being stamped with NOW().
    async fn insert_amplification_event_raw(
        &self,
        user_did: &str,
        event: &AmplificationEvent,
    ) -> Result<i64>;

    // --- Behavioral context ---

    /// Get the median engagement across all scored accounts with behavioral data for a user.
    async fn get_median_engagement(&self, user_did: &str) -> Result<f64>;

    // --- Single-account lookup ---

    /// Get a single account score by exact handle match, scoped to a user.
    async fn get_account_by_handle(
        &self,
        user_did: &str,
        handle: &str,
    ) -> Result<Option<AccountScore>>;

    /// Get a single account score by DID, scoped to a user.
    async fn get_account_by_did(&self, user_did: &str, did: &str) -> Result<Option<AccountScore>>;

    // --- User labels (ground truth for accuracy measurement) ---

    /// Create or update a user-provided label for a target account.
    async fn upsert_user_label(
        &self,
        user_did: &str,
        target_did: &str,
        label: &str,
        notes: Option<&str>,
    ) -> Result<()>;

    /// Get the user-provided label for a target account, if one exists.
    async fn get_user_label(&self, user_did: &str, target_did: &str) -> Result<Option<UserLabel>>;

    /// Get scored accounts that have no user label, sorted by threat_score DESC.
    async fn get_unlabeled_accounts(&self, user_did: &str, limit: i64)
        -> Result<Vec<AccountScore>>;

    /// Compute accuracy metrics comparing predicted tiers to user labels.
    async fn get_accuracy_metrics(&self, user_did: &str) -> Result<AccuracyMetrics>;

    // --- Inferred pairs (topic-matched post pairs for NLI scoring) ---

    /// Delete all inferred pairs for a target account (before re-inferring).
    async fn delete_inferred_pairs(&self, user_did: &str, target_did: &str) -> Result<()>;

    /// Insert a topic-matched post pair for NLI scoring.
    #[allow(clippy::too_many_arguments)]
    async fn insert_inferred_pair(
        &self,
        user_did: &str,
        target_did: &str,
        target_post_text: &str,
        target_post_uri: &str,
        user_post_text: &str,
        user_post_uri: &str,
        similarity: f64,
        context_score: Option<f64>,
    ) -> Result<i64>;

    /// Get all inferred pairs for a target account.
    async fn get_inferred_pairs(
        &self,
        user_did: &str,
        target_did: &str,
    ) -> Result<Vec<InferredPair>>;

    // --- Admin dashboard ---

    /// List all users in the system.
    async fn list_users(&self) -> Result<Vec<UserRow>>;

    /// Count scored accounts for a user.
    async fn get_scored_account_count(&self, user_did: &str) -> Result<i64>;

    /// Check if a topic fingerprint exists for a user.
    async fn has_fingerprint(&self, user_did: &str) -> Result<bool>;

    /// Delete all data for a user (cascade across all user-scoped tables).
    async fn delete_user_data(&self, user_did: &str) -> Result<()>;

    /// Update last_login_at timestamp for a user.
    async fn update_last_login(&self, did: &str) -> Result<()>;

    /// Get all DIDs that have ever been scored for a user, fresh or not.
    ///
    /// Not a scoring-eligibility gate (#344 R06) — `run_topic_first`'s
    /// discovery dedup uses `get_fresh_scored_dids` instead, so a legacy or
    /// expired row no longer suppresses re-discovery of an account. This
    /// method has no remaining production caller; kept for history/export
    /// uses where "was this DID ever scored" is the actual question.
    async fn get_all_scored_dids(&self, user_did: &str) -> Result<Vec<String>>;

    // --- Classification staging (#208) ---

    /// Enqueue a batch of classifier work-queue rows for a user.
    ///
    /// Uses UPSERT on `(user_did, account_did, post_uri)` so Phase A is fully
    /// idempotent — enqueuing the same post twice results in one row.
    async fn enqueue_classifications(&self, user_did: &str, rows: &[QueueRow]) -> Result<()>;

    /// Stash a serialised `AccountInput` blob for an account.
    ///
    /// Uses UPSERT on `(user_did, account_did)` — re-stashing replaces the blob.
    async fn stash_account_input(
        &self,
        user_did: &str,
        account_did: &str,
        payload_json: &str,
    ) -> Result<()>;

    /// Fetch up to `limit` pending (status='pending') queue rows for a user.
    async fn fetch_pending_classifications(
        &self,
        user_did: &str,
        limit: i64,
    ) -> Result<Vec<QueueRow>>;

    /// Record a batch of completed classifier verdicts.
    ///
    /// Updates each matching row to `status='done'` and fills in the verdict
    /// fields. Rows are matched by `(user_did, account_did, post_uri)`.
    async fn record_classification_verdicts(
        &self,
        user_did: &str,
        verdicts: &[VerdictRow],
    ) -> Result<()>;

    /// List the distinct `account_did` values present in the classification
    /// queue for a user.
    async fn list_scan_accounts(&self, user_did: &str) -> Result<Vec<String>>;

    /// Fetch all queue rows (any status) for a specific account.
    async fn fetch_account_verdicts(
        &self,
        user_did: &str,
        account_did: &str,
    ) -> Result<Vec<QueueRow>>;

    /// Retrieve the stashed `AccountInput` JSON blob for an account, if any.
    async fn fetch_account_input(
        &self,
        user_did: &str,
        account_did: &str,
    ) -> Result<Option<String>>;

    /// Count rows with `status='pending'` in the classification queue for a user.
    async fn count_pending_classifications(&self, user_did: &str) -> Result<i64>;

    /// Delete all staging data for a user from both `classification_queue` and
    /// `scan_account_input`.  Does NOT touch the `scan_phase` key in
    /// `scan_state` — that is the orchestrator's responsibility.
    async fn clear_scan_staging(&self, user_did: &str) -> Result<()>;

    /// Record that an account was dropped from a scan, and why (#226).
    ///
    /// A skip is a real gap in a scan's coverage, so it belongs in the database
    /// rather than only in a log line. Railway drops log messages by rate once
    /// a replica exceeds 500/sec — WARN included — which made the #220
    /// skip counts a floor rather than a total.
    ///
    /// `error` should be the FULL anyhow chain (`{e:#}`), not just the
    /// outermost context; the missing cause is what made #220 expensive.
    ///
    /// Upserts on (user_did, account_did, phase) so a failing re-gather updates
    /// rather than duplicating.
    async fn record_scan_skip(
        &self,
        user_did: &str,
        account_did: &str,
        phase: &str,
        error: &str,
    ) -> Result<()>;

    /// Number of accounts skipped in the current scan. Surfaced on the
    /// scan-complete log line so `degraded=true` carries a magnitude.
    async fn count_scan_skips(&self, user_did: &str) -> Result<i64>;

    /// All skips recorded for a user, for diagnosis and reporting.
    async fn list_scan_skips(&self, user_did: &str) -> Result<Vec<ScanSkip>>;

    /// Drop a user's recorded skips. Called at gather entry so the count always
    /// describes the CURRENT scan rather than accumulating across every run.
    async fn clear_scan_skips(&self, user_did: &str) -> Result<()>;

    /// Delete the staging data for a single account from both
    /// `classification_queue` and `scan_account_input`.  Used by Phase C to
    /// discard an account whose stashed blob is unreadable or version-stale
    /// (the deploy-straddle re-gather path) without disturbing other accounts.
    async fn clear_account_staging(&self, user_did: &str, account_did: &str) -> Result<()>;

    // --- Language abstention (#222) ---

    /// Count accounts whose `threat_tier` is `'NotAssessed'` for a user.
    ///
    /// `get_ranked_threats` filters on `threat_score >= ?`, which always
    /// excludes NULL-score NotAssessed rows — so this count cannot be derived
    /// from the accounts slice the report already has. It must come from its
    /// own query. Shared by the markdown report and (later) the status
    /// dashboard.
    async fn count_not_assessed(&self, user_did: &str) -> Result<i64>;

    // --- Scan admission queue (#257) ---

    /// Queue a full scan. Re-queues `done`/`failed` rows; upgrades a queued
    /// `refresh` in place (keeps `enqueued_at`); records `full_requested_at`
    /// on a running `refresh` so it runs afterwards; no-op on queued/running
    /// `full`. Idempotent. See `EnqueueOutcome`.
    async fn enqueue_scan(&self, user_did: &str) -> Result<EnqueueOutcome>;

    /// Queue a refresh (#343 §4.4). Re-queues `done`/`failed` rows as
    /// `refresh`; never touches a queued or running row of either kind.
    async fn enqueue_refresh_scan(&self, user_did: &str) -> Result<()>;

    /// Claim the oldest queued scan if fewer than `limit` are running.
    /// Returns the claim (user_did plus fencing token), or None when at
    /// capacity or empty. `lease_secs` sets how long the claim is valid before
    /// it can be reclaimed.
    async fn claim_next_scan(&self, limit: usize, lease_secs: i64) -> Result<Option<ScanClaim>>;

    /// Extend a running scan's lease. Called periodically while it runs.
    /// Returns false when `claim_id` no longer owns the row — the lease lapsed
    /// and someone else holds the slot, so the caller should stop.
    async fn heartbeat_scan(&self, user_did: &str, claim_id: &str, lease_secs: i64)
        -> Result<bool>;

    /// Does `claim_id` still own this user's queue row? (#344 F2)
    ///
    /// A read-only ownership probe for bookkeeping that writes OUTSIDE
    /// `scan_queue` — the refresh schedule lives on `users`, so it cannot be
    /// fenced by a `WHERE claim_id = ?` the way `finish_queued_scan` is. False
    /// for an absent row, an unclaimed row and a row claimed by someone else,
    /// matching the fence inside `finish_full_scan_state`. Unlike
    /// `heartbeat_scan` it writes nothing and does not care about `status`:
    /// the caller is finishing, not running.
    async fn scan_claim_is_current(&self, user_did: &str, claim_id: &str) -> Result<bool>;

    /// Mark a scan done (error None) or failed (error Some), releasing its slot.
    /// Returns false when the row is not running under `claim_id`, in which
    /// case nothing was changed.
    ///
    /// Writes `completion`. A `refresh` row with `full_requested_at` set
    /// becomes a queued `full` row dated from the request, obligation kept. A
    /// `full` row finishing `Complete`/`CompleteWithSkips`/`CompleteUnverified`
    /// clears `full_requested_at`; `Resumable`/`Failed` keep it (V3-03). The
    /// refresh's own outcome is recorded in `scan_state` by `run_refresh`.
    async fn finish_queued_scan(
        &self,
        user_did: &str,
        claim_id: &str,
        completion: FinishCompletion,
        error: Option<&str>,
    ) -> Result<bool>;

    /// Return running rows whose lease has lapsed to 'queued'. Called on every
    /// admitter pass, not only at boot — a lease that lapses after boot has no
    /// other backstop. Returns how many were reclaimed.
    async fn reclaim_expired_scans(&self) -> Result<usize>;

    /// Count queued and running rows, so the admitter can tell "nothing to do"
    /// from "at capacity" from "wedged".
    async fn scan_queue_depth(&self) -> Result<ScanQueueDepth>;

    /// A user's queue entry, or None if the user has never been enqueued.
    /// A row in any status ("queued", "running", "done", "failed") returns
    /// Some; only the absence of a row returns None.
    ///
    /// `concurrency_limit` is the admission cap, needed because the ETA
    /// depends on how many scans run at once — see `ScanQueueEntry::eta_seconds`.
    async fn scan_queue_entry(
        &self,
        user_did: &str,
        concurrency_limit: usize,
    ) -> Result<Option<ScanQueueEntry>>;

    /// Every `scan_queue` row, oldest first, for the admin dashboard (#288).
    ///
    /// Returns rows in ALL statuses, not just the active ones: the dashboard
    /// needs `done`/`failed` too, because "this user's last scan failed" and
    /// "this user has never scanned" are the two states the old
    /// `ScanManager`-derived column could not tell apart.
    ///
    /// One query, two consumers — the handler filters the active rows for the
    /// queue panel and indexes the whole set by DID to enrich the user table.
    /// A second, narrower method would be a second snapshot of a table that
    /// changes under it.
    async fn list_scan_queue(&self) -> Result<Vec<ScanQueueRow>>;

    // --- Refresh schedule (#343 §4.4, #344) ---
    //
    // Three columns on `users`, two different facts (V2-03):
    //
    // * `next_refresh_at` — when this user may be attempted again.
    // * `refresh_attempted_generation` — the revision an attempt has already
    //   been *scheduled* for. Written by the tick (in its claiming
    //   transaction) and by `schedule_retry_at`, so a failed, deferred or
    //   resumable attempt does not become due again by revision; only its
    //   deadline brings it back.
    // * `refreshed_generation` — the revision a *completed* run has proven.
    //   Observability, never a scheduling input.

    /// When the refresh tick may next consider this user. `None` for a user
    /// who has never been scheduled (the v18-migrated shape) — which makes
    /// them due, because the revision clause fires instead.
    async fn next_refresh_at(&self, user_did: &str) -> Result<Option<String>>;

    /// The scoring revision a completed refresh or full scan has proven for
    /// this user. Read by the runbook, never by the scheduler.
    async fn refreshed_generation(&self, user_did: &str) -> Result<Option<String>>;

    /// The scoring revision an attempt has already been scheduled for.
    async fn refresh_attempted_generation(&self, user_did: &str) -> Result<Option<String>>;

    /// Set `next_refresh_at` alone. Used after a success and by `migrate`.
    async fn schedule_refresh(&self, user_did: &str, at_rfc3339: &str) -> Result<()>;

    /// Retry: sets `next_refresh_at` AND `refresh_attempted_generation` in one
    /// statement (V4-01). Two statements would leave a window in which the
    /// deadline is set but the revision clause still fires, and the tick would
    /// claim the user it just backed off.
    async fn schedule_retry_at(
        &self,
        user_did: &str,
        at_rfc3339: &str,
        attempted_generation: &str,
    ) -> Result<()>;

    /// Proof: sets BOTH `refreshed_generation` and
    /// `refresh_attempted_generation` (V3-04) — a proven revision is also an
    /// attempted one, so the tick stays quiet after a manual full scan.
    async fn mark_refreshed_generation(&self, user_did: &str, generation: &str) -> Result<()>;

    /// Attempt only. Used solely by `charcoal migrate`, to copy a pending
    /// attempt from the source database verbatim; production code reaches
    /// this column through `schedule_retry_at` or the tick.
    async fn mark_refresh_attempted_generation(
        &self,
        user_did: &str,
        generation: &str,
    ) -> Result<()>;

    /// ONE transaction: select up to `limit` due users, then for each
    /// CONDITIONALLY write the refresh queue row (`ON CONFLICT … WHERE
    /// status IN ('done','failed')`) and, only if that write affected a row,
    /// set `next_refresh_at = next_rfc3339` and
    /// `refresh_attempted_generation = current_generation`.
    ///
    /// A user is due when they have at least one score row **or** a finished
    /// queue row that still owes a full scan (V4-01), have no queued or
    /// running row of either kind, and either their deadline has passed or
    /// `refresh_attempted_generation` is not `current_generation`.
    ///
    /// Returns the DIDs actually delivered — a user whose row turned out to be
    /// queued or running at write time is left entirely alone (schedule
    /// included) and reconsidered next tick (V2-02). A failure rolls back
    /// everything, so nothing is half-scheduled (R04).
    async fn claim_and_enqueue_due_refreshes(
        &self,
        now_rfc3339: &str,
        next_rfc3339: &str,
        current_generation: &str,
        limit: usize,
    ) -> Result<Vec<String>>;

    // --- Access requests (#309) ---

    async fn get_access_request(&self, did: &str) -> Result<Option<AccessRequestRow>>;

    /// Creates a 'pending' row; if a row exists, refreshes handle ONLY (status untouched).
    async fn upsert_access_request_pending(&self, did: &str, handle: &str) -> Result<()>;

    /// Sets status + decided_at/decided_by on an existing row. Returns false if no row.
    async fn set_access_status(&self, did: &str, status: &str, decided_by: &str) -> Result<bool>;

    /// Admin grant-by-handle: upsert straight to 'allowed' (works with or without a prior row).
    async fn grant_access(&self, did: &str, handle: &str, decided_by: &str) -> Result<()>;

    /// All rows, oldest requested_at first.
    async fn list_access_requests(&self) -> Result<Vec<AccessRequestRow>>;

    // --- OAuth write sessions (#315) ---

    async fn get_oauth_session(&self, user_did: &str) -> Result<Option<OauthSessionRow>>;

    /// Insert, or replace every column except `created_at` (re-consent keeps
    /// the original connection date).
    async fn upsert_oauth_session(&self, row: &OauthSessionRow) -> Result<()>;

    /// Compare-and-swap token rotation. Writes the new pair ONLY when the
    /// row's `updated_at` still equals `expected_updated_at`, and returns
    /// whether it did. AT Protocol refresh tokens are single-use, so two
    /// concurrent refreshes must never both persist — the loser sees `false`
    /// and re-reads the winner's tokens (`web::actions::session`). `scope`
    /// is the grant the token endpoint reported with the rotated pair; it
    /// travels with the tokens so the row never describes an older grant.
    #[allow(clippy::too_many_arguments)]
    async fn update_oauth_tokens(
        &self,
        user_did: &str,
        access_token_enc: &[u8],
        refresh_token_enc: &[u8],
        access_expires_at: i64,
        scope: &str,
        expected_updated_at: &str,
        new_updated_at: &str,
    ) -> Result<bool>;

    /// Returns whether a row existed.
    async fn delete_oauth_session(&self, user_did: &str) -> Result<bool>;

    /// Compare-and-delete: remove the row only if its `updated_at` still
    /// equals `expected_updated_at`. The refresh path uses this so a session
    /// replaced mid-request (re-consent, another replica) is never deleted on
    /// the strength of a stale read. Returns whether a row was deleted.
    async fn delete_oauth_session_if_unchanged(
        &self,
        user_did: &str,
        expected_updated_at: &str,
    ) -> Result<bool>;

    // --- Action batches (#315) ---

    /// One transaction: the batch (`queued`, `requested = rows.len()`) and
    /// every action (`pending`). Returns the batch id.
    async fn create_action_batch(
        &self,
        user_did: &str,
        kind: &str,
        source: &str,
        rows: &[NewAction],
    ) -> Result<i64>;

    async fn get_action_batch(&self, id: i64) -> Result<Option<ActionBatchRow>>;

    /// Newest first (`created_at DESC, id DESC`), scoped to one user.
    async fn list_action_batches(
        &self,
        user_did: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ActionBatchRow>>;

    /// `id ASC`.
    async fn list_actions_for_batch(&self, batch_id: i64) -> Result<Vec<ActionRow>>;

    /// Every batch in `queued` or `running`, across all users, `id ASC`.
    /// Boot-time resume (§4.5).
    async fn list_unfinished_batches(&self) -> Result<Vec<i64>>;

    /// Stamps `started_at` the first time a batch goes `running`, and
    /// `finished_at` on any terminal status (`done`/`partial`/`failed`).
    /// `error` replaces the stored error (pass `None` to clear it).
    async fn set_action_batch_status(
        &self,
        id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<()>;

    /// Stamps `applied_at` on `applied`/`skipped_already_done` and `undone_at`
    /// on `undone`. `record_uri` is written only when `Some` — an undo must
    /// never erase the URI that proves what Charcoal created.
    async fn update_action(
        &self,
        id: i64,
        status: &str,
        record_uri: Option<&str>,
        error: Option<&str>,
    ) -> Result<()>;

    async fn get_action(&self, id: i64) -> Result<Option<ActionRow>>;

    /// Rows currently in effect for a user: `applied` or
    /// `skipped_already_done`, `id ASC`. Drives "already muted" in the confirm
    /// sheet and the detail-page buttons.
    async fn active_actions(&self, user_did: &str) -> Result<Vec<ActionRow>>;

    /// `did, handle, threat_score, threat_tier` for every scored account of
    /// the user. Target validation, snapshots, and drift all read this.
    async fn list_score_snapshots(&self, user_did: &str) -> Result<Vec<ScoreSnapshot>>;

    // --- Shared cache (#343 §4.1) ---
    //
    // None of these take a user_did: a post's toxicity is a property of the
    // post. Lookups take a batch of hashes and return only the hits, so a
    // decorator can score the misses in one pass.

    async fn get_feed_snapshot(&self, did: &str) -> Result<Option<FeedSnapshot>>;

    /// Insert or replace every column for `snapshot.did`.
    async fn upsert_feed_snapshot(&self, snapshot: &FeedSnapshot) -> Result<()>;

    /// Scores for `model_id` keyed by hash — absent hashes are simply absent.
    async fn get_onnx_scores(
        &self,
        model_id: &str,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, f64>>;

    /// Insert or replace; `scored_at` is set by the backend.
    async fn upsert_onnx_scores(&self, model_id: &str, rows: &[OnnxScoreRow]) -> Result<()>;

    async fn get_classifier_verdicts(
        &self,
        model_id: &str,
        policy_version: &str,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, ClassifierVerdictRow>>;

    /// Insert or replace; `classified_at` is set by the backend.
    async fn upsert_classifier_verdicts(
        &self,
        model_id: &str,
        policy_version: &str,
        rows: &[ClassifierVerdictRow],
    ) -> Result<()>;

    /// Delete cache rows older than the given RFC3339 cutoffs — `feed_cutoff`
    /// for `account_feed_snapshots`, `score_cutoff` for both scoring tables.
    /// Returns how many rows went, per table.
    ///
    /// Comparisons are lexicographic on the stored RFC3339 TEXT. That is sound
    /// for one reason only: **every writer stamps these columns with
    /// `DateTime<Utc>::to_rfc3339()`**, which always ends in `+00:00` (never
    /// `Z`). Its fractional-seconds part is *not* fixed width (0, 3, 6 or 9
    /// digits), but a shorter fraction terminates in `+` (0x2B), which sorts
    /// below `.` (0x2E) and every digit, so a truncated stamp never outranks a
    /// longer one in the same second. A future writer that emits `Z` (0x5A,
    /// above the digits) would silently make stale rows survive the sweep —
    /// keep using `to_rfc3339()`. Postgres compares TEXT under the database
    /// collation rather than byte order, so the worst case there is a
    /// sub-second boundary error against a 7-/90-day window. Callers compute
    /// the cutoffs in Rust; see [`crate::db::cache_retention`], which also
    /// wraps this so a failed sweep never fails a scan.
    async fn evict_stale_cache(
        &self,
        feed_cutoff: &str,
        score_cutoff: &str,
    ) -> Result<crate::db::cache_retention::CacheEviction>;
}

/// Reject bundles that would poison future cosines: every stored float must
/// be finite, and every vector must be exactly `EMBEDDING_DIM` wide. Runs
/// before any I/O so a bad build can never split a generation.
/// (#296 discipline, applied to #302.)
///
/// The width check exists because the two backends disagree about what they
/// will accept: PostgreSQL's `vector(384)` rejects a wrong-width row outright,
/// while SQLite stores centroids as unconstrained JSON. A wrong-width centroid
/// that reached SQLite would then score 0.0 in every future cosine — inert, but
/// indistinguishable from real data. Reject at the write instead.
/// (CodeRabbit PR #102)
pub fn validate_bundle(embedding: Option<&[f64]>, clusters: &[ClusterCentroid]) -> Result<()> {
    use crate::topics::embeddings::EMBEDDING_DIM;

    if let Some(emb) = embedding {
        anyhow::ensure!(
            emb.iter().all(|v| v.is_finite()),
            "non-finite value in mean embedding",
        );
        anyhow::ensure!(
            emb.len() == EMBEDDING_DIM,
            "mean embedding has {} dimensions, expected {EMBEDDING_DIM}",
            emb.len(),
        );
    }
    for (i, cluster) in clusters.iter().enumerate() {
        anyhow::ensure!(
            cluster.centroid.iter().all(|v| v.is_finite()),
            "non-finite value in centroid of cluster {i}",
        );
        anyhow::ensure!(
            cluster.centroid.len() == EMBEDDING_DIM,
            "centroid of cluster {i} has {} dimensions, expected {EMBEDDING_DIM}",
            cluster.centroid.len(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod validate_bundle_tests {
    use super::{validate_bundle, ClusterCentroid};
    use crate::topics::embeddings::EMBEDDING_DIM;

    fn cluster(width: usize) -> ClusterCentroid {
        ClusterCentroid {
            centroid: vec![0.5; width],
            post_count: 3,
        }
    }

    #[test]
    fn well_formed_bundle_passes() {
        let emb = vec![0.25; EMBEDDING_DIM];
        assert!(validate_bundle(Some(&emb), &[cluster(EMBEDDING_DIM)]).is_ok());
        // Keyword-only: no embedding, no centroids.
        assert!(validate_bundle(None, &[]).is_ok());
    }

    #[test]
    fn wrong_width_embedding_is_rejected() {
        let emb = vec![0.25; EMBEDDING_DIM - 1];
        assert!(validate_bundle(Some(&emb), &[]).is_err());
    }

    #[test]
    fn wrong_width_centroid_is_rejected() {
        let emb = vec![0.25; EMBEDDING_DIM];
        // One good, one narrow — the whole bundle must fail, not persist half.
        let clusters = vec![cluster(EMBEDDING_DIM), cluster(8)];
        assert!(validate_bundle(Some(&emb), &clusters).is_err());
    }
}
