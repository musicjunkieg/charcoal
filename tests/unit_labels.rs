//! Unit tests for user labels and contextual scoring data models.
//!
//! Tests the new data structures added for Phase 1.75 contextual scoring:
//! - UserLabel: user-provided account classification
//! - InferredPair: topic-matched post pairs for NLI scoring
//! - AccuracyMetrics: predicted vs actual tier comparison
//! - AccountScore.context_score: NLI-derived hostility score
//! - AmplificationEvent new fields: original_post_text, context_score

use charcoal::db::models::{
    AccountScore, AccuracyMetrics, AmplificationEvent, InferredPair, UserLabel,
};
use rusqlite::params;

#[test]
fn user_label_fields_accessible() {
    let label = UserLabel {
        user_did: "did:plc:user1".to_string(),
        target_did: "did:plc:target1".to_string(),
        label: "high".to_string(),
        labeled_at: "2026-03-19T12:00:00Z".to_string(),
        notes: Some("known troll".to_string()),
    };
    assert_eq!(label.label, "high");
    assert_eq!(label.notes, Some("known troll".to_string()));
}

#[test]
fn user_label_notes_optional() {
    let label = UserLabel {
        user_did: "did:plc:user1".to_string(),
        target_did: "did:plc:target1".to_string(),
        label: "safe".to_string(),
        labeled_at: "2026-03-19T12:00:00Z".to_string(),
        notes: None,
    };
    assert!(label.notes.is_none());
}

#[test]
fn inferred_pair_fields_accessible() {
    let pair = InferredPair {
        id: 1,
        user_did: "did:plc:user1".to_string(),
        target_did: "did:plc:target1".to_string(),
        target_post_text: "fatphobia is overblown".to_string(),
        target_post_uri: "at://did:plc:target1/app.bsky.feed.post/abc".to_string(),
        user_post_text: "fatphobia in healthcare is real".to_string(),
        user_post_uri: "at://did:plc:user1/app.bsky.feed.post/xyz".to_string(),
        similarity: 0.82,
        context_score: Some(0.71),
        created_at: "2026-03-19T12:00:00Z".to_string(),
    };
    assert_eq!(pair.similarity, 0.82);
    assert_eq!(pair.context_score, Some(0.71));
}

#[test]
fn accuracy_metrics_computation() {
    let metrics = AccuracyMetrics {
        total_labeled: 50,
        exact_matches: 35,
        overscored: 10,
        underscored: 5,
        accuracy: 0.70,
    };
    assert_eq!(metrics.total_labeled, 50);
    assert!((metrics.accuracy - 0.70).abs() < f64::EPSILON);
}

#[test]
fn account_score_has_context_score() {
    let score = AccountScore {
        did: "did:plc:test".to_string(),
        handle: "test.bsky.social".to_string(),
        toxicity_score: Some(0.5),
        topic_overlap: Some(0.3),
        threat_score: Some(25.0),
        threat_tier: Some("Elevated".to_string()),
        posts_analyzed: 10,
        top_toxic_posts: vec![],
        scored_at: "2026-03-19T12:00:00Z".to_string(),
        behavioral_signals: None,
        context_score: Some(0.65),
    };
    assert_eq!(score.context_score, Some(0.65));
}

#[test]
fn account_score_context_score_defaults_none() {
    let score = AccountScore {
        did: "did:plc:test".to_string(),
        handle: "test.bsky.social".to_string(),
        toxicity_score: None,
        topic_overlap: None,
        threat_score: None,
        threat_tier: None,
        posts_analyzed: 0,
        top_toxic_posts: vec![],
        scored_at: "2026-03-19T12:00:00Z".to_string(),
        behavioral_signals: None,
        context_score: None,
    };
    assert!(score.context_score.is_none());
}

