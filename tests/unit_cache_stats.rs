use std::sync::Arc;

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::observability::cache_stats::{record_cache_stats, CacheStats};
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

#[test]
fn counters_start_at_zero_and_accumulate() {
    let s = CacheStats::default();
    assert_eq!((s.hits(), s.misses()), (0, 0));
    s.hit(3);
    s.miss(1);
    s.hit(2);
    assert_eq!((s.hits(), s.misses()), (5, 1));
}

#[tokio::test]
async fn record_writes_prefixed_scan_state_keys() {
    let db = setup_db();
    let s = CacheStats::default();
    s.hit(7);
    s.miss(2);
    record_cache_stats(db.as_ref(), "did:plc:u", "feed", &s)
        .await
        .unwrap();
    assert_eq!(
        db.get_scan_state("did:plc:u", "feed_cache_hits")
            .await
            .unwrap()
            .as_deref(),
        Some("7")
    );
    assert_eq!(
        db.get_scan_state("did:plc:u", "feed_cache_misses")
            .await
            .unwrap()
            .as_deref(),
        Some("2")
    );
}
