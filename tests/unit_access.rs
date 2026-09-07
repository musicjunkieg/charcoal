//! access_requests state machine at the Database-trait level (#309).
#![cfg(feature = "web")]

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use rusqlite::Connection;

const DID: &str = "did:plc:accesstest00000000000000";

async fn setup_db() -> SqliteDatabase {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    SqliteDatabase::new(conn)
}

#[tokio::test]
async fn pending_upsert_creates_then_only_refreshes_handle() {
    let db = setup_db().await;
    db.upsert_access_request_pending(DID, "old.bsky.social")
        .await
        .unwrap();
    let row = db.get_access_request(DID).await.unwrap().unwrap();
    assert_eq!(row.status, "pending");
    assert_eq!(row.handle, "old.bsky.social");
    assert!(row.decided_at.is_none());

    // Deny, then sign in again with a new handle: status must NOT reset.
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
}

#[tokio::test]
async fn set_status_records_decision_and_reports_missing_rows() {
    let db = setup_db().await;
    assert!(!db
        .set_access_status(DID, "allowed", "did:plc:admin")
        .await
        .unwrap());
    db.upsert_access_request_pending(DID, "w.bsky.social")
        .await
        .unwrap();
    assert!(db
        .set_access_status(DID, "allowed", "did:plc:admin")
        .await
        .unwrap());
    let row = db.get_access_request(DID).await.unwrap().unwrap();
    assert_eq!(row.status, "allowed");
    assert_eq!(row.decided_by.as_deref(), Some("did:plc:admin"));
    assert!(row.decided_at.is_some());
    // Idempotent repeat is a success, not an error.
    assert!(db
        .set_access_status(DID, "allowed", "did:plc:admin")
        .await
        .unwrap());
}

#[tokio::test]
async fn grant_access_upserts_allowed_with_and_without_prior_row() {
    let db = setup_db().await;
    db.grant_access(DID, "granted.bsky.social", "did:plc:admin")
        .await
        .unwrap();
    let row = db.get_access_request(DID).await.unwrap().unwrap();
    assert_eq!(row.status, "allowed");
    // Re-grant over a denied row flips it back to allowed.
    db.set_access_status(DID, "denied", "did:plc:admin")
        .await
        .unwrap();
    db.grant_access(DID, "granted.bsky.social", "did:plc:admin")
        .await
        .unwrap();
    assert_eq!(
        db.get_access_request(DID).await.unwrap().unwrap().status,
        "allowed"
    );
}

#[tokio::test]
async fn list_returns_oldest_first() {
    let db = setup_db().await;
    db.upsert_access_request_pending("did:plc:first000000000000000000", "a.bsky.social")
        .await
        .unwrap();
    db.upsert_access_request_pending("did:plc:second00000000000000000", "b.bsky.social")
        .await
        .unwrap();
    let rows = db.list_access_requests().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].requested_at <= rows[1].requested_at);
}
