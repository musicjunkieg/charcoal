use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::pipeline::scan_phases::staging::{
    AccountInput, QueueRow, ScanPhase, VerdictRow, ACCOUNT_INPUT_SCHEMA_VERSION,
};
use rusqlite::Connection;

// ── helpers ───────────────────────────────────────────────────────────────────

const TEST_USER: &str = "did:plc:testuser000000000000";

async fn setup_db() -> SqliteDatabase {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    SqliteDatabase::new(conn)
}

fn make_queue_row(account_did: &str, post_uri: &str, status: &str) -> QueueRow {
    QueueRow {
        account_did: account_did.to_string(),
        post_uri: post_uri.to_string(),
        text: "hello world".to_string(),
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

// ── Database trait staging tests ──────────────────────────────────────────────

#[tokio::test]
async fn staging_enqueue_then_fetch_pending_honors_status() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    let pending = make_queue_row("did:plc:acct1", "at://did:plc:acct1/post/1", "pending");
    let done = make_queue_row("did:plc:acct1", "at://did:plc:acct1/post/2", "done");

    db.enqueue_classifications(TEST_USER, &[pending.clone(), done.clone()])
        .await
        .unwrap();

    let fetched = db
        .fetch_pending_classifications(TEST_USER, 100)
        .await
        .unwrap();
    assert_eq!(fetched.len(), 1, "only pending rows should be returned");
    assert_eq!(fetched[0].post_uri, "at://did:plc:acct1/post/1");
    assert_eq!(fetched[0].status, "pending");
}

#[tokio::test]
async fn staging_record_verdicts_flips_pending_to_done() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    let row = make_queue_row("did:plc:acct2", "at://did:plc:acct2/post/1", "pending");
    db.enqueue_classifications(TEST_USER, &[row]).await.unwrap();

    let verdict = VerdictRow {
        account_did: "did:plc:acct2".to_string(),
        post_uri: "at://did:plc:acct2/post/1".to_string(),
        toxic_token: true,
        confidence: 0.87,
        model_id: "cope-b-v1".to_string(),
        policy_version: "p1".to_string(),
    };
    db.record_classification_verdicts(TEST_USER, &[verdict])
        .await
        .unwrap();

    // Should no longer be pending
    let pending = db
        .fetch_pending_classifications(TEST_USER, 100)
        .await
        .unwrap();
    assert!(pending.is_empty(), "row should be flipped to done");

    // Verify via fetch_account_verdicts
    let all = db
        .fetch_account_verdicts(TEST_USER, "did:plc:acct2")
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].status, "done");
    assert_eq!(all[0].toxic_token, Some(true));
    assert!((all[0].confidence.unwrap() - 0.87).abs() < 1e-5);
    assert_eq!(all[0].model_id.as_deref(), Some("cope-b-v1"));
    assert_eq!(all[0].policy_version.as_deref(), Some("p1"));
}

#[tokio::test]
async fn staging_enqueue_upsert_same_pk_yields_one_row() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    let row = make_queue_row("did:plc:acct3", "at://did:plc:acct3/post/1", "pending");
    // Enqueue the same PK twice
    db.enqueue_classifications(TEST_USER, std::slice::from_ref(&row))
        .await
        .unwrap();
    db.enqueue_classifications(TEST_USER, &[row]).await.unwrap();

    let all = db
        .fetch_account_verdicts(TEST_USER, "did:plc:acct3")
        .await
        .unwrap();
    assert_eq!(
        all.len(),
        1,
        "UPSERT: same PK twice must yield exactly one row"
    );
}

