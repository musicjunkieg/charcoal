# Allowlist Admin UI + Minimal Scan Cooldown — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** DB-backed access allowlist with auto-waitlist on denied OAuth, admin approve/deny UI, and a per-user scan cooldown — replacing "edit a Railway env var and redeploy" as the onboarding mechanism.

**Architecture:** One new `access_requests` table (schema v14) consulted by the auth gate as a third OR-clause after the `CHARCOAL_ALLOWED_DID` env bootstrap and admin DIDs. Denied OAuth logins upsert a `pending` row and redirect to `/waitlist` instead of returning raw JSON. Five new admin endpoints manage the list. Cooldown is a check in `trigger_scan` against `scan_queue.finished_at` — no new columns.

**Tech Stack:** Rust/axum (`--features web`), rusqlite + sqlx-postgres dual backend, SvelteKit 5 SPA in `web/`, vitest.

**Spec:** `docs/superpowers/specs/2026-08-24-allowlist-admin-ui-design.md` — read it first; it is the contract.

## Global Constraints

- Chainlink issue: **#309** (already active). Branch: `feat/allowlist-admin-ui` (already checked out).
- Log deciduous `action` before and `outcome` after each task (`--commit HEAD`), link immediately. Check `deciduous nodes | tail` before adding — subagents may have logged already.
- Run tests with `CHARCOAL_MODEL_DIR=./models cargo test --features web ...` — test binaries do not load `.env`.
- **NEVER** use heredocs in shell commands. Single-quoted multi-line strings for commit messages.
- **NEVER** `git add -A`/`.` — stage files explicitly by name.
- Do NOT background `cargo test`. Run it in the foreground and wait.
- Postgres migrations MUST self-record their version (`INSERT INTO schema_version ... ON CONFLICT DO NOTHING`).
- Timestamps are RFC3339 TEXT on both backends, computed in Rust via `chrono::Utc::now().to_rfc3339()`.
- Status strings: exactly `pending`, `allowed`, `denied`. Error code string: exactly `access_revoked`.
- Cooldown env var: `CHARCOAL_SCAN_COOLDOWN_HOURS`, default `24`, `0` disables.
- Gate failures fail CLOSED (500 via `api_error`), never open.
- The `?` operator and `anyhow::Result` for errors; no `.unwrap()` outside tests; `cargo clippy` clean.
- Frontend build: `npm --prefix web run build` (never `cd web && ...`).

---

### Task 1: Schema v14 — `access_requests` on both backends

**Files:**
- Modify: `src/db/schema.rs` (append v14 after the v13 block ending at ~line 452; update version-pin tests at ~511, ~583, ~696)
- Create: `migrations/postgres/0014_access_requests.sql`
- Modify: `src/db/postgres.rs` (register 0014 in the `migrations` array, after the `(13, include_str!(...0013...))` entry at ~line 116–170)

**Interfaces:**
- Produces: table `access_requests (did TEXT PK, handle TEXT NOT NULL, status TEXT NOT NULL CHECK IN pending/allowed/denied, requested_at TEXT NOT NULL, decided_at TEXT, decided_by TEXT)` on both backends, schema version 14.

- [ ] **Step 1: Write the failing SQLite test**

Add to the `tests` module in `src/db/schema.rs` (and bump the two existing pins — the table-count test at ~line 503 from `13i64` to `14i64` with `access_requests` appended to its comment list, and the version-list assertions at ~lines 583/696 from `vec![1, ..., 13]` to include `14`):

```rust
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib db::schema -- --show-output`
Expected: FAIL — `no such table: access_requests` (and the count test fails at 13 ≠ 14 once edited).

- [ ] **Step 3: Add the SQLite migration**

In `src/db/schema.rs`, immediately after the v13 `run_migration` block and before the final `Ok(())`:

```rust
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
```

- [ ] **Step 4: Add the Postgres migration**

Create `migrations/postgres/0014_access_requests.sql`:

```sql
-- #309: access_requests — DB-backed allowlist for gated onboarding.
-- One row per DID, ever. 'denied' covers both "denied from waitlist" and
-- "revoked after having access". Timestamps are RFC3339 TEXT computed in
-- Rust, matching the trait convention, so both backends store identical
-- values and parity tests compare strings directly.
-- NOT cascaded by delete_user_data: admin grant/deny record, not user content.

CREATE TABLE IF NOT EXISTS access_requests (
    did TEXT PRIMARY KEY,
    handle TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending','allowed','denied')),
    requested_at TEXT NOT NULL,
    decided_at TEXT,
    decided_by TEXT
);

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (14) ON CONFLICT DO NOTHING;
```

Register it in `src/db/postgres.rs` in the `migrations` array, after the `13` entry:

```rust
                (
                    14,
                    include_str!("../../migrations/postgres/0014_access_requests.sql"),
                ),
```

- [ ] **Step 5: Run tests to verify pass**

Run: `cargo test --lib db::schema -- --show-output`
Expected: PASS, including the updated count (14 tables) and version-list tests.

- [ ] **Step 6: Postgres migration smoke test** (skip if no local Postgres)

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --features postgres --lib -- --show-output`
Expected: PASS (migration applies once; re-run stays at 14).

- [ ] **Step 7: Commit**

```bash
git add src/db/schema.rs migrations/postgres/0014_access_requests.sql src/db/postgres.rs
git commit -m 'feat(309): schema v14 access_requests table, both backends'
```

---

### Task 2: `AccessRequestRow` + trait methods + SQLite implementation

**Files:**
- Modify: `src/db/traits.rs` (row struct near `ScanQueueRow` ~line 128; methods in a new `--- Access requests (#309) ---` section of the trait)
- Modify: `src/db/queries.rs` (query functions, following the `enqueue_scan(&conn, ...)` idiom)
- Modify: `src/db/sqlite.rs` (thin `self.conn.lock().await` delegations, like the scan-queue block at ~line 443)
- Test: `tests/unit_access.rs` (new)

**Interfaces:**
- Produces (used by Tasks 3–8):

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessRequestRow {
    pub did: String,
    pub handle: String,
    /// "pending" | "allowed" | "denied"
    pub status: String,
    /// RFC3339, like every other timestamp on this trait.
    pub requested_at: String,
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
}

// On trait Database:
async fn get_access_request(&self, did: &str) -> Result<Option<AccessRequestRow>>;
/// Creates a 'pending' row; if a row exists, refreshes handle ONLY (status untouched).
async fn upsert_access_request_pending(&self, did: &str, handle: &str) -> Result<()>;
/// Sets status + decided_at/decided_by on an existing row. Returns false if no row.
async fn set_access_status(&self, did: &str, status: &str, decided_by: &str) -> Result<bool>;
/// Admin grant-by-handle: upsert straight to 'allowed' (works with or without a prior row).
async fn grant_access(&self, did: &str, handle: &str, decided_by: &str) -> Result<()>;
/// All rows, oldest requested_at first.
async fn list_access_requests(&self) -> Result<Vec<AccessRequestRow>>;
```

- [ ] **Step 1: Write the failing tests**

Create `tests/unit_access.rs`:

```rust
//! access_requests state machine at the Database-trait level (#309).
#![cfg(feature = "web")]

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use rusqlite::Connection;

const DID: &str = "did:plc:accesstest00000000000000";

async fn setup_db() -> SqliteDatabase {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    SqliteDatabase::new(conn)
}

#[tokio::test]
async fn pending_upsert_creates_then_only_refreshes_handle() {
    let db = setup_db().await;
    db.upsert_access_request_pending(DID, "old.bsky.social").await.unwrap();
    let row = db.get_access_request(DID).await.unwrap().unwrap();
    assert_eq!(row.status, "pending");
    assert_eq!(row.handle, "old.bsky.social");
    assert!(row.decided_at.is_none());

    // Deny, then sign in again with a new handle: status must NOT reset.
    assert!(db.set_access_status(DID, "denied", "did:plc:admin").await.unwrap());
    db.upsert_access_request_pending(DID, "new.bsky.social").await.unwrap();
    let row = db.get_access_request(DID).await.unwrap().unwrap();
    assert_eq!(row.status, "denied", "denied is sticky through re-login");
    assert_eq!(row.handle, "new.bsky.social", "handle refreshes anyway");
}

#[tokio::test]
async fn set_status_records_decision_and_reports_missing_rows() {
    let db = setup_db().await;
    assert!(!db.set_access_status(DID, "allowed", "did:plc:admin").await.unwrap());
    db.upsert_access_request_pending(DID, "w.bsky.social").await.unwrap();
    assert!(db.set_access_status(DID, "allowed", "did:plc:admin").await.unwrap());
    let row = db.get_access_request(DID).await.unwrap().unwrap();
    assert_eq!(row.status, "allowed");
    assert_eq!(row.decided_by.as_deref(), Some("did:plc:admin"));
    assert!(row.decided_at.is_some());
    // Idempotent repeat is a success, not an error.
    assert!(db.set_access_status(DID, "allowed", "did:plc:admin").await.unwrap());
}

#[tokio::test]
async fn grant_access_upserts_allowed_with_and_without_prior_row() {
    let db = setup_db().await;
    db.grant_access(DID, "granted.bsky.social", "did:plc:admin").await.unwrap();
    let row = db.get_access_request(DID).await.unwrap().unwrap();
    assert_eq!(row.status, "allowed");
    // Re-grant over a denied row flips it back to allowed.
    db.set_access_status(DID, "denied", "did:plc:admin").await.unwrap();
    db.grant_access(DID, "granted.bsky.social", "did:plc:admin").await.unwrap();
    assert_eq!(db.get_access_request(DID).await.unwrap().unwrap().status, "allowed");
}

#[tokio::test]
async fn list_returns_oldest_first() {
    let db = setup_db().await;
    db.upsert_access_request_pending("did:plc:first000000000000000000", "a.bsky.social").await.unwrap();
    db.upsert_access_request_pending("did:plc:second00000000000000000", "b.bsky.social").await.unwrap();
    let rows = db.list_access_requests().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].requested_at <= rows[1].requested_at);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test unit_access -- --show-output`
Expected: COMPILE FAIL — methods not on trait.

- [ ] **Step 3: Implement**

`src/db/traits.rs`: add the struct and the five trait methods exactly as in Interfaces (doc comments included).

`src/db/queries.rs`: add, following the file's existing `(&Connection, ...)` style:

```rust
// --- Access requests (#309) ---

