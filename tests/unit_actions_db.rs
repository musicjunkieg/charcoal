//! oauth_sessions / action_batches / actions at the Database-trait level
//! (#315). SQLite here; `tests/db_postgres.rs` mirrors every assertion.
#![cfg(feature = "web")]

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::traits::OauthSessionRow;
use charcoal::db::Database;
use rusqlite::Connection;

const DID: &str = "did:plc:actionstest0000000000000";

async fn setup_db() -> SqliteDatabase {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    SqliteDatabase::new(conn)
}

fn session_row(updated_at: &str) -> OauthSessionRow {
    OauthSessionRow {
        user_did: DID.to_string(),
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

#[tokio::test]
async fn oauth_session_roundtrip_and_upsert_preserves_created_at() {
    let db = setup_db().await;
    assert!(db.get_oauth_session(DID).await.unwrap().is_none());

    db.upsert_oauth_session(&session_row("2026-09-01T00:00:00+00:00"))
        .await
        .unwrap();
    let got = db.get_oauth_session(DID).await.unwrap().unwrap();
    assert_eq!(got, session_row("2026-09-01T00:00:00+00:00"));

    // Re-consent: every column replaced except created_at.
    let mut second = session_row("2026-09-02T00:00:00+00:00");
    second.created_at = "2026-09-02T00:00:00+00:00".to_string();
    second.access_token_enc = vec![9, 9, 9];
    second.scope = "atproto".to_string();
    db.upsert_oauth_session(&second).await.unwrap();
    let got = db.get_oauth_session(DID).await.unwrap().unwrap();
    assert_eq!(got.created_at, "2026-09-01T00:00:00+00:00");
    assert_eq!(got.updated_at, "2026-09-02T00:00:00+00:00");
    assert_eq!(got.access_token_enc, vec![9, 9, 9]);
    assert_eq!(got.scope, "atproto");
}

#[tokio::test]
async fn update_oauth_tokens_is_compare_and_swap() {
    let db = setup_db().await;
    db.upsert_oauth_session(&session_row("t1")).await.unwrap();

    // Stale expectation: nothing written.
    let ok = db
        .update_oauth_tokens(DID, &[10], &[11], 2_000_000_000, "t0", "t2")
        .await
        .unwrap();
    assert!(!ok);
    let got = db.get_oauth_session(DID).await.unwrap().unwrap();
    assert_eq!(got.access_token_enc, vec![1, 2, 3]);
    assert_eq!(got.updated_at, "t1");

    // Matching expectation: written, updated_at advanced.
    let ok = db
        .update_oauth_tokens(DID, &[10], &[11], 2_000_000_000, "t1", "t2")
        .await
        .unwrap();
    assert!(ok);
    let got = db.get_oauth_session(DID).await.unwrap().unwrap();
    assert_eq!(got.access_token_enc, vec![10]);
    assert_eq!(got.refresh_token_enc, vec![11]);
    assert_eq!(got.access_expires_at, 2_000_000_000);
    assert_eq!(got.updated_at, "t2");
    assert_eq!(
        got.dpop_key_enc,
        vec![7, 8, 9],
        "dpop key untouched by refresh"
    );

    // Missing row: false, no error.
    assert!(!db
        .update_oauth_tokens("did:plc:nobody", &[1], &[2], 0, "t2", "t3")
        .await
        .unwrap());
}

#[tokio::test]
async fn delete_oauth_session_reports_presence() {
    let db = setup_db().await;
    assert!(!db.delete_oauth_session(DID).await.unwrap());
    db.upsert_oauth_session(&session_row("t1")).await.unwrap();
    assert!(db.delete_oauth_session(DID).await.unwrap());
    assert!(db.get_oauth_session(DID).await.unwrap().is_none());
}

#[tokio::test]
async fn delete_user_data_cascades_oauth_session() {
    let db = setup_db().await;
    db.upsert_user(DID, "actions.test").await.unwrap();
    db.upsert_oauth_session(&session_row("t1")).await.unwrap();
    db.delete_user_data(DID).await.unwrap();
    assert!(db.get_oauth_session(DID).await.unwrap().is_none());
}
