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

use super::models::{AccountScore, AmplificationEvent};
use super::traits::Database;

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

    async fn get_scan_state(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        super::queries::get_scan_state(&conn, key)
    }

    async fn set_scan_state(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::set_scan_state(&conn, key, value)
    }

    async fn get_all_scan_state(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock().await;
        super::queries::get_all_scan_state(&conn)
    }

    async fn save_fingerprint(&self, fingerprint_json: &str, post_count: u32) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::save_fingerprint(&conn, fingerprint_json, post_count)
    }

    async fn save_embedding(&self, embedding: &[f64]) -> Result<()> {
        let json = serde_json::to_string(embedding)?;
        let conn = self.conn.lock().await;
        super::queries::save_embedding(&conn, &json)
    }

    async fn get_fingerprint(&self) -> Result<Option<(String, u32, String)>> {
        let conn = self.conn.lock().await;
        super::queries::get_fingerprint(&conn)
    }

    async fn get_embedding(&self) -> Result<Option<Vec<f64>>> {
        let conn = self.conn.lock().await;
        super::queries::get_embedding(&conn)
    }

    async fn upsert_account_score(&self, score: &AccountScore) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_account_score(&conn, score)
    }

    async fn get_ranked_threats(&self, min_score: f64) -> Result<Vec<AccountScore>> {
        let conn = self.conn.lock().await;
        super::queries::get_ranked_threats(&conn, min_score)
    }

    async fn is_score_stale(&self, did: &str, max_age_days: i64) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::is_score_stale(&conn, did, max_age_days)
    }

    async fn insert_amplification_event(
        &self,
        event_type: &str,
        amplifier_did: &str,
        amplifier_handle: &str,
        original_post_uri: &str,
        amplifier_post_uri: Option<&str>,
        amplifier_text: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::insert_amplification_event(
            &conn,
            event_type,
            amplifier_did,
            amplifier_handle,
            original_post_uri,
            amplifier_post_uri,
            amplifier_text,
        )
    }

    async fn get_recent_events(&self, limit: u32) -> Result<Vec<AmplificationEvent>> {
        let conn = self.conn.lock().await;
        super::queries::get_recent_events(&conn, limit)
    }

    async fn get_events_for_pile_on(&self) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().await;
        super::queries::get_events_for_pile_on(&conn)
    }

    async fn get_median_engagement(&self) -> Result<f64> {
        let conn = self.conn.lock().await;
        super::queries::get_median_engagement(&conn)
    }

    async fn insert_amplification_event_raw(
        &self,
        event: &super::models::AmplificationEvent,
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::insert_amplification_event_with_detected_at(&conn, event)
    }

    async fn get_account_by_handle(&self, handle: &str) -> Result<Option<AccountScore>> {
        let conn = self.conn.lock().await;
        super::queries::get_account_by_handle(&conn, handle)
    }

    async fn get_account_by_did(&self, did: &str) -> Result<Option<AccountScore>> {
        let conn = self.conn.lock().await;
        super::queries::get_account_by_did(&conn, did)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::create_tables;

    async fn test_db() -> SqliteDatabase {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        SqliteDatabase::new(conn)
    }

    #[tokio::test]
    async fn test_trait_scan_state_roundtrip() {
        let db = test_db().await;
        assert_eq!(db.get_scan_state("cursor").await.unwrap(), None);
        db.set_scan_state("cursor", "abc123").await.unwrap();
        assert_eq!(
            db.get_scan_state("cursor").await.unwrap(),
            Some("abc123".to_string())
        );
    }

    #[tokio::test]
    async fn test_trait_fingerprint_roundtrip() {
        let db = test_db().await;
        assert!(db.get_fingerprint().await.unwrap().is_none());
        db.save_fingerprint(r#"{"topics": []}"#, 100).await.unwrap();
        let (json, count, _) = db.get_fingerprint().await.unwrap().unwrap();
        assert_eq!(json, r#"{"topics": []}"#);
        assert_eq!(count, 100);
    }

    #[tokio::test]
    async fn test_trait_embedding_roundtrip() {
        let db = test_db().await;
        db.save_fingerprint(r#"{"clusters":[]}"#, 50).await.unwrap();
        assert!(db.get_embedding().await.unwrap().is_none());
        db.save_embedding(&[0.1, 0.2, 0.3]).await.unwrap();
        let loaded = db.get_embedding().await.unwrap().unwrap();
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
            threat_score: Some(65.0),
            threat_tier: Some("Elevated".to_string()),
            posts_analyzed: 20,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
        };
        db.upsert_account_score(&score).await.unwrap();
        let ranked = db.get_ranked_threats(0.0).await.unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].handle, "test.bsky.social");
    }

    #[tokio::test]
    async fn test_trait_amplification_event() {
        let db = test_db().await;
        let id = db
            .insert_amplification_event(
                "quote",
                "did:plc:xyz",
                "troll.bsky.social",
                "at://did:plc:me/app.bsky.feed.post/abc",
                Some("at://did:plc:xyz/app.bsky.feed.post/def"),
                Some("lol look at this"),
            )
            .await
            .unwrap();
        assert!(id > 0);
        let events = db.get_recent_events(10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "quote");
    }

    #[tokio::test]
    async fn test_trait_table_count() {
        let db = test_db().await;
        let count = db.table_count().await.unwrap();
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn test_trait_median_engagement_empty() {
        let db = test_db().await;
        let median = db.get_median_engagement().await.unwrap();
        assert!((median - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_trait_is_score_stale_missing() {
        let db = test_db().await;
        assert!(db.is_score_stale("did:plc:missing", 7).await.unwrap());
    }

    #[tokio::test]
    async fn test_trait_get_account_by_handle() {
        let db = test_db().await;
        // Insert a score
        let score = AccountScore {
            did: "did:plc:test123".to_string(),
            handle: "test.bsky.social".to_string(),
            toxicity_score: Some(0.5),
            topic_overlap: Some(0.3),
            threat_score: Some(20.0),
            threat_tier: Some("Elevated".to_string()),
            posts_analyzed: 10,
            top_toxic_posts: vec![],
            scored_at: "2024-01-01".to_string(),
            behavioral_signals: None,
        };
        db.upsert_account_score(&score).await.unwrap();
        // Exact match
        let found = db.get_account_by_handle("test.bsky.social").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().did, "did:plc:test123");
        // Case insensitive
        let found_upper = db.get_account_by_handle("TEST.BSKY.SOCIAL").await.unwrap();
        assert!(found_upper.is_some());
        // Not found
        let missing = db
            .get_account_by_handle("nobody.bsky.social")
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
            threat_score: Some(5.0),
            threat_tier: Some("Low".to_string()),
            posts_analyzed: 5,
            top_toxic_posts: vec![],
            scored_at: "2024-01-01".to_string(),
            behavioral_signals: None,
        };
        db.upsert_account_score(&score).await.unwrap();
        let found = db.get_account_by_did("did:plc:findme").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().handle, "findme.bsky.social");
        let missing = db.get_account_by_did("did:plc:nobody").await.unwrap();
        assert!(missing.is_none());
    }
}