pub fn get_access_request(
    conn: &Connection,
    did: &str,
) -> Result<Option<crate::db::traits::AccessRequestRow>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, status, requested_at, decided_at, decided_by
         FROM access_requests WHERE did = ?1",
    )?;
    let row = stmt
        .query_row([did], |r| {
            Ok(crate::db::traits::AccessRequestRow {
                did: r.get(0)?,
                handle: r.get(1)?,
                status: r.get(2)?,
                requested_at: r.get(3)?,
                decided_at: r.get(4)?,
                decided_by: r.get(5)?,
            })
        })
        .optional()?;
    Ok(row)
}

pub fn upsert_access_request_pending(conn: &Connection, did: &str, handle: &str) -> Result<()> {
    // ON CONFLICT refreshes the handle ONLY: a denied row stays denied and an
    // allowed row stays allowed — sign-in attempts never move the state machine.
    conn.execute(
        "INSERT INTO access_requests (did, handle, status, requested_at)
         VALUES (?1, ?2, 'pending', ?3)
         ON CONFLICT (did) DO UPDATE SET handle = excluded.handle",
        rusqlite::params![did, handle, chrono::Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

pub fn set_access_status(
    conn: &Connection,
    did: &str,
    status: &str,
    decided_by: &str,
) -> Result<bool> {
    let n = conn.execute(
        "UPDATE access_requests SET status = ?2, decided_at = ?3, decided_by = ?4
         WHERE did = ?1",
        rusqlite::params![did, status, chrono::Utc::now().to_rfc3339(), decided_by],
    )?;
    Ok(n > 0)
}

pub fn grant_access(conn: &Connection, did: &str, handle: &str, decided_by: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO access_requests (did, handle, status, requested_at, decided_at, decided_by)
         VALUES (?1, ?2, 'allowed', ?3, ?3, ?4)
         ON CONFLICT (did) DO UPDATE SET status = 'allowed', handle = excluded.handle,
             decided_at = excluded.decided_at, decided_by = excluded.decided_by",
        rusqlite::params![did, handle, now, decided_by],
    )?;
    Ok(())
}

pub fn list_access_requests(
    conn: &Connection,
) -> Result<Vec<crate::db::traits::AccessRequestRow>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, status, requested_at, decided_at, decided_by
         FROM access_requests ORDER BY requested_at ASC, did ASC",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(crate::db::traits::AccessRequestRow {
                did: r.get(0)?,
                handle: r.get(1)?,
                status: r.get(2)?,
                requested_at: r.get(3)?,
                decided_at: r.get(4)?,
                decided_by: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
```

(Use `rusqlite::OptionalExtension` for `.optional()` — check the top of `queries.rs`; it is almost certainly already imported. Match the file's exact `Result` alias.)

`src/db/sqlite.rs`: five thin delegations in a new `// --- Access requests (#309) ---` section:

```rust
    async fn get_access_request(&self, did: &str) -> Result<Option<AccessRequestRow>> {
        let conn = self.conn.lock().await;
        super::queries::get_access_request(&conn, did)
    }

    async fn upsert_access_request_pending(&self, did: &str, handle: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_access_request_pending(&conn, did, handle)
    }

    async fn set_access_status(&self, did: &str, status: &str, decided_by: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::set_access_status(&conn, did, status, decided_by)
    }

    async fn grant_access(&self, did: &str, handle: &str, decided_by: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::grant_access(&conn, did, handle, decided_by)
    }

    async fn list_access_requests(&self) -> Result<Vec<AccessRequestRow>> {
        let conn = self.conn.lock().await;
        super::queries::list_access_requests(&conn)
    }
```

(Import `AccessRequestRow` alongside the file's existing `ScanQueueRow` import.)

Note: `src/db/postgres.rs` will not compile until Task 3 adds its impls. Add them in the same commit if the trait change breaks the `postgres` feature build — Tasks 2 and 3 may land as one commit if needed; keep the test files separate.

- [ ] **Step 4: Run tests to verify pass**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test unit_access -- --show-output`
Expected: PASS (4 tests).

- [ ] **Step 5: Clippy + commit**

Run: `cargo clippy --features web --all-targets`
Expected: clean.

```bash
git add src/db/traits.rs src/db/queries.rs src/db/sqlite.rs tests/unit_access.rs
git commit -m 'feat(309): AccessRequestRow + access_requests trait methods, SQLite impl'
```

---

### Task 3: Postgres implementation + parity tests

**Files:**
- Modify: `src/db/postgres.rs` (five methods in a `// --- Access requests (#309) ---` section)
- Modify: `tests/db_postgres.rs` (parity tests following the file's existing per-feature module pattern)

**Interfaces:**
- Consumes: the trait methods from Task 2, byte-identical semantics.

- [ ] **Step 1: Write the failing parity test**

Add to `tests/db_postgres.rs`, following its existing setup helper (it connects via `DATABASE_URL` and skips/creates schema per its established pattern — copy the harness of the nearest recent test group, e.g. the scan_queue one):

```rust
#[tokio::test]
async fn access_requests_state_machine_parity() {
    let db = setup_pg().await; // use this file's existing helper name — check before writing
    let did = "did:plc:pgaccesstest000000000000";
    db.upsert_access_request_pending(did, "old.bsky.social").await.unwrap();
    let row = db.get_access_request(did).await.unwrap().unwrap();
    assert_eq!((row.status.as_str(), row.handle.as_str()), ("pending", "old.bsky.social"));

    assert!(db.set_access_status(did, "denied", "did:plc:admin").await.unwrap());
    db.upsert_access_request_pending(did, "new.bsky.social").await.unwrap();
    let row = db.get_access_request(did).await.unwrap().unwrap();
    assert_eq!(row.status, "denied");
    assert_eq!(row.handle, "new.bsky.social");

    db.grant_access(did, "new.bsky.social", "did:plc:admin").await.unwrap();
    assert_eq!(db.get_access_request(did).await.unwrap().unwrap().status, "allowed");

    assert!(!db.set_access_status("did:plc:norow0000000000000000000", "allowed", "x").await.unwrap());
    assert!(!db.list_access_requests().await.unwrap().is_empty());
}
```

(If the file's tests clean up after themselves, mirror that — delete the row at the end with a raw query through the same mechanism its other tests use.)

- [ ] **Step 2: Run to verify failure**

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres access_requests -- --show-output`
Expected: COMPILE FAIL (methods missing on `PgDatabase`) or FAIL.

- [ ] **Step 3: Implement in `postgres.rs`**

Same SQL as SQLite with `$n` binds, RFC3339 strings computed in Rust (columns are TEXT — no `NOW()`):

```rust
    // --- Access requests (#309) ---

    async fn get_access_request(&self, did: &str) -> Result<Option<AccessRequestRow>> {
        use sqlx_core::row::Row as _;
        let row = sqlx_core::query::query(
            "SELECT did, handle, status, requested_at, decided_at, decided_by
             FROM access_requests WHERE did = $1",
        )
        .bind(did)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| AccessRequestRow {
            did: r.get(0),
            handle: r.get(1),
            status: r.get(2),
            requested_at: r.get(3),
            decided_at: r.get(4),
            decided_by: r.get(5),
        }))
    }

    async fn upsert_access_request_pending(&self, did: &str, handle: &str) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO access_requests (did, handle, status, requested_at)
             VALUES ($1, $2, 'pending', $3)
             ON CONFLICT (did) DO UPDATE SET handle = EXCLUDED.handle",
        )
        .bind(did)
        .bind(handle)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn set_access_status(&self, did: &str, status: &str, decided_by: &str) -> Result<bool> {
        let res = sqlx_core::query::query(
            "UPDATE access_requests SET status = $2, decided_at = $3, decided_by = $4
             WHERE did = $1",
        )
        .bind(did)
        .bind(status)
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(decided_by)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    async fn grant_access(&self, did: &str, handle: &str, decided_by: &str) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        sqlx_core::query::query(
            "INSERT INTO access_requests (did, handle, status, requested_at, decided_at, decided_by)
             VALUES ($1, $2, 'allowed', $3, $3, $4)
             ON CONFLICT (did) DO UPDATE SET status = 'allowed', handle = EXCLUDED.handle,
                 decided_at = EXCLUDED.decided_at, decided_by = EXCLUDED.decided_by",
        )
        .bind(did)
        .bind(handle)
        .bind(now)
        .bind(decided_by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_access_requests(&self) -> Result<Vec<AccessRequestRow>> {
        use sqlx_core::row::Row as _;
        let rows = sqlx_core::query::query(
            "SELECT did, handle, status, requested_at, decided_at, decided_by
             FROM access_requests ORDER BY requested_at ASC, did ASC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| AccessRequestRow {
                did: r.get(0),
                handle: r.get(1),
                status: r.get(2),
                requested_at: r.get(3),
                decided_at: r.get(4),
                decided_by: r.get(5),
            })
            .collect())
    }
```

(Match the file's actual query/row-access idiom — it may use `.try_get` or typed helpers; copy whatever the scan_queue methods at ~line 1612 do.)

- [ ] **Step 4: Run tests to verify pass**

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres access_requests -- --show-output`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/db/postgres.rs tests/db_postgres.rs
git commit -m 'feat(309): access_requests Postgres impl + parity tests'
```

---

### Task 4: The gate — `check_access` + `require_auth` rewiring

**Files:**
- Modify: `src/web/auth.rs` (new async `check_access`; rewire `require_auth` at ~lines 197–267; update the stale module doc comment at the top which still describes only the env-var gate)
- Test: `tests/web_access_gate.rs` (new)

**Interfaces:**
- Consumes: `get_access_request` (Task 2).
- Produces (used by Tasks 5–8):

```rust
/// Three OR'd clauses, in order: env bootstrap (empty env = open access,
/// table never consulted), admin DIDs, DB row with status 'allowed'.
/// Errors mean "we could not check" — callers must fail CLOSED.
pub async fn check_access(
    did: &str,
    config: &crate::config::Config,
    db: &dyn crate::db::Database,
) -> anyhow::Result<bool>
```

Response contract on revocation (relied on by Task 10):
`403` with body `{"error": "Access is not currently active for this account", "code": "access_revoked"}`.

- [ ] **Step 1: Write the failing tests**

Create `tests/web_access_gate.rs`:

```rust
//! The three-clause access gate (#309): env bootstrap OR admin OR DB row.
#![cfg(feature = "web")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use charcoal::web::auth::{create_token, COOKIE_NAME};
use charcoal::web::test_helpers::{build_test_app_with_db, TEST_DID, TEST_SECRET};
use serde_json::Value;
use tower::ServiceExt;

const MODELS_REQUIRED: &str = "ONNX models required — run with CHARCOAL_MODEL_DIR=./models";
const OUTSIDER: &str = "did:plc:outsider0000000000000000";

fn session_cookie(did: &str) -> String {
    format!("{}={}", COOKIE_NAME, create_token(TEST_SECRET, did))
}

async fn get_me(app: &axum::Router, did: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/me")
                .header("cookie", session_cookie(did))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

// build_test_app_with_db pins CHARCOAL_ALLOWED_DID to TEST_DID, so the env
// gate is ACTIVE in these tests and OUTSIDER is not on it.

#[tokio::test]
async fn env_member_still_passes_with_empty_table() {
    let (app, _db) = build_test_app_with_db().expect(MODELS_REQUIRED);
    let (status, _) = get_me(&app, TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn outsider_with_valid_cookie_gets_access_revoked_403() {
    let (app, _db) = build_test_app_with_db().expect(MODELS_REQUIRED);
    let (status, body) = get_me(&app, OUTSIDER).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "access_revoked", "machine-readable code: {body}");
}

#[tokio::test]
async fn allowed_db_row_passes_the_env_gate() {
    let (app, db) = build_test_app_with_db().expect(MODELS_REQUIRED);
    db.grant_access(OUTSIDER, "outsider.bsky.social", "did:plc:admin")
        .await
        .expect("grant");
    let (status, _) = get_me(&app, OUTSIDER).await;
    assert_eq!(status, StatusCode::OK, "allowed row must pass");
}

#[tokio::test]
async fn denied_and_pending_rows_do_not_pass() {
    let (app, db) = build_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_access_request_pending(OUTSIDER, "outsider.bsky.social")
        .await
        .expect("pending");
    let (status, _) = get_me(&app, OUTSIDER).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "pending is not allowed");

    db.set_access_status(OUTSIDER, "denied", "did:plc:admin").await.expect("deny");
    let (status, _) = get_me(&app, OUTSIDER).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "denied is not allowed");
}
```

Also add a pure-logic test module inside `src/web/auth.rs`'s existing `tests` mod exercising `check_access` directly against an in-memory `SqliteDatabase` with a hand-built `Config` (env empty → `Ok(true)` without any row; env set + admin DID → `Ok(true)`; env set + no row → `Ok(false)`).

- [ ] **Step 2: Run to verify failure**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_access_gate -- --show-output`
Expected: FAIL — `allowed_db_row_passes_the_env_gate` gets 403 (gate ignores the table), `outsider...` sees no `code` field.

- [ ] **Step 3: Implement**

In `src/web/auth.rs`:

```rust
/// Full access decision for an authenticated DID (#309).
///
/// Three OR'd clauses, in order:
/// 1. `CHARCOAL_ALLOWED_DID` env bootstrap — `did_is_allowed` returns true for
///    an EMPTY env var (open access), so the table is only consulted when the
///    gate is actively configured. The env list can never be locked out by
///    DB state.
/// 2. `CHARCOAL_ADMIN_DIDS` — an admin cannot revoke their own access via
///    the table.
/// 3. An `access_requests` row with status 'allowed'.
///
/// `Err` means "could not check" — callers MUST fail closed.
pub async fn check_access(
    did: &str,
    config: &crate::config::Config,
    db: &dyn crate::db::Database,
) -> anyhow::Result<bool> {
    if did_is_allowed(did, &config.allowed_did) {
        return Ok(true);
    }
    if did_is_admin(did, &config.admin_dids) {
        return Ok(true);
    }
    Ok(matches!(
        db.get_access_request(did).await?,
        Some(row) if row.status == "allowed"
    ))
}
```

Rewire `require_auth` — replace the `Some(did) if !did_is_allowed(...)` guard arm with a check inside the success arm:

```rust
        Some(did) => {
            match check_access(&did, &state.config, &*state.db).await {
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), "access check failed — failing closed");
                    return super::api_error(
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        "Database error",
                    );
                }
                Ok(false) => {
                    // Machine-readable code so the SPA can route to /waitlist
                    // instead of rendering a broken dashboard (#309).
                    return (
                        axum::http::StatusCode::FORBIDDEN,
                        axum::Json(serde_json::json!({
                            "error": "Access is not currently active for this account",
                            "code": "access_revoked",
                        })),
                    )
                        .into_response();
                }
                Ok(true) => {}
            }
            let is_admin = did_is_admin(&did, &state.config.admin_dids);
            // ... rest of the existing arm unchanged ...
```

(Keep the existing `IntoResponse` import situation in mind — the file returns `Response`; use `.into_response()` as shown, adding `use axum::response::IntoResponse;` if not present.)

Update the module doc comment at the top of the file (line 14 currently says "check against CHARCOAL_ALLOWED_DID → allow/deny") to describe the three-clause gate.

- [ ] **Step 4: Run tests to verify pass**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_access_gate -- --show-output`
Expected: PASS (4 tests). Also run `--test unit_oauth --test web_oauth` and `--test web_scan_queue` to confirm no regressions (open-access helpers must still pass — `build_open_test_app_with_db` has an empty env var, so clause 1 short-circuits).

- [ ] **Step 5: Commit**

```bash
git add src/web/auth.rs tests/web_access_gate.rs
git commit -m 'feat(309): three-clause access gate with access_revoked 403 code'
```

---

### Task 5: OAuth callback — auto-waitlist + redirect

**Files:**
- Modify: `src/web/handlers/oauth.rs` (~lines 434–445, the gate block)
- Test: extend `tests/web_oauth.rs` — follow its existing harness for driving the callback; if the existing suite stubs the token exchange, extend that stub path. If the callback cannot be driven end-to-end without a live PDS, place the tests at the highest level the existing oauth tests reach and note which assertions moved where.

**Interfaces:**
- Consumes: `check_access` (Task 4), `upsert_access_request_pending` (Task 2).
- Produces: denied OAuth → `302 Location: /waitlist`, `pending` row upserted, no `Set-Cookie`, no `users` row.

- [ ] **Step 1: Write the failing tests**

In `tests/web_oauth.rs`, alongside its existing callback tests (reuse its harness/mocks exactly — the assertions are what matter):

```rust
// Denied DID: 302 to /waitlist, pending row recorded, no cookie, no user row.
// (Wire these into this file's existing callback-driving helper.)
assert_eq!(res.status(), StatusCode::FOUND);
assert_eq!(res.headers().get("location").unwrap(), "/waitlist");
assert!(res.headers().get("set-cookie").is_none());
let row = db.get_access_request(DENIED_DID).await.unwrap().unwrap();
assert_eq!(row.status, "pending");
assert!(db.get_user_handle(DENIED_DID).await.unwrap().is_none());
```

Plus: a second denied attempt refreshes the handle but keeps status; a `denied` row stays `denied` and still redirects.

- [ ] **Step 2: Run to verify failure**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_oauth -- --show-output`
Expected: FAIL — current code returns 403 JSON, no row.

- [ ] **Step 3: Implement**

Replace the gate block in the callback (oauth.rs ~434–445):

```rust
    // Gate: env bootstrap OR admin OR DB allowlist row (#309).
    match crate::web::auth::check_access(&authenticated_did, &state.config, &*state.db).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::info!(
                did = %authenticated_did,
                handle = %pending.handle,
                "Disallowed sign-in recorded as access request"
            );
            // Best-effort bookkeeping: the person's experience must not depend
            // on the upsert succeeding — the gate already denied them.
            if let Err(e) = state
                .db
                .upsert_access_request_pending(&authenticated_did, &pending.handle)
                .await
            {
                tracing::error!(error = %format!("{e:#}"), "failed to record access request");
            }
            // Top-level browser navigation: redirect to a real page, never JSON.
            return Redirect::to("/waitlist").into_response();
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "access check failed at login — failing closed");
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Could not complete sign-in — please try again",
            );
        }
    }
```

(`Redirect` is already imported in this file — it is used at line 480.)

- [ ] **Step 4: Run tests to verify pass**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_oauth -- --show-output`
Expected: PASS, including pre-existing oauth tests.

- [ ] **Step 5: Commit**

```bash
git add src/web/handlers/oauth.rs tests/web_oauth.rs
git commit -m 'feat(309): denied OAuth auto-waitlists and redirects to /waitlist'
```

---

### Task 6: Admin access endpoints — list / approve / deny

**Files:**
- Create: `src/web/handlers/access.rs`
- Modify: `src/web/handlers/mod.rs` (add `pub mod access;`)
- Modify: `src/web/mod.rs` (routes in `protected_api`, next to the existing admin routes at ~177–189)
- Test: `tests/web_access_admin.rs` (new)

**Interfaces:**
- Consumes: Task 2 trait methods; `AuthUser.is_admin`.
- Produces (relied on by Task 10's frontend):
  - `GET /api/admin/access` → `200 {"pending": [row...], "allowed": [row...], "denied": [row...]}` where row = `{did, handle, status, requested_at, decided_at, decided_by}`
  - `POST /api/admin/access/{did}/approve` → `200 {"did", "status": "allowed"}` | `404` no row
  - `POST /api/admin/access/{did}/deny` → `200 {"did", "status": "denied"}` | `404` no row
  - all: `403 {"error": "Admin required"}` for non-admins

- [ ] **Step 1: Write the failing tests**

Create `tests/web_access_admin.rs` (harness copied from `tests/web_scan_queue.rs`; `build_admin_test_app_with_db` makes `TEST_DID` an admin with an empty env gate):

```rust
//! Admin allowlist endpoints (#309): list, approve, deny.
#![cfg(feature = "web")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use charcoal::web::auth::{create_token, COOKIE_NAME};
use charcoal::web::test_helpers::{build_admin_test_app_with_db, TEST_DID, TEST_SECRET};
use serde_json::Value;
use tower::ServiceExt;

const MODELS_REQUIRED: &str = "ONNX models required — run with CHARCOAL_MODEL_DIR=./models";
const WAITER: &str = "did:plc:waiter000000000000000000";
const NON_ADMIN: &str = "did:plc:regular00000000000000000";

fn session_cookie(did: &str) -> String {
    format!("{}={}", COOKIE_NAME, create_token(TEST_SECRET, did))
}

async fn call(app: &axum::Router, method: &str, uri: &str, did: &str) -> (StatusCode, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(uri)
                .method(method)
                .header("cookie", session_cookie(did))
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.expect("body");
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn non_admin_gets_403_on_every_access_endpoint() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(NON_ADMIN, "r.bsky.social").await.expect("user");
    for (m, u) in [
        ("GET", "/api/admin/access"),
        ("POST", "/api/admin/access/did:plc:x/approve"),
        ("POST", "/api/admin/access/did:plc:x/deny"),
    ] {
        let (status, _) = call(&app, m, u, NON_ADMIN).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{m} {u}");
    }
}

#[tokio::test]
async fn list_groups_rows_by_status() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_access_request_pending(WAITER, "w.bsky.social").await.expect("row");
    db.grant_access("did:plc:granted00000000000000000", "g.bsky.social", TEST_DID)
        .await
        .expect("row");
    let (status, body) = call(&app, "GET", "/api/admin/access", TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending"][0]["did"], WAITER);
    assert_eq!(body["allowed"][0]["handle"], "g.bsky.social");
    assert!(body["denied"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn approve_deny_flip_status_and_404_without_a_row() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_access_request_pending(WAITER, "w.bsky.social").await.expect("row");

    let uri = format!("/api/admin/access/{WAITER}/approve");
    let (status, body) = call(&app, "POST", &uri, TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "allowed");
    assert_eq!(db.get_access_request(WAITER).await.unwrap().unwrap().status, "allowed");

    let uri = format!("/api/admin/access/{WAITER}/deny");
    let (status, _) = call(&app, "POST", &uri, TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(db.get_access_request(WAITER).await.unwrap().unwrap().status, "denied");

    let (status, _) = call(&app, "POST", "/api/admin/access/did:plc:norow/approve", TEST_DID).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_access_admin -- --show-output`
Expected: FAIL — routes don't exist (404s where 403/200 expected).

- [ ] **Step 3: Implement**

Create `src/web/handlers/access.rs`:

```rust
// Admin allowlist handlers (#309) — the DB-backed layer of the access gate.
//
// Same authorization idiom as handlers/admin.rs: each handler checks
// `auth.is_admin` at the top and returns 403. Decision endpoints are
// idempotent — repeating a decision is a success, not an error.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};

use crate::db::traits::AccessRequestRow;
use crate::web::{AppState, AuthUser};

fn admin_guard(auth: &AuthUser) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if !auth.is_admin {
        return Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "Admin required"})),
        ));
    }
    Ok(())
}

fn row_json(r: &AccessRequestRow) -> serde_json::Value {
    serde_json::json!({
        "did": r.did,
        "handle": r.handle,
        "status": r.status,
        "requested_at": r.requested_at,
        "decided_at": r.decided_at,
        "decided_by": r.decided_by,
    })
}

/// GET /api/admin/access — every row, grouped by status, oldest first.
pub async fn list_access(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    admin_guard(&auth)?;
    let rows = state.db.list_access_requests().await.map_err(|e| {
        tracing::error!(error = %format!("{e:#}"), "failed to list access requests");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Database error"})),
        )
    })?;
    let group = |s: &str| -> Vec<serde_json::Value> {
        rows.iter().filter(|r| r.status == s).map(row_json).collect()
    };
    Ok(Json(serde_json::json!({
        "pending": group("pending"),
        "allowed": group("allowed"),
        "denied": group("denied"),
    })))
}

