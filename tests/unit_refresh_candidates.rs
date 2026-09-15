// #344: the refresh job's candidate source. Rows come FROM account_scores —
// High/Elevated by score, and either expiring within the horizon, already
// expired (including NULL/malformed expiry, R11), or stamped with an older
// generation. Most dangerous first.

use charcoal::db::queries::list_refresh_candidates;
use charcoal::db::schema::create_tables;
use charcoal::scoring::generation::{scoring_revision, LEGACY_GENERATION};
use rusqlite::{params, Connection};

const USER: &str = "did:plc:refreshuser0000000000000";

fn insert(
    conn: &Connection,
    user: &str,
    did: &str,
    score: f64,
    valid_until_sql: &str,
    generation: &str,
    graph: Option<&str>,
) {
    conn.execute(
        &format!(
            "INSERT INTO account_scores
                 (user_did, did, handle, threat_score, threat_tier, scoring_generation, valid_until, graph_distance)
             VALUES (?1, ?2, ?3, ?4, 'x', ?5, {valid_until_sql}, ?6)"
        ),
        params![user, did, format!("{did}.handle"), score, generation, graph],
    )
    .unwrap();
}

fn days(n: i64) -> String {
    format!("datetime('now', '{n:+} days')")
}

#[test]
fn selects_high_and_elevated_rows_that_are_expiring_expired_malformed_or_old_generation() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    // Included:
    insert(
        &conn,
        USER,
        "did:plc:high-expiring",
        60.0,
        &days(1),
        scoring_revision(),
        Some("Stranger"),
    );
    insert(
        &conn,
        USER,
        "did:plc:high-expired",
        40.0,
        &days(-3),
        scoring_revision(),
        Some("Follows you"),
    );
    insert(
        &conn,
        USER,
        "did:plc:high-null",
        45.0,
        "NULL",
        scoring_revision(),
        None,
    );
    insert(
        &conn,
        USER,
        "did:plc:high-malformed",
        42.0,
        "'yesterday-ish'",
        scoring_revision(),
        None,
    );
    insert(
        &conn,
        USER,
        "did:plc:elevated-legacy",
        20.0,
        &days(10),
        LEGACY_GENERATION,
        None,
    );
    insert(
        &conn,
        USER,
        "did:plc:elevated-floor",
        15.0,
        &days(1),
        scoring_revision(),
        None,
    );
    // Excluded:
    insert(
        &conn,
        USER,
        "did:plc:high-fresh",
        50.0,
        &days(10),
        scoring_revision(),
        None,
    );
    insert(
        &conn,
        USER,
        "did:plc:watch-legacy",
        14.99,
        &days(1),
        LEGACY_GENERATION,
        None,
    );
    insert(
        &conn,
        "did:plc:otheruser",
        "did:plc:high-other",
        60.0,
        &days(1),
        scoring_revision(),
        None,
    );
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, threat_tier, scoring_generation, valid_until)
         VALUES (?1, 'did:plc:na', 'na.handle', 'NotAssessed', ?2, datetime('now', '-1 days'))",
        params![USER, LEGACY_GENERATION],
    )
    .unwrap();

    let rows = list_refresh_candidates(&conn, USER, 2).unwrap();
    let dids: Vec<&str> = rows.iter().map(|r| r.did.as_str()).collect();
    assert_eq!(
        dids,
        [
            "did:plc:high-expiring",   // 60
            "did:plc:high-null",       // 45
            "did:plc:high-malformed",  // 42
            "did:plc:high-expired",    // 40
            "did:plc:elevated-legacy", // 20
            "did:plc:elevated-floor",  // 15
        ],
        "most dangerous first; NULL and malformed expiry are eligible"
    );
    assert_eq!(rows[0].graph_distance.as_deref(), Some("Stranger"));
    assert_eq!(rows[3].graph_distance.as_deref(), Some("Follows you"));
}

#[test]
fn horizon_zero_means_only_already_expired_or_old_generation() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    insert(
        &conn,
        USER,
        "did:plc:soon",
        60.0,
        &days(1),
        scoring_revision(),
        None,
    );
    insert(
        &conn,
        USER,
        "did:plc:gone",
        60.0,
        &days(-1),
        scoring_revision(),
        None,
    );
    insert(
        &conn,
        USER,
        "did:plc:boundary",
        60.0,
        "datetime('now')",
        scoring_revision(),
        None,
    );
    let dids: Vec<String> = list_refresh_candidates(&conn, USER, 0)
        .unwrap()
        .into_iter()
        .map(|r| r.did)
        .collect();
    // Tied on threat_score (60.0), so the tie-break `did` ASC decides order:
    // "did:plc:boundary" < "did:plc:gone" alphabetically.
    assert_eq!(
        dids,
        ["did:plc:boundary", "did:plc:gone"],
        "valid_until == now is expired (fresh is strict >)"
    );
}

#[test]
fn elevated_floor_matches_from_score() {
    use charcoal::db::models::ThreatTier;
    assert_eq!(
        ThreatTier::from_score(ThreatTier::ELEVATED_MIN),
        ThreatTier::Elevated
    );
    assert_eq!(
        ThreatTier::from_score(ThreatTier::ELEVATED_MIN - 0.01),
        ThreatTier::Watch
    );
}

/// R12: the query must use the (user_did, threat_score) index, not scan the
/// table. Asserted on the plan text so a future edit that drops the index
/// or rewrites the predicate into something unindexable fails here.
#[test]
fn candidate_query_uses_the_user_score_index() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    let plan: Vec<String> = conn
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            charcoal::db::queries::REFRESH_CANDIDATES_SQL
        ))
        .unwrap()
        .query_map(params![USER, 15.0, "+2 days", scoring_revision()], |r| {
            r.get::<_, String>(3)
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter()
            .any(|line| line.contains("idx_account_scores_user_score")),
        "plan: {plan:?}"
    );
}
