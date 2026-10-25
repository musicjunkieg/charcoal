# Scan Cost Ledger Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Record the estimated GPU cost of every full scan and refresh in a new `scan_costs` table, log it, and show per-user and windowed totals on the admin page.

**Architecture:** `build_scan_scorers` creates the per-scan `ScanCostMeter` itself and keeps a handle in `ScanScorers`. A new `observability::scan_cost` module turns the meter + classifier cache stats into a `ScanCostRow`, logs it, and inserts it (best-effort). The full scan and the refresh pipeline each call it once, on success and on failure. An admin endpoint aggregates the table; the admin page renders it.

**Tech Stack:** Rust (tokio, rusqlite, sqlx-core/sqlx-postgres, axum, chrono, tracing), SvelteKit 5 + vitest.

Spec: `docs/superpowers/specs/2026-10-24-393-scan-cost-ledger-design.md`.

## Global Constraints

- Schema version **20**, both backends. Postgres file `migrations/postgres/0020_scan_costs.sql` MUST end with `INSERT INTO schema_version (version) VALUES (20) ON CONFLICT DO NOTHING;`.
- `kind` values are `ScanKind::as_str()`: `"full"` or `"refresh"`. `outcome` is `"completed"` or `"failed"`.
- Timestamps are UTC RFC 3339 with whole seconds and `Z`: `chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)` — lexicographic order equals time order.
- Writing a cost row NEVER fails a scan: errors are `warn!`-logged and swallowed.
- UI labels the figure **"est., excludes idle time"**. Onboarding flag threshold: **200 cents**.
- No `.unwrap()` in non-test code; `anyhow::Result`; comments explain *why*.
- Model-gated tests need `CHARCOAL_MODEL_DIR=./models`. Postgres tests: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --features postgres --test db_postgres <name>`.
- Run only targeted tests locally; pre-push + CI run the rest. Never background `cargo test` inside a subagent and walk away.
- Any `.svelte` edit: invoke the svelte skill and run `mcp__svelte__svelte-autofixer` on the file.
- Stage files by explicit path only; conventional commits; end messages with the session's Co-Authored-By / Claude-Session lines.

---

### Task 1: Meter reports peak concurrency and a snapshot

**Files:**
- Modify: `src/toxicity/cost_meter.rs` (struct `Acc` ~48–89; `impl ScanCostMeter` near `estimated_cents` ~214; `mod acc_tests` ~328)
- Test: `tests/unit_cost_meter.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq)]
  pub struct CostSnapshot { pub worker_secs: f64, pub peak_in_flight: u32, pub est_cents: u32, pub rate_cents_per_hour: u32 }
  impl ScanCostMeter { pub fn snapshot(&self) -> CostSnapshot }
  ```

- [ ] **Step 1: Failing unit test for peak tracking** — add to `mod acc_tests`:

```rust
    #[test]
    fn peak_tracks_most_concurrent_requests_capped_at_workers_max() {
        let mut a = Acc::new(2);
        a.enter(0.0);
        a.enter(1.0);
        a.enter(2.0); // 3 in flight, but RunPod runs at most 2 workers
        a.leave(3.0);
        a.leave(4.0);
        a.leave(5.0);
        assert_eq!(a.peak_in_flight, 2);
    }
```

- [ ] **Step 2: Failing integration test for snapshot** — append to `tests/unit_cost_meter.rs`:

```rust
#[test]
fn snapshot_reports_integral_rate_and_matching_cents() {
    let m = ScanCostMeter::new(500, 329);
    m.force_worker_seconds(3600.0);
    let s = m.snapshot();
    assert_eq!(s.rate_cents_per_hour, 329);
    assert_eq!(s.est_cents, 329);
    assert!(s.worker_secs >= 3600.0);
    assert_eq!(s.peak_in_flight, 0, "no request ever entered");
}

#[test]
fn snapshot_of_an_unused_meter_is_zero() {
    let s = ScanCostMeter::new(500, 329).snapshot();
    assert_eq!((s.worker_secs, s.est_cents, s.peak_in_flight), (0.0, 0, 0));
}
```

(Check `force_worker_seconds`' exact semantics at ~line 299 first; if it sets rather than adds, the `>=` holds either way.)

- [ ] **Step 3: Run, expect compile failure** — `cargo test --test unit_cost_meter snapshot && cargo test --lib acc_tests::peak` → errors: no field `peak_in_flight`, no method `snapshot`.

- [ ] **Step 4: Implement.** In `struct Acc` add `peak_in_flight: u32,` (init `0` in `Acc::new`). In `Acc::enter`, after `self.in_flight += 1;`:

```rust
        // Most workers that were ever busy at once. Stored with the scan's
        // cost so the 60 s idle tail RunPod bills per worker (which the
        // integral cannot see) can be estimated later against real billing.
        self.peak_in_flight = self.peak_in_flight.max(self.in_flight.min(self.workers_max));
```

Above `struct ScanCostMeter` add the `CostSnapshot` struct (doc: "Point-in-time reading of a meter, persisted per scan (#393). A lower bound on the bill: excludes idle tails and cold starts."). In `impl ScanCostMeter`, after `estimated_cents`:

```rust
    /// Everything the scan cost ledger (#393) stores, read under one lock so
    /// the cents and the integral agree.
    pub fn snapshot(&self) -> CostSnapshot {
        let now = self.now_secs();
        let mut acc = self.acc.lock().unwrap();
        acc.accrue(now);
        CostSnapshot {
            worker_secs: acc.worker_secs,
            peak_in_flight: acc.peak_in_flight,
            est_cents: (acc.worker_secs / 3600.0 * self.rate_cents_per_hour as f64) as u32,
            rate_cents_per_hour: self.rate_cents_per_hour,
        }
    }
