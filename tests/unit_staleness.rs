// Score freshness (#213 Task 5, redefined by #344 / #343 §4.4).
//
// FRESH = scoring_generation == scoring_revision() AND valid_until is a
// well-formed timestamp in the future. The predicate is boolean-explicit on
// SQLite (COALESCE(..., 0)) so a NULL or malformed valid_until is expired —
// hidden, stale, counted — rather than SQL-unknown (R11). These tests pin
// that the bulk set equals the complement of is_score_stale and that the
// write path stamps both columns from the confidence tier.

use charcoal::db::models::{AccountScore, ScoringConfidence};
use charcoal::db::queries::{
    count_expired, count_not_assessed, get_fresh_scored_dids, get_ranked_threats, is_score_stale,
    upsert_account_score,
};
use charcoal::db::schema::create_tables;
use charcoal::scoring::generation::{scoring_revision, LEGACY_GENERATION};
use rusqlite::{params, Connection};
use std::collections::HashSet;

const USER: &str = "did:plc:testuser000000000000";

fn insert_raw(conn: &Connection, did: &str, valid_until_sql: &str, generation: &str) {
    conn.execute(
        &format!(
            "INSERT INTO account_scores
                 (user_did, did, handle, threat_score, threat_tier, scoring_generation, valid_until)
             VALUES (?1, ?2, ?3, 20.0, 'Elevated', ?4, {valid_until_sql})"
        ),
        params![USER, did, format!("{did}.handle"), generation],
    )
    .unwrap();
}

/// `valid_for_days` from now (negative = expired) under `generation`.
fn insert_score(conn: &Connection, did: &str, valid_for_days: i64, generation: &str) {
    insert_raw(
        conn,
        did,
        &format!("datetime('now', '{valid_for_days:+} days')"),
        generation,
    );
}

fn score_with_confidence(did: &str, confidence: Option<&str>) -> AccountScore {
    AccountScore {
        did: did.to_string(),
        handle: format!("{did}.handle"),
        toxicity_score: Some(0.5),
        topic_overlap: Some(0.5),
        overlap_legacy: None,
        threat_score: Some(20.0),
        threat_tier: Some("Elevated".to_string()),
        posts_analyzed: 50,
        top_toxic_posts: vec![],
        scored_at: String::new(),
        behavioral_signals: None,
        context_score: None,
        graph_distance: None,
        fingerprint_quality: None,
        scoring_confidence: confidence.map(str::to_string),
    }
}

fn fresh_set(conn: &Connection) -> HashSet<String> {
    get_fresh_scored_dids(conn, USER)
        .unwrap()
        .into_iter()
        .collect()
}

#[test]
fn fresh_set_is_exactly_the_non_stale_dids_including_null_and_malformed_expiry() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    insert_score(&conn, "did:plc:current", 5, scoring_revision());
    insert_score(&conn, "did:plc:expired", -1, scoring_revision());
    insert_score(&conn, "did:plc:legacy", 5, LEGACY_GENERATION);
    insert_score(&conn, "did:plc:oldgen", 5, "1999-01-01");
    insert_raw(&conn, "did:plc:nullvalid", "NULL", scoring_revision());
    insert_raw(
        &conn,
        "did:plc:malformed",
        "'not a timestamp'",
        scoring_revision(),
    );
    // Exact boundary: valid_until == now is NOT fresh (strict >).
    insert_raw(
        &conn,
        "did:plc:boundary",
        "datetime('now')",
        scoring_revision(),
    );

    let fresh = fresh_set(&conn);
    assert_eq!(fresh, HashSet::from(["did:plc:current".to_string()]));

    for did in [
        "did:plc:current",
        "did:plc:expired",
        "did:plc:legacy",
        "did:plc:oldgen",
        "did:plc:nullvalid",
        "did:plc:malformed",
        "did:plc:boundary",
        "did:plc:neverscored",
    ] {
        assert_eq!(
            fresh.contains(did),
            !is_score_stale(&conn, USER, did).unwrap(),
            "{did}"
        );
    }
    // Every stored non-fresh row is COUNTED as expired — including NULL and
    // malformed, which a non-COALESCEd `NOT (...)` would have left unknown.
    assert_eq!(count_expired(&conn, USER).unwrap(), 6);
}

