//! scan_costs ledger (#393): insert, per-user aggregates, windows, deletion.
use std::sync::Arc;

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::{Database, ScanCostRow};
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

fn row(did: &str, kind: &str, started: &str, cents: i64) -> ScanCostRow {
    ScanCostRow {
        user_did: did.into(),
        kind: kind.into(),
        outcome: "completed".into(),
        started_at: started.into(),
        finished_at: started.into(),
        classifications: 10,
        classifier_cache_hits: 2,
        metered_worker_secs: 1.5,
        peak_in_flight: 1,
        est_cents: cents,
        rate_cents_per_hour: 329,
    }
}

#[tokio::test]
async fn aggregates_first_full_scan_refreshes_and_total_per_user() {
    let db = setup_db();
    db.insert_scan_cost(&row("did:plc:a", "full", "2026-10-01T00:00:00Z", 150))
        .await
        .unwrap();
    db.insert_scan_cost(&row("did:plc:a", "full", "2026-10-05T00:00:00Z", 40))
        .await
        .unwrap();
    db.insert_scan_cost(&row("did:plc:a", "refresh", "2026-10-02T00:00:00Z", 7))
        .await
        .unwrap();
    db.insert_scan_cost(&row("did:plc:b", "refresh", "2026-10-03T00:00:00Z", 3))
        .await
        .unwrap();

    let users = db.list_user_scan_costs().await.unwrap();
    assert_eq!(users.len(), 2);
    let a = &users[0]; // highest total first
    assert_eq!(a.user_did, "did:plc:a");
    assert_eq!(a.first_full_scan_cents, Some(150), "the EARLIEST full scan");
    assert_eq!(a.refresh_cents, 7);
    assert_eq!(a.total_cents, 197);
    assert_eq!(a.runs, 3);
    assert_eq!(a.last_finished_at, "2026-10-05T00:00:00Z");
    assert_eq!(
        users[1].first_full_scan_cents, None,
        "b never had a full scan"
    );
}

#[tokio::test]
async fn window_sums_only_rows_started_on_or_after_since() {
    let db = setup_db();
    db.insert_scan_cost(&row("did:plc:a", "full", "2026-10-01T00:00:00Z", 100))
        .await
        .unwrap();
    db.insert_scan_cost(&row("did:plc:a", "refresh", "2026-10-20T00:00:00Z", 5))
        .await
        .unwrap();
    assert_eq!(
        db.scan_cost_cents_since("2026-10-20T00:00:00Z")
            .await
            .unwrap(),
        5
    );
    assert_eq!(
        db.scan_cost_cents_since("2026-09-01T00:00:00Z")
            .await
            .unwrap(),
        105
    );
    assert_eq!(
        db.scan_cost_cents_since("2026-11-01T00:00:00Z")
            .await
            .unwrap(),
        0,
        "empty window is 0, not NULL"
    );
}

#[tokio::test]
async fn delete_user_data_removes_that_users_cost_rows_only() {
    let db = setup_db();
    db.insert_scan_cost(&row("did:plc:a", "full", "2026-10-01T00:00:00Z", 100))
        .await
        .unwrap();
    db.insert_scan_cost(&row("did:plc:b", "full", "2026-10-01T00:00:00Z", 50))
        .await
        .unwrap();
    db.delete_user_data("did:plc:a").await.unwrap();
    let users = db.list_user_scan_costs().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].user_did, "did:plc:b");
}
