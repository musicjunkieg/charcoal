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

use super::models::{AccountScore, AccuracyMetrics, AmplificationEvent, InferredPair, UserLabel};

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

    // --- Account scores ---

    /// Save or update an account's scores for a specific user.
    async fn upsert_account_score(&self, user_did: &str, score: &AccountScore) -> Result<()>;

    /// Get all scored accounts above a minimum score for a user, ranked by threat score descending.
    async fn get_ranked_threats(&self, user_did: &str, min_score: f64)
        -> Result<Vec<AccountScore>>;

    /// Check if an account's score is stale for a user (older than the given number of days).
    async fn is_score_stale(&self, user_did: &str, did: &str, max_age_days: i64) -> Result<bool>;

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
}