```

(The existing `.lock().unwrap()` pattern in this file is the established idiom for a poisoned-mutex-is-a-bug lock; match it.)

- [ ] **Step 5: Run tests, expect PASS** — same commands as Step 3.
- [ ] **Step 6: Commit** — `git add src/toxicity/cost_meter.rs tests/unit_cost_meter.rs` → `feat(393): cost meter reports peak concurrency and a snapshot`.

---

### Task 2: The scan keeps a handle on its meter

**Files:**
- Modify: `src/toxicity/classifier.rs:170-212` (`build_from_env`)
- Modify: `src/web/scan_setup.rs:173-224` (`ScanScorers`, `build_scan_scorers`)
- Test: `tests/unit_classifier.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn build_from_env_with_meter(meter: Arc<ScanCostMeter>) -> Result<Arc<dyn ToxicityClassifier>>
  // ScanScorers gains: pub cost_meter: Arc<crate::toxicity::cost_meter::ScanCostMeter>
  ```

- [ ] **Step 1: Failing test** — in `tests/unit_classifier.rs`, beside the existing env-driven `build_from_env` tests (follow their env-var setup/teardown pattern exactly, including any serial lock they use):

```rust
#[test]
fn build_from_env_with_meter_meters_through_the_given_meter() {
    // set CHARCOAL_CLASSIFIER=runpod, RUNPOD_ENDPOINT_URL, RUNPOD_API_KEY as the
    // neighbouring runpod build test does
    let meter = std::sync::Arc::new(charcoal::toxicity::cost_meter::ScanCostMeter::new(500, 329));
    let c = charcoal::toxicity::classifier::build_from_env_with_meter(std::sync::Arc::clone(&meter))
        .expect("runpod backend builds");
    assert_eq!(c.name(), "runpod");
    // The caller's handle is the client's meter: two strong refs now exist.
    assert_eq!(std::sync::Arc::strong_count(&meter), 2);
}
```

(Use the exact `name()` string the neighbouring runpod test asserts.)

- [ ] **Step 2: Run, expect FAIL** — `cargo test --test unit_classifier build_from_env_with_meter` → "cannot find function".

- [ ] **Step 3: Implement.** Rename the body of `build_from_env` into `build_from_env_with_meter(meter)`; in the `"runpod"` arm replace the `let meter = …from_env()` line with use of the parameter (`.with_meter(meter)`); the `"zentropi"` arm ignores it — add `let _ = &meter;` is NOT needed if the param is simply unused in that arm (it is used in another arm, so no warning). Then:

```rust
/// As [`build_from_env_with_meter`], with a fresh meter from the environment.
/// Callers that need the scan's cost afterwards (#393) build the meter
/// themselves and pass it in.
pub fn build_from_env() -> Result<Arc<dyn ToxicityClassifier>> {
    build_from_env_with_meter(Arc::new(crate::toxicity::cost_meter::ScanCostMeter::from_env()))
}
```

In `scan_setup.rs`: add `pub cost_meter: Arc<crate::toxicity::cost_meter::ScanCostMeter>,` to `ScanScorers` (doc: "This scan's GPU cost meter (#393). Zentropi scans leave it at zero — that backend bills per request, not per GPU-second."). In `build_scan_scorers`, before the classifier is built:

```rust
    // #393: the meter is created here, not inside the classifier, so the
    // scan can read what it spent when it finishes.
    let cost_meter = Arc::new(crate::toxicity::cost_meter::ScanCostMeter::from_env());
```

replace `crate::toxicity::classifier::build_from_env()?` with `crate::toxicity::classifier::build_from_env_with_meter(Arc::clone(&cost_meter))?`, and add `cost_meter,` to the returned struct literal.

- [ ] **Step 4: Run, expect PASS** — Step 2 command, plus `cargo clippy --features web --all-targets -- -D warnings`.
- [ ] **Step 5: Commit** — `git add src/toxicity/classifier.rs src/web/scan_setup.rs tests/unit_classifier.rs` → `feat(393): scan scorers keep a handle on the scan's cost meter`.

---

### Task 3: `scan_costs` table, both backends

**Files:**
- Create: `migrations/postgres/0020_scan_costs.sql`
- Modify: `src/db/schema.rs` (after v19 block ~675–682; pinned-version tests at ~749, ~821, ~926, ~936, ~1159, ~1164, ~1400)
- Modify: `src/db/sqlite.rs` (`test_trait_table_count` ~1213 + its comment list)
- Modify: `src/db/postgres.rs` (`all_migrations` ~297 array length + entry after ~374; `delete_user_data` ~1739; new impls)
- Modify: `src/db/traits.rs` (row structs near `AccessRequestRow` ~388; methods near access-requests group ~1174)
- Modify: `src/db/queries.rs` (`delete_user_data` ~1374; new fns)
- Test: `tests/unit_scan_costs.rs` (create), `tests/db_postgres.rs`

**Interfaces:**
- Produces (in `src/db/traits.rs`, re-exported the same way `AccessRequestRow` is):
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub struct ScanCostRow {
      pub user_did: String, pub kind: String, pub outcome: String,
      pub started_at: String, pub finished_at: String,
      pub classifications: i64, pub classifier_cache_hits: i64,
      pub metered_worker_secs: f64, pub peak_in_flight: i64,
      pub est_cents: i64, pub rate_cents_per_hour: i64,
  }
  #[derive(Debug, Clone, PartialEq, serde::Serialize)]
  pub struct UserScanCost {
      pub user_did: String, pub handle: Option<String>,
      pub first_full_scan_cents: Option<i64>, pub refresh_cents: i64,
      pub total_cents: i64, pub runs: i64, pub last_finished_at: String,
  }
  // trait Database:
  async fn insert_scan_cost(&self, row: &ScanCostRow) -> Result<()>;
  async fn list_user_scan_costs(&self) -> Result<Vec<UserScanCost>>;   // highest total first
  async fn scan_cost_cents_since(&self, since: &str) -> Result<i64>;   // started_at >= since
  ```

- [ ] **Step 1: Failing SQLite tests** — create `tests/unit_scan_costs.rs`:

```rust
//! scan_costs ledger (#393): insert, per-user aggregates, windows, deletion.
use std::sync::Arc;

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::{Database, ScanCostRow};
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

fn row(did: &str, kind: &str, started: &str, cents: i64) -> ScanCostRow {
    ScanCostRow {
        user_did: did.into(), kind: kind.into(), outcome: "completed".into(),
        started_at: started.into(), finished_at: started.into(),
        classifications: 10, classifier_cache_hits: 2,
        metered_worker_secs: 1.5, peak_in_flight: 1,
        est_cents: cents, rate_cents_per_hour: 329,
    }
}

