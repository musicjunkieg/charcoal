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

use charcoal::db::models::AccountScore;
use charcoal::db::traits::NewAction;

fn score_fixture(did: &str, handle: &str, score: f64, tier: &str) -> AccountScore {
    AccountScore {
        did: did.to_string(),
        handle: handle.to_string(),
        toxicity_score: Some(0.5),
        topic_overlap: Some(0.3),
        overlap_legacy: None,
        threat_score: Some(score),
        threat_tier: Some(tier.to_string()),
        posts_analyzed: 10,
        top_toxic_posts: vec![],
        scored_at: "2026-09-01T12:00:00Z".to_string(),
        behavioral_signals: None,
        context_score: None,
        graph_distance: None,
        fingerprint_quality: None,
        scoring_confidence: None,
    }
}

fn new_action(target: &str, kind: &str) -> NewAction {
    NewAction {
        target_did: target.to_string(),
        kind: kind.to_string(),
        undo_of: None,
        score_at_action: Some(41.5),
        tier_at_action: Some("High".to_string()),
    }
}

#[tokio::test]
async fn create_batch_inserts_pending_rows_atomically() {
    let db = setup_db().await;
    let id = db
        .create_action_batch(
            DID,
            "mute",
            "tier:High",
            &[
                new_action("did:plc:a", "mute"),
                new_action("did:plc:b", "mute"),
            ],
        )
        .await
        .unwrap();
    let batch = db.get_action_batch(id).await.unwrap().unwrap();
    assert_eq!(batch.user_did, DID);
    assert_eq!(batch.kind, "mute");
    assert_eq!(batch.source, "tier:High");
    assert_eq!(batch.requested, 2);
    assert_eq!(batch.status, "queued");
    assert!(batch.started_at.is_none() && batch.finished_at.is_none());

    let rows = db.list_actions_for_batch(id).await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|r| r.status == "pending" && r.batch_id == id));
    assert_eq!(rows[0].target_did, "did:plc:a");
    assert_eq!(rows[0].score_at_action, Some(41.5));
    assert_eq!(rows[0].tier_at_action.as_deref(), Some("High"));
    assert!(rows[0].undo_of.is_none() && rows[0].record_uri.is_none());

    // Empty batch is still a batch (requested = 0), not an error.
    let empty = db
        .create_action_batch(DID, "block", "single", &[])
        .await
        .unwrap();
    assert_eq!(
        db.get_action_batch(empty).await.unwrap().unwrap().requested,
        0
    );
}

#[tokio::test]
async fn batch_status_transitions_stamp_timestamps() {
    let db = setup_db().await;
    let id = db
        .create_action_batch(DID, "block", "single", &[new_action("did:plc:a", "block")])
        .await
        .unwrap();
    db.set_action_batch_status(id, "running", None)
        .await
        .unwrap();
    let b = db.get_action_batch(id).await.unwrap().unwrap();
    assert_eq!(b.status, "running");
    let started = b.started_at.clone().expect("started_at set on running");
    assert!(b.finished_at.is_none());

    // Re-running (resume after restart) keeps the original started_at.
    db.set_action_batch_status(id, "running", None)
        .await
        .unwrap();
    assert_eq!(
        db.get_action_batch(id).await.unwrap().unwrap().started_at,
        Some(started)
    );

    db.set_action_batch_status(id, "failed", Some("not_connected"))
        .await
        .unwrap();
    let b = db.get_action_batch(id).await.unwrap().unwrap();
    assert_eq!(b.status, "failed");
    assert_eq!(b.error.as_deref(), Some("not_connected"));
    assert!(b.finished_at.is_some());

    // Back to queued clears the error (session reconnect path).
    db.set_action_batch_status(id, "queued", None)
        .await
        .unwrap();
    let b = db.get_action_batch(id).await.unwrap().unwrap();
    assert!(b.error.is_none());
    assert!(
        b.finished_at.is_none(),
        "a queued transition must clear finished_at, not just error"
    );
}

