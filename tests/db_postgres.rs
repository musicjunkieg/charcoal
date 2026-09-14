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
use charcoal::db::traits::LAST_FULL_SCAN_DURATION_KEY;
use charcoal::db::{EnqueueOutcome, FinishCompletion, ScanKind};
use charcoal::pipeline::scan_phases::staging::{QueueRow, VerdictRow};

const TEST_USER: &str = "did:plc:pgtest_user000000000000";

/// In CI the Postgres service is mandatory; a missing DATABASE_URL must fail
/// the job, not turn every test in this file into a silent early return.
fn database_url() -> Option<String> {
    let url = std::env::var("DATABASE_URL")
        .ok()
        .filter(|u| u.starts_with("postgres://"));
    if url.is_none() && std::env::var("CI").is_ok() {
        panic!("DATABASE_URL is required in CI for tests/db_postgres.rs");
    }
    url
}

/// The dedicated database destructive migration fixtures are allowed to reset
/// (V3-06) — everything else in this file uses `database_url()`'s ordinary
/// `charcoal_test` database and must never see a table dropped out from under
/// it. Same CI contract as `database_url()`.
fn migrations_database_url() -> Option<String> {
    let url = std::env::var("DATABASE_URL_MIGRATIONS")
        .ok()
        .filter(|u| u.starts_with("postgres://"));
    if url.is_none() && std::env::var("CI").is_ok() {
        panic!("DATABASE_URL_MIGRATIONS is required in CI for tests/db_postgres.rs");
    }
    url
}

/// Process-local half of the double serialization for tests that reset the
/// `_migrations` database (V4-05). The Postgres session advisory lock taken
/// in `migrations_fixture` covers separate processes or overlapping CI
/// invocations; this mutex covers tests within this one binary, matching the
/// `scan_queue_test_lock` / `cache_test_lock` pattern above.
fn migrations_db_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Guard for a test against the dedicated `_migrations` database, built to an
/// AUTHENTIC older-schema state (V2-07) by `migrate_postgres_through`. Holds
/// the process-local mutex AND a dedicated (non-pooled) Postgres connection
/// carrying a session advisory lock
/// (`pg_advisory_lock(hashtext('charcoal_migrations_fixture'))`) for its
/// whole lifetime — released together when the guard drops at the end of the
/// test, so a second test binary or an overlapping CI invocation blocks
/// rather than interleaves (V4-05). The drop-all inside
/// `migrate_postgres_through` happens at construction time, under both
/// locks, so a failed test leaves nothing for the next one to trip over.
struct MigrationsFixture {
    _mutex_guard: tokio::sync::MutexGuard<'static, ()>,
    _lock_conn: sqlx_postgres::PgConnection,
}