#[tokio::test]
async fn aggregates_first_full_scan_refreshes_and_total_per_user() {
    let db = setup_db();
    db.insert_scan_cost(&row("did:plc:a", "full", "2026-10-01T00:00:00Z", 150)).await.unwrap();
    db.insert_scan_cost(&row("did:plc:a", "full", "2026-10-05T00:00:00Z", 40)).await.unwrap();
    db.insert_scan_cost(&row("did:plc:a", "refresh", "2026-10-02T00:00:00Z", 7)).await.unwrap();
    db.insert_scan_cost(&row("did:plc:b", "refresh", "2026-10-03T00:00:00Z", 3)).await.unwrap();

    let users = db.list_user_scan_costs().await.unwrap();
    assert_eq!(users.len(), 2);
    let a = &users[0]; // highest total first
    assert_eq!(a.user_did, "did:plc:a");
    assert_eq!(a.first_full_scan_cents, Some(150), "the EARLIEST full scan");
    assert_eq!(a.refresh_cents, 7);
    assert_eq!(a.total_cents, 197);
    assert_eq!(a.runs, 3);
    assert_eq!(a.last_finished_at, "2026-10-05T00:00:00Z");
    assert_eq!(users[1].first_full_scan_cents, None, "b never had a full scan");
}

#[tokio::test]
async fn window_sums_only_rows_started_on_or_after_since() {
    let db = setup_db();
    db.insert_scan_cost(&row("did:plc:a", "full", "2026-10-01T00:00:00Z", 100)).await.unwrap();
    db.insert_scan_cost(&row("did:plc:a", "refresh", "2026-10-20T00:00:00Z", 5)).await.unwrap();
    assert_eq!(db.scan_cost_cents_since("2026-10-20T00:00:00Z").await.unwrap(), 5);
    assert_eq!(db.scan_cost_cents_since("2026-09-01T00:00:00Z").await.unwrap(), 105);
    assert_eq!(db.scan_cost_cents_since("2026-11-01T00:00:00Z").await.unwrap(), 0, "empty window is 0, not NULL");
}

