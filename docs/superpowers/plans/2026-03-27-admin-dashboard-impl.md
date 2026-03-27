# Admin Dashboard Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an admin dashboard that lets Bryan pre-seed Bluesky users by handle, trigger scans on their behalf, and view their scored data via impersonation.

**Architecture:** Env-var-based admin identity (`CHARCOAL_ADMIN_DIDS`). Admin API endpoints gated by `require_admin` middleware. Impersonation via `?as_user=` query param on all existing endpoints, with `AuthUser` expanded to carry both original and effective DIDs. Per-user scan status tracking with global one-at-a-time gate. Single `/admin` page in SvelteKit frontend.

**Tech Stack:** Rust (Axum), SvelteKit 5, SQLite/PostgreSQL dual backend, AT Protocol API

**Spec:** `docs/superpowers/specs/2026-03-27-admin-dashboard-design.md`

---

## File Structure

### New Files

| File | Responsibility |
|------|---------------|
| `src/web/handlers/admin.rs` | Admin API handlers: list users, pre-seed, trigger scan, delete user, get identity |
| `migrations/postgres/0007_last_login_at.sql` | Postgres migration for `last_login_at` column |
| `web/src/routes/(protected)/admin/+page.svelte` | Admin page: pre-seed form, user table, scan controls |
| `tests/unit_admin.rs` | Backend tests for admin middleware and endpoints |

### Modified Files

| File | Changes |
|------|---------|
| `src/config.rs` | Add `admin_dids` field to Config struct |
| `src/web/auth.rs` | Add `did_is_admin`, `resolve_effective_did`, add `as_user` middleware logic |
| `src/web/mod.rs` | Move `AuthUser` struct expansion (add `effective_did`, `is_admin`), add admin routes, refactor `AppState` to use `ScanManager` |
| `src/web/mod.rs` | Add `DELETE` to CORS allowed methods |
| `src/web/handlers/mod.rs` | Add `pub mod admin;` |
| `src/web/handlers/scan.rs` | Update to use `ScanManager` instead of single `ScanStatus` |
| `src/web/scan_job.rs` | Add `ScanManager` struct, replace `ScanStatus` in `launch_scan` signature |
| `src/db/traits.rs` | Add 5 new trait methods |
| `src/db/queries.rs` | Add query implementations for new trait methods |
| `src/db/sqlite.rs` | Add trait impl delegations for new methods |
| `src/db/postgres.rs` | Add trait impl for new methods |
| `src/db/schema.rs` | Add migration v7 (`last_login_at` column) |
| `src/web/handlers/oauth.rs` | Call `update_last_login` after `upsert_user` |
| `web/src/lib/types.ts` | Add admin TypeScript interfaces |
| `web/src/lib/api.ts` | Add admin API functions, add `as_user` param support |
| `web/src/routes/(protected)/+layout.svelte` | Add Admin nav link (conditional), impersonation banner |

---

## Chunk 1: Config, Auth Middleware, and Schema Migration

### Task 1: Add `admin_dids` to Config

**Files:**
- Modify: `src/config.rs:19-52` (Config struct), `src/config.rs:59-97` (Config::load), `src/config.rs:165-188` (test_defaults)

- [ ] **Step 1: Write failing test for admin_dids config parsing**

In `src/config.rs`, add a test at the bottom of the existing test module:

```rust
#[test]
fn test_admin_dids_parsing() {
    let config = Config::test_defaults();
    assert!(config.admin_dids.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features web test_admin_dids_parsing`
Expected: FAIL — `admin_dids` field does not exist on Config

- [ ] **Step 3: Add `admin_dids` field to Config**

Add field to Config struct (after `allowed_did` at ~line 43):
```rust
    #[cfg(feature = "web")]
    pub admin_dids: String,
```

Add to `Config::load()` (after `allowed_did` loading at ~line 71):
```rust
    let admin_dids = env::var("CHARCOAL_ADMIN_DIDS").unwrap_or_default();
```

Add to struct initialization in `Config::load()` return value:
```rust
    admin_dids,
```

Add to `test_defaults()` (after `allowed_did` at ~line 182):
```rust
    admin_dids: String::new(),
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --features web test_admin_dids_parsing`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m 'feat: add CHARCOAL_ADMIN_DIDS config field'
```

---

### Task 2: Expand AuthUser and add require_admin middleware

**Files:**
- Modify: `src/web/mod.rs:239-242` (AuthUser struct), `src/web/auth.rs:120-128` (did_is_allowed), `src/web/auth.rs:134-155` (require_auth)

- [ ] **Step 1: Write failing tests for admin middleware**

Create test file `tests/unit_admin.rs`:

```rust
//! Tests for admin authorization middleware and impersonation.

#[cfg(feature = "web")]
mod admin_tests {
    use charcoal::config::Config;
    use charcoal::web::auth::{did_is_admin, AuthUser};

    #[test]
    fn test_did_is_admin_with_matching_did() {
        assert!(did_is_admin("did:plc:admin1", "did:plc:admin1"));
    }

    #[test]
    fn test_did_is_admin_with_comma_separated_list() {
        assert!(did_is_admin("did:plc:admin2", "did:plc:admin1,did:plc:admin2"));
    }

    #[test]
    fn test_did_is_admin_non_admin() {
        assert!(!did_is_admin("did:plc:user1", "did:plc:admin1"));
    }

    #[test]
    fn test_did_is_admin_empty_list() {
        assert!(!did_is_admin("did:plc:user1", ""));
    }

    #[test]
    fn test_auth_user_not_impersonating() {
        let auth = AuthUser {
            did: "did:plc:me".to_string(),
            effective_did: "did:plc:me".to_string(),
            is_admin: false,
        };
        assert!(!auth.is_impersonating());
    }