#[tokio::test]
async fn staging_stash_and_fetch_account_input_roundtrip() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    // Nothing stashed yet
    let missing = db
        .fetch_account_input(TEST_USER, "did:plc:acct4")
        .await
        .unwrap();
    assert!(missing.is_none());

    let payload = r#"{"schema_version":1,"fingerprint_quality":"normal"}"#;
    db.stash_account_input(TEST_USER, "did:plc:acct4", payload)
        .await
        .unwrap();

    let fetched = db
        .fetch_account_input(TEST_USER, "did:plc:acct4")
        .await
        .unwrap();
    assert_eq!(fetched.as_deref(), Some(payload));

    // Re-stash replaces the blob
    let payload2 = r#"{"schema_version":1,"fingerprint_quality":"degraded"}"#;
    db.stash_account_input(TEST_USER, "did:plc:acct4", payload2)
        .await
        .unwrap();
    let fetched2 = db
        .fetch_account_input(TEST_USER, "did:plc:acct4")
        .await
        .unwrap();
    assert_eq!(fetched2.as_deref(), Some(payload2));
}

#[tokio::test]
async fn staging_count_pending_classifications() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    assert_eq!(
        db.count_pending_classifications(TEST_USER).await.unwrap(),
        0
    );

    let rows = vec![
        make_queue_row("did:plc:acct5", "at://did:plc:acct5/post/1", "pending"),
        make_queue_row("did:plc:acct5", "at://did:plc:acct5/post/2", "pending"),
        make_queue_row("did:plc:acct5", "at://did:plc:acct5/post/3", "done"),
    ];
    db.enqueue_classifications(TEST_USER, &rows).await.unwrap();

    assert_eq!(
        db.count_pending_classifications(TEST_USER).await.unwrap(),
        2
    );
}

#[tokio::test]
async fn staging_clear_scan_staging_empties_both_tables() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    let row = make_queue_row("did:plc:acct6", "at://did:plc:acct6/post/1", "pending");
    db.enqueue_classifications(TEST_USER, &[row]).await.unwrap();
    db.stash_account_input(TEST_USER, "did:plc:acct6", r#"{"schema_version":1}"#)
        .await
        .unwrap();

    // Set a scan_state marker that clear must NOT touch
    db.set_scan_state(TEST_USER, "scan_phase", "burst")
        .await
        .unwrap();

    db.clear_scan_staging(TEST_USER).await.unwrap();

    // Both tables should be empty for this user
    assert_eq!(
        db.count_pending_classifications(TEST_USER).await.unwrap(),
        0
    );
    let all = db
        .fetch_account_verdicts(TEST_USER, "did:plc:acct6")
        .await
        .unwrap();
    assert!(all.is_empty(), "classification_queue should be empty");

    let input = db
        .fetch_account_input(TEST_USER, "did:plc:acct6")
        .await
        .unwrap();
    assert!(input.is_none(), "scan_account_input should be empty");

    // scan_state must survive clear_scan_staging
    assert_eq!(
        db.get_scan_state(TEST_USER, "scan_phase").await.unwrap(),
        Some("burst".to_string()),
        "clear_scan_staging must not touch scan_state"
    );
}

#[tokio::test]
async fn staging_list_scan_accounts_returns_distinct_dids() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    let rows = vec![
        make_queue_row("did:plc:acct7", "at://did:plc:acct7/post/1", "pending"),
        make_queue_row("did:plc:acct7", "at://did:plc:acct7/post/2", "pending"),
        make_queue_row("did:plc:acct8", "at://did:plc:acct8/post/1", "pending"),
    ];
    db.enqueue_classifications(TEST_USER, &rows).await.unwrap();

    let mut accounts = db.list_scan_accounts(TEST_USER).await.unwrap();
    accounts.sort();
    assert_eq!(
        accounts,
        vec!["did:plc:acct7".to_string(), "did:plc:acct8".to_string()],
        "list_scan_accounts must return distinct account_dids"
    );
}