#[tokio::test]
async fn delete_user_data_removes_that_users_cost_rows_only() {
    let db = setup_db();
    db.insert_scan_cost(&row("did:plc:a", "full", "2026-10-01T00:00:00Z", 100)).await.unwrap();
    db.insert_scan_cost(&row("did:plc:b", "full", "2026-10-01T00:00:00Z", 50)).await.unwrap();
    db.delete_user_data("did:plc:a").await.unwrap();
    let users = db.list_user_scan_costs().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].user_did, "did:plc:b");
}
```

- [ ] **Step 2: Run, expect compile failure** — `cargo test --test unit_scan_costs` → unresolved `ScanCostRow`.

- [ ] **Step 3: Postgres migration** — create `migrations/postgres/0020_scan_costs.sql`:

```sql
-- Migration v20 (#393): one row per full scan or refresh recording its
-- estimated GPU cost. The figure is the in-app meter's worker-seconds
-- integral, a LOWER BOUND on the RunPod bill (it cannot see the idle tail
-- or cold start each worker is billed for). Cascaded by delete_user_data.
CREATE TABLE IF NOT EXISTS scan_costs (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_did TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('full', 'refresh')),
    outcome TEXT NOT NULL CHECK (outcome IN ('completed', 'failed')),
    started_at TEXT NOT NULL,
    finished_at TEXT NOT NULL,
    classifications BIGINT NOT NULL,
    classifier_cache_hits BIGINT NOT NULL,
    metered_worker_secs DOUBLE PRECISION NOT NULL,
    peak_in_flight BIGINT NOT NULL,
    est_cents BIGINT NOT NULL,
    rate_cents_per_hour BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS scan_costs_user_started ON scan_costs (user_did, started_at);
CREATE INDEX IF NOT EXISTS scan_costs_started ON scan_costs (started_at);

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (20) ON CONFLICT DO NOTHING;
```

In `postgres.rs`: `fn all_migrations() -> [(i64, &'static str); 20]` and append `(20, include_str!("../../migrations/postgres/0020_scan_costs.sql")),`.

- [ ] **Step 4: SQLite migration** — in `schema.rs`, after the v19 `run_migration` block and before `Ok(())`:

```rust
    // v20 (#393): per-scan GPU cost ledger. Mirrors 0020_scan_costs.sql.
    run_migration(conn, 20, max_version, |c| {
        c.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS scan_costs (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 user_did TEXT NOT NULL,
                 kind TEXT NOT NULL CHECK (kind IN ('full', 'refresh')),
                 outcome TEXT NOT NULL CHECK (outcome IN ('completed', 'failed')),
                 started_at TEXT NOT NULL,
                 finished_at TEXT NOT NULL,
                 classifications INTEGER NOT NULL,
                 classifier_cache_hits INTEGER NOT NULL,
                 metered_worker_secs REAL NOT NULL,
                 peak_in_flight INTEGER NOT NULL,
                 est_cents INTEGER NOT NULL,
                 rate_cents_per_hour INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS scan_costs_user_started ON scan_costs (user_did, started_at);
             CREATE INDEX IF NOT EXISTS scan_costs_started ON scan_costs (started_at);
             COMMIT;",
        )
    })?;
```

Bump pinned tests: `(1..=19)` → `(1..=20)` (~821, ~936); `max, 19` → `max, 20` (~1164, ~1400); table count `20` → `21` (~749, ~926, ~1159, `sqlite.rs` ~1213) and add `scan_costs` to the `test_trait_table_count` comment list. Read each assertion before changing it — if one pins a *specific* earlier version on purpose (e.g. `create_tables_through(…, 18)`), leave it.

- [ ] **Step 5: Trait + structs** — add the two structs (Interfaces above, each with a one-line doc citing #393) beside `AccessRequestRow` and the three methods in a `// --- Scan cost ledger (#393) ---` group beside the access-request methods. Re-export `ScanCostRow`/`UserScanCost` from `crate::db` exactly as `AccessRequestRow` is.

- [ ] **Step 6: SQLite queries** — in `queries.rs`:

```rust
pub fn insert_scan_cost(conn: &Connection, r: &crate::db::traits::ScanCostRow) -> Result<()> {
    conn.execute(
        "INSERT INTO scan_costs (user_did, kind, outcome, started_at, finished_at,
             classifications, classifier_cache_hits, metered_worker_secs,
             peak_in_flight, est_cents, rate_cents_per_hour)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![r.user_did, r.kind, r.outcome, r.started_at, r.finished_at,
                r.classifications, r.classifier_cache_hits, r.metered_worker_secs,
                r.peak_in_flight, r.est_cents, r.rate_cents_per_hour],
    )?;
    Ok(())
}

/// Shared by both backends (no placeholders). The CASTs matter: Postgres
/// sums BIGINT into NUMERIC, which does not decode as i64.
pub const USER_SCAN_COSTS_SQL: &str =
    "SELECT c.user_did, u.handle,
            (SELECT f.est_cents FROM scan_costs f
              WHERE f.user_did = c.user_did AND f.kind = 'full'
              ORDER BY f.started_at ASC, f.id ASC LIMIT 1) AS first_full_scan_cents,
            CAST(COALESCE(SUM(CASE WHEN c.kind = 'refresh' THEN c.est_cents ELSE 0 END), 0) AS BIGINT) AS refresh_cents,
            CAST(COALESCE(SUM(c.est_cents), 0) AS BIGINT) AS total_cents,
            COUNT(*) AS runs,
            MAX(c.finished_at) AS last_finished_at
       FROM scan_costs c
       LEFT JOIN users u ON u.did = c.user_did
      GROUP BY c.user_did, u.handle
      ORDER BY total_cents DESC, c.user_did ASC";

pub fn list_user_scan_costs(conn: &Connection) -> Result<Vec<crate::db::traits::UserScanCost>> {
    let mut stmt = conn.prepare(USER_SCAN_COSTS_SQL)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(crate::db::traits::UserScanCost {
                user_did: r.get(0)?, handle: r.get(1)?, first_full_scan_cents: r.get(2)?,
                refresh_cents: r.get(3)?, total_cents: r.get(4)?, runs: r.get(5)?,
                last_finished_at: r.get(6)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn scan_cost_cents_since(conn: &Connection, since: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(est_cents), 0) FROM scan_costs WHERE started_at >= ?1",
        params![since],
        |r| r.get(0),
    )?)
}
```

Wrap each in `sqlite.rs` like `list_access_requests` (`let conn = self.conn.lock().await; super::queries::…(&conn, …)`). In `queries::delete_user_data`, add `tx.execute("DELETE FROM scan_costs WHERE user_did = ?1", params![user_did])?;` beside the `scan_skips` delete.

- [ ] **Step 7: Run SQLite tests, expect PASS** — `cargo test --test unit_scan_costs && cargo test --lib db::schema db::sqlite`.

- [ ] **Step 8: Postgres impls** — in `postgres.rs`:

```rust
    async fn insert_scan_cost(&self, r: &ScanCostRow) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO scan_costs (user_did, kind, outcome, started_at, finished_at,
                 classifications, classifier_cache_hits, metered_worker_secs,
                 peak_in_flight, est_cents, rate_cents_per_hour)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(&r.user_did).bind(&r.kind).bind(&r.outcome)
        .bind(&r.started_at).bind(&r.finished_at)
        .bind(r.classifications).bind(r.classifier_cache_hits)
        .bind(r.metered_worker_secs).bind(r.peak_in_flight)
        .bind(r.est_cents).bind(r.rate_cents_per_hour)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn list_user_scan_costs(&self) -> Result<Vec<UserScanCost>> {
        let rows = sqlx_core::query::query(crate::db::queries::USER_SCAN_COSTS_SQL)
            .fetch_all(&self.pool)
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| UserScanCost {
                user_did: r.get::<String, _>(0),
                handle: r.get::<Option<String>, _>(1),
                first_full_scan_cents: r.get::<Option<i64>, _>(2),
                refresh_cents: r.get::<i64, _>(3),
                total_cents: r.get::<i64, _>(4),
                runs: r.get::<i64, _>(5),
                last_finished_at: r.get::<String, _>(6),
            })
            .collect())
    }

    async fn scan_cost_cents_since(&self, since: &str) -> Result<i64> {
        let row = sqlx_core::query::query(
            "SELECT CAST(COALESCE(SUM(est_cents), 0) AS BIGINT) FROM scan_costs WHERE started_at >= $1",
        )
        .bind(since)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>(0))
    }
```

Postgres `SUM(BIGINT)` returns `NUMERIC`, which will not decode as `i64`. So in `USER_SCAN_COSTS_SQL` (Step 6) the two sums MUST be written with the portable cast both engines accept: `CAST(COALESCE(SUM(CASE WHEN c.kind = 'refresh' THEN c.est_cents ELSE 0 END), 0) AS BIGINT) AS refresh_cents` and `CAST(COALESCE(SUM(c.est_cents), 0) AS BIGINT) AS total_cents` — and likewise use `CAST(COALESCE(SUM(est_cents), 0) AS BIGINT)` in the Postgres `scan_cost_cents_since` instead of `::BIGINT`. One SQL string then serves both backends (`queries.rs` is always compiled; SQLite is the default backend). The Postgres test in Step 9 is what proves the decode. In Postgres `delete_user_data`, add `sqlx_core::query::query("DELETE FROM scan_costs WHERE user_did = $1").bind(user_did).execute(&mut *tx).await?;` beside the other per-user deletes.

- [ ] **Step 9: Postgres tests** — append to `tests/db_postgres.rs` (unique DIDs per test, as the file's convention requires):

```rust
#[tokio::test]
async fn test_pg_scan_costs_aggregate_window_and_delete() {
    let Some(url) = database_url() else { return; };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    const A: &str = "did:plc:pgtest_scancost_a";
    const B: &str = "did:plc:pgtest_scancost_b";
    for d in [A, B] { db.delete_user_data(d).await.unwrap(); }
    let row = |did: &str, kind: &str, at: &str, cents: i64| charcoal::db::ScanCostRow {
        user_did: did.into(), kind: kind.into(), outcome: "completed".into(),
        started_at: at.into(), finished_at: at.into(), classifications: 10,
        classifier_cache_hits: 2, metered_worker_secs: 1.5, peak_in_flight: 1,
        est_cents: cents, rate_cents_per_hour: 329,
    };
    db.insert_scan_cost(&row(A, "full", "2099-01-01T00:00:00Z", 150)).await.unwrap();
    db.insert_scan_cost(&row(A, "refresh", "2099-01-02T00:00:00Z", 7)).await.unwrap();
    db.insert_scan_cost(&row(B, "refresh", "2099-01-03T00:00:00Z", 3)).await.unwrap();

    let users = db.list_user_scan_costs().await.unwrap();
    let a = users.iter().find(|u| u.user_did == A).expect("A listed");
    assert_eq!((a.first_full_scan_cents, a.refresh_cents, a.total_cents, a.runs), (Some(150), 7, 157, 2));
    // 2099 dates keep this window free of other tests' rows.
    assert_eq!(db.scan_cost_cents_since("2099-01-02T00:00:00Z").await.unwrap(), 10);

    db.delete_user_data(A).await.unwrap();
    assert!(db.list_user_scan_costs().await.unwrap().iter().all(|u| u.user_did != A));
    db.delete_user_data(B).await.unwrap();
}
```

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --features postgres --test db_postgres scan_costs` → PASS. Also run the file's migration-version test if one pins 19 (`rg -n "19" tests/db_postgres.rs | rg -i version`) and bump it.

- [ ] **Step 10: Lint + commit** — `cargo clippy --features postgres --all-targets -- -D warnings && cargo clippy --features web,postgres --all-targets -- -D warnings`. `git add migrations/postgres/0020_scan_costs.sql src/db/schema.rs src/db/sqlite.rs src/db/postgres.rs src/db/traits.rs src/db/queries.rs src/db/mod.rs tests/unit_scan_costs.rs tests/db_postgres.rs` → `feat(393): scan_costs ledger table (schema v20) on both backends`.

---

### Task 4: `record_scan_cost` helper

**Files:**
- Create: `src/observability/scan_cost.rs`
- Modify: `src/observability/mod.rs` (add `pub mod scan_cost;`)
- Test: `tests/unit_scan_cost_record.rs` (create)

**Interfaces:**
- Consumes: `CostSnapshot`/`snapshot()` (Task 1), `ScanCostRow`/`insert_scan_cost` (Task 3), `CacheStats::{hits, misses}`, `crate::db::ScanKind`.
- Produces:
  ```rust
  pub fn build_scan_cost_row(user_did: &str, kind: ScanKind, completed: bool,
      started_at: &str, finished_at: &str, snap: CostSnapshot, classifier_stats: &CacheStats) -> ScanCostRow
  pub async fn record_scan_cost(db: &dyn Database, user_did: &str, kind: ScanKind, completed: bool,
      started_at: &str, meter: &ScanCostMeter, classifier_stats: &CacheStats)
  pub fn now_rfc3339() -> String
  ```

- [ ] **Step 1: Failing tests** — create `tests/unit_scan_cost_record.rs`:

```rust
//! record_scan_cost (#393): one row per run, failures recorded, never fatal.
use std::sync::Arc;

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::{Database, ScanKind};
use charcoal::observability::cache_stats::CacheStats;
use charcoal::observability::scan_cost::{build_scan_cost_row, record_scan_cost};
use charcoal::toxicity::cost_meter::ScanCostMeter;
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

#[test]
fn row_carries_meter_and_cache_counts() {
    let m = ScanCostMeter::new(500, 329);
    m.force_worker_seconds(3600.0);
    let stats = CacheStats::default();
    stats.miss(40);
    stats.hit(12);
    let r = build_scan_cost_row("did:plc:a", ScanKind::Refresh, false,
        "2026-10-24T00:00:00Z", "2026-10-24T00:01:00Z", m.snapshot(), &stats);
    assert_eq!(r.kind, "refresh");
    assert_eq!(r.outcome, "failed");
    assert_eq!((r.classifications, r.classifier_cache_hits), (40, 12));
    assert_eq!((r.est_cents, r.rate_cents_per_hour), (329, 329));
}

#[tokio::test]
async fn a_run_writes_exactly_one_row() {
    let db = setup_db();
    let m = ScanCostMeter::new(500, 329);
    record_scan_cost(db.as_ref(), "did:plc:a", ScanKind::Full, true,
        "2026-10-24T00:00:00Z", &m, &CacheStats::default()).await;
    let users = db.list_user_scan_costs().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].runs, 1);
    assert_eq!(users[0].first_full_scan_cents, Some(0), "an all-cached scan costs 0");
}

#[tokio::test]
async fn a_db_failure_is_swallowed() {
    let conn = Connection::open_in_memory().unwrap(); // no tables: insert must fail
    let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));
    let m = ScanCostMeter::new(500, 329);
    // Must return normally — the cost row is bookkeeping, never the scan's result.
    record_scan_cost(db.as_ref(), "did:plc:a", ScanKind::Full, true,
        "2026-10-24T00:00:00Z", &m, &CacheStats::default()).await;
}
```

- [ ] **Step 2: Run, expect compile failure** — `cargo test --test unit_scan_cost_record` → unresolved `scan_cost`.

- [ ] **Step 3: Implement** — `src/observability/scan_cost.rs`:

```rust
//! Per-scan GPU cost ledger (#393). Every full scan and refresh leaves one
//! `scan_costs` row, success or failure, so the cost of onboarding a user is
//! a query instead of a billing-API investigation.
//!
//! The figure is the meter's worker-seconds integral: a LOWER BOUND on the
//! RunPod bill, which also charges each worker's idle tail and cold start.

use tracing::{info, warn};

use crate::db::{Database, ScanCostRow, ScanKind};
use crate::observability::cache_stats::CacheStats;
use crate::toxicity::cost_meter::{CostSnapshot, ScanCostMeter};

/// UTC, whole seconds, `Z` — sorts lexicographically in time order, which the
/// windowed sums rely on.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn build_scan_cost_row(
    user_did: &str,
    kind: ScanKind,
    completed: bool,
    started_at: &str,
    finished_at: &str,
    snap: CostSnapshot,
    classifier_stats: &CacheStats,
) -> ScanCostRow {
    ScanCostRow {
        user_did: user_did.to_string(),
        kind: kind.as_str().to_string(),
        outcome: if completed { "completed" } else { "failed" }.to_string(),
        started_at: started_at.to_string(),
        finished_at: finished_at.to_string(),
        // Cache misses are the verdicts that actually went to the backend.
        classifications: i64::try_from(classifier_stats.misses()).unwrap_or(i64::MAX),
        classifier_cache_hits: i64::try_from(classifier_stats.hits()).unwrap_or(i64::MAX),
        metered_worker_secs: snap.worker_secs,
        peak_in_flight: i64::from(snap.peak_in_flight),
        est_cents: i64::from(snap.est_cents),
        rate_cents_per_hour: i64::from(snap.rate_cents_per_hour),
    }
}

