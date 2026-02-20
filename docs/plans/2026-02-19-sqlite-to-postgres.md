# SQLite to PostgreSQL Migration — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add PostgreSQL support alongside SQLite via a trait-based abstraction, enabling multi-user server deployment while preserving CLI mode.

**Architecture:** Trait-based dual backend. A `Database` async trait defines all 11 query operations. `SqliteDatabase` wraps the existing `rusqlite::Connection` in a `tokio::sync::Mutex` and delegates to the current `queries.rs` functions (zero SQL duplication). `PgDatabase` uses `sqlx::PgPool` with native async queries, `TIMESTAMPTZ` timestamps, `JSONB` for structured data, and `pgvector` for embeddings. Feature flags (`sqlite` default, `postgres` optional) control which backends compile. `Arc<dyn Database>` is the handle type passed through pipelines.

**Tech Stack:** rusqlite 0.38 (bundled), sqlx 0.8 (postgres + macros + chrono), pgvector 0.4, async-trait 0.1, tokio::sync::Mutex

**Chainlink Issue:** #48

**Branch:** `feat/sqlite-to-postgres`

---

## Phase 1: Trait Foundation (no behavior changes)

### Task 1: Add feature flags to Cargo.toml

**Files:**
- Modify: `Cargo.toml`

**Step 1: Add features section and make sqlx + pgvector optional dependencies**

Add a `[features]` section and the new optional deps. Keep `rusqlite` as a non-optional dependency (existing tests need it unconditionally).

```toml
[features]
default = ["sqlite"]
sqlite = []
postgres = ["dep:sqlx", "dep:pgvector"]

# Add under [dependencies]:
sqlx = { version = "0.8", features = ["runtime-tokio-rustls", "postgres", "macros", "chrono", "json"], optional = true }
pgvector = { version = "0.4", features = ["sqlx"], optional = true }
```

**Step 2: Verify compilation**

Run: `cargo check`
Expected: Clean compilation, no warnings

**Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: add feature flags and optional sqlx/pgvector deps for postgres support"
```

---

### Task 2: Create the Database trait

**Files:**
- Create: `src/db/traits.rs`
- Modify: `src/db/mod.rs` (add `pub mod traits;`)

**Step 1: Write the Database trait**

Create `src/db/traits.rs` with all 11 operations matching the current `queries.rs` function signatures, plus `initialize` and `table_count`:

```rust
// Database trait — backend-agnostic async interface for all DB operations.
//
// Implementors: SqliteDatabase (wraps rusqlite), PgDatabase (wraps sqlx).
// All methods are async so both sync (rusqlite) and async (sqlx) backends fit.

use anyhow::Result;
use async_trait::async_trait;

use super::models::{AccountScore, AmplificationEvent};

#[async_trait]
pub trait Database: Send + Sync {
    // --- Lifecycle ---
    async fn table_count(&self) -> Result<i64>;

    // --- Scan state ---
    async fn get_scan_state(&self, key: &str) -> Result<Option<String>>;
    async fn set_scan_state(&self, key: &str, value: &str) -> Result<()>;

    // --- Topic fingerprint ---
    async fn save_fingerprint(&self, fingerprint_json: &str, post_count: u32) -> Result<()>;
    async fn save_embedding(&self, embedding: &[f64]) -> Result<()>;
    async fn get_fingerprint(&self) -> Result<Option<(String, u32, String)>>;
    async fn get_embedding(&self) -> Result<Option<Vec<f64>>>;

    // --- Account scores ---
    async fn upsert_account_score(&self, score: &AccountScore) -> Result<()>;
    async fn get_ranked_threats(&self, min_score: f64) -> Result<Vec<AccountScore>>;
    async fn is_score_stale(&self, did: &str, max_age_days: i64) -> Result<bool>;

    // --- Amplification events ---
    async fn insert_amplification_event(
        &self,
        event_type: &str,
        amplifier_did: &str,
        amplifier_handle: &str,
        original_post_uri: &str,
        amplifier_post_uri: Option<&str>,
        amplifier_text: Option<&str>,
    ) -> Result<i64>;
    async fn get_recent_events(&self, limit: u32) -> Result<Vec<AmplificationEvent>>;
    async fn get_events_for_pile_on(&self) -> Result<Vec<(String, String, String)>>;

