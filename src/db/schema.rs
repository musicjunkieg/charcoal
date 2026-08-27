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

    // Migration v7: add last_login_at to users table.
    // Tracks when each user last authenticated via OAuth, used by admin dashboard.
    run_migration(conn, 7, |c| {
        c.execute_batch("ALTER TABLE users ADD COLUMN last_login_at TEXT;")?;
        Ok(())
    })?;

    // Migration v8: add fingerprint_quality and scoring_confidence to account_scores.
    // fingerprint_quality tracks whether the fingerprint was built from originals only
    // (normal), mixed (degraded), or insufficient data (unreliable).
    // scoring_confidence tracks the depth of analysis (low/standard/high).
    run_migration(conn, 8, |c| {
        c.execute_batch(
            "
            ALTER TABLE account_scores ADD COLUMN fingerprint_quality TEXT;
            ALTER TABLE account_scores ADD COLUMN scoring_confidence TEXT;
            ",
        )
    })?;

    // Migration v9: decoupled pipeline staging tables.
    //
    // classification_queue — one row per post awaiting or done with GPU
    //   classification. The composite PK (user_did, account_did, post_uri)
    //   makes enqueue an UPSERT so Phase A is fully idempotent.
    //   idx_clsq_pending speeds the burst's pending-row scan.
    //
    // scan_account_input — one row per account containing the serialised
    //   AccountInput blob produced by Phase A and consumed by Phase B.
    //   The phase marker (gather/burst/finalize/done) continues to live in
    //   scan_state as a key='scan_phase' row — this migration does NOT alter
    //   scan_state.
    run_migration(conn, 9, |c| {
        c.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS classification_queue (
                user_did        TEXT    NOT NULL,
                account_did     TEXT    NOT NULL,
                post_uri        TEXT    NOT NULL,
                text            TEXT    NOT NULL,  -- 'text' is intentionally a column name here, not the TEXT type keyword
                context_text    TEXT,
                post_kind       TEXT    NOT NULL,
                onnx_score      REAL    NOT NULL,
                status          TEXT    NOT NULL CHECK (status IN ('pending', 'done')),
                toxic_token     INTEGER,
                confidence      REAL,
                model_id        TEXT,
                policy_version  TEXT,
                PRIMARY KEY (user_did, account_did, post_uri)
            );

            CREATE INDEX IF NOT EXISTS idx_clsq_pending
                ON classification_queue (user_did, status);

            CREATE TABLE IF NOT EXISTS scan_account_input (
                user_did        TEXT    NOT NULL,
                account_did     TEXT    NOT NULL,
                payload_json    TEXT    NOT NULL,
                PRIMARY KEY (user_did, account_did)
            );
            ",
        )
    })?;

    // v10 — scan_skips: a durable record of accounts dropped from a scan.
    //
    // A skipped account is a real gap in a scan's coverage, and until now the
    // only evidence was a WARN log line. #220 had to reconstruct which accounts
    // were dropped, and why, by grepping Railway logs — and #226 showed those
    // logs are not even reliable, since Railway drops messages by rate (not by
    // severity) once a replica exceeds 500/sec, taking WARN lines with them.
    //
    // The PK is (user_did, account_did, phase): a re-gather that fails again
    // updates rather than duplicates, so the count keeps meaning "accounts
    // missing from this scan". The same account failing at two different
    // phases is two distinct facts and gets two rows.
    run_migration(conn, 10, |c| {
        c.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS scan_skips (
                user_did        TEXT    NOT NULL,
                account_did     TEXT    NOT NULL,
                phase           TEXT    NOT NULL,
                error           TEXT    NOT NULL,
                skipped_at      TEXT    NOT NULL,
                PRIMARY KEY (user_did, account_did, phase)
            );

            CREATE INDEX IF NOT EXISTS idx_scan_skips_user
                ON scan_skips (user_did);
            ",
        )
    })?;

    // v11 — scan_queue: durable scan admission (#257).
    //
    // Mirrors migrations/postgres/0011_scan_queue.sql. SQLite stores timestamps
    // as TEXT where Postgres uses TIMESTAMPTZ.
    //
    // The SQLite implementation is deliberately minimal — single process, no
    // SKIP LOCKED needed — because #263 will delete this backend entirely.
    // Written to be removed, not maintained.
    //
    // claim_id is the fencing token; see migrations/postgres/0011_scan_queue.sql
    // for why it exists.
    run_migration(conn, 11, |c| {
        c.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS scan_queue (
                user_did        TEXT    NOT NULL PRIMARY KEY,
                status          TEXT    NOT NULL,
                enqueued_at     TEXT    NOT NULL,
                started_at      TEXT,
                finished_at     TEXT,
                lease_expires   TEXT,
                last_error      TEXT,
                claim_id        TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_scan_queue_status_enqueued
                ON scan_queue (status, enqueued_at);
            ",
        )
    })?;

    // v12 — backfill scan_queue.claim_id (#257).
    //
    // v11 was amended in place to add `claim_id` while #257 was still on its
    // branch. A database created from the PRE-amendment v11 has a scan_queue
    // table with no `claim_id` column and version 11 already recorded, so
    // `run_migration` skips v11 and the column is never added — every claim,
    // heartbeat and finish then fails with a missing-column error. Repairing it
    // inside v11 is impossible: v11 is precisely the migration those databases
    // no longer run. Only a new version reaches them.
    //
    // Mirrors migrations/postgres/0012_scan_queue_claim_id.sql. SQLite has no
    // `ADD COLUMN IF NOT EXISTS`, so the column is probed first — and the table
    // itself is probed too, so this stays a no-op rather than an error on any
    // database that somehow lacks scan_queue.
    //
    // On a fresh database v11 runs first and creates the column, so both probes
    // find it already present and v12 does nothing.
    run_migration(conn, 12, |c| {
        let table_exists: bool = c.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master
             WHERE type = 'table' AND name = 'scan_queue'",
            [],
            |row| row.get(0),
        )?;
        if !table_exists {
            return Ok(());
        }

        let has_claim_id: bool = c.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('scan_queue')
             WHERE name = 'claim_id'",
            [],
            |row| row.get(0),
        )?;
        if !has_claim_id {
            c.execute_batch("ALTER TABLE scan_queue ADD COLUMN claim_id TEXT;")?;
        }

        Ok(())
    })?;

    // v13 (#297/#302): per-topic centroid rows + the shadow-compare column.
    // No FK on SQLite — topic_fingerprint was rebuilt via a rename in v4 and
    // rusqlite FK enforcement is off by default; deletes are handled
    // explicitly inside delete_user_data's transaction instead.
    run_migration(conn, 13, |c| {
        c.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS topic_clusters (
                 user_did TEXT NOT NULL,
                 cluster_index INTEGER NOT NULL,
                 centroid TEXT NOT NULL,
                 post_count INTEGER NOT NULL,
                 PRIMARY KEY (user_did, cluster_index)
             );
             ALTER TABLE account_scores ADD COLUMN overlap_legacy REAL;
             COMMIT;",
        )
    })?;

    // v14 (#309): access_requests — DB-backed allowlist for gated onboarding.
    // One row per DID, ever. 'denied' covers both "denied from waitlist" and
    // "revoked after having access"; the waitlist page never distinguishes.
    // Deliberately NOT touched by delete_user_data: this is the admin's
    // grant/deny record (DID + public handle), not user content.
    run_migration(conn, 14, |c| {
        c.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS access_requests (
                 did TEXT PRIMARY KEY,
                 handle TEXT NOT NULL,
                 status TEXT NOT NULL CHECK (status IN ('pending','allowed','denied')),
                 requested_at TEXT NOT NULL,
                 decided_at TEXT,
                 decided_by TEXT
             );
             COMMIT;",
        )
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
        // inferred_pairs, classification_queue, scan_account_input,
        // scan_skips, scan_queue, topic_clusters, access_requests = 14 tables (v14)
        assert_eq!(count, 14i64);
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

        // Verify schema_version has all versions through v14
        let versions: Vec<i64> = conn
            .prepare("SELECT version FROM schema_version ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            versions,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
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
        // inferred_pairs, classification_queue, scan_account_input,
        // scan_skips, scan_queue, topic_clusters, access_requests = 14 tables (v14)
        assert_eq!(count, 14i64);

        // Verify schema_version includes v4 through v14
        let versions: Vec<i64> = conn
            .prepare("SELECT version FROM schema_version ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(
            versions,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
    }

    /// Does `scan_queue` currently have a `claim_id` column?
    fn has_claim_id(conn: &Connection) -> bool {
        conn.query_row(
            "SELECT COUNT(*) > 0 FROM pragma_table_info('scan_queue') WHERE name = 'claim_id'",
            [],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn test_migration_v12_is_a_noop_on_a_fresh_database() {
        // Direction 1: fresh database. v11 creates scan_queue WITH claim_id, so
        // v12 must find nothing to do and must not error or duplicate a column.
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();

        assert!(
            has_claim_id(&conn),
            "v11 should have created claim_id on a fresh database"
        );

        // Exactly one claim_id column — an unconditional ALTER would have thrown
        // rather than produced two, but assert the count so a future "fix" that
        // swallows the error is still caught.
        let claim_id_columns: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('scan_queue') WHERE name = 'claim_id'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(claim_id_columns, 1);

        // And re-running is still clean.
        create_tables(&conn).unwrap();
        assert!(has_claim_id(&conn));
    }

    #[test]
    fn test_migration_v12_adds_claim_id_to_a_pre_amendment_v11_database() {
        // Direction 2: the database this migration exists for. Reconstruct the
        // pre-amendment v11 state by hand — scan_queue with NO claim_id, and
        // version 11 already recorded so the runner skips v11 — then verify
        // create_tables repairs it.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE schema_version (
                version     INTEGER PRIMARY KEY,
                applied_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE scan_queue (
                user_did        TEXT    NOT NULL PRIMARY KEY,
                status          TEXT    NOT NULL,
                enqueued_at     TEXT    NOT NULL,
                started_at      TEXT,
                finished_at     TEXT,
                lease_expires   TEXT,
                last_error      TEXT
            );

            INSERT INTO schema_version (version) VALUES (11);
            ",
        )
        .unwrap();

        // Precondition: this is genuinely the broken shape.
        assert!(
            !has_claim_id(&conn),
            "fixture should start without claim_id — otherwise the test proves nothing"
        );

        create_tables(&conn).unwrap();

        assert!(
            has_claim_id(&conn),
            "v12 should have added claim_id to a pre-amendment v11 database"
        );

        // The column is usable, not just present.
        conn.execute(
            "INSERT INTO scan_queue (user_did, status, enqueued_at, claim_id)
             VALUES ('did:plc:v12', 'queued', '2026-08-06T00:00:00Z', 'token-1')",
            [],
        )
        .unwrap();
        let claim: String = conn
            .query_row(
                "SELECT claim_id FROM scan_queue WHERE user_did = 'did:plc:v12'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(claim, "token-1");
    }

    #[test]
    fn test_migration_v12_is_a_noop_when_scan_queue_is_absent() {
        // Defensive: v11 always runs first in practice, but a v12 that assumed
        // the table exists would be a boot-time failure rather than a skip.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "
            CREATE TABLE schema_version (
                version     INTEGER PRIMARY KEY,
                applied_at  TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            INSERT INTO schema_version (version)
                VALUES (1),(2),(3),(4),(5),(6),(7),(8),(9),(10),(11);
            ",
        )
        .unwrap();

        // Every migration but v12 is already recorded, so v12 is the only one
        // that runs — against a database with no scan_queue table at all.
        create_tables(&conn).unwrap();

        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master
                 WHERE type = 'table' AND name = 'scan_queue'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            !table_exists,
            "fixture should have no scan_queue — otherwise the absent-table path is untested"
        );

        let recorded: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM schema_version WHERE version = 12",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(recorded, "v12 should still record itself as applied");
    }

    #[test]
    fn test_migration_v14_creates_access_requests() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        // Insert exercises every column and the CHECK constraint's happy path.
        conn.execute(
            "INSERT INTO access_requests (did, handle, status, requested_at)
             VALUES ('did:plc:waitlisted', 'w.bsky.social', 'pending', '2026-08-24T00:00:00Z')",
            [],
        )
        .unwrap();
        // The CHECK constraint rejects unknown statuses.
        let err = conn.execute(
            "INSERT INTO access_requests (did, handle, status, requested_at)
             VALUES ('did:plc:bad', 'b.bsky.social', 'banana', '2026-08-24T00:00:00Z')",
            [],
        );
        assert!(err.is_err(), "CHECK constraint must reject invalid status");
    }
}
