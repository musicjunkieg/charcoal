// Unit tests for the bulk staleness lookup (#213 Task 5).
//
// The scan discovery loops filtered candidates with one `is_score_stale`
// DB round-trip per candidate (an N+1). `get_fresh_scored_dids` replaces that
// with a single query returning the DIDs scored within the window; the loops
// then test set membership in memory. These tests pin that the bulk set is
// EXACTLY the complement of `is_score_stale` for the same data and cutoff —
// a mismatched window would silently re-score (or skip) every account.

use charcoal::db::queries::{get_fresh_scored_dids, is_score_stale};
use charcoal::db::schema::create_tables;
use rusqlite::{params, Connection};
use std::collections::HashSet;

const USER: &str = "did:plc:testuser000000000000";

/// Insert a scored account, then force its `scored_at` to `days_ago` days old.
fn insert_score(conn: &Connection, did: &str, days_ago: i64) {
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle) VALUES (?1, ?2, ?3)",
        params![USER, did, format!("{did}.handle")],
    )
    .unwrap();
    conn.execute(
        "UPDATE account_scores SET scored_at = datetime('now', ?1) WHERE user_did = ?2 AND did = ?3",
        params![format!("-{days_ago} days"), USER, did],
    )
    .unwrap();
}

#[test]
fn fresh_set_is_exactly_the_non_stale_dids() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();

    // scored today (fresh), 3 days ago (fresh), 8 days ago (stale).
    insert_score(&conn, "did:plc:today", 0);
    insert_score(&conn, "did:plc:threedays", 3);
    insert_score(&conn, "did:plc:eightdays", 8);

    let fresh: HashSet<String> = get_fresh_scored_dids(&conn, USER, 7)
        .unwrap()
        .into_iter()
        .collect();

    assert!(fresh.contains("did:plc:today"));
    assert!(fresh.contains("did:plc:threedays"));
    assert!(!fresh.contains("did:plc:eightdays"));

    // Equivalence: fresh membership == NOT stale, for every stored DID and a
    // DID that was never scored (must be stale / absent).
    for did in [
        "did:plc:today",
        "did:plc:threedays",
        "did:plc:eightdays",
        "did:plc:neverscored",
    ] {
        let stale = is_score_stale(&conn, USER, did, 7).unwrap();
        assert_eq!(
            fresh.contains(did),
            !stale,
            "fresh-set membership must equal !is_score_stale for {did}"
        );
    }
}

#[test]
fn fresh_set_is_scoped_to_the_user() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();

    // A fresh score owned by a DIFFERENT user must not leak into USER's set.
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle) VALUES (?1, ?2, ?3)",
        params!["did:plc:otheruser", "did:plc:shared", "shared.handle"],
    )
    .unwrap();

    let fresh: HashSet<String> = get_fresh_scored_dids(&conn, USER, 7)
        .unwrap()
        .into_iter()
        .collect();

    assert!(!fresh.contains("did:plc:shared"));
    // And is_score_stale agrees: USER has no score for it → stale.
    assert!(is_score_stale(&conn, USER, "did:plc:shared", 7).unwrap());
}