#[tokio::test]
async fn staging_phase_marker_via_existing_scan_state_api() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    // Initially absent
    assert!(db
        .get_scan_state(TEST_USER, "scan_phase")
        .await
        .unwrap()
        .is_none());

    // Set via existing API
    db.set_scan_state(TEST_USER, "scan_phase", "burst")
        .await
        .unwrap();
    assert_eq!(
        db.get_scan_state(TEST_USER, "scan_phase").await.unwrap(),
        Some("burst".to_string())
    );

    // Advance phase
    db.set_scan_state(TEST_USER, "scan_phase", "finalize")
        .await
        .unwrap();
    assert_eq!(
        db.get_scan_state(TEST_USER, "scan_phase").await.unwrap(),
        Some("finalize".to_string())
    );
}

#[tokio::test]
async fn staging_reenqueue_does_not_clobber_done_row() {
    let db = setup_db().await;
    db.upsert_user(TEST_USER, "testuser.bsky.social")
        .await
        .unwrap();

    // Phase A: enqueue a pending row
    let pending = make_queue_row("did:plc:acctX", "at://did:plc:acctX/post/1", "pending");
    db.enqueue_classifications(TEST_USER, &[pending])
        .await
        .unwrap();

    // Phase B: record a verdict — flips the row to done with a real verdict
    let verdict = VerdictRow {
        account_did: "did:plc:acctX".to_string(),
        post_uri: "at://did:plc:acctX/post/1".to_string(),
        toxic_token: true,
        confidence: 0.9,
        model_id: "m".to_string(),
        policy_version: "p".to_string(),
    };
    db.record_classification_verdicts(TEST_USER, &[verdict])
        .await
        .unwrap();

    // Phase A re-runs (e.g. after a crash/retry): enqueue the same post_uri as fresh pending
    let re_enqueue = make_queue_row("did:plc:acctX", "at://did:plc:acctX/post/1", "pending");
    db.enqueue_classifications(TEST_USER, &[re_enqueue])
        .await
        .unwrap();

    // The row must still be done with the original verdict intact
    let all = db
        .fetch_account_verdicts(TEST_USER, "did:plc:acctX")
        .await
        .unwrap();
    assert_eq!(all.len(), 1, "re-enqueue must not create a duplicate row");
    assert_eq!(
        all[0].status, "done",
        "re-enqueue must not reset status to pending"
    );
    assert_eq!(
        all[0].toxic_token,
        Some(true),
        "re-enqueue must not clear toxic_token"
    );
    assert!(
        (all[0].confidence.unwrap() - 0.9).abs() < 1e-5,
        "re-enqueue must not clear confidence"
    );
    assert_eq!(
        all[0].model_id.as_deref(),
        Some("m"),
        "re-enqueue must not clear model_id"
    );
    assert_eq!(
        all[0].policy_version.as_deref(),
        Some("p"),
        "re-enqueue must not clear policy_version"
    );
}

// --- Schema v9 migration tests ---

fn setup_v9_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    conn
}

#[test]
fn schema_v9_creates_classification_queue_and_scan_account_input() {
    let conn = setup_v9_db();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' \
             AND name IN ('classification_queue','scan_account_input')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 2,
        "both classification_queue and scan_account_input tables should exist after v9 migration"
    );
}

