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

    // Migration v4: multi-user schema. Adds a `users` table, and adds
    // `user_did` to topic_fingerprint, account_scores, amplification_events,
    // and scan_state. Tables with single-column primary keys are rebuilt
    // to use composite keys including user_did.
    run_migration(conn, 4, |c| {
        // Wrap in explicit transaction — execute_batch does NOT auto-wrap,
        // so a failure mid-batch would leave a half-migrated schema.
        c.execute_batch(
            "
            BEGIN;

            -- New users table
            CREATE TABLE IF NOT EXISTS users (
                did TEXT PRIMARY KEY,
                handle TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            -- Rebuild topic_fingerprint with user_did as primary key
            CREATE TABLE topic_fingerprint_v4 (
                user_did TEXT NOT NULL,
                fingerprint_json TEXT NOT NULL,
                post_count INTEGER NOT NULL,
                embedding_vector TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (user_did)
            );
            INSERT OR IGNORE INTO topic_fingerprint_v4
                (user_did, fingerprint_json, post_count, embedding_vector, created_at, updated_at)
                SELECT '', fingerprint_json, post_count, embedding_vector, created_at, updated_at
                FROM topic_fingerprint;
            DROP TABLE topic_fingerprint;
            ALTER TABLE topic_fingerprint_v4 RENAME TO topic_fingerprint;

            -- Rebuild account_scores with composite key (user_did, did)
            CREATE TABLE account_scores_v4 (
                user_did TEXT NOT NULL,
                did TEXT NOT NULL,
                handle TEXT NOT NULL,
                toxicity_score REAL,
                topic_overlap REAL,
                threat_score REAL,
                threat_tier TEXT,
                posts_analyzed INTEGER NOT NULL DEFAULT 0,
                top_toxic_posts TEXT,
                scored_at TEXT NOT NULL DEFAULT (datetime('now')),
                behavioral_signals TEXT,
                PRIMARY KEY (user_did, did)
            );
            INSERT OR IGNORE INTO account_scores_v4
                (user_did, did, handle, toxicity_score, topic_overlap, threat_score,
                 threat_tier, posts_analyzed, top_toxic_posts, scored_at, behavioral_signals)
                SELECT '', did, handle, toxicity_score, topic_overlap, threat_score,
                 threat_tier, posts_analyzed, top_toxic_posts, scored_at, behavioral_signals
                FROM account_scores;
            DROP TABLE account_scores;
            ALTER TABLE account_scores_v4 RENAME TO account_scores;

            -- Rebuild amplification_events with user_did (no DEFAULT, so future
            -- inserts without user_did fail hard)
            CREATE TABLE amplification_events_v4 (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_did TEXT NOT NULL,
                event_type TEXT NOT NULL,
                amplifier_did TEXT NOT NULL,
                amplifier_handle TEXT NOT NULL,
                original_post_uri TEXT NOT NULL,
                amplifier_post_uri TEXT,
                amplifier_text TEXT,
                detected_at TEXT NOT NULL DEFAULT (datetime('now')),
                followers_fetched INTEGER NOT NULL DEFAULT 0,
                followers_scored INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO amplification_events_v4
                (id, user_did, event_type, amplifier_did, amplifier_handle,
                 original_post_uri, amplifier_post_uri, amplifier_text,
                 detected_at, followers_fetched, followers_scored)
                SELECT id, '', event_type, amplifier_did, amplifier_handle,
                 original_post_uri, amplifier_post_uri, amplifier_text,
                 detected_at, followers_fetched, followers_scored
                FROM amplification_events;
            DROP TABLE amplification_events;
            ALTER TABLE amplification_events_v4 RENAME TO amplification_events;

            -- Rebuild scan_state with composite key (user_did, key)
            CREATE TABLE scan_state_v4 (
                user_did TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now')),
                PRIMARY KEY (user_did, key)
            );
            INSERT OR IGNORE INTO scan_state_v4
                (user_did, key, value, updated_at)
                SELECT '', key, value, updated_at
                FROM scan_state;
            DROP TABLE scan_state;
            ALTER TABLE scan_state_v4 RENAME TO scan_state;

            -- Rebuild indices with user_did
            DROP INDEX IF EXISTS idx_events_amplifier;
            CREATE INDEX idx_events_amplifier ON amplification_events(user_did, amplifier_did);
            DROP INDEX IF EXISTS idx_scores_tier;
            CREATE INDEX idx_scores_tier ON account_scores(user_did, threat_tier);
            DROP INDEX IF EXISTS idx_scores_age;
            CREATE INDEX idx_scores_age ON account_scores(user_did, scored_at);

            COMMIT;
            ",
        )
    })?;

    // Migration v5: contextual scoring support. Adds new columns for NLI
    // pair scoring, a user_labels table for ground truth, and an
    // inferred_pairs table for topic-matched post pairs.
    run_migration(conn, 5, |c| {
        c.execute_batch(
            "
            BEGIN;

            -- Add original post text and NLI context score to amplification events
            ALTER TABLE amplification_events ADD COLUMN original_post_text TEXT;
            ALTER TABLE amplification_events ADD COLUMN context_score REAL;

            -- Add NLI context score to account scores
            ALTER TABLE account_scores ADD COLUMN context_score REAL;

            -- User-provided labels for scoring accuracy measurement
            CREATE TABLE IF NOT EXISTS user_labels (
                user_did TEXT NOT NULL,
                target_did TEXT NOT NULL,
                label TEXT NOT NULL,
                labeled_at TEXT NOT NULL DEFAULT (datetime('now')),
                notes TEXT,
                PRIMARY KEY (user_did, target_did)
            );

            -- Topic-matched post pairs for second-degree NLI scoring
            CREATE TABLE IF NOT EXISTS inferred_pairs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                user_did TEXT NOT NULL,
                target_did TEXT NOT NULL,
                target_post_text TEXT NOT NULL,
                target_post_uri TEXT NOT NULL,
                user_post_text TEXT NOT NULL,
                user_post_uri TEXT NOT NULL,
                similarity REAL NOT NULL,
                context_score REAL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE INDEX IF NOT EXISTS idx_inferred_pairs_target
                ON inferred_pairs(user_did, target_did);
            CREATE UNIQUE INDEX IF NOT EXISTS idx_inferred_pairs_dedup
                ON inferred_pairs(user_did, target_did, target_post_uri, user_post_uri);

            COMMIT;
            ",
        )
    })?;

    // Migration v6: add graph_distance column to account_scores.
    // Stores the social graph relationship label (Mutual follow, Follows you,
    // You follow, Stranger) for scoring weight adjustments.
    run_migration(conn, 6, |c| {
        c.execute_batch("ALTER TABLE account_scores ADD COLUMN graph_distance TEXT;")
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
        // amplification_events, scan_state, users, user_labels,
        // inferred_pairs = 8 tables
        assert_eq!(count, 8i64);
    }

    #[test]
    fn test_migration_v2_adds_embedding_column() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();

        // Verify the embedding_vector column exists by inserting a row with it
        // (After v4, topic_fingerprint uses user_did as primary key instead of id)
        conn.execute(
            "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, embedding_vector)
             VALUES ('did:plc:test', '{}', 10, '[0.1, 0.2]')",
            [],
        )
        .unwrap();

        let result: String = conn
            .query_row(
                "SELECT embedding_vector FROM topic_fingerprint WHERE user_did = 'did:plc:test'",
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

        // After v4, account_scores has composite key (user_did, did)
        conn.execute(
            "INSERT INTO account_scores (user_did, did, handle, posts_analyzed)
             VALUES ('', 'did:plc:test', 'test.bsky.social', 10)",
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
        assert_eq!(versions, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn test_migration_v4_adds_user_did_columns() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();

        // Verify users table exists and can accept rows
        conn.execute(
            "INSERT INTO users (did, handle) VALUES ('did:plc:abc123', 'alice.bsky.social')",
            [],
        )
        .unwrap();

        let handle: String = conn
            .query_row(
                "SELECT handle FROM users WHERE did = 'did:plc:abc123'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(handle, "alice.bsky.social");

        // Verify topic_fingerprint now has user_did column (no singleton constraint)
        conn.execute(
            "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count)
             VALUES ('did:plc:abc123', '{\"test\":1}', 5)",
            [],
        )
        .unwrap();

        let fp_user: String = conn
            .query_row(
                "SELECT user_did FROM topic_fingerprint WHERE user_did = 'did:plc:abc123'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fp_user, "did:plc:abc123");

        // Verify account_scores has composite key (user_did, did)
        conn.execute(
            "INSERT INTO account_scores (user_did, did, handle, posts_analyzed)
             VALUES ('did:plc:abc123', 'did:plc:target1', 'target.bsky.social', 10)",
            [],
        )
        .unwrap();

        let score_user: String = conn
            .query_row(
                "SELECT user_did FROM account_scores WHERE did = 'did:plc:target1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(score_user, "did:plc:abc123");

        // Verify amplification_events has user_did column
        conn.execute(
            "INSERT INTO amplification_events
             (event_type, amplifier_did, amplifier_handle, original_post_uri, user_did)
             VALUES ('quote', 'did:plc:amp1', 'amp.bsky.social', 'at://did:plc:abc123/app.bsky.feed.post/1', 'did:plc:abc123')",
            [],
        )
        .unwrap();

        let event_user: String = conn
            .query_row(
                "SELECT user_did FROM amplification_events WHERE amplifier_did = 'did:plc:amp1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(event_user, "did:plc:abc123");

        // Verify scan_state has composite key (user_did, key)
        conn.execute(
            "INSERT INTO scan_state (user_did, key, value)
             VALUES ('did:plc:abc123', 'last_scan', '2026-03-10')",
            [],
        )
        .unwrap();

        let scan_user: String = conn
            .query_row(
                "SELECT user_did FROM scan_state WHERE key = 'last_scan'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(scan_user, "did:plc:abc123");
    }

    #[test]
    fn test_migration_v4_updates_table_count() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        let count = table_count(&conn).unwrap();
        // schema_version, topic_fingerprint, account_scores,
        // amplification_events, scan_state, users, user_labels,
        // inferred_pairs = 8 tables
        assert_eq!(count, 8i64);

        // Verify schema_version includes v4
        let versions: Vec<i64> = conn
            .prepare("SELECT version FROM schema_version ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(versions, vec![1, 2, 3, 4, 5, 6]);
    }
}
