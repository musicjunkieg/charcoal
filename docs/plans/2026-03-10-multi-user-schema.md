# Multi-User Schema Redesign — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Scope all database tables and queries per-user (by DID) so multiple
protected users can each have their own fingerprint, scores, events, and scan
state.

**Architecture:** Add a `user_did TEXT NOT NULL` column to every data table
(`topic_fingerprint`, `account_scores`, `amplification_events`, `scan_state`).
Add a `users` table for registration. Thread `user_did` through every
`Database` trait method, both backend implementations, all pipeline code, CLI
commands, and web handlers. Migration v4 backfills existing single-user data
with the configured `BLUESKY_HANDLE`'s resolved DID.

**Tech Stack:** Rust, rusqlite, sqlx-postgres, Axum, async-trait

**Key design decisions:**
- `user_did` is execution context passed as a parameter, NOT a field on
  `AccountScore` or `AmplificationEvent` models (keeps models clean)
- `topic_fingerprint` drops the `id=1 singleton` pattern — becomes one row per
  user, keyed by `user_did`
- `scan_state` becomes composite key `(user_did, key)`
- `account_scores` becomes composite key `(user_did, did)`
- CLI commands resolve `BLUESKY_HANDLE` → DID once at startup
- Web handlers extract `user_did` from `AuthUser` (already set by middleware)
- `CHARCOAL_ALLOWED_DID` stays for now (single-user gate) — multi-user
  open signup is a separate task that replaces this gate

**Build note:** This machine has 1GB RAM. Use `CARGO_BUILD_JOBS=2` for all
cargo commands. Never run the server and build simultaneously.

---

## Task 1: Users Table + Schema Migration v4 (SQLite)

**Files:**
- Modify: `src/db/schema.rs`

**Step 1: Write the migration v4 test**

Add to the `#[cfg(test)] mod tests` block at the bottom of `schema.rs`:

```rust
#[test]
fn test_migration_v4_adds_user_did_columns() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();

    // Verify users table exists
    let has_users: bool = conn
        .query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='users'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(has_users, "users table should exist after migration v4");

    // Verify user_did column on topic_fingerprint
    conn.execute(
        "INSERT INTO users (did, handle) VALUES ('did:plc:test', 'test.bsky.social')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count)
         VALUES ('did:plc:test', '{}', 10)",
        [],
    )
    .unwrap();
    let result: String = conn
        .query_row(
            "SELECT user_did FROM topic_fingerprint WHERE user_did = 'did:plc:test'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(result, "did:plc:test");

    // Verify user_did on account_scores (composite key)
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, posts_analyzed)
         VALUES ('did:plc:test', 'did:plc:target', 'target.bsky.social', 5)",
        [],
    )
    .unwrap();

    // Verify user_did on amplification_events
    conn.execute(
        "INSERT INTO amplification_events (user_did, event_type, amplifier_did,
         amplifier_handle, original_post_uri)
         VALUES ('did:plc:test', 'quote', 'did:plc:amp', 'amp.bsky.social', 'at://post')",
        [],
    )
    .unwrap();

    // Verify scan_state composite key
    conn.execute(
        "INSERT INTO scan_state (user_did, key, value)
         VALUES ('did:plc:test', 'cursor', 'abc123')",
        [],
    )
    .unwrap();
}

#[test]
fn test_migration_v4_updates_table_count() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    let count = table_count(&conn).unwrap();
    // schema_version + users + topic_fingerprint + account_scores +
    // amplification_events + scan_state = 6 tables
    assert_eq!(count, 6i64);
}
```

**Step 2: Run test to verify it fails**