#[test]
fn schema_v9_classification_queue_has_expected_columns() {
    let conn = setup_v9_db();
    // Insert a row using all required columns — compile-time DDL check
    conn.execute(
        "INSERT INTO classification_queue \
         (user_did, account_did, post_uri, text, context_text, post_kind, onnx_score, status) \
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7)",
        rusqlite::params![
            "did:plc:user1",
            "did:plc:acct1",
            "at://did:plc:acct1/app.bsky.feed.post/abc",
            "hello world",
            "original",
            0.05_f64,
            "pending",
        ],
    )
    .unwrap();

    let status: String = conn
        .query_row(
            "SELECT status FROM classification_queue \
             WHERE user_did='did:plc:user1' AND account_did='did:plc:acct1' \
             AND post_uri='at://did:plc:acct1/app.bsky.feed.post/abc'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "pending");
}

#[test]
fn schema_v9_classification_queue_verdict_columns_roundtrip() {
    let conn = setup_v9_db();
    // Insert a row with nullable verdict columns left NULL
    conn.execute(
        "INSERT INTO classification_queue \
         (user_did, account_did, post_uri, text, context_text, post_kind, onnx_score, status) \
         VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7)",
        rusqlite::params![
            "did:plc:user2",
            "did:plc:acct2",
            "at://did:plc:acct2/app.bsky.feed.post/def",
            "test post",
            "original",
            0.10_f64,
            "pending",
        ],
    )
    .unwrap();

    // Update the row with verdict values
    conn.execute(
        "UPDATE classification_queue SET toxic_token=?1, confidence=?2, \
         model_id=?3, policy_version=?4 \
         WHERE user_did=?5 AND account_did=?6 AND post_uri=?7",
        rusqlite::params![
            1i64,     // toxic_token
            0.87_f64, // confidence
            "m1",     // model_id
            "p1",     // policy_version
            "did:plc:user2",
            "did:plc:acct2",
            "at://did:plc:acct2/app.bsky.feed.post/def",
        ],
    )
    .unwrap();

    // Select those four columns back and verify round-trip
    let (toxic_token, confidence, model_id, policy_version): (i64, f64, String, String) = conn
        .query_row(
            "SELECT toxic_token, confidence, model_id, policy_version FROM classification_queue \
             WHERE user_did=?1 AND account_did=?2 AND post_uri=?3",
            rusqlite::params![
                "did:plc:user2",
                "did:plc:acct2",
                "at://did:plc:acct2/app.bsky.feed.post/def",
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();

    assert_eq!(toxic_token, 1);
    assert!((confidence - 0.87_f64).abs() < 1e-6); // Float comparison with epsilon
    assert_eq!(model_id, "m1");
    assert_eq!(policy_version, "p1");
}

#[test]
fn schema_v9_scan_account_input_has_payload_json() {
    let conn = setup_v9_db();
    conn.execute(
        "INSERT INTO scan_account_input (user_did, account_did, payload_json) \
         VALUES (?1, ?2, ?3)",
        rusqlite::params!["did:plc:user1", "did:plc:acct1", r#"{"schema_version":1}"#,],
    )
    .unwrap();

    let payload: String = conn
        .query_row(
            "SELECT payload_json FROM scan_account_input \
             WHERE user_did='did:plc:user1' AND account_did='did:plc:acct1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(payload, r#"{"schema_version":1}"#);
}

#[test]
fn schema_v9_does_not_alter_scan_state() {
    let conn = setup_v9_db();
    // scan_state must still accept key/value rows — the phase marker is stored
    // as key='scan_phase', not as a column
    conn.execute(
        "INSERT INTO scan_state (user_did, key, value) VALUES (?1, ?2, ?3)",
        rusqlite::params!["did:plc:user1", "scan_phase", "gather"],
    )
    .unwrap();

    let val: String = conn
        .query_row(
            "SELECT value FROM scan_state WHERE user_did='did:plc:user1' AND key='scan_phase'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(val, "gather");
}

#[test]
fn scan_phase_roundtrips_through_str() {
    for p in [
        ScanPhase::Gather,
        ScanPhase::Burst,
        ScanPhase::Finalize,
        ScanPhase::Done,
    ] {
        assert_eq!(ScanPhase::from_value(p.as_str()), Some(p));
    }
    assert_eq!(ScanPhase::from_value("nonsense"), None);
}

#[test]
fn account_input_is_versioned_and_roundtrips() {
    let blob = AccountInput::new_for_test();
    let json = serde_json::to_string(&blob).unwrap();
    let back: AccountInput = serde_json::from_str(&json).unwrap();
    assert_eq!(back.schema_version, ACCOUNT_INPUT_SCHEMA_VERSION);
    assert_eq!(back, blob);
}
