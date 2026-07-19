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

    // Delete test-specific scan_state keys (scoped by user_did)
    sqlx_core::query::query(
        "DELETE FROM scan_state WHERE user_did = 'did:plc:pgtest_user000000000000' AND key = 'test_cursor'",
    )
    .execute(&pool)
    .await
    .map_err(|e| anyhow::anyhow!("cleanup: scan_state delete failed: {e}"))?;

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