    #[test]
    fn test_auth_user_impersonating() {
        let auth = AuthUser {
            did: "did:plc:admin".to_string(),
            effective_did: "did:plc:other".to_string(),
            is_admin: true,
        };
        assert!(auth.is_impersonating());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features web --test unit_admin`
Expected: FAIL — `did_is_admin` doesn't exist, `AuthUser` fields don't match

- [ ] **Step 3: Expand AuthUser struct**

In `src/web/mod.rs`, replace the AuthUser struct (~line 239):

```rust
#[derive(Debug, Clone)]
pub struct AuthUser {
    /// The DID of the authenticated user (from session cookie)
    pub did: String,
    /// The effective DID for DB queries (impersonated DID, or same as `did`)
    pub effective_did: String,
    /// Whether this user is an admin
    pub is_admin: bool,
}

impl AuthUser {
    /// Returns true if the user is viewing as someone else
    pub fn is_impersonating(&self) -> bool {
        self.did != self.effective_did
    }
}
```

- [ ] **Step 4: Add `did_is_admin` function**

In `src/web/auth.rs`, after `did_is_allowed` (~line 128):

```rust
/// Check if a DID is in the admin allowlist (comma-separated).
/// Returns false if admin_dids is empty (no admins configured).
pub fn did_is_admin(did: &str, admin_dids: &str) -> bool {
    if admin_dids.is_empty() {
        return false;
    }
    admin_dids
        .split(',')
        .map(|s| s.trim())
        .any(|admin_did| constant_time_eq(did, admin_did))
}
```

- [ ] **Step 5: Update `require_auth` middleware to populate new AuthUser fields**

In `require_auth` (~line 134), after DID extraction and allowlist check, update the `AuthUser` insertion:

```rust
    let is_admin = did_is_admin(&did, &state.config.admin_dids);

    request.extensions_mut().insert(AuthUser {
        did: did.clone(),
        effective_did: did,
        is_admin,
    });
```

Note: `as_user` impersonation logic is added in a later task — for now, `effective_did` always equals `did`.

- [ ] **Step 6: Update all existing handlers that use `auth.did`**

All existing handlers currently access `auth.did`. Update them to use `auth.effective_did` for DB queries. The handlers are in:
- `src/web/handlers/scan.rs` — `trigger_scan`: use `auth.did` (scans are for the authenticated user, not impersonated)
- `src/web/handlers/status.rs` — use `auth.effective_did`
- `src/web/handlers/accounts.rs` — use `auth.effective_did`
- `src/web/handlers/events.rs` — use `auth.effective_did`
- `src/web/handlers/fingerprint.rs` — use `auth.effective_did`
- `src/web/handlers/labels.rs` — use `auth.effective_did` for reads, but reject writes if impersonating

For label writes, add at top of `upsert_label`:
```rust
    if auth.is_impersonating() {
        return Err(StatusCode::BAD_REQUEST);
    }
```

- [ ] **Step 7: Verify all handlers use correct DID field**

Run: `grep -rn 'auth\.did' src/web/handlers/` to find all usages. Verify:
- Read handlers (status, accounts, events, fingerprint, labels reads, review, accuracy) use `auth.effective_did`
- Write handlers (scan trigger, label upsert) use `auth.did` and reject impersonation

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --features web --test unit_admin`
Expected: PASS (6 tests)

Run: `cargo test --features web`
Expected: All existing tests still pass (AuthUser struct change is backwards-compatible because we set effective_did = did)

- [ ] **Step 8: Commit**

```bash
git add src/web/auth.rs src/web/handlers/ tests/unit_admin.rs
git commit -m 'feat: expand AuthUser with effective_did and is_admin fields'
```

---

### Task 3: Schema v7 — add `last_login_at` to users table

**Files:**
- Modify: `src/db/schema.rs:262-267` (after last migration)
- Create: `migrations/postgres/0007_last_login_at.sql`

- [ ] **Step 1: Write failing test for last_login_at column**

In `tests/unit_admin.rs`, add:

```rust
#[cfg(feature = "web")]
mod schema_tests {
    use charcoal::db::sqlite::SqliteDatabase;
    use charcoal::db::Database;
    use rusqlite::Connection;

    #[tokio::test]
    async fn test_users_table_has_last_login_at() {
        let conn = Connection::open_in_memory().unwrap();
        charcoal::db::schema::create_tables(&conn).unwrap();
        let db = SqliteDatabase::new(conn);
        // update_last_login should work without error
        db.update_last_login("did:plc:test").await.ok();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features web --test unit_admin schema_tests`
Expected: FAIL — `update_last_login` doesn't exist

- [ ] **Step 3: Add migration v7 to SQLite schema**

In `src/db/schema.rs`, after the v6 migration block (~line 267):

```rust
    // Migration v7: add last_login_at to users table
    run_migration(conn, 7, |c| {
        c.execute_batch("ALTER TABLE users ADD COLUMN last_login_at TEXT;")?;
        Ok(())
    })?;
```

- [ ] **Step 4: Add Postgres migration file**

Create `migrations/postgres/0007_last_login_at.sql`:

```sql
ALTER TABLE users ADD COLUMN IF NOT EXISTS last_login_at TIMESTAMPTZ;
```

Load it in `src/db/postgres.rs` alongside existing migration includes and apply in the migration runner.

- [ ] **Step 5: Do NOT commit yet — Task 4 adds the trait methods needed to compile. Continue to Task 4.**

---

### Task 4: Add new Database trait methods

**Files:**
- Modify: `src/db/traits.rs:17-182` (Database trait)
- Modify: `src/db/queries.rs` (add query functions)
- Modify: `src/db/sqlite.rs` (add trait impls)
- Modify: `src/db/postgres.rs` (add trait impls)

- [ ] **Step 1: Write failing tests for new trait methods**

In `tests/unit_admin.rs`, add:

```rust
#[cfg(feature = "web")]
mod db_tests {
    use charcoal::db::sqlite::SqliteDatabase;
    use charcoal::db::Database;
    use rusqlite::Connection;

    async fn setup_db() -> SqliteDatabase {
        let conn = Connection::open_in_memory().unwrap();
        charcoal::db::schema::create_tables(&conn).unwrap();
        SqliteDatabase::new(conn)
    }

    #[tokio::test]
    async fn test_list_users_empty() {
        let db = setup_db().await;
        let users = db.list_users().await.unwrap();
        assert!(users.is_empty());
    }

    #[tokio::test]
    async fn test_list_users_after_upsert() {
        let db = setup_db().await;
        db.upsert_user("did:plc:abc", "alice.bsky.social").await.unwrap();
        db.upsert_user("did:plc:def", "bob.bsky.social").await.unwrap();
        let users = db.list_users().await.unwrap();
        assert_eq!(users.len(), 2);
    }

    #[tokio::test]
    async fn test_has_fingerprint_false() {
        let db = setup_db().await;
        db.upsert_user("did:plc:abc", "alice.bsky.social").await.unwrap();
        assert!(!db.has_fingerprint("did:plc:abc").await.unwrap());
    }

    #[tokio::test]
    async fn test_get_scored_account_count_zero() {
        let db = setup_db().await;
        let count = db.get_scored_account_count("did:plc:abc").await.unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn test_update_last_login() {
        let db = setup_db().await;
        db.upsert_user("did:plc:abc", "alice.bsky.social").await.unwrap();
        db.update_last_login("did:plc:abc").await.unwrap();
        let users = db.list_users().await.unwrap();
        assert!(users[0].last_login_at.is_some());
    }

    #[tokio::test]
    async fn test_delete_user_data() {
        let db = setup_db().await;
        db.upsert_user("did:plc:abc", "alice.bsky.social").await.unwrap();
        db.delete_user_data("did:plc:abc").await.unwrap();
        let users = db.list_users().await.unwrap();
        assert!(users.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features web --test unit_admin db_tests`
Expected: FAIL — methods don't exist on Database trait

- [ ] **Step 3: Add UserRow model**

In `src/db/models.rs`, add:

```rust
/// A row from the users table, used by admin endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct UserRow {
    pub did: String,
    pub handle: String,
    pub created_at: String,
    pub last_login_at: Option<String>,
}
```

- [ ] **Step 4: Add trait methods to Database trait**

In `src/db/traits.rs`, add after the last method:

```rust
    /// List all users in the system.
    async fn list_users(&self) -> Result<Vec<UserRow>>;

    /// Count scored accounts for a user.
    async fn get_scored_account_count(&self, user_did: &str) -> Result<i64>;

    /// Check if a topic fingerprint exists for a user.
    async fn has_fingerprint(&self, user_did: &str) -> Result<bool>;

    /// Delete all data for a user (cascade across all user-scoped tables).
    async fn delete_user_data(&self, user_did: &str) -> Result<()>;

    /// Update last_login_at timestamp for a user.
    async fn update_last_login(&self, did: &str) -> Result<()>;
```

- [ ] **Step 5: Add query implementations**

In `src/db/queries.rs`, add:

```rust
pub fn list_users(conn: &Connection) -> Result<Vec<UserRow>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, created_at, last_login_at FROM users ORDER BY created_at DESC"
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(UserRow {
            did: row.get(0)?,
            handle: row.get(1)?,
            created_at: row.get(2)?,
            last_login_at: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn get_scored_account_count(conn: &Connection, user_did: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM account_scores WHERE user_did = ?1 AND threat_score IS NOT NULL",
        params![user_did],
        |row| row.get(0),
    )?;
    Ok(count)
}

pub fn has_fingerprint(conn: &Connection, user_did: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM topic_fingerprint WHERE user_did = ?1",
        params![user_did],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

pub fn delete_user_data(conn: &Connection, user_did: &str) -> Result<()> {
    // Delete in FK-safe order
    conn.execute("DELETE FROM inferred_pairs WHERE user_did = ?1", params![user_did])?;
    conn.execute("DELETE FROM user_labels WHERE user_did = ?1", params![user_did])?;
    conn.execute("DELETE FROM amplification_events WHERE user_did = ?1", params![user_did])?;
    conn.execute("DELETE FROM account_scores WHERE user_did = ?1", params![user_did])?;
    conn.execute("DELETE FROM scan_state WHERE user_did = ?1", params![user_did])?;
    conn.execute("DELETE FROM topic_fingerprint WHERE user_did = ?1", params![user_did])?;
    conn.execute("DELETE FROM users WHERE did = ?1", params![user_did])?;
    Ok(())
}

pub fn update_last_login(conn: &Connection, did: &str) -> Result<()> {
    conn.execute(
        "UPDATE users SET last_login_at = datetime('now') WHERE did = ?1",
        params![did],
    )?;
    Ok(())
}
```

- [ ] **Step 6: Add SQLite trait impls**

In `src/db/sqlite.rs`, add to the `impl Database for SqliteDatabase` block:

```rust
    async fn list_users(&self) -> Result<Vec<UserRow>> {
        let conn = self.conn.lock().await;
        super::queries::list_users(&conn)
    }

    async fn get_scored_account_count(&self, user_did: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::get_scored_account_count(&conn, user_did)
    }

    async fn has_fingerprint(&self, user_did: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::has_fingerprint(&conn, user_did)
    }

    async fn delete_user_data(&self, user_did: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::delete_user_data(&conn, user_did)
    }

    async fn update_last_login(&self, did: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::update_last_login(&conn, did)
    }
```

- [ ] **Step 7: Add Postgres trait impls**

In `src/db/postgres.rs`, add to the `impl Database for PgDatabase` block:

```rust
    async fn list_users(&self) -> Result<Vec<UserRow>> {
        let rows = sqlx_core::query_as::<_, (String, String, String, Option<String>)>(
            "SELECT did, handle, created_at::text, last_login_at::text FROM users ORDER BY created_at DESC"
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(did, handle, created_at, last_login_at)| {
            UserRow { did, handle, created_at, last_login_at }
        }).collect())
    }

    async fn get_scored_account_count(&self, user_did: &str) -> Result<i64> {
        let (count,): (i64,) = sqlx_core::query_as(
            "SELECT COUNT(*) FROM account_scores WHERE user_did = $1 AND threat_score IS NOT NULL"
        )
        .bind(user_did)
        .fetch_one(&self.pool)
        .await?;
        Ok(count)
    }

    async fn has_fingerprint(&self, user_did: &str) -> Result<bool> {
        let (count,): (i64,) = sqlx_core::query_as(
            "SELECT COUNT(*) FROM topic_fingerprint WHERE user_did = $1"
        )
        .bind(user_did)
        .fetch_one(&self.pool)
        .await?;
        Ok(count > 0)
    }

    async fn delete_user_data(&self, user_did: &str) -> Result<()> {
        // Delete in FK-safe order
        sqlx_core::query("DELETE FROM inferred_pairs WHERE user_did = $1").bind(user_did).execute(&self.pool).await?;
        sqlx_core::query("DELETE FROM user_labels WHERE user_did = $1").bind(user_did).execute(&self.pool).await?;
        sqlx_core::query("DELETE FROM amplification_events WHERE user_did = $1").bind(user_did).execute(&self.pool).await?;
        sqlx_core::query("DELETE FROM account_scores WHERE user_did = $1").bind(user_did).execute(&self.pool).await?;
        sqlx_core::query("DELETE FROM scan_state WHERE user_did = $1").bind(user_did).execute(&self.pool).await?;
        sqlx_core::query("DELETE FROM topic_fingerprint WHERE user_did = $1").bind(user_did).execute(&self.pool).await?;
        sqlx_core::query("DELETE FROM users WHERE did = $1").bind(user_did).execute(&self.pool).await?;
        Ok(())
    }

    async fn update_last_login(&self, did: &str) -> Result<()> {
        sqlx_core::query("UPDATE users SET last_login_at = NOW() WHERE did = $1")
            .bind(did)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
```

- [ ] **Step 8: Run all tests**

Run: `cargo test --features web`
Expected: All tests pass, including new admin db tests

- [ ] **Step 9: Commit**

```bash
git add src/db/traits.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs src/db/models.rs src/db/schema.rs migrations/postgres/0007_last_login_at.sql tests/unit_admin.rs
git commit -m 'feat: schema v7 + Database trait methods for admin dashboard'
```

---

### Task 5: Update OAuth callback to set last_login_at

**Files:**
- Modify: `src/web/handlers/oauth.rs:450-457`

- [ ] **Step 1: Add `update_last_login` call after `upsert_user`**

In `src/web/handlers/oauth.rs`, after the `upsert_user` call (~line 452):

```rust
    // Update last login timestamp
    if let Err(e) = state.db.update_last_login(&authenticated_did).await {
        tracing::warn!("Failed to update last_login_at: {e}");
    }
```

This is non-fatal — if it fails, the user can still log in.

- [ ] **Step 2: Run tests**

Run: `cargo test --features web`
Expected: All tests pass

- [ ] **Step 3: Commit**

```bash
git add src/web/handlers/oauth.rs
git commit -m 'feat: update last_login_at on OAuth callback'
```

---

## Chunk 2: ScanManager Refactor and Admin API Endpoints

### Task 6: Refactor ScanStatus to ScanManager

**Files:**
- Modify: `src/web/scan_job.rs:28-38` (ScanStatus struct)
- Modify: `src/web/mod.rs:38-51` (AppState)
- Modify: `src/web/handlers/scan.rs` (trigger_scan)
- Modify: `src/web/handlers/status.rs` (get_status)

- [ ] **Step 1: Write failing test for ScanManager**

In `tests/unit_admin.rs`, add:

```rust
#[cfg(feature = "web")]
mod scan_manager_tests {
    use charcoal::web::scan_job::ScanManager;

    #[test]
    fn test_scan_manager_starts_empty() {
        let mgr = ScanManager::new();
        assert!(!mgr.is_any_running());
    }

    #[test]
    fn test_scan_manager_try_start_succeeds() {
        let mut mgr = ScanManager::new();
        assert!(mgr.try_start_scan("did:plc:abc").is_ok());
        assert!(mgr.is_any_running());
    }

    #[test]
    fn test_scan_manager_try_start_rejects_second() {
        let mut mgr = ScanManager::new();
        mgr.try_start_scan("did:plc:abc").unwrap();
        assert!(mgr.try_start_scan("did:plc:def").is_err());
    }

    #[test]
    fn test_scan_manager_finish_allows_next() {
        let mut mgr = ScanManager::new();
        mgr.try_start_scan("did:plc:abc").unwrap();
        mgr.finish_scan("did:plc:abc");
        assert!(mgr.try_start_scan("did:plc:def").is_ok());
    }

    #[test]
    fn test_scan_manager_per_user_status() {
        let mut mgr = ScanManager::new();
        mgr.try_start_scan("did:plc:abc").unwrap();
        let status = mgr.get_status("did:plc:abc");
        assert!(status.is_some());
        assert!(status.unwrap().running);
        assert!(mgr.get_status("did:plc:other").is_none());
    }

    #[test]
    fn test_fingerprint_building_tracking() {
        let mut mgr = ScanManager::new();
        assert!(!mgr.is_fingerprint_building("did:plc:abc"));
        mgr.start_fingerprint_build("did:plc:abc");
        assert!(mgr.is_fingerprint_building("did:plc:abc"));
        mgr.finish_fingerprint_build("did:plc:abc");
        assert!(!mgr.is_fingerprint_building("did:plc:abc"));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features web --test unit_admin scan_manager_tests`
Expected: FAIL — `ScanManager` doesn't exist

- [ ] **Step 3: Implement ScanManager**

In `src/web/scan_job.rs`, add above `ScanStatus`:

```rust
use std::collections::{HashMap, HashSet};

/// Manages per-user scan status with a global one-at-a-time gate.
pub struct ScanManager {
    statuses: HashMap<String, ScanStatus>,
    fingerprint_building: HashSet<String>,
    any_running: bool,
}

impl ScanManager {
    pub fn new() -> Self {
        Self {
            statuses: HashMap::new(),
            fingerprint_building: HashSet::new(),
            any_running: false,
        }
    }

    /// Atomically check the global gate and start a scan.
    /// Returns Err if a scan is already running.
    pub fn try_start_scan(&mut self, user_did: &str) -> Result<(), String> {
        if self.any_running {
            return Err("A scan is already running".to_string());
        }
        self.any_running = true;
        self.statuses.insert(user_did.to_string(), ScanStatus {
            running: true,
            started_at: Some(chrono::Utc::now().to_rfc3339()),
            progress_message: "Starting scan...".to_string(),
            last_error: None,
        });
        Ok(())
    }

    /// Mark a scan as finished.
    pub fn finish_scan(&mut self, user_did: &str) {
        self.any_running = false;
        if let Some(status) = self.statuses.get_mut(user_did) {
            status.running = false;
        }
    }

    /// Get scan status for a specific user.
    pub fn get_status(&self, user_did: &str) -> Option<&ScanStatus> {
        self.statuses.get(user_did)
    }

    /// Get mutable scan status for updating progress.
    pub fn get_status_mut(&mut self, user_did: &str) -> Option<&mut ScanStatus> {
        self.statuses.get_mut(user_did)
    }

    pub fn is_any_running(&self) -> bool {
        self.any_running
    }

    pub fn is_scan_running_for(&self, user_did: &str) -> bool {
        self.statuses.get(user_did).map_or(false, |s| s.running)
    }

    pub fn start_fingerprint_build(&mut self, user_did: &str) {
        self.fingerprint_building.insert(user_did.to_string());
    }

    pub fn finish_fingerprint_build(&mut self, user_did: &str) {
        self.fingerprint_building.remove(user_did);
    }

    pub fn is_fingerprint_building(&self, user_did: &str) -> bool {
        self.fingerprint_building.contains(user_did)
    }
}
```

- [ ] **Step 4: Update AppState to use ScanManager**

In `src/web/mod.rs`, replace the `scan_status` field in `AppState`:

```rust
    pub scan_manager: Arc<RwLock<ScanManager>>,
```

Update `run_server()` to initialize it:
```rust
    scan_manager: Arc::new(RwLock::new(ScanManager::new())),
```

- [ ] **Step 5: Update trigger_scan and get_status handlers**

Update `src/web/handlers/scan.rs` to use `scan_manager` instead of `scan_status`:

```rust
    let mut mgr = state.scan_manager.write().await;
    if let Err(msg) = mgr.try_start_scan(&auth.did) {
        return (StatusCode::CONFLICT, Json(json!({"error": msg}))).into_response();
    }
```

Update `launch_scan` signature in `src/web/scan_job.rs` to accept `Arc<RwLock<ScanManager>>` and `user_did`. Update progress reporting to use `mgr.get_status_mut(user_did)`.

Update `src/web/handlers/status.rs` to read from `scan_manager.get_status(&auth.effective_did)`.

- [ ] **Step 6: Run all tests**

Run: `cargo test --features web`
Expected: All tests pass

- [ ] **Step 7: Commit**

```bash
git add src/web/scan_job.rs src/web/mod.rs src/web/handlers/scan.rs src/web/handlers/status.rs tests/unit_admin.rs
git commit -m 'refactor: replace ScanStatus with per-user ScanManager'
```

---

### Task 7: Add impersonation (`as_user`) middleware logic

**Files:**
- Modify: `src/web/auth.rs:134-155` (require_auth)

- [ ] **Step 1: Write failing tests for as_user logic**

In `tests/unit_admin.rs`, add:

```rust
#[cfg(feature = "web")]
mod impersonation_tests {
    use charcoal::web::auth::resolve_effective_did;

    #[test]
    fn test_no_as_user_returns_own_did() {
        let result = resolve_effective_did("did:plc:me", true, None);
        assert_eq!(result.unwrap(), "did:plc:me");
    }

    #[test]
    fn test_admin_with_as_user_returns_target() {
        let result = resolve_effective_did("did:plc:me", true, Some("did:plc:other"));
        assert_eq!(result.unwrap(), "did:plc:other");
    }

    #[test]
    fn test_non_admin_with_as_user_returns_error() {
        let result = resolve_effective_did("did:plc:me", false, Some("did:plc:other"));
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --features web --test unit_admin impersonation_tests`
Expected: FAIL — `resolve_effective_did` doesn't exist

- [ ] **Step 3: Add `resolve_effective_did` function**

In `src/web/auth.rs`:

```rust
/// Resolve the effective DID for a request.
/// If as_user is provided and requester is admin, returns the as_user DID.
/// If as_user is provided but requester is not admin, returns Err.
/// Otherwise returns the requester's own DID.
pub fn resolve_effective_did(
    own_did: &str,
    is_admin: bool,
    as_user: Option<&str>,
) -> Result<String, &'static str> {
    match as_user {
        Some(target_did) => {
            if !is_admin {
                return Err("Only admins can use as_user");
            }
            Ok(target_did.to_string())
        }
        None => Ok(own_did.to_string()),
    }
}
```

- [ ] **Step 4: Wire into require_auth middleware**

In `require_auth`, after determining `is_admin`, extract `as_user` from query params and call `resolve_effective_did`:

```rust
    // Extract as_user query param
    let as_user = request.uri().query()
        .and_then(|q| {
            q.split('&')
                .find_map(|pair| {
                    let (key, value) = pair.split_once('=')?;
                    if key == "as_user" { Some(value) } else { None }
                })
        });

    let effective_did = match resolve_effective_did(&did, is_admin, as_user) {
        Ok(d) => d,
        Err(_) => {
            return (StatusCode::FORBIDDEN, Json(json!({"error": "Only admins can impersonate"}))).into_response();
        }
    };

    // Validate target user exists (if impersonating)
    if as_user.is_some() {
        if state.db.get_user_handle(&effective_did).await?.is_none() {
            return (StatusCode::NOT_FOUND, Json(json!({"error": "Target user not found"}))).into_response();
        }
    }

    request.extensions_mut().insert(AuthUser {
        did: did.clone(),
        effective_did,
        is_admin,
    });
```

- [ ] **Step 5: Run all tests**

Run: `cargo test --features web`
Expected: All tests pass

- [ ] **Step 6: Commit**

```bash
git add src/web/auth.rs tests/unit_admin.rs
git commit -m 'feat: add as_user impersonation support to auth middleware'
```

---

### Task 7.5: Extract fingerprint-building logic into reusable function

**Files:**
- Modify: `src/web/scan_job.rs` (extract auto-fingerprint section ~lines 196-247)
- Modify: `src/pipeline/mod.rs` (add `pub mod fingerprint;` if creating new file) OR keep in `scan_job.rs`

The `pre_seed_user` handler needs to build a fingerprint for a user without running a full scan. Currently, the auto-fingerprint logic lives inline in `run_scan()` in `scan_job.rs`. Extract it into a standalone async function.

- [ ] **Step 1: Identify the fingerprint-building code in scan_job.rs**

Read `src/web/scan_job.rs` and locate the auto-fingerprint section (around lines 196-247). This code: fetches user's posts, runs TF-IDF, computes embeddings, and stores them.

- [ ] **Step 2: Extract into a standalone function**

Create a public function in `src/web/scan_job.rs` (or a new `src/pipeline/fingerprint.rs` module):

```rust
/// Build a topic fingerprint and embeddings for a user.
/// Fetches their recent posts, runs TF-IDF, and computes MiniLM embeddings.
pub async fn build_user_fingerprint(
    config: &Config,
    db: &dyn Database,
    user_did: &str,
    handle: &str,
) -> Result<()> {
    // ... extracted from run_scan auto-fingerprint section
}
```

Update `run_scan()` to call this extracted function instead of the inline code.

- [ ] **Step 3: Run tests to verify no regressions**

Run: `cargo test --features web`
Expected: All tests pass

- [ ] **Step 4: Commit**

```bash
git add src/web/scan_job.rs
git commit -m 'refactor: extract build_user_fingerprint into reusable function'
```

---

### Task 8: Admin API handlers

**Files:**
- Create: `src/web/handlers/admin.rs`
- Modify: `src/web/handlers/mod.rs`
- Modify: `src/web/mod.rs` (routes)

- [ ] **Step 1: Write failing tests for admin endpoints**

In `tests/unit_admin.rs`, add:

```rust
#[cfg(feature = "web")]
mod endpoint_tests {
    use charcoal::db::sqlite::SqliteDatabase;
    use charcoal::db::Database;
    use charcoal::web::handlers::admin::{list_users_handler, AdminUserResponse};
    use rusqlite::Connection;

    #[tokio::test]
    async fn test_admin_user_response_serialization() {
        let resp = AdminUserResponse {
            did: "did:plc:abc".to_string(),
            handle: "alice.bsky.social".to_string(),
            has_fingerprint: true,
            fingerprint_building: false,
            last_scan_at: None,
            scored_accounts: 42,
            last_login_at: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("alice.bsky.social"));
        assert!(json.contains("42"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features web --test unit_admin endpoint_tests`
Expected: FAIL — module doesn't exist

- [ ] **Step 3: Create admin handlers module**

Create `src/web/handlers/admin.rs`:

```rust
//! Admin dashboard API handlers.
//!
//! All handlers in this module require admin authorization.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::web::AuthUser;
use crate::web::AppState;

#[derive(Debug, Serialize)]
pub struct AdminUserResponse {
    pub did: String,
    pub handle: String,
    pub has_fingerprint: bool,
    pub fingerprint_building: bool,
    pub last_scan_at: Option<String>,
    pub scored_accounts: i64,
    pub last_login_at: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct IdentityResponse {
    pub did: String,
    pub handle: String,
    pub is_admin: bool,
}

#[derive(Debug, Deserialize)]
pub struct PreSeedRequest {
    pub handle: String,
}

/// GET /api/me — returns the authenticated user's identity.
pub async fn get_identity(
    Extension(auth): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let handle = state.db.get_user_handle(&auth.did).await
        .ok()
        .flatten()
        .unwrap_or_default();

    Json(IdentityResponse {
        did: auth.did,
        handle,
        is_admin: auth.is_admin,
    })
}

/// GET /api/admin/users — list all registered users with status.
pub async fn list_users(
    Extension(auth): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, StatusCode> {
    if !auth.is_admin {
        return Err(StatusCode::FORBIDDEN);
    }

    let users = state.db.list_users().await.map_err(|e| {
        tracing::error!("Failed to list users: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let mgr = state.scan_manager.read().await;
    let mut response = Vec::new();

    for user in users {
        let has_fp = state.db.has_fingerprint(&user.did).await.unwrap_or(false);
        let count = state.db.get_scored_account_count(&user.did).await.unwrap_or(0);
        let fp_building = mgr.is_fingerprint_building(&user.did);
        let last_scan = mgr.get_status(&user.did).and_then(|s| s.started_at.clone());

        response.push(AdminUserResponse {
            did: user.did,
            handle: user.handle,
            has_fingerprint: has_fp,
            fingerprint_building: fp_building,
            last_scan_at: last_scan,
            scored_accounts: count,
            last_login_at: user.last_login_at,
        });
    }

    Ok(Json(serde_json::json!({ "users": response })))
}

/// POST /api/admin/users — pre-seed a user by Bluesky handle.
pub async fn pre_seed_user(
    Extension(auth): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<PreSeedRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if !auth.is_admin {
        return Err((StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "Admin required"}))));
    }

    let handle = body.handle.trim().to_string();
    if handle.is_empty() {
        return Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Handle is required"}))));
    }

    // Resolve handle to DID
    let client = crate::bluesky::PublicAtpClient::new();
    let did = match client.resolve_handle(&handle).await {
        Ok(did) => did,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") || msg.contains("404") {
                return Err((StatusCode::NOT_FOUND, Json(serde_json::json!({"error": format!("Handle not found: {handle}")}))));
            }
            return Err((StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error": format!("Failed to resolve handle: {msg}")}))));
        }
    };

    // Check if user already exists
    if state.db.get_user_handle(&did).await.ok().flatten().is_some() {
        return Err((StatusCode::CONFLICT, Json(serde_json::json!({"error": "User already exists"}))));
    }

    // Insert user
    state.db.upsert_user(&did, &handle).await.map_err(|e| {
        tracing::error!("Failed to upsert user: {e}");
        (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Database error"})))
    })?;

    tracing::info!(admin_did = %auth.did, target_did = %did, target_handle = %handle, "Admin pre-seeded user");

    // Spawn background fingerprint build
    let db = state.db.clone();
    let config = state.config.clone();
    let scan_mgr = state.scan_manager.clone();
    let fp_did = did.clone();
    let fp_handle = handle.clone();

    {
        let mut mgr = scan_mgr.write().await;
        mgr.start_fingerprint_build(&fp_did);
    }

    tokio::spawn(async move {
        let result = crate::web::scan_job::build_user_fingerprint(
            &config, &*db, &fp_did, &fp_handle,
        ).await;

        let mut mgr = scan_mgr.write().await;
        mgr.finish_fingerprint_build(&fp_did);

        if let Err(e) = result {
            tracing::error!(target_did = %fp_did, "Fingerprint build failed: {e}");
        } else {
            tracing::info!(target_did = %fp_did, "Fingerprint build complete");
        }
    });

    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({"did": did, "handle": handle}))))
}

/// POST /api/admin/users/{did}/scan — trigger a scan for a specific user.
pub async fn trigger_admin_scan(
    Extension(auth): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Path(target_did): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if !auth.is_admin {
        return Err((StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "Admin required"}))));
    }

    // Verify user exists
    let handle = state.db.get_user_handle(&target_did).await
        .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Database error"}))))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "User not found"}))))?;

    // Atomic check-and-start
    {
        let mut mgr = state.scan_manager.write().await;
        mgr.try_start_scan(&target_did).map_err(|msg| {
            (StatusCode::CONFLICT, Json(serde_json::json!({"error": msg})))
        })?;
    }

    tracing::info!(admin_did = %auth.did, target_did = %target_did, "Admin triggered scan");

    // Launch scan
    crate::web::scan_job::launch_scan(
        state.config.clone(),
        state.db.clone(),
        state.scan_manager.clone(),
        target_did.clone(),
        handle,
    );

    Ok((StatusCode::ACCEPTED, Json(serde_json::json!({"message": "Scan started", "user_did": target_did}))))
}

/// DELETE /api/admin/users/{did} — remove a pre-seeded user and all their data.
pub async fn delete_user(
    Extension(auth): Extension<AuthUser>,
    State(state): State<Arc<AppState>>,
    Path(target_did): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    if !auth.is_admin {
        return Err((StatusCode::FORBIDDEN, Json(serde_json::json!({"error": "Admin required"}))));
    }

    if auth.did == target_did {
        return Err((StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "Cannot delete yourself"}))));
    }

    // Check user exists
    if state.db.get_user_handle(&target_did).await.ok().flatten().is_none() {
        return Err((StatusCode::NOT_FOUND, Json(serde_json::json!({"error": "User not found"}))));
    }

    // Check no running scan
    {
        let mgr = state.scan_manager.read().await;
        if mgr.is_scan_running_for(&target_did) {
            return Err((StatusCode::CONFLICT, Json(serde_json::json!({"error": "Cannot delete user with running scan"}))));
        }
    }

    tracing::info!(admin_did = %auth.did, target_did = %target_did, "Admin deleted user");

    state.db.delete_user_data(&target_did).await.map_err(|e| {
        tracing::error!("Failed to delete user data: {e}");
        (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Database error"})))
    })?;

    Ok(Json(serde_json::json!({"deleted": target_did})))
}
```

- [ ] **Step 4: Register the module and routes**

In `src/web/handlers/mod.rs`, add:
```rust
pub mod admin;
```

In `src/web/mod.rs` `build_router()`, add admin routes in the protected router:
```rust
    .route("/api/me", get(handlers::admin::get_identity))
    .route("/api/admin/users", get(handlers::admin::list_users))
    .route("/api/admin/users", post(handlers::admin::pre_seed_user))
    .route("/api/admin/users/{did}/scan", post(handlers::admin::trigger_admin_scan))
    .route("/api/admin/users/{did}", delete(handlers::admin::delete_user))
```

Also add `Method::DELETE` to the CORS `allow_methods` list (~line 161-164) so the browser DELETE request passes preflight.

- [ ] **Step 5: Run all tests**

Run: `cargo test --features web`
Expected: All tests pass

- [ ] **Step 6: Commit**

```bash
git add src/web/handlers/admin.rs src/web/handlers/mod.rs src/web/mod.rs tests/unit_admin.rs
git commit -m 'feat: add admin API endpoints — list users, pre-seed, scan, delete'
```

---

## Chunk 3: Frontend — Admin Page, Impersonation, and Integration

### Task 9: Add admin TypeScript types and API functions

**Files:**
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/lib/api.ts`

- [ ] **Step 1: Add types**

In `web/src/lib/types.ts`, add:

```typescript
export interface AdminUser {
  did: string;
  handle: string;
  has_fingerprint: boolean;
  fingerprint_building: boolean;
  last_scan_at: string | null;
  scored_accounts: number;
  last_login_at: string | null;
}

export interface AdminUsersResponse {
  users: AdminUser[];
}

export interface Identity {
  did: string;
  handle: string;
  is_admin: boolean;
}

export interface PreSeedResponse {
  did: string;
  handle: string;
}
```

- [ ] **Step 2: Add API functions**

In `web/src/lib/api.ts`, add:

```typescript
export async function getIdentity(): Promise<Identity> {
  return apiFetch<Identity>('/api/me');
}

export async function getAdminUsers(): Promise<AdminUsersResponse> {
  return apiFetch<AdminUsersResponse>('/api/admin/users');
}

export async function preSeedUser(handle: string): Promise<PreSeedResponse> {
  return apiFetch<PreSeedResponse>('/api/admin/users', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ handle }),
  });
}