async fn migrations_fixture(url: &str, max_version: i64) -> MigrationsFixture {
    use sqlx_core::connection::Connection;

    let mutex_guard = migrations_db_lock().lock().await;

    // A dedicated (non-pooled) connection: the session advisory lock it takes
    // is released when THIS connection's session ends, which happens exactly
    // when it drops at the end of the test — no pool reuse to worry about.
    let mut lock_conn = sqlx_postgres::PgConnection::connect(url)
        .await
        .expect("connect dedicated advisory-lock connection to the migrations database");
    sqlx_core::query::query("SELECT pg_advisory_lock(hashtext('charcoal_migrations_fixture'))")
        .execute(&mut lock_conn)
        .await
        .expect("acquire charcoal_migrations_fixture advisory lock");

    charcoal::db::postgres::migrate_postgres_through(url, max_version)
        .await
        .expect("reset migrations database to the requested version");

    MigrationsFixture {
        _mutex_guard: mutex_guard,
        _lock_conn: lock_conn,
    }
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
    // A dedicated user_did, NOT the shared TEST_USER: topic_fingerprint's
    // primary key is user_did ALONE (one row per user, unlike account_scores'
    // (user_did, did) composite), so every test that writes a fingerprint
    // under TEST_USER races the same singleton row against every other such
    // test running concurrently in this file (#344 fixup — observed as a
    // real, reproducible failure, not a hypothetical).
    const OWNER: &str = "did:plc:pgtest_fp_roundtrip_own";
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(OWNER).await.unwrap();

    db.save_fingerprint(OWNER, r#"{"topics": ["test"]}"#, 42)
        .await
        .unwrap();
    let (json, count, _) = db.get_fingerprint(OWNER).await.unwrap().unwrap();
    assert_eq!(json, r#"{"topics": ["test"]}"#);
    assert_eq!(count, 42);

    db.delete_user_data(OWNER).await.unwrap();
}

#[tokio::test]
async fn test_pg_embedding_roundtrip() {
    // Dedicated user_did — see test_pg_fingerprint_roundtrip: TEST_USER's
    // topic_fingerprint row is a singleton other concurrent tests also
    // write, so sharing it here is a real race (observed: this test's own
    // "ensure fingerprint row exists" step can be clobbered by another
    // test's concurrent delete-then-rewrite before save_embedding runs).
    const OWNER: &str = "did:plc:pgtest_emb_roundtrip_own";
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(OWNER).await.unwrap();

    // Ensure fingerprint row exists
    db.save_fingerprint(OWNER, r#"{"clusters":[]}"#, 10)
        .await
        .unwrap();

    let embedding: Vec<f64> = (0..384).map(|i| i as f64 / 384.0).collect();
    db.save_embedding(OWNER, &embedding).await.unwrap();

    let loaded = db.get_embedding(OWNER).await.unwrap().unwrap();
    assert_eq!(loaded.len(), 384);
    // f64→f32→f64 round-trip loses some precision
    assert!((loaded[0] - 0.0).abs() < 0.001);
    assert!((loaded[383] - 383.0 / 384.0).abs() < 0.001);

    db.delete_user_data(OWNER).await.unwrap();
}

/// #302: `save_fingerprint_bundle` writes the fingerprint row, the mean
/// embedding, and per-topic centroid rows in one transaction; a later
/// keyword-only generation (`embedding: None`) must NULL out a prior
/// embedding and drop the previous generation's extra cluster rows — the
/// bundle IS the generation, not an incremental patch.
#[tokio::test]
async fn test_pg_bundle_roundtrip_and_replacement() {
    // Dedicated user_did — see test_pg_fingerprint_roundtrip: topic_fingerprint
    // is a per-user singleton, so sharing TEST_USER here races every other
    // concurrently-running test that also writes a fingerprint for it.
    const OWNER: &str = "did:plc:pgtest_bundle_roundtrip";
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(OWNER).await.unwrap();

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
    db.save_fingerprint_bundle(
        OWNER,
        "{}",
        42,
        Some(&emb),
        Some(charcoal::topics::embeddings::EMBEDDING_MODEL_ID),
        &clusters,
    )
    .await
    .unwrap();

    let stored = db.get_topic_centroids(OWNER).await.unwrap();
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
    db.save_fingerprint_bundle(OWNER, "{}", 9, None, None, &one)
        .await
        .unwrap();
    let stored = db.get_topic_centroids(OWNER).await.unwrap();
    assert_eq!(stored.len(), 1);
    // Count alone would also pass if the delete ran but the insert did not —
    // assert the survivor is the NEW generation's row, not an old one.
    assert_eq!(stored[0].post_count, 9);
    assert!(
        stored[0].centroid.iter().all(|v| (v - 0.9).abs() < 1e-6),
        "the surviving row must be the new generation's centroid"
    );
    // None embedding leaves the column NULL for this generation.
    assert!(db.get_embedding(OWNER).await.unwrap().is_none());

    db.delete_user_data(OWNER).await.unwrap();
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
    db.save_fingerprint_bundle(DEL_USER, "{}", 3, None, None, &clusters)
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
        .is_score_stale(TEST_USER, "did:plc:nonexistent_pg")
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

/// #344 R11: the fresh predicate on Postgres — `scoring_generation = $n AND
/// valid_until > NOW()`, natively boolean (no COALESCE needed; valid_until is
/// NOT NULL here) — matches the SQLite semantics pinned in
/// `unit_staleness::fresh_set_is_exactly_the_non_stale_dids_including_null_and_malformed_expiry`.
/// Four rows: one fresh, three not (wrong revision, past expiry, and the
/// exact boundary one second before NOW — strict `>`, not `>=`).
#[tokio::test]
async fn test_pg_get_fresh_scored_dids_matches_is_score_stale() {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;
    use std::collections::HashSet;

    let Some(url) = database_url() else {
        return;
    };

    // A dedicated user_did, NOT the shared TEST_USER: get_fresh_scored_dids
    // and count_expired aggregate over every row for the user_did, and many
    // other tests in this file concurrently write fresh rows under
    // TEST_USER — an aggregate assertion scoped to TEST_USER would be
    // flaky under real parallel execution. Scoping to a DID nothing else
    // touches makes the aggregate exact and safe.
    const OWNER: &str = "did:plc:pgfresh_owner_user";
    let current_did = "did:plc:pgfresh_current";
    let expired_did = "did:plc:pgfresh_expired";
    let legacy_did = "did:plc:pgfresh_legacy";
    let boundary_did = "did:plc:pgfresh_boundary";
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    sqlx_core::query::query("DELETE FROM account_scores WHERE user_did = $1")
        .bind(OWNER)
        .execute(&pool)
        .await
        .unwrap();

    let rev = charcoal::scoring::generation::scoring_revision();
    // (did, generation, valid_until_offset_sql)
    let rows: [(&str, &str, &str); 4] = [
        (current_did, rev, "NOW() + make_interval(days => 5)"),
        (expired_did, rev, "NOW() - make_interval(days => 1)"),
        (legacy_did, "legacy", "NOW() + make_interval(days => 5)"),
        (boundary_did, rev, "NOW() - INTERVAL '1 second'"),
    ];
    for (did, generation, valid_until_sql) in rows {
        sqlx_core::query::query(&format!(
            "INSERT INTO account_scores (user_did, did, handle, scoring_generation, valid_until)
             VALUES ($1, $2, $3, $4, {valid_until_sql})"
        ))
        .bind(OWNER)
        .bind(did)
        .bind(format!("{did}.handle"))
        .bind(generation)
        .execute(&pool)
        .await
        .unwrap();
    }

    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    let fresh: HashSet<String> = db
        .get_fresh_scored_dids(OWNER)
        .await
        .unwrap()
        .into_iter()
        .collect();

    assert_eq!(
        fresh,
        HashSet::from([current_did.to_string()]),
        "only the current-revision, not-yet-expired row is fresh"
    );

    // Equivalence with the per-DID path, including a never-scored DID which
    // must be stale/absent.
    for did in [
        current_did,
        expired_did,
        legacy_did,
        boundary_did,
        "did:plc:pgfresh_never_scored",
    ] {
        let stale = db.is_score_stale(OWNER, did).await.unwrap();
        assert_eq!(
            fresh.contains(did),
            !stale,
            "fresh-set membership must equal !is_score_stale for {did}"
        );
    }

    // Every stored non-fresh row is counted — 3 of 4 (R11).
    assert_eq!(db.count_expired(OWNER).await.unwrap(), 3);

    // Cleanup.
    sqlx_core::query::query("DELETE FROM account_scores WHERE user_did = $1")
        .bind(OWNER)
        .execute(&pool)
        .await
        .unwrap();
}

/// #344: `upsert_account_score` stamps `scoring_generation` and `valid_until`
/// from the confidence tier (3/7/14 d) — the write-path twin of
/// `unit_staleness::upsert_stamps_generation_and_valid_until_from_confidence`,
/// on the backend that actually runs in production.
#[tokio::test]
async fn test_pg_upsert_stamps_generation_and_valid_until_from_confidence() {
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let Some(url) = database_url() else {
        return;
    };
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    let dids = [
        "did:plc:pgstamp_low",
        "did:plc:pgstamp_standard",
        "did:plc:pgstamp_high",
        "did:plc:pgstamp_none",
        "did:plc:pgstamp_bogus",
    ];
    for did in dids {
        sqlx_core::query::query("DELETE FROM account_scores WHERE user_did = $1 AND did = $2")
            .bind(TEST_USER)
            .bind(did)
            .execute(&pool)
            .await
            .unwrap();
    }

    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    let rev = charcoal::scoring::generation::scoring_revision();
    for (did, confidence, expected_days) in [
        (dids[0], Some("low"), 3.0_f64),
        (dids[1], Some("standard"), 7.0),
        (dids[2], Some("high"), 14.0),
        (dids[3], None, 7.0),
        (dids[4], Some("bogus"), 7.0),
    ] {
        let score = AccountScore {
            did: did.to_string(),
            handle: format!("{did}.handle"),
            toxicity_score: Some(0.5),
            topic_overlap: Some(0.5),
            overlap_legacy: None,
            threat_score: Some(20.0),
            threat_tier: Some("Elevated".to_string()),
            posts_analyzed: 50,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: confidence.map(str::to_string),
        };
        db.upsert_account_score(TEST_USER, &score).await.unwrap();

        let row = sqlx_core::query::query(
            "SELECT scoring_generation,
                    (EXTRACT(EPOCH FROM (valid_until - scored_at)) / 86400.0)::float8
             FROM account_scores WHERE user_did = $1 AND did = $2",
        )
        .bind(TEST_USER)
        .bind(did)
        .fetch_one(&pool)
        .await
        .unwrap();
        let generation: String = row.get(0);
        let days: f64 = row.get(1);
        assert_eq!(generation, rev, "{did}");
        assert!((days - expected_days).abs() < 0.01, "{did}: {days} days");
        assert!(!db.is_score_stale(TEST_USER, did).await.unwrap(), "{did}");
    }

    for did in dids {
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

/// Sibling to `scan_queue_test_lock` for the #343 shared-cache tables
/// (`account_feed_snapshots`, `onnx_scores`, `classifier_verdicts`). The two
/// migration tests DROP/reconnect against those tables, and the round-trip
/// test below writes into them; none of that overlaps scan_queue, so this is
/// a separate lock rather than reusing `scan_queue_test_lock` and serializing
/// against unrelated work.
fn cache_test_lock() -> &'static tokio::sync::Mutex<()> {
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
    // Since #344 F1 the ETA median is drawn from `scan_state`, so a duration
    // sample left behind by a panicking test skews every later ETA assertion
    // in this group exactly the way a stray queue row used to.
    sqlx_core::query::query("DELETE FROM scan_state WHERE key = $1 AND user_did LIKE $2")
        .bind(LAST_FULL_SCAN_DURATION_KEY)
        .bind(SCAN_QUEUE_DID_PREFIX)
        .execute(&pool)
        .await
        .unwrap();
}

/// Record a full-scan duration sample of exactly `duration_secs` for
/// `user_did`, the way `finish_full_scan_state` does — the only population the
/// ETA median is drawn from since #344 F1. Written directly because a real
/// scan stamps wall-clock times and these tests need durations they can name.
async fn seed_full_scan_duration(url: &str, user_did: &str, duration_secs: &str) {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;

    let pool = Pool::<Postgres>::connect(url).await.unwrap();
    sqlx_core::query::query(
        "INSERT INTO scan_state (user_did, key, value, updated_at)
         VALUES ($1, $2, $3, NOW())
         ON CONFLICT (user_did, key) DO UPDATE SET value = $3, updated_at = NOW()",
    )
    .bind(user_did)
    .bind(LAST_FULL_SCAN_DURATION_KEY)
    .bind(duration_secs)
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
        !db.finish_queued_scan(U, &a.claim_id, FinishCompletion::Complete, None)
            .await
            .unwrap(),
        "a stale claim must not finish the new owner's scan"
    );
    assert_eq!(
        db.scan_queue_entry(U, 1).await.unwrap().unwrap().status,
        "running",
        "the row must still belong to B"
    );

    // B, holding the live token, succeeds.
    assert!(db.heartbeat_scan(U, &b.claim_id, 120).await.unwrap());
    assert!(db
        .finish_queued_scan(U, &b.claim_id, FinishCompletion::Complete, None)
        .await
        .unwrap());
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
        .finish_queued_scan(U, &claim.claim_id, FinishCompletion::Failed, Some("boom"))
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
        !db.finish_queued_scan(U, &claim.claim_id, FinishCompletion::Complete, None)
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

/// The two backends must quote the SAME `eta_seconds` for the same duration
/// history. Both read the raw `scan_state` sample and hand it to the shared
/// median helper, so the only way they can disagree is if one of them starts
/// truncating — which is exactly what the SQLite side used to do with
/// `num_seconds()` while Postgres's `EXTRACT` kept the fraction, changing a
/// user's estimate when their deployment moved backends with no change in
/// history.
///
/// Both sides are seeded with the SAME hand-written duration rather than real
/// scans, because a wall-clock duration differs between the two runs and could
/// not be compared for equality at all. It carries a half second and the
/// queued user sits two batches out, so truncation is visible: 181s correct,
/// 180s truncated.
#[tokio::test]
async fn test_pg_eta_matches_sqlite_for_fractional_durations() {
    let _guard = scan_queue_test_lock().lock().await;

    const DONE: &str = "did:plc:pgtest_q_nnnnnnnnnnnnn";
    const A: &str = "did:plc:pgtest_q_ooooooooooooo";
    const B: &str = "did:plc:pgtest_q_ppppppppppppp";
    // A 90.5-second scan: the half second is the whole point.
    const DURATION: &str = "90.5";

    let Some(url) = database_url() else {
        return;
    };

    // --- SQLite side -------------------------------------------------------
    let conn = rusqlite::Connection::open_in_memory().expect("in-memory SQLite");
    charcoal::db::schema::create_tables(&conn).expect("schema");
    conn.execute(
        // Since #344 F1 the median is drawn from `scan_state`; a `done`
        // `scan_queue` row is invisible to it on both backends.
        "INSERT INTO scan_state (user_did, key, value, updated_at)
         VALUES (?1, ?2, ?3, datetime('now'))",
        rusqlite::params![DONE, LAST_FULL_SCAN_DURATION_KEY, DURATION],
    )
    .expect("seed the duration sample");
    charcoal::db::queries::enqueue_scan(&conn, A).expect("enqueue A");
    std::thread::sleep(std::time::Duration::from_millis(10));
    charcoal::db::queries::enqueue_scan(&conn, B).expect("enqueue B");
    let sqlite_entry = charcoal::db::queries::scan_queue_entry(&conn, B, 1)
        .expect("sqlite queue entry")
        .expect("row exists");

    // --- Postgres side -----------------------------------------------------
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    seed_full_scan_duration(&url, DONE, DURATION).await;
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
    db.finish_queued_scan(
        &claim.user_did,
        &claim.claim_id,
        FinishCompletion::Complete,
        None,
    )
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

    // One recorded duration sample gives a deterministic median.
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

    // The median is a whole-table figure, so this test needs the duration
    // samples to be exactly one — its own. `reset_scan_queue_fixtures` above
    // cleared the group's queue rows AND its duration keys; anything else in
    // `scan_state` at this point belongs to production data in a shared
    // database, which this test cannot and should not assume away, so it
    // asserts nothing about other users.
    db.delete_user_data(DONE).await.unwrap();
    db.upsert_user(DONE, "q.bsky.social").await.unwrap();
    // A full scan that lasted exactly 600s.
    seed_full_scan_duration(&url, DONE, "600").await;

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
    db.finish_queued_scan(
        CHARLIE,
        &claim.claim_id,
        FinishCompletion::Failed,
        Some("gather exploded"),
    )
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

    db.finish_queued_scan(ALPHA, &claim.claim_id, FinishCompletion::Complete, None)
        .await
        .unwrap();
    for d in [ALPHA, BRAVO, CHARLIE] {
        db.delete_user_data(d).await.unwrap();
    }
}

// ── #344: scan_queue.kind, the full-request obligation, the handover ────────
//
// Postgres twins of `tests/unit_scan_kind.rs`. They share the scan_queue
// fixture prefix, lock and wholesale reset used by the #257 tests above rather
// than a prefix of their own: cap, position and median are whole-table
// figures, so one row left behind by a panicking test breaks every later one.

/// The refresh → owed-full handover, end to end (R09/V3-03).
#[tokio::test]
async fn test_pg_enqueue_outcomes_and_handover() {
    let _guard = scan_queue_test_lock().lock().await;

    const U: &str = "did:plc:pgtest_q_kind_handover";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "kind.bsky.social").await.unwrap();

    db.enqueue_refresh_scan(U).await.unwrap();
    let claim = db.claim_next_scan(1, 120).await.unwrap().expect("claimed");
    assert_eq!(claim.kind, ScanKind::Refresh);

    assert_eq!(
        db.enqueue_scan(U).await.unwrap(),
        EnqueueOutcome::QueuedAfterRefresh
    );
    let requested = pg_row(&db, U)
        .await
        .full_requested_at
        .expect("request recorded");
    assert_eq!(
        db.enqueue_scan(U).await.unwrap(),
        EnqueueOutcome::QueuedAfterRefresh,
        "a second click coalesces"
    );
    assert_eq!(
        pg_row(&db, U).await.full_requested_at.as_deref(),
        Some(requested.as_str()),
        "and does not move the request time"
    );

    // The refresh finishes: the row becomes the user's queued FULL scan,
    // dated from the request, with the obligation still recorded.
    assert!(db
        .finish_queued_scan(U, &claim.claim_id, FinishCompletion::Complete, None)
        .await
        .unwrap());
    let row = pg_row(&db, U).await;
    assert_eq!((row.status.as_str(), row.kind), ("queued", ScanKind::Full));
    assert_eq!(row.enqueued_at, requested);
    assert_eq!(row.full_requested_at.as_deref(), Some(requested.as_str()));
    assert_eq!(
        row.completion, None,
        "the handover clears the refresh's own"
    );

    let claim = db.claim_next_scan(1, 120).await.unwrap().expect("admitted");
    assert_eq!(claim.kind, ScanKind::Full);

    // A resumable full attempt keeps the obligation; an unverified one
    // fulfils it (V6-01).
    assert!(db
        .finish_queued_scan(U, &claim.claim_id, FinishCompletion::Resumable, None)
        .await
        .unwrap());
    let row = pg_row(&db, U).await;
    assert_eq!(row.completion, Some(FinishCompletion::Resumable));
    assert_eq!(row.full_requested_at.as_deref(), Some(requested.as_str()));

    db.enqueue_scan(U).await.unwrap();
    let claim = db.claim_next_scan(1, 120).await.unwrap().unwrap();
    assert!(db
        .finish_queued_scan(
            U,
            &claim.claim_id,
            FinishCompletion::CompleteUnverified,
            None
        )
        .await
        .unwrap());
    let row = pg_row(&db, U).await;
    assert_eq!(row.completion, Some(FinishCompletion::CompleteUnverified));
    assert!(row.full_requested_at.is_none(), "fulfilled");

    db.delete_user_data(U).await.unwrap();
}

/// A user's click over a queued refresh keeps the place the refresh held.
#[tokio::test]
async fn test_pg_full_enqueue_upgrades_queued_refresh_keeping_position() {
    let _guard = scan_queue_test_lock().lock().await;

    const U: &str = "did:plc:pgtest_q_kind_upgrade";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "up.bsky.social").await.unwrap();

    db.enqueue_refresh_scan(U).await.unwrap();
    let before = pg_row(&db, U).await.enqueued_at;
    assert_eq!(db.enqueue_scan(U).await.unwrap(), EnqueueOutcome::Queued);
    let row = pg_row(&db, U).await;
    assert_eq!((row.status.as_str(), row.kind), ("queued", ScanKind::Full));
    assert_eq!(
        row.enqueued_at, before,
        "the user keeps the place the refresh held (R09)"
    );
    assert!(row.full_requested_at.is_some());

    db.delete_user_data(U).await.unwrap();
}

/// A refresh enqueue never downgrades a queued full row, never touches a
/// running one, and re-queues OWED full work as full (V3-03).
#[tokio::test]
async fn test_pg_refresh_never_downgrades() {
    let _guard = scan_queue_test_lock().lock().await;

    const U: &str = "did:plc:pgtest_q_kind_nodown";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "nd.bsky.social").await.unwrap();

    // Queued full: untouched.
    db.enqueue_scan(U).await.unwrap();
    db.enqueue_refresh_scan(U).await.unwrap();
    assert_eq!(pg_row(&db, U).await.kind, ScanKind::Full);

    // Running full: untouched, and the claim still owns the row.
    let claim = db.claim_next_scan(1, 120).await.unwrap().unwrap();
    db.enqueue_refresh_scan(U).await.unwrap();
    let row = pg_row(&db, U).await;
    assert_eq!((row.status.as_str(), row.kind), ("running", ScanKind::Full));
    assert!(db
        .finish_queued_scan(U, &claim.claim_id, FinishCompletion::Resumable, None)
        .await
        .unwrap());

    // Finished but still owed: re-queued as FULL, never as a refresh.
    db.enqueue_refresh_scan(U).await.unwrap();
    let row = pg_row(&db, U).await;
    assert_eq!((row.status.as_str(), row.kind), ("queued", ScanKind::Full));
    assert_eq!(
        row.completion, None,
        "a re-queue clears the stale completion"
    );

    db.delete_user_data(U).await.unwrap();
}

/// Postgres twin of
/// `unit_scan_kind::a_full_enqueue_records_the_obligation_on_a_*_full_row_that_lacks_one`
/// (#344 Minor 3): the `COALESCE` on the already-queued and already-running
/// arms records the obligation for a user whose own full scan is already in
/// flight. A row written by a pre-v18 binary carries `full_requested_at IS
/// NULL`, and the enqueue has to repair it — otherwise an interrupted attempt
/// on that row is never retried, because nothing says a full scan is owed.
#[tokio::test]
async fn test_pg_enqueue_records_the_obligation_on_in_flight_full_rows() {
    let _guard = scan_queue_test_lock().lock().await;

    const QUEUED: &str = "did:plc:pgtest_q_oblig_queued";
    const RUNNING: &str = "did:plc:pgtest_q_oblig_running";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let pool = sqlx_core::pool::Pool::<sqlx_postgres::Postgres>::connect(&url)
        .await
        .unwrap();
    sqlx_core::query::query(
        "INSERT INTO scan_queue (user_did, status, kind, enqueued_at, full_requested_at)
         VALUES ($1, 'queued', 'full', $2, NULL)",
    )
    .bind(QUEUED)
    .bind(chrono::DateTime::parse_from_rfc3339("2026-09-10T00:00:00+00:00").unwrap())
    .execute(&pool)
    .await
    .unwrap();
    sqlx_core::query::query(
        "INSERT INTO scan_queue (user_did, status, kind, claim_id, enqueued_at, started_at, full_requested_at)
         VALUES ($1, 'running', 'full', 'claim-1', $2, $2, NULL)",
    )
    .bind(RUNNING)
    .bind(chrono::DateTime::parse_from_rfc3339("2026-09-10T00:00:00+00:00").unwrap())
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(
        db.enqueue_scan(QUEUED).await.unwrap(),
        EnqueueOutcome::AlreadyQueued
    );
    let r = pg_row(&db, QUEUED).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
    assert!(
        r.full_requested_at.is_some(),
        "the obligation is recorded even though nothing else changed"
    );
    assert_eq!(
        r.enqueued_at, "2026-09-10T00:00:00+00:00",
        "and the user keeps the place they already held"
    );

    assert_eq!(
        db.enqueue_scan(RUNNING).await.unwrap(),
        EnqueueOutcome::AlreadyRunning
    );
    let r = pg_row(&db, RUNNING).await;
    assert_eq!((r.status.as_str(), r.kind), ("running", ScanKind::Full));
    assert!(
        r.full_requested_at.is_some(),
        "the obligation is recorded; the running scan is left alone"
    );
    assert_eq!(r.started_at.as_deref(), Some("2026-09-10T00:00:00+00:00"));

    reset_scan_queue_fixtures(&url).await;
}

/// Postgres twin of
/// `unit_scan_kind::the_eta_median_survives_the_refresh_that_rewrites_the_queue_row`
/// (#344 F1): a fulfilled full scan records its duration in `scan_state`, and
/// the nightly refresh that reuses the very same queue row cannot erase it.
///
/// Sourced from the queue row, the second half of this went to `None` and
/// every queued user's ETA disappeared for good the first night the tick ran.
#[tokio::test]
async fn test_pg_eta_median_survives_the_refresh_that_rewrites_the_queue_row() {
    let _guard = scan_queue_test_lock().lock().await;

    const WAITER: &str = "did:plc:pgtest_q_kind_waiter";
    const SCANNER: &str = "did:plc:pgtest_q_kind_scanner";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.upsert_user(SCANNER, "q.bsky.social").await.unwrap();

    // A running full scan whose start is an hour before it fulfils, written
    // directly so the derived duration is a number this test can name.
    let pool = sqlx_core::pool::Pool::<sqlx_postgres::Postgres>::connect(&url)
        .await
        .unwrap();
    sqlx_core::query::query(
        "INSERT INTO scan_queue (user_did, status, kind, claim_id, enqueued_at, started_at)
         VALUES ($1, 'running', 'full', 'claim-1', $2, $2)",
    )
    .bind(SCANNER)
    .bind(chrono::DateTime::parse_from_rfc3339("2026-09-10T00:00:00+00:00").unwrap())
    .execute(&pool)
    .await
    .unwrap();

    // Production order: the marker while the row is still running, then the
    // row is finished.
    db.finish_full_scan_state(
        SCANNER,
        "2026-09-10T01:00:00+00:00",
        "full_carried_completion",
    )
    .await
    .unwrap();
    assert_eq!(
        db.get_scan_state(SCANNER, LAST_FULL_SCAN_DURATION_KEY)
            .await
            .unwrap()
            .as_deref(),
        Some("3600"),
        "one hour of full scan, in whole seconds"
    );
    assert!(db
        .finish_queued_scan(SCANNER, "claim-1", FinishCompletion::Complete, None)
        .await
        .unwrap());

    db.enqueue_scan(WAITER).await.unwrap();
    assert_eq!(
        db.scan_queue_entry(WAITER, 1)
            .await
            .unwrap()
            .unwrap()
            .eta_seconds,
        Some(3600),
        "the median is the recorded sample"
    );

    // The nightly refresh now takes over that user's one queue row.
    db.enqueue_refresh_scan(SCANNER).await.unwrap();
    let r = pg_row(&db, SCANNER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Refresh));
    assert_eq!(
        r.started_at, None,
        "the refresh really did wipe the timing the old median read"
    );
    assert_eq!(
        db.scan_queue_entry(WAITER, 1)
            .await
            .unwrap()
            .unwrap()
            .eta_seconds,
        Some(3600),
        "the sample lives in scan_state, so the refresh cannot take it away"
    );

    db.delete_user_data(SCANNER).await.unwrap();
    reset_scan_queue_fixtures(&url).await;
}

/// The negative control for the move (#344 F1) on Postgres: the queue row is
/// not a median source any more. Clean, done, `kind = 'full'` rows with real
/// durations and no `scan_state` sample must quote no ETA.
#[tokio::test]
async fn test_pg_eta_median_is_not_read_from_the_queue_row() {
    let _guard = scan_queue_test_lock().lock().await;

    const WAITER: &str = "did:plc:pgtest_q_kind_waiter2";
    let Some(url) = database_url() else {
        return;
    };
    reset_scan_queue_fixtures(&url).await;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let pool = sqlx_core::pool::Pool::<sqlx_postgres::Postgres>::connect(&url)
        .await
        .unwrap();
    for did in [
        "did:plc:pgtest_q_kind_clean1",
        "did:plc:pgtest_q_kind_clean2",
    ] {
        sqlx_core::query::query(
            "INSERT INTO scan_queue (user_did, status, kind, completion, enqueued_at, started_at, finished_at)
             VALUES ($1, 'done', 'full', 'complete', NOW(), NOW() - INTERVAL '3600 seconds', NOW())",
        )
        .bind(did)
        .execute(&pool)
        .await
        .unwrap();
    }

    db.enqueue_scan(WAITER).await.unwrap();
    let entry = db.scan_queue_entry(WAITER, 1).await.unwrap().unwrap();
    assert_eq!(
        entry.eta_seconds, None,
        "no recorded duration sample means no ETA, however many done rows there are"
    );

    reset_scan_queue_fixtures(&url).await;
}

/// V7-02: the cooldown marker and the carried drain outcome are written and
/// retired together, and the delete touches exactly one key.
#[tokio::test]
async fn test_pg_finish_full_scan_state_is_one_transaction() {
    const U: &str = "did:plc:pgtest_fullstate00000";
    const CARRIED: &str = "full_carried_completion";
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "fs.bsky.social").await.unwrap();
    db.set_scan_state(U, CARRIED, "whatever").await.unwrap();
    db.set_scan_state(U, "scan_phase", "done").await.unwrap();

    db.finish_full_scan_state(U, "2026-09-14T00:00:00+00:00", CARRIED)
        .await
        .unwrap();

    assert_eq!(
        db.get_scan_state(U, "last_full_scan_finished_at")
            .await
            .unwrap()
            .as_deref(),
        Some("2026-09-14T00:00:00+00:00")
    );
    assert_eq!(db.get_scan_state(U, CARRIED).await.unwrap(), None);
    assert_eq!(
        db.get_scan_state(U, "scan_phase").await.unwrap().as_deref(),
        Some("done"),
        "the delete is scoped to one key"
    );
    assert_eq!(
        db.get_scan_state(U, LAST_FULL_SCAN_DURATION_KEY)
            .await
            .unwrap(),
        None,
        "no queue row, so no started_at, so no ETA sample to invent (#344 F1)"
    );

    // Deleting an absent key is not an error — absence is the wanted state.
    db.delete_scan_state(U, CARRIED).await.unwrap();
    db.delete_user_data(U).await.unwrap();
}

