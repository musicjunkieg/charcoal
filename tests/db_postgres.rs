//! PostgreSQL integration tests — only run when:
//! 1. Compiled with `--features postgres`
//! 2. `DATABASE_URL` env var points to a live Postgres instance
//!
//! Run with:
//!   DATABASE_URL=postgres://charcoal:charcoal@localhost/charcoal_test \
//!     cargo test --all-targets --features postgres

#![cfg(feature = "postgres")]

use anyhow::Result;
use charcoal::db::models::AccountScore;
use charcoal::pipeline::scan_phases::staging::{QueueRow, VerdictRow};

const TEST_USER: &str = "did:plc:pgtest_user000000000000";

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
async fn cleanup_test_data(url: &str) -> Result<()> {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;

    let pool = Pool::<Postgres>::connect(url)
        .await
        .map_err(|e| anyhow::anyhow!("cleanup: failed to connect: {e}"))?;

    // NOTE: the `test_cursor` scan_state key is deliberately NOT cleaned here.
    // Six tests call this helper concurrently, but only
    // `test_pg_scan_state_roundtrip` writes that key — so deleting it from the
    // shared helper meant any of the other five could wipe the row out from
    // under that test between its write and its read (observed as
    // `left: None, right: Some("def456")`). It cleans up its own key instead.

    // Delete test-specific account scores (scoped by user_did)
    sqlx_core::query::query(
        "DELETE FROM account_scores WHERE did = 'did:plc:pgtest1' AND user_did = 'did:plc:pgtest_user000000000000'",
    )
    .execute(&pool)
    .await
    .map_err(|e| anyhow::anyhow!("cleanup: account_scores delete failed: {e}"))?;

    // Delete test-specific amplification events
    sqlx_core::query::query(
        "DELETE FROM amplification_events WHERE user_did = 'did:plc:pgtest_user000000000000' AND amplifier_did = 'did:plc:pgtest_amp'",
    )
    .execute(&pool)
    .await
    .map_err(|e| anyhow::anyhow!("cleanup: amplification_events delete failed: {e}"))?;

    // Delete test-specific topic fingerprint (scoped by user_did)
    sqlx_core::query::query(
        "DELETE FROM topic_fingerprint WHERE user_did = 'did:plc:pgtest_user000000000000'",
    )
    .execute(&pool)
    .await
    .map_err(|e| anyhow::anyhow!("cleanup: topic_fingerprint delete failed: {e}"))?;

    Ok(())
}

#[tokio::test]
async fn test_pg_scan_state_roundtrip() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await.unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    db.set_scan_state(TEST_USER, "test_cursor", "abc123")
        .await
        .unwrap();
    let val = db.get_scan_state(TEST_USER, "test_cursor").await.unwrap();
    assert_eq!(val, Some("abc123".to_string()));

    // Upsert overwrites
    db.set_scan_state(TEST_USER, "test_cursor", "def456")
        .await
        .unwrap();
    let val = db.get_scan_state(TEST_USER, "test_cursor").await.unwrap();
    assert_eq!(val, Some("def456".to_string()));

    // Clean up
    db.set_scan_state(TEST_USER, "test_cursor", "")
        .await
        .unwrap();
}

#[tokio::test]
async fn test_pg_fingerprint_roundtrip() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await.unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    db.save_fingerprint(TEST_USER, r#"{"topics": ["test"]}"#, 42)
        .await
        .unwrap();
    let (json, count, _) = db.get_fingerprint(TEST_USER).await.unwrap().unwrap();
    assert_eq!(json, r#"{"topics": ["test"]}"#);
    assert_eq!(count, 42);
}

#[tokio::test]
async fn test_pg_embedding_roundtrip() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await.unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    // Ensure fingerprint row exists
    db.save_fingerprint(TEST_USER, r#"{"clusters":[]}"#, 10)
        .await
        .unwrap();

    let embedding: Vec<f64> = (0..384).map(|i| i as f64 / 384.0).collect();
    db.save_embedding(TEST_USER, &embedding).await.unwrap();

    let loaded = db.get_embedding(TEST_USER).await.unwrap().unwrap();
    assert_eq!(loaded.len(), 384);
    // f64→f32→f64 round-trip loses some precision
    assert!((loaded[0] - 0.0).abs() < 0.001);
    assert!((loaded[383] - 383.0 / 384.0).abs() < 0.001);
}

/// #302: `save_fingerprint_bundle` writes the fingerprint row, the mean
/// embedding, and per-topic centroid rows in one transaction; a later
/// keyword-only generation (`embedding: None`) must NULL out a prior
/// embedding and drop the previous generation's extra cluster rows — the
/// bundle IS the generation, not an incremental patch.
#[tokio::test]
async fn test_pg_bundle_roundtrip_and_replacement() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await.unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let clusters = vec![
        charcoal::db::models::ClusterCentroid {
            centroid: vec![0.5; 384],
            post_count: 30,
        },
        charcoal::db::models::ClusterCentroid {
            centroid: vec![-0.25; 384],
            post_count: 12,
        },
    ];
    let emb = vec![0.125; 384];
    db.save_fingerprint_bundle(TEST_USER, "{}", 42, Some(&emb), &clusters)
        .await
        .unwrap();

    let stored = db.get_topic_centroids(TEST_USER).await.unwrap();
    assert_eq!(stored.len(), 2);
    // pgvector stores f32 — compare with tolerance, same as the
    // mean-embedding tests.
    for (s, c) in stored.iter().zip(clusters.iter()) {
        assert_eq!(s.post_count, c.post_count);
        for (a, b) in s.centroid.iter().zip(c.centroid.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    // Replacement drops the old generation's extra rows.
    let one = vec![charcoal::db::models::ClusterCentroid {
        centroid: vec![0.9; 384],
        post_count: 9,
    }];
    db.save_fingerprint_bundle(TEST_USER, "{}", 9, None, &one)
        .await
        .unwrap();
    let stored = db.get_topic_centroids(TEST_USER).await.unwrap();
    assert_eq!(stored.len(), 1);
    // Count alone would also pass if the delete ran but the insert did not —
    // assert the survivor is the NEW generation's row, not an old one.
    assert_eq!(stored[0].post_count, 9);
    assert!(
        stored[0].centroid.iter().all(|v| (v - 0.9).abs() < 1e-6),
        "the surviving row must be the new generation's centroid"
    );
    // None embedding leaves the column NULL for this generation.
    assert!(db.get_embedding(TEST_USER).await.unwrap().is_none());

    cleanup_test_data(&url).await.unwrap();
}

/// Deleting a user must cascade to `topic_clusters` (FK ON DELETE CASCADE,
/// migration 0013). Uses its own DID, not the shared `TEST_USER` — this test
/// calls `delete_user_data`, and these tests run concurrently against one
/// database, so deleting the shared user would pull data out from under a
/// neighbouring test.
#[tokio::test]
async fn test_pg_delete_user_cascades_topic_clusters() {
    const DEL_USER: &str = "did:plc:pgtest_bundle_del00000";

    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    // Start from a known state without touching the shared fixtures.
    db.delete_user_data(DEL_USER).await.unwrap();

    let clusters = vec![charcoal::db::models::ClusterCentroid {
        centroid: vec![0.5; 384],
        post_count: 3,
    }];
    db.save_fingerprint_bundle(DEL_USER, "{}", 3, None, &clusters)
        .await
        .unwrap();
    assert_eq!(
        db.get_topic_centroids(DEL_USER).await.unwrap().len(),
        1,
        "precondition: the cluster row must exist, or this test cannot fail"
    );

    db.delete_user_data(DEL_USER).await.unwrap();

    assert!(
        db.get_topic_centroids(DEL_USER).await.unwrap().is_empty(),
        "topic_clusters must not survive account deletion on Postgres"
    );
}

#[tokio::test]
async fn test_pg_account_score_upsert_and_rank() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await.unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let score = AccountScore {
        did: "did:plc:pgtest1".to_string(),
        handle: "pgtest.bsky.social".to_string(),
        toxicity_score: Some(0.75),
        topic_overlap: Some(0.4),
        overlap_legacy: None,
        threat_score: Some(52.5),
        threat_tier: Some("High".to_string()),
        posts_analyzed: 15,
        top_toxic_posts: vec![],
        scored_at: String::new(),
        behavioral_signals: None,
        context_score: None,
        graph_distance: None,
        fingerprint_quality: None,
        scoring_confidence: None,
    };
    db.upsert_account_score(TEST_USER, &score).await.unwrap();

    let ranked = db.get_ranked_threats(TEST_USER, 50.0).await.unwrap();
    assert!(ranked.iter().any(|s| s.did == "did:plc:pgtest1"));
}

/// Delete rows written by a batch-insert test (#216), scoped to the single
/// `original_post_uri` marker the caller passes.
///
/// Each batch test MUST use its own distinct marker and pass only that
/// marker here. These tests run concurrently (cargo test's default
/// threading) and share TEST_USER, so a cleanup that touched more than one
/// test's marker could delete rows a *different*, concurrently-running test
/// had already inserted — the cleanup would protect against stale data from
/// a previous run while introducing a live race against the current one.
async fn cleanup_batch_test_data(url: &str, original_post_uri: &str) -> Result<()> {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;

    let pool = Pool::<Postgres>::connect(url)
        .await
        .map_err(|e| anyhow::anyhow!("cleanup: failed to connect: {e}"))?;

    sqlx_core::query::query(
        "DELETE FROM amplification_events WHERE user_did = $1 AND original_post_uri = $2",
    )
    .bind(TEST_USER)
    .bind(original_post_uri)
    .execute(&pool)
    .await
    .map_err(|e| anyhow::anyhow!("cleanup: amplification_events (batch) delete failed: {e}"))?;

    Ok(())
}

#[tokio::test]
async fn test_pg_batch_insert_matches_serial() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_batch_test_data(&url, "at://did:plc:me/app.bsky.feed.post/b1")
        .await
        .unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let events = vec![
        charcoal::db::models::NewAmplificationEvent {
            event_type: "quote".to_string(),
            amplifier_did: "did:plc:pgbatch1".to_string(),
            amplifier_handle: "pgbatch1.bsky.social".to_string(),
            original_post_uri: "at://did:plc:me/app.bsky.feed.post/b1".to_string(),
            amplifier_post_uri: Some("at://did:plc:pgbatch1/app.bsky.feed.post/q1".to_string()),
            amplifier_text: Some("batched quote".to_string()),
            original_post_text: Some("the original".to_string()),
            context_score: Some(0.42),
        },
        charcoal::db::models::NewAmplificationEvent {
            event_type: "repost".to_string(),
            amplifier_did: "did:plc:pgbatch2".to_string(),
            amplifier_handle: "pgbatch2.bsky.social".to_string(),
            original_post_uri: "at://did:plc:me/app.bsky.feed.post/b1".to_string(),
            amplifier_post_uri: None,
            amplifier_text: None,
            original_post_text: None,
            context_score: None,
        },
    ];

    let n = db
        .insert_amplification_events_batch(TEST_USER, &events)
        .await
        .unwrap();
    assert_eq!(n, 2);

    // Filter to this test's marker post URI rather than trusting the raw
    // top-10: other batch-insert tests in this file share TEST_USER and run
    // concurrently (cargo test's default threading), so get_recent_events's
    // global DESC ordering can otherwise surface unrelated rows here.
    let stored: Vec<_> = db
        .get_recent_events(TEST_USER, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.original_post_uri == "at://did:plc:me/app.bsky.feed.post/b1")
        .collect();
    assert_eq!(stored.len(), 2);

    let first = stored
        .iter()
        .find(|e| e.amplifier_handle == "pgbatch1.bsky.social")
        .expect("first event missing");
    let second = stored
        .iter()
        .find(|e| e.amplifier_handle == "pgbatch2.bsky.social")
        .expect("second event missing");

    assert!(first.id < second.id, "ids must ascend in input order");
    assert_eq!(first.amplifier_text, Some("batched quote".to_string()));
    assert_eq!(first.original_post_text, Some("the original".to_string()));
    assert_eq!(first.context_score, Some(0.42));
    assert_eq!(
        first.amplifier_post_uri,
        Some("at://did:plc:pgbatch1/app.bsky.feed.post/q1".to_string())
    );
    assert_eq!(second.amplifier_text, None);
    assert_eq!(second.context_score, None);
    assert_eq!(second.amplifier_post_uri, None);
}

