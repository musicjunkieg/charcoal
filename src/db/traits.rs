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

use super::models::{AccountScore, AmplificationEvent};

#[async_trait]
pub trait Database: Send + Sync {
    // --- Lifecycle ---

    /// Count the number of user-created tables in the database.
    async fn table_count(&self) -> Result<i64>;

    // --- Scan state ---

    /// Get a scan state value by key (e.g., "notifications_cursor").
    async fn get_scan_state(&self, key: &str) -> Result<Option<String>>;

    /// Set a scan state value (upsert).
    async fn set_scan_state(&self, key: &str, value: &str) -> Result<()>;

    // --- Topic fingerprint ---

    /// Store the topic fingerprint (singleton row).
    async fn save_fingerprint(&self, fingerprint_json: &str, post_count: u32) -> Result<()>;

    /// Store the protected user's mean embedding vector.
    async fn save_embedding(&self, embedding: &[f64]) -> Result<()>;

    /// Load the stored fingerprint JSON, post count, and updated_at timestamp.
    async fn get_fingerprint(&self) -> Result<Option<(String, u32, String)>>;

    /// Load the stored embedding vector (if one exists).
    async fn get_embedding(&self) -> Result<Option<Vec<f64>>>;

    // --- Account scores ---

    /// Save or update an account's scores.
    async fn upsert_account_score(&self, score: &AccountScore) -> Result<()>;

    /// Get all scored accounts above a minimum score, ranked by threat score descending.
    async fn get_ranked_threats(&self, min_score: f64) -> Result<Vec<AccountScore>>;

    /// Check if an account's score is stale (older than the given number of days).
    async fn is_score_stale(&self, did: &str, max_age_days: i64) -> Result<bool>;

    // --- Amplification events ---

    /// Record a new amplification event and return its ID.
    async fn insert_amplification_event(
        &self,
        event_type: &str,
        amplifier_did: &str,
        amplifier_handle: &str,
        original_post_uri: &str,
        amplifier_post_uri: Option<&str>,
        amplifier_text: Option<&str>,
    ) -> Result<i64>;

    /// Get recent amplification events, ordered by detection time descending.
    async fn get_recent_events(&self, limit: u32) -> Result<Vec<AmplificationEvent>>;

    /// Get amplification events for pile-on detection.
    /// Returns (amplifier_did, original_post_uri, detected_at) tuples.
    async fn get_events_for_pile_on(&self) -> Result<Vec<(String, String, String)>>;

    // --- Behavioral context ---

    /// Get the median engagement across all scored accounts with behavioral data.
    async fn get_median_engagement(&self) -> Result<f64>;
}