/// Shared body for approve/deny: set the status, 404 when no row exists.
async fn decide(
    state: &AppState,
    auth: &AuthUser,
    did: &str,
    status: &str,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    let found = state
        .db
        .set_access_status(did, status, &auth.did)
        .await
        .map_err(|e| {
            tracing::error!(error = %format!("{e:#}"), "failed to set access status");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "Database error"})),
            )
        })?;
    if !found {
        return Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "No access request for that DID"})),
        ));
    }
    tracing::info!(admin_did = %auth.did, target_did = %did, status, "Access decision");
    Ok(Json(serde_json::json!({"did": did, "status": status})))
}

/// POST /api/admin/access/{did}/approve
pub async fn approve_access(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Path(did): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    admin_guard(&auth)?;
    decide(&state, &auth, &did, "allowed").await
}

/// POST /api/admin/access/{did}/deny — also the revoke path for allowed DIDs.
pub async fn deny_access(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Path(did): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    admin_guard(&auth)?;
    decide(&state, &auth, &did, "denied").await
}
```

Add `pub mod access;` to `src/web/handlers/mod.rs`. Register routes in `src/web/mod.rs` inside `protected_api`, after the existing admin routes:

```rust
        .route(
            "/api/admin/access",
            get(handlers::access::list_access),
        )
        .route(
            "/api/admin/access/{did}/approve",
            post(handlers::access::approve_access),
        )
        .route(
            "/api/admin/access/{did}/deny",
            post(handlers::access::deny_access),
        )