#[tokio::test]
async fn test_pg_batch_insert_empty_slice_is_noop() {
    let Some(url) = database_url() else {
        return;
    };
    const MARKER: &str = "at://did:plc:me/app.bsky.feed.post/pgemptybatch";
    cleanup_batch_test_data(&url, MARKER).await.unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    // Seed one real row under this test's marker first. Without a seed, "no
    // row with this marker" is true both before and after an empty-slice
    // call, so it can't distinguish "wrote nothing" from "wrote something
    // wrong" — the count has to move for a spurious insert to be visible.
    db.insert_amplification_event(
        TEST_USER,
        "repost",
        "did:plc:pgemptybatch_seed",
        "pgemptybatch_seed.bsky.social",
        MARKER,
        None,
        None,
        None,
        None,
    )
    .await
    .unwrap();

    let count_before = db
        .get_recent_events(TEST_USER, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.original_post_uri == MARKER)
        .count();
    assert_eq!(count_before, 1, "seed row must be visible before the call");

    let n = db
        .insert_amplification_events_batch(TEST_USER, &[])
        .await
        .unwrap();
    assert_eq!(n, 0);

    // An empty-slice call must write NOTHING: the row count under this
    // marker must be unchanged from before the call, not just "the marker
    // string used by this assertion is absent" (which a garbage insert with
    // a different URI would satisfy just as well).
    let count_after = db
        .get_recent_events(TEST_USER, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.original_post_uri == MARKER)
        .count();
    assert_eq!(
        count_after, count_before,
        "empty-slice insert must not change the row count"
    );
}

#[tokio::test]
async fn test_pg_batch_insert_many_rows_preserve_own_values() {
    let Some(url) = database_url() else {
        return;
    };
    const MARKER: &str = "at://did:plc:me/app.bsky.feed.post/pgorder1";
    cleanup_batch_test_data(&url, MARKER).await.unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    // 250 rows (mirrors the SQLite test, which chunks at 100/statement).
    // Postgres has no chunk boundary — UNNEST binds 8 arrays plus the $1
    // scalar regardless of row count — but this is still the test that
    // would catch a column-order mistake in the UNNEST rewrite: a
    // mismatched array would smear one row's values onto another (or onto
    // the wrong column) rather than failing outright.
    //
    // event_type alternates quote/repost by row index (rather than staying
    // constant) and amplifier_did is per-row unique, and both are asserted
    // below — this is what catches a $2/$3 (event_type/amplifier_did) bind
    // transposition specifically: with a constant event_type and an
    // unasserted amplifier_did, that exact swap would land
    // event_type="did:plc:pgorderNNNN" / amplifier_did="repost" on every
    // row and no assertion here would notice.
    let events: Vec<charcoal::db::models::NewAmplificationEvent> = (0..250)
        .map(|i| charcoal::db::models::NewAmplificationEvent {
            event_type: if i % 2 == 0 { "repost" } else { "quote" }.to_string(),
            amplifier_did: format!("did:plc:pgorder{:04}", i),
            amplifier_handle: format!("pgorder{:04}.bsky.social", i),
            original_post_uri: MARKER.to_string(),
            amplifier_post_uri: None,
            amplifier_text: Some(format!("text-{}", i)),
            original_post_text: None,
            context_score: Some(i as f64 / 1000.0),
        })
        .collect();

    let n = db
        .insert_amplification_events_batch(TEST_USER, &events)
        .await
        .unwrap();
    assert_eq!(n, 250);

    let stored = db.get_recent_events(TEST_USER, 1000).await.unwrap();
    let stored: Vec<_> = stored
        .into_iter()
        .filter(|e| e.original_post_uri == MARKER)
        .collect();
    assert_eq!(stored.len(), 250);

    // Every row must keep its own field values — check by id order, which is
    // input order per the determinism contract.
    let mut by_id = stored;
    by_id.sort_by_key(|e| e.id);
    for (i, e) in by_id.iter().enumerate() {
        let expected_event_type = if i % 2 == 0 { "repost" } else { "quote" };
        assert_eq!(e.event_type, expected_event_type);
        assert_eq!(e.amplifier_did, format!("did:plc:pgorder{:04}", i));
        assert_eq!(e.amplifier_handle, format!("pgorder{:04}.bsky.social", i));
        assert_eq!(e.amplifier_text, Some(format!("text-{}", i)));
        assert_eq!(e.context_score, Some(i as f64 / 1000.0));
    }
}

#[tokio::test]
async fn test_pg_get_recent_events_breaks_detected_at_ties_by_id_desc() {
    let Some(url) = database_url() else {
        return;
    };
    const MARKER: &str = "at://did:plc:me/app.bsky.feed.post/pgtie";
    cleanup_batch_test_data(&url, MARKER).await.unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    // A single batch insert gives every row the same detected_at (#216): the
    // whole batch runs in one transaction, so `NOW()` is captured once, not
    // once per row. `get_recent_events` ordering by `detected_at DESC` alone
    // would then leave same-batch rows in an arbitrary (storage-dependent)
    // order — verified empirically against this same live Postgres instance
    // (250 rows, 1 distinct timestamp, non-sequential id order returned).
    // The `id DESC` tiebreaker makes "newest first" deterministic: ids
    // ascend in input order, so the returned order must be the exact reverse
    // of insertion order.
    let events: Vec<charcoal::db::models::NewAmplificationEvent> = (0..20)
        .map(|i| charcoal::db::models::NewAmplificationEvent {
            event_type: "repost".to_string(),
            amplifier_did: format!("did:plc:pgtie{:04}", i),
            amplifier_handle: format!("pgtie{:04}.bsky.social", i),
            original_post_uri: MARKER.to_string(),
            amplifier_post_uri: None,
            amplifier_text: None,
            original_post_text: None,
            context_score: None,
        })
        .collect();

    let n = db
        .insert_amplification_events_batch(TEST_USER, &events)
        .await
        .unwrap();
    assert_eq!(n, 20);

    let stored: Vec<_> = db
        .get_recent_events(TEST_USER, 1000)
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.original_post_uri == MARKER)
        .collect();
    assert_eq!(stored.len(), 20);

    // All rows share one detected_at — prove the shared-timestamp premise,
    // not just assert the assumed consequence.
    let distinct_timestamps: std::collections::HashSet<_> =
        stored.iter().map(|e| e.detected_at.clone()).collect();
    assert_eq!(
        distinct_timestamps.len(),
        1,
        "batch insert must share one detected_at across all rows"
    );

    // With detected_at tied, the tiebreaker must produce strictly descending
    // ids — i.e. the exact reverse of insertion order.
    for pair in stored.windows(2) {
        assert!(
            pair[0].id > pair[1].id,
            "ids must be strictly descending: {} then {}",
            pair[0].id,
            pair[1].id
        );
    }
    assert_eq!(stored[0].amplifier_did, "did:plc:pgtie0019");
    assert_eq!(stored[19].amplifier_did, "did:plc:pgtie0000");
}

#[tokio::test]
async fn test_pg_amplification_event() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await.unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let id = db
        .insert_amplification_event(
            TEST_USER,
            "quote",
            "did:plc:pgtest_amp",
            "pgtest_troll.bsky.social",
            "at://did:plc:me/app.bsky.feed.post/pgtest1",
            Some("at://did:plc:pgtest_amp/app.bsky.feed.post/q1"),
            Some("test quote text"),
            None,
            None,
        )
        .await
        .unwrap();
    assert!(id > 0);

    let events = db.get_recent_events(TEST_USER, 10).await.unwrap();
    assert!(!events.is_empty());
}

#[tokio::test]
async fn test_pg_table_count() {
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let count = db.table_count().await.unwrap();
    assert!(count >= 6, "Expected at least 6 tables, got {count}");
}