export async function triggerAdminScan(did: string): Promise<void> {
  await apiFetch(`/api/admin/users/${encodeURIComponent(did)}/scan`, {
    method: 'POST',
  });
}

export async function deleteAdminUser(did: string): Promise<void> {
  await apiFetch(`/api/admin/users/${encodeURIComponent(did)}`, {
    method: 'DELETE',
  });
}
```

- [ ] **Step 3: Add `as_user` support to apiFetch**

Modify the `apiFetch` helper to automatically append `?as_user=` when impersonating. Read `as_user` from the page URL's search params:

```typescript
function getAsUser(): string | null {
  if (typeof window === 'undefined') return null;
  return new URLSearchParams(window.location.search).get('as_user');
}

// In apiFetch, before the fetch call:
const asUser = getAsUser();
if (asUser) {
  const separator = path.includes('?') ? '&' : '?';
  path = `${path}${separator}as_user=${encodeURIComponent(asUser)}`;
}
```

- [ ] **Step 4: Commit**

```bash
git add web/src/lib/types.ts web/src/lib/api.ts
git commit -m 'feat: add admin TypeScript types and API functions'
```

---

### Task 10: Create admin page

**Files:**
- Create: `web/src/routes/(protected)/admin/+page.svelte`

- [ ] **Step 1: Create the admin page**

Use the Svelte skill to validate. Create `web/src/routes/(protected)/admin/+page.svelte` with:

- Pre-seed form section (handle input + Add User button)
- Protected Users table (polls `/api/admin/users` every 3 seconds while any fingerprint is building)
- Scan/View action buttons per user
- Error/success feedback inline
- Follow existing design patterns from `dashboard/+page.svelte`

Key behaviors:
- "Add User" calls `preSeedUser(handle)`, shows success/error inline
- "Scan" calls `triggerAdminScan(did)`, disables button while running
- "View" navigates to `/dashboard?as_user={did}`
- Table shows: Handle, Fingerprint (Ready/Building...), Last Scan, Accounts, Actions
- Scan button disabled if `!has_fingerprint || fingerprint_building || scanRunning`
- Poll interval: 3 seconds while `fingerprint_building` is true for any user, otherwise stop

- [ ] **Step 2: Build SPA**

Run: `npm --prefix web run build`
Expected: Build succeeds

- [ ] **Step 3: Commit**

```bash
git add web/src/routes/\(protected\)/admin/+page.svelte web/build/
git commit -m 'feat: add admin page with pre-seed form and user management'
```

---

### Task 11: Add Admin nav link and impersonation banner to layout

**Files:**
- Modify: `web/src/routes/(protected)/+layout.svelte`

- [ ] **Step 1: Add identity check on mount**

In the layout's `onMount`, call `getIdentity()` to determine if the user is admin. Store in a reactive variable:

```typescript
let identity: Identity | null = $state(null);