#[tokio::test]
async fn update_action_stamps_and_preserves_record_uri() {
    let db = setup_db().await;
    let id = db
        .create_action_batch(DID, "block", "single", &[new_action("did:plc:a", "block")])
        .await
        .unwrap();
    let row_id = db.list_actions_for_batch(id).await.unwrap()[0].id;

    db.update_action(
        row_id,
        "applied",
        Some("at://did:plc:me/app.bsky.graph.block/abc"),
        None,
    )
    .await
    .unwrap();
    let r = db.get_action(row_id).await.unwrap().unwrap();
    assert_eq!(r.status, "applied");
    assert!(r.applied_at.is_some() && r.undone_at.is_none());
    assert_eq!(
        r.record_uri.as_deref(),
        Some("at://did:plc:me/app.bsky.graph.block/abc")
    );

    // None record_uri must not erase the stored one.
    db.update_action(row_id, "undone", None, None)
        .await
        .unwrap();
    let r = db.get_action(row_id).await.unwrap().unwrap();
    assert_eq!(r.status, "undone");
    assert!(r.undone_at.is_some());
    assert!(
        r.applied_at.is_some(),
        "applied_at must survive the undone transition"
    );
    assert_eq!(
        r.record_uri.as_deref(),
        Some("at://did:plc:me/app.bsky.graph.block/abc")
    );

    db.update_action(row_id, "failed", None, Some("boom"))
        .await
        .unwrap();
    assert_eq!(
        db.get_action(row_id)
            .await
            .unwrap()
            .unwrap()
            .error
            .as_deref(),
        Some("boom")
    );
}

#[tokio::test]
async fn listing_and_active_and_unfinished() {
    let db = setup_db().await;
    let first = db
        .create_action_batch(DID, "mute", "tier:High", &[new_action("did:plc:a", "mute")])
        .await
        .unwrap();
    let second = db
        .create_action_batch(DID, "block", "single", &[new_action("did:plc:b", "block")])
        .await
        .unwrap();
    let other = db
        .create_action_batch(
            "did:plc:other",
            "mute",
            "single",
            &[new_action("did:plc:c", "mute")],
        )
        .await
        .unwrap();

    // Newest first, scoped to the user, paginated.
    let page = db.list_action_batches(DID, 10, 0).await.unwrap();
    assert_eq!(
        page.iter().map(|b| b.id).collect::<Vec<_>>(),
        vec![second, first]
    );
    assert_eq!(
        db.list_action_batches(DID, 1, 1).await.unwrap()[0].id,
        first
    );

    // Unfinished across all users, id ascending (boot resume).
    assert_eq!(
        db.list_unfinished_batches().await.unwrap(),
        vec![first, second, other]
    );
    db.set_action_batch_status(second, "done", None)
        .await
        .unwrap();
    assert_eq!(
        db.list_unfinished_batches().await.unwrap(),
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
    let active = db.active_actions(DID).await.unwrap();
    assert_eq!(active.iter().map(|r| r.id).collect::<Vec<_>>(), vec![a, b]);
    db.update_action(b, "undone", None, None).await.unwrap();
    assert_eq!(db.active_actions(DID).await.unwrap().len(), 1);
    assert!(db
        .active_actions("did:plc:nobody")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn undo_rows_point_at_originals() {
    let db = setup_db().await;
    let orig = db
        .create_action_batch(DID, "mute", "single", &[new_action("did:plc:a", "mute")])
        .await
        .unwrap();
    let orig_row = db.list_actions_for_batch(orig).await.unwrap()[0].id;
    let mut undo = new_action("did:plc:a", "mute");
    undo.undo_of = Some(orig_row);
    let undo_batch = db
        .create_action_batch(DID, "undo", &format!("undo:{orig}"), &[undo])
        .await
        .unwrap();
    let rows = db.list_actions_for_batch(undo_batch).await.unwrap();
    assert_eq!(rows[0].undo_of, Some(orig_row));
    assert_eq!(rows[0].kind, "mute");
}

#[tokio::test]
async fn score_snapshots_and_cascade() {
    let db = setup_db().await;
    db.upsert_user(DID, "actions.test").await.unwrap();
    db.upsert_account_score(DID, &score_fixture("did:plc:a", "a.test", 41.5, "High"))
        .await
        .unwrap();
    let snaps = db.list_score_snapshots(DID).await.unwrap();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].did, "did:plc:a");
    assert_eq!(snaps[0].handle, "a.test");
    assert_eq!(snaps[0].threat_tier.as_deref(), Some("High"));

    let id = db
        .create_action_batch(DID, "mute", "single", &[new_action("did:plc:a", "mute")])
        .await
        .unwrap();
    db.delete_user_data(DID).await.unwrap();
    assert!(db.get_action_batch(id).await.unwrap().is_none());
    // The `actions` rows hold target DIDs and there is no ON DELETE CASCADE
    // on `actions.batch_id`, so deleting the batch alone would leave them
    // behind — assert the rows themselves are gone, not just their parent.
    assert!(db.list_actions_for_batch(id).await.unwrap().is_empty());
    assert!(db.list_score_snapshots(DID).await.unwrap().is_empty());
}