    // --- Behavioral context ---
    async fn get_median_engagement(&self) -> Result<f64>;
}
```

**Step 2: Register the module in `src/db/mod.rs`**

Add `pub mod traits;` after the existing module declarations. Also add `pub use traits::Database;` for convenience.

**Step 3: Verify compilation**

Run: `cargo check`
Expected: Clean compilation. The trait has no implementors yet — that's fine.

Run: `cargo test --all-targets`
Expected: All 178 tests pass (nothing changed functionally).

**Step 4: Commit**

```bash
git add src/db/traits.rs src/db/mod.rs
git commit -m "feat: add Database trait defining async interface for all DB operations"
```

---

### Task 3: Create SqliteDatabase wrapper

**Files:**
- Create: `src/db/sqlite.rs`
- Modify: `src/db/mod.rs` (add `pub mod sqlite;` and factory functions)

**Step 1: Write the SqliteDatabase struct**

Create `src/db/sqlite.rs`. The key design: wrap `rusqlite::Connection` in `tokio::sync::Mutex` (making it `Send + Sync`), then each trait method locks the mutex and delegates to the existing free functions in `queries.rs`.

```rust
// SqliteDatabase — rusqlite backend implementing the Database trait.
//
// The Connection is wrapped in tokio::sync::Mutex because Connection is !Send.
// Trait methods lock the mutex, do synchronous rusqlite work, and return.
// The lock is never held across .await points — Rust enforces this because
// MutexGuard is !Send.
//
// The free functions in queries.rs remain unchanged so existing tests
// continue to work against Connection directly.

use anyhow::Result;
use async_trait::async_trait;
use tokio::sync::Mutex;
use rusqlite::Connection;

use super::models::{AccountScore, AmplificationEvent};
use super::traits::Database;

pub struct SqliteDatabase {
    conn: Mutex<Connection>,
}

impl SqliteDatabase {
    /// Wrap an already-opened rusqlite Connection.
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Mutex::new(conn),
        }
    }
}

#[async_trait]
impl Database for SqliteDatabase {
    async fn table_count(&self) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::schema::table_count(&conn)
    }

    async fn get_scan_state(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        super::queries::get_scan_state(&conn, key)
    }

    async fn set_scan_state(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::set_scan_state(&conn, key, value)
    }

    async fn save_fingerprint(&self, fingerprint_json: &str, post_count: u32) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::save_fingerprint(&conn, fingerprint_json, post_count)
    }

    async fn save_embedding(&self, embedding: &[f64]) -> Result<()> {
        let json = serde_json::to_string(embedding)?;
        let conn = self.conn.lock().await;
        super::queries::save_embedding(&conn, &json)
    }

    async fn get_fingerprint(&self) -> Result<Option<(String, u32, String)>> {
        let conn = self.conn.lock().await;
        super::queries::get_fingerprint(&conn)
    }

    async fn get_embedding(&self) -> Result<Option<Vec<f64>>> {
        let conn = self.conn.lock().await;
        super::queries::get_embedding(&conn)
    }

    async fn upsert_account_score(&self, score: &AccountScore) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_account_score(&conn, score)
    }

    async fn get_ranked_threats(&self, min_score: f64) -> Result<Vec<AccountScore>> {
        let conn = self.conn.lock().await;
        super::queries::get_ranked_threats(&conn, min_score)
    }

    async fn is_score_stale(&self, did: &str, max_age_days: i64) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::is_score_stale(&conn, did, max_age_days)
    }

    async fn insert_amplification_event(
        &self,
        event_type: &str,
        amplifier_did: &str,
        amplifier_handle: &str,
        original_post_uri: &str,
        amplifier_post_uri: Option<&str>,
        amplifier_text: Option<&str>,
    ) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::insert_amplification_event(
            &conn,
            event_type,
            amplifier_did,
            amplifier_handle,
            original_post_uri,
            amplifier_post_uri,
            amplifier_text,
        )
    }

    async fn get_recent_events(&self, limit: u32) -> Result<Vec<AmplificationEvent>> {
        let conn = self.conn.lock().await;
        super::queries::get_recent_events(&conn, limit)
    }

    async fn get_events_for_pile_on(&self) -> Result<Vec<(String, String, String)>> {
        let conn = self.conn.lock().await;
        super::queries::get_events_for_pile_on(&conn)
    }

    async fn get_median_engagement(&self) -> Result<f64> {
        let conn = self.conn.lock().await;
        super::queries::get_median_engagement(&conn)
    }
}
```

**Step 2: Add factory functions to `src/db/mod.rs`**

Add after the existing `open()` function:

```rust
pub mod sqlite;
pub mod traits;