/// #344 F2, the failure half of the one-transaction property on Postgres:
/// with a failure injected before the commit, neither the cooldown anchor nor
/// the duration sample may survive, and the carried key must still be there.
///
/// Without the transaction (or with a commit between the statements) the
/// marker is already durable when the failure lands and this goes red — which
/// is the point: the happy-path test above cannot tell a transaction from
/// three independent writes.
#[tokio::test]
async fn test_pg_a_failure_inside_finish_full_scan_state_rolls_back_every_write() {
    const U: &str = "did:plc:pgtest_fullstate_rb00";
    const CARRIED: &str = "full_carried_completion";
    let Some(url) = database_url() else {
        return;
    };
    // The concrete type, not `connect_postgres`'s `Arc<dyn Database>`: the
    // failure seam is an inherent method, not part of the trait.
    use charcoal::db::Database as _;
    let db = charcoal::db::postgres::PgDatabase::connect(&url)
        .await
        .unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "fs.bsky.social").await.unwrap();
    db.set_scan_state(U, CARRIED, "whatever").await.unwrap();

    // A running row, so the duration sample would be written too if the
    // transaction committed — both writes have to disappear.
    let pool = sqlx_core::pool::Pool::<sqlx_postgres::Postgres>::connect(&url)
        .await
        .unwrap();
    sqlx_core::query::query(
        "INSERT INTO scan_queue (user_did, status, kind, claim_id, enqueued_at, started_at)
         VALUES ($1, 'running', 'full', 'claim-1', $2, $2)",
    )
    .bind(U)
    .bind(chrono::DateTime::parse_from_rfc3339("2026-09-10T00:00:00+00:00").unwrap())
    .execute(&pool)
    .await
    .unwrap();

    let err = db
        .finish_full_scan_state_failing_for_test(U, "2026-09-10T01:00:00+00:00", CARRIED)
        .await
        .expect_err("the injected statement must fail");
    assert!(
        format!("{err:#}").contains("null value in column"),
        "the failure must be the injected NOT NULL violation: {err:#}"
    );

    assert_eq!(
        db.get_scan_state(U, "last_full_scan_finished_at")
            .await
            .unwrap(),
        None,
        "the cooldown anchor must not survive a failed transaction"
    );
    assert_eq!(
        db.get_scan_state(U, LAST_FULL_SCAN_DURATION_KEY)
            .await
            .unwrap(),
        None,
        "nor may the ETA sample"
    );
    assert_eq!(
        db.get_scan_state(U, CARRIED).await.unwrap().as_deref(),
        Some("whatever"),
        "and the carried drain outcome is still owed"
    );

    db.delete_user_data(U).await.unwrap();
}