/// Log and persist this run's cost. Best-effort: a failed write is logged
/// and swallowed, because losing a cost row must never fail the scan whose
/// results are already saved.
pub async fn record_scan_cost(
    db: &dyn Database,
    user_did: &str,
    kind: ScanKind,
    completed: bool,
    started_at: &str,
    meter: &ScanCostMeter,
    classifier_stats: &CacheStats,
) {
    let row = build_scan_cost_row(
        user_did, kind, completed, started_at, &now_rfc3339(), meter.snapshot(), classifier_stats,
    );
    info!(
        user_did = %row.user_did,
        kind = %row.kind,
        outcome = %row.outcome,
        classified = row.classifications,
        cache_hits = row.classifier_cache_hits,
        gpu_secs = format!("{:.1}", row.metered_worker_secs),
        peak = row.peak_in_flight,
        est_cents = row.est_cents,
        "scan cost (est., excludes idle time)"
    );
    if let Err(e) = db.insert_scan_cost(&row).await {
        warn!(user_did = %row.user_did, error = %format!("{e:#}"), "failed to record scan cost");
    }
}
```

Confirm `ScanKind` is reachable as `crate::db::ScanKind` (the scan job already uses that path); if not, use `crate::db::traits::ScanKind`.

- [ ] **Step 4: Run, expect PASS** — `cargo test --test unit_scan_cost_record`.
- [ ] **Step 5: Commit** — `git add src/observability/scan_cost.rs src/observability/mod.rs tests/unit_scan_cost_record.rs` → `feat(393): record_scan_cost helper, best-effort`.

---

### Task 5: Wire the full scan and the refresh

**Files:**
- Modify: `src/web/scan_job.rs:1365-1878` (`run_scan_inner`)
- Modify: `src/web/refresh_scan.rs:998-1066` (refresh `pipeline` closure)

**Interfaces:**
- Consumes: `ScanScorers.cost_meter`, `ScanScorers.classifier_stats`, `record_scan_cost`, `now_rfc3339` (Tasks 2, 4).

The wiring must record on BOTH outcomes, including the Stage-2 identity probe failing (the 2026-10-02 incident class — a refusal that still woke a GPU). The scan body has many `?` exits, so the recording goes in a wrapper around the body rather than at each exit.

- [ ] **Step 1: Full scan — split the function.** Rename the existing `async fn run_scan_inner(...)` to `async fn run_scan_body(...)` and give it one extra trailing parameter `scorers: &ScanScorers`. Delete from it everything up to and including `let scorers = build_scan_scorers(&models, &db)?;` (lines ~1375–1390: the LoadingModels `set_progress`, the cache-eviction call, and the build line). Its body now starts at the `probe_classifier_identity` comment. Then add above it:

```rust
async fn run_scan_inner(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    models: Arc<ScanModels>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: &str,
    actor_handle: &str,
    claim_id: &str,
) -> anyhow::Result<FullScanRun> {
    // <moved verbatim: the LoadingModels set_progress call and the
    //  evict_stale_cache_best_effort call with their comments>

    let scorers = build_scan_scorers(&models, &db)?;

    // #393: the meter starts here, so even a scan refused by the Stage-2
    // identity probe below leaves a cost row — that probe can wake a GPU.
    let started_at = crate::observability::scan_cost::now_rfc3339();
    let run = run_scan_body(
        config,
        Arc::clone(&db),
        models,
        scan_manager,
        user_did,
        actor_handle,
        claim_id,
        &scorers,
    )
    .await;
    crate::observability::scan_cost::record_scan_cost(
        db.as_ref(),
        user_did,
        crate::db::ScanKind::Full,
        run.is_ok(),
        &started_at,
        &scorers.cost_meter,
        &scorers.classifier_stats,
    )
    .await;
    run
}
```

Copy the moved statements exactly (do not retype them). If clippy flags `too_many_arguments` on `run_scan_body`, add `#[allow(clippy::too_many_arguments)]` with a one-line why-comment only if the codebase already does this elsewhere (`rg -n "too_many_arguments" src`); otherwise bundle nothing — ask the controller.

