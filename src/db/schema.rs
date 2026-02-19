// Database schema — table creation and migrations.
//
// We use a simple version-based migration approach: a `schema_version` table
// tracks which migrations have run, and each migration is a function that
// executes SQL statements.

use anyhow::{Context, Result};
use rusqlite::Connection;

/// Create all tables if they don't exist yet.
///
/// This is idempotent — safe to call on every startup.
pub fn create_tables(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "
        -- Tracks schema version for future migrations
        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- The protected user's topic fingerprint
        -- Stored as JSON so we can evolve the structure without migrations
        CREATE TABLE IF NOT EXISTS topic_fingerprint (
            id INTEGER PRIMARY KEY CHECK (id = 1),  -- singleton row
            fingerprint_json TEXT NOT NULL,
            post_count INTEGER NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Cached toxicity scores for accounts we've already analyzed
        CREATE TABLE IF NOT EXISTS account_scores (
            did TEXT PRIMARY KEY,              -- Bluesky DID (decentralized identifier)
            handle TEXT NOT NULL,
            toxicity_score REAL,               -- 0.0 to 1.0
            topic_overlap REAL,                -- 0.0 to 1.0
            threat_score REAL,                 -- 0.0 to 100.0
            threat_tier TEXT,                  -- Low / Watch / Elevated / High
            posts_analyzed INTEGER NOT NULL DEFAULT 0,
            top_toxic_posts TEXT,              -- JSON array of most toxic posts as evidence
            scored_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Amplification events (quotes and reposts of the protected user's posts)
        CREATE TABLE IF NOT EXISTS amplification_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            event_type TEXT NOT NULL,          -- 'quote' or 'repost'
            amplifier_did TEXT NOT NULL,       -- who quoted/reposted
            amplifier_handle TEXT NOT NULL,
            original_post_uri TEXT NOT NULL,   -- the protected user's post that was amplified
            amplifier_post_uri TEXT,           -- the quote post URI (null for reposts)
            amplifier_text TEXT,               -- the commentary added in a quote post
            detected_at TEXT NOT NULL DEFAULT (datetime('now')),
            followers_fetched INTEGER NOT NULL DEFAULT 0,
            followers_scored INTEGER NOT NULL DEFAULT 0
        );

        -- Scan state — tracks pagination cursors and last-scan timestamps
        CREATE TABLE IF NOT EXISTS scan_state (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Index for looking up events by amplifier
        CREATE INDEX IF NOT EXISTS idx_events_amplifier
            ON amplification_events(amplifier_did);

        -- Index for looking up scores by threat tier
        CREATE INDEX IF NOT EXISTS idx_scores_tier
            ON account_scores(threat_tier);

        -- Index for finding stale scores that need refreshing
        CREATE INDEX IF NOT EXISTS idx_scores_age
            ON account_scores(scored_at);
        ",
    )
    .context("Failed to create database tables")?;

    // Record initial schema version if not already set
    conn.execute(
        "INSERT OR IGNORE INTO schema_version (version) VALUES (?1)",
        [1],
    )?;

    // Migration v2: add embedding_vector column to topic_fingerprint.
    // Stores the mean sentence embedding (384-dim, JSON array) for the
    // protected user's posts. Used for semantic topic overlap scoring.
    run_migration(conn, 2, |c| {
        c.execute_batch("ALTER TABLE topic_fingerprint ADD COLUMN embedding_vector TEXT;")
    })?;

    // Migration v3: add behavioral_signals column to account_scores.
    // Stores a JSON object with quote_ratio, reply_ratio, avg_engagement,
    // pile_on, benign_gate, and behavioral_boost.
    run_migration(conn, 3, |c| {
        c.execute_batch("ALTER TABLE account_scores ADD COLUMN behavioral_signals TEXT;")
    })?;

    Ok(())
}

/// Run a migration if it hasn't been applied yet.
/// The migration function receives the connection and should execute its SQL.
fn run_migration<F>(conn: &Connection, version: i64, migrate: F) -> Result<()>
where
    F: FnOnce(&Connection) -> rusqlite::Result<()>,
{
    let already_applied: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM schema_version WHERE version = ?1",
        [version],
        |row| row.get(0),
    )?;

    if !already_applied {
        migrate(conn).with_context(|| format!("Migration v{version} failed"))?;
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [version],
        )?;
    }

    Ok(())
}

/// Count the number of tables in the database (useful for init confirmation).
pub fn table_count(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_tables_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        // Running create_tables twice should not error
        create_tables(&conn).unwrap();
        create_tables(&conn).unwrap();
    }

    #[test]
    fn test_table_count() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let count = table_count(&conn).unwrap();
        // schema_version, topic_fingerprint, account_scores,
        // amplification_events, scan_state = 5 tables
        assert_eq!(count, 5i64);
    }

    #[test]
    fn test_migration_v2_adds_embedding_column() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();

        // Verify the embedding_vector column exists by inserting a row with it
        conn.execute(
            "INSERT INTO topic_fingerprint (id, fingerprint_json, post_count, embedding_vector)
             VALUES (1, '{}', 10, '[0.1, 0.2]')",
            [],
        )
        .unwrap();

        let result: String = conn
            .query_row(
                "SELECT embedding_vector FROM topic_fingerprint WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(result, "[0.1, 0.2]");
    }

    #[test]
    fn test_migration_v3_adds_behavioral_signals_column() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();

        conn.execute(
            "INSERT INTO account_scores (did, handle, posts_analyzed)
             VALUES ('did:plc:test', 'test.bsky.social', 10)",
            [],
        )
        .unwrap();

        conn.execute(
            "UPDATE account_scores SET behavioral_signals = ?1 WHERE did = 'did:plc:test'",
            rusqlite::params![r#"{"quote_ratio":0.5}"#],
        )
        .unwrap();

        let result: String = conn
            .query_row(
                "SELECT behavioral_signals FROM account_scores WHERE did = 'did:plc:test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(result, r#"{"quote_ratio":0.5}"#);
    }

    #[test]
    fn test_migration_v2_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        // Run create_tables three times — migration should only run once
        create_tables(&conn).unwrap();
        create_tables(&conn).unwrap();
        create_tables(&conn).unwrap();

        // Verify schema_version has both v1 and v2
        let versions: Vec<i64> = conn
            .prepare("SELECT version FROM schema_version ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(versions, vec![1, 2, 3]);
    }
}