#[test]
fn amplification_event_has_new_fields() {
    let event = AmplificationEvent {
        id: 1,
        event_type: "quote".to_string(),
        amplifier_did: "did:plc:amp".to_string(),
        amplifier_handle: "amp.bsky.social".to_string(),
        original_post_uri: "at://did:plc:user/app.bsky.feed.post/abc".to_string(),
        amplifier_post_uri: Some("at://did:plc:amp/app.bsky.feed.post/def".to_string()),
        amplifier_text: Some("look at this idiot".to_string()),
        detected_at: "2026-03-19T12:00:00Z".to_string(),
        followers_fetched: false,
        followers_scored: false,
        original_post_text: Some("fatphobia in healthcare is real".to_string()),
        context_score: Some(0.85),
    };
    assert_eq!(
        event.original_post_text,
        Some("fatphobia in healthcare is real".to_string())
    );
    assert_eq!(event.context_score, Some(0.85));
}

#[test]
fn amplification_event_new_fields_optional() {
    let event = AmplificationEvent {
        id: 2,
        event_type: "repost".to_string(),
        amplifier_did: "did:plc:amp".to_string(),
        amplifier_handle: "amp.bsky.social".to_string(),
        original_post_uri: "at://did:plc:user/app.bsky.feed.post/abc".to_string(),
        amplifier_post_uri: None,
        amplifier_text: None,
        detected_at: "2026-03-19T12:00:00Z".to_string(),
        followers_fetched: false,
        followers_scored: false,
        original_post_text: None,
        context_score: None,
    };
    assert!(event.original_post_text.is_none());
    assert!(event.context_score.is_none());
}

// --- Schema v5 migration tests ---

fn setup_migrated_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    conn
}

#[test]
fn schema_v5_creates_user_labels_table() {
    let conn = setup_migrated_db();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='user_labels'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "user_labels table should exist after v5 migration"
    );
}

#[test]
fn schema_v5_creates_inferred_pairs_table() {
    let conn = setup_migrated_db();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='inferred_pairs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "inferred_pairs table should exist after v5 migration"
    );
}

#[test]
fn schema_v5_adds_context_score_to_account_scores() {
    let conn = setup_migrated_db();
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, context_score) VALUES (?1, ?2, ?3, ?4)",
        params!["did:plc:user1", "did:plc:test", "test.bsky.social", 0.75],
    )
    .unwrap();

    let score: f64 = conn
        .query_row(
            "SELECT context_score FROM account_scores WHERE did = ?1 AND user_did = ?2",
            params!["did:plc:test", "did:plc:user1"],
            |row| row.get(0),
        )
        .unwrap();
    assert!((score - 0.75).abs() < f64::EPSILON);
}

