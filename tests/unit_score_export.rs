// #344 R01 / V2-06 / V2-07: `charcoal migrate` must be lossless. Presentation
// queries hide expired/legacy rows; export/import carry every row with its
// original scored_at, revision and expiry — including SQLite's NULL and
// malformed expiries, which import as "expired when scored" — and importing
// never renews anything. Fixtures are relative to now, never calendar dates.

use charcoal::db::models::{ExportedExpiry, StoredScore};
use charcoal::db::schema::{create_tables, create_tables_through};
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::scoring::generation::{scoring_revision, LEGACY_GENERATION};
use rusqlite::{params, Connection};
use std::sync::Arc;

const USER: &str = "did:plc:exportuser00000000000000";

/// current (+14 d), expired (−6 d), legacy, NotAssessed (NULL score),
/// NULL expiry, malformed expiry.
fn seeded() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    let rev = scoring_revision();
    let rows = [
        (
            "did:plc:cur",
            "40.0",
            "'High'",
            "datetime('now', '-4 days')",
            "datetime('now', '+10 days')",
            rev,
        ),
        (
            "did:plc:exp",
            "20.0",
            "'Elevated'",
            "datetime('now', '-13 days')",
            "datetime('now', '-6 days')",
            rev,
        ),
        (
            "did:plc:leg",
            "50.0",
            "'High'",
            "datetime('now', '-140 days')",
            "datetime('now', '-126 days')",
            LEGACY_GENERATION,
        ),
        (
            "did:plc:na",
            "NULL",
            "'NotAssessed'",
            "datetime('now', '-12 days')",
            "datetime('now', '-5 days')",
            rev,
        ),
        (
            "did:plc:nul",
            "30.0",
            "'Elevated'",
            "datetime('now', '-2 days')",
            "NULL",
            rev,
        ),
        (
            "did:plc:bad",
            "35.0",
            "'High'",
            "datetime('now', '-2 days')",
            "'not a timestamp'",
            rev,
        ),
    ];
    for (did, score, tier, scored_at, valid_until, generation) in rows {
        conn.execute(
            &format!(
                "INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at, scoring_generation, valid_until)
                 VALUES (?1, ?2, ?3, {score}, {tier}, {scored_at}, ?4, {valid_until})"
            ),
            params![USER, did, format!("{did}.h"), generation],
        )
        .unwrap();
    }
    conn
}

fn by_did<'a>(rows: &'a [StoredScore], did: &str) -> &'a StoredScore {
    rows.iter().find(|r| r.score.did == did).expect(did)
}

#[tokio::test]
async fn export_returns_every_row_with_its_provenance() {
    let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(seeded()));
    let rows = db.export_scores(USER).await.unwrap();
    assert_eq!(
        rows.len(),
        6,
        "expired, legacy, NULL-score, NULL-expiry and malformed rows are all exported"
    );
    assert_eq!(
        by_did(&rows, "did:plc:leg").scoring_generation,
        LEGACY_GENERATION
    );
    assert!(by_did(&rows, "did:plc:na").score.threat_score.is_none());
    assert_eq!(
        by_did(&rows, "did:plc:nul").valid_until,
        ExportedExpiry::Missing
    );
    assert_eq!(
        by_did(&rows, "did:plc:bad").valid_until,
        ExportedExpiry::Invalid("not a timestamp".to_string())
    );
    assert!(matches!(
        by_did(&rows, "did:plc:cur").valid_until,
        ExportedExpiry::At(_)
    ));
    // RFC3339 with the millisecond field SQLite can render.
    assert!(by_did(&rows, "did:plc:cur").scored_at.ends_with("+00:00"));
}

#[tokio::test]
async fn import_preserves_provenance_and_never_renews_expiry() {
    let src: Arc<dyn Database> = Arc::new(SqliteDatabase::new(seeded()));
    let dst_conn = Connection::open_in_memory().unwrap();
    create_tables(&dst_conn).unwrap();
    let dst: Arc<dyn Database> = Arc::new(SqliteDatabase::new(dst_conn));

    let rows = src.export_scores(USER).await.unwrap();
    for r in &rows {
        dst.import_score(USER, r).await.unwrap();
    }
    for r in &rows {
        dst.import_score(USER, r).await.unwrap(); // idempotent, not a renewal
    }
    let back = dst.export_scores(USER).await.unwrap();

    // Well-formed rows round-trip exactly (SQLite → SQLite: whole seconds in,
    // whole seconds out).
    for did in ["did:plc:cur", "did:plc:exp", "did:plc:leg", "did:plc:na"] {
        assert_eq!(by_did(&rows, did), by_did(&back, did), "{did}");
    }
    // NULL / malformed expiries import as "expired when scored": the
    // destination holds a real timestamp equal to scored_at.
    for did in ["did:plc:nul", "did:plc:bad"] {
        let imported = by_did(&back, did);
        assert_eq!(
            imported.valid_until,
            ExportedExpiry::At(imported.scored_at.clone()),
            "{did}"
        );
        assert_eq!(
            imported.scored_at,
            by_did(&rows, did).scored_at,
            "{did} scored_at untouched"
        );
    }
    // Presentation agrees on both sides: one visible row, five expired.
    assert_eq!(dst.get_ranked_threats(USER, 0.0).await.unwrap().len(), 1);
    assert_eq!(dst.count_expired(USER).await.unwrap(), 5);
    assert_eq!(src.count_expired(USER).await.unwrap(), 5);
}

/// A genuinely v17 database (migrations through 17 only), opened by this
/// binary: v18 stamps 'legacy' + scored_at + 14 d, and the export/import
/// path carries exactly that into a fresh database.
#[tokio::test]
async fn v17_fixture_rows_survive_open_export_import() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables_through(&conn, 17).unwrap();
    let max: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(max, 17);
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at)
         VALUES (?1, 'did:plc:v17', 'v17.h', 40.0, 'High', datetime('now', '-30 days'))",
        params![USER],
    )
    .unwrap();
    create_tables(&conn).unwrap(); // the binary opens it: v18 applies
    let src: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));

    let rows = src.export_scores(USER).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.scoring_generation, LEGACY_GENERATION);
    let ExportedExpiry::At(valid_until) = &row.valid_until else {
        panic!("backfilled expiry")
    };
    let scored = chrono::DateTime::parse_from_rfc3339(&row.scored_at).unwrap();
    let valid = chrono::DateTime::parse_from_rfc3339(valid_until).unwrap();
    assert_eq!(valid - scored, chrono::Duration::days(14));

    let dst_conn = Connection::open_in_memory().unwrap();
    create_tables(&dst_conn).unwrap();
    let dst: Arc<dyn Database> = Arc::new(SqliteDatabase::new(dst_conn));
    dst.import_score(USER, row).await.unwrap();
    assert_eq!(dst.export_scores(USER).await.unwrap(), rows);
    assert!(
        dst.is_score_stale(USER, "did:plc:v17").await.unwrap(),
        "legacy stays hidden after import"
    );
    assert_eq!(dst.count_expired(USER).await.unwrap(), 1);
}