```

- [ ] **Step 4: Run tests to verify pass**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_access_admin -- --show-output`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add src/web/handlers/access.rs src/web/handlers/mod.rs src/web/mod.rs tests/web_access_admin.rs
git commit -m 'feat(309): admin access endpoints - list, approve, deny'
```

---

### Task 7: Grant-by-handle — `POST /api/admin/access`

**Files:**
- Modify: `src/web/handlers/access.rs`
- Modify: `src/web/mod.rs` (change the `/api/admin/access` route to `get(...).post(...)`)
- Test: extend `tests/web_access_admin.rs`

**Interfaces:**
- Consumes: `PublicAtpClient::resolve_handle` (`src/bluesky/client.rs:174`), `grant_access` (Task 2).
- Produces: `POST /api/admin/access {"handle": "x.bsky.social"}` → `200 {"did", "handle", "status": "allowed"}` | `400` empty handle | `404` handle not found | `502` resolution infra failure.

- [ ] **Step 1: Write the failing tests**

Add to `tests/web_access_admin.rs` a `post_json` helper (same shape as `call` but with `.header("content-type", "application/json")` and `Body::from(body_string)`), then:

```rust
#[tokio::test]
async fn grant_by_handle_validates_input() {
    let (app, _db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    let (status, _) = post_json(&app, "/api/admin/access", TEST_DID, r#"{"handle": "  "}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}
```

Resolution success/404/502 depend on a live upstream, so the handler's mapping is copied verbatim from `pre_seed_user` (admin.rs:182–197) which is already covered; the integration test here covers the validation path and the non-admin 403 (extend the loop in `non_admin_gets_403_on_every_access_endpoint` with `("POST", "/api/admin/access")`).

- [ ] **Step 2: Run to verify failure**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_access_admin -- --show-output`
Expected: FAIL — 405/404, route has no POST.

- [ ] **Step 3: Implement**

In `src/web/handlers/access.rs`:

```rust
use crate::bluesky::client::PublicAtpClient;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct GrantRequest {
    pub handle: String,
}

/// POST /api/admin/access — grant access by Bluesky handle.
/// Resolution + error mapping copied from pre_seed_user (handlers/admin.rs):
/// 404 for unknown handles, 502 when resolution infrastructure fails.
pub async fn grant_access_by_handle(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Json(body): Json<GrantRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    admin_guard(&auth)?;
    let handle = body.handle.trim().trim_start_matches('@').to_string();
    if handle.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Handle is required"})),
        ));
    }

    let client = PublicAtpClient::new(&state.config.public_api_url).map_err(|e| {
        tracing::error!("Failed to create ATP client: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Internal error"})),
        )
    })?;
    let did = match client.resolve_handle(&handle).await {
        Ok(did) => did,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") || msg.contains("404") {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": format!("Handle not found: {handle}")})),
                ));
            }
            return Err((
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({"error": format!("Failed to resolve handle: {msg}")})),
            ));
        }
    };

    state.db.grant_access(&did, &handle, &auth.did).await.map_err(|e| {
        tracing::error!(error = %format!("{e:#}"), "failed to grant access");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "Database error"})),
        )
    })?;

    tracing::info!(admin_did = %auth.did, target_did = %did, target_handle = %handle, "Access granted by handle");
    Ok(Json(serde_json::json!({"did": did, "handle": handle, "status": "allowed"})))
}
```

Route change in `src/web/mod.rs`:

```rust
        .route(
            "/api/admin/access",
            get(handlers::access::list_access).post(handlers::access::grant_access_by_handle),
        )
```

- [ ] **Step 4: Run tests to verify pass**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_access_admin -- --show-output`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/web/handlers/access.rs src/web/mod.rs tests/web_access_admin.rs
git commit -m 'feat(309): grant access by handle endpoint'
```

---

### Task 8: Approve + scan — `POST /api/admin/access/{did}/approve-scan`

**Files:**
- Modify: `src/web/handlers/access.rs`
- Modify: `src/web/mod.rs` (one route)
- Test: extend `tests/web_access_admin.rs`

**Interfaces:**
- Consumes: `decide` (Task 6), `upsert_user`, `enqueue_scan`, `scan_job::build_user_fingerprint` + `ScanManager` fingerprint bookkeeping (the exact spawn block from `pre_seed_user`, admin.rs:230–249), `state.scan_wake`.
- Produces: `200 {"did", "access": "granted", "scan": "queued"}`; on enqueue failure `200 {"did", "access": "granted", "scan": "failed to queue"}` — approval survives, partial failure is explicit; `404` when no access row.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test]
async fn approve_scan_grants_seeds_and_queues() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_access_request_pending(WAITER, "w.bsky.social").await.expect("row");

    let uri = format!("/api/admin/access/{WAITER}/approve-scan");
    let (status, body) = call(&app, "POST", &uri, TEST_DID).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["access"], "granted");
    assert_eq!(body["scan"], "queued");
    assert_eq!(db.get_access_request(WAITER).await.unwrap().unwrap().status, "allowed");
    assert_eq!(
        db.get_user_handle(WAITER).await.unwrap().as_deref(),
        Some("w.bsky.social"),
        "user row pre-seeded from the access row's handle"
    );
    let queued = db
        .list_scan_queue()
        .await
        .unwrap()
        .into_iter()
        .any(|r| r.user_did == WAITER && r.status == "queued");
    assert!(queued, "scan enqueued");
}