/// The `ScanQueueRow` view of the new columns, on the Postgres mapper.
async fn pg_row(
    db: &std::sync::Arc<dyn charcoal::db::Database>,
    did: &str,
) -> charcoal::db::traits::ScanQueueRow {
    db.list_scan_queue()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.user_did == did)
        .expect("row exists")
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

/// v16 (#343): fresh connect creates the three cache tables and records 16.
#[tokio::test]
async fn test_pg_migration_v16_creates_cache_tables() {
    let _guard = cache_test_lock().lock().await;
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();

    let names: Vec<String> = sqlx_core::query::query(
        "SELECT table_name::text FROM information_schema.tables
         WHERE table_schema = 'public'
           AND table_name IN ('account_feed_snapshots','onnx_scores','classifier_verdicts')
         ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.get::<String, _>(0))
    .collect();
    assert_eq!(
        names,
        [
            "account_feed_snapshots",
            "classifier_verdicts",
            "onnx_scores"
        ]
    );

    let recorded: bool =
        sqlx_core::query::query("SELECT COUNT(*) > 0 FROM schema_version WHERE version = 16")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert!(recorded, "0016 must self-record its version");
}

/// The three timestamp indexes `evict_stale_cache` sweeps on (#343 / PR #118).
const PG_CACHE_INDEXES: [&str; 3] = [
    "idx_account_feed_snapshots_fetched_at",
    "idx_classifier_verdicts_classified_at",
    "idx_onnx_scores_scored_at",
];

