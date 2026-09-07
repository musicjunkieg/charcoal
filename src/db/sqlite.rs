// SqliteDatabase — rusqlite backend implementing the Database trait.
//
// The Connection is wrapped in tokio::sync::Mutex because Connection is !Send.
// Trait methods lock the mutex, do synchronous rusqlite work, and return.
// The lock is never held across .await points — Rust enforces this because
// MutexGuard is !Send.
//
// The free functions in queries.rs remain unchanged so existing tests
// continue to work against Connection directly.

use anyhow::Result;
use async_trait::async_trait;
use rusqlite::Connection;
use tokio::sync::Mutex;

use super::models::{
    AccountScore, AccuracyMetrics, AmplificationEvent, ClusterCentroid, InferredPair,
    NewAmplificationEvent, UserLabel, UserRow,
};
use super::traits::{
    validate_bundle, AccessRequestRow, ActionBatchRow, ActionRow, Database, NewAction,
    OauthSessionRow, ScanClaim, ScanQueueDepth, ScanQueueEntry, ScanQueueRow, ScanSkip,
    ScoreSnapshot,
};
use crate::pipeline::scan_phases::staging::{QueueRow, VerdictRow};

pub struct SqliteDatabase {
    conn: Mutex<Connection>,
}

impl SqliteDatabase {
    /// Wrap an already-opened rusqlite Connection.
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }
}

