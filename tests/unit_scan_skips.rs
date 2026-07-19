//! Tests for the durable scan-skip record (#226).
//!
//! Motivation: the #220 diagnosis had to reconstruct which accounts were
//! dropped, and why, by grepping Railway logs. That was expensive and — as
//! #226 showed when Railway reported "Messages dropped: 1156" — not even
//! reliable, since dropped lines are chosen by rate rather than severity, so
//! WARN skip warnings go over the side with the noise.
//!
//! A skipped account is a real gap in a scan's coverage. It deserves a row in
//! the database, not a log line that may or may not survive.

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::traits::Database;
use rusqlite::Connection;

const USER: &str = "did:plc:testuser000000000000";
const OTHER_USER: &str = "did:plc:otheruser0000000000";

async fn setup_db() -> SqliteDatabase {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    SqliteDatabase::new(conn)
}

#[tokio::test]
async fn records_and_counts_skips() {
    let db = setup_db().await;

    assert_eq!(db.count_scan_skips(USER).await.unwrap(), 0);

    db.record_scan_skip(
        USER,
        "did:plc:a",
        "gather",
        "ONNX inference failed: bad shape",
    )
    .await
    .unwrap();
    db.record_scan_skip(USER, "did:plc:b", "gather", "Failed to fetch feed")
        .await
        .unwrap();

    assert_eq!(db.count_scan_skips(USER).await.unwrap(), 2);
}

#[tokio::test]
async fn skips_are_scoped_per_user() {
    let db = setup_db().await;

    db.record_scan_skip(USER, "did:plc:a", "gather", "boom")
        .await
        .unwrap();
    db.record_scan_skip(OTHER_USER, "did:plc:a", "gather", "boom")
        .await
        .unwrap();

    // Same account_did for both users must not collide — this is a multi-user
    // system and one user's coverage gap is not another's.
    assert_eq!(db.count_scan_skips(USER).await.unwrap(), 1);
    assert_eq!(db.count_scan_skips(OTHER_USER).await.unwrap(), 1);
}

#[tokio::test]
async fn re_recording_the_same_account_and_phase_updates_rather_than_duplicates() {
    let db = setup_db().await;

    db.record_scan_skip(USER, "did:plc:a", "gather", "first failure")
        .await
        .unwrap();
    db.record_scan_skip(USER, "did:plc:a", "gather", "second failure")
        .await
        .unwrap();

    // A re-gather that fails again must not inflate the count — otherwise the
    // number stops meaning "accounts missing from this scan".
    assert_eq!(db.count_scan_skips(USER).await.unwrap(), 1);
}

#[tokio::test]
async fn the_same_account_can_be_skipped_in_different_phases() {
    let db = setup_db().await;

    db.record_scan_skip(USER, "did:plc:a", "gather", "fetch failed")
        .await
        .unwrap();
    db.record_scan_skip(USER, "did:plc:a", "finalize", "blob decode failed")
        .await
        .unwrap();

    // Distinct failures at distinct stages are distinct facts.
    assert_eq!(db.count_scan_skips(USER).await.unwrap(), 2);
}

#[tokio::test]
async fn clearing_is_per_user_and_resets_the_count() {
    let db = setup_db().await;

    db.record_scan_skip(USER, "did:plc:a", "gather", "boom")
        .await
        .unwrap();
    db.record_scan_skip(OTHER_USER, "did:plc:b", "gather", "boom")
        .await
        .unwrap();

    db.clear_scan_skips(USER).await.unwrap();

    // Cleared at the start of each scan so the count always describes the
    // CURRENT run, not an accumulation across every scan ever.
    assert_eq!(db.count_scan_skips(USER).await.unwrap(), 0);
    assert_eq!(
        db.count_scan_skips(OTHER_USER).await.unwrap(),
        1,
        "clearing one user's skips must not touch another's"
    );
}

#[tokio::test]
async fn preserves_the_full_error_chain() {
    let db = setup_db().await;

    // The whole point of #223/#226: the anyhow source chain is what makes a
    // skip diagnosable. "ONNX inference failed" alone cost hours on #220.
    let chain = "ONNX inference failed: Non-zero status code returned while \
                 running Expand node. Name:'/roberta/Expand' Status Message: \
                 invalid expand shape";
    db.record_scan_skip(USER, "did:plc:a", "gather", chain)
        .await
        .unwrap();

    let skips = db.list_scan_skips(USER).await.unwrap();
    assert_eq!(skips.len(), 1);
    assert_eq!(skips[0].account_did, "did:plc:a");
    assert_eq!(skips[0].phase, "gather");
    assert_eq!(
        skips[0].error, chain,
        "the error chain must be stored verbatim — truncating it recreates \
         exactly the problem this table exists to solve"
    );
}