#[tokio::test]
async fn test_pg_is_score_stale_missing() {
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    assert!(db
        .is_score_stale(TEST_USER, "did:plc:nonexistent_pg", 7)
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
    let median = db.get_median_engagement(TEST_USER).await.unwrap();
    assert!(median >= 0.0);
}

// ── Classification staging tests (#208) ──────────────────────────────────────

/// Delete staging rows written by the staging test so it's idempotent.
async fn cleanup_staging_data(url: &str) -> Result<()> {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;

    let pool = Pool::<Postgres>::connect(url)
        .await
        .map_err(|e| anyhow::anyhow!("cleanup: failed to connect: {e}"))?;

    sqlx_core::query::query("DELETE FROM classification_queue WHERE user_did = $1")
        .bind(TEST_USER)
        .execute(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("cleanup: classification_queue delete failed: {e}"))?;

    sqlx_core::query::query("DELETE FROM scan_account_input WHERE user_did = $1")
        .bind(TEST_USER)
        .execute(&pool)
        .await
        .map_err(|e| anyhow::anyhow!("cleanup: scan_account_input delete failed: {e}"))?;

    Ok(())
}

fn make_pg_queue_row(account_did: &str, post_uri: &str, status: &str) -> QueueRow {
    QueueRow {
        account_did: account_did.to_string(),
        post_uri: post_uri.to_string(),
        text: format!("test post text for {post_uri}"),
        context_text: None,
        post_kind: "original".to_string(),
        onnx_score: 0.05,
        status: status.to_string(),
        toxic_token: None,
        confidence: None,
        model_id: None,
        policy_version: None,
    }
}

#[tokio::test]
async fn test_pg_staging_round_trip() {
    let Some(url) = database_url() else {
        return;
    };
    // Connect first so migrations run — on a fresh DB the staging tables don't
    // exist yet, so cleanup must come AFTER connect creates them.
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    cleanup_staging_data(&url).await.unwrap();
    cleanup_test_data(&url).await.unwrap();

    // Ensure user exists (FK constraint)
    db.upsert_user(TEST_USER, "pgtest.bsky.social")
        .await
        .unwrap();

    // --- enqueue → fetch_pending honors status and limit ---
    let row_a = make_pg_queue_row("did:plc:pga", "at://did:plc:pga/post/1", "pending");
    let row_b = make_pg_queue_row("did:plc:pgb", "at://did:plc:pgb/post/1", "pending");
    let row_done = QueueRow {
        status: "done".to_string(),
        toxic_token: Some(true),
        confidence: Some(0.9),
        model_id: Some("test-model".to_string()),
        policy_version: Some("v1".to_string()),
        ..make_pg_queue_row("did:plc:pgc", "at://did:plc:pgc/post/1", "done")
    };

    db.enqueue_classifications(TEST_USER, &[row_a.clone(), row_b.clone(), row_done.clone()])
        .await
        .unwrap();

    // fetch_pending should return only the 2 pending rows, capped by limit
    let pending = db
        .fetch_pending_classifications(TEST_USER, 1)
        .await
        .unwrap();
    assert_eq!(
        pending.len(),
        1,
        "limit=1 must return exactly 1 pending row"
    );

    let all_pending = db
        .fetch_pending_classifications(TEST_USER, 100)
        .await
        .unwrap();
    assert_eq!(
        all_pending.len(),
        2,
        "should have 2 pending rows, done row excluded"
    );

    // --- count_pending ---
    let count = db.count_pending_classifications(TEST_USER).await.unwrap();
    assert_eq!(count, 2, "count_pending must match pending row count");

    // --- record_verdicts flips pending→done; read back via fetch_account_verdicts ---
    let verdict = VerdictRow {
        account_did: "did:plc:pga".to_string(),
        post_uri: "at://did:plc:pga/post/1".to_string(),
        toxic_token: false,
        confidence: 0.7,
        model_id: "cope-b-v1".to_string(),
        policy_version: "p1".to_string(),
    };
    db.record_classification_verdicts(TEST_USER, &[verdict])
        .await
        .unwrap();

    let verdicts_a = db
        .fetch_account_verdicts(TEST_USER, "did:plc:pga")
        .await
        .unwrap();
    assert_eq!(verdicts_a.len(), 1);
    assert_eq!(verdicts_a[0].status, "done");
    assert_eq!(verdicts_a[0].toxic_token, Some(false));
    assert!((verdicts_a[0].confidence.unwrap() - 0.7).abs() < 0.001);
    assert_eq!(verdicts_a[0].model_id.as_deref(), Some("cope-b-v1"));
    assert_eq!(verdicts_a[0].policy_version.as_deref(), Some("p1"));

    // --- enqueue UPSERT: same PK → one row ---
    db.enqueue_classifications(TEST_USER, std::slice::from_ref(&row_b))
        .await
        .unwrap();
    let rows_b = db
        .fetch_account_verdicts(TEST_USER, "did:plc:pgb")
        .await
        .unwrap();
    assert_eq!(
        rows_b.len(),
        1,
        "UPSERT: re-enqueue same PK must yield one row"
    );

    // --- done-preservation: re-enqueueing a done row must not clear its verdict ---
    let re_enqueue_done = make_pg_queue_row("did:plc:pgc", "at://did:plc:pgc/post/1", "pending");
    db.enqueue_classifications(TEST_USER, std::slice::from_ref(&re_enqueue_done))
        .await
        .unwrap();
    let rows_c = db
        .fetch_account_verdicts(TEST_USER, "did:plc:pgc")
        .await
        .unwrap();
    assert_eq!(rows_c.len(), 1);
    assert_eq!(
        rows_c[0].status, "done",
        "done-preservation: status must stay 'done' after re-enqueue"
    );
    assert_eq!(
        rows_c[0].toxic_token,
        Some(true),
        "done-preservation: toxic_token must be preserved"
    );

    // --- stash/fetch_account_input round-trip (compare parsed JSON) ---
    let payload = r#"{"schema_version":1,"foo":"bar","nums":[1,2,3]}"#;
    db.stash_account_input(TEST_USER, "did:plc:pga", payload)
        .await
        .unwrap();
    let fetched = db
        .fetch_account_input(TEST_USER, "did:plc:pga")
        .await
        .unwrap()
        .expect("stashed payload must be retrievable");
    // JSONB does not preserve byte-exact strings; compare parsed values
    let expected: serde_json::Value = serde_json::from_str(payload).unwrap();
    let actual: serde_json::Value = serde_json::from_str(&fetched).unwrap();
    assert_eq!(
        actual, expected,
        "stash/fetch round-trip must preserve JSON semantics"
    );

    // --- list_scan_accounts returns distinct DIDs ---
    let accounts = db.list_scan_accounts(TEST_USER).await.unwrap();
    assert!(
        accounts.contains(&"did:plc:pga".to_string()),
        "list_scan_accounts must include enqueued DID"
    );
    assert!(
        accounts.contains(&"did:plc:pgb".to_string()),
        "list_scan_accounts must include enqueued DID"
    );
    // Distinct: each DID appears exactly once regardless of row count
    let pga_count = accounts
        .iter()
        .filter(|d| d.as_str() == "did:plc:pga")
        .count();
    assert_eq!(pga_count, 1, "list_scan_accounts must return distinct DIDs");

    // --- clear_scan_staging empties both tables ---
    db.clear_scan_staging(TEST_USER).await.unwrap();
    let after_clear = db.count_pending_classifications(TEST_USER).await.unwrap();
    assert_eq!(
        after_clear, 0,
        "clear_scan_staging must empty classification_queue"
    );
    let input_after = db
        .fetch_account_input(TEST_USER, "did:plc:pga")
        .await
        .unwrap();
    assert!(
        input_after.is_none(),
        "clear_scan_staging must empty scan_account_input"
    );
}

#[tokio::test]
async fn test_pg_get_fresh_scored_dids_matches_is_score_stale() {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;
    use std::collections::HashSet;

    let Some(url) = database_url() else {
        return;
    };

    // Marker DIDs unique to this test; clean them up first so a prior run's rows
    // can't leak in.
    let fresh_did = "did:plc:pgfresh_stale_test_ok";
    let stale_did = "did:plc:pgfresh_stale_test_old";
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    for did in [fresh_did, stale_did] {
        sqlx_core::query::query("DELETE FROM account_scores WHERE user_did = $1 AND did = $2")
            .bind(TEST_USER)
            .bind(did)
            .execute(&pool)
            .await
            .unwrap();
    }

    // Insert both fresh, then age one to 8 days (stale). Raw INSERT so we can
    // control scored_at directly (the trait upsert always stamps NOW()).
    for (did, age_days) in [(fresh_did, 0i32), (stale_did, 8i32)] {
        sqlx_core::query::query(
            "INSERT INTO account_scores (user_did, did, handle, scored_at)
             VALUES ($1, $2, $3, NOW() - make_interval(days => $4))",
        )
        .bind(TEST_USER)
        .bind(did)
        .bind(format!("{did}.handle"))
        .bind(age_days)
        .execute(&pool)
        .await
        .unwrap();
    }

    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    let fresh: HashSet<String> = db
        .get_fresh_scored_dids(TEST_USER, 7)
        .await
        .unwrap()
        .into_iter()
        .collect();

    assert!(
        fresh.contains(fresh_did),
        "recently-scored DID must be fresh"
    );
    assert!(
        !fresh.contains(stale_did),
        "8-day-old DID must not be fresh"
    );

    // Equivalence with the per-DID path (same make_interval cutoff), including
    // a never-scored DID which must be stale/absent.
    for did in [fresh_did, stale_did, "did:plc:pgfresh_never_scored"] {
        let stale = db.is_score_stale(TEST_USER, did, 7).await.unwrap();
        assert_eq!(
            fresh.contains(did),
            !stale,
            "fresh-set membership must equal !is_score_stale for {did}"
        );
    }

    // Cleanup.
    for did in [fresh_did, stale_did] {
        sqlx_core::query::query("DELETE FROM account_scores WHERE user_did = $1 AND did = $2")
            .bind(TEST_USER)
            .bind(did)
            .execute(&pool)
            .await
            .unwrap();
    }
}

/// Account deletion must clear `scan_skips` on the Postgres backend too (#234).
///
/// Production runs Postgres, so the SQLite test for this proves nothing about
/// the deployed path. `scan_skips` holds the user's DID, the DIDs of accounts
/// scanned on their behalf, and raw error text.
/// Uses its OWN user, not the shared `TEST_USER`: this test calls
/// `delete_user_data`, and these tests run in parallel against one database, so
/// deleting the shared user would pull data out from under its neighbours.
#[tokio::test]
async fn test_pg_delete_user_data_clears_scan_skips() {
    const DEL_USER: &str = "did:plc:pgtest_del0000000000000";

    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    // Start from a known state without touching the shared fixtures.
    db.delete_user_data(DEL_USER).await.unwrap();

    db.upsert_user(DEL_USER, "pgtest-del.bsky.social")
        .await
        .unwrap();
    db.record_scan_skip(DEL_USER, "did:plc:pgharasser", "gather", "boom")
        .await
        .unwrap();
    assert_eq!(
        db.count_scan_skips(DEL_USER).await.unwrap(),
        1,
        "precondition: the skip must exist, or this test cannot fail"
    );

    db.delete_user_data(DEL_USER).await.unwrap();

    assert_eq!(
        db.count_scan_skips(DEL_USER).await.unwrap(),
        0,
        "scan_skips must not survive account deletion on Postgres"
    );
}

// --- scan_queue (#257) ---
//
// Unlike every other table in this file, `scan_queue` position/count queries
// are deliberately GLOBAL — that's what "queue position" means. Every other
// test in this file scopes its assertions to its own user_did, which is
// enough isolation when tests run in parallel (the default). These three
// cannot: `test_pg_enqueue_is_idempotent`'s position assertion counts every
// currently-queued row in the table, so a `queued` row left mid-flight by
// `test_pg_claim_respects_the_concurrency_cap` (its cap-3 refusal is a queued
// row until that test's own cleanup runs) can inflate it. Observed directly:
// running with the default parallel harness failed ~1 run in 3 with
// `left: 2, right: 1` on the position assertion. Individually, or under
// `--test-threads=1` on a clean table, all three pass every time — so the
// queue logic itself is correct; only cross-test scheduling was at fault.
// Serializing them (not the whole suite) with a static mutex fixes it without
// slowing down every other test in this file.
fn scan_queue_test_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Every DID used by the scan_queue tests, so they can be cleared wholesale.
const SCAN_QUEUE_DID_PREFIX: &str = "did:plc:pgtest_q_%";

/// Clear the whole scan_queue fixture set before a test runs.
///
/// Per-test `delete_user_data` is not enough here: cap, position, and median
/// are whole-table figures, so ONE row left behind by a panicking test — a
/// `running` row in particular — occupies a slot and fails every later test in
/// this group. That cascade is exactly what happened when negative controls
/// were run against these tests. The prefix belongs solely to this group, and
/// the group is serialized by `scan_queue_test_lock`, so this is safe.
async fn reset_scan_queue_fixtures(url: &str) {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;

    let pool = Pool::<Postgres>::connect(url).await.unwrap();
    sqlx_core::query::query("DELETE FROM scan_queue WHERE user_did LIKE $1")
        .bind(SCAN_QUEUE_DID_PREFIX)
        .execute(&pool)
        .await
        .unwrap();
}

/// Admission must never exceed the cap, and claims must come back in FIFO
/// order — `ORDER BY enqueued_at` is the whole reason position means anything.
///
/// This test is SEQUENTIAL by construction, so it exercises the cap guard and
/// the ordering but NOT the concurrent case; see
/// `test_pg_concurrent_claims_never_exceed_the_cap` for that.
#[tokio::test]
async fn test_pg_claim_respects_the_concurrency_cap() {
    let _guard = scan_queue_test_lock().lock().await;

    const A: &str = "did:plc:pgtest_q_aaaaaaaaaaaaa";
    const B: &str = "did:plc:pgtest_q_bbbbbbbbbbbbb";
    const C: &str = "did:plc:pgtest_q_ccccccccccccc";

    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    for d in [A, B, C] {
        db.delete_user_data(d).await.unwrap();
        db.upsert_user(d, "q.bsky.social").await.unwrap();
        db.enqueue_scan(d).await.unwrap();
        // NOW() has microsecond resolution but the three enqueues are fast
        // enough to land in the same tick on some machines; a real gap makes
        // the FIFO assertion below deterministic.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Cap of 2: two claims succeed, the third is refused.
    let first = db.claim_next_scan(2, 120).await.unwrap();
    let second = db.claim_next_scan(2, 120).await.unwrap();
    let third = db.claim_next_scan(2, 120).await.unwrap();

    assert_eq!(
        first.as_ref().map(|c| c.user_did.as_str()),
        Some(A),
        "FIFO: the oldest enqueued row must be claimed first"
    );
    assert_eq!(
        second.as_ref().map(|c| c.user_did.as_str()),
        Some(B),
        "FIFO: the second-oldest row must be claimed second"
    );
    assert!(third.is_none(), "third claim must be refused at cap 2");

    // Each claim mints its own fencing token.
    assert_ne!(
        first.as_ref().unwrap().claim_id,
        second.as_ref().unwrap().claim_id,
        "each claim must get a distinct claim_id"
    );

    for d in [A, B, C] {
        db.delete_user_data(d).await.unwrap();
    }
}

/// A worker whose lease lapsed must not be able to free or extend the slot
/// that was handed to someone else. This is what the claim_id fencing token
/// exists for: without it, a zombie's `finish_queued_scan` stomps the new
/// owner's running row to 'done' and over-admits the next claim.
#[tokio::test]
async fn test_pg_stale_claim_cannot_finish_or_heartbeat() {
    let _guard = scan_queue_test_lock().lock().await;

    const U: &str = "did:plc:pgtest_q_jjjjjjjjjjjjj";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "q.bsky.social").await.unwrap();
    db.enqueue_scan(U).await.unwrap();

    // Worker A claims with an already-expired lease, then the row is reclaimed
    // and worker B claims it — exactly the sequence a redeploy produces.
    let a = db.claim_next_scan(2, -1).await.unwrap().expect("A claims");
    assert_eq!(db.reclaim_expired_scans().await.unwrap(), 1);
    let b = db.claim_next_scan(2, 120).await.unwrap().expect("B claims");
    assert_eq!(b.user_did, U);
    assert_ne!(a.claim_id, b.claim_id, "the reclaim must invalidate A");

    // Zombie A must be rejected on both surfaces.
    assert!(
        !db.heartbeat_scan(U, &a.claim_id, 120).await.unwrap(),
        "a stale claim must not extend the new owner's lease"
    );
    assert!(
        !db.finish_queued_scan(U, &a.claim_id, None).await.unwrap(),
        "a stale claim must not finish the new owner's scan"
    );
    assert_eq!(
        db.scan_queue_entry(U, 1).await.unwrap().unwrap().status,
        "running",
        "the row must still belong to B"
    );

    // B, holding the live token, succeeds.
    assert!(db.heartbeat_scan(U, &b.claim_id, 120).await.unwrap());
    assert!(db.finish_queued_scan(U, &b.claim_id, None).await.unwrap());
    assert_eq!(
        db.scan_queue_entry(U, 1).await.unwrap().unwrap().status,
        "done"
    );

    // A finished scan releases its slot and can be re-enqueued.
    db.enqueue_scan(U).await.unwrap();
    let entry = db.scan_queue_entry(U, 1).await.unwrap().expect("re-queued");
    assert_eq!(entry.status, "queued", "a done scan can be requeued");
    assert_eq!(entry.position, 1);

    db.delete_user_data(U).await.unwrap();
}

/// `finish_queued_scan` on a failure records the error and the 'failed' status.
#[tokio::test]
async fn test_pg_finish_records_failure() {
    let _guard = scan_queue_test_lock().lock().await;

    const U: &str = "did:plc:pgtest_q_kkkkkkkkkkkkk";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "q.bsky.social").await.unwrap();
    db.enqueue_scan(U).await.unwrap();

    let claim = db.claim_next_scan(1, 120).await.unwrap().expect("claimed");
    assert!(db
        .finish_queued_scan(U, &claim.claim_id, Some("boom"))
        .await
        .unwrap());

    let entry = db.scan_queue_entry(U, 1).await.unwrap().expect("present");
    assert_eq!(entry.status, "failed");
    assert_eq!(
        entry.eta_seconds, None,
        "a finished scan has no ETA — not Some(0)"
    );

    // Finishing twice must be a no-op, not a second state change.
    assert!(
        !db.finish_queued_scan(U, &claim.claim_id, None)
            .await
            .unwrap(),
        "the row is no longer running, so finish must not fire again"
    );

    db.delete_user_data(U).await.unwrap();
}