Run: `CARGO_BUILD_JOBS=2 cargo test --lib -- schema::tests::test_migration_v4 -v`
Expected: FAIL (migration v4 doesn't exist yet, no users table)

**Step 3: Implement migration v4 in `create_tables()`**

After the existing migration v3 block (around line 99), add:

```rust
// Migration v4: multi-user schema — add user_did to all data tables,
// create users table, rebuild tables with new composite keys.
run_migration(conn, 4, |c| {
    c.execute_batch(
        "
        -- Users table — one row per protected user
        CREATE TABLE IF NOT EXISTS users (
            did TEXT PRIMARY KEY,
            handle TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        -- Rebuild topic_fingerprint with user_did (replaces singleton id=1 pattern)
        CREATE TABLE IF NOT EXISTS topic_fingerprint_v4 (
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

        -- Rebuild account_scores with user_did composite key
        CREATE TABLE IF NOT EXISTS account_scores_v4 (
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

        -- Add user_did to amplification_events
        ALTER TABLE amplification_events ADD COLUMN user_did TEXT NOT NULL DEFAULT '';

        -- Rebuild scan_state with composite key
        CREATE TABLE IF NOT EXISTS scan_state_v4 (
            user_did TEXT NOT NULL,
            key TEXT NOT NULL,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            PRIMARY KEY (user_did, key)
        );
        INSERT OR IGNORE INTO scan_state_v4 (user_did, key, value, updated_at)
            SELECT '', key, value, updated_at FROM scan_state;
        DROP TABLE scan_state;
        ALTER TABLE scan_state_v4 RENAME TO scan_state;

        -- Rebuild indices with user_did
        DROP INDEX IF EXISTS idx_events_amplifier;
        CREATE INDEX idx_events_amplifier
            ON amplification_events(user_did, amplifier_did);

        DROP INDEX IF EXISTS idx_scores_tier;
        CREATE INDEX idx_scores_tier
            ON account_scores(user_did, threat_tier);

        DROP INDEX IF EXISTS idx_scores_age;
        CREATE INDEX idx_scores_age
            ON account_scores(user_did, scored_at);
        "
    )
})?;
```

Also update the existing `test_table_count` test to expect 6 instead of 5.

**Step 4: Run tests to verify they pass**

Run: `CARGO_BUILD_JOBS=2 cargo test --lib -- schema::tests -v`
Expected: PASS (all schema tests including new v4 migration tests)

**Step 5: Commit**

```bash
git add src/db/schema.rs
git commit -m 'feat(db): add migration v4 — multi-user schema with user_did columns

Adds users table. Rebuilds topic_fingerprint, account_scores, and scan_state
with user_did composite keys. Adds user_did column to amplification_events.
Backfills existing data with empty string (to be updated during user setup).'
```

---

## Task 2: Database Trait — Add `user_did` Parameter

**Files:**
- Modify: `src/db/traits.rs`

**Step 1: Update all trait method signatures**

Replace the entire trait body. Every method that operates on user-scoped data
gets `user_did: &str` as its first data parameter. The three methods that are
truly global (`table_count`, `get_all_scan_state`) keep their signatures, but
`get_all_scan_state` also gets scoped.

```rust
#[async_trait]
pub trait Database: Send + Sync {
    // --- Lifecycle ---
    async fn table_count(&self) -> Result<i64>;

    // --- User management ---
    async fn upsert_user(&self, did: &str, handle: &str) -> Result<()>;

    // --- Scan state ---
    async fn get_scan_state(&self, user_did: &str, key: &str) -> Result<Option<String>>;
    async fn set_scan_state(&self, user_did: &str, key: &str, value: &str) -> Result<()>;
    async fn get_all_scan_state(&self, user_did: &str) -> Result<Vec<(String, String)>>;

    // --- Topic fingerprint ---
    async fn save_fingerprint(&self, user_did: &str, fingerprint_json: &str, post_count: u32) -> Result<()>;
    async fn save_embedding(&self, user_did: &str, embedding: &[f64]) -> Result<()>;
    async fn get_fingerprint(&self, user_did: &str) -> Result<Option<(String, u32, String)>>;
    async fn get_embedding(&self, user_did: &str) -> Result<Option<Vec<f64>>>;

    // --- Account scores ---
    async fn upsert_account_score(&self, user_did: &str, score: &AccountScore) -> Result<()>;
    async fn get_ranked_threats(&self, user_did: &str, min_score: f64) -> Result<Vec<AccountScore>>;
    async fn is_score_stale(&self, user_did: &str, did: &str, max_age_days: i64) -> Result<bool>;

    // --- Amplification events ---
    async fn insert_amplification_event(
        &self,
        user_did: &str,
        event_type: &str,
        amplifier_did: &str,
        amplifier_handle: &str,
        original_post_uri: &str,
        amplifier_post_uri: Option<&str>,
        amplifier_text: Option<&str>,
    ) -> Result<i64>;
    async fn get_recent_events(&self, user_did: &str, limit: u32) -> Result<Vec<AmplificationEvent>>;
    async fn get_events_for_pile_on(&self, user_did: &str) -> Result<Vec<(String, String, String)>>;
    async fn insert_amplification_event_raw(&self, user_did: &str, event: &AmplificationEvent) -> Result<i64>;

    // --- Behavioral context ---
    async fn get_median_engagement(&self, user_did: &str) -> Result<f64>;

    // --- Single-account lookup ---
    async fn get_account_by_handle(&self, user_did: &str, handle: &str) -> Result<Option<AccountScore>>;
    async fn get_account_by_did(&self, user_did: &str, did: &str) -> Result<Option<AccountScore>>;
}
```

**Step 2: Verify compilation fails (expected — impls not updated yet)**

Run: `CARGO_BUILD_JOBS=2 cargo check 2>&1 | head -30`
Expected: FAIL with trait implementation errors

**Step 3: Commit the trait change alone**

```bash
git add src/db/traits.rs
git commit -m 'feat(db): add user_did parameter to all Database trait methods

Every user-scoped method now takes user_did as its first parameter.
Adds upsert_user method for user registration. Implementations will
be updated in the next commit.'
```

---

## Task 3: SQLite Queries — Add `user_did` to All Functions

**Files:**
- Modify: `src/db/queries.rs`

**Step 1: Update all query function signatures and SQL**

Every function gets `user_did: &str` parameter. Every SQL query gets
`WHERE user_did = ?N` or includes `user_did` in INSERT columns.

Key changes by function:

- `get_scan_state(conn, user_did, key)` — `WHERE user_did = ?1 AND key = ?2`
- `set_scan_state(conn, user_did, key, value)` — upsert with `(user_did, key)` composite
- `get_all_scan_state(conn, user_did)` — `WHERE user_did = ?1`
- `save_fingerprint(conn, user_did, json, count)` — `INSERT OR REPLACE INTO topic_fingerprint (user_did, ...)`
- `save_embedding(conn, user_did, json)` — `UPDATE topic_fingerprint SET embedding_vector = ?1 WHERE user_did = ?2`
- `get_fingerprint(conn, user_did)` — `WHERE user_did = ?1`
- `get_embedding(conn, user_did)` — `WHERE user_did = ?1`
- `upsert_account_score(conn, user_did, score)` — composite key `(user_did, did)`
- `get_ranked_threats(conn, user_did, min_score)` — `WHERE user_did = ?1 AND threat_score >= ?2`
- `is_score_stale(conn, user_did, did, max_age_days)` — `WHERE user_did = ?1 AND did = ?2`
- `insert_amplification_event(conn, user_did, ...)` — add `user_did` to INSERT
- `insert_amplification_event_with_detected_at(conn, user_did, event)` — add `user_did` to INSERT
- `get_recent_events(conn, user_did, limit)` — `WHERE user_did = ?1`
- `get_events_for_pile_on(conn, user_did)` — `WHERE user_did = ?1`
- `get_median_engagement(conn, user_did)` — `WHERE user_did = ?1 AND behavioral_signals IS NOT NULL`
- `get_account_by_handle(conn, user_did, handle)` — `WHERE user_did = ?1 AND lower(handle) = lower(?2)`
- `get_account_by_did(conn, user_did, did)` — `WHERE user_did = ?1 AND did = ?2`

Add new function:
- `upsert_user(conn, did, handle)` — `INSERT OR REPLACE INTO users (did, handle) VALUES (?1, ?2)`

**Step 2: Update all tests in `queries.rs`**

Every test needs a `const TEST_USER: &str = "did:plc:testuser000000000000";` and
must pass it to all query calls. Tests that insert data must first insert a user row.

**Step 3: Run tests**

Run: `CARGO_BUILD_JOBS=2 cargo test --lib -- db::queries::tests -v`
Expected: PASS

**Step 4: Commit**

```bash
git add src/db/queries.rs
git commit -m 'feat(db): scope all SQLite queries to user_did

All query functions now take user_did parameter and filter by it.
Composite keys on (user_did, did) for account_scores and
(user_did, key) for scan_state.'
```

---

## Task 4: SQLite Backend — Update Trait Implementation

**Files:**
- Modify: `src/db/sqlite.rs`

**Step 1: Update all `Database` trait method implementations**

Each method passes `user_did` through to the corresponding `queries::*`
function. The pattern is mechanical — add the parameter to each method
signature and forward it.

Add `upsert_user` implementation.

**Step 2: Update all tests in `sqlite.rs`**

Same pattern as queries.rs tests — add `TEST_USER` constant, pass to all calls.

**Step 3: Run tests**

Run: `CARGO_BUILD_JOBS=2 cargo test --lib -- db::sqlite::tests -v`
Expected: PASS

**Step 4: Commit**

```bash
git add src/db/sqlite.rs
git commit -m 'feat(db): update SqliteDatabase trait impl for user_did params

All Database trait methods now forward user_did to query functions.'
```

---

## Task 5: PostgreSQL Backend — Update Trait Implementation

**Files:**
- Modify: `src/db/postgres.rs`
- Create: `migrations/postgres/0004_multiuser.sql`

**Step 1: Create Postgres migration**

```sql
-- Migration v4: multi-user schema
-- Adds user_did column to all data tables, creates users table.

CREATE TABLE IF NOT EXISTS users (
    did TEXT PRIMARY KEY,
    handle TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Add user_did to topic_fingerprint
ALTER TABLE topic_fingerprint ADD COLUMN IF NOT EXISTS user_did TEXT NOT NULL DEFAULT '';
ALTER TABLE topic_fingerprint DROP CONSTRAINT IF EXISTS topic_fingerprint_pkey;
ALTER TABLE topic_fingerprint ADD PRIMARY KEY (user_did);

-- Add user_did to account_scores
ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS user_did TEXT NOT NULL DEFAULT '';
ALTER TABLE account_scores DROP CONSTRAINT IF EXISTS account_scores_pkey;
ALTER TABLE account_scores ADD PRIMARY KEY (user_did, did);

-- Add user_did to amplification_events
ALTER TABLE amplification_events ADD COLUMN IF NOT EXISTS user_did TEXT NOT NULL DEFAULT '';

-- Add user_did to scan_state
ALTER TABLE scan_state ADD COLUMN IF NOT EXISTS user_did TEXT NOT NULL DEFAULT '';
ALTER TABLE scan_state DROP CONSTRAINT IF EXISTS scan_state_pkey;
ALTER TABLE scan_state ADD PRIMARY KEY (user_did, key);

-- Rebuild indices with user_did
DROP INDEX IF EXISTS idx_events_amplifier;
CREATE INDEX idx_events_amplifier ON amplification_events(user_did, amplifier_did);

DROP INDEX IF EXISTS idx_scores_tier;
CREATE INDEX idx_scores_tier ON account_scores(user_did, threat_tier);

DROP INDEX IF EXISTS idx_scores_age;
CREATE INDEX idx_scores_age ON account_scores(user_did, scored_at);

INSERT INTO schema_version (version)
VALUES (4) ON CONFLICT DO NOTHING;
```

**Step 2: Update PgDatabase trait implementation**

All methods get `user_did` parameter. All SQL queries get `WHERE user_did = $N`
or include `user_did` in INSERT. Same mechanical pattern as SQLite.

Add `upsert_user` implementation using `INSERT ... ON CONFLICT (did) DO UPDATE`.

Update the `run_migrations` function to include `0004_multiuser.sql`.

**Step 3: Run check (no live Postgres needed for compilation)**

Run: `CARGO_BUILD_JOBS=2 cargo check --features postgres`
Expected: PASS (compiles)

**Step 4: Commit**

```bash
git add src/db/postgres.rs migrations/postgres/0004_multiuser.sql
git commit -m 'feat(db): update PgDatabase for multi-user schema

Adds migration 0004_multiuser.sql. All PgDatabase trait methods
now accept and filter by user_did.'
```

---

## Task 6: Pipeline Code — Thread `user_did` Through

**Files:**
- Modify: `src/pipeline/amplification.rs`
- Modify: `src/pipeline/sweep.rs`

**Step 1: Update `amplification::run()` signature**

Add `user_did: &str` parameter. Pass it to all `db.set_scan_state()`,
`db.insert_amplification_event()`, and `db.get_scan_state()` calls.

**Step 2: Update `sweep::run()` signature**

Add `user_did: &str` parameter. Pass it to all `db.is_score_stale()`,
`db.upsert_account_score()`, `db.get_events_for_pile_on()`, and
`db.get_median_engagement()` calls.

**Step 3: Check for other pipeline files**

Search `src/pipeline/` for any other files with DB calls and update them too.

**Step 4: Verify compilation (will fail — callers not updated yet)**

Run: `CARGO_BUILD_JOBS=2 cargo check 2>&1 | head -30`
Expected: errors in `main.rs` and `scan_job.rs` (callers of pipeline functions)

**Step 5: Commit**

```bash
git add src/pipeline/
git commit -m 'feat(pipeline): thread user_did through amplification and sweep

Both pipeline entry points now accept user_did and pass it to all
DB operations. Callers will be updated in the next commit.'
```

---

## Task 7: CLI Commands — Resolve Handle to DID, Thread Through

**Files:**
- Modify: `src/main.rs`

**Step 1: Add DID resolution helper**

Near the top of main (or in a helper function), add a pattern that resolves
the configured `BLUESKY_HANDLE` to a DID. This DID becomes `user_did` for
all DB calls from CLI commands.

```rust
/// Resolve the configured handle to a DID for user-scoped DB operations.
async fn resolve_user_did(client: &PublicAtpClient, config: &Config) -> Result<String> {
    config.require_bluesky()?;
    let did = client.resolve_handle(&config.bluesky_handle).await?;
    Ok(did)
}
```

**Step 2: Update every CLI command that calls DB methods**

For each command (`fingerprint`, `scan`, `sweep`, `score`, `report`, `status`,
`validate`):
1. Resolve handle → DID at the start
2. Call `db.upsert_user(&did, &config.bluesky_handle)` to ensure user exists
3. Pass `&did` to all DB calls and pipeline functions

The `migrate` command needs special handling — it must iterate all rows from the
source DB and assign them the resolved DID in the target.

**Step 3: Verify compilation**

Run: `CARGO_BUILD_JOBS=2 cargo check 2>&1 | head -30`
Expected: May still fail if web handlers aren't updated

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m 'feat(cli): resolve handle to DID for all user-scoped operations

CLI commands now resolve BLUESKY_HANDLE to a DID at startup and
thread it through all pipeline and DB calls.'
```

---

## Task 8: Web Handlers — Extract `user_did` from Auth Context

**Files:**
- Modify: `src/web/handlers/accounts.rs`
- Modify: `src/web/handlers/events.rs`
- Modify: `src/web/handlers/fingerprint.rs`
- Modify: `src/web/handlers/status.rs`
- Modify: `src/web/handlers/scan.rs`
- Modify: `src/web/scan_job.rs`

**Step 1: Add `AuthUser` extractor to all protected handlers**

The `require_auth` middleware already inserts `AuthUser { did }` into request
extensions. Each handler needs to extract it. Use Axum's `Extension` extractor
directly in the handler signature:

```rust
async fn list_accounts(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
    Query(params): Query<AccountParams>,
) -> impl IntoResponse {
    let threats = state.db.get_ranked_threats(&auth.did, 0.0).await;
    // ...
}
```

**Step 2: Update each handler to pass `user_did` to DB calls**

- `accounts.rs`: `get_ranked_threats(&auth.did, ...)`, `get_account_by_handle(&auth.did, ...)`
- `events.rs`: `get_recent_events(&auth.did, ...)`
- `fingerprint.rs`: `get_fingerprint(&auth.did)`
- `status.rs`: `get_ranked_threats(&auth.did, ...)` for tier counts
- `scan.rs`: pass `auth.did` to `launch_scan()`

**Step 3: Update `scan_job.rs`**

`launch_scan()` and `run_scan()` both need `user_did: String` parameter.
All DB calls inside pass it through.

**Step 4: Verify compilation**

Run: `CARGO_BUILD_JOBS=2 cargo check --features web`
Expected: PASS

**Step 5: Commit**

```bash
git add src/web/handlers/ src/web/scan_job.rs
git commit -m 'feat(web): extract user_did from auth context in all handlers

All protected handlers now extract AuthUser.did and pass it to
DB calls. Scan job accepts user_did for background operations.'
```

---

## Task 9: Update All Tests

**Files:**
- Modify: `tests/web_oauth.rs`
- Modify: `tests/db_postgres.rs`
- Verify: `tests/composition.rs` (likely no changes — pure functions)
- Verify: `tests/unit_oauth.rs` (likely no changes — token tests)
- Verify: `tests/unit_scoring.rs` (likely no changes — pure scoring)
- Verify: `tests/unit_behavioral.rs` (likely no changes — pure functions)
- Verify: `tests/unit_topics.rs` (likely no changes — pure functions)
- Verify: `tests/unit_constellation.rs` (likely no changes — pure functions)

**Step 1: Update `web_oauth.rs`**

Handler tests that hit protected routes need the DB calls to work with
`user_did`. The test helper `test_app()` or `test_app_with_state()` may
need to pre-insert a user row.

**Step 2: Update `db_postgres.rs`**

All 8 tests need `user_did` parameter on every DB call. Add `TEST_USER`
constant.

**Step 3: Run the full test suite**

Run: `CARGO_BUILD_JOBS=2 cargo test --all-targets --features web`
Expected: PASS (all 215+ tests)

**Step 4: Run clippy**

Run: `CARGO_BUILD_JOBS=2 cargo clippy --all-targets --features web`
Expected: No warnings

**Step 5: Commit**

```bash
git add tests/
git commit -m 'test: update all integration tests for multi-user schema

All DB-touching tests now pass user_did to trait methods.
Handler tests pre-insert user rows for auth context.'
```

---

## Task 10: Update Migrate Command

**Files:**
- Modify: `src/main.rs` (the `migrate` subcommand)

**Step 1: Update the migrate command**

When transferring data from SQLite to Postgres, the migrate command must:
1. Resolve the current user's DID
2. Pass it when reading from source DB (`get_all_scan_state(user_did)`, etc.)
3. Pass it when writing to target DB
4. Ensure the user row exists in the target (`upsert_user`)

**Step 2: Test manually (if Postgres is available)**

Run: `DATABASE_URL=... CARGO_BUILD_JOBS=2 cargo run --features postgres -- migrate`

**Step 3: Commit**

```bash
git add src/main.rs
git commit -m 'feat(migrate): thread user_did through SQLite→Postgres migration

Migrate command now resolves the protected user DID and passes it
when reading from source and writing to target database.'
```

---

## Task 11: Update CLAUDE.md and Documentation

**Files:**
- Modify: `CLAUDE.md`

**Step 1: Update the schema description**

Update the "Database architecture" section to reflect:
- `users` table
- `user_did` columns on all tables
- DB schema at v4
- Updated test count

**Step 2: Update the "Current status" section**

Add a bullet for the multi-user schema redesign.

**Step 3: Commit**

```bash
git add CLAUDE.md
git commit -m 'docs: update CLAUDE.md for multi-user schema (v4)'
```

---

## Execution Order and Dependencies

```
Task 1 (schema)
    ↓
Task 2 (trait)
    ↓
Task 3 (queries) → Task 4 (sqlite impl)
    ↓
Task 5 (postgres)
    ↓
Task 6 (pipelines)
    ↓
Task 7 (CLI) ←→ Task 8 (web handlers)  [can be parallel]
    ↓
Task 9 (tests)
    ↓
Task 10 (migrate)
    ↓
Task 11 (docs)
```

Tasks 1-6 are strictly sequential (each depends on the previous).
Tasks 7 and 8 can be done in parallel. Tasks 9-11 come last.

**Total estimated scope:** ~17 files modified, ~1500 lines changed.
