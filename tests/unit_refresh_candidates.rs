// #344: the refresh job's candidate source. Rows come FROM account_scores —
// High/Elevated by score, and either expiring within the horizon, already
// expired (including NULL/malformed expiry, R11), or stamped with an older
// generation. Most dangerous first.

use charcoal::db::models::ThreatTier;
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

/// `{n:+}` renders the sign for both directions — the same spelling the
/// production modifier in `list_refresh_candidates` now uses. This helper
/// having had it all along is why the production `format!("+{n} days")`
/// escaped notice: the fixtures were correct, the query was not.
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
        .query_map(
            params![
                USER,
                ThreatTier::ELEVATED_MIN,
                "+2 days",
                scoring_revision()
            ],
            |r| r.get::<_, String>(3),
        )
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter()
            .any(|line| line.contains("idx_account_scores_user_score")),
        "plan: {plan:?}"
    );
}

/// A negative horizon must NARROW the candidate set (only rows already
/// expired by more than `|horizon|` days), never widen it. The old modifier
/// `format!("+{horizon_days} days")` rendered -1 as "+-1 days", which SQLite
/// cannot parse: `datetime('now','+-1 days')` is NULL, `COALESCE(… , 1)` then
/// returns 1 for every row and BOTH the expiry and generation filters
/// disappear — every High/Elevated row came back. Postgres's
/// `make_interval(days => -1)` narrowed correctly, so the backends disagreed.
/// The Postgres twin of this test is
/// `test_pg_list_refresh_candidates_negative_horizon_narrows` in
/// `tests/db_postgres.rs`, seeded with the same four rows.
#[test]
fn negative_horizon_narrows_the_candidate_set() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    // Expired two days ago: due at horizon 0 AND at horizon -1.
    insert(
        &conn,
        USER,
        "did:plc:neg-gone-2d",
        60.0,
        &days(-2),
        scoring_revision(),
        None,
    );
    // Expired two hours ago: due at horizon 0, NOT at horizon -1.
    insert(
        &conn,
        USER,
        "did:plc:neg-gone-2h",
        50.0,
        "datetime('now', '-2 hours')",
        scoring_revision(),
        None,
    );
    // Fresh and current: due at neither horizon.
    insert(
        &conn,
        USER,
        "did:plc:neg-fresh",
        40.0,
        &days(10),
        scoring_revision(),
        None,
    );
    // Old generation: due at every horizon — that branch is time-independent.
    insert(
        &conn,
        USER,
        "did:plc:neg-legacy",
        30.0,
        &days(10),
        LEGACY_GENERATION,
        None,
    );

    let at_zero: Vec<String> = list_refresh_candidates(&conn, USER, 0)
        .unwrap()
        .into_iter()
        .map(|r| r.did)
        .collect();
    let at_minus_one: Vec<String> = list_refresh_candidates(&conn, USER, -1)
        .unwrap()
        .into_iter()
        .map(|r| r.did)
        .collect();

    assert_eq!(
        at_zero,
        [
            "did:plc:neg-gone-2d",
            "did:plc:neg-gone-2h",
            "did:plc:neg-legacy"
        ],
        "horizon 0: everything already expired, plus the legacy row"
    );
    assert_eq!(
        at_minus_one,
        ["did:plc:neg-gone-2d", "did:plc:neg-legacy"],
        "horizon -1: only rows expired more than a day ago, plus the legacy row"
    );
    assert!(
        at_minus_one.len() < at_zero.len(),
        "a negative horizon narrows; it must never fail open and widen. \
         at_minus_one={at_minus_one:?} at_zero={at_zero:?}"
    );
    assert!(
        at_minus_one.iter().all(|did| at_zero.contains(did)),
        "and the narrower set is a subset of the wider one"
    );
}