#[async_trait]
impl Database for SqliteDatabase {
    async fn table_count(&self) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::schema::table_count(&conn)
    }

    async fn upsert_user(&self, did: &str, handle: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_user(&conn, did, handle)
    }

    async fn get_user_handle(&self, did: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        super::queries::get_user_handle(&conn, did)
    }

    async fn get_scan_state(&self, user_did: &str, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        super::queries::get_scan_state(&conn, user_did, key)
    }

    async fn set_scan_state(&self, user_did: &str, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::set_scan_state(&conn, user_did, key, value)
    }

    async fn get_all_scan_state(&self, user_did: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().await;
        super::queries::get_all_scan_state(&conn, user_did)
    }

    async fn save_fingerprint(
        &self,
        user_did: &str,
        fingerprint_json: &str,
        post_count: u32,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::save_fingerprint(&conn, user_did, fingerprint_json, post_count)
    }

    async fn save_embedding(&self, user_did: &str, embedding: &[f64]) -> Result<()> {
        let json = serde_json::to_string(embedding)?;
        let conn = self.conn.lock().await;
        super::queries::save_embedding(&conn, user_did, &json)
    }

    async fn get_fingerprint(&self, user_did: &str) -> Result<Option<(String, u32, String)>> {
        let conn = self.conn.lock().await;
        super::queries::get_fingerprint(&conn, user_did)
    }

    async fn get_embedding(&self, user_did: &str) -> Result<Option<Vec<f64>>> {
        let conn = self.conn.lock().await;
        super::queries::get_embedding(&conn, user_did)
    }

    async fn save_fingerprint_bundle(
        &self,
        user_did: &str,
        fingerprint_json: &str,
        post_count: u32,
        embedding: Option<&[f64]>,
        clusters: &[ClusterCentroid],
    ) -> Result<()> {
        validate_bundle(embedding, clusters)?;
        let embedding_json = embedding.map(serde_json::to_string).transpose()?;
        let conn = self.conn.lock().await;
        super::queries::save_fingerprint_bundle(
            &conn,
            user_did,
            fingerprint_json,
            post_count,
            embedding_json.as_deref(),
            clusters,
        )
    }

    async fn get_topic_centroids(&self, user_did: &str) -> Result<Vec<ClusterCentroid>> {
        let conn = self.conn.lock().await;
        super::queries::get_topic_centroids(&conn, user_did)
    }

    async fn upsert_account_score(&self, user_did: &str, score: &AccountScore) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_account_score(&conn, user_did, score)
    }

    async fn get_ranked_threats(
        &self,
        user_did: &str,
        min_score: f64,
    ) -> Result<Vec<AccountScore>> {
        let conn = self.conn.lock().await;
        super::queries::get_ranked_threats(&conn, user_did, min_score)
    }

    async fn is_score_stale(&self, user_did: &str, did: &str, max_age_days: i64) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::is_score_stale(&conn, user_did, did, max_age_days)
    }

    async fn get_fresh_scored_dids(
        &self,
        user_did: &str,
        max_age_days: i64,
    ) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        super::queries::get_fresh_scored_dids(&conn, user_did, max_age_days)
    }

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
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::insert_amplification_event(
            &conn,
            user_did,
            event_type,
            amplifier_did,
            amplifier_handle,
            original_post_uri,
            amplifier_post_uri,
            amplifier_text,
            original_post_text,
            context_score,
        )
    }

    async fn insert_amplification_events_batch(
        &self,
        user_did: &str,
        events: &[NewAmplificationEvent],
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        super::queries::insert_amplification_events_batch(&conn, user_did, events)
    }

    async fn get_recent_events(
        &self,
        user_did: &str,
        limit: u32,
    ) -> Result<Vec<AmplificationEvent>> {
        let conn = self.conn.lock().await;
        super::queries::get_recent_events(&conn, user_did, limit)
    }

    async fn get_events_for_pile_on(
        &self,
        user_did: &str,
    ) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().await;
        super::queries::get_events_for_pile_on(&conn, user_did)
    }

    async fn get_events_by_amplifier(
        &self,
        user_did: &str,
        amplifier_did: &str,
    ) -> Result<Vec<AmplificationEvent>> {
        let conn = self.conn.lock().await;
        super::queries::get_events_by_amplifier(&conn, user_did, amplifier_did)
    }

    async fn get_median_engagement(&self, user_did: &str) -> Result<f64> {
        let conn = self.conn.lock().await;
        super::queries::get_median_engagement(&conn, user_did)
    }

    async fn insert_amplification_event_raw(
        &self,
        user_did: &str,
        event: &super::models::AmplificationEvent,
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::insert_amplification_event_with_detected_at(&conn, user_did, event)
    }

    async fn get_account_by_handle(
        &self,
        user_did: &str,
        handle: &str,
    ) -> Result<Option<AccountScore>> {
        let conn = self.conn.lock().await;
        super::queries::get_account_by_handle(&conn, user_did, handle)
    }

    async fn get_account_by_did(&self, user_did: &str, did: &str) -> Result<Option<AccountScore>> {
        let conn = self.conn.lock().await;
        super::queries::get_account_by_did(&conn, user_did, did)
    }

    async fn upsert_user_label(
        &self,
        user_did: &str,
        target_did: &str,
        label: &str,
        notes: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_user_label(&conn, user_did, target_did, label, notes)
    }

    async fn get_user_label(&self, user_did: &str, target_did: &str) -> Result<Option<UserLabel>> {
        let conn = self.conn.lock().await;
        super::queries::get_user_label(&conn, user_did, target_did)
    }

    async fn get_unlabeled_accounts(
        &self,
        user_did: &str,
        limit: i64,
    ) -> Result<Vec<AccountScore>> {
        let conn = self.conn.lock().await;
        super::queries::get_unlabeled_accounts(&conn, user_did, limit)
    }

    async fn get_accuracy_metrics(&self, user_did: &str) -> Result<AccuracyMetrics> {
        let conn = self.conn.lock().await;
        super::queries::get_accuracy_metrics(&conn, user_did)
    }

    async fn delete_inferred_pairs(&self, user_did: &str, target_did: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::delete_inferred_pairs(&conn, user_did, target_did)
    }

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
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::insert_inferred_pair(
            &conn,
            user_did,
            target_did,
            target_post_text,
            target_post_uri,
            user_post_text,
            user_post_uri,
            similarity,
            context_score,
        )
    }

    async fn get_inferred_pairs(
        &self,
        user_did: &str,
        target_did: &str,
    ) -> Result<Vec<InferredPair>> {
        let conn = self.conn.lock().await;
        super::queries::get_inferred_pairs(&conn, user_did, target_did)
    }

    async fn list_users(&self) -> Result<Vec<UserRow>> {
        let conn = self.conn.lock().await;
        super::queries::list_users(&conn)
    }

    async fn get_scored_account_count(&self, user_did: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::get_scored_account_count(&conn, user_did)
    }

    async fn has_fingerprint(&self, user_did: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::has_fingerprint(&conn, user_did)
    }

    async fn delete_user_data(&self, user_did: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::delete_user_data(&conn, user_did)
    }

    async fn update_last_login(&self, did: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::update_last_login(&conn, did)
    }

    async fn get_all_scored_dids(&self, user_did: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        super::queries::get_all_scored_dids(&conn, user_did)
    }

    // --- Classification staging (#208) ---

    async fn enqueue_classifications(&self, user_did: &str, rows: &[QueueRow]) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::enqueue_classifications(&conn, user_did, rows)
    }

    async fn stash_account_input(
        &self,
        user_did: &str,
        account_did: &str,
        payload_json: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::stash_account_input(&conn, user_did, account_did, payload_json)
    }

    async fn fetch_pending_classifications(
        &self,
        user_did: &str,
        limit: i64,
    ) -> Result<Vec<QueueRow>> {
        let conn = self.conn.lock().await;
        super::queries::fetch_pending_classifications(&conn, user_did, limit)
    }

    async fn record_classification_verdicts(
        &self,
        user_did: &str,
        verdicts: &[VerdictRow],
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::record_classification_verdicts(&conn, user_did, verdicts)
    }

    async fn list_scan_accounts(&self, user_did: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        super::queries::list_scan_accounts(&conn, user_did)
    }

    async fn fetch_account_verdicts(
        &self,
        user_did: &str,
        account_did: &str,
    ) -> Result<Vec<QueueRow>> {
        let conn = self.conn.lock().await;
        super::queries::fetch_account_verdicts(&conn, user_did, account_did)
    }

    async fn fetch_account_input(
        &self,
        user_did: &str,
        account_did: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        super::queries::fetch_account_input(&conn, user_did, account_did)
    }

    async fn count_pending_classifications(&self, user_did: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::count_pending_classifications(&conn, user_did)
    }

    async fn clear_scan_staging(&self, user_did: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::clear_scan_staging(&conn, user_did)
    }

    async fn clear_account_staging(&self, user_did: &str, account_did: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::clear_account_staging(&conn, user_did, account_did)
    }

    async fn record_scan_skip(
        &self,
        user_did: &str,
        account_did: &str,
        phase: &str,
        error: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::record_scan_skip(&conn, user_did, account_did, phase, error)
    }

    async fn count_scan_skips(&self, user_did: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::count_scan_skips(&conn, user_did)
    }

    async fn list_scan_skips(&self, user_did: &str) -> Result<Vec<ScanSkip>> {
        let conn = self.conn.lock().await;
        super::queries::list_scan_skips(&conn, user_did)
    }

    async fn clear_scan_skips(&self, user_did: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::clear_scan_skips(&conn, user_did)
    }

    async fn count_not_assessed(&self, user_did: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::count_not_assessed(&conn, user_did)
    }

    // --- Scan admission queue (#257) ---

    async fn enqueue_scan(&self, user_did: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::enqueue_scan(&conn, user_did)
    }

    async fn claim_next_scan(&self, limit: usize, lease_secs: i64) -> Result<Option<ScanClaim>> {
        // The guard is held for the whole call, which is what makes in-process
        // admission single-file here — see the note in queries.rs on why this
        // backend needs no advisory lock.
        let conn = self.conn.lock().await;
        super::queries::claim_next_scan(&conn, limit, lease_secs)
    }

    async fn heartbeat_scan(
        &self,
        user_did: &str,
        claim_id: &str,
        lease_secs: i64,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::heartbeat_scan(&conn, user_did, claim_id, lease_secs)
    }

    async fn finish_queued_scan(
        &self,
        user_did: &str,
        claim_id: &str,
        error: Option<&str>,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::finish_queued_scan(&conn, user_did, claim_id, error)
    }

    async fn reclaim_expired_scans(&self) -> Result<usize> {
        let conn = self.conn.lock().await;
        super::queries::reclaim_expired_scans(&conn)
    }

    async fn scan_queue_depth(&self) -> Result<ScanQueueDepth> {
        let conn = self.conn.lock().await;
        super::queries::scan_queue_depth(&conn)
    }

    async fn scan_queue_entry(
        &self,
        user_did: &str,
        concurrency_limit: usize,
    ) -> Result<Option<ScanQueueEntry>> {
        let conn = self.conn.lock().await;
        super::queries::scan_queue_entry(&conn, user_did, concurrency_limit)
    }

    async fn list_scan_queue(&self) -> Result<Vec<ScanQueueRow>> {
        let conn = self.conn.lock().await;
        super::queries::list_scan_queue(&conn)
    }

    // --- Access requests (#309) ---

    async fn get_access_request(&self, did: &str) -> Result<Option<AccessRequestRow>> {
        let conn = self.conn.lock().await;
        super::queries::get_access_request(&conn, did)
    }

    async fn upsert_access_request_pending(&self, did: &str, handle: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_access_request_pending(&conn, did, handle)
    }

    async fn set_access_status(&self, did: &str, status: &str, decided_by: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::set_access_status(&conn, did, status, decided_by)
    }

    async fn grant_access(&self, did: &str, handle: &str, decided_by: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::grant_access(&conn, did, handle, decided_by)
    }

    async fn list_access_requests(&self) -> Result<Vec<AccessRequestRow>> {
        let conn = self.conn.lock().await;
        super::queries::list_access_requests(&conn)
    }

    // --- OAuth write sessions (#315) ---

    async fn get_oauth_session(&self, user_did: &str) -> Result<Option<OauthSessionRow>> {
        let conn = self.conn.lock().await;
        super::queries::get_oauth_session(&conn, user_did)
    }

    async fn upsert_oauth_session(&self, row: &OauthSessionRow) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_oauth_session(&conn, row)
    }

    async fn update_oauth_tokens(
        &self,
        user_did: &str,
        access_token_enc: &[u8],
        refresh_token_enc: &[u8],
        access_expires_at: i64,
        scope: &str,
        expected_updated_at: &str,
        new_updated_at: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::update_oauth_tokens(
            &conn,
            user_did,
            access_token_enc,
            refresh_token_enc,
            access_expires_at,
            scope,
            expected_updated_at,
            new_updated_at,
        )
    }

    async fn delete_oauth_session(&self, user_did: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::delete_oauth_session(&conn, user_did)
    }

    async fn delete_oauth_session_if_unchanged(
        &self,
        user_did: &str,
        expected_updated_at: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::delete_oauth_session_if_unchanged(&conn, user_did, expected_updated_at)
    }

    // --- Action batches (#315) ---

    async fn create_action_batch(
        &self,
        user_did: &str,
        kind: &str,
        source: &str,
        rows: &[NewAction],
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::create_action_batch(&conn, user_did, kind, source, rows)
    }

    async fn get_action_batch(&self, id: i64) -> Result<Option<ActionBatchRow>> {
        let conn = self.conn.lock().await;
        super::queries::get_action_batch(&conn, id)
    }

    async fn list_action_batches(
        &self,
        user_did: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ActionBatchRow>> {
        let conn = self.conn.lock().await;
        super::queries::list_action_batches(&conn, user_did, limit, offset)
    }

    async fn list_actions_for_batch(&self, batch_id: i64) -> Result<Vec<ActionRow>> {
        let conn = self.conn.lock().await;
        super::queries::list_actions_for_batch(&conn, batch_id)
    }

    async fn list_unfinished_batches(&self) -> Result<Vec<i64>> {
        let conn = self.conn.lock().await;
        super::queries::list_unfinished_batches(&conn)
    }

    async fn set_action_batch_status(
        &self,
        id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::set_action_batch_status(&conn, id, status, error)
    }

    async fn update_action(
        &self,
        id: i64,
        status: &str,
        record_uri: Option<&str>,
        error: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::update_action(&conn, id, status, record_uri, error)
    }

    async fn get_action(&self, id: i64) -> Result<Option<ActionRow>> {
        let conn = self.conn.lock().await;
        super::queries::get_action(&conn, id)
    }

    async fn active_actions(&self, user_did: &str) -> Result<Vec<ActionRow>> {
        let conn = self.conn.lock().await;
        super::queries::active_actions(&conn, user_did)
    }

    async fn list_score_snapshots(&self, user_did: &str) -> Result<Vec<ScoreSnapshot>> {
        let conn = self.conn.lock().await;
        super::queries::list_score_snapshots(&conn, user_did)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::create_tables;

    const TEST_USER: &str = "did:plc:testuser000000000000";

    async fn test_db() -> SqliteDatabase {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        SqliteDatabase::new(conn)
    }

    #[tokio::test]
    async fn test_trait_scan_state_roundtrip() {
        let db = test_db().await;
        assert_eq!(db.get_scan_state(TEST_USER, "cursor").await.unwrap(), None);
        db.set_scan_state(TEST_USER, "cursor", "abc123")
            .await
            .unwrap();
        assert_eq!(
            db.get_scan_state(TEST_USER, "cursor").await.unwrap(),
            Some("abc123".to_string())
        );
    }

    #[tokio::test]
    async fn test_trait_fingerprint_roundtrip() {
        let db = test_db().await;
        assert!(db.get_fingerprint(TEST_USER).await.unwrap().is_none());
        db.save_fingerprint(TEST_USER, r#"{"topics": []}"#, 100)
            .await
            .unwrap();
        let (json, count, _) = db.get_fingerprint(TEST_USER).await.unwrap().unwrap();
        assert_eq!(json, r#"{"topics": []}"#);
        assert_eq!(count, 100);
    }

    #[tokio::test]
    async fn test_bundle_roundtrip_full() {
        let db = test_db().await;
        let clusters = vec![
            crate::db::models::ClusterCentroid {
                centroid: vec![0.5; 384],
                post_count: 30,
            },
            crate::db::models::ClusterCentroid {
                centroid: vec![-0.25; 384],
                post_count: 12,
            },
        ];
        let emb = vec![0.125; 384];
        db.save_fingerprint_bundle(
            TEST_USER,
            "{\"clusters\":[],\"post_count\":42}",
            42,
            Some(&emb),
            &clusters,
        )
        .await
        .unwrap();

        let (json, count, updated_at) = db.get_fingerprint(TEST_USER).await.unwrap().unwrap();
        assert_eq!(count, 42);
        assert!(json.contains("post_count"));
        // updated_at honors the staleness parser's format contract.
        assert!(chrono::NaiveDateTime::parse_from_str(&updated_at, "%Y-%m-%d %H:%M:%S").is_ok());

        let stored_emb = db.get_embedding(TEST_USER).await.unwrap().unwrap();
        assert_eq!(stored_emb.len(), 384);
        let stored = db.get_topic_centroids(TEST_USER).await.unwrap();
        assert_eq!(stored, clusters); // order = cluster_index
    }

    #[tokio::test]
    async fn test_bundle_replaces_previous_generation_completely() {
        let db = test_db().await;
        let three = vec![
            crate::db::models::ClusterCentroid {
                centroid: vec![0.1; 384],
                post_count: 5,
            },
            crate::db::models::ClusterCentroid {
                centroid: vec![0.2; 384],
                post_count: 6,
            },
            crate::db::models::ClusterCentroid {
                centroid: vec![0.3; 384],
                post_count: 7,
            },
        ];
        db.save_fingerprint_bundle(TEST_USER, "{}", 18, None, &three)
            .await
            .unwrap();
        let two = vec![
            crate::db::models::ClusterCentroid {
                centroid: vec![0.9; 384],
                post_count: 9,
            },
            crate::db::models::ClusterCentroid {
                centroid: vec![0.8; 384],
                post_count: 8,
            },
        ];
        db.save_fingerprint_bundle(TEST_USER, "{}", 17, None, &two)
            .await
            .unwrap();
        let stored = db.get_topic_centroids(TEST_USER).await.unwrap();
        assert_eq!(
            stored.len(),
            2,
            "old generation's third row must not survive"
        );
        assert_eq!(stored, two);
    }

    #[tokio::test]
    async fn test_bundle_keyword_only_is_legal() {
        let db = test_db().await;
        db.save_fingerprint_bundle(TEST_USER, "{}", 10, None, &[])
            .await
            .unwrap();
        assert!(db.get_embedding(TEST_USER).await.unwrap().is_none());
        assert!(db.get_topic_centroids(TEST_USER).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_bundle_rejects_non_finite_and_preserves_previous_generation() {
        let db = test_db().await;
        let good = vec![crate::db::models::ClusterCentroid {
            centroid: vec![0.5; 384],
            post_count: 3,
        }];
        db.save_fingerprint_bundle(TEST_USER, "{\"gen\":1}", 3, None, &good)
            .await
            .unwrap();

        let bad = vec![crate::db::models::ClusterCentroid {
            centroid: vec![f64::NAN; 384],
            post_count: 4,
        }];
        assert!(db
            .save_fingerprint_bundle(TEST_USER, "{\"gen\":2}", 4, None, &bad)
            .await
            .is_err());

        // Generation 1 intact, in full: JSON and clusters both from gen 1.
        let (json, count, _) = db.get_fingerprint(TEST_USER).await.unwrap().unwrap();
        assert!(json.contains("gen\":1"));
        assert_eq!(count, 3);
        assert_eq!(db.get_topic_centroids(TEST_USER).await.unwrap(), good);
    }

    #[tokio::test]
    async fn test_keyword_only_bundle_nulls_a_prior_embedding() {
        let db = test_db().await;
        let clusters = vec![crate::db::models::ClusterCentroid {
            centroid: vec![0.5; 384],
            post_count: 3,
        }];
        let emb = vec![0.25; 384];
        db.save_fingerprint_bundle(TEST_USER, "{\"gen\":1}", 3, Some(&emb), &clusters)
            .await
            .unwrap();
        assert!(db.get_embedding(TEST_USER).await.unwrap().is_some());

        // Downgrade to keyword-only: the new generation replaces EVERYTHING —
        // a stale embedding surviving here would mix generations (#302).
        db.save_fingerprint_bundle(TEST_USER, "{\"gen\":2}", 4, None, &[])
            .await
            .unwrap();
        assert!(db.get_embedding(TEST_USER).await.unwrap().is_none());
        assert!(db.get_topic_centroids(TEST_USER).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_delete_user_data_clears_topic_clusters() {
        let db = test_db().await;
        let clusters = vec![crate::db::models::ClusterCentroid {
            centroid: vec![0.5; 384],
            post_count: 3,
        }];
        db.save_fingerprint_bundle(TEST_USER, "{}", 3, None, &clusters)
            .await
            .unwrap();
        db.delete_user_data(TEST_USER).await.unwrap();
        assert!(db.get_topic_centroids(TEST_USER).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_trait_embedding_roundtrip() {
        let db = test_db().await;
        db.save_fingerprint(TEST_USER, r#"{"clusters":[]}"#, 50)
            .await
            .unwrap();
        assert!(db.get_embedding(TEST_USER).await.unwrap().is_none());
        db.save_embedding(TEST_USER, &[0.1, 0.2, 0.3])
            .await
            .unwrap();
        let loaded = db.get_embedding(TEST_USER).await.unwrap().unwrap();
        assert_eq!(loaded.len(), 3);
        assert!((loaded[0] - 0.1).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_trait_account_score_upsert_and_rank() {
        let db = test_db().await;
        let score = AccountScore {
            did: "did:plc:abc".to_string(),
            handle: "test.bsky.social".to_string(),
            toxicity_score: Some(0.8),
            topic_overlap: Some(0.3),
            overlap_legacy: None,
            threat_score: Some(65.0),
            threat_tier: Some("Elevated".to_string()),
            posts_analyzed: 20,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        db.upsert_account_score(TEST_USER, &score).await.unwrap();
        let ranked = db.get_ranked_threats(TEST_USER, 0.0).await.unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].handle, "test.bsky.social");
    }

    #[tokio::test]
    async fn test_trait_count_not_assessed() {
        let db = test_db().await;

        // A NotAssessed account (null score — unsupported language, #222).
        let not_assessed = AccountScore {
            did: "did:plc:notassessed".to_string(),
            handle: "unsupported.bsky.social".to_string(),
            toxicity_score: None,
            topic_overlap: None,
            overlap_legacy: None,
            threat_score: None,
            threat_tier: Some("NotAssessed".to_string()),
            posts_analyzed: 5,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        // A normally-scored account that must NOT be counted.
        let normal = AccountScore {
            did: "did:plc:normal".to_string(),
            handle: "normal.bsky.social".to_string(),
            toxicity_score: Some(0.1),
            topic_overlap: Some(0.1),
            overlap_legacy: None,
            threat_score: Some(5.0),
            threat_tier: Some("Low".to_string()),
            posts_analyzed: 10,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        db.upsert_account_score(TEST_USER, &not_assessed)
            .await
            .unwrap();
        db.upsert_account_score(TEST_USER, &normal).await.unwrap();

        assert_eq!(db.count_not_assessed(TEST_USER).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn test_trait_amplification_event() {
        let db = test_db().await;
        let id = db
            .insert_amplification_event(
                TEST_USER,
                "quote",
                "did:plc:xyz",
                "troll.bsky.social",
                "at://did:plc:me/app.bsky.feed.post/abc",
                Some("at://did:plc:xyz/app.bsky.feed.post/def"),
                Some("lol look at this"),
                None,
                None,
            )
            .await
            .unwrap();
        assert!(id > 0);
        let events = db.get_recent_events(TEST_USER, 10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "quote");
    }

    #[tokio::test]
    async fn test_trait_table_count() {
        let db = test_db().await;
        let count = db.table_count().await.unwrap();
        // schema_version, topic_fingerprint, account_scores, amplification_events,
        // scan_state, users, user_labels, inferred_pairs,
        // classification_queue, scan_account_input, scan_skips,
        // scan_queue, topic_clusters, access_requests,
        // oauth_sessions, action_batches, actions = 17 tables (v15)
        assert_eq!(count, 17);
    }

    #[tokio::test]
    async fn test_migration_v13_creates_topic_clusters_and_overlap_legacy() {
        let db = test_db().await;
        let conn = db.conn.lock().await;
        let has_table: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='topic_clusters'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_table, "topic_clusters table missing");
        let has_col: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('account_scores') WHERE name='overlap_legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_col, "account_scores.overlap_legacy missing");
    }

    #[tokio::test]
    async fn test_trait_median_engagement_empty() {
        let db = test_db().await;
        let median = db.get_median_engagement(TEST_USER).await.unwrap();
        assert!((median - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_trait_is_score_stale_missing() {
        let db = test_db().await;
        assert!(db
            .is_score_stale(TEST_USER, "did:plc:missing", 7)
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_trait_get_account_by_handle() {
        let db = test_db().await;
        let score = AccountScore {
            did: "did:plc:test123".to_string(),
            handle: "test.bsky.social".to_string(),
            toxicity_score: Some(0.5),
            topic_overlap: Some(0.3),
            overlap_legacy: None,
            threat_score: Some(20.0),
            threat_tier: Some("Elevated".to_string()),
            posts_analyzed: 10,
            top_toxic_posts: vec![],
            scored_at: "2024-01-01".to_string(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        db.upsert_account_score(TEST_USER, &score).await.unwrap();
        // Exact match
        let found = db
            .get_account_by_handle(TEST_USER, "test.bsky.social")
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().did, "did:plc:test123");
        // Case insensitive
        let found_upper = db
            .get_account_by_handle(TEST_USER, "TEST.BSKY.SOCIAL")
            .await
            .unwrap();
        assert!(found_upper.is_some());
        // Not found
        let missing = db
            .get_account_by_handle(TEST_USER, "nobody.bsky.social")
            .await
            .unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_trait_get_account_by_did() {
        let db = test_db().await;
        let score = AccountScore {
            did: "did:plc:findme".to_string(),
            handle: "findme.bsky.social".to_string(),
            toxicity_score: Some(0.1),
            topic_overlap: Some(0.2),
            overlap_legacy: None,
            threat_score: Some(5.0),
            threat_tier: Some("Low".to_string()),
            posts_analyzed: 5,
            top_toxic_posts: vec![],
            scored_at: "2024-01-01".to_string(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: None,
        };
        db.upsert_account_score(TEST_USER, &score).await.unwrap();
        let found = db
            .get_account_by_did(TEST_USER, "did:plc:findme")
            .await
            .unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().handle, "findme.bsky.social");
        let missing = db
            .get_account_by_did(TEST_USER, "did:plc:nobody")
            .await
            .unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn test_trait_upsert_user() {
        let db = test_db().await;
        db.upsert_user("did:plc:abc123", "test.bsky.social")
            .await
            .unwrap();
        // Upsert again with different handle should not error
        db.upsert_user("did:plc:abc123", "updated.bsky.social")
            .await
            .unwrap();
    }
}