#[tokio::test]
async fn approve_scan_404s_without_a_row() {
    let (app, _db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    let (status, _) = call(&app, "POST", "/api/admin/access/did:plc:norow/approve-scan", TEST_DID).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_access_admin -- --show-output`
Expected: FAIL — route missing.

- [ ] **Step 3: Implement**

In `src/web/handlers/access.rs` (imports: `use crate::web::scan_job;`):

```rust
/// POST /api/admin/access/{did}/approve-scan — approve AND kick off onboarding.
///
/// Two operations reported honestly: approval commits first; a scan-side
/// failure downgrades the `scan` field, never the approval. The enqueue goes
/// through `enqueue_scan` like every other scan (#257: one admission path).
pub async fn approve_access_and_scan(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
    Path(did): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<serde_json::Value>)> {
    admin_guard(&auth)?;
    // Reuse the approve path: 404 / decided_by / idempotency all match.
    decide(&state, &auth, &did, "allowed").await?;

    // The access row is the source of the handle for pre-seeding.
    let row = match state.db.get_access_request(&did).await {
        Ok(Some(row)) => row,
        Ok(None) | Err(_) => {
            // The decide() above just succeeded, so this is a read-back race
            // or DB blip — approval stands, scan does not happen.
            return Ok(Json(serde_json::json!({
                "did": did, "access": "granted", "scan": "failed to queue",
            })));
        }
    };

    // Pre-seed the user row if they have never signed in, spawning the same
    // background fingerprint build pre_seed_user does.
    let user_missing = state.db.get_user_handle(&did).await.ok().flatten().is_none();
    if user_missing {
        if let Err(e) = state.db.upsert_user(&did, &row.handle).await {
            tracing::error!(error = %format!("{e:#}"), "approve-scan: upsert_user failed");
            return Ok(Json(serde_json::json!({
                "did": did, "access": "granted", "scan": "failed to queue",
            })));
        }
        let db = state.db.clone();
        let config = state.config.clone();
        let scan_mgr = state.scan_manager.clone();
        let fp_did = did.clone();
        let fp_handle = row.handle.clone();
        {
            let mut mgr = scan_mgr.write().await;
            mgr.start_fingerprint_build(&fp_did);
        }
        tokio::spawn(async move {
            let result = scan_job::build_user_fingerprint(&config, &*db, &fp_did, &fp_handle).await;
            let mut mgr = scan_mgr.write().await;
            mgr.finish_fingerprint_build(&fp_did);
            if let Err(e) = result {
                tracing::error!(target_did = %fp_did, "Fingerprint build failed: {e}");
            }
        });
    }

    let scan = match state.db.enqueue_scan(&did).await {
        Ok(()) => {
            if let Some(wake) = &state.scan_wake {
                let _ = wake.try_send(());
            }
            "queued"
        }
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "approve-scan: enqueue failed");
            "failed to queue"
        }
    };

    tracing::info!(admin_did = %auth.did, target_did = %did, scan, "Approve + scan");
    Ok(Json(serde_json::json!({"did": did, "access": "granted", "scan": scan})))
}
```

Route:

```rust
        .route(
            "/api/admin/access/{did}/approve-scan",
            post(handlers::access::approve_access_and_scan),
        )
