//! record_scan_cost (#393): one row per run, failures recorded, never fatal.
use std::sync::Arc;

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::{Database, ScanKind};
use charcoal::observability::cache_stats::CacheStats;
use charcoal::observability::scan_cost::{build_scan_cost_row, record_scan_cost, ScanCostOutcome};
use charcoal::toxicity::cost_meter::ScanCostMeter;
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

#[test]
fn row_carries_meter_and_cache_counts() {
    let m = ScanCostMeter::new(500, 329);
    m.force_worker_seconds(3600.0);
    let stats = CacheStats::default();
    stats.miss(40);
    stats.hit(12);
    let r = build_scan_cost_row(
        "did:plc:a",
        ScanKind::Refresh,
        ScanCostOutcome::Failed,
        "2026-10-24T00:00:00Z",
        "2026-10-24T00:01:00Z",
        m.snapshot(),
        &stats,
    );
    assert_eq!(r.kind, "refresh");
    assert_eq!(r.outcome, "failed");
    assert_eq!((r.classifications, r.classifier_cache_hits), (40, 12));
    assert_eq!((r.est_cents, r.rate_cents_per_hour), (329, 329));
}

#[tokio::test]
async fn a_run_writes_exactly_one_row() {
    let db = setup_db();
    let m = ScanCostMeter::new(500, 329);
    record_scan_cost(
        db.as_ref(),
        "did:plc:a",
        ScanKind::Full,
        ScanCostOutcome::Completed,
        "2026-10-24T00:00:00Z",
        &m,
        &CacheStats::default(),
    )
    .await;
    let users = db.list_user_scan_costs().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].runs, 1);
    assert_eq!(
        users[0].full_scan_cents,
        Some(0),
        "an all-cached scan costs 0"
    );
}

#[tokio::test]
async fn a_db_failure_is_swallowed() {
    let conn = Connection::open_in_memory().unwrap(); // no tables: insert must fail
    let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));
    let m = ScanCostMeter::new(500, 329);
    // Prove the failure path is real: without this, a DB that silently
    // accepted the insert would let the test pass without testing anything.
    let row = build_scan_cost_row(
        "did:plc:a",
        ScanKind::Full,
        ScanCostOutcome::Completed,
        "2026-10-24T00:00:00Z",
        "2026-10-24T00:01:00Z",
        m.snapshot(),
        &CacheStats::default(),
    );
    assert!(
        db.insert_scan_cost(&row).await.is_err(),
        "table-less DB must reject the insert"
    );
    // Must return normally — the cost row is bookkeeping, never the scan's result.
    record_scan_cost(
        db.as_ref(),
        "did:plc:a",
        ScanKind::Full,
        ScanCostOutcome::Completed,
        "2026-10-24T00:00:00Z",
        &m,
        &CacheStats::default(),
    )
    .await;
}

#[tokio::test]
async fn a_paused_row_round_trips() {
    // `paused`: returned Ok but did not finish (cost ceiling or interruption).
    // Inserting it proves the SQLite CHECK constraint accepts the value.
    let db = setup_db();
    let m = ScanCostMeter::new(500, 329);
    let row = build_scan_cost_row(
        "did:plc:a",
        ScanKind::Full,
        ScanCostOutcome::Paused,
        "2026-10-24T00:00:00Z",
        "2026-10-24T00:01:00Z",
        m.snapshot(),
        &CacheStats::default(),
    );
    assert_eq!(row.outcome, "paused");
    db.insert_scan_cost(&row)
        .await
        .expect("the CHECK constraint must accept 'paused'");
    let users = db.list_user_scan_costs().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].runs, 1);
}

#[test]
fn outcome_strings_are_stable() {
    assert_eq!(ScanCostOutcome::Completed.as_str(), "completed");
    assert_eq!(ScanCostOutcome::Paused.as_str(), "paused");
    assert_eq!(ScanCostOutcome::Failed.as_str(), "failed");
}
