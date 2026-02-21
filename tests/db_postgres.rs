//! PostgreSQL integration tests — only run when:
//! 1. Compiled with `--features postgres`
//! 2. `DATABASE_URL` env var points to a live Postgres instance
//!
//! Run with:
//!   DATABASE_URL=postgres://charcoal:charcoal@localhost/charcoal_test \
//!     cargo test --all-targets --features postgres

#![cfg(feature = "postgres")]

use charcoal::db::models::AccountScore;

/// Skip the test if DATABASE_URL is not set or doesn't point to Postgres.
fn database_url() -> Option<String> {
    std::env::var("DATABASE_URL")
        .ok()
        .filter(|u| u.starts_with("postgres://") || u.starts_with("postgresql://"))
}

/// Delete rows written by this test file so tests are idempotent across runs.
///
/// Called at the START of each writing test so leftover state from a previous
/// interrupted run doesn't cause spurious failures.
async fn cleanup_test_data(url: &str) {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;

    let pool = Pool::<Postgres>::connect(url).await.unwrap();

    // Delete test-specific scan_state keys
    sqlx_core::query::query("DELETE FROM scan_state WHERE key = 'test_cursor'")
        .execute(&pool)
        .await
        .unwrap();

    // Delete test-specific account scores
    sqlx_core::query::query("DELETE FROM account_scores WHERE did = 'did:plc:pgtest1'")
        .execute(&pool)
        .await
        .unwrap();

    // Delete test-specific amplification events
    sqlx_core::query::query(
        "DELETE FROM amplification_events WHERE amplifier_did = 'did:plc:pgtest_amp'",
    )
    .execute(&pool)
    .await
    .unwrap();

    // topic_fingerprint has only one row (id = 1); reset to a neutral state
    // so embedding and fingerprint tests don't interfere with each other.
    sqlx_core::query::query("DELETE FROM topic_fingerprint WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_pg_scan_state_roundtrip() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    db.set_scan_state("test_cursor", "abc123").await.unwrap();
    let val = db.get_scan_state("test_cursor").await.unwrap();
    assert_eq!(val, Some("abc123".to_string()));

    // Upsert overwrites
    db.set_scan_state("test_cursor", "def456").await.unwrap();
    let val = db.get_scan_state("test_cursor").await.unwrap();
    assert_eq!(val, Some("def456".to_string()));

    // Clean up
    db.set_scan_state("test_cursor", "").await.unwrap();
}

#[tokio::test]
async fn test_pg_fingerprint_roundtrip() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    db.save_fingerprint(r#"{"topics": ["test"]}"#, 42)
        .await
        .unwrap();
    let (json, count, _) = db.get_fingerprint().await.unwrap().unwrap();
    assert_eq!(json, r#"{"topics": ["test"]}"#);
    assert_eq!(count, 42);
}

#[tokio::test]
async fn test_pg_embedding_roundtrip() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    // Ensure fingerprint row exists
    db.save_fingerprint(r#"{"clusters":[]}"#, 10).await.unwrap();

    let embedding: Vec<f64> = (0..384).map(|i| i as f64 / 384.0).collect();
    db.save_embedding(&embedding).await.unwrap();

    let loaded = db.get_embedding().await.unwrap().unwrap();
    assert_eq!(loaded.len(), 384);
    // f64→f32→f64 round-trip loses some precision
    assert!((loaded[0] - 0.0).abs() < 0.001);
    assert!((loaded[383] - 383.0 / 384.0).abs() < 0.001);
}

#[tokio::test]
async fn test_pg_account_score_upsert_and_rank() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let score = AccountScore {
        did: "did:plc:pgtest1".to_string(),
        handle: "pgtest.bsky.social".to_string(),
        toxicity_score: Some(0.75),
        topic_overlap: Some(0.4),
        threat_score: Some(52.5),
        threat_tier: Some("High".to_string()),
        posts_analyzed: 15,
        top_toxic_posts: vec![],
        scored_at: String::new(),
        behavioral_signals: None,
    };
    db.upsert_account_score(&score).await.unwrap();

    let ranked = db.get_ranked_threats(50.0).await.unwrap();
    assert!(ranked.iter().any(|s| s.did == "did:plc:pgtest1"));
}

#[tokio::test]
async fn test_pg_amplification_event() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let id = db
        .insert_amplification_event(
            "quote",
            "did:plc:pgtest_amp",
            "pgtest_troll.bsky.social",
            "at://did:plc:me/app.bsky.feed.post/pgtest1",
            Some("at://did:plc:pgtest_amp/app.bsky.feed.post/q1"),
            Some("test quote text"),
        )
        .await
        .unwrap();
    assert!(id > 0);

    let events = db.get_recent_events(10).await.unwrap();
    assert!(!events.is_empty());
}

#[tokio::test]
async fn test_pg_table_count() {
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let count = db.table_count().await.unwrap();
    assert!(count >= 5, "Expected at least 5 tables, got {count}");
}

#[tokio::test]
async fn test_pg_is_score_stale_missing() {
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    assert!(db
        .is_score_stale("did:plc:nonexistent_pg", 7)
        .await
        .unwrap());
}

#[tokio::test]
async fn test_pg_median_engagement_empty() {
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    // Should return 0.0 when no behavioral data exists
    let median = db.get_median_engagement().await.unwrap();
    assert!(median >= 0.0);
}