```

Note: `decide` currently returns `impl IntoResponse` — for reuse with `?` here it must return a concrete type. Change its signature to return `Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)>` (the existing callers are unaffected — `Json<Value>` is `IntoResponse`).

- [ ] **Step 4: Run tests to verify pass**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_access_admin -- --show-output`
Expected: PASS. (The spawned fingerprint build runs against the in-memory DB and fails harmlessly in tests — same as the existing `pre_seed_user` tests tolerate.)

- [ ] **Step 5: Commit**

```bash
git add src/web/handlers/access.rs src/web/mod.rs tests/web_access_admin.rs
git commit -m 'feat(309): approve-scan endpoint - grant plus pre-seed plus enqueue'
```

---

### Task 9: Scan cooldown (#258 minimal)

**Files:**
- Modify: `src/config.rs` (field + load + `test_defaults`)
- Modify: `src/web/handlers/scan.rs` (pure helper + check in `trigger_scan`)
- Modify: `.env.example` (document `CHARCOAL_SCAN_COOLDOWN_HOURS` AND the currently-undocumented `CHARCOAL_ADMIN_DIDS`)
- Test: unit tests inline in `scan.rs`; integration in `tests/web_scan_queue.rs`

**Interfaces:**
- Consumes: `list_scan_queue()` (existing), `ScanQueueRow.finished_at`.
- Produces: `Config.scan_cooldown_hours: u64` (default 24, 0 = disabled); `429 {"error": "...", "retry_at": "<RFC3339>"}` from `POST /api/scan` inside the window. `trigger_admin_scan` untouched.

- [ ] **Step 1: Write the failing tests**

Inline in `src/web/handlers/scan.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    #[test]
    fn inside_window_returns_retry_at() {
        let finished = (Utc::now() - Duration::hours(1)).to_rfc3339();
        let retry = cooldown_retry_at(&finished, Utc::now(), 24);
        assert!(retry.is_some());
        let expected = chrono::DateTime::parse_from_rfc3339(&finished).unwrap() + Duration::hours(24);
        assert_eq!(retry.unwrap(), expected.to_rfc3339());
    }

    #[test]
    fn outside_window_and_disabled_return_none() {
        let finished = (Utc::now() - Duration::hours(25)).to_rfc3339();
        assert!(cooldown_retry_at(&finished, Utc::now(), 24).is_none());
        let recent = (Utc::now() - Duration::hours(1)).to_rfc3339();
        assert!(cooldown_retry_at(&recent, Utc::now(), 0).is_none(), "0 disables");
    }

    #[test]
    fn unparseable_finished_at_never_blocks() {
        assert!(cooldown_retry_at("not-a-timestamp", Utc::now(), 24).is_none());
    }
}
```