- [ ] **Step 2: Refresh — wrap the pipeline body.** In `refresh_scan.rs`, change the pipeline closure to:

```rust
        let pipeline = move |plan: RefreshPlan| async move {
            // #393: record this refresh's GPU cost whether it scores or fails.
            let started_at = crate::observability::scan_cost::now_rfc3339();
            let result: anyhow::Result<_> = async {
                // <existing body, lines 999–1065, moved verbatim, ending in Ok(summary)>
            }
            .await;
            crate::observability::scan_cost::record_scan_cost(
                db.as_ref(),
                &uid,
                crate::db::ScanKind::Refresh,
                result.is_ok(),
                &started_at,
                &scorers.cost_meter,
                &scorers.classifier_stats,
            )
            .await;
            result
        };
```

If the inner block's borrows of `db`/`uid`/`scorers` conflict, make it `async { … }` (borrowing, not `move`) — it is awaited before the outer block uses them again, so borrows suffice. If the error type cannot be inferred, end the inner block with `Ok::<_, anyhow::Error>(summary)`.

- [ ] **Step 3: Compile + existing tests stay green** — `cargo fmt && cargo clippy --features web --all-targets -- -D warnings`, then `CHARCOAL_MODEL_DIR=./models cargo test --features web --lib web::refresh_scan web::scan_job` (these in-module tests exercise both paths with fakes). Expect PASS with no behaviour change.

- [ ] **Step 4: Commit** — `git add src/web/scan_job.rs src/web/refresh_scan.rs` → `feat(393): every full scan and refresh records its GPU cost`.

