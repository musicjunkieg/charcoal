// #344 Task 8 / R05: the amplifier direct-pair loader. "No pairs" and "could
// not read pairs" are different answers, and only the first is Ok.

use std::sync::Arc;

use charcoal::db::schema::create_tables;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::pipeline::amplification::direct_pairs_for;
use rusqlite::Connection;

const USER: &str = "did:plc:pairsuser000000000000000";

fn db_with_events() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

#[tokio::test]
async fn pairs_are_deduplicated_and_empty_text_is_skipped() {
    let db = db_with_events();
    for (orig, amp) in [
        (Some("hello"), Some("lol no")),
        (Some("hello"), Some("lol no")), // duplicate event
        (Some("hello"), Some("")),       // empty commentary: not a pair
        (None, Some("orphan")),          // no original text: not a pair
        (Some("second post"), Some("also bad")),
    ] {
        db.insert_amplification_event(
            USER,
            "quote",
            "did:plc:amp",
            "amp.handle",
            "at://did:plc:me/app.bsky.feed.post/1",
            Some("at://did:plc:amp/app.bsky.feed.post/x"),
            amp,
            orig,
            None,
        )
        .await
        .unwrap();
    }
    let pairs = direct_pairs_for(&db, USER, "did:plc:amp").await.unwrap();
    // `get_events_by_amplifier` orders by `detected_at DESC, id DESC` and
    // `detected_at` has whole-second resolution, so the row order these five
    // same-second inserts come back in is the clock's business, not this
    // helper's. Assert the CONTENT — duplicates collapsed, text-less and
    // empty-text events skipped — over a sorted copy so the assertion cannot
    // go flaky on a second boundary.
    let mut sorted = pairs.clone();
    sorted.sort();
    assert_eq!(
        sorted,
        vec![
            ("hello".to_string(), "lol no".to_string()),
            ("second post".to_string(), "also bad".to_string())
        ]
    );
    assert_eq!(
        pairs.len(),
        2,
        "the duplicate event collapses and the text-less events are skipped"
    );
    assert!(
        direct_pairs_for(&db, USER, "did:plc:nobody")
            .await
            .unwrap()
            .is_empty(),
        "a successful lookup with no events is Ok(empty)"
    );
}

/// A read failure is an Err, not an empty list. Simulated by dropping the
/// events table out from under the query.
#[tokio::test]
async fn a_read_failure_is_an_error_not_an_empty_list() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute_batch("DROP TABLE amplification_events;")
        .unwrap();
    let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));
    assert!(direct_pairs_for(&db, USER, "did:plc:amp").await.is_err());
}
