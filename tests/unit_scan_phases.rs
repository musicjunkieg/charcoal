use charcoal::pipeline::scan_phases::staging::{
    AccountInput, ScanPhase, ACCOUNT_INPUT_SCHEMA_VERSION,
};

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