onMount(async () => {
  try {
    identity = await getIdentity();
  } catch (e) {
    if (e instanceof AuthError) goto('/login');
  }
});
```

- [ ] **Step 2: Add Admin nav link (conditional)**

In the nav section, after the Review link (~line 66), add:

```svelte
{#if identity?.is_admin}
  <a href="/admin" class:active={$page.url.pathname === '/admin'}>Admin</a>
{/if}
```

- [ ] **Step 3: Add impersonation banner**

Above the main content slot, add:

```svelte
{#if $page.url.searchParams.get('as_user')}
  <div class="impersonation-banner">
    Viewing as <strong>{impersonatedHandle}</strong> (read-only)
    <button onclick={() => goto('/admin')}>Exit</button>
  </div>
{/if}
```

Style with amber background matching the mockup. Resolve the impersonated handle from the admin users list or a separate API call.

- [ ] **Step 4: Build SPA**

Run: `npm --prefix web run build`
Expected: Build succeeds

- [ ] **Step 5: Commit**

```bash
git add web/src/routes/\(protected\)/+layout.svelte web/build/
git commit -m 'feat: add Admin nav link and impersonation banner to layout'
```

---

### Task 12: Integration testing and final verification

**Files:**
- Modify: `tests/unit_admin.rs` (add integration-style tests)

- [ ] **Step 1: Run full test suite**

Run: `cargo test --features web`
Expected: All tests pass (existing + new admin tests)

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --features web -- -D warnings`
Expected: Clean

- [ ] **Step 3: Build SPA and verify embed**

Run: `npm --prefix web run build && cargo build --features web`
Expected: Both succeed

- [ ] **Step 4: Final commit and push**

```bash
git push origin feat/admin-dashboard
```

- [ ] **Step 5: Create PR to staging**

Create PR: `feat/admin-dashboard` → `staging`
Title: "Phase 1.5: Admin dashboard — pre-seed, scan, impersonate"