Integration, in `tests/web_scan_queue.rs` (uses the file's existing helpers; drive the row to `done` through the trait):

```rust
#[tokio::test]
async fn a_completed_scan_starts_the_cooldown() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");
    db.enqueue_scan(USER_A).await.expect("enqueue");
    let claim = db.claim_next_scan(1, 600).await.expect("claim").expect("claimed");
    db.finish_queued_scan(USER_A, &claim.claim_id, None).await.expect("finish");

    let (status, body) = post_scan(&app, USER_A).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(body["retry_at"].is_string(), "retry_at present: {body}");
}

#[tokio::test]
async fn a_failed_scan_does_not_start_the_cooldown() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    db.upsert_user(USER_A, "a.bsky.social").await.expect("user");
    db.enqueue_scan(USER_A).await.expect("enqueue");
    let claim = db.claim_next_scan(1, 600).await.expect("claim").expect("claimed");
    db.finish_queued_scan(USER_A, &claim.claim_id, Some("boom")).await.expect("finish");

    let (status, _) = post_scan(&app, USER_A).await;
    assert_eq!(status, StatusCode::ACCEPTED, "failed scans may retry immediately");
}
```

Admin-bypass integration test goes in `tests/web_access_admin.rs`: drive USER row to `done` the same way, then `POST /api/admin/users/{did}/scan` as `TEST_DID` → expect `202`.

- [ ] **Step 2: Run to verify failure**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_scan_queue -- --show-output`
Expected: COMPILE FAIL (`cooldown_retry_at` undefined), then 202-vs-429 FAIL once it compiles.

- [ ] **Step 3: Implement**

`src/config.rs` — field on `Config`:

```rust
    /// Minimum hours between one user's successful scans (CHARCOAL_SCAN_COOLDOWN_HOURS).
    /// 0 disables the cooldown. Admin-triggered scans bypass it.
    #[cfg(feature = "web")]
    pub scan_cooldown_hours: u64,
```

In `Config::load`:

```rust
        #[cfg(feature = "web")]
        let scan_cooldown_hours = env::var("CHARCOAL_SCAN_COOLDOWN_HOURS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(24);
```

(and the field in the `Self { ... }` literal). In `Config::test_defaults()` add `scan_cooldown_hours: 24`.

`src/web/handlers/scan.rs` — pure helper:

```rust
/// If the last successful scan finished inside the cooldown window, the
/// RFC3339 instant at which the next scan becomes available. None = no
/// cooldown (elapsed, disabled, or unparseable timestamp — never block on
/// bad data).
pub(crate) fn cooldown_retry_at(
    finished_at: &str,
    now: chrono::DateTime<chrono::Utc>,
    cooldown_hours: u64,
) -> Option<String> {
    if cooldown_hours == 0 {
        return None;
    }
    let finished = chrono::DateTime::parse_from_rfc3339(finished_at).ok()?;
    let retry_at = finished + chrono::Duration::hours(cooldown_hours as i64);
    if now < retry_at {
        Some(retry_at.to_rfc3339())
    } else {
        None
    }
}
```

In `trigger_scan`, after the user-existence check and before `enqueue_scan` (queued/running rows fall through to the existing idempotent enqueue — the cooldown only reads `done` rows):

```rust
    // #258: one successful scan per user per cooldown window. Failed scans
    // don't count, and the admin trigger path (handlers/admin.rs) deliberately
    // has no such check — that is the operator's bypass.
    match state.db.list_scan_queue().await {
        Ok(rows) => {
            if let Some(row) = rows.iter().find(|r| r.user_did == auth.did) {
                if row.status == "done" {
                    if let Some(finished_at) = &row.finished_at {
                        if let Some(retry_at) = cooldown_retry_at(
                            finished_at,
                            chrono::Utc::now(),
                            state.config.scan_cooldown_hours,
                        ) {
                            return (
                                StatusCode::TOO_MANY_REQUESTS,
                                Json(serde_json::json!({
                                    "error": "You scanned recently — scans are limited to one per day",
                                    "retry_at": retry_at,
                                })),
                            )
                                .into_response();
                        }
                    }
                }
            }
        }
        Err(e) => {
            // A cooldown is an abuse guard, not a correctness gate: if we
            // cannot read the queue, let the enqueue proceed rather than
            // refusing service on a DB blip.
            tracing::warn!(error = %format!("{e:#}"), "cooldown check skipped — could not read scan queue");
        }
    }
```

`.env.example` — append:

```
# Comma-separated DIDs with admin privileges (admin dashboard, impersonation,
# allowlist management). Admins always retain access regardless of the
# allowlist table.
CHARCOAL_ADMIN_DIDS=

# Minimum hours between one user's successful scans. 0 disables.
# Admin-triggered scans bypass the cooldown.
CHARCOAL_SCAN_COOLDOWN_HOURS=24
```

- [ ] **Step 4: Run tests to verify pass**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_scan_queue --test web_access_admin -- --show-output`
Expected: PASS, including all pre-existing queue tests (no existing test re-scans after `done`; if one does, it now needs a `finished_at` older than 24h — fix by adjusting that test's setup, not by weakening the cooldown).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs src/web/handlers/scan.rs .env.example tests/web_scan_queue.rs tests/web_access_admin.rs
git commit -m 'feat(258,309): per-user scan cooldown with admin bypass'
```

---

### Task 10: Frontend plumbing + placeholder `/waitlist`

**Files:**
- Modify: `web/src/lib/api.ts` (`AccessRevokedError`, 403 handling, five wrappers)
- Modify: `web/src/lib/types.ts` (`AccessRequest`, `AccessListResponse`, `ApproveScanResponse`)
- Modify: `web/src/routes/(protected)/+layout.svelte` (route `AccessRevokedError` → `/waitlist`)
- Create: `web/src/routes/waitlist/+page.svelte` (tokens-compliant placeholder — replaced in Task 11)
- Test: `web/src/lib/api-errors.test.ts` (new)

**Interfaces:**
- Consumes: the `access_revoked` 403 contract (Task 4), endpoint shapes (Tasks 6–8).
- Produces (used by Task 11's UI):

```ts
export class AccessRevokedError extends Error {}
export async function getAccessRequests(): Promise<AccessListResponse>;
export async function grantAccess(handle: string): Promise<{ did: string; handle: string; status: string }>;
export async function approveAccess(did: string): Promise<void>;
export async function approveAccessAndScan(did: string): Promise<ApproveScanResponse>;
export async function denyAccess(did: string): Promise<void>;
```

- [ ] **Step 1: Write the failing vitest**

Create `web/src/lib/api-errors.test.ts`:

```ts
import { afterEach, describe, expect, it, vi } from 'vitest';
import { AccessRevokedError, AuthError, getStatus } from './api.js';

function mockFetch(status: number, body: unknown) {
	vi.stubGlobal(
		'fetch',
		vi.fn(async () => new Response(JSON.stringify(body), { status }))
	);
}

afterEach(() => vi.unstubAllGlobals());

describe('apiFetch error classification', () => {
	it('throws AuthError on 401', async () => {
		mockFetch(401, {});
		await expect(getStatus()).rejects.toBeInstanceOf(AuthError);
	});

	it('throws AccessRevokedError on 403 with code access_revoked', async () => {
		mockFetch(403, { error: 'Access is not currently active', code: 'access_revoked' });
		await expect(getStatus()).rejects.toBeInstanceOf(AccessRevokedError);
	});

	it('throws plain Error on a 403 without the code', async () => {
		mockFetch(403, { error: 'Admin required' });
		const err = await getStatus().catch((e) => e);
		expect(err).toBeInstanceOf(Error);
		expect(err).not.toBeInstanceOf(AccessRevokedError);
	});
});
```

- [ ] **Step 2: Run to verify failure**

Run: `npm --prefix web run test -- --run api-errors`
Expected: FAIL — `AccessRevokedError` not exported. (Check `web/package.json` for the exact test script name; the vitest suite already exists.)

- [ ] **Step 3: Implement**

`web/src/lib/api.ts` — after `AuthError`:

```ts
/** 403 with code "access_revoked": the DID is no longer on the allowlist.
 *  Callers route to /waitlist instead of rendering a broken dashboard. */
export class AccessRevokedError extends Error {
	constructor() {
		super('Access is not currently active for this account');
		this.name = 'AccessRevokedError';
	}
}
```

In `apiFetch`, replace the `if (!res.ok)` block so 403 is inspected before the generic throw:

```ts
	if (res.status === 401) {
		throw new AuthError();
	}
	if (!res.ok) {
		const body = await res.json().catch(() => ({}));
		if (res.status === 403 && body.code === 'access_revoked') {
			throw new AccessRevokedError();
		}
		throw new Error(body.error ?? `HTTP ${res.status}`);
	}
```

Wrappers at the bottom, `// ---- Access (allowlist) ----` section:

```ts
export async function getAccessRequests(): Promise<AccessListResponse> {
	return apiFetch<AccessListResponse>('/api/admin/access');
}

export async function grantAccess(
	handle: string
): Promise<{ did: string; handle: string; status: string }> {
	return apiFetch('/api/admin/access', {
		method: 'POST',
		headers: { 'Content-Type': 'application/json' },
		body: JSON.stringify({ handle })
	});
}

export async function approveAccess(did: string): Promise<void> {
	await apiFetch(`/api/admin/access/${encodeURIComponent(did)}/approve`, { method: 'POST' });
}

export async function approveAccessAndScan(did: string): Promise<ApproveScanResponse> {
	return apiFetch<ApproveScanResponse>(
		`/api/admin/access/${encodeURIComponent(did)}/approve-scan`,
		{ method: 'POST' }
	);
}

export async function denyAccess(did: string): Promise<void> {
	await apiFetch(`/api/admin/access/${encodeURIComponent(did)}/deny`, { method: 'POST' });
}
```

`web/src/lib/types.ts`:

```ts
export interface AccessRequest {
	did: string;
	handle: string;
	status: 'pending' | 'allowed' | 'denied';
	requested_at: string;
	decided_at: string | null;
	decided_by: string | null;
}

export interface AccessListResponse {
	pending: AccessRequest[];
	allowed: AccessRequest[];
	denied: AccessRequest[];
}

export interface ApproveScanResponse {
	did: string;
	access: string;
	/** "queued" on full success; anything else is an honest partial failure. */
	scan: string;
}
```

(Add `AccessListResponse`/`ApproveScanResponse` to the type import at the top of `api.ts`.)

`(protected)/+layout.svelte`: import `AccessRevokedError` from `$lib/api.js`; where the layout's identity fetch catches `AuthError` and calls `goto('/login')`, add the analogous branch:

```ts
		} else if (e instanceof AccessRevokedError) {
			goto('/waitlist');
```

Then run `rg -n "AuthError" web/src` and apply the identical two-line pattern at every catch site that redirects to `/login` (pages catch independently — dashboard, accounts, review at minimum).

`web/src/routes/waitlist/+page.svelte` — placeholder (Task 11 replaces it):

```svelte
<script lang="ts">
	import '$lib/website/styles/tokens.css';
</script>

<svelte:head>
	<title>Charcoal — You're on the list</title>
</svelte:head>

<main class="waitlist">
	<h1>You're on the list</h1>
	<p>
		Charcoal is onboarding a small number of people at a time. Your request has
		been recorded — if you're granted access, signing in with your Bluesky
		account will just work.
	</p>
	<a href="/">Back to the front page</a>
</main>

<style>
	.waitlist {
		max-width: 32rem;
		margin: 4rem auto;
		padding: 0 1rem;
		font-family: var(--font-body);
		color: var(--cream-100);
	}
	h1 {
		font-family: var(--font-display);
	}
</style>
```

(Verify the token names against `web/src/lib/website/styles/tokens.css` before using — promote, never invent.)

- [ ] **Step 4: Run tests + build to verify**

Run: `npm --prefix web run test -- --run` then `npm --prefix web run build`
Expected: vitest PASS (including the 36 pre-existing), build clean.

For any `.svelte` edits, run the svelte-autofixer MCP validation per house rules.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/api.ts web/src/lib/types.ts web/src/lib/api-errors.test.ts web/src/routes/waitlist/+page.svelte 'web/src/routes/(protected)/+layout.svelte'
git commit -m 'feat(309): frontend plumbing - access wrappers, access_revoked routing, waitlist placeholder'
```

(Also stage any additional page files touched by the `AuthError` sweep.)

---

### Task 11: The UI — invoke impeccable

**Files:**
- Rework: `web/src/routes/waitlist/+page.svelte`
- Modify: `web/src/routes/(protected)/admin/+page.svelte` (new "Access" area)
- Possibly: `web/src/lib/website/styles/tokens.css`, `DESIGN.md` (only via impeccable's documenter)

**Interfaces:**
- Consumes: everything from Task 10. No backend changes permitted in this task.

- [ ] **Step 1: Invoke the impeccable skill** (`Skill: impeccable:impeccable`) with this brief:

> Two surfaces inside the existing Hearth Watch design system (DESIGN.md + tokens.css — promote shipped values into tokens, never invent hex):
> 1. `/waitlist` — replace the placeholder. Content contract from the spec: confirms the person is on the list; NO status detail; pending and denied must be indistinguishable; calm, non-provocative tone (this page's audience includes exactly the population Charcoal guards against).
> 2. Admin page "Access" area on `(protected)/admin/+page.svelte`: pending requests (handle + requested-at; actions Approve / Approve + scan / Deny, `confirm()` before Deny), access list (allowed + denied, with Revoke / Approve), grant-by-handle form using the page's `@`-input idiom. Data via `getAccessRequests()`, `grantAccess()`, `approveAccess()`, `approveAccessAndScan()`, `denyAccess()` from `$lib/api.js`; refetch after every action. Surface `ApproveScanResponse.scan !== "queued"` as a visible partial-failure message, not a silent success.
> Known IA wrinkle, yours to resolve: the page now has two add-by-handle forms (pre-seed "Add Protected User" vs "Grant access") — merge, relabel, or reposition so they are distinguishable.
> Match the page's existing accessibility care: `.sr-only` full text for truncated content, `role` semantics, focus states.

- [ ] **Step 2: Follow impeccable's own process** (direction → comp → Bryan's approval → build → finish review → documenter). Do not skip its review gate.

- [ ] **Step 3: Validate + build**

Run: `npm --prefix web run test -- --run` and `npm --prefix web run build`, plus svelte-autofixer on every touched `.svelte` file.
Expected: clean.

- [ ] **Step 4: Rebuild the embedded SPA and re-run backend tests** (the binary embeds `web/build`):

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:"`
Expected: zero SKIP lines, suite green.

- [ ] **Step 5: Commit**

```bash
git add web/src/routes/waitlist/+page.svelte 'web/src/routes/(protected)/admin/+page.svelte'
git commit -m 'feat(309): waitlist page + admin access area (impeccable)'
```

(Stage tokens.css / DESIGN.md too if impeccable's documenter touched them.)

---

### Task 12: Full verification sweep + changelog

**Files:**
- Modify: `CHANGELOG.md` (handwritten entry — `chainlink issue close` writes a BACKWARDS line; always use `--no-changelog`)

- [ ] **Step 1: Full test matrix**

```bash
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:"
```
Expected: empty (zero model-gated skips — note the exact `SKIP:` sentinel, no `-i`).

```bash
CHARCOAL_MODEL_DIR=./models cargo test --features web
DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres
cargo clippy --features web --all-targets
cargo clippy --features postgres --all-targets
cargo clippy --all-targets
npm --prefix web run test -- --run
```
Expected: all green, clippy clean on all three feature sets (CI runs a newer clippy — expect possible new-lint surprises there, not here).

- [ ] **Step 2: CHANGELOG entry** (top of file, matching its format):

```markdown
## 2026-08-24 — Allowlist admin UI + scan cooldown (#309, #258)

Gated onboarding no longer means editing a Railway env var. A denied Bluesky
sign-in now records a pending access request and lands on a styled /waitlist
page (previously: raw JSON on a bare page). Admins manage the list from the
dashboard: approve, approve + first scan, deny (sticky and indistinguishable
from pending, by design), grant by handle. The env var remains as a
lockout-proof bootstrap; admins are implicitly allowed. Schema v14
(access_requests). Plus #258's minimal cooldown: one successful scan per user
per CHARCOAL_SCAN_COOLDOWN_HOURS (default 24, 0 disables, admin bypasses).
```

- [ ] **Step 3: Deciduous sync + final audit**

```bash
deciduous nodes | tail -20
deciduous edges | tail -20
deciduous sync
```
Verify every task's action has an outcome and no orphans.

- [ ] **Step 4: Commit + push**

```bash
git add CHANGELOG.md
git commit -m 'docs(309): changelog for allowlist admin UI + cooldown'
git push -u origin feat/allowlist-admin-ui
```

(Pre-push cold-compiles default-feature gates — expect a slow first push; do not interrupt it.)

- [ ] **Step 5: Hand off** — do NOT merge or open the PR unprompted. Report completion to Bryan; the finishing-a-development-branch skill takes it from here (PR → staging per house promotion rules, CodeRabbit loop until APPROVED with per-thread replies).

---

## Plan Self-Review (performed at write time)

- **Spec coverage:** data model (T1–3), gate + fail-closed + revoked 403 (T4), auto-waitlist OAuth (T5), five endpoints (T6–8), cooldown + env docs (T9), frontend plumbing + waitlist route + revoked routing (T10), UI via impeccable incl. the two-forms wrinkle and partial-failure surfacing (T11), test matrix incl. Postgres parity and version pins (T1, T3, T12). Out-of-scope items from the spec are absent here, as intended.
- **Known judgment calls an implementer may hit:** (a) `tests/web_oauth.rs` harness may not drive the callback fully — Task 5 says where to land assertions if so; (b) exact sqlx row-access idiom in `postgres.rs` — copy the scan_queue methods; (c) if the `postgres` feature build breaks between T2 and T3, land them as one commit.
- **Type consistency:** `AccessRequestRow` fields, `check_access` signature, `cooldown_retry_at` signature, and the five TS wrappers are each defined once and referenced identically in later tasks.