pub use traits::Database;

use std::sync::Arc;

/// Open SQLite database and return it as a trait object.
pub fn open_sqlite(db_path: &str) -> Result<Arc<dyn Database>> {
    let conn = open(db_path)?;
    Ok(Arc::new(sqlite::SqliteDatabase::new(conn)))
}

/// Initialize SQLite database and return it as a trait object.
pub fn initialize_sqlite(db_path: &str) -> Result<Arc<dyn Database>> {
    let conn = initialize(db_path)?;
    Ok(Arc::new(sqlite::SqliteDatabase::new(conn)))
}
```

**Step 3: Verify compilation and tests**

Run: `cargo check`
Expected: Clean compilation.

Run: `cargo test --all-targets`
Expected: All 178 tests still pass.

**Step 4: Commit**

```bash
git add src/db/sqlite.rs src/db/mod.rs
git commit -m "feat: add SqliteDatabase trait impl wrapping rusqlite in Mutex"
```

---

### Task 4: Write tests for the SqliteDatabase trait wrapper

**Files:**
- Modify: `src/db/sqlite.rs` (add `#[cfg(test)]` module)

**Step 1: Write failing tests**

Add tests at the bottom of `src/db/sqlite.rs` that exercise the trait methods through `SqliteDatabase`. These tests confirm the Mutex wrapper works and the trait delegation is correct:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::create_tables;

    async fn test_db() -> SqliteDatabase {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        SqliteDatabase::new(conn)
    }

    #[tokio::test]
    async fn test_trait_scan_state_roundtrip() {
        let db = test_db().await;
        assert_eq!(db.get_scan_state("cursor").await.unwrap(), None);
        db.set_scan_state("cursor", "abc123").await.unwrap();
        assert_eq!(
            db.get_scan_state("cursor").await.unwrap(),
            Some("abc123".to_string())
        );
    }

    #[tokio::test]
    async fn test_trait_fingerprint_roundtrip() {
        let db = test_db().await;
        assert!(db.get_fingerprint().await.unwrap().is_none());
        db.save_fingerprint(r#"{"topics": []}"#, 100).await.unwrap();
        let (json, count, _) = db.get_fingerprint().await.unwrap().unwrap();
        assert_eq!(json, r#"{"topics": []}"#);
        assert_eq!(count, 100);
    }

    #[tokio::test]
    async fn test_trait_embedding_roundtrip() {
        let db = test_db().await;
        db.save_fingerprint(r#"{"clusters":[]}"#, 50).await.unwrap();
        assert!(db.get_embedding().await.unwrap().is_none());
        db.save_embedding(&[0.1, 0.2, 0.3]).await.unwrap();
        let loaded = db.get_embedding().await.unwrap().unwrap();
        assert_eq!(loaded.len(), 3);
        assert!((loaded[0] - 0.1).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_trait_account_score_upsert_and_rank() {
        let db = test_db().await;
        let score = AccountScore {
            did: "did:plc:abc".to_string(),
            handle: "test.bsky.social".to_string(),
            toxicity_score: Some(0.8),
            topic_overlap: Some(0.3),
            threat_score: Some(65.0),
            threat_tier: Some("Elevated".to_string()),
            posts_analyzed: 20,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
        };
        db.upsert_account_score(&score).await.unwrap();
        let ranked = db.get_ranked_threats(0.0).await.unwrap();
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].handle, "test.bsky.social");
    }

    #[tokio::test]
    async fn test_trait_amplification_event() {
        let db = test_db().await;
        let id = db
            .insert_amplification_event(
                "quote",
                "did:plc:xyz",
                "troll.bsky.social",
                "at://did:plc:me/app.bsky.feed.post/abc",
                Some("at://did:plc:xyz/app.bsky.feed.post/def"),
                Some("lol look at this"),
            )
            .await
            .unwrap();
        assert!(id > 0);
        let events = db.get_recent_events(10).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "quote");
    }

    #[tokio::test]
    async fn test_trait_table_count() {
        let db = test_db().await;
        let count = db.table_count().await.unwrap();
        assert_eq!(count, 5);
    }

    #[tokio::test]
    async fn test_trait_median_engagement_empty() {
        let db = test_db().await;
        let median = db.get_median_engagement().await.unwrap();
        assert!((median - 0.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn test_trait_is_score_stale_missing() {
        let db = test_db().await;
        assert!(db.is_score_stale("did:plc:missing", 7).await.unwrap());
    }
}
```

**Step 2: Run tests**

Run: `cargo test --all-targets`
Expected: All 178 existing tests + 8 new trait tests pass (186 total).

**Step 3: Commit**

```bash
git add src/db/sqlite.rs
git commit -m "test: add async trait wrapper tests for SqliteDatabase"
```

---

## Phase 2: Wire Pipelines to the Trait

### Task 5: Update pipeline/amplification.rs to use `&dyn Database`

**Files:**
- Modify: `src/pipeline/amplification.rs`

**Step 1: Replace Connection with trait reference**

Changes:
1. Replace `use rusqlite::Connection;` with `use crate::db::Database;` and `use std::sync::Arc;`
2. Change the `run()` signature: `conn: &Connection` → `db: &Arc<dyn Database>`
3. Replace every `queries::*(&conn, ...)` call with `db.*(...).await?`
4. Remove `use crate::db::queries;`

The specific call sites to change:
- Line 58: `queries::set_scan_state(conn, ...)` → `db.set_scan_state(...).await?`
- Line 93: `queries::insert_amplification_event(conn, ...)` → `db.insert_amplification_event(...).await?`
- Line 163: `queries::is_score_stale(conn, ...)` → `db.is_score_stale(...).await` (inside a filter — needs to become a pre-filter loop since `.filter()` can't be async)
- Line 214: `queries::upsert_account_score(conn, ...)` → `db.upsert_account_score(...).await?`

**Important:** The `is_score_stale` call is inside an iterator `.filter()` closure, which can't be async. Convert the filter to a pre-collection loop:

```rust
// Before:
let stale_followers: Vec<_> = follower_list
    .iter()
    .filter(|f| f.handle != protected_handle)
    .filter(|f| queries::is_score_stale(conn, &f.did, 7).unwrap_or(true))
    .collect();

// After:
let mut stale_followers = Vec::new();
for f in follower_list.iter().filter(|f| f.handle != protected_handle) {
    if db.is_score_stale(&f.did, 7).await.unwrap_or(true) {
        stale_followers.push(f);
    }
}
```

**Step 2: Verify compilation**

Run: `cargo check`
Expected: May fail — `main.rs` still passes `&Connection`. That's expected. We'll fix main.rs in Task 8.

**Step 3: Commit (even if main.rs doesn't compile yet — the module is internally consistent)**

```bash
git add src/pipeline/amplification.rs
git commit -m "refactor: update amplification pipeline to use Database trait"
```

---

### Task 6: Update pipeline/sweep.rs to use `&dyn Database`

**Files:**
- Modify: `src/pipeline/sweep.rs`

**Step 1: Same changes as amplification.rs**

1. Replace `use rusqlite::Connection;` with `use crate::db::Database;` and `use std::sync::Arc;`
2. Change `conn: &Connection` → `db: &Arc<dyn Database>`
3. Convert the `is_score_stale` filter to an async loop (same pattern as Task 5)
4. Add `.await` to `db.upsert_account_score(&score)`
5. Remove `use crate::db::queries;`

**Step 2: Commit**

```bash
git add src/pipeline/sweep.rs
git commit -m "refactor: update sweep pipeline to use Database trait"
```

---

### Task 7: Update status.rs to use `&dyn Database`

**Files:**
- Modify: `src/status.rs`

**Step 1: Make `show()` accept `&dyn Database` and become async**

The current `show()` opens its own connection with `db::open()`. Change it to receive an `Arc<dyn Database>` from the caller and use trait methods:

```rust
use crate::db::Database;
use std::sync::Arc;

/// Display system status to the terminal.
pub async fn show(db: &Arc<dyn Database>, db_display_path: &str) -> Result<()> {
    // Database info
    let file_size = std::fs::metadata(db_display_path)
        .map(|m| format_bytes(m.len()))
        .unwrap_or_else(|_| "unknown".to_string());
    println!("Database: {} ({})", db_display_path, file_size);

    // Fingerprint status
    match db.get_fingerprint().await? {
        Some((_json, post_count, updated_at)) => { /* unchanged display */ }
        None => { /* unchanged display */ }
    }

    // Scored accounts
    let all_scores = db.get_ranked_threats(0.0).await?;
    // ... unchanged display logic

    // Recent events
    let events = db.get_recent_events(5).await?;
    // ... unchanged display logic

    // Last scan
    match db.get_scan_state("notifications_cursor").await? {
        // ... unchanged display logic
    }

    Ok(())
}
```

Remove the `HasDbPath` trait — it's no longer needed since the caller passes the DB handle. The `db_display_path` param is just for the display string.

**Step 2: Commit**

```bash
git add src/status.rs
git commit -m "refactor: update status module to use Database trait"
```

---

### Task 8: Update main.rs to use `Arc<dyn Database>`

**Files:**
- Modify: `src/main.rs`

This is the largest single task. Every command handler that touches the DB needs updating.

**Step 1: Update all command handlers**

For each command that currently does `let conn = charcoal::db::open(&config.db_path)?;`:
- Replace with `let db = charcoal::db::open_sqlite(&config.db_path)?;`
- Replace `&conn` in pipeline calls with `&db`
- Replace direct `charcoal::db::queries::*(&conn, ...)` calls with `db.*(...).await?`

For `Commands::Init`:
- Replace `charcoal::db::initialize(&config.db_path)?` with `charcoal::db::initialize_sqlite(&config.db_path)?`
- Replace `charcoal::db::schema::table_count(&conn)?` with `db.table_count().await?`

For `Commands::Fingerprint`:
- Replace `charcoal::db::queries::get_fingerprint(&conn)?` with `db.get_fingerprint().await?`
- Replace `charcoal::db::queries::save_fingerprint(&conn, ...)` with `db.save_fingerprint(...).await?`
- Replace `charcoal::db::queries::save_embedding(&conn, &emb_json)?` with `db.save_embedding(&mean_emb).await?` (pass slice, not JSON — trait handles serialization)

For `Commands::Scan`, `Sweep`, `Score`, `Validate`:
- Replace `charcoal::db::queries::get_median_engagement(&conn)?` with `db.get_median_engagement().await?`
- Replace `charcoal::db::queries::get_events_for_pile_on(&conn)?` with `db.get_events_for_pile_on().await?`
- Replace `charcoal::db::queries::upsert_account_score(&conn, &score)?` with `db.upsert_account_score(&score).await?`

For `Commands::Report`:
- Replace `charcoal::db::queries::get_ranked_threats(&conn, ...)` with `db.get_ranked_threats(...).await?`
- Replace `charcoal::db::queries::get_recent_events(&conn, ...)` with `db.get_recent_events(...).await?`
- Replace `charcoal::db::queries::get_fingerprint(&conn)?` with `db.get_fingerprint().await?`

For `Commands::Status`:
- Replace `charcoal::status::show(&config)?` with `charcoal::status::show(&db, &config.db_path).await?`

**Step 2: Update `load_fingerprint` and `load_embedder` helper functions**

```rust
async fn load_fingerprint(
    db: &std::sync::Arc<dyn charcoal::db::Database>,
) -> Result<charcoal::topics::fingerprint::TopicFingerprint> {
    match db.get_fingerprint().await? {
        Some((json, _, _)) => Ok(serde_json::from_str(&json)?),
        None => anyhow::bail!(
            "No topic fingerprint found. Run `charcoal fingerprint` first."
        ),
    }
}

async fn load_embedder(
    config: &config::Config,
    db: &std::sync::Arc<dyn charcoal::db::Database>,
) -> (
    Option<charcoal::topics::embeddings::SentenceEmbedder>,
    Option<Vec<f64>>,
) {
    let embed_dir = charcoal::toxicity::download::embedding_model_dir(&config.model_dir);
    let embedder = if charcoal::toxicity::download::embedding_files_present(&config.model_dir) {
        match charcoal::topics::embeddings::SentenceEmbedder::load(&embed_dir) {
            Ok(e) => { info!("Loaded sentence embedding model"); Some(e) }
            Err(e) => { warn!("Failed to load embedding model: {e}"); None }
        }
    } else {
        None
    };

    let embedding = match db.get_embedding().await {
        Ok(Some(v)) => Some(v),
        Ok(None) => {
            if embedder.is_some() {
                warn!("Embedding model loaded but no stored embedding.");
            }
            None
        }
        Err(e) => { warn!("Failed to load stored embedding: {e}"); None }
    };

    (embedder, embedding)
}
```

**Step 3: Remove unused HasDbPath trait impl**

The `impl charcoal::status::HasDbPath for Config` block can be removed since `status::show()` no longer uses the trait.

**Step 4: Verify compilation and tests**

Run: `cargo check`
Expected: Clean compilation.

Run: `cargo test --all-targets`
Expected: All 186 tests pass.

Run: `cargo clippy`
Expected: No warnings.

**Step 5: Commit**

```bash
git add src/main.rs
git commit -m "refactor: wire all CLI commands through Database trait"
```

---

### Task 9: Clean up status.rs HasDbPath trait

**Files:**
- Modify: `src/status.rs` (remove `HasDbPath` trait if no longer used)

**Step 1: Check if `HasDbPath` is still referenced anywhere**

Run: `grep -rn "HasDbPath" src/`

If it's only defined and used in main.rs (which we already updated), remove the trait definition from `status.rs`.

**Step 2: Run tests**

Run: `cargo test --all-targets`
Expected: All tests pass.

**Step 3: Commit**

```bash
git add src/status.rs
git commit -m "refactor: remove unused HasDbPath trait from status module"
```

---

### Task 10: Run full validation

**Step 1: Format, lint, test**

```bash
cargo fmt
cargo clippy --all-targets
cargo test --all-targets
```

Expected: All pass clean. This is the Phase 2 checkpoint — the application now uses the Database trait throughout, with SQLite as the only backend.

**Step 2: Commit any formatting fixes**

```bash
git add -A src/
git commit -m "style: apply cargo fmt after trait migration"
```

---

## Phase 3: PostgreSQL Backend

### Task 11: Create PostgreSQL migration files

**Files:**
- Create: `migrations/postgres/0001_initial.sql`
- Create: `migrations/postgres/0002_pgvector.sql`
- Create: `migrations/postgres/0003_behavioral_signals.sql`

**Step 1: Write the initial schema**

`migrations/postgres/0001_initial.sql`:
```sql
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE IF NOT EXISTS schema_version (
    version INTEGER PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS topic_fingerprint (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    fingerprint_json TEXT NOT NULL,
    post_count INTEGER NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS account_scores (
    did TEXT PRIMARY KEY,
    handle TEXT NOT NULL,
    toxicity_score DOUBLE PRECISION,
    topic_overlap DOUBLE PRECISION,
    threat_score DOUBLE PRECISION,
    threat_tier TEXT,
    posts_analyzed INTEGER NOT NULL DEFAULT 0,
    top_toxic_posts JSONB,
    scored_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE IF NOT EXISTS amplification_events (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_type TEXT NOT NULL,
    amplifier_did TEXT NOT NULL,
    amplifier_handle TEXT NOT NULL,
    original_post_uri TEXT NOT NULL,
    amplifier_post_uri TEXT,
    amplifier_text TEXT,
    detected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    followers_fetched BOOLEAN NOT NULL DEFAULT FALSE,
    followers_scored BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE TABLE IF NOT EXISTS scan_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_events_amplifier ON amplification_events(amplifier_did);
CREATE INDEX IF NOT EXISTS idx_scores_tier ON account_scores(threat_tier);
CREATE INDEX IF NOT EXISTS idx_scores_age ON account_scores(scored_at);

INSERT INTO schema_version (version) VALUES (1) ON CONFLICT DO NOTHING;
```

**Step 2: Write pgvector migration**

`migrations/postgres/0002_pgvector.sql`:
```sql
ALTER TABLE topic_fingerprint ADD COLUMN IF NOT EXISTS embedding_vector vector(384);
INSERT INTO schema_version (version) VALUES (2) ON CONFLICT DO NOTHING;
```

**Step 3: Write behavioral signals migration**

`migrations/postgres/0003_behavioral_signals.sql`:
```sql
ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS behavioral_signals JSONB;
INSERT INTO schema_version (version) VALUES (3) ON CONFLICT DO NOTHING;
```

**Step 4: Commit**

```bash
git add migrations/
git commit -m "feat: add PostgreSQL schema migrations with pgvector and JSONB"
```

---

### Task 12: Create PgDatabase implementation

**Files:**
- Create: `src/db/postgres.rs`
- Modify: `src/db/mod.rs` (add conditional module and factory)

**Step 1: Write the PgDatabase struct**

Create `src/db/postgres.rs` with all 11 trait methods using sqlx. Key differences from SQLite:
- `NOW()` instead of `datetime('now')`
- `$1, $2` parameters (sqlx handles this)
- `pgvector::Vector` for embeddings
- `RETURNING id` for insert
- `JSONB` for behavioral_signals
- `percentile_cont` for median engagement
- `to_char()` for timestamp display formatting
- `information_schema.tables` instead of `sqlite_master`

Note: Use `sqlx::query()` with `.bind()` (runtime API) rather than `sqlx::query!()` (compile-time macro) to avoid requiring DATABASE_URL at compile time. This keeps the build simple for contributors.

**Step 2: Add conditional module to `src/db/mod.rs`**

```rust
#[cfg(feature = "postgres")]
pub mod postgres;

#[cfg(feature = "postgres")]
pub use postgres::PgDatabase;

/// Connect to Postgres and return a trait object.
#[cfg(feature = "postgres")]
pub async fn connect_postgres(database_url: &str) -> Result<Arc<dyn Database>> {
    let db = postgres::PgDatabase::connect(database_url).await?;
    Ok(Arc::new(db))
}
```

**Step 3: Verify it compiles with the feature flag**

Run: `cargo check --features postgres`
Expected: Clean compilation.

Run: `cargo test --all-targets` (without --features postgres)
Expected: All existing tests still pass.

**Step 4: Commit**

```bash
git add src/db/postgres.rs src/db/mod.rs
git commit -m "feat: add PgDatabase implementation with pgvector and JSONB"
```

---

### Task 13: Wire Postgres into config and main.rs

**Files:**
- Modify: `src/main.rs` (binary only)
- Modify: `src/main.rs::config` module

**Step 1: Add DATABASE_URL to config**

Add a `database_url: Option<String>` field to Config. When `DATABASE_URL` is set and starts with `postgres://`, use the Postgres backend.

```rust
pub database_url: Option<String>,
```

In `Config::load()`:
```rust
database_url: env::var("DATABASE_URL").ok(),
```

**Step 2: Add backend selection to main.rs**

Create a helper function that selects the backend:

```rust
async fn open_database(config: &config::Config) -> Result<Arc<dyn charcoal::db::Database>> {
    if let Some(ref url) = config.database_url {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            #[cfg(feature = "postgres")]
            return charcoal::db::connect_postgres(url).await;
            #[cfg(not(feature = "postgres"))]
            anyhow::bail!(
                "DATABASE_URL points to PostgreSQL but the 'postgres' feature is not compiled in.\n\
                 Rebuild with: cargo build --features postgres"
            );
        }
    }
    charcoal::db::open_sqlite(&config.db_path)
}
```

Replace `charcoal::db::open_sqlite(&config.db_path)?` in command handlers with `open_database(&config).await?`.

**Step 3: Verify compilation both ways**

Run: `cargo check`
Run: `cargo check --features postgres`
Both should compile clean.

Run: `cargo test --all-targets`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: add runtime PostgreSQL backend selection via DATABASE_URL"
```

---

### Task 14: Add PostgreSQL integration tests

**Files:**
- Create: `tests/db_postgres.rs`

**Step 1: Write integration tests gated on the postgres feature and DATABASE_URL**

```rust
//! PostgreSQL integration tests — only run when:
//! 1. Compiled with --features postgres
//! 2. DATABASE_URL env var points to a live Postgres instance

#![cfg(feature = "postgres")]

use charcoal::db::{connect_postgres, Database};

fn skip_without_database_url() -> Option<String> {
    std::env::var("DATABASE_URL").ok().filter(|u| u.starts_with("postgres"))
}

#[tokio::test]
async fn test_pg_scan_state_roundtrip() {
    let Some(url) = skip_without_database_url() else { return; };
    let db = connect_postgres(&url).await.unwrap();
    // Test scan state CRUD
    db.set_scan_state("test_key", "test_value").await.unwrap();
    let val = db.get_scan_state("test_key").await.unwrap();
    assert_eq!(val, Some("test_value".to_string()));
    // Clean up (would need a cleanup strategy for shared test DB)
}

// Additional tests for fingerprint, scores, events, etc.
```

**Step 2: Run Postgres tests (requires a live instance)**

Run: `DATABASE_URL=postgres://charcoal:charcoal@localhost/charcoal_test cargo test --all-targets --features postgres`

**Step 3: Commit**

```bash
git add tests/db_postgres.rs
git commit -m "test: add PostgreSQL integration tests (gated on feature + env var)"
```

---

## Phase 4: Data Migration Command

### Task 15: Add `charcoal migrate` CLI command

**Files:**
- Modify: `src/main.rs` (add Migrate subcommand)

**Step 1: Add the Migrate command variant**

```rust
/// Migrate data from SQLite to PostgreSQL
#[cfg(feature = "postgres")]
Migrate {
    /// PostgreSQL connection URL
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,
},
```

**Step 2: Implement the migration handler**

Read all data from SQLite, write to Postgres:

```rust
Commands::Migrate { database_url } => {
    let config = config::Config::load()?;

    println!("Migrating data from SQLite to PostgreSQL...");

    // Open source (SQLite)
    let sqlite_db = charcoal::db::open_sqlite(&config.db_path)?;

    // Open destination (Postgres)
    let pg_db = charcoal::db::connect_postgres(&database_url).await?;

    // Migrate fingerprint
    if let Some((json, count, _)) = sqlite_db.get_fingerprint().await? {
        pg_db.save_fingerprint(&json, count).await?;
        println!("  Fingerprint migrated");
    }

    // Migrate embedding
    if let Some(embedding) = sqlite_db.get_embedding().await? {
        pg_db.save_embedding(&embedding).await?;
        println!("  Embedding migrated ({}-dim)", embedding.len());
    }

    // Migrate account scores
    let scores = sqlite_db.get_ranked_threats(0.0).await?;
    for score in &scores {
        pg_db.upsert_account_score(score).await?;
    }
    println!("  {} account scores migrated", scores.len());

    // Migrate events
    let events = sqlite_db.get_recent_events(u32::MAX).await?;
    for event in &events {
        pg_db.insert_amplification_event(
            &event.event_type,
            &event.amplifier_did,
            &event.amplifier_handle,
            &event.original_post_uri,
            event.amplifier_post_uri.as_deref(),
            event.amplifier_text.as_deref(),
        ).await?;
    }
    println!("  {} amplification events migrated", events.len());

    println!("\nMigration complete. Set DATABASE_URL in your .env to use PostgreSQL.");
}
```

**Step 3: Verify compilation**

Run: `cargo check --features postgres`
Expected: Clean compilation.

**Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat: add migrate command to transfer data from SQLite to PostgreSQL"
```

---

## Phase 5: Documentation and Cleanup

### Task 16: Update documentation

**Files:**
- Modify: `CLAUDE.md` (add PostgreSQL backend docs)
- Modify: `.env.example` (add DATABASE_URL)

**Step 1: Update .env.example**

Add:
```
# Database backend (SQLite is default, PostgreSQL for server deployment)
# DATABASE_URL=postgres://charcoal:password@localhost/charcoal
```

**Step 2: Update CLAUDE.md**

Add a section documenting:
- The Database trait and dual-backend architecture
- Feature flags (`--features postgres`)
- How to set up PostgreSQL locally for development
- The `charcoal migrate` command

**Step 3: Commit**

```bash
git add CLAUDE.md .env.example
git commit -m "docs: add PostgreSQL backend documentation"
```

---

## Summary: All Tasks

| Phase | Task | Description | Est. Time |
|-------|------|-------------|-----------|
| 1 | 1 | Feature flags in Cargo.toml | 10 min |
| 1 | 2 | Database trait definition | 15 min |
| 1 | 3 | SqliteDatabase wrapper | 20 min |
| 1 | 4 | Trait wrapper tests | 15 min |
| 2 | 5 | Update amplification pipeline | 15 min |
| 2 | 6 | Update sweep pipeline | 10 min |
| 2 | 7 | Update status.rs | 15 min |
| 2 | 8 | Update main.rs (largest task) | 45 min |
| 2 | 9 | Clean up HasDbPath | 5 min |
| 2 | 10 | Full validation | 10 min |
| 3 | 11 | PostgreSQL migrations | 15 min |
| 3 | 12 | PgDatabase implementation | 45 min |
| 3 | 13 | Wire Postgres into config | 20 min |
| 3 | 14 | PostgreSQL integration tests | 20 min |
| 4 | 15 | Migrate command | 30 min |
| 5 | 16 | Documentation | 15 min |

**Total: ~5 hours of focused implementation time**

**Critical checkpoints:**
- After Task 4: Trait layer works, all 186+ tests pass, no behavior change
- After Task 10: Entire app uses the trait, SQLite-only, all tests pass
- After Task 12: Postgres backend compiles with `--features postgres`
- After Task 14: Postgres backend tested against live database
