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
    NewAmplificationEvent, UserLabel, UserRow,
};
use crate::pipeline::scan_phases::staging::{QueueRow, VerdictRow};

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
    pub eta_seconds: Option<i64>,
    pub enqueued_at: String,
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
    async fn save_fingerprint_bundle(
        &self,
        user_did: &str,
        fingerprint_json: &str,
        post_count: u32,
        embedding: Option<&[f64]>,
        clusters: &[ClusterCentroid],
    ) -> Result<()>;

    /// Load stored topic centroids ordered by cluster_index. Empty = legacy
    /// (pre-#297) or keyword-only fingerprint.
    async fn get_topic_centroids(&self, user_did: &str) -> Result<Vec<ClusterCentroid>>;

    // --- Account scores ---

    /// Save or update an account's scores for a specific user.
    async fn upsert_account_score(&self, user_did: &str, score: &AccountScore) -> Result<()>;

    /// Get all scored accounts above a minimum score for a user, ranked by threat score descending.
    async fn get_ranked_threats(&self, user_did: &str, min_score: f64)
        -> Result<Vec<AccountScore>>;

    /// Check if an account's score is stale for a user (older than the given number of days).
    async fn is_score_stale(&self, user_did: &str, did: &str, max_age_days: i64) -> Result<bool>;

    /// Return the DIDs scored within the last `max_age_days` — the complement
    /// of `is_score_stale` over a whole user's scores, in one query. Discovery
    /// loops fetch this once and test membership in memory instead of one
    /// `is_score_stale` round-trip per candidate (#213).
    async fn get_fresh_scored_dids(&self, user_did: &str, max_age_days: i64)
        -> Result<Vec<String>>;

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

    /// Get all DIDs that have been scored for a user (for deduplication during discovery).
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

    /// Add a user to the scan queue. Idempotent — a second call while queued or
    /// running is a no-op, so a double-click cannot double-book.
    async fn enqueue_scan(&self, user_did: &str) -> Result<()>;

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

    /// Mark a scan done (error None) or failed (error Some), releasing its slot.
    /// Returns false when the row is not running under `claim_id`, in which
    /// case nothing was changed.
    async fn finish_queued_scan(
        &self,
        user_did: &str,
        claim_id: &str,
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
    /// and re-reads the winner's tokens (`web::actions::session`).
    async fn update_oauth_tokens(
        &self,
        user_did: &str,
        access_token_enc: &[u8],
        refresh_token_enc: &[u8],
        access_expires_at: i64,
        expected_updated_at: &str,
        new_updated_at: &str,
    ) -> Result<bool>;

    /// Returns whether a row existed.
    async fn delete_oauth_session(&self, user_did: &str) -> Result<bool>;
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