/// A running scan's remaining time is unknown, so `eta_seconds` must be None —
/// `position` is forced to 0 for non-queued rows, so computing anyway would
/// tell a user watching their own scan "0 seconds remaining".
#[tokio::test]
async fn test_pg_running_scan_has_no_eta() {
    let _guard = scan_queue_test_lock().lock().await;

    const U: &str = "did:plc:pgtest_q_lllllllllllll";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "q.bsky.social").await.unwrap();
    db.enqueue_scan(U).await.unwrap();
    db.claim_next_scan(1, 120).await.unwrap().expect("claimed");

    let entry = db.scan_queue_entry(U, 1).await.unwrap().expect("present");
    assert_eq!(entry.status, "running");
    assert_eq!(entry.position, 0);
    assert_eq!(
        entry.eta_seconds, None,
        "a running scan reports no ETA, not Some(0)"
    );

    db.delete_user_data(U).await.unwrap();
}

/// The two backends must quote the SAME `eta_seconds` for the same scan
/// history. They compute the median duration by different routes — Postgres
/// via `EXTRACT(EPOCH FROM (finished_at - started_at))`, SQLite in Rust — and
/// the SQLite side used `num_seconds()`, which truncates, while `EXTRACT`
/// keeps fractional seconds. A user whose deployment moved from SQLite to
/// Postgres therefore saw the estimate change with no change in history.
///
/// Both sides are seeded with the SAME hand-written timestamps rather than
/// real scans, because a wall-clock duration differs between the two runs and
/// could not be compared for equality at all. The duration carries a half
/// second and the queued user sits two batches out, so truncation is visible:
/// 181s correct, 180s truncated.
#[tokio::test]
async fn test_pg_eta_matches_sqlite_for_fractional_durations() {
    let _guard = scan_queue_test_lock().lock().await;

    const DONE: &str = "did:plc:pgtest_q_nnnnnnnnnnnnn";
    const A: &str = "did:plc:pgtest_q_ooooooooooooo";
    const B: &str = "did:plc:pgtest_q_ppppppppppppp";
    // A 90.5-second scan: the half second is the whole point.
    const STARTED: &str = "2026-08-06T00:00:00+00:00";
    const FINISHED: &str = "2026-08-06T00:01:30.5+00:00";

    let Some(url) = database_url() else {
        return;
    };

    // --- SQLite side -------------------------------------------------------
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    charcoal::db::schema::create_tables(&conn).expect("schema");
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, enqueued_at, started_at, finished_at)
         VALUES (?1, 'done', ?2, ?2, ?3)",
        rusqlite::params![DONE, STARTED, FINISHED],
    )
    .expect("seed the completed scan");
    charcoal::db::queries::enqueue_scan(&conn, A).expect("enqueue A");
    std::thread::sleep(std::time::Duration::from_millis(10));
    charcoal::db::queries::enqueue_scan(&conn, B).expect("enqueue B");
    let sqlite_entry = charcoal::db::queries::scan_queue_entry(&conn, B, 1)
        .expect("sqlite queue entry")
        .expect("row exists");

    // --- Postgres side -----------------------------------------------------
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    {
        use sqlx_core::pool::Pool;
        use sqlx_postgres::Postgres;

        let pool = Pool::<Postgres>::connect(&url).await.unwrap();
        sqlx_core::query::query(
            "INSERT INTO scan_queue (user_did, status, enqueued_at, started_at, finished_at)
             VALUES ($1, 'done', $2, $2, $3)",
        )
        .bind(DONE)
        .bind(chrono::DateTime::parse_from_rfc3339(STARTED).unwrap())
        .bind(chrono::DateTime::parse_from_rfc3339(FINISHED).unwrap())
        .execute(&pool)
        .await
        .expect("seed the completed scan");
    }
    db.upsert_user(A, "q.bsky.social").await.unwrap();
    db.upsert_user(B, "q.bsky.social").await.unwrap();
    db.enqueue_scan(A).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    db.enqueue_scan(B).await.unwrap();
    let pg_entry = db
        .scan_queue_entry(B, 1)
        .await
        .unwrap()
        .expect("row exists");

    // Same position on both, or the ETAs would be comparing different waits.
    assert_eq!(sqlite_entry.position, 2, "SQLite: B waits behind A");
    assert_eq!(pg_entry.position, 2, "Postgres: B waits behind A");
    assert_eq!(
        sqlite_entry.eta_seconds, pg_entry.eta_seconds,
        "the backends must quote the same ETA for the same scan history"
    );
    assert_eq!(
        pg_entry.eta_seconds,
        Some(181),
        "a 90.5s median over two batches is 181s; 180 means a backend truncated \
         the half second away"
    );

    for d in [DONE, A, B] {
        db.delete_user_data(d).await.unwrap();
    }
}