#[test]
fn schema_v5_adds_columns_to_amplification_events() {
    let conn = setup_migrated_db();
    conn.execute(
        "INSERT INTO amplification_events (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri, original_post_text, context_score)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            "did:plc:user1",
            "quote",
            "did:plc:amp",
            "amp.bsky.social",
            "at://did:plc:user1/app.bsky.feed.post/abc",
            "my original post text",
            0.85
        ],
    )
    .unwrap();

    let (text, score): (Option<String>, Option<f64>) = conn
        .query_row(
            "SELECT original_post_text, context_score FROM amplification_events WHERE amplifier_did = ?1",
            params!["did:plc:amp"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(text, Some("my original post text".to_string()));
    assert!((score.unwrap() - 0.85).abs() < f64::EPSILON);
}

#[test]
fn schema_v5_user_labels_upsert_on_conflict() {
    let conn = setup_migrated_db();

    // Insert first label
    conn.execute(
        "INSERT INTO user_labels (user_did, target_did, label, labeled_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            "did:plc:user1",
            "did:plc:target1",
            "high",
            "2026-03-19T12:00:00Z"
        ],
    )
    .unwrap();

    // Upsert with different label
    conn.execute(
        "INSERT INTO user_labels (user_did, target_did, label, labeled_at, notes)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(user_did, target_did) DO UPDATE SET label=excluded.label, labeled_at=excluded.labeled_at, notes=excluded.notes",
        params!["did:plc:user1", "did:plc:target1", "safe", "2026-03-19T13:00:00Z", "actually a friend"],
    )
    .unwrap();

    let (label, notes): (String, Option<String>) = conn
        .query_row(
            "SELECT label, notes FROM user_labels WHERE user_did = ?1 AND target_did = ?2",
            params!["did:plc:user1", "did:plc:target1"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(label, "safe");
    assert_eq!(notes, Some("actually a friend".to_string()));
}

#[test]
fn schema_v5_inferred_pairs_dedup_index() {
    let conn = setup_migrated_db();

    conn.execute(
        "INSERT INTO inferred_pairs (user_did, target_did, target_post_text, target_post_uri, user_post_text, user_post_uri, similarity, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params!["did:plc:u", "did:plc:t", "post1", "at://t/p/1", "upost", "at://u/p/1", 0.8, "2026-03-19"],
    )
    .unwrap();

    // Duplicate should fail due to unique index
    let result = conn.execute(
        "INSERT INTO inferred_pairs (user_did, target_did, target_post_text, target_post_uri, user_post_text, user_post_uri, similarity, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params!["did:plc:u", "did:plc:t", "post1", "at://t/p/1", "upost", "at://u/p/1", 0.9, "2026-03-20"],
    );
    assert!(
        result.is_err(),
        "Duplicate inferred pair should be rejected by unique index"
    );
}

// --- Trait-level tests using SqliteDatabase ---

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use rusqlite::Connection;

async fn test_db() -> SqliteDatabase {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    SqliteDatabase::new(conn)
}

const TEST_USER: &str = "did:plc:testuser000000000000";

fn make_score(did: &str, handle: &str, threat_score: f64, tier: &str) -> AccountScore {
    AccountScore {
        did: did.to_string(),
        handle: handle.to_string(),
        toxicity_score: Some(0.5),
        topic_overlap: Some(0.3),
        threat_score: Some(threat_score),
        threat_tier: Some(tier.to_string()),
        posts_analyzed: 10,
        top_toxic_posts: vec![],
        scored_at: String::new(),
        behavioral_signals: None,
        context_score: None,
    }
}

#[tokio::test]
async fn trait_upsert_and_get_user_label() {
    let db = test_db().await;

    // No label initially
    let label = db
        .get_user_label(TEST_USER, "did:plc:target1")
        .await
        .unwrap();
    assert!(label.is_none());

    // Insert label
    db.upsert_user_label(TEST_USER, "did:plc:target1", "high", Some("known troll"))
        .await
        .unwrap();

    let label = db
        .get_user_label(TEST_USER, "did:plc:target1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(label.label, "high");
    assert_eq!(label.notes, Some("known troll".to_string()));
    assert_eq!(label.user_did, TEST_USER);
    assert_eq!(label.target_did, "did:plc:target1");

    // Upsert overwrites
    db.upsert_user_label(TEST_USER, "did:plc:target1", "safe", None)
        .await
        .unwrap();

    let label = db
        .get_user_label(TEST_USER, "did:plc:target1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(label.label, "safe");
    assert!(label.notes.is_none());
}

#[tokio::test]
async fn trait_user_label_isolation() {
    let db = test_db().await;

    db.upsert_user_label("did:plc:user_a", "did:plc:target1", "high", None)
        .await
        .unwrap();
    db.upsert_user_label("did:plc:user_b", "did:plc:target1", "safe", None)
        .await
        .unwrap();

    let label_a = db
        .get_user_label("did:plc:user_a", "did:plc:target1")
        .await
        .unwrap()
        .unwrap();
    let label_b = db
        .get_user_label("did:plc:user_b", "did:plc:target1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(label_a.label, "high");
    assert_eq!(label_b.label, "safe");
}

#[tokio::test]
async fn trait_get_unlabeled_accounts() {
    let db = test_db().await;

    // Insert 3 scored accounts
    db.upsert_account_score(
        TEST_USER,
        &make_score("did:plc:a", "a.bsky.social", 40.0, "High"),
    )
    .await
    .unwrap();
    db.upsert_account_score(
        TEST_USER,
        &make_score("did:plc:b", "b.bsky.social", 20.0, "Elevated"),
    )
    .await
    .unwrap();
    db.upsert_account_score(
        TEST_USER,
        &make_score("did:plc:c", "c.bsky.social", 5.0, "Low"),
    )
    .await
    .unwrap();

    // All 3 should be unlabeled
    let unlabeled = db.get_unlabeled_accounts(TEST_USER, 10).await.unwrap();
    assert_eq!(unlabeled.len(), 3);
    // Should be sorted by threat_score DESC
    assert_eq!(unlabeled[0].did, "did:plc:a");
    assert_eq!(unlabeled[1].did, "did:plc:b");
    assert_eq!(unlabeled[2].did, "did:plc:c");

    // Label one account
    db.upsert_user_label(TEST_USER, "did:plc:a", "high", None)
        .await
        .unwrap();

    let unlabeled = db.get_unlabeled_accounts(TEST_USER, 10).await.unwrap();
    assert_eq!(unlabeled.len(), 2);
    assert_eq!(unlabeled[0].did, "did:plc:b");

    // Limit works
    let unlabeled = db.get_unlabeled_accounts(TEST_USER, 1).await.unwrap();
    assert_eq!(unlabeled.len(), 1);
}

#[tokio::test]
async fn trait_get_accuracy_metrics_empty() {
    let db = test_db().await;

    let metrics = db.get_accuracy_metrics(TEST_USER).await.unwrap();
    assert_eq!(metrics.total_labeled, 0);
    assert_eq!(metrics.exact_matches, 0);
    assert!((metrics.accuracy - 0.0).abs() < f64::EPSILON);
}

#[tokio::test]
async fn trait_get_accuracy_metrics_mixed() {
    let db = test_db().await;

    // Insert scored accounts with various tiers
    db.upsert_account_score(
        TEST_USER,
        &make_score("did:plc:a", "a.bsky.social", 40.0, "High"),
    )
    .await
    .unwrap();
    db.upsert_account_score(
        TEST_USER,
        &make_score("did:plc:b", "b.bsky.social", 20.0, "Elevated"),
    )
    .await
    .unwrap();
    db.upsert_account_score(
        TEST_USER,
        &make_score("did:plc:c", "c.bsky.social", 5.0, "Low"),
    )
    .await
    .unwrap();
    db.upsert_account_score(
        TEST_USER,
        &make_score("did:plc:d", "d.bsky.social", 10.0, "Watch"),
    )
    .await
    .unwrap();

    // Label them: a=high (exact), b=safe (overscored), c=elevated (underscored), d=watch (exact)
    db.upsert_user_label(TEST_USER, "did:plc:a", "high", None)
        .await
        .unwrap();
    db.upsert_user_label(TEST_USER, "did:plc:b", "safe", None)
        .await
        .unwrap();
    db.upsert_user_label(TEST_USER, "did:plc:c", "elevated", None)
        .await
        .unwrap();
    db.upsert_user_label(TEST_USER, "did:plc:d", "watch", None)
        .await
        .unwrap();

    let metrics = db.get_accuracy_metrics(TEST_USER).await.unwrap();
    assert_eq!(metrics.total_labeled, 4);
    assert_eq!(metrics.exact_matches, 2); // a=high, d=watch
    assert_eq!(metrics.overscored, 1); // b: predicted Elevated, labeled safe
    assert_eq!(metrics.underscored, 1); // c: predicted Low, labeled elevated
    assert!((metrics.accuracy - 0.5).abs() < f64::EPSILON);
}

#[tokio::test]
async fn trait_inferred_pairs_crud() {
    let db = test_db().await;

    // No pairs initially
    let pairs = db
        .get_inferred_pairs(TEST_USER, "did:plc:target1")
        .await
        .unwrap();
    assert!(pairs.is_empty());

    // Insert a pair
    let id = db
        .insert_inferred_pair(
            TEST_USER,
            "did:plc:target1",
            "fatphobia is overblown",
            "at://did:plc:target1/app.bsky.feed.post/abc",
            "fatphobia in healthcare is real",
            "at://did:plc:user/app.bsky.feed.post/xyz",
            0.82,
            Some(0.71),
        )
        .await
        .unwrap();
    assert!(id > 0);

    // Retrieve it
    let pairs = db
        .get_inferred_pairs(TEST_USER, "did:plc:target1")
        .await
        .unwrap();
    assert_eq!(pairs.len(), 1);
    assert_eq!(pairs[0].target_post_text, "fatphobia is overblown");
    assert_eq!(pairs[0].user_post_text, "fatphobia in healthcare is real");
    assert!((pairs[0].similarity - 0.82).abs() < f64::EPSILON);
    assert_eq!(pairs[0].context_score, Some(0.71));

    // Insert a second pair
    db.insert_inferred_pair(
        TEST_USER,
        "did:plc:target1",
        "another post",
        "at://did:plc:target1/app.bsky.feed.post/def",
        "my other post",
        "at://did:plc:user/app.bsky.feed.post/ghi",
        0.65,
        None,
    )
    .await
    .unwrap();

    let pairs = db
        .get_inferred_pairs(TEST_USER, "did:plc:target1")
        .await
        .unwrap();
    assert_eq!(pairs.len(), 2);
    // Sorted by similarity DESC
    assert!((pairs[0].similarity - 0.82).abs() < f64::EPSILON);
    assert!((pairs[1].similarity - 0.65).abs() < f64::EPSILON);

    // Delete all pairs for this target
    db.delete_inferred_pairs(TEST_USER, "did:plc:target1")
        .await
        .unwrap();
    let pairs = db
        .get_inferred_pairs(TEST_USER, "did:plc:target1")
        .await
        .unwrap();
    assert!(pairs.is_empty());
}

#[tokio::test]
async fn trait_inferred_pairs_dedup_upsert() {
    let db = test_db().await;

    // Insert a pair
    db.insert_inferred_pair(
        TEST_USER,
        "did:plc:target1",
        "post text",
        "at://did:plc:target1/app.bsky.feed.post/abc",
        "user post text",
        "at://did:plc:user/app.bsky.feed.post/xyz",
        0.80,
        None,
    )
    .await
    .unwrap();

    // Insert same URIs — should upsert, not duplicate
    db.insert_inferred_pair(
        TEST_USER,
        "did:plc:target1",
        "post text",
        "at://did:plc:target1/app.bsky.feed.post/abc",
        "user post text",
        "at://did:plc:user/app.bsky.feed.post/xyz",
        0.90,
        Some(0.55),
    )
    .await
    .unwrap();

    let pairs = db
        .get_inferred_pairs(TEST_USER, "did:plc:target1")
        .await
        .unwrap();
    assert_eq!(pairs.len(), 1);
    // Should have the updated values
    assert!((pairs[0].similarity - 0.90).abs() < f64::EPSILON);
    assert_eq!(pairs[0].context_score, Some(0.55));
}

#[tokio::test]
async fn trait_insert_amplification_event_with_new_fields() {
    let db = test_db().await;

    let id = db
        .insert_amplification_event(
            TEST_USER,
            "quote",
            "did:plc:amp",
            "amp.bsky.social",
            "at://did:plc:me/app.bsky.feed.post/abc",
            Some("at://did:plc:amp/app.bsky.feed.post/def"),
            Some("look at this"),
            Some("my original post"),
            Some(0.85),
        )
        .await
        .unwrap();
    assert!(id > 0);

    let events = db.get_recent_events(TEST_USER, 10).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].original_post_text,
        Some("my original post".to_string())
    );
    assert_eq!(events[0].context_score, Some(0.85));
}

#[tokio::test]
async fn trait_insert_amplification_event_new_fields_none() {
    let db = test_db().await;

    db.insert_amplification_event(
        TEST_USER,
        "repost",
        "did:plc:amp",
        "amp.bsky.social",
        "at://did:plc:me/app.bsky.feed.post/abc",
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let events = db.get_recent_events(TEST_USER, 10).await.unwrap();
    assert_eq!(events.len(), 1);
    assert!(events[0].original_post_text.is_none());
    assert!(events[0].context_score.is_none());
}