#[test]
fn fresh_set_is_scoped_to_the_user() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, scoring_generation, valid_until)
         VALUES ('did:plc:otheruser', 'did:plc:shared', 'shared.handle', ?1, datetime('now', '+5 days'))",
        params![scoring_revision()],
    )
    .unwrap();
    assert!(!fresh_set(&conn).contains("did:plc:shared"));
    assert!(is_score_stale(&conn, USER, "did:plc:shared").unwrap());
    assert_eq!(count_expired(&conn, USER).unwrap(), 0);
}

#[test]
fn upsert_stamps_generation_and_valid_until_from_confidence() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    for (did, confidence, expected_days) in [
        ("did:plc:low", Some("low"), 3.0),
        ("did:plc:standard", Some("standard"), 7.0),
        ("did:plc:high", Some("high"), 14.0),
        ("did:plc:none", None, 7.0),
        ("did:plc:bogus", Some("bogus"), 7.0),
    ] {
        upsert_account_score(&conn, USER, &score_with_confidence(did, confidence)).unwrap();
        let (generation, days): (String, f64) = conn
            .query_row(
                "SELECT scoring_generation, julianday(valid_until) - julianday(scored_at)
                 FROM account_scores WHERE user_did = ?1 AND did = ?2",
                params![USER, did],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(generation, scoring_revision(), "{did}");
        assert!((days - expected_days).abs() < 0.01, "{did}: {days} days");
        assert!(!is_score_stale(&conn, USER, did).unwrap());
    }
}

#[test]
fn upsert_restamps_an_existing_legacy_row() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    insert_score(&conn, "did:plc:reborn", 5, LEGACY_GENERATION);
    assert!(is_score_stale(&conn, USER, "did:plc:reborn").unwrap());
    upsert_account_score(
        &conn,
        USER,
        &score_with_confidence("did:plc:reborn", Some("high")),
    )
    .unwrap();
    assert!(!is_score_stale(&conn, USER, "did:plc:reborn").unwrap());
    assert_eq!(count_expired(&conn, USER).unwrap(), 0);
}

#[test]
fn ranked_threats_and_counts_hide_expired_rows() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    insert_score(&conn, "did:plc:current", 5, scoring_revision());
    insert_score(&conn, "did:plc:expired", -1, scoring_revision());
    insert_score(&conn, "did:plc:legacy", 5, LEGACY_GENERATION);
    for (did, days) in [("did:plc:na-expired", "-1"), ("did:plc:na-fresh", "+1")] {
        conn.execute(
            "INSERT INTO account_scores (user_did, did, handle, threat_tier, scoring_generation, valid_until)
             VALUES (?1, ?2, 'na.handle', 'NotAssessed', ?3, datetime('now', ?4 || ' days'))",
            params![USER, did, scoring_revision(), days],
        )
        .unwrap();
    }
    let ranked = get_ranked_threats(&conn, USER, 0.0).unwrap();
    assert_eq!(
        ranked.iter().map(|a| a.did.as_str()).collect::<Vec<_>>(),
        ["did:plc:current"]
    );
    assert_eq!(count_not_assessed(&conn, USER).unwrap(), 1);
    assert_eq!(count_expired(&conn, USER).unwrap(), 3);
}

#[test]
fn staleness_days_are_three_seven_fourteen() {
    assert_eq!(ScoringConfidence::Low.staleness_days(), 3);
    assert_eq!(ScoringConfidence::Standard.staleness_days(), 7);
    assert_eq!(ScoringConfidence::High.staleness_days(), 14);
}