/// The fresh predicate (`fresh_sql`, used by `get_fresh_scored_dids`) and the
/// candidate predicate (`REFRESH_CANDIDATES_SQL`) are two hand-written
/// spellings of one rule. Nothing else fails if one is edited and the other
/// is not, so this test pins the partition itself: over the High/Elevated
/// rows, "fresh" and "refresh candidate at horizon 0" must be exact
/// complements — union is every High/Elevated row, intersection is empty.
#[test]
fn fresh_and_candidate_predicates_partition_the_high_elevated_rows() {
    use std::collections::BTreeSet;

    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();

    // (did, score, valid_until SQL, generation)
    let fixture: [(&str, f64, &str, &str); 10] = [
        // High/Elevated, fresh:
        (
            "did:plc:pt-fresh-high",
            60.0,
            "datetime('now', '+10 days')",
            scoring_revision(),
        ),
        (
            "did:plc:pt-fresh-floor",
            15.0,
            "datetime('now', '+10 days')",
            scoring_revision(),
        ),
        // High/Elevated, not fresh — each for a different reason:
        (
            "did:plc:pt-expired",
            50.0,
            "datetime('now', '-1 days')",
            scoring_revision(),
        ),
        ("did:plc:pt-null", 45.0, "NULL", scoring_revision()),
        (
            "did:plc:pt-malformed",
            40.0,
            "'yesterday-ish'",
            scoring_revision(),
        ),
        (
            "did:plc:pt-boundary",
            35.0,
            "datetime('now')",
            scoring_revision(),
        ),
        (
            "did:plc:pt-legacy-unexpired",
            30.0,
            "datetime('now', '+10 days')",
            LEGACY_GENERATION,
        ),
        (
            "did:plc:pt-legacy-expired",
            25.0,
            "datetime('now', '-5 days')",
            LEGACY_GENERATION,
        ),
        // Below the floor: in neither half of the partition under test, but
        // present so the fresh query (which does NOT filter by score) has
        // rows it must return and the candidate query must still exclude.
        (
            "did:plc:pt-watch-fresh",
            10.0,
            "datetime('now', '+10 days')",
            scoring_revision(),
        ),
        (
            "did:plc:pt-watch-expired",
            9.0,
            "datetime('now', '-1 days')",
            scoring_revision(),
        ),
    ];
    for (did, score, valid_until_sql, generation) in fixture {
        insert(&conn, USER, did, score, valid_until_sql, generation, None);
    }

    let high_elevated: BTreeSet<String> = fixture
        .iter()
        .filter(|(_, score, _, _)| *score >= ThreatTier::ELEVATED_MIN)
        .map(|(did, ..)| (*did).to_string())
        .collect();
    assert_eq!(
        high_elevated.len(),
        8,
        "fixture sanity: 8 rows clear the floor"
    );

    // The fresh half, restricted to High/Elevated — get_fresh_scored_dids
    // has no score filter of its own.
    let fresh_high_elevated: BTreeSet<String> =
        charcoal::db::queries::get_fresh_scored_dids(&conn, USER)
            .unwrap()
            .into_iter()
            .filter(|did| high_elevated.contains(did))
            .collect();
    // The candidate half. Horizon 0 is the complement boundary: fresh is
    // `valid_until > now`, a candidate at horizon 0 is `valid_until <= now`.
    let candidates: BTreeSet<String> = list_refresh_candidates(&conn, USER, 0)
        .unwrap()
        .into_iter()
        .map(|r| r.did)
        .collect();

    assert!(
        fresh_high_elevated.is_disjoint(&candidates),
        "no High/Elevated row may be both fresh and a refresh candidate. \
         fresh={fresh_high_elevated:?} candidates={candidates:?}"
    );
    let union: BTreeSet<String> = fresh_high_elevated.union(&candidates).cloned().collect();
    assert_eq!(
        union, high_elevated,
        "every High/Elevated row must land in exactly one half. \
         fresh={fresh_high_elevated:?} candidates={candidates:?}"
    );
    // Both halves must be non-empty, or a predicate that matched nothing (or
    // everything) would satisfy the two assertions above vacuously.
    assert_eq!(fresh_high_elevated.len(), 2, "fresh half");
    assert_eq!(candidates.len(), 6, "candidate half");
}