/// enqueued_at must be RFC3339 so both backends parse identically — Postgres's
/// `::TEXT` rendering ("2026-08-06 00:36:25.231997-07") is not.
#[tokio::test]
async fn test_pg_enqueued_at_is_rfc3339() {
    let _guard = scan_queue_test_lock().lock().await;

    const U: &str = "did:plc:pgtest_q_mmmmmmmmmmmmm";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "q.bsky.social").await.unwrap();
    db.enqueue_scan(U).await.unwrap();

    let entry = db.scan_queue_entry(U, 1).await.unwrap().expect("queued");
    chrono::DateTime::parse_from_rfc3339(&entry.enqueued_at)
        .unwrap_or_else(|e| panic!("enqueued_at {:?} is not RFC3339: {e}", entry.enqueued_at));

    db.delete_user_data(U).await.unwrap();
}

/// The admitter cannot tell an idle queue from a wedged one without this —
/// `claim_next_scan` returns None for both. Postgres side of the parity with
/// `queries::tests::depth_counts_queued_and_running_separately`.
///
/// The counts are whole-table, so this belongs to the serialized scan_queue
/// group and asserts DELTAS rather than absolutes: another suite's leftover row
/// would otherwise make it flap.
#[tokio::test]
async fn test_pg_scan_queue_depth_counts_queued_and_running() {
    let _guard = scan_queue_test_lock().lock().await;

    const A: &str = "did:plc:pgtest_q_nnnnnnnnnnnnn";
    const B: &str = "did:plc:pgtest_q_ooooooooooooo";

    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    let before = db.scan_queue_depth().await.unwrap();

    for d in [A, B] {
        db.delete_user_data(d).await.unwrap();
        db.upsert_user(d, "q.bsky.social").await.unwrap();
        db.enqueue_scan(d).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let queued = db.scan_queue_depth().await.unwrap();
    assert_eq!(queued.queued, before.queued + 2, "two rows now waiting");
    assert_eq!(queued.running, before.running, "nothing claimed yet");

    let claim = db
        .claim_next_scan(before.running + 1, 120)
        .await
        .unwrap()
        .expect("a queued row exists");
    let claimed = db.scan_queue_depth().await.unwrap();
    assert_eq!(
        (claimed.queued, claimed.running),
        (before.queued + 1, before.running + 1),
        "a claim must move the row from queued to running, not double-count it"
    );

    // Finished rows are neither waiting nor holding a slot.
    db.finish_queued_scan(&claim.user_did, &claim.claim_id, None)
        .await
        .unwrap();
    let finished = db.scan_queue_depth().await.unwrap();
    assert_eq!(
        (finished.queued, finished.running),
        (before.queued + 1, before.running),
        "a finished row must not keep counting against the cap"
    );

    for d in [A, B] {
        db.delete_user_data(d).await.unwrap();
    }
}

/// The cap must hold when admitters run at the SAME TIME, which is the only
/// scenario that matters — a sequential run never contends and so never
/// exercises the locking at all.
///
/// N concurrent claimers against N queued rows at cap 1: exactly one may be
/// granted. Asserting equality rather than `<=` keeps the test from passing
/// vacuously if nothing is claimable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_pg_concurrent_claims_never_exceed_the_cap() {
    let _guard = scan_queue_test_lock().lock().await;

    const CAP: usize = 1;
    const DIDS: [&str; 4] = [
        "did:plc:pgtest_q_fffffffffffff",
        "did:plc:pgtest_q_ggggggggggggg",
        "did:plc:pgtest_q_hhhhhhhhhhhhh",
        "did:plc:pgtest_q_iiiiiiiiiiiii",
    ];

    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    for d in DIDS {
        db.delete_user_data(d).await.unwrap();
        db.upsert_user(d, "q.bsky.social").await.unwrap();
        db.enqueue_scan(d).await.unwrap();
    }

    let mut set = tokio::task::JoinSet::new();
    for _ in 0..DIDS.len() {
        let db = std::sync::Arc::clone(&db);
        set.spawn(async move { db.claim_next_scan(CAP, 120).await.unwrap() });
    }

    let mut granted = 0usize;
    while let Some(res) = set.join_next().await {
        if res.unwrap().is_some() {
            granted += 1;
        }
    }

    assert_eq!(
        granted, CAP,
        "cap {CAP}: exactly {CAP} concurrent claim(s) may be admitted, got {granted}"
    );

    for d in DIDS {
        db.delete_user_data(d).await.unwrap();
    }
}

/// Enqueue is keyed by user_did, so a double-click cannot double-book.
///
/// The row count and position alone do NOT test this — user_did is the primary
/// key, so a second insert can only ever produce one row regardless of the
/// `WHERE status IN ('done','failed')` guard. What that guard actually buys is
/// an UNCHANGED `enqueued_at`: without it the second call resets the timestamp
/// and the double-clicking user is sent to the back of the FIFO. That is the
/// assertion below.
#[tokio::test]
async fn test_pg_enqueue_is_idempotent() {
    let _guard = scan_queue_test_lock().lock().await;

    const U: &str = "did:plc:pgtest_q_ddddddddddddd";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "q.bsky.social").await.unwrap();

    db.enqueue_scan(U).await.unwrap();
    let first = db.scan_queue_entry(U, 1).await.unwrap().expect("queued");

    // A real gap, so a reset enqueued_at would be visibly different.
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    db.enqueue_scan(U).await.unwrap();
    let second = db.scan_queue_entry(U, 1).await.unwrap().expect("queued");

    assert_eq!(second.status, "queued");
    assert_eq!(second.position, 1, "one row, so position 1 — not two rows");
    assert_eq!(
        second.enqueued_at, first.enqueued_at,
        "a re-enqueue while still queued must not move the user's place in line"
    );

    db.delete_user_data(U).await.unwrap();
}

/// A scan orphaned by a redeploy must return to the queue, not vanish.
/// Combined with #208's scan_phase the reclaimed scan resumes rather than
/// restarting, so nobody re-pays for completed work.
#[tokio::test]
async fn test_pg_expired_lease_is_reclaimed() {
    let _guard = scan_queue_test_lock().lock().await;

    const U: &str = "did:plc:pgtest_q_eeeeeeeeeeeee";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "q.bsky.social").await.unwrap();
    db.enqueue_scan(U).await.unwrap();

    // Claim with a lease that has already expired.
    let claimed = db.claim_next_scan(2, -1).await.unwrap();
    assert_eq!(claimed.map(|c| c.user_did).as_deref(), Some(U));

    let reclaimed = db.reclaim_expired_scans().await.unwrap();
    assert_eq!(reclaimed, 1, "the expired running row must be re-queued");

    let entry = db.scan_queue_entry(U, 1).await.unwrap().expect("present");
    assert_eq!(entry.status, "queued", "reclaimed back to queued");

    db.delete_user_data(U).await.unwrap();
}

/// A running row whose lease is NULL is otherwise unrecoverable — nothing
/// would ever reclaim it and the slot stays occupied forever.
#[tokio::test]
async fn test_pg_null_lease_is_reclaimed() {
    let _guard = scan_queue_test_lock().lock().await;

    // DIDs are unique per test in this module even though the lock plus
    // `reset_scan_queue_fixtures` serialize the group. Sharing them made
    // isolation depend on that serialization holding, and made a panic
    // mid-test impossible to attribute to one test's rows.
    const U: &str = "did:plc:pgtest_q_ttttttttttttt";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "q.bsky.social").await.unwrap();
    db.enqueue_scan(U).await.unwrap();
    db.claim_next_scan(1, 120).await.unwrap().expect("claimed");

    // Null the lease directly — the state a crash between claim and heartbeat
    // can leave behind.
    {
        use sqlx_core::pool::Pool;
        use sqlx_postgres::Postgres;
        let pool = Pool::<Postgres>::connect(&url).await.unwrap();
        sqlx_core::query::query("UPDATE scan_queue SET lease_expires = NULL WHERE user_did = $1")
            .bind(U)
            .execute(&pool)
            .await
            .unwrap();
    }

    assert_eq!(
        db.reclaim_expired_scans().await.unwrap(),
        1,
        "a running row with a NULL lease must be reclaimed, not stranded"
    );
    assert_eq!(
        db.scan_queue_entry(U, 1).await.unwrap().unwrap().status,
        "queued"
    );

    db.delete_user_data(U).await.unwrap();
}