(End-to-end proof that a real run writes a row is Task 8's staging check: the scan body needs models and live Bluesky, which unit tests here fake out.)

---

### Task 6: `GET /api/admin/costs`

**Files:**
- Modify: `src/web/handlers/admin.rs` (new handler, after `list_users` ~84–153)
- Modify: `src/web/mod.rs:223-251` (route)
- Test: `tests/web_admin_costs.rs` (create)

**Interfaces:**
- Consumes: `list_user_scan_costs`, `scan_cost_cents_since` (Task 3).
- Produces JSON:
  ```json
  { "last_7d_cents": 0, "last_30d_cents": 0, "onboarding_target_cents": 200,
    "users": [ { "user_did": "", "handle": null, "first_full_scan_cents": null,
                 "refresh_cents": 0, "total_cents": 0, "runs": 0,
                 "last_finished_at": "", "over_onboarding_target": false } ] }
  ```

- [ ] **Step 1: Failing tests** — create `tests/web_admin_costs.rs`, copying the `session_cookie` and `call` helpers verbatim from `tests/web_access_admin.rs:17-41` and its imports/`MODELS_REQUIRED` const:

```rust
#[tokio::test]
async fn non_admin_gets_403() {
    let (app, _db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    let (status, _) = call(&app, "GET", "/api/admin/costs", "did:plc:notanadmin000000000000").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn reports_per_user_totals_windows_and_the_onboarding_flag() {
    let (app, db) = build_admin_test_app_with_db().expect(MODELS_REQUIRED);
    let now = chrono::Utc::now();
    let at = |days: i64| (now - chrono::Duration::days(days))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let row = |did: &str, kind: &str, started: String, cents: i64| charcoal::db::ScanCostRow {
        user_did: did.into(), kind: kind.into(), outcome: "completed".into(),
        finished_at: started.clone(), started_at: started, classifications: 1,
        classifier_cache_hits: 0, metered_worker_secs: 1.0, peak_in_flight: 1,
        est_cents: cents, rate_cents_per_hour: 329,
    };
    db.insert_scan_cost(&row("did:plc:big", "full", at(20), 250)).await.unwrap();
    db.insert_scan_cost(&row("did:plc:small", "full", at(1), 90)).await.unwrap();
    db.insert_scan_cost(&row("did:plc:small", "refresh", at(1), 4)).await.unwrap();

    let (status, body) = call(&app, "GET", "/api/admin/costs", TEST_DID).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["last_7d_cents"], 94);
    assert_eq!(body["last_30d_cents"], 344);
    assert_eq!(body["onboarding_target_cents"], 200);
    assert_eq!(body["users"][0]["user_did"], "did:plc:big");
    assert_eq!(body["users"][0]["over_onboarding_target"], true);
    assert_eq!(body["users"][1]["over_onboarding_target"], false);
}
```

- [ ] **Step 2: Run, expect FAIL** — `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_admin_costs` → 404 instead of 403/200.

- [ ] **Step 3: Implement** — in `admin.rs`:

```rust
/// First-scan cost above which an onboarding is flagged (#385: Bryan's
/// target is under $2.00 per new user).
const ONBOARDING_TARGET_CENTS: i64 = 200;

/// GET /api/admin/costs — estimated GPU spend (#393). Figures are the
/// in-app meter's lower bound; RunPod also bills idle tails.
pub async fn scan_costs(
    Extension(auth): Extension<AuthUser>,
    State(state): State<AppState>,
) -> Result<impl IntoResponse, StatusCode> {
    if !auth.is_admin {
        return Err(StatusCode::FORBIDDEN);
    }
    let db_error = |e: anyhow::Error| {
        tracing::error!(error = %format!("{e:#}"), "failed to read scan costs");
        StatusCode::INTERNAL_SERVER_ERROR
    };
    let since = |days: i64| {
        (chrono::Utc::now() - chrono::Duration::days(days))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    };
    let last_7d = state.db.scan_cost_cents_since(&since(7)).await.map_err(db_error)?;
    let last_30d = state.db.scan_cost_cents_since(&since(30)).await.map_err(db_error)?;
    let users: Vec<serde_json::Value> = state
        .db
        .list_user_scan_costs()
        .await
        .map_err(db_error)?
        .into_iter()
        .map(|u| {
            let over = u.first_full_scan_cents.is_some_and(|c| c > ONBOARDING_TARGET_CENTS);
            let mut v = serde_json::to_value(&u).unwrap_or_default();
            v["over_onboarding_target"] = serde_json::Value::Bool(over);
            v
        })
        .collect();
    Ok(Json(serde_json::json!({
        "last_7d_cents": last_7d,
        "last_30d_cents": last_30d,
        "onboarding_target_cents": ONBOARDING_TARGET_CENTS,
        "users": users,
    })))
}
```

(If `db_error` is used by value three times, make it `fn` or clone the closure — closures without captures are `Copy`, so this compiles.) In `mod.rs` beside the other admin routes: `.route("/api/admin/costs", get(handlers::admin::scan_costs))`.

- [ ] **Step 4: Run, expect PASS** — Step 2 command.
- [ ] **Step 5: Commit** — `git add src/web/handlers/admin.rs src/web/mod.rs tests/web_admin_costs.rs` → `feat(393): admin endpoint for estimated GPU spend`.

---

### Task 7: Admin page "GPU cost" section

**Files:**
- Create: `web/src/lib/scan-costs.ts`, `web/src/lib/scan-costs.test.ts`
- Modify: `web/src/lib/types.ts` (after `AccessListResponse` ~255), `web/src/lib/api.ts` (after `getAccessRequests` ~286)
- Modify: `web/src/routes/(protected)/admin/+page.svelte` (state ~33, loader near `loadAccess` ~244, `onMount` ~378, new section before `access-section` ~452)

**Interfaces:**
- Consumes: Task 6 JSON.
- Produces: `formatCents(cents: number | null): string`, types `UserScanCost`, `ScanCostsResponse`, `getScanCosts()`.

- [ ] **Step 1: Failing vitest** — `web/src/lib/scan-costs.test.ts`:

```ts
import { describe, expect, it } from 'vitest';
import { formatCents } from './scan-costs';

describe('formatCents', () => {
	it('renders dollars with two decimals', () => {
		expect(formatCents(0)).toBe('$0.00');
		expect(formatCents(7)).toBe('$0.07');
		expect(formatCents(250)).toBe('$2.50');
	});
	it('renders a missing first scan as a dash', () => {
		expect(formatCents(null)).toBe('—');
	});
});
```

Run `npm --prefix web run test -- scan-costs` → FAIL (module not found).

- [ ] **Step 2: Implement** — `web/src/lib/scan-costs.ts`:

```ts
/** Cents → "$d.cc". `null` (no full scan yet) → an em dash. */
export function formatCents(cents: number | null): string {
	if (cents === null) return '—';
	return `$${(cents / 100).toFixed(2)}`;
}
```

`types.ts`:

```ts
export interface UserScanCost {
	user_did: string;
	handle: string | null;
	first_full_scan_cents: number | null;
	refresh_cents: number;
	total_cents: number;
	runs: number;
	last_finished_at: string;
	over_onboarding_target: boolean;
}
export interface ScanCostsResponse {
	last_7d_cents: number;
	last_30d_cents: number;
	onboarding_target_cents: number;
	users: UserScanCost[];
}
```

`api.ts`:

```ts
export async function getScanCosts(): Promise<ScanCostsResponse> {
	return apiFetch<ScanCostsResponse>('/api/admin/costs');
}
```

Run vitest → PASS.

- [ ] **Step 3: Page section** — invoke the svelte skill first. Add imports (`getScanCosts`, `formatCents`, `ScanCostsResponse`), state `let costs = $state<ScanCostsResponse | null>(null); let costsError = $state(false);`, a loader mirroring `loadAccess` (on error: `if (await routeAuthFailure(err)) return; costsError = true;`), call `loadCosts()` in `onMount` (not in polling — cost changes slowly). Section, matching the existing `queue-section` markup and reusing existing classes (`section-title`, `loading-state`, `spinner`):

```svelte
<section class="costs-section">
	<h2 class="section-title">GPU cost <span class="costs-note">est., excludes idle time</span></h2>
	{#if costs}
		<p class="costs-totals">
			Last 7 days <strong>{formatCents(costs.last_7d_cents)}</strong> ·
			last 30 days <strong>{formatCents(costs.last_30d_cents)}</strong>
		</p>
		{#if costs.users.length === 0}
			<p class="queue-idle">No scans recorded yet.</p>
		{:else}
			<table class="costs-table">
				<thead>
					<tr><th>User</th><th>First scan</th><th>Refreshes</th><th>Total</th><th>Runs</th></tr>
				</thead>
				<tbody>
					{#each costs.users as u (u.user_did)}
						<tr class:over-target={u.over_onboarding_target}>
							<td>{u.handle ?? u.user_did}</td>
							<td>
								{formatCents(u.first_full_scan_cents)}
								{#if u.over_onboarding_target}
									<span class="over-flag">over {formatCents(costs.onboarding_target_cents)}</span>
								{/if}
							</td>
							<td>{formatCents(u.refresh_cents)}</td>
							<td>{formatCents(u.total_cents)}</td>
							<td>{u.runs}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		{/if}
	{:else if costsError}
		<p class="queue-idle">Could not load GPU costs.</p>
	{:else}
		<div class="loading-state"><div class="spinner"></div></div>
	{/if}
</section>
```

Styles: add scoped rules for `.costs-note` (muted, smaller), `.costs-table` (full width, left-aligned, tabular numbers, horizontal scroll wrapper not needed at 5 columns but the table must not overflow at phone width — use `font-size` step-down under the page's existing mobile breakpoint), `.over-flag` (the page's existing warning/error colour token — reuse one already in the file, do not invent a colour). Run `mcp__svelte__svelte-autofixer` on the file until clean, then `npm --prefix web run build` and `npm --prefix web run check` if that script exists.

- [ ] **Step 4: Commit** — `git add web/src/lib/scan-costs.ts web/src/lib/scan-costs.test.ts web/src/lib/types.ts web/src/lib/api.ts "web/src/routes/(protected)/admin/+page.svelte"` → `feat(393): GPU cost section on the admin page`.

---

### Task 8: Docs, changelog, staging verification

**Files:**
- Modify: `CHANGELOG.md` (`## [Unreleased]` → `### Added`)
- Modify: `docs/superpowers/specs/2026-10-24-393-scan-cost-ledger-design.md` (kind values `'full'`/`'refresh'`; scan-path tests → staging check)

- [ ] **Step 1: CHANGELOG** — under `### Added`:

```markdown
- #393 — every full scan and nightly refresh records its estimated GPU cost
  in a new `scan_costs` table (schema v20), success or failure, and logs a
  one-line summary. The admin page shows per-user first-scan, refresh and
  total cost plus 7- and 30-day totals, and flags any first scan over the
  $2.00 onboarding target. The figure is the app's own meter — a lower bound
  that excludes the idle time RunPod bills per worker — and is labelled so.
  A scan refused by the Stage-2 identity probe still records what it spent.
```

- [ ] **Step 2: Spec amendments** — change `'scan'` to `'full'` wherever the spec names a kind; replace the "Scan paths" test bullet with: "Scan-path wiring is verified on staging (Task 8): the scan body needs live Bluesky and models, which the unit suites fake."
- [ ] **Step 3: Commit** — `git add CHANGELOG.md docs/superpowers/specs/2026-10-24-393-scan-cost-ledger-design.md` → `docs(393): changelog and spec amendments`.
- [ ] **Step 4: Review, push, PR** — internal review of the full diff, then Codex review against `origin/staging`, triage each finding, then push (`git -c core.sshCommand="ssh -o ServerAliveInterval=15 -o ServerAliveCountMax=80" push`, confirm with `git ls-remote`), open the PR to `staging`, and loop until CodeRabbit approves.
- [ ] **Step 5: Staging verification (after merge to staging)** — trigger one scan for a staging user from the admin page, then:
  `railway ssh --service Postgres --environment staging -- psql -h 127.0.0.1 -U postgres -d railway -c "select kind, outcome, classifications, round(metered_worker_secs::numeric,1), peak_in_flight, est_cents from scan_costs order by id desc limit 3"` → one new row with `kind=full`. Confirm the Railway log shows `scan cost (est., excludes idle time)`, and `/admin` renders the section. Compare `est_cents` with that hour's RunPod billing bucket (`mcp__runpod__list-serverless-billing`, `bucketSize=hour`) and record the ratio in a #393 comment — it seeds the Part 3 calibration.