/// v17 (#343, PR #118 review): the eviction indexes exist and 17 is recorded.
#[tokio::test]
async fn test_pg_migration_v17_creates_cache_indexes() {
    let _guard = cache_test_lock().lock().await;
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();

    let names: Vec<String> = sqlx_core::query::query(
        "SELECT indexname::text FROM pg_indexes
         WHERE schemaname = 'public'
           AND indexname IN ('idx_account_feed_snapshots_fetched_at',
                             'idx_onnx_scores_scored_at',
                             'idx_classifier_verdicts_classified_at')
         ORDER BY indexname",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.get::<String, _>(0))
    .collect();
    assert_eq!(names, PG_CACHE_INDEXES);

    let recorded: bool =
        sqlx_core::query::query("SELECT COUNT(*) > 0 FROM schema_version WHERE version = 17")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert!(recorded, "0017 must self-record its version");
}

/// Simulate a v16 database (indexes dropped, 17 unrecorded) and prove 0017
/// re-applies exactly once.
#[tokio::test]
async fn test_pg_migration_v17_upgrades_from_v16() {
    let _guard = cache_test_lock().lock().await;
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    sqlx_core::raw_sql::raw_sql(
        "DROP INDEX IF EXISTS idx_account_feed_snapshots_fetched_at;
         DROP INDEX IF EXISTS idx_onnx_scores_scored_at;
         DROP INDEX IF EXISTS idx_classifier_verdicts_classified_at;
         DELETE FROM schema_version WHERE version = 17;",
    )
    .execute(&pool)
    .await
    .unwrap();

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();

    let count: i64 = sqlx_core::query::query(
        "SELECT COUNT(*) FROM pg_indexes
         WHERE schemaname = 'public'
           AND indexname IN ('idx_account_feed_snapshots_fetched_at',
                             'idx_onnx_scores_scored_at',
                             'idx_classifier_verdicts_classified_at')",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(count, 3);
    let versions: i64 =
        sqlx_core::query::query("SELECT COUNT(*) FROM schema_version WHERE version = 17")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(versions, 1);
}

/// Simulate a v15 database (drop the v16 tables and version row), reconnect,
/// and prove the migration re-applies exactly once.
#[tokio::test]
async fn test_pg_migration_v16_upgrades_from_v15() {
    let _guard = cache_test_lock().lock().await;
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    // Two statements in one string: the simple prepared-statement path
    // (`query()`) rejects multi-command strings, so use raw_sql like the
    // migration runner itself does (src/db/postgres.rs).
    // v17 goes too: its indexes live on the v16 tables and vanish with them,
    // so leaving 17 recorded would skip re-creating them and strand the next
    // test (and a real v15 database has neither row anyway).
    sqlx_core::raw_sql::raw_sql(
        "DROP TABLE IF EXISTS account_feed_snapshots, onnx_scores, classifier_verdicts;
         DELETE FROM schema_version WHERE version IN (16, 17);",
    )
    .execute(&pool)
    .await
    .unwrap();

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();

    let count: i64 = sqlx_core::query::query(
        "SELECT COUNT(*) FROM information_schema.tables
         WHERE table_schema = 'public'
           AND table_name IN ('account_feed_snapshots','onnx_scores','classifier_verdicts')",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(count, 3);
    let versions: i64 =
        sqlx_core::query::query("SELECT COUNT(*) FROM schema_version WHERE version = 16")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(versions, 1);
}

/// Parity with tests/unit_feed_cache.rs against Postgres.
#[tokio::test]
async fn test_pg_shared_cache_round_trips() {
    let _guard = cache_test_lock().lock().await;
    let Some(url) = database_url() else {
        return;
    };
    use charcoal::db::{ClassifierVerdictRow, FeedSnapshot, OnnxScoreRow};
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let snap = FeedSnapshot {
        did: "did:plc:pgcache_snap000000000000".into(),
        handle: "cache.bsky.social".into(),
        posts_json: "[]".into(),
        fetched_at: "2026-09-08T00:00:00+00:00".into(),
        source: "bluesky".into(),
    };
    db.upsert_feed_snapshot(&snap).await.unwrap();
    let newer = FeedSnapshot {
        handle: "renamed.bsky.social".into(),
        ..snap.clone()
    };
    db.upsert_feed_snapshot(&newer).await.unwrap();
    assert_eq!(db.get_feed_snapshot(&snap.did).await.unwrap(), Some(newer));

    db.upsert_onnx_scores(
        "pgtest-model",
        &[
            OnnxScoreRow {
                text_sha256: "pgh1".into(),
                score: 0.1,
            },
            OnnxScoreRow {
                text_sha256: "pgh2".into(),
                score: 0.9,
            },
        ],
    )
    .await
    .unwrap();
    db.upsert_onnx_scores(
        "pgtest-model",
        &[OnnxScoreRow {
            text_sha256: "pgh2".into(),
            score: 0.8,
        }],
    )
    .await
    .unwrap();
    let got = db
        .get_onnx_scores(
            "pgtest-model",
            &["pgh1".into(), "pgh2".into(), "nope".into()],
        )
        .await
        .unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got["pgh2"], 0.8);
    assert!(db
        .get_onnx_scores("other-model", &["pgh1".into()])
        .await
        .unwrap()
        .is_empty());
    assert!(db
        .get_onnx_scores("pgtest-model", &[])
        .await
        .unwrap()
        .is_empty());

    let v = ClassifierVerdictRow {
        text_sha256: "pgh1".into(),
        toxic_token: true,
        confidence: 0.95,
    };
    db.upsert_classifier_verdicts("pgtest-clf", "v1", std::slice::from_ref(&v))
        .await
        .unwrap();
    let got = db
        .get_classifier_verdicts("pgtest-clf", "v1", &["pgh1".into(), "nope".into()])
        .await
        .unwrap();
    assert_eq!(got.get("pgh1"), Some(&v));
    assert!(db
        .get_classifier_verdicts("pgtest-clf", "v2", &["pgh1".into()])
        .await
        .unwrap()
        .is_empty());
}

/// Retention parity with tests/unit_feed_cache.rs (#343 / PR #118 review):
/// each cutoff applies to its own tables, and only rows past it go.
#[tokio::test]
async fn test_pg_evict_stale_cache() {
    let _guard = cache_test_lock().lock().await;
    let Some(url) = database_url() else {
        return;
    };
    use charcoal::db::{CacheEviction, ClassifierVerdictRow, FeedSnapshot, OnnxScoreRow};
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;

    const ANCIENT: &str = "2020-01-01T00:00:00+00:00";

    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    // Start from a known-empty cache: the counts below are exact, and this
    // suite's other cache tests leave rows behind. Safe because every cache
    // test seeds whatever it asserts on, and they all serialize on this lock.
    sqlx_core::raw_sql::raw_sql(
        "TRUNCATE account_feed_snapshots, onnx_scores, classifier_verdicts;",
    )
    .execute(&pool)
    .await
    .unwrap();

    let now = chrono::Utc::now();
    for (did, fetched_at) in [
        ("did:plc:pgevict_old", ANCIENT.to_string()),
        ("did:plc:pgevict_fresh", now.to_rfc3339()),
    ] {
        db.upsert_feed_snapshot(&FeedSnapshot {
            did: did.into(),
            handle: format!("{did}.bsky.social"),
            posts_json: "[]".into(),
            fetched_at,
            source: "bluesky".into(),
        })
        .await
        .unwrap();
    }
    db.upsert_onnx_scores(
        "evict-m",
        &[
            OnnxScoreRow {
                text_sha256: "old".into(),
                score: 0.1,
            },
            OnnxScoreRow {
                text_sha256: "fresh".into(),
                score: 0.2,
            },
        ],
    )
    .await
    .unwrap();
    db.upsert_classifier_verdicts(
        "evict-m",
        "v1",
        &[
            ClassifierVerdictRow {
                text_sha256: "old".into(),
                toxic_token: true,
                confidence: 0.9,
            },
            ClassifierVerdictRow {
                text_sha256: "fresh".into(),
                toxic_token: false,
                confidence: 0.1,
            },
        ],
    )
    .await
    .unwrap();
    // The upserts stamp scored_at/classified_at themselves — age the "old"
    // rows behind their back, the only way to test a backend-set timestamp.
    for sql in [
        "UPDATE onnx_scores SET scored_at = $1 WHERE text_sha256 = 'old'",
        "UPDATE classifier_verdicts SET classified_at = $1 WHERE text_sha256 = 'old'",
    ] {
        sqlx_core::query::query(sql)
            .bind(ANCIENT)
            .execute(&pool)
            .await
            .unwrap();
    }

    // A score cutoff that predates even the ancient rows must leave the
    // scoring tables alone while the feed cutoff still bites.
    let feed_only = db
        .evict_stale_cache(
            &(now - charcoal::db::cache_retention::FEED_SNAPSHOT_RETENTION).to_rfc3339(),
            "1999-01-01T00:00:00+00:00",
        )
        .await
        .unwrap();
    assert_eq!(
        feed_only,
        CacheEviction {
            feed_snapshots: 1,
            onnx_scores: 0,
            classifier_verdicts: 0,
        }
    );

    let evicted = db
        .evict_stale_cache(
            &(now - charcoal::db::cache_retention::FEED_SNAPSHOT_RETENTION).to_rfc3339(),
            &(now - charcoal::db::cache_retention::SCORE_RETENTION).to_rfc3339(),
        )
        .await
        .unwrap();
    assert_eq!(
        evicted,
        CacheEviction {
            feed_snapshots: 0,
            onnx_scores: 1,
            classifier_verdicts: 1,
        }
    );

    // Fresh rows survive, stale ones are gone, and a repeat sweep is a no-op.
    assert!(db
        .get_feed_snapshot("did:plc:pgevict_old")
        .await
        .unwrap()
        .is_none());
    assert!(db
        .get_feed_snapshot("did:plc:pgevict_fresh")
        .await
        .unwrap()
        .is_some());
    let scores = db
        .get_onnx_scores("evict-m", &["old".into(), "fresh".into()])
        .await
        .unwrap();
    assert_eq!(scores.len(), 1);
    assert_eq!(scores["fresh"], 0.2);
    let verdicts = db
        .get_classifier_verdicts("evict-m", "v1", &["old".into(), "fresh".into()])
        .await
        .unwrap();
    assert_eq!(verdicts.len(), 1);
    assert!(verdicts.contains_key("fresh"));

    assert_eq!(
        db.evict_stale_cache(
            &(now - charcoal::db::cache_retention::FEED_SNAPSHOT_RETENTION).to_rfc3339(),
            &(now - charcoal::db::cache_retention::SCORE_RETENTION).to_rfc3339(),
        )
        .await
        .unwrap(),
        CacheEviction::default()
    );
}

/// The UNNEST-batched cache upserts (CodeRabbit, PR #118): a multi-row batch
/// round-trips, a second upsert of the same keys updates in place, and
/// duplicate keys inside one batch collapse last-write-wins instead of
/// tripping "ON CONFLICT DO UPDATE command cannot affect row a second time".
#[tokio::test]
async fn test_pg_cache_batch_upserts_round_trip_and_update() {
    let _guard = cache_test_lock().lock().await;
    let Some(url) = database_url() else {
        return;
    };
    use charcoal::db::{ClassifierVerdictRow, OnnxScoreRow};
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let keys = ["batch1", "batch2", "batch3"];
    let scores: Vec<OnnxScoreRow> = keys
        .iter()
        .enumerate()
        .map(|(i, h)| OnnxScoreRow {
            text_sha256: (*h).into(),
            score: i as f64 / 10.0,
        })
        .collect();
    db.upsert_onnx_scores("batch-model", &scores).await.unwrap();
    let got = db
        .get_onnx_scores("batch-model", &keys.map(String::from))
        .await
        .unwrap();
    assert_eq!(got.len(), 3);
    assert_eq!(got["batch1"], 0.0);
    assert_eq!(got["batch3"], 0.2);

    // Same keys, new values: the ON CONFLICT path runs inside the UNNEST.
    let bumped: Vec<OnnxScoreRow> = keys
        .iter()
        .map(|h| OnnxScoreRow {
            text_sha256: (*h).into(),
            score: 0.75,
        })
        .collect();
    db.upsert_onnx_scores("batch-model", &bumped).await.unwrap();
    let got = db
        .get_onnx_scores("batch-model", &keys.map(String::from))
        .await
        .unwrap();
    assert_eq!(got.len(), 3);
    assert!(got.values().all(|v| *v == 0.75));

    // Duplicate keys in one batch: the last value wins, no error.
    db.upsert_onnx_scores(
        "batch-model",
        &[
            OnnxScoreRow {
                text_sha256: "batch1".into(),
                score: 0.1,
            },
            OnnxScoreRow {
                text_sha256: "batch1".into(),
                score: 0.6,
            },
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        db.get_onnx_scores("batch-model", &["batch1".to_string()])
            .await
            .unwrap()["batch1"],
        0.6
    );

    let verdicts: Vec<ClassifierVerdictRow> = keys
        .iter()
        .enumerate()
        .map(|(i, h)| ClassifierVerdictRow {
            text_sha256: (*h).into(),
            toxic_token: i % 2 == 0,
            confidence: 0.5,
        })
        .collect();
    db.upsert_classifier_verdicts("batch-clf", "v1", &verdicts)
        .await
        .unwrap();
    let got = db
        .get_classifier_verdicts("batch-clf", "v1", &keys.map(String::from))
        .await
        .unwrap();
    assert_eq!(got.len(), 3);
    assert_eq!(got["batch1"], verdicts[0]);
    assert_eq!(got["batch2"], verdicts[1]);

    // Flip every verdict through the conflict path.
    let flipped: Vec<ClassifierVerdictRow> = verdicts
        .iter()
        .map(|v| ClassifierVerdictRow {
            toxic_token: !v.toxic_token,
            confidence: 0.99,
            ..v.clone()
        })
        .collect();
    db.upsert_classifier_verdicts("batch-clf", "v1", &flipped)
        .await
        .unwrap();
    let got = db
        .get_classifier_verdicts("batch-clf", "v1", &keys.map(String::from))
        .await
        .unwrap();
    assert_eq!(got.len(), 3);
    for v in &flipped {
        assert_eq!(got[&v.text_sha256], *v);
    }

    // Duplicate keys in one verdict batch.
    db.upsert_classifier_verdicts(
        "batch-clf",
        "v1",
        &[
            ClassifierVerdictRow {
                text_sha256: "batch1".into(),
                toxic_token: true,
                confidence: 0.1,
            },
            ClassifierVerdictRow {
                text_sha256: "batch1".into(),
                toxic_token: false,
                confidence: 0.2,
            },
        ],
    )
    .await
    .unwrap();
    let got = db
        .get_classifier_verdicts("batch-clf", "v1", &["batch1".to_string()])
        .await
        .unwrap();
    assert!(!got["batch1"].toxic_token);
    assert_eq!(got["batch1"].confidence, 0.2);
}

/// v18 (#344): fresh connect adds expiry/generation, queue kind, refresh
/// schedule and embedding-model-id columns, the (user_did, threat_score)
/// index, and records 18. Runs against the ordinary `charcoal_test` database
/// — non-destructive, so no fixture lock is needed.
#[tokio::test]
async fn test_pg_migration_v18_creates_expiry_and_refresh_columns() {
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();

    for (table, col) in [
        ("account_scores", "scoring_generation"),
        ("account_scores", "valid_until"),
        ("scan_queue", "kind"),
        ("scan_queue", "full_requested_at"),
        ("scan_queue", "completion"),
        ("users", "next_refresh_at"),
        ("users", "refreshed_generation"),
        ("users", "refresh_attempted_generation"),
        ("topic_fingerprint", "embedding_model_id"),
    ] {
        let exists: bool = sqlx_core::query::query(
            "SELECT COUNT(*) > 0 FROM information_schema.columns
             WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2",
        )
        .bind(table)
        .bind(col)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
        assert!(exists, "{table}.{col}");
    }

    let has_index: bool = sqlx_core::query::query(
        "SELECT COUNT(*) > 0 FROM pg_indexes
         WHERE schemaname = 'public' AND indexname = 'idx_account_scores_user_score'",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert!(has_index);

    let recorded: bool =
        sqlx_core::query::query("SELECT COUNT(*) > 0 FROM schema_version WHERE version = 18")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert!(recorded, "0018 must self-record its version");
}

/// v18 upgrade, on the dedicated `_migrations` database (V3-06): an
/// AUTHENTIC v17 fixture (via `migrate_postgres_through`, not columns
/// reconstructed by dropping them off a current database — V2-07) with live
/// data, upgraded by a normal `connect_postgres` boot. Mirrors
/// `test_migration_v18_upgrades_a_v17_database` in `src/db/schema.rs`.
#[tokio::test]
async fn test_pg_migration_v18_upgrades_from_v17() {
    let Some(murl) = migrations_database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let _fixture = migrations_fixture(&murl, 17).await;
    let pool = Pool::<Postgres>::connect(&murl).await.unwrap();

    // Precondition: genuinely at v17, valid_until does not exist yet.
    // MAX(int4) stays int4 in Postgres (unlike COUNT(*), which is always
    // bigint) — cast explicitly so sqlx's i64 decode doesn't reject it.
    let max: i64 = sqlx_core::query::query("SELECT MAX(version)::bigint FROM schema_version")
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(max, 17, "fixture is genuinely at v17 before the upgrade");
    let has_valid_until: bool = sqlx_core::query::query(
        "SELECT COUNT(*) > 0 FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'account_scores' AND column_name = 'valid_until'",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert!(!has_valid_until);

    const V18_SCORED: &str = "did:plc:v18scored";
    const V18_UNSCORED: &str = "did:plc:v18unscored";
    const V18_WAITING: &str = "did:plc:v18waiting";
    const V18_ACCT: &str = "did:plc:v18acct";
    const V18_NA: &str = "did:plc:v18na";
    // pgvector's `vector(384)` column enforces the dimension on insert, so a
    // short literal like SQLite's `[0.1,0.2]` fixture would fail here.
    let vector_384 = format!("[{}]", vec!["0.1"; 384].join(","));

    sqlx_core::query::query(
        "INSERT INTO users (did, handle) VALUES ($1, 'scored.test'), ($2, 'unscored.test')",
    )
    .bind(V18_SCORED)
    .bind(V18_UNSCORED)
    .execute(&pool)
    .await
    .unwrap();
    sqlx_core::query::query(
        "INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at)
         VALUES ($1, $2, 'acct.test', 40.0, 'High', '2026-09-01T12:00:00+00:00'::timestamptz)",
    )
    .bind(V18_SCORED)
    .bind(V18_ACCT)
    .execute(&pool)
    .await
    .unwrap();
    sqlx_core::query::query(
        "INSERT INTO account_scores (user_did, did, handle, threat_tier, scored_at)
         VALUES ($1, $2, 'na.test', 'NotAssessed', '2026-09-02T12:00:00+00:00'::timestamptz)",
    )
    .bind(V18_SCORED)
    .bind(V18_NA)
    .execute(&pool)
    .await
    .unwrap();
    sqlx_core::query::query(
        "INSERT INTO scan_queue (user_did, status, enqueued_at, started_at, finished_at)
         VALUES ($1, 'done', '2026-09-01T11:00:00+00:00'::timestamptz,
                 '2026-09-01T11:00:00+00:00'::timestamptz, '2026-09-01T12:00:00+00:00'::timestamptz)",
    )
    .bind(V18_SCORED)
    .execute(&pool)
    .await
    .unwrap();
    sqlx_core::query::query(
        "INSERT INTO scan_queue (user_did, status, enqueued_at)
         VALUES ($1, 'queued', '2026-09-01T13:00:00+00:00'::timestamptz)",
    )
    .bind(V18_WAITING)
    .execute(&pool)
    .await
    .unwrap();
    sqlx_core::query::query(
        "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, embedding_vector)
         VALUES ($1, '{\"clusters\":[],\"post_count\":0}', 0, $2::vector)",
    )
    .bind(V18_SCORED)
    .bind(&vector_384)
    .execute(&pool)
    .await
    .unwrap();
    sqlx_core::query::query(
        "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, embedding_vector)
         VALUES ($1, '{\"clusters\":[],\"post_count\":0}', 0, NULL)",
    )
    .bind(V18_UNSCORED)
    .execute(&pool)
    .await
    .unwrap();

    // The upgrade itself — an ordinary boot against the migrations database.
    let _db = charcoal::db::connect_postgres(&murl).await.unwrap();

    let row = sqlx_core::query::query(
        "SELECT scoring_generation, valid_until = scored_at + INTERVAL '14 days'
         FROM account_scores WHERE user_did = $1 AND did = $2",
    )
    .bind(V18_SCORED)
    .bind(V18_ACCT)
    .fetch_one(&pool)
    .await
    .unwrap();
    let generation: String = row.get(0);
    let valid_matches_14d: bool = row.get(1);
    assert_eq!(generation, "legacy");
    assert!(valid_matches_14d, "scored_at + 14 days");

    let na_valid_matches_14d: bool = sqlx_core::query::query(
        "SELECT valid_until = scored_at + INTERVAL '14 days'
         FROM account_scores WHERE user_did = $1 AND did = $2",
    )
    .bind(V18_SCORED)
    .bind(V18_NA)
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert!(na_valid_matches_14d, "NULL-score rows are backfilled too");

    let is_nullable: String = sqlx_core::query::query(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_schema = 'public' AND table_name = 'account_scores' AND column_name = 'valid_until'",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(
        is_nullable, "NO",
        "Postgres valid_until is NOT NULL after backfill"
    );

    let queue_row = sqlx_core::query::query(
        "SELECT kind, completion, full_requested_at FROM scan_queue WHERE user_did = $1",
    )
    .bind(V18_SCORED)
    .fetch_one(&pool)
    .await
    .unwrap();
    let kind: String = queue_row.get(0);
    let completion: Option<String> = queue_row.get(1);
    let done_requested: Option<chrono::DateTime<chrono::Utc>> = queue_row.get(2);
    assert_eq!(kind, "full");
    assert!(completion.is_none());
    assert!(
        done_requested.is_none(),
        "a finished row owes nothing, so it keeps a NULL obligation"
    );

    // A row still in flight at deploy time IS an outstanding request, and
    // enqueued_at is when the user made it. Left NULL, the tick would never
    // retry an attempt interrupted across the deploy as full work.
    let waiting = sqlx_core::query::query(
        "SELECT status, full_requested_at = enqueued_at FROM scan_queue WHERE user_did = $1",
    )
    .bind(V18_WAITING)
    .fetch_one(&pool)
    .await
    .unwrap();
    let waiting_status: String = waiting.get(0);
    let waiting_backfilled: Option<bool> = waiting.get(1);
    assert_eq!(waiting_status, "queued");
    assert_eq!(
        waiting_backfilled,
        Some(true),
        "an in-flight row's obligation is backfilled from enqueued_at"
    );

    let user_row = sqlx_core::query::query(
        "SELECT next_refresh_at IS NULL, refreshed_generation IS NULL, refresh_attempted_generation IS NULL
         FROM users WHERE did = $1",
    )
    .bind(V18_SCORED)
    .fetch_one(&pool)
    .await
    .unwrap();
    let next_is_null: bool = user_row.get(0);
    let refreshed_is_null: bool = user_row.get(1);
    let attempted_is_null: bool = user_row.get(2);
    assert!(
        next_is_null && refreshed_is_null && attempted_is_null,
        "due-ness comes from the NULL attempted generation, not a stamped time"
    );

    let with_vec: Option<String> = sqlx_core::query::query(
        "SELECT embedding_model_id FROM topic_fingerprint WHERE user_did = $1",
    )
    .bind(V18_SCORED)
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(with_vec.as_deref(), Some("all-MiniLM-L6-v2"));
    let without_vec: Option<String> = sqlx_core::query::query(
        "SELECT embedding_model_id FROM topic_fingerprint WHERE user_did = $1",
    )
    .bind(V18_UNSCORED)
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert!(
        without_vec.is_none(),
        "keyword-only fingerprints have no embedding model"
    );

    let marker: String = sqlx_core::query::query(
        "SELECT value FROM scan_state WHERE user_did = $1 AND key = 'last_full_scan_finished_at'",
    )
    .bind(V18_SCORED)
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(
        marker, "2026-09-01T12:00:00+00:00",
        "cooldown anchor backfilled from the done row"
    );

    let has_index: bool = sqlx_core::query::query(
        "SELECT COUNT(*) > 0 FROM pg_indexes
         WHERE schemaname = 'public' AND indexname = 'idx_account_scores_user_score'",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert!(has_index);

    let version_count: i64 =
        sqlx_core::query::query("SELECT COUNT(*) FROM schema_version WHERE version = 18")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(version_count, 1);
}

/// V3-06: the destructive reset must refuse anything but the dedicated
/// `_migrations` database. No fixture/lock needed — the `bail!` happens
/// before any connection is opened.
#[tokio::test]
async fn test_migrate_postgres_through_refuses_a_non_migrations_database() {
    let Some(url) = database_url() else {
        return;
    };
    let result = charcoal::db::postgres::migrate_postgres_through(&url, 17).await;
    assert!(
        result.is_err(),
        "must refuse to reset a database whose name does not end in _migrations"
    );
}

/// V4-05: two `migrations_fixture` calls in one process never run
/// concurrently. Proven two ways: an active-guard counter never exceeds 1,
/// and each call's own sentinel row is the ONLY row present at its own
/// check-point — proof the other call's drop-all (whichever ran first)
/// completed, and the other call's own insert (if it ran second) had not
/// started yet, at the moment this call looked.
#[tokio::test]
async fn test_migrations_fixture_serializes_concurrent_calls() {
    let Some(murl) = migrations_database_url() else {
        return;
    };

    static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static MAX_ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    async fn hold_fixture(url: &str, marker: &str) -> i64 {
        use sqlx_core::pool::Pool;
        use sqlx_core::row::Row;
        use sqlx_postgres::Postgres;

        let _guard = migrations_fixture(url, 17).await;
        let now = ACTIVE.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        MAX_ACTIVE.fetch_max(now, std::sync::atomic::Ordering::SeqCst);

        let pool = Pool::<Postgres>::connect(url).await.unwrap();
        sqlx_core::query::query("INSERT INTO users (did, handle) VALUES ($1, 'concurrent.test')")
            .bind(marker)
            .execute(&pool)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        let count: i64 = sqlx_core::query::query(
            "SELECT COUNT(*) FROM users WHERE did LIKE 'did:plc:v18concurrent_%'",
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);

        ACTIVE.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        count
    }

    // `tokio::join!` rather than `tokio::spawn`: both futures run on this
    // task, interleaved at await points, which is enough to prove
    // serialization without fighting sqlx-core's `Executor` HRTB across a
    // spawned (`Send + 'static`) boundary.
    let (count_a, count_b) = tokio::join!(
        hold_fixture(&murl, "did:plc:v18concurrent_a"),
        hold_fixture(&murl, "did:plc:v18concurrent_b")
    );

    assert_eq!(
        MAX_ACTIVE.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "migrations_fixture must serialize — two guards were live at once"
    );
    assert_eq!(
        count_a, 1,
        "whichever call ran first sees only its own sentinel"
    );
    assert_eq!(
        count_b, 1,
        "the second call's drop-all wiped the first's sentinel before inserting its own"
    );
}

/// #344 R01/V2-06/V2-07: `charcoal migrate`'s real path — export_scores on an
/// authentic SQLite source (one row backfilled from a genuine v17 fixture,
/// the rest inserted directly at v18, matching `unit_score_export::seeded()`)
/// piped through import_score into Postgres. Destructive on the Postgres
/// side (drop-all via `migrate_postgres_through`), so this runs on the
/// dedicated `_migrations` database under the double lock (V3-06/V4-05),
/// not `charcoal_test`.
#[tokio::test]
async fn test_pg_migrate_from_sqlite_preserves_every_row() {
    use charcoal::db::schema::{create_tables, create_tables_through};
    use charcoal::db::sqlite::SqliteDatabase;
    use charcoal::scoring::generation::{scoring_revision, LEGACY_GENERATION};
    use rusqlite::{params, Connection};
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let Some(murl) = migrations_database_url() else {
        return;
    };
    const MIG_USER: &str = "did:plc:pgmig_user";

    // --- Build the SQLite source: v17 fixture for the legacy row, v18 for
    // the rest, exactly as unit_score_export::seeded() does. ---
    let conn = Connection::open_in_memory().unwrap();
    create_tables_through(&conn, 17).unwrap();
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at)
         VALUES (?1, 'did:plc:pgmig_leg', 'leg.h', 50.0, 'High', datetime('now', '-140 days'))",
        params![MIG_USER],
    )
    .unwrap();
    create_tables(&conn).unwrap(); // v18 boot: backfills the legacy row

    let rev = scoring_revision();
    let rows = [
        (
            "did:plc:pgmig_cur",
            "40.0",
            "'High'",
            "datetime('now', '-4 days')",
            "datetime('now', '+10 days')",
            rev,
        ),
        (
            "did:plc:pgmig_exp",
            "20.0",
            "'Elevated'",
            "datetime('now', '-13 days')",
            "datetime('now', '-6 days')",
            rev,
        ),
        (
            "did:plc:pgmig_na",
            "NULL",
            "'NotAssessed'",
            "datetime('now', '-12 days')",
            "datetime('now', '-5 days')",
            rev,
        ),
        (
            "did:plc:pgmig_nul",
            "30.0",
            "'Elevated'",
            "datetime('now', '-2 days')",
            "NULL",
            rev,
        ),
        (
            "did:plc:pgmig_bad",
            "35.0",
            "'High'",
            "datetime('now', '-2 days')",
            "'not a timestamp'",
            rev,
        ),
    ];
    for (did, score, tier, scored_at, valid_until, generation) in rows {
        conn.execute(
            &format!(
                "INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at, scoring_generation, valid_until)
                 VALUES (?1, ?2, ?3, {score}, {tier}, {scored_at}, ?4, {valid_until})"
            ),
            params![MIG_USER, did, format!("{did}.h"), generation],
        )
        .unwrap();
    }
    let src: std::sync::Arc<dyn charcoal::db::Database> =
        std::sync::Arc::new(SqliteDatabase::new(conn));

    // --- Destination: a clean v18 Postgres, via the destructive fixture. ---
    let _fixture = migrations_fixture(&murl, 18).await;
    let pg_db = charcoal::db::connect_postgres(&murl).await.unwrap();
    let pool = Pool::<Postgres>::connect(&murl).await.unwrap();

    // The sequence `charcoal migrate` runs: export every row, import every row.
    let exported = src.export_scores(MIG_USER).await.unwrap();
    assert_eq!(exported.len(), 6);
    for row in &exported {
        pg_db.import_score(MIG_USER, row).await.unwrap();
    }

    let count: i64 =
        sqlx_core::query::query("SELECT COUNT(*) FROM account_scores WHERE user_did = $1")
            .bind(MIG_USER)
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(count, 6);

    // Direct SQL against Postgres — not a re-export of the SQLite source —
    // so this actually checks what `import_score` wrote, not a tautology
    // about `exported`.
    for did in [
        "did:plc:pgmig_cur",
        "did:plc:pgmig_exp",
        "did:plc:pgmig_na",
        "did:plc:pgmig_nul",
        "did:plc:pgmig_bad",
    ] {
        let matches: bool = sqlx_core::query::query(
            "SELECT scoring_generation = $3 FROM account_scores WHERE user_did = $1 AND did = $2",
        )
        .bind(MIG_USER)
        .bind(did)
        .bind(rev)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
        assert!(
            matches,
            "{did}: migrated scoring_generation must equal the current revision, as read back from Postgres"
        );
    }
    let leg_row = sqlx_core::query::query(
        "SELECT scoring_generation FROM account_scores WHERE user_did = $1 AND did = 'did:plc:pgmig_leg'",
    )
    .bind(MIG_USER)
    .fetch_one(&pool)
    .await
    .unwrap();
    let leg_generation: String = leg_row.get(0);
    assert_eq!(leg_generation, LEGACY_GENERATION);

    let na_threat_score_is_null: bool = sqlx_core::query::query(
        "SELECT threat_score IS NULL FROM account_scores WHERE user_did = $1 AND did = 'did:plc:pgmig_na'",
    )
    .bind(MIG_USER)
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert!(
        na_threat_score_is_null,
        "NotAssessed row carries its NULL score verbatim"
    );

    for did in ["did:plc:pgmig_nul", "did:plc:pgmig_bad"] {
        let matches: bool = sqlx_core::query::query(
            "SELECT valid_until = scored_at FROM account_scores WHERE user_did = $1 AND did = $2",
        )
        .bind(MIG_USER)
        .bind(did)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
        assert!(
            matches,
            "{did}: NULL/malformed expiry imports as expired-when-scored"
        );
    }

    let leg_valid_matches_14d: bool = sqlx_core::query::query(
        "SELECT valid_until - scored_at = INTERVAL '14 days'
         FROM account_scores WHERE user_did = $1 AND did = 'did:plc:pgmig_leg'",
    )
    .bind(MIG_USER)
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert!(
        leg_valid_matches_14d,
        "legacy row's v18 backfill (scored_at + 14 d) survives migration"
    );

    // Every scored_at carried through unchanged: compare against the exact
    // instant the SQLite source exported (parsed from its own RFC3339 text),
    // not a re-formatted string — SQLite and Postgres render fractional
    // seconds differently (milliseconds vs microseconds) even for the same
    // instant.
    for row in &exported {
        let matches: bool = sqlx_core::query::query(
            "SELECT scored_at = $3::timestamptz FROM account_scores WHERE user_did = $1 AND did = $2",
        )
        .bind(MIG_USER)
        .bind(&row.score.did)
        .bind(&row.scored_at)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
        assert!(
            matches,
            "{}: scored_at must survive migration exactly",
            row.score.did
        );
    }

    assert_eq!(pg_db.count_expired(MIG_USER).await.unwrap(), 5);
    assert_eq!(
        pg_db.get_ranked_threats(MIG_USER, 0.0).await.unwrap().len(),
        1
    );

    // Re-import (as a second `charcoal migrate` run would): idempotent, no
    // row is renewed or changed.
    for row in &exported {
        pg_db.import_score(MIG_USER, row).await.unwrap();
    }
    for row in &exported {
        let matches: bool = sqlx_core::query::query(
            "SELECT scored_at = $3::timestamptz FROM account_scores WHERE user_did = $1 AND did = $2",
        )
        .bind(MIG_USER)
        .bind(&row.score.did)
        .bind(&row.scored_at)
        .fetch_one(&pool)
        .await
        .unwrap()
        .get(0);
        assert!(
            matches,
            "{}: re-import must not change scored_at",
            row.score.did
        );
    }
    assert_eq!(
        pg_db.count_expired(MIG_USER).await.unwrap(),
        5,
        "re-import must not change expiry counts"
    );
}

/// #344 V2-06: Postgres keeps microseconds through export/import — the
/// precision contract, positive direction.
#[tokio::test]
async fn test_pg_export_import_keeps_microseconds() {
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let Some(url) = database_url() else {
        return;
    };
    let src_did = "did:plc:pgmicro_src";
    let dst_did = "did:plc:pgmicro_dst";
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    for did in [src_did, dst_did] {
        sqlx_core::query::query("DELETE FROM account_scores WHERE user_did = $1 AND did = $2")
            .bind(TEST_USER)
            .bind(did)
            .execute(&pool)
            .await
            .unwrap();
    }

    sqlx_core::query::query(
        "INSERT INTO account_scores (user_did, did, handle, scoring_generation, scored_at, valid_until)
         VALUES ($1, $2, $3, 'legacy', '2026-09-01 12:00:00.123456+00'::timestamptz,
                 '2026-09-01 12:00:00.123456+00'::timestamptz + INTERVAL '14 days')",
    )
    .bind(TEST_USER)
    .bind(src_did)
    .bind(format!("{src_did}.handle"))
    .execute(&pool)
    .await
    .unwrap();

    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    let exported = db.export_scores(TEST_USER).await.unwrap();
    let src_row = exported
        .iter()
        .find(|r| r.score.did == src_did)
        .expect("source row exported");
    let mut dst_row = src_row.clone();
    dst_row.score.did = dst_did.to_string();
    dst_row.score.handle = format!("{dst_did}.handle");
    db.import_score(TEST_USER, &dst_row).await.unwrap();

    let row = sqlx_core::query::query(
        "SELECT scored_at = '2026-09-01 12:00:00.123456+00'::timestamptz,
                valid_until = '2026-09-01 12:00:00.123456+00'::timestamptz + INTERVAL '14 days'
         FROM account_scores WHERE user_did = $1 AND did = $2",
    )
    .bind(TEST_USER)
    .bind(dst_did)
    .fetch_one(&pool)
    .await
    .unwrap();
    let scored_matches: bool = row.get(0);
    let valid_matches: bool = row.get(1);
    assert!(
        scored_matches,
        "scored_at keeps microseconds through export/import"
    );
    assert!(
        valid_matches,
        "valid_until keeps microseconds through export/import"
    );

    for did in [src_did, dst_did] {
        sqlx_core::query::query("DELETE FROM account_scores WHERE user_did = $1 AND did = $2")
            .bind(TEST_USER)
            .bind(did)
            .execute(&pool)
            .await
            .unwrap();
    }
}

/// #344 V2-06: the documented one-way truncation — a Postgres source's
/// microseconds are truncated to whole seconds when imported into SQLite
/// (SQLite's `datetime()` column form), and only there.
#[tokio::test]
async fn test_pg_import_into_sqlite_truncates_to_seconds() {
    use charcoal::db::sqlite::SqliteDatabase;
    use rusqlite::Connection;
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;

    let Some(url) = database_url() else {
        return;
    };
    let src_did = "did:plc:pgtrunc_src";
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    sqlx_core::query::query("DELETE FROM account_scores WHERE user_did = $1 AND did = $2")
        .bind(TEST_USER)
        .bind(src_did)
        .execute(&pool)
        .await
        .unwrap();

    sqlx_core::query::query(
        "INSERT INTO account_scores (user_did, did, handle, scoring_generation, scored_at, valid_until)
         VALUES ($1, $2, $3, 'legacy', '2026-09-01 12:00:00.123456+00'::timestamptz,
                 '2026-09-01 12:00:00.123456+00'::timestamptz + INTERVAL '14 days')",
    )
    .bind(TEST_USER)
    .bind(src_did)
    .bind(format!("{src_did}.handle"))
    .execute(&pool)
    .await
    .unwrap();

    let pg_db = charcoal::db::connect_postgres(&url).await.unwrap();
    let exported = pg_db.export_scores(TEST_USER).await.unwrap();
    let row = exported
        .iter()
        .find(|r| r.score.did == src_did)
        .expect("source row exported")
        .clone();

    let sqlite_conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&sqlite_conn).unwrap();
    let sqlite_db: std::sync::Arc<dyn charcoal::db::Database> =
        std::sync::Arc::new(SqliteDatabase::new(sqlite_conn));
    sqlite_db.import_score(TEST_USER, &row).await.unwrap();

    let (stored_scored_at, stored_valid_until): (String, String) = {
        // Reach into the SqliteDatabase's connection isn't exposed, so
        // re-export and check the truncated value the same way the rest of
        // this suite verifies SQLite state — through the trait.
        let back = sqlite_db.export_scores(TEST_USER).await.unwrap();
        let r = back.iter().find(|r| r.score.did == src_did).unwrap();
        let charcoal::db::models::ExportedExpiry::At(valid_until) = &r.valid_until else {
            panic!("imported row must have a well-formed expiry");
        };
        (r.scored_at.clone(), valid_until.clone())
    };
    // SQLite's datetime() column form is whole seconds — the microsecond
    // fraction from the Postgres source is truncated on import, and
    // re-exporting renders that truncated value back out with a
    // millisecond field of all zeros.
    assert!(
        stored_scored_at.starts_with("2026-09-01T12:00:00.000"),
        "scored_at truncated to whole seconds: {stored_scored_at}"
    );
    assert!(
        stored_valid_until.starts_with("2026-09-15T12:00:00.000"),
        "valid_until truncated to whole seconds: {stored_valid_until}"
    );

    sqlx_core::query::query("DELETE FROM account_scores WHERE user_did = $1 AND did = $2")
        .bind(TEST_USER)
        .bind(src_did)
        .execute(&pool)
        .await
        .unwrap();
}