/// The ETA must divide by the concurrency cap: with cap N the expected wait is
/// about `ceil(position / N)` scan-lengths, not `position` of them. Scans here
/// run 22 minutes to 2 hours, so ignoring the cap tells a position-4 user at
/// cap 2 roughly twice their real wait.
#[tokio::test]
async fn test_pg_eta_accounts_for_the_concurrency_cap() {
    let _guard = scan_queue_test_lock().lock().await;

    // One finished scan of a known duration gives a deterministic median.
    // Unique to this test — see the note in test_pg_null_lease_is_reclaimed.
    const DONE: &str = "did:plc:pgtest_q_uuuuuuuuuuuuu";
    // Four queued rows so the last one sits at position 4.
    const QUEUED: [&str; 4] = [
        "did:plc:pgtest_q_ppppppppppppp",
        "did:plc:pgtest_q_qqqqqqqqqqqqq",
        "did:plc:pgtest_q_rrrrrrrrrrrrr",
        "did:plc:pgtest_q_sssssssssssss",
    ];

    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    // The median is a whole-table figure, so this test needs the table to hold
    // exactly one finished row — its own. `reset_scan_queue_fixtures` above
    // cleared the group's rows; anything else in scan_queue at this point
    // belongs to production data in a shared database, which this test cannot
    // and should not assume away, so it asserts nothing about other users.
    {
        use sqlx_core::pool::Pool;
        use sqlx_postgres::Postgres;
        let pool = Pool::<Postgres>::connect(&url).await.unwrap();

        db.delete_user_data(DONE).await.unwrap();
        db.upsert_user(DONE, "q.bsky.social").await.unwrap();
        // A 'done' row lasting exactly 600s.
        sqlx_core::query::query(
            "INSERT INTO scan_queue (user_did, status, enqueued_at, started_at, finished_at)
             VALUES ($1, 'done', NOW(), NOW() - INTERVAL '600 seconds', NOW())",
        )
        .bind(DONE)
        .execute(&pool)
        .await
        .unwrap();
    }

    for d in QUEUED {
        db.delete_user_data(d).await.unwrap();
        db.upsert_user(d, "q.bsky.social").await.unwrap();
        db.enqueue_scan(d).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let last = QUEUED[3];
    let at_cap_1 = db.scan_queue_entry(last, 1).await.unwrap().unwrap();
    assert_eq!(at_cap_1.position, 4);
    assert_eq!(
        at_cap_1.eta_seconds,
        Some(2400),
        "cap 1: four ahead-or-self x 600s"
    );

    let at_cap_2 = db.scan_queue_entry(last, 2).await.unwrap().unwrap();
    assert_eq!(
        at_cap_2.eta_seconds,
        Some(1200),
        "cap 2: ceil(4/2) = 2 batches x 600s — half of the cap-1 figure"
    );

    let at_cap_3 = db.scan_queue_entry(last, 3).await.unwrap().unwrap();
    assert_eq!(
        at_cap_3.eta_seconds,
        Some(1200),
        "cap 3: ceil(4/3) = 2 batches — ceiling, not floor"
    );

    db.delete_user_data(DONE).await.unwrap();
    for d in QUEUED {
        db.delete_user_data(d).await.unwrap();
    }
}

/// Postgres side of `queries::tests::list_scan_queue_*` (#288).
///
/// Ordering, position, and the timestamp FORMAT all have to match SQLite —
/// the admin dashboard renders whatever this returns, and #270 exists because
/// one backend was covered and the other was not.
///
/// The timestamp assertion is the one that has already bitten this branch
/// once: `enqueued_at::TEXT` renders "2026-08-06 00:36:25.231997-07", which
/// is neither RFC3339 nor stable across connection TimeZones.
#[tokio::test]
async fn test_pg_list_scan_queue_orders_and_numbers_rows() {
    let _guard = scan_queue_test_lock().lock().await;

    // Unique to this test — colliding with another test's DIDs was a review
    // finding on #257. Named for INSERTION order, which here is also
    // alphabetical order and the exact REVERSE of the expected result.
    const ALPHA: &str = "did:plc:pgtest_q_list288alpha";
    const BRAVO: &str = "did:plc:pgtest_q_list288bravo";
    const CHARLIE: &str = "did:plc:pgtest_q_list288chrly";

    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    // Seeded with chosen timestamps running BACKWARDS, not via `enqueue_scan`.
    // `enqueue_scan` stamps NOW(), so its rows come out in insertion order and
    // enqueued_at order at once — and an unordered Postgres SELECT returns heap
    // (insertion) order, so an assertion built that way passes against a query
    // with no ORDER BY at all. Here insertion and alphabetical order both say
    // ALPHA, BRAVO, CHARLIE while the only correct answer is the reverse.
    {
        use sqlx_core::pool::Pool;
        use sqlx_postgres::Postgres;
        let pool = Pool::<Postgres>::connect(&url).await.unwrap();
        for (age_secs, did) in [ALPHA, BRAVO, CHARLIE].iter().enumerate() {
            db.delete_user_data(did).await.unwrap();
            db.upsert_user(did, "q.bsky.social").await.unwrap();
            sqlx_core::query::query(
                "INSERT INTO scan_queue (user_did, status, enqueued_at)
                 VALUES ($1, 'queued', NOW() - make_interval(secs => $2))",
            )
            .bind(did)
            .bind(age_secs as f64)
            .execute(&pool)
            .await
            .unwrap();
        }
    }

    let rows = db.list_scan_queue().await.unwrap();
    let dids: Vec<&str> = rows.iter().map(|r| r.user_did.as_str()).collect();
    assert_eq!(
        dids,
        vec![CHARLIE, BRAVO, ALPHA],
        "rows must come back oldest-first; insertion order and alphabetical \
         order both say the opposite here"
    );
    let positions: Vec<i64> = rows.iter().map(|r| r.position).collect();
    assert_eq!(
        positions,
        vec![1, 2, 3],
        "queued rows are numbered 1..n by enqueued_at — the row behind the \
         oldest is 2, not 1"
    );

    // Every timestamp is RFC3339, matching SQLite's TEXT column.
    for row in &rows {
        chrono::DateTime::parse_from_rfc3339(&row.enqueued_at)
            .unwrap_or_else(|e| panic!("enqueued_at {:?} is not RFC3339: {e}", row.enqueued_at));
        assert!(
            row.started_at.is_none() && row.finished_at.is_none() && row.last_error.is_none(),
            "a queued row has not started, finished, or failed"
        );
    }

    // Claiming the oldest must renumber the rest: a running row holds a slot,
    // not a place in line, so BRAVO becomes position 1.
    let claim = db.claim_next_scan(1, 120).await.unwrap().expect("claim");
    assert_eq!(claim.user_did, CHARLIE, "FIFO claims the oldest row");
    let rows = db.list_scan_queue().await.unwrap();
    let by_did = |did: &str| {
        rows.iter()
            .find(|r| r.user_did == did)
            .unwrap_or_else(|| panic!("{did} must be listed"))
            .clone()
    };
    let running = by_did(CHARLIE);
    assert_eq!(running.status, "running");
    assert_eq!(
        running.position, 0,
        "a running row holds a slot, not a place"
    );
    let started_at = running.started_at.expect("claiming stamps started_at");
    chrono::DateTime::parse_from_rfc3339(&started_at)
        .unwrap_or_else(|e| panic!("started_at {started_at:?} is not RFC3339: {e}"));
    assert_eq!(
        by_did(BRAVO).position,
        1,
        "BRAVO is now first in line — a position that ignored status would say 2"
    );

    // A failed scan must be distinguishable from one that never ran, which is
    // the whole reason #288 exists.
    db.finish_queued_scan(CHARLIE, &claim.claim_id, Some("gather exploded"))
        .await
        .unwrap();
    let failed = db
        .list_scan_queue()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.user_did == CHARLIE)
        .expect("CHARLIE must still be listed after failing");
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.position, 0);
    assert_eq!(failed.last_error.as_deref(), Some("gather exploded"));
    let finished_at = failed.finished_at.expect("a failed row records when");
    chrono::DateTime::parse_from_rfc3339(&finished_at)
        .unwrap_or_else(|e| panic!("finished_at {finished_at:?} is not RFC3339: {e}"));

    for d in [ALPHA, BRAVO, CHARLIE] {
        db.delete_user_data(d).await.unwrap();
    }
}

/// Postgres side of `queries::tests::*_ties_by_user_did` (#271).
///
/// `enqueued_at` alone is a PARTIAL order, and `enqueue_scan` stamps `NOW()` —
/// two requests inside the same microsecond tie. When they do, a position
/// counted as `enqueued_at <=` gives every tied row the SAME number while the
/// display `ORDER BY` still renders a definite sequence, and
/// `claim_next_scan`'s own `ORDER BY` picks whichever row the plan happens to
/// reach first. Display, position, and admission must share one total order:
/// `(enqueued_at, user_did)`.
#[tokio::test]
async fn test_pg_scan_queue_breaks_enqueued_at_ties_by_user_did() {
    let _guard = scan_queue_test_lock().lock().await;

    // Named for INSERTION order below, which is the exact REVERSE of the
    // expected answer — so heap order cannot pass this by accident.
    const ALPHA: &str = "did:plc:pgtest_q_tie271_alpha";
    const BRAVO: &str = "did:plc:pgtest_q_tie271_bravo";
    const CHARLIE: &str = "did:plc:pgtest_q_tie271_chrly";

    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    // One literal timestamp for all three: a genuine tie, written directly
    // because `enqueue_scan`'s NOW() cannot be relied on to collide.
    {
        use sqlx_core::pool::Pool;
        use sqlx_postgres::Postgres;
        let pool = Pool::<Postgres>::connect(&url).await.unwrap();
        for did in [CHARLIE, BRAVO, ALPHA] {
            db.delete_user_data(did).await.unwrap();
            db.upsert_user(did, "q.bsky.social").await.unwrap();
            sqlx_core::query::query(
                "INSERT INTO scan_queue (user_did, status, enqueued_at)
                 VALUES ($1, 'queued', TIMESTAMPTZ '2026-08-09 00:00:01+00')",
            )
            .bind(did)
            .execute(&pool)
            .await
            .unwrap();
        }
    }

    let rows = db.list_scan_queue().await.unwrap();
    let listed: Vec<(&str, i64)> = rows
        .iter()
        .map(|r| (r.user_did.as_str(), r.position))
        .collect();
    assert_eq!(
        listed,
        vec![(ALPHA, 1), (BRAVO, 2), (CHARLIE, 3)],
        "tied rows must get DISTINCT positions, in the order they are \
         displayed; counting `enqueued_at <=` alone gives all three 3"
    );

    // The per-user column has to agree with the queue panel row-for-row.
    for (did, expected) in [(ALPHA, 1), (BRAVO, 2), (CHARLIE, 3)] {
        let entry = db.scan_queue_entry(did, 1).await.unwrap().unwrap();
        assert_eq!(
            entry.position, expected,
            "{did} must be told the same position the queue panel shows"
        );
    }

    // And admission must take whoever the dashboard shows as next.
    let claim = db.claim_next_scan(1, 120).await.unwrap().expect("claim");
    assert_eq!(
        claim.user_did, rows[0].user_did,
        "admission must take the row the dashboard shows as next"
    );
    assert_eq!(
        claim.user_did, ALPHA,
        "the total order is (enqueued_at, user_did), so ALPHA goes first"
    );

    db.finish_queued_scan(ALPHA, &claim.claim_id, None)
        .await
        .unwrap();
    for d in [ALPHA, BRAVO, CHARLIE] {
        db.delete_user_data(d).await.unwrap();
    }
}

// --- Access requests (#309) ---

/// Delete a test's `access_requests` row directly — the table is deliberately
/// NOT cascaded by `delete_user_data` (it's an admin grant/deny record, not
/// user content; see migration 0014), so cleanup has to go around it.
async fn delete_access_request(url: &str, did: &str) {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;
    let pool = Pool::<Postgres>::connect(url).await.unwrap();
    sqlx_core::query::query("DELETE FROM access_requests WHERE did = $1")
        .bind(did)
        .execute(&pool)
        .await
        .unwrap();
}

/// Postgres side of `tests/unit_access.rs` — same state machine, same
/// assertions, against `PgDatabase` instead of `SqliteDatabase`.
#[tokio::test]
async fn test_pg_access_requests_state_machine_parity() {
    let Some(url) = database_url() else {
        return;
    };
    const DID: &str = "did:plc:pgaccesstest000000000000";
    const NO_ROW_DID: &str = "did:plc:norow0000000000000000000";
    // `connect_postgres` runs migrations, so it must go first — this may be
    // the run that creates `access_requests` (migration 0014) in a fresh
    // `charcoal_test` database.
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    delete_access_request(&url, DID).await;

    db.upsert_access_request_pending(DID, "old.bsky.social")
        .await
        .unwrap();
    let row = db.get_access_request(DID).await.unwrap().unwrap();
    assert_eq!(
        (row.status.as_str(), row.handle.as_str()),
        ("pending", "old.bsky.social")
    );

    // Deny, then sign in again with a new handle: status must NOT reset —
    // ON CONFLICT only refreshes handle.
    assert!(db
        .set_access_status(DID, "denied", "did:plc:admin")
        .await
        .unwrap());
    db.upsert_access_request_pending(DID, "new.bsky.social")
        .await
        .unwrap();
    let row = db.get_access_request(DID).await.unwrap().unwrap();
    assert_eq!(row.status, "denied", "denied is sticky through re-login");
    assert_eq!(row.handle, "new.bsky.social", "handle refreshes anyway");

    // Admin grant-by-handle flips a denied row straight to allowed.
    db.grant_access(DID, "new.bsky.social", "did:plc:admin")
        .await
        .unwrap();
    assert_eq!(
        db.get_access_request(DID).await.unwrap().unwrap().status,
        "allowed"
    );

    // Deciding a row that doesn't exist reports false, not an error.
    assert!(!db
        .set_access_status(NO_ROW_DID, "allowed", "x")
        .await
        .unwrap());

    assert!(!db.list_access_requests().await.unwrap().is_empty());

    delete_access_request(&url, DID).await;
}

// --- OAuth write sessions (#315) ---
//
// Each test below owns a private DID (rather than sharing one constant) so
// that parallel test runs cannot delete each other's rows out from under a
// concurrently-running test — the CodeRabbit R3 finding on PR #109.

async fn delete_actions_rows(url: &str, did: &str) {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;
    let pool = Pool::<Postgres>::connect(url).await.unwrap();
    for sql in [
        "DELETE FROM actions WHERE user_did = $1",
        "DELETE FROM action_batches WHERE user_did = $1",
        "DELETE FROM oauth_sessions WHERE user_did = $1",
    ] {
        sqlx_core::query::query(sql)
            .bind(did)
            .execute(&pool)
            .await
            .unwrap();
    }
}

fn pg_session_row(did: &str, updated_at: &str) -> charcoal::db::traits::OauthSessionRow {
    charcoal::db::traits::OauthSessionRow {
        user_did: did.to_string(),
        pds_url: "https://pds.example".to_string(),
        scope: "atproto repo:app.bsky.graph.block".to_string(),
        access_token_enc: vec![1, 2, 3],
        refresh_token_enc: vec![4, 5, 6],
        dpop_key_enc: vec![7, 8, 9],
        access_expires_at: 1_700_000_000,
        created_at: "2026-09-01T00:00:00+00:00".to_string(),
        updated_at: updated_at.to_string(),
    }
}

/// Postgres side of `tests/unit_actions_db.rs` oauth_sessions tests.
#[tokio::test]
async fn test_pg_oauth_session_parity() {
    const OAUTH_DID: &str = "did:plc:pgactionsoauth0000000000";

    let Some(url) = database_url() else {
        return;
    };
    delete_actions_rows(&url, OAUTH_DID).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    assert!(db.get_oauth_session(OAUTH_DID).await.unwrap().is_none());
    db.upsert_oauth_session(&pg_session_row(OAUTH_DID, "t1"))
        .await
        .unwrap();
    assert_eq!(
        db.get_oauth_session(OAUTH_DID).await.unwrap().unwrap(),
        pg_session_row(OAUTH_DID, "t1")
    );

    let mut second = pg_session_row(OAUTH_DID, "t2");
    second.created_at = "2026-09-02T00:00:00+00:00".to_string();
    second.access_token_enc = vec![9, 9, 9];
    db.upsert_oauth_session(&second).await.unwrap();
    let got = db.get_oauth_session(OAUTH_DID).await.unwrap().unwrap();
    assert_eq!(got.created_at, "2026-09-01T00:00:00+00:00");
    assert_eq!(got.access_token_enc, vec![9, 9, 9]);

    assert!(!db
        .update_oauth_tokens(
            OAUTH_DID,
            &[10],
            &[11],
            2_000_000_000,
            "atproto new",
            "stale",
            "t3"
        )
        .await
        .unwrap());
    assert!(db
        .update_oauth_tokens(
            OAUTH_DID,
            &[10],
            &[11],
            2_000_000_000,
            "atproto new",
            "t2",
            "t3"
        )
        .await
        .unwrap());
    let got = db.get_oauth_session(OAUTH_DID).await.unwrap().unwrap();
    assert_eq!(got.access_token_enc, vec![10]);
    assert_eq!(got.scope, "atproto new");
    assert_eq!(got.updated_at, "t3");
    assert_eq!(got.dpop_key_enc, vec![7, 8, 9]);

    // Compare-and-delete: a stale expectation leaves the row alone.
    assert!(!db
        .delete_oauth_session_if_unchanged(OAUTH_DID, "t2")
        .await
        .unwrap());
    assert!(db.get_oauth_session(OAUTH_DID).await.unwrap().is_some());
    assert!(db
        .delete_oauth_session_if_unchanged(OAUTH_DID, "t3")
        .await
        .unwrap());
    assert!(db.get_oauth_session(OAUTH_DID).await.unwrap().is_none());

    db.upsert_oauth_session(&pg_session_row(OAUTH_DID, "t4"))
        .await
        .unwrap();
    assert!(db.delete_oauth_session(OAUTH_DID).await.unwrap());
    assert!(!db.delete_oauth_session(OAUTH_DID).await.unwrap());
    delete_actions_rows(&url, OAUTH_DID).await;
}

/// Postgres side of the action_batches/actions tests in tests/unit_actions_db.rs.
#[tokio::test]
async fn test_pg_action_batches_parity() {
    const BATCHES_DID: &str = "did:plc:pgactionsbatches00000000";

    let Some(url) = database_url() else {
        return;
    };
    delete_actions_rows(&url, BATCHES_DID).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    use charcoal::db::traits::NewAction;

    let na = |t: &str, k: &str| NewAction {
        target_did: t.to_string(),
        kind: k.to_string(),
        undo_of: None,
        score_at_action: Some(41.5),
        tier_at_action: Some("High".to_string()),
    };

    let first = db
        .create_action_batch(
            BATCHES_DID,
            "mute",
            "tier:High",
            &[na("did:plc:a", "mute"), na("did:plc:b", "mute")],
        )
        .await
        .unwrap();
    let b = db.get_action_batch(first).await.unwrap().unwrap();
    assert_eq!((b.status.as_str(), b.requested), ("queued", 2));
    let rows = db.list_actions_for_batch(first).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].score_at_action, Some(41.5));

    db.set_action_batch_status(first, "running", None)
        .await
        .unwrap();
    let started = db
        .get_action_batch(first)
        .await
        .unwrap()
        .unwrap()
        .started_at
        .unwrap();
    db.set_action_batch_status(first, "running", None)
        .await
        .unwrap();
    assert_eq!(
        db.get_action_batch(first)
            .await
            .unwrap()
            .unwrap()
            .started_at
            .unwrap(),
        started
    );

    // "failed" stamps finished_at AND error; "queued" clears both (session
    // reconnect / retry path) — mirrors batch_status_transitions_stamp_timestamps.
    db.set_action_batch_status(first, "failed", Some("not_connected"))
        .await
        .unwrap();
    let b = db.get_action_batch(first).await.unwrap().unwrap();
    assert_eq!(b.status, "failed");
    assert_eq!(b.error.as_deref(), Some("not_connected"));
    assert!(b.finished_at.is_some());

    db.set_action_batch_status(first, "queued", None)
        .await
        .unwrap();
    let b = db.get_action_batch(first).await.unwrap().unwrap();
    assert!(b.error.is_none());
    assert!(
        b.finished_at.is_none(),
        "a queued transition must clear finished_at, not just error"
    );

    db.update_action(
        rows[0].id,
        "applied",
        Some("at://x/app.bsky.graph.block/y"),
        None,
    )
    .await
    .unwrap();
    db.update_action(rows[0].id, "undone", None, None)
        .await
        .unwrap();
    let r = db.get_action(rows[0].id).await.unwrap().unwrap();
    assert_eq!(
        r.record_uri.as_deref(),
        Some("at://x/app.bsky.graph.block/y")
    );
    assert!(r.undone_at.is_some() && r.applied_at.is_some());

    db.update_action(rows[0].id, "failed", None, Some("boom"))
        .await
        .unwrap();
    assert_eq!(
        db.get_action(rows[0].id)
            .await
            .unwrap()
            .unwrap()
            .error
            .as_deref(),
        Some("boom")
    );

    db.update_action(rows[1].id, "skipped_already_done", None, None)
        .await
        .unwrap();
    assert_eq!(db.active_actions(BATCHES_DID).await.unwrap().len(), 1);

    let second = db
        .create_action_batch(BATCHES_DID, "block", "single", &[])
        .await
        .unwrap();
    assert_eq!(
        db.list_action_batches(BATCHES_DID, 10, 0)
            .await
            .unwrap()
            .iter()
            .map(|b| b.id)
            .collect::<Vec<_>>(),
        vec![second, first]
    );
    let unfinished = db.list_unfinished_batches().await.unwrap();
    assert!(unfinished.contains(&first) && unfinished.contains(&second));
    db.set_action_batch_status(first, "partial", Some("1 failed"))
        .await
        .unwrap();
    assert!(db
        .get_action_batch(first)
        .await
        .unwrap()
        .unwrap()
        .finished_at
        .is_some());
    assert!(!db.list_unfinished_batches().await.unwrap().contains(&first));

    delete_actions_rows(&url, BATCHES_DID).await;
}

/// Postgres side of `undo_rows_point_at_originals` in
/// tests/unit_actions_db.rs: an undo row's `undo_of` must point back at the
/// original action's id, surviving the round trip through `create_action_batch`.
#[tokio::test]
async fn test_pg_undo_rows_point_at_originals() {
    const UNDO_DID: &str = "did:plc:pgactions_undo00000000";

    let Some(url) = database_url() else {
        return;
    };
    delete_actions_rows(&url, UNDO_DID).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    use charcoal::db::traits::NewAction;

    let na = |t: &str, k: &str| NewAction {
        target_did: t.to_string(),
        kind: k.to_string(),
        undo_of: None,
        score_at_action: Some(41.5),
        tier_at_action: Some("High".to_string()),
    };

    let orig = db
        .create_action_batch(UNDO_DID, "mute", "single", &[na("did:plc:a", "mute")])
        .await
        .unwrap();
    let orig_row = db.list_actions_for_batch(orig).await.unwrap()[0].id;
    let mut undo = na("did:plc:a", "mute");
    undo.undo_of = Some(orig_row);
    let undo_batch = db
        .create_action_batch(UNDO_DID, "undo", &format!("undo:{orig}"), &[undo])
        .await
        .unwrap();
    let rows = db.list_actions_for_batch(undo_batch).await.unwrap();
    assert_eq!(rows[0].undo_of, Some(orig_row));
    assert_eq!(rows[0].kind, "mute");

    delete_actions_rows(&url, UNDO_DID).await;
}

/// Postgres side of `listing_and_active_and_unfinished` in
/// tests/unit_actions_db.rs. `list_unfinished_batches` is a GLOBAL query (no
/// user_did filter), unlike everything else this test checks — so unlike the
/// SQLite version, which runs against a fresh in-memory database and can
/// compare the returned Vec for exact equality outright, this test filters
/// the result down to the three ids it created before comparing. That keeps
/// the same "exact list, not just contains" assertion shape without assuming
/// the whole `action_batches` table is empty, which isn't safe against a
/// shared, persistent Postgres instance.
#[tokio::test]
async fn test_pg_listing_and_active_and_unfinished() {
    const LST_DID: &str = "did:plc:pgactions_lst_first000";
    const LST_OTHER_DID: &str = "did:plc:pgactions_lst_other000";

    let Some(url) = database_url() else {
        return;
    };
    delete_actions_rows(&url, LST_DID).await;
    delete_actions_rows(&url, LST_OTHER_DID).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    use charcoal::db::traits::NewAction;

    let na = |t: &str, k: &str| NewAction {
        target_did: t.to_string(),
        kind: k.to_string(),
        undo_of: None,
        score_at_action: Some(41.5),
        tier_at_action: Some("High".to_string()),
    };

    let first = db
        .create_action_batch(LST_DID, "mute", "tier:High", &[na("did:plc:a", "mute")])
        .await
        .unwrap();
    let second = db
        .create_action_batch(LST_DID, "block", "single", &[na("did:plc:b", "block")])
        .await
        .unwrap();
    let other = db
        .create_action_batch(LST_OTHER_DID, "mute", "single", &[na("did:plc:c", "mute")])
        .await
        .unwrap();

    // Newest first, scoped to the user, paginated.
    let page = db.list_action_batches(LST_DID, 10, 0).await.unwrap();
    assert_eq!(
        page.iter().map(|b| b.id).collect::<Vec<_>>(),
        vec![second, first]
    );
    assert_eq!(
        db.list_action_batches(LST_DID, 1, 1).await.unwrap()[0].id,
        first
    );

    // Unfinished across all users, id ascending (boot resume) — filtered to
    // this test's own ids, see the doc comment above.
    let ids_of_interest = [first, second, other];
    let filtered = |all: Vec<i64>| -> Vec<i64> {
        all.into_iter()
            .filter(|id| ids_of_interest.contains(id))
            .collect()
    };
    assert_eq!(
        filtered(db.list_unfinished_batches().await.unwrap()),
        vec![first, second, other]
    );
    db.set_action_batch_status(second, "done", None)
        .await
        .unwrap();
    assert_eq!(
        filtered(db.list_unfinished_batches().await.unwrap()),
        vec![first, other]
    );

    // Active = applied or skipped_already_done, per user.
    let a = db.list_actions_for_batch(first).await.unwrap()[0].id;
    let b = db.list_actions_for_batch(second).await.unwrap()[0].id;
    db.update_action(a, "skipped_already_done", None, None)
        .await
        .unwrap();
    db.update_action(b, "applied", Some("at://x/app.bsky.graph.block/y"), None)
        .await
        .unwrap();
    let active = db.active_actions(LST_DID).await.unwrap();
    assert_eq!(active.iter().map(|r| r.id).collect::<Vec<_>>(), vec![a, b]);
    db.update_action(b, "undone", None, None).await.unwrap();
    assert_eq!(db.active_actions(LST_DID).await.unwrap().len(), 1);
    assert!(db
        .active_actions("did:plc:nobody")
        .await
        .unwrap()
        .is_empty());

    delete_actions_rows(&url, LST_DID).await;
    delete_actions_rows(&url, LST_OTHER_DID).await;
}

/// Postgres side of the score-snapshot + cascade test in
/// tests/unit_actions_db.rs (score_snapshots_and_cascade).
#[tokio::test]
async fn test_pg_action_score_snapshots_and_cascade() {
    const CASCADE_DID: &str = "did:plc:pgactionscascade0000000";

    let Some(url) = database_url() else {
        return;
    };
    delete_actions_rows(&url, CASCADE_DID).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    use charcoal::db::traits::NewAction;

    db.upsert_user(CASCADE_DID, "actions.pgtest").await.unwrap();
    let score = AccountScore {
        did: "did:plc:a".to_string(),
        handle: "a.test".to_string(),
        toxicity_score: Some(0.5),
        topic_overlap: Some(0.3),
        overlap_legacy: None,
        threat_score: Some(41.5),
        threat_tier: Some("High".to_string()),
        posts_analyzed: 10,
        top_toxic_posts: vec![],
        scored_at: "2026-09-01T12:00:00Z".to_string(),
        behavioral_signals: None,
        context_score: None,
        graph_distance: None,
        fingerprint_quality: None,
        scoring_confidence: None,
    };
    db.upsert_account_score(CASCADE_DID, &score).await.unwrap();
    let snaps = db.list_score_snapshots(CASCADE_DID).await.unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].did, "did:plc:a");
    assert_eq!(snaps[0].handle, "a.test");
    assert_eq!(snaps[0].threat_tier.as_deref(), Some("High"));

    let id = db
        .create_action_batch(
            CASCADE_DID,
            "mute",
            "single",
            &[NewAction {
                target_did: "did:plc:a".to_string(),
                kind: "mute".to_string(),
                undo_of: None,
                score_at_action: Some(41.5),
                tier_at_action: Some("High".to_string()),
            }],
        )
        .await
        .unwrap();
    // An OAuth write session is the other row `delete_user_data` has to clear
    // on the backend that actually runs in production (#315).
    db.upsert_oauth_session(&charcoal::db::traits::OauthSessionRow {
        user_did: CASCADE_DID.to_string(),
        pds_url: "https://pds.pgtest".to_string(),
        scope: "atproto".to_string(),
        access_token_enc: vec![1, 2, 3],
        refresh_token_enc: vec![4, 5, 6],
        dpop_key_enc: vec![7, 8, 9],
        access_expires_at: 4_102_444_800,
        created_at: "2026-09-01T12:00:00Z".to_string(),
        updated_at: "2026-09-01T12:00:00Z".to_string(),
    })
    .await
    .unwrap();
    assert!(db.get_oauth_session(CASCADE_DID).await.unwrap().is_some());

    db.delete_user_data(CASCADE_DID).await.unwrap();
    assert!(db.get_action_batch(id).await.unwrap().is_none());
    // No ON DELETE CASCADE on `actions.batch_id`: deleting the batch alone
    // would leave the target DIDs behind.
    assert!(db.list_actions_for_batch(id).await.unwrap().is_empty());
    assert!(db.get_oauth_session(CASCADE_DID).await.unwrap().is_none());
    assert!(db
        .list_score_snapshots(CASCADE_DID)
        .await
        .unwrap()
        .is_empty());

    delete_actions_rows(&url, CASCADE_DID).await;
}
