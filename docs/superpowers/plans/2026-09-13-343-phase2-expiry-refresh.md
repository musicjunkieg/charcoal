# #343 Phase 2 — Score Expiry and Nightly Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Every stored score carries a generation stamp and an expiry; tier lists show only current, unexpired rows; and a nightly refresh job, driven from the existing admitter tick, re-scores the High/Elevated set before it expires — closing #344 and giving #342 the scheduler seam it needs.

**Architecture:** Migration v18 adds `scoring_generation` + `valid_until` to `account_scores`, `kind` to `scan_queue`, and `next_refresh_at` to `users`. The write path stamps both new score columns from a build-time `SCORING_GENERATION` constant and `ScoringConfidence::staleness_days()` (3/7/14 d, finally wired). Every read that feeds a tier list, count, or the pipeline's "already scored" gate uses one **fresh** predicate: `valid_until > now AND scoring_generation = current`. The refresh is a second queue kind (`refresh`) that reuses the admitter, the claim/lease/fencing machinery and `run_phased_scan` unchanged; its candidates come **from `account_scores`** (High/Elevated rows expiring within 2 days or stamped with an old generation), not from Constellation. No second loop, no second lock.

**Tech Stack:** Rust 2021, tokio, async-trait, rusqlite 0.38 (SQLite) + sqlx-core/sqlx-postgres (Postgres), chrono, serde_json, tracing; SvelteKit 5 + vitest for the two-line frontend change.

**Spec:** `docs/superpowers/specs/2026-09-08-343-scalability-onboarding-design.md` §4.4 (S4), §6 Phase 2, §7. Deciduous decision **777** is the policy; parent action node for this plan is **872**.

## Global Constraints

- **Branch/PR:** work on `feat/343-phase2-expiry-refresh` off `staging` (`git checkout staging && git pull --no-rebase origin staging && git checkout -b feat/343-phase2-expiry-refresh`); PR to `staging`; done only at **CodeRabbit APPROVED**. CodeRabbit allows **5 reviews/hour — if it says wait, wait.** Batch fixes into fewer pushes.
- **Chainlink issue before code** (hook-enforced): `chainlink session work 344` — #344 is the issue this phase closes. Close with `chainlink issue close 344 --no-changelog`; handwrite the CHANGELOG entry (Task 10).
- **Git:** explicit `git add <paths>`; no `git add -A`; no heredocs anywhere; single-quoted multi-line commit messages ending with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX`. `git merge/rebase/cherry-pick/reset/tag/branch -D` are blocked. Push in the background: `git -c credential.helper='!gh auth git-credential' push https://github.com/musicjunkieg/charcoal.git feat/343-phase2-expiry-refresh`.
- **Deciduous:** log `action` (`--commit HEAD`) before and `outcome` after each task; link each action from **872** immediately. Verify node IDs with `deciduous nodes | tail` before every link — subagent implementers add their own nodes, so IDs are not sequential from yours.
- **TDD:** failing test first, then code (spec §7). Unit tests: `CHARCOAL_MODEL_DIR=./models cargo test --features web`; model-gated tests must show **zero** lines matching `^\s*SKIP:` under `-- --show-output` (no `-i`). Postgres: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres` (`createdb charcoal_test` once if missing). Frontend: `npm --prefix web run test` and `npm --prefix web run build` (never `cd web`).
- **Migrations** get a fresh-DB test and an upgrade-from-v17 test on **both** backends. Postgres migrations **self-record** their version (`INSERT INTO schema_version (version) VALUES (18) ON CONFLICT DO NOTHING;`). The spec text says "v17"; v17 shipped in Phase 1 (cache indexes), so this is **v18** — Task 10 amends the spec.
- **Every new env knob has a clamp/default test.** Knobs in this plan: `CHARCOAL_REFRESH_INTERVAL_HOURS` (default 24, `0`/`off` disables, otherwise clamp 1..=168).
- **The fresh predicate is the single source of truth.** SQLite: `valid_until IS NOT NULL AND datetime(valid_until) > datetime('now') AND scoring_generation = ?`. Postgres: `valid_until > NOW() AND scoring_generation = $n`. Every query in Task 3 and Task 7 uses exactly this shape; `tests/unit_staleness.rs` pins `get_fresh_scored_dids` ≡ `!is_score_stale`.
- **Freshness reads are hard errors** (spec §4.4): the two `.unwrap_or_default()` calls become `?`. A scan that cannot read freshness does not run.
- **Nothing is deleted.** Expired and legacy rows stay in `account_scores`; they are hidden and counted as `expired`. Re-entry is by re-engagement (a full scan) or by the refresh job (High/Elevated only — decision 777).
- **Queue order stays FIFO across kinds.** `claim_next_scan`, `list_scan_queue` position and `scan_queue_entry` all order by `(enqueued_at, user_did)` (#271 — one total order); this plan does **not** reorder by kind. Refresh rows are small (tens of accounts) so a human enqueued behind the nightly wave waits minutes, not hours.
- **A refresh never rebuilds the fingerprint** and never touches Constellation. Fingerprint age is a full-scan concern (#342's fortnightly full scan stays out of scope — Task 10 records this).
- **SQLite/Postgres divergence, deliberate:** Postgres `valid_until` is `NOT NULL` after backfill; SQLite cannot add `NOT NULL` to an existing column without a table rebuild, so it stays nullable and the fresh predicate treats `NULL` as expired (fail closed). Every writer supplies it.
- **Privacy:** never log tokens, DPoP proofs, request bodies, `CHARCOAL_TOKEN_KEY`, `SOOT_TOKEN`; never log post text; strip credentials from any `DATABASE_URL` printed.
- **Pass numbers (spec §6 Phase 2):** bump the generation on staging → every row expired and hidden (`tier_counts.expired` = row count, tiers 0); the nightly refresh re-scores exactly the High/Elevated set; no `legacy` row in any tier list after one nightly; `refresh_feed_cache_hits / (hits+misses)` ≥ 80 % on the refresh set (< 50 % ⇒ TTL/cadence misaligned — tune that first).
- **Clippy clean** on `--features web`, `--features postgres`, and no features. CI clippy (1.98) sees lints local 1.94 does not; fix rather than allow, except the existing `result_large_err` allows on async handlers.
- Comments explain **why**; `?` for errors; `anyhow::Result` at application level.

---

## File map

| Path | Responsibility |
|---|---|
| `src/scoring/generation.rs` (new) | Task 1: `SCORING_GENERATION`, `LEGACY_GENERATION` constants + the bump rule. |
| `src/scoring/mod.rs` | Task 1: `pub mod generation;` |
| `src/db/models.rs` | Task 1: `ScoringConfidence::from_label`, `staleness_days_for_label`. Task 5: `ScanKind` lives in traits.rs (see there). |
| `src/db/schema.rs` | Task 2: migration v18 (SQLite) + tests; bump the `(1..=17)` assertions to 18. |
| `migrations/postgres/0018_score_expiry.sql` (new) | Task 2: migration v18 (Postgres, self-recording). |
| `src/db/postgres.rs` | Task 2: register 18. Task 3/5/6/7: Postgres implementations. |
| `src/db/traits.rs` | Task 3: `is_score_stale`/`get_fresh_scored_dids` lose `max_age_days`; `count_expired`. Task 5: `ScanKind`, `ScanClaim.kind`, `ScanQueueRow.kind`, `enqueue_refresh_scan`. Task 6: `schedule_refresh`, `claim_due_refresh_users`. Task 7: `RefreshCandidate`, `list_refresh_candidates`. |
| `src/db/queries.rs` + `src/db/sqlite.rs` | Tasks 3/5/6/7: SQLite implementations. |
| `src/db/mod.rs` | Task 5: re-export `ScanKind`; Task 7: re-export `RefreshCandidate`. |
| `src/pipeline/sweep.rs:132-140`, `src/pipeline/amplification.rs:309-318` | Task 3: hard-error freshness read, no `7`. Task 8: `direct_pairs_for` extracted. |
| `src/web/handlers/status.rs` | Task 4: `tier_counts.expired`. |
| `src/status.rs` | Task 4: CLI prints the expired count. |
| `web/src/lib/types.ts`, `web/src/lib/dashboard-state.ts` (+ `.test.ts`), `web/src/routes/(protected)/dashboard/+page.svelte` | Task 4: `expired` in `TierCounts`, dashboard card, results gating. Task 5: `kind` on the admin queue row type. |
| `src/web/handlers/scan.rs` | Task 5: cooldown falls back to `scan_state.last_full_scan_finished_at` when the queue row is a refresh. |
| `src/web/handlers/admin.rs` | Task 5: `kind` in `scan_row_json`. |
| `src/web/refresh.rs` (new) | Task 6: `CHARCOAL_REFRESH_INTERVAL_HOURS` parsing + `enqueue_due_refreshes`. |
| `src/web/admitter.rs` | Task 6: `run_admitter` gains `refresh: Option<Duration>`; tick calls `enqueue_due_refreshes`. Task 9: launcher dispatches on `claim.kind`. |
| `src/web/scan_job.rs` | Task 5: write `last_full_scan_finished_at`. Task 6: `schedule_refresh` after a successful full scan. Task 8: extract `ScanScorers`/`build_scan_scorers`/`record_scan_cache_stats`/`embed_protected_posts`/`pile_on_dids`. Task 9: `launch_scan` takes `ScanKind`. |
| `src/web/refresh_scan.rs` (new) | Task 9: `run_refresh`, `refresh_is_deferred`, `to_candidate`. |
| `src/web/mod.rs` | Task 6: `pub mod refresh;` Task 9: `pub mod refresh_scan;` |
| `tests/unit_scoring.rs`, `tests/unit_staleness.rs`, `tests/db_postgres.rs`, `tests/unit_scan_kind.rs` (new), `tests/unit_refresh_candidates.rs` (new), `tests/web_oauth.rs` | Tests per task. |
| `CHANGELOG.md`, `README.md`, `docs/runbooks/343-phase2-expiry-refresh.md` (new), spec §4.4/§6 | Task 10. |

---

### Task 1: The generation constant and the confidence → staleness mapping

**Why:** Spec §4.4: "the current generation is a build-time constant bumped whenever the formula, the models or the policy change." And `ScoringConfidence::staleness_days()` (`src/db/models.rs:63`) has one caller, a test — this task gives it the parsing helper the write path (Task 3) needs, because `AccountScore.scoring_confidence` is stored as `Option<String>` (`"low" | "standard" | "high"`, set in `src/scoring/profile.rs:420,951`; `None` on the not-assessed and insufficient-data paths at `:76,:315`).

**Files:**
- Create: `src/scoring/generation.rs`
- Modify: `src/scoring/mod.rs` (add `pub mod generation;` after `pub mod context;`)
- Modify: `src/db/models.rs` (inside `impl ScoringConfidence`, after `staleness_days`)
- Test: `tests/unit_scoring.rs` (append)

**Interfaces:**
- Produces: `charcoal::scoring::generation::SCORING_GENERATION: &str`, `LEGACY_GENERATION: &str`; `ScoringConfidence::from_label(&str) -> Option<ScoringConfidence>`; `ScoringConfidence::staleness_days_for_label(Option<&str>) -> i64`. Tasks 3 and 7 bind `SCORING_GENERATION` into SQL; Task 3 calls `staleness_days_for_label` in both upserts.

- [ ] **Step 1: Write the failing tests**

Append to `tests/unit_scoring.rs`:

```rust
// --- #343 Phase 2 / #344: generation stamp + confidence label parsing ---

#[test]
fn scoring_generation_is_a_real_stamp_not_legacy() {
    use charcoal::scoring::generation::{LEGACY_GENERATION, SCORING_GENERATION};
    assert!(!SCORING_GENERATION.is_empty());
    assert_ne!(SCORING_GENERATION, LEGACY_GENERATION);
    assert_eq!(LEGACY_GENERATION, "legacy", "migration v18 backfills this literal");
    // No whitespace: the value is bound into SQL equality and shown in the UI.
    assert!(!SCORING_GENERATION.contains(char::is_whitespace));
}

#[test]
fn scoring_confidence_round_trips_through_its_label() {
    for c in [
        ScoringConfidence::Low,
        ScoringConfidence::Standard,
        ScoringConfidence::High,
    ] {
        assert_eq!(ScoringConfidence::from_label(c.as_str()), Some(c));
    }
    assert_eq!(ScoringConfidence::from_label("bogus"), None);
    assert_eq!(ScoringConfidence::from_label(""), None);
}

/// The write path stamps `valid_until` from the stored label. Rows with no
/// label (NotAssessed, insufficient data) and rows with an unknown label
/// get the Standard 7-day window — never 0, never forever.
#[test]
fn staleness_days_for_label_falls_back_to_standard() {
    assert_eq!(ScoringConfidence::staleness_days_for_label(Some("low")), 3);
    assert_eq!(ScoringConfidence::staleness_days_for_label(Some("standard")), 7);
    assert_eq!(ScoringConfidence::staleness_days_for_label(Some("high")), 14);
    assert_eq!(ScoringConfidence::staleness_days_for_label(None), 7);
    assert_eq!(ScoringConfidence::staleness_days_for_label(Some("bogus")), 7);
}
```

Check the top of `tests/unit_scoring.rs`: `ScoringConfidence` is already imported for the existing `scoring_confidence_staleness_days` test at line ~450; add `use charcoal::db::models::ScoringConfidence;` only if it is missing.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test unit_scoring scoring_generation scoring_confidence_round staleness_days_for_label`
Expected: compile error — `generation` module and `from_label` do not exist.

- [ ] **Step 3: Write the implementation**

Create `src/scoring/generation.rs`:

```rust
//! The scoring generation stamp (#343 §4.4, #344).
//!
//! Every `account_scores` row records the generation it was scored under.
//! A row is *fresh* only if its generation equals [`SCORING_GENERATION`]
//! **and** its `valid_until` is in the future; anything else is hidden from
//! tier lists and re-scored by the refresh job (High/Elevated) or on the
//! account's next re-engagement (everything else).
//!
//! # When to bump
//!
//! Change the value whenever a stored score would no longer be comparable
//! to a freshly computed one:
//! - the threat formula or its weights (`src/scoring/threat.rs`)
//! - the topic-overlap math or fingerprint format (`src/topics/`, #297/#302)
//! - a toxicity model (`src/toxicity/onnx.rs`), the NLI model, or the
//!   embedding model
//! - the classifier policy on Zentropi / CoPE-B
//!
//! Bumping is deliberately a code change, not a config knob: a generation
//! that differs between two replicas would make them hide each other's
//! scores. The value is opaque; the date form is only for humans reading
//! `SELECT scoring_generation, COUNT(*) …`.
pub const SCORING_GENERATION: &str = "2026-09-13";

/// The stamp migration v18 writes onto rows scored before generations
/// existed. Never equal to [`SCORING_GENERATION`], so every pre-v18 row is
/// expired the moment the migration runs and the refresh job re-scores the
/// High/Elevated ones on its first tick (spec §4.4 "Migration").
pub const LEGACY_GENERATION: &str = "legacy";
```

Add `pub mod generation;` to `src/scoring/mod.rs` (alphabetical, after `context`).

In `src/db/models.rs`, inside `impl ScoringConfidence`, after `staleness_days`:

```rust
    /// Inverse of [`as_str`](Self::as_str). `None` for anything the enum
    /// never produced — callers decide the fallback; see
    /// [`staleness_days_for_label`](Self::staleness_days_for_label).
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "low" => Some(ScoringConfidence::Low),
            "standard" => Some(ScoringConfidence::Standard),
            "high" => Some(ScoringConfidence::High),
            _ => None,
        }
    }

    /// Staleness window for a stored `scoring_confidence` label (#344).
    ///
    /// `AccountScore.scoring_confidence` is `Option<String>`: `None` on the
    /// NotAssessed and insufficient-data paths, and a label the write path
    /// never validated otherwise. Both fall back to Standard (7 days) — long
    /// enough not to churn, short enough that a wrong label is not forever.
    pub fn staleness_days_for_label(label: Option<&str>) -> i64 {
        label
            .and_then(Self::from_label)
            .unwrap_or(ScoringConfidence::Standard)
            .staleness_days()
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test unit_scoring`
Expected: all pass, including the three new tests.

- [ ] **Step 5: Commit**

```bash
git add src/scoring/generation.rs src/scoring/mod.rs src/db/models.rs tests/unit_scoring.rs
git commit -m 'feat(344): SCORING_GENERATION stamp + ScoringConfidence label parsing

The generation constant every score row will carry (#343 §4.4), and the
inverse of ScoringConfidence::as_str so the write path can turn the stored
label back into a staleness window. No schema or behaviour change yet.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 2: Migration v18 — expiry columns, queue kind, refresh schedule

**Why:** Spec §4.4: `account_scores` gains `scoring_generation` + `valid_until` (backfilled to `scored_at + 14 d` / `'legacy'`), `scan_queue.kind`, `users.next_refresh_at`. The backfill sets `next_refresh_at = now` for every user who already has scores so the **first** tick after deploy refreshes their legacy High/Elevated rows instead of leaving the list blank for a day.

**Files:**
- Modify: `src/db/schema.rs` (after the v17 `run_migration` block, before `Ok(())`; the two `(1..=17)` assertions at ~`:717` and ~`:832`; tests module)
- Create: `migrations/postgres/0018_score_expiry.sql`
- Modify: `src/db/postgres.rs:183-187` (register 18 after 17)
- Test: `src/db/schema.rs` tests module; `tests/db_postgres.rs` (append)

**Interfaces:**
- Produces: columns `account_scores.scoring_generation TEXT NOT NULL` (`DEFAULT 'legacy'` on SQLite; default dropped on Postgres after backfill), `account_scores.valid_until` (`TEXT` nullable on SQLite, `TIMESTAMPTZ NOT NULL` on Postgres), index `idx_account_scores_user_tier_valid (user_did, threat_tier, valid_until)`, `scan_queue.kind TEXT NOT NULL DEFAULT 'full'` (Postgres: `CHECK (kind IN ('full','refresh'))`), `users.next_refresh_at` (`TEXT` RFC3339 on SQLite, `TIMESTAMPTZ` on Postgres). Tasks 3, 5, 6, 7 read/write these.
- Timestamp conventions: `account_scores.valid_until` follows `scored_at` (`datetime('now')` format `YYYY-MM-DD HH:MM:SS` on SQLite, `TIMESTAMPTZ` on Postgres). `users.next_refresh_at` follows `scan_queue` (RFC3339 text on SQLite, written by `DateTime<Utc>::to_rfc3339()`; `TIMESTAMPTZ` on Postgres).

- [ ] **Step 1: Write the failing SQLite tests**

In the `#[cfg(test)] mod tests` of `src/db/schema.rs`, after `test_migration_v17_upgrades_a_v16_database`:

```rust
    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// v18 (#344): expiry columns on account_scores, kind on scan_queue,
    /// next_refresh_at on users, and the refresh-candidate index.
    #[test]
    fn test_migration_v18_adds_expiry_and_refresh_columns() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();

        let scores = column_names(&conn, "account_scores");
        assert!(scores.iter().any(|c| c == "scoring_generation"));
        assert!(scores.iter().any(|c| c == "valid_until"));
        assert!(column_names(&conn, "scan_queue").iter().any(|c| c == "kind"));
        assert!(column_names(&conn, "users")
            .iter()
            .any(|c| c == "next_refresh_at"));

        let has_index: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_account_scores_user_tier_valid'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(has_index);

        let recorded: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM schema_version WHERE version = 18",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(recorded);
    }

    /// A v17 database with existing scores and users: the upgrade must stamp
    /// every score 'legacy' with valid_until = scored_at + 14 d, default every
    /// queue row to 'full', and schedule a first refresh for every user who
    /// has scores — and only those users.
    #[test]
    fn test_migration_v18_upgrades_a_v17_database() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        // Back to the v17 shape. Index first: SQLite refuses to drop a column
        // an index still references.
        conn.execute_batch(
            "DROP INDEX idx_account_scores_user_tier_valid;
             ALTER TABLE account_scores DROP COLUMN scoring_generation;
             ALTER TABLE account_scores DROP COLUMN valid_until;
             ALTER TABLE scan_queue DROP COLUMN kind;
             ALTER TABLE users DROP COLUMN next_refresh_at;
             DELETE FROM schema_version WHERE version = 18;",
        )
        .unwrap();
        // Pre-existing rows, as a v17 deployment would hold them.
        conn.execute_batch(
            "INSERT INTO users (did, handle) VALUES ('did:plc:scored', 'scored.test');
             INSERT INTO users (did, handle) VALUES ('did:plc:unscored', 'unscored.test');
             INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at)
                 VALUES ('did:plc:scored', 'did:plc:acct', 'acct.test', 40.0, 'High', '2026-09-01 12:00:00');
             INSERT INTO scan_queue (user_did, status, enqueued_at)
                 VALUES ('did:plc:scored', 'done', '2026-09-01T12:00:00+00:00');",
        )
        .unwrap();

        create_tables(&conn).unwrap();

        let (generation, valid_until): (String, String) = conn
            .query_row(
                "SELECT scoring_generation, valid_until FROM account_scores WHERE did = 'did:plc:acct'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(generation, "legacy");
        assert_eq!(valid_until, "2026-09-15 12:00:00", "scored_at + 14 days");

        let kind: String = conn
            .query_row("SELECT kind FROM scan_queue WHERE user_did = 'did:plc:scored'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "full");

        let scheduled: Option<String> = conn
            .query_row("SELECT next_refresh_at FROM users WHERE did = 'did:plc:scored'", [], |r| r.get(0))
            .unwrap();
        assert!(scheduled.is_some(), "a user with scores gets a first refresh");
        let unscheduled: Option<String> = conn
            .query_row("SELECT next_refresh_at FROM users WHERE did = 'did:plc:unscored'", [], |r| r.get(0))
            .unwrap();
        assert!(unscheduled.is_none(), "nothing to refresh, nothing scheduled");

        let max: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(max, 18);
    }
```

Also change both existing assertions `assert_eq!(versions, (1..=17).collect::<Vec<i64>>());` (near `:717` and `:832`) to `(1..=18)`, and the two comments above them ("through v17") to v18.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib db::schema::tests`
Expected: the two v18 tests FAIL (no such column); the two version-list tests FAIL (17 ≠ 18).

- [ ] **Step 3: Write the SQLite migration**

In `src/db/schema.rs`, after the v17 `run_migration` block and before `Ok(())`:

```rust
    // v18 (#343 §4.4, #344): score expiry + refresh bookkeeping.
    //
    // account_scores gains a generation stamp and an expiry. Existing rows
    // are stamped 'legacy' (never equal to SCORING_GENERATION, so they are
    // expired immediately and hidden) with valid_until = scored_at + 14 d,
    // the widest window, purely so the column is non-NULL for them.
    //
    // users.next_refresh_at is set to NOW for every user who has scores, so
    // the first admitter tick after this deploy enqueues a refresh for each
    // of them and their legacy High/Elevated rows come back within the hour
    // rather than the next nightly. RFC3339, like scan_queue timestamps —
    // the refresh tick compares it against `Utc::now().to_rfc3339()`.
    //
    // valid_until stays nullable here: SQLite cannot add NOT NULL to an
    // existing column without rebuilding the table. The fresh predicate
    // treats NULL as expired, so a missing value fails closed.
    run_migration(conn, 18, |c| {
        c.execute_batch(
            "BEGIN;
             ALTER TABLE account_scores
                 ADD COLUMN scoring_generation TEXT NOT NULL DEFAULT 'legacy';
             ALTER TABLE account_scores ADD COLUMN valid_until TEXT;
             UPDATE account_scores
                 SET valid_until = datetime(scored_at, '+14 days')
                 WHERE valid_until IS NULL;
             CREATE INDEX IF NOT EXISTS idx_account_scores_user_tier_valid
                 ON account_scores (user_did, threat_tier, valid_until);
             ALTER TABLE scan_queue ADD COLUMN kind TEXT NOT NULL DEFAULT 'full';
             ALTER TABLE users ADD COLUMN next_refresh_at TEXT;
             UPDATE users
                 SET next_refresh_at = strftime('%Y-%m-%dT%H:%M:%S+00:00', 'now')
                 WHERE did IN (SELECT DISTINCT user_did FROM account_scores);
             COMMIT;",
        )
    })?;
```

- [ ] **Step 4: Run the SQLite tests to verify they pass**

Run: `cargo test --lib db::schema::tests`
Expected: all pass.

- [ ] **Step 5: Write the failing Postgres tests**

Append to `tests/db_postgres.rs` (same harness as `test_pg_migration_v17_upgrades_from_v16`: `cache_test_lock()`, `database_url()`, `connect_postgres`):

```rust
/// v18 (#344): expiry columns, queue kind, refresh schedule — fresh database.
#[tokio::test]
async fn test_pg_migration_v18_adds_expiry_and_refresh_columns() {
    let _guard = cache_test_lock().lock().await;
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();

    let cols: i64 = sqlx_core::query::query(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_name = 'account_scores'
           AND column_name IN ('scoring_generation', 'valid_until')",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(cols, 2);

    let not_null: String = sqlx_core::query::query(
        "SELECT is_nullable FROM information_schema.columns
         WHERE table_name = 'account_scores' AND column_name = 'valid_until'",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(not_null, "NO", "valid_until is NOT NULL on Postgres");

    let kind_cols: i64 = sqlx_core::query::query(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE (table_name = 'scan_queue' AND column_name = 'kind')
            OR (table_name = 'users' AND column_name = 'next_refresh_at')",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(kind_cols, 2);

    let idx: i64 = sqlx_core::query::query(
        "SELECT COUNT(*) FROM pg_indexes
         WHERE schemaname = 'public' AND indexname = 'idx_account_scores_user_tier_valid'",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(idx, 1);

    let recorded: i64 =
        sqlx_core::query::query("SELECT COUNT(*) FROM schema_version WHERE version = 18")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(recorded, 1);
}

/// Simulate a v17 database holding scores, a queue row and two users; the
/// upgrade must backfill exactly as the SQLite migration does.
#[tokio::test]
async fn test_pg_migration_v18_upgrades_from_v17() {
    let _guard = cache_test_lock().lock().await;
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    sqlx_core::raw_sql::raw_sql(
        "DROP INDEX IF EXISTS idx_account_scores_user_tier_valid;
         ALTER TABLE account_scores DROP COLUMN IF EXISTS scoring_generation;
         ALTER TABLE account_scores DROP COLUMN IF EXISTS valid_until;
         ALTER TABLE scan_queue DROP COLUMN IF EXISTS kind;
         ALTER TABLE users DROP COLUMN IF EXISTS next_refresh_at;
         DELETE FROM schema_version WHERE version = 18;
         DELETE FROM account_scores WHERE user_did IN ('did:plc:v18scored');
         DELETE FROM scan_queue WHERE user_did IN ('did:plc:v18scored');
         DELETE FROM users WHERE did IN ('did:plc:v18scored', 'did:plc:v18unscored');
         INSERT INTO users (did, handle) VALUES ('did:plc:v18scored', 'scored.test'), ('did:plc:v18unscored', 'unscored.test');
         INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at)
             VALUES ('did:plc:v18scored', 'did:plc:v18acct', 'acct.test', 40.0, 'High', '2026-09-01 12:00:00+00');
         INSERT INTO scan_queue (user_did, status, enqueued_at)
             VALUES ('did:plc:v18scored', 'done', '2026-09-01 12:00:00+00');",
    )
    .execute(&pool)
    .await
    .unwrap();

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();

    let row = sqlx_core::query::query(
        "SELECT scoring_generation, to_char(valid_until AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')
         FROM account_scores WHERE did = 'did:plc:v18acct'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>(0), "legacy");
    assert_eq!(row.get::<String, _>(1), "2026-09-15 12:00:00");

    let kind: String =
        sqlx_core::query::query("SELECT kind FROM scan_queue WHERE user_did = 'did:plc:v18scored'")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(kind, "full");

    let scheduled: Option<chrono::DateTime<chrono::Utc>> =
        sqlx_core::query::query("SELECT next_refresh_at FROM users WHERE did = 'did:plc:v18scored'")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert!(scheduled.is_some());
    let unscheduled: Option<chrono::DateTime<chrono::Utc>> =
        sqlx_core::query::query("SELECT next_refresh_at FROM users WHERE did = 'did:plc:v18unscored'")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert!(unscheduled.is_none());

    let recorded: i64 =
        sqlx_core::query::query("SELECT COUNT(*) FROM schema_version WHERE version = 18")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(recorded, 1, "applied exactly once");

    // Leave the test database as we found it.
    sqlx_core::raw_sql::raw_sql(
        "DELETE FROM account_scores WHERE user_did = 'did:plc:v18scored';
         DELETE FROM scan_queue WHERE user_did = 'did:plc:v18scored';
         DELETE FROM users WHERE did IN ('did:plc:v18scored', 'did:plc:v18unscored');",
    )
    .execute(&pool)
    .await
    .unwrap();
}
```

If `chrono` is not already a dev-dependency-visible crate in `tests/db_postgres.rs`, it is a normal dependency of the crate and available to integration tests as `chrono::` — check `Cargo.toml` `[dependencies]` has `chrono` (it does; `scan_queue_entry` uses it).

- [ ] **Step 6: Run the Postgres tests to verify they fail**

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --features postgres --test db_postgres v18`
Expected: FAIL — columns missing.

- [ ] **Step 7: Write the Postgres migration and register it**

Create `migrations/postgres/0018_score_expiry.sql`:

```sql
-- Migration v18 (#343 §4.4, #344): score expiry + refresh bookkeeping.
--
-- account_scores.scoring_generation / valid_until: a row is fresh only when
-- scoring_generation = the binary's SCORING_GENERATION AND valid_until is in
-- the future. Pre-existing rows are stamped 'legacy' (never the current
-- value, so they are hidden immediately) with valid_until = scored_at + 14 d
-- so the column can be NOT NULL. The DEFAULT on scoring_generation is
-- dropped after the backfill on purpose: a writer that forgets to stamp a
-- row must fail loudly, not silently write 'legacy'.
--
-- scan_queue.kind: 'full' is today's scan; 'refresh' re-scores the
-- High/Elevated rows read from account_scores (no Constellation, no
-- fingerprint rebuild). Same admitter, same lease, same fencing token.
--
-- users.next_refresh_at: when the admitter tick next enqueues a refresh for
-- this user. Set to NOW() for every user who already has scores so the first
-- tick after this deploy refreshes their legacy High/Elevated rows.

ALTER TABLE account_scores
    ADD COLUMN IF NOT EXISTS scoring_generation TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE account_scores
    ADD COLUMN IF NOT EXISTS valid_until TIMESTAMPTZ;
UPDATE account_scores
    SET valid_until = scored_at + INTERVAL '14 days'
    WHERE valid_until IS NULL;
ALTER TABLE account_scores ALTER COLUMN valid_until SET NOT NULL;
ALTER TABLE account_scores ALTER COLUMN scoring_generation DROP DEFAULT;

-- The refresh job reads "High/Elevated rows about to expire" per user; the
-- tier lists read "fresh rows" per user. Both filter on these three.
CREATE INDEX IF NOT EXISTS idx_account_scores_user_tier_valid
    ON account_scores (user_did, threat_tier, valid_until);

ALTER TABLE scan_queue
    ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'full'
    CHECK (kind IN ('full', 'refresh'));

ALTER TABLE users ADD COLUMN IF NOT EXISTS next_refresh_at TIMESTAMPTZ;
UPDATE users
    SET next_refresh_at = NOW()
    WHERE next_refresh_at IS NULL
      AND did IN (SELECT DISTINCT user_did FROM account_scores);

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (18) ON CONFLICT DO NOTHING;
```

In `src/db/postgres.rs`, after the `(17, include_str!(...0017_cache_indexes.sql))` tuple:

```rust
                (
                    18,
                    include_str!("../../migrations/postgres/0018_score_expiry.sql"),
                ),
```

- [ ] **Step 8: Run the Postgres tests to verify they pass**

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --features postgres --test db_postgres`
Expected: all pass (the whole file, not only `v18` — the upgrade test drops columns other tests rely on, and the lock serialises them; a full run proves nothing else broke).

- [ ] **Step 9: Commit**

```bash
git add src/db/schema.rs migrations/postgres/0018_score_expiry.sql src/db/postgres.rs tests/db_postgres.rs
git commit -m 'feat(344): migration v18 — score expiry columns, scan_queue.kind, users.next_refresh_at

account_scores.scoring_generation + valid_until (legacy rows stamped
"legacy", valid_until = scored_at + 14 d), scan_queue.kind (full|refresh),
users.next_refresh_at (NOW for every user with scores so the first tick
refreshes them). Fresh + upgrade-from-v17 tests on both backends. Nothing
reads the columns yet.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 3: Stamp on write, filter on read, hard-error freshness

**Why:** This is the heart of #344. Today `upsert_account_score` never records which formula produced a row; `get_fresh_scored_dids(user_did, 7)` gates re-scoring on a flat 7 days (`sweep.rs:136`, `amplification.rs:314`), both wrapped in `.unwrap_or_default()` so a DB error silently re-scores everything; and `get_ranked_threats` shows rows of any age. After this task every writer stamps `scoring_generation = SCORING_GENERATION` and `valid_until = now + staleness_days(confidence)`, every tier read is fresh-only, and the pipeline's freshness read is a hard error.

**Files:**
- Modify: `src/db/traits.rs:343-355` (three signatures), `src/db/queries.rs:216-312` (upsert, ranked), `:315-364` (stale, fresh), `:1410-1417` (`count_not_assessed`), `src/db/sqlite.rs:124-150` (trait impls), `src/db/postgres.rs:445-600` (upsert, ranked, stale, fresh), `count_not_assessed` in postgres.rs
- Modify: `src/pipeline/sweep.rs:132-140`, `src/pipeline/amplification.rs:309-318`
- Modify: `src/db/queries.rs` inline tests `:2704-2730` (`test_is_score_stale`), `src/db/sqlite.rs:1055-1060` (`test_trait_is_score_stale_missing`), `tests/db_postgres.rs:601-620` and `:836-900` (the two stale/fresh tests)
- Test: `tests/unit_staleness.rs` (rewrite), `tests/db_postgres.rs` (append)

**Interfaces:**
- Consumes: `SCORING_GENERATION` (Task 1), `ScoringConfidence::staleness_days_for_label` (Task 1), columns from Task 2.
- Produces (trait `Database`):
  - `async fn is_score_stale(&self, user_did: &str, did: &str) -> Result<bool>` — `true` when no row, expired, or old generation.
  - `async fn get_fresh_scored_dids(&self, user_did: &str) -> Result<Vec<String>>` — exact complement of `is_score_stale` over the user's rows.
  - `async fn count_expired(&self, user_did: &str) -> Result<i64>` — rows that are **not** fresh (any tier, including NotAssessed).
  - `get_ranked_threats` and `count_not_assessed` become fresh-only. Unchanged signatures.
  - `upsert_account_score` stamps both columns; `AccountScore` gains **no** fields (the stamp is derived at write time; the read side never needs it back).
- Consequence to record (Task 10 CHANGELOG): `charcoal migrate` copies via `get_ranked_threats`, so it now copies only fresh rows and restamps them as the current generation with a new `valid_until`. Expired rows are not migrated; the refresh job rebuilds the High/Elevated ones.

- [ ] **Step 1: Rewrite `tests/unit_staleness.rs` as the failing spec**

Replace the file's contents with:

```rust
// Unit tests for score freshness (#213 Task 5, redefined by #344 / #343 §4.4).
//
// A row is FRESH when `valid_until` is in the future AND `scoring_generation`
// equals the binary's SCORING_GENERATION. Discovery loops fetch the fresh set
// once (`get_fresh_scored_dids`) and test membership in memory; these tests
// pin that the bulk set is EXACTLY the complement of `is_score_stale` for the
// same data — a mismatch would silently re-score (or skip) every account —
// and that the write path stamps both columns from the confidence tier.

use charcoal::db::models::{AccountScore, ScoringConfidence};
use charcoal::db::queries::{
    count_expired, count_not_assessed, get_fresh_scored_dids, get_ranked_threats, is_score_stale,
    upsert_account_score,
};
use charcoal::db::schema::create_tables;
use charcoal::scoring::generation::{LEGACY_GENERATION, SCORING_GENERATION};
use rusqlite::{params, Connection};
use std::collections::HashSet;

const USER: &str = "did:plc:testuser000000000000";

/// Insert a scored row whose `valid_until` is `valid_for_days` from now
/// (negative = already expired) under `generation`.
fn insert_score(conn: &Connection, did: &str, valid_for_days: i64, generation: &str) {
    conn.execute(
        "INSERT INTO account_scores
             (user_did, did, handle, threat_score, threat_tier, scoring_generation, valid_until)
         VALUES (?1, ?2, ?3, 20.0, 'Elevated', ?4, datetime('now', ?5))",
        params![
            USER,
            did,
            format!("{did}.handle"),
            generation,
            format!("{valid_for_days:+} days")
        ],
    )
    .unwrap();
}

fn score_with_confidence(did: &str, confidence: Option<&str>) -> AccountScore {
    AccountScore {
        did: did.to_string(),
        handle: format!("{did}.handle"),
        toxicity_score: Some(0.5),
        topic_overlap: Some(0.5),
        overlap_legacy: None,
        threat_score: Some(20.0),
        threat_tier: Some("Elevated".to_string()),
        posts_analyzed: 50,
        top_toxic_posts: vec![],
        scored_at: String::new(),
        behavioral_signals: None,
        context_score: None,
        graph_distance: None,
        fingerprint_quality: None,
        scoring_confidence: confidence.map(str::to_string),
    }
}

fn fresh_set(conn: &Connection) -> HashSet<String> {
    get_fresh_scored_dids(conn, USER).unwrap().into_iter().collect()
}

#[test]
fn fresh_set_is_exactly_the_non_stale_dids() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();

    insert_score(&conn, "did:plc:current", 5, SCORING_GENERATION);
    insert_score(&conn, "did:plc:expired", -1, SCORING_GENERATION);
    insert_score(&conn, "did:plc:legacy", 5, LEGACY_GENERATION);
    insert_score(&conn, "did:plc:oldgen", 5, "1999-01-01");
    // A NULL valid_until (SQLite cannot enforce NOT NULL post-hoc) is expired.
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, scoring_generation, valid_until)
         VALUES (?1, 'did:plc:nullvalid', 'n.handle', ?2, NULL)",
        params![USER, SCORING_GENERATION],
    )
    .unwrap();

    let fresh = fresh_set(&conn);
    assert_eq!(
        fresh,
        HashSet::from(["did:plc:current".to_string()]),
        "only current-generation rows with a future valid_until are fresh"
    );

    for did in [
        "did:plc:current",
        "did:plc:expired",
        "did:plc:legacy",
        "did:plc:oldgen",
        "did:plc:nullvalid",
        "did:plc:neverscored",
    ] {
        let stale = is_score_stale(&conn, USER, did).unwrap();
        assert_eq!(
            fresh.contains(did),
            !stale,
            "fresh-set membership must equal !is_score_stale for {did}"
        );
    }
}

#[test]
fn fresh_set_is_scoped_to_the_user() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, scoring_generation, valid_until)
         VALUES ('did:plc:otheruser', 'did:plc:shared', 'shared.handle', ?1, datetime('now', '+5 days'))",
        params![SCORING_GENERATION],
    )
    .unwrap();

    assert!(!fresh_set(&conn).contains("did:plc:shared"));
    assert!(is_score_stale(&conn, USER, "did:plc:shared").unwrap());
}

/// The write path is what makes every other query correct: both columns
/// must be stamped, and the window must come from the confidence tier.
#[test]
fn upsert_stamps_generation_and_valid_until_from_confidence() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();

    for (did, confidence, expected_days) in [
        ("did:plc:low", Some("low"), 3.0),
        ("did:plc:standard", Some("standard"), 7.0),
        ("did:plc:high", Some("high"), 14.0),
        ("did:plc:none", None, 7.0),
        ("did:plc:bogus", Some("bogus"), 7.0),
    ] {
        upsert_account_score(&conn, USER, &score_with_confidence(did, confidence)).unwrap();
        let (generation, days): (String, f64) = conn
            .query_row(
                "SELECT scoring_generation, julianday(valid_until) - julianday(scored_at)
                 FROM account_scores WHERE user_did = ?1 AND did = ?2",
                params![USER, did],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(generation, SCORING_GENERATION, "{did}");
        assert!((days - expected_days).abs() < 0.01, "{did}: {days} days");
        assert!(!is_score_stale(&conn, USER, did).unwrap(), "{did} just written");
    }
}

/// Re-scoring a legacy row must bring it back: the ON CONFLICT branch has
/// to restamp both columns, not only the score.
#[test]
fn upsert_restamps_an_existing_legacy_row() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    insert_score(&conn, "did:plc:reborn", 5, LEGACY_GENERATION);
    assert!(is_score_stale(&conn, USER, "did:plc:reborn").unwrap());

    upsert_account_score(&conn, USER, &score_with_confidence("did:plc:reborn", Some("high")))
        .unwrap();

    assert!(!is_score_stale(&conn, USER, "did:plc:reborn").unwrap());
    assert_eq!(count_expired(&conn, USER).unwrap(), 0);
}

#[test]
fn ranked_threats_and_counts_hide_expired_rows() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    insert_score(&conn, "did:plc:current", 5, SCORING_GENERATION);
    insert_score(&conn, "did:plc:expired", -1, SCORING_GENERATION);
    insert_score(&conn, "did:plc:legacy", 5, LEGACY_GENERATION);
    // A NotAssessed row that expired must leave the not_assessed count too.
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, threat_tier, scoring_generation, valid_until)
         VALUES (?1, 'did:plc:na-expired', 'na.handle', 'NotAssessed', ?2, datetime('now', '-1 days'))",
        params![USER, SCORING_GENERATION],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, threat_tier, scoring_generation, valid_until)
         VALUES (?1, 'did:plc:na-fresh', 'na2.handle', 'NotAssessed', ?2, datetime('now', '+1 days'))",
        params![USER, SCORING_GENERATION],
    )
    .unwrap();

    let ranked = get_ranked_threats(&conn, USER, 0.0).unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0].did, "did:plc:current");
    assert_eq!(count_not_assessed(&conn, USER).unwrap(), 1);
    // expired + legacy + na-expired
    assert_eq!(count_expired(&conn, USER).unwrap(), 3);
}

/// Pin the enum's numbers, since the write path now depends on them.
#[test]
fn staleness_days_are_three_seven_fourteen() {
    assert_eq!(ScoringConfidence::Low.staleness_days(), 3);
    assert_eq!(ScoringConfidence::Standard.staleness_days(), 7);
    assert_eq!(ScoringConfidence::High.staleness_days(), 14);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test unit_staleness`
Expected: compile error — `count_expired` does not exist; `get_fresh_scored_dids` takes three arguments.

- [ ] **Step 3: Change the trait**

In `src/db/traits.rs`, replace the `is_score_stale` and `get_fresh_scored_dids` declarations (`:345-355`) with:

```rust
    /// True when this account needs (re)scoring for this user: no row, or
    /// the row is expired (`valid_until <= now`), or it was scored under a
    /// generation other than [`crate::scoring::generation::SCORING_GENERATION`]
    /// (#344). The three conditions are one predicate — see the module doc
    /// on `generation` for why the generation is a build-time constant.
    async fn is_score_stale(&self, user_did: &str, did: &str) -> Result<bool>;

    /// Return the DIDs the user has FRESH scores for — the exact complement
    /// of `is_score_stale` over a whole user's scores, in one query.
    /// Discovery loops fetch this set once and test membership in memory
    /// instead of an `is_score_stale` round-trip per candidate (#213).
    ///
    /// Callers propagate the error (spec §4.4): a scan that cannot read
    /// freshness must not run, because the fallback ("treat everything as
    /// stale") re-scores the whole candidate set at full cost.
    async fn get_fresh_scored_dids(&self, user_did: &str) -> Result<Vec<String>>;

    /// Count this user's rows that are NOT fresh — expired or old-generation,
    /// any tier including NotAssessed. Shown as "N expired" so a user can see
    /// why a list shrank (#344); the rows themselves are kept.
    async fn count_expired(&self, user_did: &str) -> Result<i64>;
```

Update the doc comment on `get_ranked_threats` (`:341-343`) to add: "Fresh rows only (#344): expired and old-generation rows are hidden, not deleted — `count_expired` says how many." Same one-line note on `count_not_assessed` (`:611`).

- [ ] **Step 4: SQLite implementations**

In `src/db/queries.rs`, add at the top with the other `use` lines:

```rust
use crate::scoring::generation::SCORING_GENERATION;
```

Replace `upsert_account_score` (`:216-256`) with:

```rust
/// Insert or replace a user's score for an account.
///
/// Stamps `scoring_generation` and `valid_until` (#344): the generation is
/// the binary's constant, the window is `staleness_days()` for the row's
/// confidence label (3/7/14 d; missing or unknown label → 7 d). The ON
/// CONFLICT branch restamps both, so re-scoring a legacy or expired row is
/// what brings it back into the tier lists.
pub fn upsert_account_score(conn: &Connection, user_did: &str, score: &AccountScore) -> Result<()> {
    let top_posts_json = serde_json::to_string(&score.top_toxic_posts)?;
    let valid_for = format!(
        "+{} days",
        ScoringConfidence::staleness_days_for_label(score.scoring_confidence.as_deref())
    );
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, toxicity_score, topic_overlap, threat_score, threat_tier, posts_analyzed, top_toxic_posts, scored_at, behavioral_signals, context_score, graph_distance, fingerprint_quality, scoring_confidence, overlap_legacy, scoring_generation, valid_until)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'), ?10, ?11, ?12, ?13, ?14, ?15, ?16, datetime('now', ?17))
         ON CONFLICT(user_did, did) DO UPDATE SET
            handle = ?3,
            toxicity_score = ?4,
            topic_overlap = ?5,
            threat_score = ?6,
            threat_tier = ?7,
            posts_analyzed = ?8,
            top_toxic_posts = ?9,
            scored_at = datetime('now'),
            behavioral_signals = ?10,
            context_score = ?11,
            graph_distance = ?12,
            fingerprint_quality = ?13,
            scoring_confidence = ?14,
            overlap_legacy = ?15,
            scoring_generation = ?16,
            valid_until = datetime('now', ?17)",
        params![
            user_did,
            score.did,
            score.handle,
            score.toxicity_score,
            score.topic_overlap,
            score.threat_score,
            score.threat_tier,
            score.posts_analyzed,
            top_posts_json,
            score.behavioral_signals,
            score.context_score,
            score.graph_distance,
            score.fingerprint_quality,
            score.scoring_confidence,
            score.overlap_legacy,
            SCORING_GENERATION,
            valid_for,
        ],
    )?;
    Ok(())
}
```

(`ScoringConfidence` is in `super::models`; add it to the existing `use super::models::{...}` line.)

In `get_ranked_threats` (`:258-270`), change the `WHERE` to:

```rust
         FROM account_scores
         WHERE user_did = ?1 AND threat_score >= ?2
           AND valid_until IS NOT NULL
           AND datetime(valid_until) > datetime('now')
           AND scoring_generation = ?3
         ORDER BY threat_score DESC",
```

and the `query_map(params![user_did, min_score], …)` to `params![user_did, min_score, SCORING_GENERATION]`.

Replace `is_score_stale` and `get_fresh_scored_dids` (`:314-364`) with:

```rust
/// The fresh predicate, SQLite spelling. `valid_until` is nullable here
/// (see migration v18), and `datetime(NULL)` is NULL, so the explicit
/// `IS NOT NULL` makes a missing value read as expired rather than as
/// three-valued "unknown" that `NOT (...)` would flip.
const FRESH_SQL: &str = "valid_until IS NOT NULL
    AND datetime(valid_until) > datetime('now')
    AND scoring_generation = ";

/// Stale = no fresh row for (user, account). See `Database::is_score_stale`.
pub fn is_score_stale(conn: &Connection, user_did: &str, did: &str) -> Result<bool> {
    let fresh_rows: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM account_scores
             WHERE user_did = ?1 AND did = ?2 AND {FRESH_SQL}?3"
        ),
        params![user_did, did, SCORING_GENERATION],
        |row| row.get(0),
    )?;
    Ok(fresh_rows == 0)
}

/// The DIDs with a fresh score for this user — the complement of
/// `is_score_stale`; `tests/unit_staleness.rs` pins the equivalence.
pub fn get_fresh_scored_dids(conn: &Connection, user_did: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT did FROM account_scores WHERE user_did = ?1 AND {FRESH_SQL}?2"
    ))?;
    let dids = stmt
        .query_map(params![user_did, SCORING_GENERATION], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(dids)
}

/// Rows that are NOT fresh, any tier. See `Database::count_expired`.
pub fn count_expired(conn: &Connection, user_did: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM account_scores
             WHERE user_did = ?1 AND NOT ({FRESH_SQL}?2)"
        ),
        params![user_did, SCORING_GENERATION],
        |row| row.get(0),
    )?;
    Ok(count)
}
```

`FRESH_SQL` is interpolated with `format!` — it is a compile-time constant, not user input, so this is not string-built SQL in the injection sense; the user-supplied values still go through `params!`. Say so in a one-line comment above the const.

Replace `count_not_assessed` (`:1410-1417`) with:

```rust
pub fn count_not_assessed(conn: &Connection, user_did: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM account_scores
             WHERE user_did = ?1 AND threat_tier = 'NotAssessed' AND {FRESH_SQL}?2"
        ),
        params![user_did, SCORING_GENERATION],
        |row| row.get(0),
    )?;
    Ok(count)
}
```

In `src/db/sqlite.rs`, update the three trait impls (`:138-150`) to the new signatures and add `count_expired`:

```rust
    async fn is_score_stale(&self, user_did: &str, did: &str) -> Result<bool> {
        let conn = self.conn.lock().await;
        super::queries::is_score_stale(&conn, user_did, did)
    }

    async fn get_fresh_scored_dids(&self, user_did: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        super::queries::get_fresh_scored_dids(&conn, user_did)
    }

    async fn count_expired(&self, user_did: &str) -> Result<i64> {
        let conn = self.conn.lock().await;
        super::queries::count_expired(&conn, user_did)
    }
```

(Match the lock/`conn` idiom the neighbouring impls in that file use — copy the exact two lines from `get_all_scored_dids` at `:336`.)

Fix the inline tests: `src/db/queries.rs:2704-2730` `test_is_score_stale` — drop the `, 7` argument in both calls; the "fresh after upsert" assertion still holds because the upsert now stamps the row. `src/db/sqlite.rs:1055-1060` — drop `, 7`.

- [ ] **Step 5: Postgres implementations**

In `src/db/postgres.rs`, add `use crate::scoring::generation::SCORING_GENERATION;` to the imports. Replace `upsert_account_score` (`:445-491`): extend the column list with `scoring_generation, valid_until`, the VALUES with `$16, NOW() + make_interval(days => $17)`, the `DO UPDATE SET` with `scoring_generation = $16, valid_until = NOW() + make_interval(days => $17)`, and append the binds:

```rust
        .bind(SCORING_GENERATION)
        .bind(
            i32::try_from(ScoringConfidence::staleness_days_for_label(
                score.scoring_confidence.as_deref(),
            ))
            .context("staleness days exceed i32 range")?,
        )
```

(`ScoringConfidence` from `super::models`; `Context` from `anyhow` is already imported for `make_interval` binds.)

`get_ranked_threats` (`:494-508`): `WHERE user_did = $1 AND threat_score >= $2 AND valid_until > NOW() AND scoring_generation = $3` and `.bind(SCORING_GENERATION)` after `.bind(min_score)`.

Replace `is_score_stale` and `get_fresh_scored_dids` (`:555-590`) with:

```rust
    async fn is_score_stale(&self, user_did: &str, did: &str) -> Result<bool> {
        let fresh_rows: i64 = sqlx_core::query::query(
            "SELECT COUNT(*) FROM account_scores
             WHERE user_did = $1 AND did = $2
               AND valid_until > NOW() AND scoring_generation = $3",
        )
        .bind(user_did)
        .bind(did)
        .bind(SCORING_GENERATION)
        .fetch_one(&self.pool)
        .await?
        .get(0);
        Ok(fresh_rows == 0)
    }

    async fn get_fresh_scored_dids(&self, user_did: &str) -> Result<Vec<String>> {
        let rows = sqlx_core::query::query(
            "SELECT did FROM account_scores
             WHERE user_did = $1 AND valid_until > NOW() AND scoring_generation = $2",
        )
        .bind(user_did)
        .bind(SCORING_GENERATION)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>(0)).collect())
    }

    async fn count_expired(&self, user_did: &str) -> Result<i64> {
        let count: i64 = sqlx_core::query::query(
            "SELECT COUNT(*) FROM account_scores
             WHERE user_did = $1
               AND NOT (valid_until > NOW() AND scoring_generation = $2)",
        )
        .bind(user_did)
        .bind(SCORING_GENERATION)
        .fetch_one(&self.pool)
        .await?
        .get(0);
        Ok(count)
    }
```

`count_not_assessed` in postgres.rs (find with `rg -n "fn count_not_assessed" src/db/postgres.rs`): append `AND valid_until > NOW() AND scoring_generation = $2` and `.bind(SCORING_GENERATION)`.

- [ ] **Step 6: Callers — hard error, no literal**

`src/pipeline/sweep.rs:130-140`: replace the comment and the call with

```rust
    // Step 3: Filter to accounts with stale or missing scores (candidate set).
    // One bulk query instead of an is_score_stale round-trip per candidate
    // (#213). Freshness is generation + valid_until (#344); a read failure is
    // a hard error — the old empty-set fallback re-scored everything at full
    // cost, silently.
    let fresh: HashSet<String> = db
        .get_fresh_scored_dids(user_did)
        .await
        .context("reading fresh-score set for sweep")?
        .into_iter()
        .collect();
```

`src/pipeline/amplification.rs:306-318`: same shape —

```rust
    // Fetch the fresh-scored DID set ONCE for both the amplifier and follower
    // staleness gates below (#213). Scores aren't written until Phase C, so
    // the set is stable across both loops. Freshness is generation +
    // valid_until (#344); a read failure aborts the scan rather than
    // re-scoring the whole candidate set at full cost.
    let fresh_scored: HashSet<String> = db
        .get_fresh_scored_dids(user_did)
        .await
        .context("reading fresh-score set for amplification scoring")?
        .into_iter()
        .collect();
```

Both files: ensure `use anyhow::Context;` is present (add if not).

- [ ] **Step 7: Update the two Postgres tests and add the write-path one**

`tests/db_postgres.rs:601-620` (`test_pg_is_score_stale_missing`): drop `, 7`. `:836-900` (`test_pg_get_fresh_scored_dids_matches_is_score_stale`): the helper that inserts rows with `scored_at` offsets must instead set `valid_until`/`scoring_generation` — rewrite its inserts to

```rust
    // (did, valid_for_days, generation)
    for (did, days, generation) in [
        ("did:plc:pgfresh_current", 5, SCORING_GENERATION),
        ("did:plc:pgfresh_expired", -1, SCORING_GENERATION),
        ("did:plc:pgfresh_legacy", 5, "legacy"),
    ] {
        sqlx_core::query::query(
            "INSERT INTO account_scores (user_did, did, handle, scoring_generation, valid_until)
             VALUES ($1, $2, $3, $4, NOW() + make_interval(days => $5))
             ON CONFLICT (user_did, did) DO UPDATE
                 SET scoring_generation = $4, valid_until = NOW() + make_interval(days => $5)",
        )
        .bind(TEST_USER)
        .bind(did)
        .bind(format!("{did}.handle"))
        .bind(generation)
        .bind(days)
        .execute(&pool)
        .await
        .unwrap();
    }
```

then assert the fresh set is exactly `{pgfresh_current}` and loop the equivalence over the three plus `did:plc:pgfresh_never` with `db.is_score_stale(TEST_USER, did)`. Add `use charcoal::scoring::generation::SCORING_GENERATION;` at the top. Append a Postgres twin of `upsert_stamps_generation_and_valid_until_from_confidence`:

```rust
#[tokio::test]
async fn test_pg_upsert_stamps_generation_and_valid_until() {
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();

    for (did, confidence, expected_days) in [
        ("did:plc:pgstamp_low", Some("low"), 3.0_f64),
        ("did:plc:pgstamp_high", Some("high"), 14.0),
        ("did:plc:pgstamp_none", None, 7.0),
    ] {
        let mut score = make_test_score(did); // the file's existing AccountScore helper
        score.scoring_confidence = confidence.map(str::to_string);
        db.upsert_account_score(TEST_USER, &score).await.unwrap();
        let row = sqlx_core::query::query(
            "SELECT scoring_generation,
                    EXTRACT(EPOCH FROM (valid_until - scored_at)) / 86400.0
             FROM account_scores WHERE user_did = $1 AND did = $2",
        )
        .bind(TEST_USER)
        .bind(did)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.get::<String, _>(0), SCORING_GENERATION);
        let days: f64 = row.get(1);
        assert!((days - expected_days).abs() < 0.01, "{did}: {days}");
        assert!(!db.is_score_stale(TEST_USER, did).await.unwrap());
    }
    assert_eq!(db.count_expired(TEST_USER).await.unwrap(), 0);
}
```

Check the file for the name of its `AccountScore` builder helper (`rg -n "fn .*AccountScore" tests/db_postgres.rs`) and use that name instead of `make_test_score` if it differs; if there is none, construct the struct inline exactly as `score_with_confidence` does in `tests/unit_staleness.rs`. The `EXTRACT` result type is `NUMERIC` on Postgres ≥ 14; cast it: `(EXTRACT(EPOCH FROM (valid_until - scored_at)) / 86400.0)::float8`.

- [ ] **Step 8: Build and run everything**

Run: `cargo build --features web && cargo build --features postgres && cargo build`
Expected: clean. Any remaining `get_fresh_scored_dids(x, 7)` / `is_score_stale(x, y, 7)` call site the compiler finds (search: `rg -n "is_score_stale\(|get_fresh_scored_dids\(" src tests`) gets the extra argument removed.

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web`
Expected: all pass. `tests/composition.rs:421 report_includes_all_tier_counts` and `tests/web_oauth.rs:390` still pass because their rows go through `upsert_account_score` and are therefore fresh.

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres`
Expected: all pass.

Run: `cargo clippy --features web --all-targets && cargo clippy --features postgres --all-targets && cargo clippy --all-targets`
Expected: no warnings.

- [ ] **Step 9: Commit**

```bash
git add src/db/traits.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs src/pipeline/sweep.rs src/pipeline/amplification.rs tests/unit_staleness.rs tests/db_postgres.rs
git commit -m 'feat(344): stamp scores with generation + valid_until; tier reads are fresh-only

upsert_account_score writes scoring_generation = SCORING_GENERATION and
valid_until = now + staleness_days(confidence) — the 3/7/14 d tiers are
finally live. is_score_stale / get_fresh_scored_dids drop max_age_days and
use one predicate (valid_until > now AND generation = current); the two
pipeline callers propagate the error instead of re-scoring everything on
a DB blip. get_ranked_threats and count_not_assessed hide expired rows;
count_expired reports them.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 4: "N expired" — status API, dashboard, CLI

**Why:** Spec §4.4: "Expired rows are kept but filtered out of tier lists; the UI shows 'N expired' so a user can see why a list shrank." Without this, the day Phase 2 deploys every user's dashboard drops to zeros with no explanation.

**Files:**
- Modify: `src/web/handlers/status.rs:191-254` (the tier-count block and JSON body)
- Modify: `src/status.rs:51-60` (CLI)
- Modify: `web/src/lib/types.ts:32-42` (`TierCounts`), `web/src/lib/dashboard-state.ts:20-27`, `web/src/routes/(protected)/dashboard/+page.svelte:391-399`
- Test: `tests/web_oauth.rs` (append next to `status_surfaces_not_assessed_count_in_tier_counts`), `web/src/lib/dashboard-state.test.ts`

**Interfaces:**
- Consumes: `Database::count_expired` (Task 3).
- Produces: `GET /api/status` → `tier_counts.expired: number` (not included in `total`, like `not_assessed`). `TierCounts.expired` in TypeScript.

- [ ] **Step 1: Write the failing Rust test**

In `tests/web_oauth.rs`, inside the same `mod` as `status_surfaces_not_assessed_count_in_tier_counts`, append:

```rust
    /// #344: rows scored under an older generation are hidden from every tier
    /// and reported as `expired`, so the user can see why a list shrank.
    #[tokio::test]
    async fn status_reports_expired_rows_outside_the_tiers() {
        use charcoal::db::models::AccountScore;

        let Some((app, db)) = build_test_app_with_db() else {
            eprintln!("SKIP: models not present, cannot build test AppState");
            return;
        };

        let high = AccountScore {
            did: "did:plc:wasHigh".to_string(),
            handle: "washigh.bsky.social".to_string(),
            toxicity_score: Some(0.9),
            topic_overlap: Some(0.8),
            overlap_legacy: None,
            threat_score: Some(60.0),
            threat_tier: Some("High".to_string()),
            posts_analyzed: 50,
            top_toxic_posts: vec![],
            scored_at: String::new(),
            behavioral_signals: None,
            context_score: None,
            graph_distance: None,
            fingerprint_quality: None,
            scoring_confidence: Some("high".to_string()),
        };
        db.upsert_account_score(TEST_DID, &high).await.unwrap();
        // Age the row out of its generation the way migration v18 would.
        charcoal::web::test_helpers::expire_all_scores(&db, TEST_DID).await;

        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .header("cookie", session_cookie(TEST_DID))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["tier_counts"]["high"], 0, "expired rows leave the tiers");
        assert_eq!(json["tier_counts"]["expired"], 1);
        assert_eq!(json["tier_counts"]["total"], 0, "expired is not in total");
    }
```

`expire_all_scores` is a new test-support helper. The `Database` trait exposes no raw SQL (by design), and `build_test_app_with_db` (`src/web/test_helpers.rs:71`) hands back only an `Arc<dyn Database>`, so the helper downcasts to the SQLite backend the test apps are built on. Add to `src/web/test_helpers.rs`:

```rust
/// Test-only: mark every one of `user_did`'s scores as scored under an older
/// generation, the shape migration v18 leaves pre-existing rows in (#344).
/// Lives here rather than on the `Database` trait because production has no
/// business rewriting generations.
pub async fn expire_all_scores(db: &Arc<dyn crate::db::Database>, user_did: &str) {
    // `Database` exposes no raw SQL; go through the SQLite handle the helper
    // built. Downcast is safe: every test app in this module is SQLite.
    let sqlite = db
        .as_any()
        .downcast_ref::<crate::db::sqlite::SqliteDatabase>()
        .expect("test app databases are SqliteDatabase");
    sqlite
        .with_conn(|conn| {
            conn.execute(
                "UPDATE account_scores SET scoring_generation = 'legacy' WHERE user_did = ?1",
                rusqlite::params![user_did],
            )
            .map(|_| ())
        })
        .await
        .expect("expire scores");
}
```

This needs two small additions, made in Step 3: `fn as_any(&self) -> &dyn std::any::Any` on the `Database` trait (implemented as `{ self }` on both backends) and `SqliteDatabase::with_conn`, which locks the connection and runs a closure against it.


- [ ] **Step 2: Run the test to verify it fails**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_oauth status_reports_expired -- --show-output`
Expected: compile error (`expire_all_scores` / `as_any` missing). After adding the helpers and rerunning: FAIL on `json["tier_counts"]["expired"]` being null.

- [ ] **Step 3: Implement**

`src/db/traits.rs`, inside `pub trait Database`: `fn as_any(&self) -> &dyn std::any::Any;`. Implement `fn as_any(&self) -> &dyn std::any::Any { self }` in both `impl Database for SqliteDatabase` and `impl Database for PgDatabase`. In `src/db/sqlite.rs`, add to `impl SqliteDatabase`:

```rust
    /// Run a closure against the raw connection. Test-support only: lets
    /// integration tests shape rows the trait deliberately cannot (e.g. age a
    /// score out of its generation, #344) without widening the trait.
    pub async fn with_conn<T>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
    ) -> anyhow::Result<T> {
        let conn = self.conn.lock().await;
        Ok(f(&conn)?)
    }
```

`src/web/handlers/status.rs`: after the `not_assessed` block (`:225-232`) add

```rust
    // #344: rows hidden because they expired or predate the current scoring
    // generation. Reported so a shrunken list is explicable; NOT part of
    // `total`, which counts current results only. Same 500-on-error policy as
    // the two reads above — a silent 0 would hide the very thing this
    // number exists to show.
    let expired = match state.db.count_expired(&auth.effective_did).await {
        Ok(n) => n as u32,
        Err(e) => {
            tracing::error!(error = %e, "DB error counting expired scores in get_status");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    };
```

and in the JSON body after `"not_assessed": not_assessed,` add `"expired": expired,`.

`src/status.rs:51-60`: after the "Scored accounts" `println!`, add

```rust
    let expired = db.count_expired(user_did).await?;
    if expired > 0 {
        println!(
            "  {expired} expired (scored under an older generation or past their window — \
             hidden until the refresh job or a re-engagement re-scores them)"
        );
    }
```

`web/src/lib/types.ts` `TierCounts`: after `not_assessed: number;` add

```ts
	// Rows hidden because they expired or predate the current scoring
	// generation (#344). Not in `total`; shown so a shrunken list is explicable.
	expired: number;
```

`web/src/lib/dashboard-state.ts:24`: the `results` gate becomes

```ts
	if (
		status.scan_running ||
		status.tier_counts.total > 0 ||
		status.tier_counts.not_assessed > 0 ||
		status.tier_counts.expired > 0
	) {
		return 'results';
	}
```

with a comment line: `// expired rows are results too — the grid explains why they are hidden (#344).`

`web/src/lib/dashboard-state.test.ts`: the `status()` fixture's `tier_counts` gains `expired: 0`; add

```ts
	it('shows results (not welcome) when every score has expired', () => {
		expect(
			dashboardView(
				status({ tier_counts: { high: 0, elevated: 0, watch: 0, low: 0, not_assessed: 0, expired: 12, total: 0 } })
			)
		).toBe('results');
	});
```

`web/src/routes/(protected)/dashboard/+page.svelte:395-399`: after the not-assessed card add

```svelte
				<!-- #344: scored under an older generation or past their window.
				     Hidden from the tiers until the nightly refresh (High/Elevated)
				     or a re-engagement re-scores them. Neutral, unlinked. -->
				{#if status.tier_counts.expired > 0}
					<div
						class="tier-card tier-not-assessed"
						title="Scores past their refresh window — hidden until re-scored"
					>
						<span class="tier-count">{status.tier_counts.expired}</span>
						<span class="tier-label">Expired</span>
					</div>
				{/if}
```

Reuse the `tier-not-assessed` class; do not add CSS. Run the Svelte MCP `svelte-autofixer` on the edited component per the project rule and apply what it says.

Search `web/src` for other `tier_counts` literals that construct the type (`rg -n "not_assessed: 0" web/src`) and add `expired: 0` to each so `npm run build`'s type check passes.

- [ ] **Step 4: Run tests**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_oauth -- --show-output 2>&1 | grep -E "^\s*SKIP:|test result"`
Expected: no `SKIP:` lines; all pass.

Run: `npm --prefix web run test && npm --prefix web run build`
Expected: vitest green (37 tests); build clean.

- [ ] **Step 5: Commit**

```bash
git add src/db/traits.rs src/db/sqlite.rs src/db/postgres.rs src/web/test_helpers.rs src/web/handlers/status.rs src/status.rs tests/web_oauth.rs web/src/lib/types.ts web/src/lib/dashboard-state.ts web/src/lib/dashboard-state.test.ts "web/src/routes/(protected)/dashboard/+page.svelte"
git commit -m 'feat(344): report expired scores in tier_counts, dashboard card, CLI status

Expired/old-generation rows are hidden from the tiers (Task 3); this makes
the hiding visible: tier_counts.expired (outside total), an "Expired" card
on the dashboard, and a line in `charcoal status`. Adds Database::as_any +
SqliteDatabase::with_conn so a web test can age a row out of its generation.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 5: `scan_queue.kind` — refresh rows in the same queue

**Why:** Spec §4.4: `scan_queue.kind = 'full' | 'refresh'`. The refresh reuses the admitter, lease, fencing token and `LiveScans` untouched; the only new rules are (a) how a refresh enqueue and a user's full enqueue interact on the one row per user, (b) the ETA median must not learn from five-minute refreshes, (c) the 24-hour scan cooldown must not be reset by a refresh finishing.

**Rules (decided here, recorded in the spec by Task 10):**
- `enqueue_scan(user)` (full): re-queues a `done`/`failed` row as `full`; **upgrades** a `queued` `refresh` row to `full` (the human's request wins; `enqueued_at` is reset to now — honest about when they asked); no-op on `queued full` or any `running` row.
- `enqueue_refresh_scan(user)`: re-queues a `done`/`failed` row as `refresh`; **never** touches a `queued` or `running` row of either kind.
- Admission order is unchanged: `(enqueued_at, user_did)` — see Global Constraints.
- `scan_queue_entry`'s rolling median uses `kind = 'full'` rows only.
- `trigger_scan`'s cooldown reads the row's `finished_at` only when `kind = 'full'`; for a `refresh` row it falls back to `scan_state.last_full_scan_finished_at`, which `run_scan` writes on success.

**Files:**
- Modify: `src/db/traits.rs:117-131` (`ScanClaim`, `ScanQueueRow`), `:619` (trait — add `enqueue_refresh_scan`), `src/db/mod.rs` (re-export `ScanKind`)
- Modify: `src/db/queries.rs:1466-1548` (enqueue, claim), `:1634-1666` (list), `:1692-1712` (median); `src/db/sqlite.rs` (trait impl for `enqueue_refresh_scan`); `src/db/postgres.rs` (enqueue ~`:1640`, claim `:1667-1740`, list `:1867-1910`, median `:1842-1855`, `enqueue_refresh_scan`)
- Modify: `src/web/handlers/admin.rs` (`scan_row_json`), `web/src/lib/types.ts:168-177` (admin row type)
- Modify: `src/web/handlers/scan.rs:63-95` (cooldown), `src/web/scan_job.rs` (`run_scan`, after the pipeline result)
- Test: `tests/unit_scan_kind.rs` (new, SQLite, no models), `tests/db_postgres.rs` (append), `src/web/handlers/scan.rs` inline tests

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum ScanKind { Full, Refresh }
  impl ScanKind { pub fn as_str(&self) -> &'static str; pub fn from_str(s: &str) -> Option<Self>; }
  pub struct ScanClaim { pub user_did: String, pub claim_id: String, pub kind: ScanKind }
  pub struct ScanQueueRow { …existing…, pub kind: ScanKind }
  async fn enqueue_refresh_scan(&self, user_did: &str) -> Result<()>;
  ```
  `scan_state` key `last_full_scan_finished_at` (RFC3339), written by `run_scan` after `amplification::run` returns `Ok`.
- Task 6 calls `enqueue_refresh_scan`; Task 9 matches on `claim.kind`.

- [ ] **Step 1: Write the failing SQLite tests**

Create `tests/unit_scan_kind.rs`:

```rust
// #343 Phase 2 / #344: a second queue kind, `refresh`, in the one-row-per-user
// scan_queue. The rules under test are the ones that keep a nightly refresh
// from ever getting in a human's way: a full enqueue upgrades a queued
// refresh, a refresh enqueue never touches a queued or running row, and the
// ETA median ignores refresh durations.

use std::sync::Arc;

use charcoal::db::schema::create_tables;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::{Database, ScanKind};
use rusqlite::{params, Connection};

const USER: &str = "did:plc:kindtest0000000000000000";

fn db() -> Arc<SqliteDatabase> {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

async fn row(db: &SqliteDatabase, did: &str) -> (String, ScanKind) {
    let rows = db.list_scan_queue().await.unwrap();
    let r = rows.iter().find(|r| r.user_did == did).expect("row exists");
    (r.status.clone(), r.kind)
}

#[tokio::test]
async fn refresh_enqueue_creates_a_refresh_row() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    assert_eq!(row(&db, USER).await, ("queued".to_string(), ScanKind::Refresh));
}

#[tokio::test]
async fn a_full_enqueue_upgrades_a_queued_refresh() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    db.enqueue_scan(USER).await.unwrap();
    assert_eq!(row(&db, USER).await, ("queued".to_string(), ScanKind::Full));
}

#[tokio::test]
async fn a_refresh_enqueue_never_downgrades_a_queued_full() {
    let db = db();
    db.enqueue_scan(USER).await.unwrap();
    db.enqueue_refresh_scan(USER).await.unwrap();
    assert_eq!(row(&db, USER).await, ("queued".to_string(), ScanKind::Full));
}

#[tokio::test]
async fn a_refresh_enqueue_leaves_a_running_row_alone() {
    let db = db();
    db.enqueue_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().expect("claimed");
    assert_eq!(claim.kind, ScanKind::Full);
    db.enqueue_refresh_scan(USER).await.unwrap();
    assert_eq!(row(&db, USER).await, ("running".to_string(), ScanKind::Full));
    // And the fencing token still belongs to the running scan.
    assert!(db.heartbeat_scan(USER, &claim.claim_id, 60).await.unwrap());
}

#[tokio::test]
async fn a_done_row_is_requeued_as_refresh_and_claimed_with_its_kind() {
    let db = db();
    db.enqueue_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    db.finish_queued_scan(USER, &claim.claim_id, None).await.unwrap();

    db.enqueue_refresh_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().expect("refresh claimed");
    assert_eq!(claim.kind, ScanKind::Refresh);
    assert_eq!(row(&db, USER).await, ("running".to_string(), ScanKind::Refresh));
}

/// The ETA quoted to a queued user is a median of FULL scan durations. A
/// five-minute refresh in the sample would tell the next human "about 5
/// minutes" for a two-hour scan.
#[tokio::test]
async fn eta_median_ignores_refresh_rows() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    // One full scan of 3600 s, one refresh of 60 s, both done.
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, kind, enqueued_at, started_at, finished_at)
         VALUES ('did:plc:full', 'done', 'full',
                 '2026-09-10T00:00:00+00:00', '2026-09-10T00:00:00+00:00', '2026-09-10T01:00:00+00:00'),
                ('did:plc:refresh', 'done', 'refresh',
                 '2026-09-11T00:00:00+00:00', '2026-09-11T00:00:00+00:00', '2026-09-11T00:01:00+00:00')",
        [],
    )
    .unwrap();
    let db = SqliteDatabase::new(conn);
    db.enqueue_scan(USER).await.unwrap();
    let entry = db.scan_queue_entry(USER, 1).await.unwrap().unwrap();
    assert_eq!(entry.position, 1);
    assert_eq!(entry.eta_seconds, Some(3600), "median over full scans only");
}

#[test]
fn scan_kind_round_trips() {
    for k in [ScanKind::Full, ScanKind::Refresh] {
        assert_eq!(ScanKind::from_str(k.as_str()), Some(k));
    }
    assert_eq!(ScanKind::from_str("nightly"), None);
}

/// Every existing row in a v17 database was a full scan; list must say so.
#[test]
fn legacy_rows_default_to_full() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, enqueued_at) VALUES (?1, 'done', '2026-09-10T00:00:00+00:00')",
        params![USER],
    )
    .unwrap();
    let kind: String = conn
        .query_row("SELECT kind FROM scan_queue WHERE user_did = ?1", params![USER], |r| r.get(0))
        .unwrap();
    assert_eq!(kind, "full");
}
```

`eta_seconds` for position 1 with `concurrency_limit` 1 is `ceil(1/1) × median` = 3600 (see `eta_seconds` in traits/queries). If the existing `eta_seconds` helper rounds differently, assert on the value it produces for a single 3600 s full row — the point is that the 60 s refresh row does not pull it down.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test unit_scan_kind`
Expected: compile error — `ScanKind`, `enqueue_refresh_scan`, `claim.kind` missing.

- [ ] **Step 3: Types and trait**

`src/db/traits.rs` — above `ScanClaim` (`:117`):

```rust
/// What a `scan_queue` row asks the admitter to run (#343 §4.4).
///
/// `Full` is the scan the user triggers: Constellation events, follower
/// expansion, fingerprint refresh, the works. `Refresh` re-scores only this
/// user's High/Elevated rows that are about to expire or predate the current
/// scoring generation — candidates come from `account_scores`, not from the
/// network. Both run under the same claim/lease/fencing machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    Full,
    Refresh,
}

impl ScanKind {
    /// Column value. Postgres enforces the set with a CHECK constraint.
    pub fn as_str(&self) -> &'static str {
        match self {
            ScanKind::Full => "full",
            ScanKind::Refresh => "refresh",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "full" => Some(ScanKind::Full),
            "refresh" => Some(ScanKind::Refresh),
            _ => None,
        }
    }
}
```

Add `pub kind: ScanKind,` to `ScanClaim` (with doc: "What to run. Read from the row at claim time so a superseding full enqueue is honoured.") and to `ScanQueueRow` (doc: "`full` for every row that predates v18."). Add to the trait after `enqueue_scan`:

```rust
    /// Queue a refresh (#343 §4.4) for a user. Re-queues a `done`/`failed`
    /// row as `refresh`; a `queued` or `running` row of either kind is left
    /// untouched — a refresh never delays, downgrades or interrupts a scan
    /// the user asked for. Idempotent.
    async fn enqueue_refresh_scan(&self, user_did: &str) -> Result<()>;
```

Update the `enqueue_scan` doc: "Re-queues `done`/`failed` rows as `full` and **upgrades a queued `refresh` to `full`** (the human's request wins); no-op on a queued `full` or any `running` row."

`src/db/mod.rs`: add `ScanKind` to the `pub use traits::{…}` list.

- [ ] **Step 4: SQLite**

`src/db/queries.rs` `enqueue_scan` (`:1466-1480`):

```rust
pub fn enqueue_scan(conn: &Connection, user_did: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, kind, enqueued_at)
         VALUES (?1, 'queued', 'full', ?2)
         ON CONFLICT(user_did) DO UPDATE SET
             status = 'queued', kind = 'full', enqueued_at = ?2,
             started_at = NULL, finished_at = NULL,
             lease_expires = NULL, last_error = NULL,
             claim_id = NULL
         WHERE status IN ('done', 'failed')
            OR (status = 'queued' AND kind = 'refresh')",
        params![user_did, now],
    )?;
    Ok(())
}

/// Mirrors PgDatabase::enqueue_refresh_scan. Only finished rows are touched.
pub fn enqueue_refresh_scan(conn: &Connection, user_did: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, kind, enqueued_at)
         VALUES (?1, 'queued', 'refresh', ?2)
         ON CONFLICT(user_did) DO UPDATE SET
             status = 'queued', kind = 'refresh', enqueued_at = ?2,
             started_at = NULL, finished_at = NULL,
             lease_expires = NULL, last_error = NULL,
             claim_id = NULL
         WHERE status IN ('done', 'failed')",
        params![user_did, now],
    )?;
    Ok(())
}
```

`claim_next_scan` (`:1509-1545`): select `user_did, kind` (`Option<(String, String)>`), and return

```rust
    Ok(Some(ScanClaim {
        user_did: did,
        claim_id,
        kind: ScanKind::from_str(&kind)
            .with_context(|| format!("scan_queue.kind holds an unknown value {kind:?}"))?,
    }))
```

(`ScanKind` from `super::traits`; `anyhow::Context` already imported in queries.rs.)

`list_scan_queue` (`:1634-1666`): add `kind` as the 8th selected column (`q.kind` — after `last_error`, before the position subquery becomes index 7); map `kind: ScanKind::from_str(&row.get::<_, String>(6)?).unwrap_or(ScanKind::Full)` — hmm, no: an unknown value must not be silently `Full`. Use the same `with_context` pattern and convert inside the loop after `row?`, or select it as a String into a local and map with `.map_err(|e| rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e)))`. Choose the loop conversion: collect `(row fields…, kind_str)` then build `ScanQueueRow` with `ScanKind::from_str(&kind_str).with_context(...)?`. Renumber the position column index accordingly.

`scan_queue_entry` median (`:1712-1716`): `WHERE status = 'done' AND kind = 'full' AND started_at IS NOT NULL`.

`src/db/sqlite.rs`: add the trait impl `enqueue_refresh_scan` delegating to `queries::enqueue_refresh_scan` (same two-line shape as `enqueue_scan`).

- [ ] **Step 5: Postgres**

`src/db/postgres.rs`: `enqueue_scan` — same SQL as SQLite with `$1,$2` and `NOW()` in place of the bound time if that is what the existing impl does (read it first: `rg -n "async fn enqueue_scan" -A18 src/db/postgres.rs`) and the extended `WHERE`. Add `enqueue_refresh_scan` next to it. `claim_next_scan` (`:1697-1740`): `SELECT user_did, kind … FOR UPDATE SKIP LOCKED`; carry `kind` into `ScanClaim` via `ScanKind::from_str(...).with_context(...)?`. `list_scan_queue` (`:1872-1910`): select `kind`, map it. Median (`:1844-1852`): `WHERE status = 'done' AND kind = 'full' AND started_at IS NOT NULL`.

- [ ] **Step 6: Admin JSON + TypeScript type**

`src/web/handlers/admin.rs` `scan_row_json`: add `"kind": row.kind.as_str(),`. `web/src/lib/types.ts:168-177` (the admin queue row interface): add `kind: 'full' | 'refresh';` with the comment `// 'refresh' rows are the nightly High/Elevated re-score (#344).`

- [ ] **Step 7: Cooldown fallback + `last_full_scan_finished_at`**

`src/web/scan_job.rs` `run_scan`, immediately after `let result = crate::pipeline::amplification::run(...).await;` (`:1104`):

```rust
    // #344: a nightly refresh reuses this user's single scan_queue row, so
    // `finished_at` on the row stops meaning "last FULL scan". The cooldown
    // in handlers/scan.rs reads this key when the row is a refresh.
    if result.is_ok() {
        if let Err(e) = db
            .set_scan_state(
                user_did,
                "last_full_scan_finished_at",
                &chrono::Utc::now().to_rfc3339(),
            )
            .await
        {
            warn!(error = %format!("{e:#}"), "could not record last_full_scan_finished_at");
        }
    }
```

`src/web/handlers/scan.rs`: add a pure helper above `trigger_scan`:

```rust
/// The instant the user's last FULL scan finished, for the cooldown.
///
/// `scan_queue` holds one row per user, and since #344 that row may be a
/// nightly refresh. A refresh finishing must not (a) start a new cooldown
/// or (b) hide the real last full scan, so a `refresh` row defers to the
/// `last_full_scan_finished_at` marker `run_scan` writes on success.
fn last_full_finished_at(
    row: Option<&crate::db::traits::ScanQueueRow>,
    marker: Option<String>,
) -> Option<String> {
    match row {
        Some(r) if r.status == "done" && r.kind == crate::db::ScanKind::Full => {
            r.finished_at.clone()
        }
        Some(r) if r.kind == crate::db::ScanKind::Refresh => marker,
        _ => None,
    }
}
```

Rewrite the cooldown block (`:63-95`) to:

```rust
    match state.db.list_scan_queue().await {
        Ok(rows) => {
            let row = rows.iter().find(|r| r.user_did == auth.did);
            let marker = match state
                .db
                .get_scan_state(&auth.did, "last_full_scan_finished_at")
                .await
            {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %format!("{e:#}"), "cooldown marker unreadable — treating as absent");
                    None
                }
            };
            if let Some(finished_at) = last_full_finished_at(row, marker) {
                if let Some(retry_at) = cooldown_retry_at(
                    &finished_at,
                    chrono::Utc::now(),
                    state.config.scan_cooldown_hours,
                ) {
                    let window = match state.config.scan_cooldown_hours {
                        24 => "one per day".to_string(),
                        h => format!("one every {h} hours"),
                    };
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        Json(serde_json::json!({
                            "error": format!("You scanned recently — scans are limited to {window}"),
                            "retry_at": retry_at,
                        })),
                    )
                        .into_response();
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "cooldown check skipped — could not read scan queue");
        }
    }
```

(keep the existing `#258` comment above it). Add inline tests in `scan.rs`'s `#[cfg(test)] mod tests` (create the module if there is none — `cooldown_retry_at` may already have tests; add beside them):

```rust
    fn row(status: &str, kind: crate::db::ScanKind, finished_at: Option<&str>) -> crate::db::traits::ScanQueueRow {
        crate::db::traits::ScanQueueRow {
            user_did: "did:plc:x".into(),
            status: status.into(),
            position: 0,
            enqueued_at: "2026-09-10T00:00:00+00:00".into(),
            started_at: None,
            finished_at: finished_at.map(str::to_string),
            last_error: None,
            kind,
        }
    }

    #[test]
    fn a_done_full_row_is_the_cooldown_anchor() {
        let r = row("done", crate::db::ScanKind::Full, Some("2026-09-12T00:00:00+00:00"));
        assert_eq!(
            last_full_finished_at(Some(&r), Some("2026-01-01T00:00:00+00:00".into())),
            Some("2026-09-12T00:00:00+00:00".into())
        );
    }

    #[test]
    fn a_refresh_row_defers_to_the_marker() {
        let r = row("done", crate::db::ScanKind::Refresh, Some("2026-09-13T03:00:00+00:00"));
        assert_eq!(
            last_full_finished_at(Some(&r), Some("2026-09-12T00:00:00+00:00".into())),
            Some("2026-09-12T00:00:00+00:00".into()),
            "the refresh's own finished_at must not start a cooldown"
        );
        assert_eq!(last_full_finished_at(Some(&r), None), None);
    }

    #[test]
    fn queued_running_failed_or_missing_rows_have_no_anchor() {
        for status in ["queued", "running", "failed"] {
            let r = row(status, crate::db::ScanKind::Full, Some("2026-09-12T00:00:00+00:00"));
            assert_eq!(last_full_finished_at(Some(&r), None), None, "{status}");
        }
        assert_eq!(last_full_finished_at(None, Some("x".into())), None);
    }
```

- [ ] **Step 8: Postgres twins of the kind tests**

Append to `tests/db_postgres.rs` (harness: `database_url()`, `connect_postgres`, `cleanup_test_data` semantics — use distinct DIDs prefixed `did:plc:pgkind_` and delete them at the end of each test):

```rust
#[tokio::test]
async fn test_pg_full_enqueue_upgrades_queued_refresh_and_refresh_never_downgrades() {
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    let did = "did:plc:pgkind_upgrade";
    db.enqueue_refresh_scan(did).await.unwrap();
    db.enqueue_scan(did).await.unwrap();
    let rows = db.list_scan_queue().await.unwrap();
    let r = rows.iter().find(|r| r.user_did == did).unwrap();
    assert_eq!((r.status.as_str(), r.kind), ("queued", charcoal::db::ScanKind::Full));

    db.enqueue_refresh_scan(did).await.unwrap();
    let rows = db.list_scan_queue().await.unwrap();
    let r = rows.iter().find(|r| r.user_did == did).unwrap();
    assert_eq!(r.kind, charcoal::db::ScanKind::Full, "refresh never downgrades");

    // Clean up: reuse the file's raw-SQL cleanup idiom.
    pg_delete_queue_row(&url, did).await;
}

#[tokio::test]
async fn test_pg_claim_carries_kind_and_median_ignores_refresh() {
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    for did in ["did:plc:pgkind_full", "did:plc:pgkind_refresh", "did:plc:pgkind_waiting"] {
        pg_delete_queue_row(&url, did).await;
    }
    sqlx_core::raw_sql::raw_sql(
        "INSERT INTO scan_queue (user_did, status, kind, enqueued_at, started_at, finished_at) VALUES
           ('did:plc:pgkind_full', 'done', 'full', '2026-09-10 00:00:00+00', '2026-09-10 00:00:00+00', '2026-09-10 01:00:00+00'),
           ('did:plc:pgkind_refresh', 'done', 'refresh', '2026-09-11 00:00:00+00', '2026-09-11 00:00:00+00', '2026-09-11 00:01:00+00');",
    )
    .execute(&pool)
    .await
    .unwrap();

    db.enqueue_refresh_scan("did:plc:pgkind_refresh").await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().expect("claimed");
    assert_eq!(claim.user_did, "did:plc:pgkind_refresh");
    assert_eq!(claim.kind, charcoal::db::ScanKind::Refresh);
    db.finish_queued_scan(&claim.user_did, &claim.claim_id, None).await.unwrap();

    db.enqueue_scan("did:plc:pgkind_waiting").await.unwrap();
    let entry = db.scan_queue_entry("did:plc:pgkind_waiting", 1).await.unwrap().unwrap();
    assert_eq!(entry.eta_seconds, Some(3600), "median over full scans only");

    for did in ["did:plc:pgkind_full", "did:plc:pgkind_refresh", "did:plc:pgkind_waiting"] {
        pg_delete_queue_row(&url, did).await;
    }
}

async fn pg_delete_queue_row(url: &str, did: &str) {
    use sqlx_core::pool::Pool;
    use sqlx_postgres::Postgres;
    let pool = Pool::<Postgres>::connect(url).await.unwrap();
    sqlx_core::query::query("DELETE FROM scan_queue WHERE user_did = $1")
        .bind(did)
        .execute(&pool)
        .await
        .unwrap();
}
```

The median test assumes no other `done full` rows exist in `charcoal_test` at that moment; other queue tests in the file clean up after themselves (check `cleanup_test_data` at `:28` deletes `scan_queue` rows for their DIDs — if it does not cover the file's other queue DIDs, extend it to `DELETE FROM scan_queue WHERE user_did LIKE 'did:plc:pg%'`).

- [ ] **Step 9: Run everything**

Run: `cargo test --test unit_scan_kind && CHARCOAL_MODEL_DIR=./models cargo test --features web && DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres && npm --prefix web run build`
Expected: all green. The admitter and slot-lifecycle inline tests still pass because `enqueue_scan`'s signature is unchanged and they never inspect `kind`.

Run the three clippy invocations from Global Constraints. Expected: clean.

- [ ] **Step 10: Commit**

```bash
git add src/db/traits.rs src/db/mod.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs src/web/handlers/admin.rs src/web/handlers/scan.rs src/web/scan_job.rs web/src/lib/types.ts tests/unit_scan_kind.rs tests/db_postgres.rs
git commit -m 'feat(344): scan_queue.kind — refresh rows share the queue without getting in the way

ScanKind {Full, Refresh} on claims and rows; enqueue_refresh_scan touches
only finished rows, a full enqueue upgrades a queued refresh, the ETA
median learns from full scans only, and the 24 h cooldown anchors on
scan_state.last_full_scan_finished_at when the row is a refresh.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 6: The refresh schedule — `users.next_refresh_at` and the admitter tick

**Why:** Spec §4.4: "`users.next_refresh_at`. The refresh tick runs inside the worker role, reusing the admitter's `TICK` (`admitter.rs:53`) — no second lock, no second loop. Defaults: refresh nightly." Two replicas must not double-enqueue: the "claim" is a single `UPDATE … RETURNING` that advances `next_refresh_at` in the same statement that selects the due users, so whichever replica runs it first wins and the other sees nothing due.

**Files:**
- Create: `src/web/refresh.rs`
- Modify: `src/web/mod.rs` (`pub mod refresh;`), `src/db/traits.rs` (two methods), `src/db/queries.rs` + `src/db/sqlite.rs` + `src/db/postgres.rs` (impls), `src/web/admitter.rs:441-497` (`run_admitter`), `:529-545` (`spawn_admitter`), the three test call sites `:889`, `:969`, `:1009`; `src/web/scan_job.rs` (`run_scan`, schedule after success)
- Test: `src/web/refresh.rs` inline tests; `tests/db_postgres.rs` (append)

**Interfaces:**
- Produces (trait):
  ```rust
  /// Set when this user's next refresh is due (RFC3339). Called after a
  /// successful full scan and by the tick itself.
  async fn schedule_refresh(&self, user_did: &str, at_rfc3339: &str) -> Result<()>;
  /// Atomically select every user whose refresh is due at `now` and move
  /// their `next_refresh_at` to `next`. Returns the DIDs claimed. One
  /// statement, so two replicas cannot both claim the same user.
  async fn claim_due_refresh_users(&self, now_rfc3339: &str, next_rfc3339: &str) -> Result<Vec<String>>;
  ```
- Produces (`crate::web::refresh`):
  ```rust
  pub const REFRESH_INTERVAL_ENV: &str = "CHARCOAL_REFRESH_INTERVAL_HOURS";
  pub const DEFAULT_REFRESH_INTERVAL_HOURS: u64 = 24;
  pub const MAX_REFRESH_INTERVAL_HOURS: u64 = 168;
  pub fn parse_refresh_interval(raw: Option<&str>) -> Option<Duration>; // None = disabled
  pub fn refresh_interval_from_env() -> Option<Duration>;
  pub async fn enqueue_due_refreshes(db: &Arc<dyn Database>, now: DateTime<Utc>, interval: Duration) -> usize;
  ```
- `run_admitter(db, launcher, live, wake_rx, tick, cap, refresh: Option<Duration>)`.

- [ ] **Step 1: Write the failing tests**

Create `src/web/refresh.rs` with only the tests module first:

```rust
//! The nightly refresh schedule (#343 §4.4, #344).
//!
//! Runs inside the admitter tick — no second loop, no second lock. Each tick
//! atomically claims every user whose `next_refresh_at` has passed, pushes
//! their next due time out by the interval, and enqueues a `refresh` row for
//! each. The enqueue is idempotent and never touches a queued or running
//! scan (`Database::enqueue_refresh_scan`), so a user mid-scan simply gets
//! their refresh on the next tick after they finish — their
//! `next_refresh_at` has already moved, so "next tick" means "next interval".

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::create_tables;
    use crate::db::sqlite::SqliteDatabase;
    use crate::db::{Database, ScanKind};
    use chrono::{Duration as ChronoDuration, Utc};
    use rusqlite::Connection;
    use std::sync::Arc;

    fn db() -> Arc<dyn Database> {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        Arc::new(SqliteDatabase::new(conn))
    }

    async fn user(db: &Arc<dyn Database>, did: &str, next_refresh_at: Option<&str>) {
        db.upsert_user(did, &format!("{did}.handle")).await.unwrap();
        if let Some(at) = next_refresh_at {
            db.schedule_refresh(did, at).await.unwrap();
        }
    }

    async fn queue_kind(db: &Arc<dyn Database>, did: &str) -> Option<(String, ScanKind)> {
        db.list_scan_queue()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.user_did == did)
            .map(|r| (r.status, r.kind))
    }

    #[test]
    fn interval_knob_defaults_disables_and_clamps() {
        let h = |n: u64| Duration::from_secs(n * 3600);
        assert_eq!(parse_refresh_interval(None), Some(h(24)));
        assert_eq!(parse_refresh_interval(Some("6")), Some(h(6)));
        assert_eq!(parse_refresh_interval(Some("0")), None, "0 disables");
        assert_eq!(parse_refresh_interval(Some("off")), None);
        assert_eq!(parse_refresh_interval(Some("500")), Some(h(168)), "clamped to a week");
        assert_eq!(parse_refresh_interval(Some("abc")), Some(h(24)), "garbage → default");
        assert_eq!(parse_refresh_interval(Some(" 12 ")), Some(h(12)), "trimmed");
    }

    #[tokio::test]
    async fn due_users_are_enqueued_as_refresh_and_rescheduled() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        let future = (now + ChronoDuration::hours(1)).to_rfc3339();
        user(&db, "did:plc:due", Some(&past)).await;
        user(&db, "did:plc:notyet", Some(&future)).await;
        user(&db, "did:plc:never", None).await;

        let n = enqueue_due_refreshes(&db, now, Duration::from_secs(24 * 3600)).await;
        assert_eq!(n, 1);
        assert_eq!(
            queue_kind(&db, "did:plc:due").await,
            Some(("queued".to_string(), ScanKind::Refresh))
        );
        assert_eq!(queue_kind(&db, "did:plc:notyet").await, None);
        assert_eq!(queue_kind(&db, "did:plc:never").await, None);

        // Rescheduled ~24 h out: a second tick a minute later claims nothing.
        let n = enqueue_due_refreshes(&db, now + ChronoDuration::minutes(1), Duration::from_secs(24 * 3600)).await;
        assert_eq!(n, 0, "already rescheduled — no double enqueue");
        // …and is due again after the interval.
        let n = enqueue_due_refreshes(&db, now + ChronoDuration::hours(25), Duration::from_secs(24 * 3600)).await;
        assert_eq!(n, 1);
    }

    /// A user with a queued FULL scan still gets rescheduled (the tick must
    /// not retry them every 30 s) but their row is not downgraded.
    #[tokio::test]
    async fn a_queued_full_scan_is_not_downgraded_but_the_user_is_rescheduled() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        user(&db, "did:plc:busy", Some(&past)).await;
        db.enqueue_scan("did:plc:busy").await.unwrap();

        let n = enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await;
        assert_eq!(n, 1, "claimed and rescheduled");
        assert_eq!(
            queue_kind(&db, "did:plc:busy").await,
            Some(("queued".to_string(), ScanKind::Full))
        );
        let n = enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await;
        assert_eq!(n, 0);
    }

    /// The claim is one UPDATE … RETURNING, so a user is handed out once even
    /// if two ticks race. Simulated by calling twice with the same `now`.
    #[tokio::test]
    async fn a_user_is_claimed_once_per_due_time() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        user(&db, "did:plc:once", Some(&past)).await;
        let a = db
            .claim_due_refresh_users(&now.to_rfc3339(), &(now + ChronoDuration::hours(24)).to_rfc3339())
            .await
            .unwrap();
        let b = db
            .claim_due_refresh_users(&now.to_rfc3339(), &(now + ChronoDuration::hours(24)).to_rfc3339())
            .await
            .unwrap();
        assert_eq!(a, vec!["did:plc:once".to_string()]);
        assert!(b.is_empty());
    }

    /// The direct-SQL shape migration v18 writes (strftime RFC3339, no
    /// fractional seconds) must compare correctly against chrono's RFC3339.
    #[tokio::test]
    async fn migration_style_timestamps_are_due() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn.execute(
            "INSERT INTO users (did, handle, next_refresh_at)
             VALUES ('did:plc:mig', 'mig.handle', strftime('%Y-%m-%dT%H:%M:%S+00:00', 'now', '-1 hour'))",
            [],
        )
        .unwrap();
        let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));
        let n = enqueue_due_refreshes(&db, Utc::now(), Duration::from_secs(3600)).await;
        assert_eq!(n, 1);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web --lib web::refresh`
Expected: compile error — module has no items; `schedule_refresh`/`claim_due_refresh_users` missing.

- [ ] **Step 3: Trait + SQLite + Postgres**

`src/db/traits.rs`, after `update_last_login` (or the last `users`-related method):

```rust
    // --- Refresh schedule (#343 §4.4) ---

    /// Set when this user's next refresh is due. RFC3339. Called after a
    /// successful full scan, and by the tick that claims the user.
    async fn schedule_refresh(&self, user_did: &str, at_rfc3339: &str) -> Result<()>;

    /// Atomically select every user whose `next_refresh_at <= now` and move
    /// it to `next`, returning the DIDs claimed. One statement, so two
    /// replicas ticking at once cannot both claim the same user. Users with
    /// a NULL `next_refresh_at` (never scanned) are never due.
    async fn claim_due_refresh_users(
        &self,
        now_rfc3339: &str,
        next_rfc3339: &str,
    ) -> Result<Vec<String>>;
```

`src/db/queries.rs`:

```rust
pub fn schedule_refresh(conn: &Connection, user_did: &str, at_rfc3339: &str) -> Result<()> {
    conn.execute(
        "UPDATE users SET next_refresh_at = ?2 WHERE did = ?1",
        params![user_did, at_rfc3339],
    )?;
    Ok(())
}

/// `next_refresh_at` is RFC3339 text (always `+00:00`), so `<=` is a
/// lexicographic comparison that agrees with time order — the same argument
/// `scan_queue.enqueued_at` relies on. RETURNING makes the select and the
/// reschedule one statement.
pub fn claim_due_refresh_users(
    conn: &Connection,
    now_rfc3339: &str,
    next_rfc3339: &str,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "UPDATE users SET next_refresh_at = ?2
         WHERE next_refresh_at IS NOT NULL AND next_refresh_at <= ?1
         RETURNING did",
    )?;
    let dids = stmt
        .query_map(params![now_rfc3339, next_rfc3339], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(dids)
}
```

`src/db/sqlite.rs`: the two delegating impls. `src/db/postgres.rs`:

```rust
    async fn schedule_refresh(&self, user_did: &str, at_rfc3339: &str) -> Result<()> {
        sqlx_core::query::query("UPDATE users SET next_refresh_at = $2::timestamptz WHERE did = $1")
            .bind(user_did)
            .bind(at_rfc3339)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn claim_due_refresh_users(
        &self,
        now_rfc3339: &str,
        next_rfc3339: &str,
    ) -> Result<Vec<String>> {
        // One UPDATE … RETURNING: the row-level lock it takes is what stops a
        // second replica's tick from claiming the same user (#277's
        // process-local caveat does not apply here — this is DB-scoped).
        let rows = sqlx_core::query::query(
            "UPDATE users SET next_refresh_at = $2::timestamptz
             WHERE next_refresh_at IS NOT NULL AND next_refresh_at <= $1::timestamptz
             RETURNING did",
        )
        .bind(now_rfc3339)
        .bind(next_rfc3339)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>(0)).collect())
    }
```

- [ ] **Step 4: `src/web/refresh.rs` implementation (above the tests module)**

```rust
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::{error, info, warn};

use crate::db::Database;

pub const REFRESH_INTERVAL_ENV: &str = "CHARCOAL_REFRESH_INTERVAL_HOURS";
/// Nightly (spec §4.4 "refresh nightly").
pub const DEFAULT_REFRESH_INTERVAL_HOURS: u64 = 24;
/// A week. Longer than the widest staleness window (14 d) is pointless —
/// every High/Elevated row would expire before its refresh; a week already
/// lets Low-confidence rows (3 d) lapse, which is the operator's call.
pub const MAX_REFRESH_INTERVAL_HOURS: u64 = 168;

/// `None` disables the tick entirely (`0` or `off`); anything unparseable is
/// the default, so a typo in Railway does not silently switch refreshes off.
pub fn parse_refresh_interval(raw: Option<&str>) -> Option<Duration> {
    let hours = match raw.map(str::trim) {
        None | Some("") => DEFAULT_REFRESH_INTERVAL_HOURS,
        Some("off") => return None,
        Some(s) => match s.parse::<u64>() {
            Ok(0) => return None,
            Ok(n) => n.min(MAX_REFRESH_INTERVAL_HOURS),
            Err(_) => {
                warn!(value = s, "{REFRESH_INTERVAL_ENV} is not a number; using the default");
                DEFAULT_REFRESH_INTERVAL_HOURS
            }
        },
    };
    Some(Duration::from_secs(hours * 3600))
}

pub fn refresh_interval_from_env() -> Option<Duration> {
    parse_refresh_interval(std::env::var(REFRESH_INTERVAL_ENV).ok().as_deref())
}

/// One tick's worth of scheduling: claim the due users, enqueue a refresh
/// for each. Returns how many were claimed (not how many were enqueued —
/// a user mid-scan is claimed and rescheduled but their row is untouched;
/// see the module doc). Errors are logged, never propagated: this runs on
/// the admitter loop, which must keep admitting.
pub async fn enqueue_due_refreshes(
    db: &Arc<dyn Database>,
    now: DateTime<Utc>,
    interval: Duration,
) -> usize {
    let next = now
        + chrono::Duration::from_std(interval).expect("interval is at most a week");
    let due = match db
        .claim_due_refresh_users(&now.to_rfc3339(), &next.to_rfc3339())
        .await
    {
        Ok(d) => d,
        Err(e) => {
            error!(error = %format!("{e:#}"), "refresh tick could not claim due users");
            return 0;
        }
    };
    for did in &due {
        if let Err(e) = db.enqueue_refresh_scan(did).await {
            // Already rescheduled, so this user waits a full interval. Loud
            // on purpose: a persistent enqueue failure is a wedge.
            error!(user_did = %did, error = %format!("{e:#}"), "could not enqueue refresh");
        }
    }
    if !due.is_empty() {
        info!(claimed = due.len(), "refresh tick enqueued due users");
    }
    due.len()
}
```

`src/web/mod.rs`: `pub mod refresh;` (alphabetical, before `scan_job`).

- [ ] **Step 5: Wire the tick and the post-scan schedule**

`src/web/admitter.rs` `run_admitter` (`:441-449`): add a parameter `refresh: Option<Duration>` after `cap`, and at the top of the loop body, before the reclaim:

```rust
        // #343 §4.4: the refresh schedule rides this tick. Disabled (None)
        // when CHARCOAL_REFRESH_INTERVAL_HOURS=0. Runs BEFORE admit so a
        // user claimed this tick starts this tick when a slot is free.
        if let Some(interval) = refresh {
            crate::web::refresh::enqueue_due_refreshes(&db, chrono::Utc::now(), interval).await;
        }
```

Update the doc comment on `run_admitter`. In `spawn_admitter` (`:545`): `run_admitter(db, launcher, live, rx, TICK, scan_concurrency, crate::web::refresh::refresh_interval_from_env())`. The three test spawns (`:889`, `:969`, `:1009`) pass `None` as the new last argument. Add one admitter test proving the wiring:

```rust
    /// The refresh schedule runs on the admitter tick (#343 §4.4) — a due
    /// user is enqueued and admitted by the same loop, with no second task.
    #[tokio::test]
    async fn the_tick_enqueues_and_admits_a_due_refresh() {
        let db = test_db();
        db.upsert_user("did:plc:due", "due.handle").await.unwrap();
        let past = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        db.schedule_refresh("did:plc:due", &past).await.unwrap();

        let (tx, mut rx) = mpsc::channel(4);
        let launcher = Arc::new(RecordingLauncher::notifying(tx));
        let (_wake_tx, wake_rx) = mpsc::channel(1);
        let handle = tokio::spawn(run_admitter(
            db.clone(),
            launcher.clone(),
            LiveScans::new(),
            wake_rx,
            Duration::from_millis(20),
            || 2,
            Some(Duration::from_secs(3600)),
        ));

        let launched = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("admitted within two seconds")
            .expect("channel open");
        assert_eq!(launched, "did:plc:due");
        assert_eq!(launcher.claim_ids().len(), 1);
        handle.abort();
    }
```

(`RecordingLauncher::notifying` sends the launched DID — see `:610`. If it sends something else, assert on what it sends.) Also extend `RecordingLauncher` to record `claim.kind` and assert `ScanKind::Refresh` here — add a `kinds()` accessor mirroring `claim_ids()`.

`src/web/scan_job.rs` `run_scan`, in the same `if result.is_ok()` block Task 5 added:

```rust
        // A full scan (re)starts the refresh clock: the next nightly refresh
        // is one interval from now, not from whenever the user was last
        // scheduled. Disabled interval → never scheduled.
        if let Some(interval) = crate::web::refresh::refresh_interval_from_env() {
            let next = chrono::Utc::now()
                + chrono::Duration::from_std(interval).expect("interval is at most a week");
            if let Err(e) = db.schedule_refresh(user_did, &next.to_rfc3339()).await {
                warn!(error = %format!("{e:#}"), "could not schedule the next refresh");
            }
        }
```

- [ ] **Step 6: Postgres twin**

Append to `tests/db_postgres.rs`:

```rust
#[tokio::test]
async fn test_pg_claim_due_refresh_users_is_once_per_due_time() {
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    let now = chrono::Utc::now();
    for (did, at) in [
        ("did:plc:pgrefresh_due", Some(now - chrono::Duration::hours(1))),
        ("did:plc:pgrefresh_notyet", Some(now + chrono::Duration::hours(1))),
        ("did:plc:pgrefresh_never", None),
    ] {
        db.upsert_user(did, &format!("{did}.handle")).await.unwrap();
        if let Some(at) = at {
            db.schedule_refresh(did, &at.to_rfc3339()).await.unwrap();
        }
    }
    let next = (now + chrono::Duration::hours(24)).to_rfc3339();
    let a = db.claim_due_refresh_users(&now.to_rfc3339(), &next).await.unwrap();
    let b = db.claim_due_refresh_users(&now.to_rfc3339(), &next).await.unwrap();
    assert_eq!(a, vec!["did:plc:pgrefresh_due".to_string()]);
    assert!(b.is_empty(), "claimed once");
    for did in ["did:plc:pgrefresh_due", "did:plc:pgrefresh_notyet", "did:plc:pgrefresh_never"] {
        db.delete_user_data(did).await.unwrap();
    }
}
```

(`delete_user_data` removes the `users` row — confirm with `rg -n "DELETE FROM users" src/db/postgres.rs`; if it does not, delete with raw SQL as `pg_delete_queue_row` does.)

- [ ] **Step 7: Run everything**

Run: `cargo test --features web --lib web::refresh web::admitter && CHARCOAL_MODEL_DIR=./models cargo test --features web && DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres`
Expected: green. Clippy ×3: clean.

- [ ] **Step 8: Commit**

```bash
git add src/web/refresh.rs src/web/mod.rs src/db/traits.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs src/web/admitter.rs src/web/scan_job.rs tests/db_postgres.rs
git commit -m 'feat(344): nightly refresh schedule on the admitter tick

users.next_refresh_at + claim_due_refresh_users (one UPDATE … RETURNING,
so two replicas cannot double-claim); enqueue_due_refreshes runs at the
top of every admitter pass — no second loop, no second lock (spec §4.4).
CHARCOAL_REFRESH_INTERVAL_HOURS: default 24, 0/off disables, clamp 1..=168.
A successful full scan schedules the next refresh one interval out.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 7: `list_refresh_candidates` — candidates from the table, not the network

**Why:** #344's second gap: "the staleness gate is a FILTER on the candidate set, never a SOURCE of candidates." This is the source. Spec §4.4: "high and elevated rows with `valid_until < now() + 2 d`, read from `account_scores`" — plus old-generation rows, or the "no legacy row in any tier list after one nightly" pass cannot be met.

**Tier test:** `threat_score >= 15.0`, not the stored `threat_tier` string — `get_ranked_threats` recomputes the tier from the score, so the UI's "High/Elevated" is the score's, and a threshold change would otherwise leave rows the UI calls Elevated out of the refresh. `15.0` is `ThreatTier::from_score`'s Elevated floor (`src/db/models.rs:177`); expose it as a constant rather than a second literal.

**Files:**
- Modify: `src/db/models.rs` (`ThreatTier::ELEVATED_MIN`), `src/db/traits.rs` (`RefreshCandidate`, method), `src/db/mod.rs` (re-export), `src/db/queries.rs`, `src/db/sqlite.rs`, `src/db/postgres.rs`
- Test: `tests/unit_refresh_candidates.rs` (new), `tests/db_postgres.rs` (append)

**Interfaces:**
- Produces:
  ```rust
  impl ThreatTier { pub const ELEVATED_MIN: f64 = 15.0; }   // from_score uses it
  #[derive(Debug, Clone, PartialEq)]
  pub struct RefreshCandidate { pub did: String, pub handle: String, pub graph_distance: Option<String> }
  async fn list_refresh_candidates(&self, user_did: &str, horizon_days: i64) -> Result<Vec<RefreshCandidate>>;
  ```
  Ordered by `threat_score DESC` so a cost-capped refresh re-scores the most dangerous first. Task 9 consumes this.

- [ ] **Step 1: Write the failing tests**

Create `tests/unit_refresh_candidates.rs`:

```rust
// #344: the refresh job's candidate source. Rows come FROM account_scores —
// High/Elevated (by score), and either expiring within the horizon or
// stamped with an older generation. Nothing else; ordering is most
// dangerous first so a cost-capped refresh spends its budget well.

use charcoal::db::queries::list_refresh_candidates;
use charcoal::db::schema::create_tables;
use charcoal::scoring::generation::{LEGACY_GENERATION, SCORING_GENERATION};
use rusqlite::{params, Connection};

const USER: &str = "did:plc:refreshuser0000000000000";

fn insert(conn: &Connection, user: &str, did: &str, score: f64, valid_for_days: i64, generation: &str, graph: Option<&str>) {
    conn.execute(
        "INSERT INTO account_scores
             (user_did, did, handle, threat_score, threat_tier, scoring_generation, valid_until, graph_distance)
         VALUES (?1, ?2, ?3, ?4, 'x', ?5, datetime('now', ?6), ?7)",
        params![user, did, format!("{did}.handle"), score, generation, format!("{valid_for_days:+} days"), graph],
    )
    .unwrap();
}

#[test]
fn selects_high_and_elevated_rows_that_are_expiring_or_old_generation() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    // Included:
    insert(&conn, USER, "did:plc:high-expiring", 60.0, 1, SCORING_GENERATION, Some("Stranger"));
    insert(&conn, USER, "did:plc:elevated-legacy", 20.0, 10, LEGACY_GENERATION, None);
    insert(&conn, USER, "did:plc:high-expired", 40.0, -3, SCORING_GENERATION, Some("Follows you"));
    // Excluded:
    insert(&conn, USER, "did:plc:high-fresh", 50.0, 10, SCORING_GENERATION, None); // not expiring
    insert(&conn, USER, "did:plc:watch-legacy", 10.0, 1, LEGACY_GENERATION, None); // below Elevated
    insert(&conn, "did:plc:otheruser", "did:plc:high-other", 60.0, 1, SCORING_GENERATION, None);
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, threat_tier, scoring_generation, valid_until)
         VALUES (?1, 'did:plc:na', 'na.handle', 'NotAssessed', ?2, datetime('now', '-1 days'))",
        params![USER, LEGACY_GENERATION],
    )
    .unwrap(); // NULL score: never a refresh candidate

    let rows = list_refresh_candidates(&conn, USER, 2).unwrap();
    let dids: Vec<&str> = rows.iter().map(|r| r.did.as_str()).collect();
    assert_eq!(
        dids,
        ["did:plc:high-expiring", "did:plc:high-expired", "did:plc:elevated-legacy"],
        "most dangerous first"
    );
    assert_eq!(rows[0].handle, "did:plc:high-expiring.handle");
    assert_eq!(rows[0].graph_distance.as_deref(), Some("Stranger"));
    assert_eq!(rows[2].graph_distance, None);
}

#[test]
fn horizon_zero_means_only_already_expired_or_old_generation() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    insert(&conn, USER, "did:plc:soon", 60.0, 1, SCORING_GENERATION, None);
    insert(&conn, USER, "did:plc:gone", 60.0, -1, SCORING_GENERATION, None);
    let dids: Vec<String> = list_refresh_candidates(&conn, USER, 0)
        .unwrap()
        .into_iter()
        .map(|r| r.did)
        .collect();
    assert_eq!(dids, ["did:plc:gone"]);
}

#[test]
fn elevated_floor_matches_from_score() {
    use charcoal::db::models::ThreatTier;
    assert_eq!(ThreatTier::from_score(ThreatTier::ELEVATED_MIN), ThreatTier::Elevated);
    assert_eq!(ThreatTier::from_score(ThreatTier::ELEVATED_MIN - 0.01), ThreatTier::Watch);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test unit_refresh_candidates`
Expected: compile error.

- [ ] **Step 3: Implement**

`src/db/models.rs`, inside `impl ThreatTier` before `from_score`:

```rust
    /// The Elevated floor. The refresh job (#344) selects by score with this
    /// constant so it agrees with `from_score`, which the tier lists use.
    pub const ELEVATED_MIN: f64 = 15.0;
```

and in `from_score` replace `s if s >= 15.0 => ThreatTier::Elevated,` with `s if s >= Self::ELEVATED_MIN => ThreatTier::Elevated,`.

`src/db/traits.rs`, next to `ScanQueueRow`:

```rust
/// One `account_scores` row the refresh job should re-score (#344).
///
/// Only what `run_refresh` needs to build a `CandidateInput`: the handle for
/// fetching, the stored graph distance so the refreshed score keeps the
/// relationship context a full scan computed (a refresh never calls
/// `classify_relationships`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshCandidate {
    pub did: String,
    pub handle: String,
    /// `GraphDistance::as_str()` of the stored value, if any.
    pub graph_distance: Option<String>,
}
```

and in the trait, after `count_expired`:

```rust
    /// This user's High/Elevated rows (by score, `ThreatTier::ELEVATED_MIN`)
    /// that expire within `horizon_days` or were scored under an older
    /// generation — the refresh job's candidate set, most dangerous first
    /// (#344, spec §4.4). NULL-score rows (NotAssessed) are never included.
    async fn list_refresh_candidates(
        &self,
        user_did: &str,
        horizon_days: i64,
    ) -> Result<Vec<RefreshCandidate>>;
```

`src/db/mod.rs`: re-export `RefreshCandidate`.

`src/db/queries.rs`:

```rust
/// See `Database::list_refresh_candidates`. `valid_until IS NULL` counts as
/// expired (SQLite could not make the column NOT NULL — migration v18).
pub fn list_refresh_candidates(
    conn: &Connection,
    user_did: &str,
    horizon_days: i64,
) -> Result<Vec<RefreshCandidate>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, graph_distance FROM account_scores
         WHERE user_did = ?1
           AND threat_score >= ?2
           AND (valid_until IS NULL
                OR datetime(valid_until) <= datetime('now', ?3)
                OR scoring_generation != ?4)
         ORDER BY threat_score DESC, did",
    )?;
    let rows = stmt
        .query_map(
            params![
                user_did,
                ThreatTier::ELEVATED_MIN,
                format!("+{horizon_days} days"),
                SCORING_GENERATION
            ],
            |r| {
                Ok(RefreshCandidate {
                    did: r.get(0)?,
                    handle: r.get(1)?,
                    graph_distance: r.get(2)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
```

(`RefreshCandidate` from `super::traits`; `ThreatTier` is already imported in queries.rs for `get_ranked_threats`.) `src/db/sqlite.rs`: delegate. `src/db/postgres.rs`:

```rust
    async fn list_refresh_candidates(
        &self,
        user_did: &str,
        horizon_days: i64,
    ) -> Result<Vec<RefreshCandidate>> {
        let rows = sqlx_core::query::query(
            "SELECT did, handle, graph_distance FROM account_scores
             WHERE user_did = $1
               AND threat_score >= $2
               AND (valid_until <= NOW() + make_interval(days => $3)
                    OR scoring_generation <> $4)
             ORDER BY threat_score DESC, did",
        )
        .bind(user_did)
        .bind(ThreatTier::ELEVATED_MIN)
        .bind(i32::try_from(horizon_days).context("horizon_days exceeds i32 range")?)
        .bind(SCORING_GENERATION)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| RefreshCandidate {
                did: r.get(0),
                handle: r.get(1),
                graph_distance: r.get(2),
            })
            .collect())
    }
```

- [ ] **Step 4: Postgres twin test**

Append to `tests/db_postgres.rs` a test with the same six rows as `selects_high_and_elevated_rows_that_are_expiring_or_old_generation`, inserted with `NOW() + make_interval(days => $n)` for `valid_until`, asserting the same ordered DID list and the same `graph_distance` values, and deleting the rows afterwards (`DELETE FROM account_scores WHERE user_did IN ($1, 'did:plc:otheruser_pg')`). Use DIDs prefixed `did:plc:pgrc_` so the cleanup is unambiguous.

- [ ] **Step 5: Run**

Run: `cargo test --test unit_refresh_candidates && cargo test --test unit_scoring && DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres refresh_candidates`
Expected: green. Clippy ×3 clean.

- [ ] **Step 6: Commit**

```bash
git add src/db/models.rs src/db/traits.rs src/db/mod.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs tests/unit_refresh_candidates.rs tests/db_postgres.rs
git commit -m 'feat(344): list_refresh_candidates — High/Elevated rows expiring or old-generation, from account_scores

The staleness gate becomes a SOURCE: rows with threat_score >= ThreatTier::ELEVATED_MIN
whose valid_until is within the horizon or whose generation is not current,
most dangerous first. Both backends; no caller yet.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 8: Extract the shared scan setup (no behaviour change)

**Why:** `run_refresh` (Task 9) needs the same scorer bundle, protected-post embeddings, pile-on set and direct-pair loading that `run_scan` (`src/web/scan_job.rs:672-1130`) and `amplification::run` (`src/pipeline/amplification.rs:340-366`) build inline. Extracting them first, with the existing tests as the safety net, keeps Task 9 a pure composition.

**Files:**
- Modify: `src/pipeline/amplification.rs:340-366` → `pub async fn direct_pairs_for(...)`
- Modify: `src/web/scan_job.rs:693-735` → `ScanScorers` + `build_scan_scorers`; `:1108-1128` → `record_scan_cache_stats`; `:875-900` → `embed_protected_posts`; `:1045-1053` → `pile_on_dids`
- Test: existing (`cargo test --features web`), plus one unit test for `direct_pairs_for`

**Interfaces:**
- Produces:
  ```rust
  // src/pipeline/amplification.rs
  /// Deduplicated (original, amplifier) text pairs from this user's stored
  /// events for `amplifier_did`. Empty on a read error (logged), so a failed
  /// read degrades to follower-mode NLI instead of aborting the scan.
  pub async fn direct_pairs_for(db: &Arc<dyn Database>, user_did: &str, amplifier_did: &str) -> Vec<(String, String)>;

  // src/web/scan_job.rs
  pub(crate) struct ScanScorers {
      pub scorer: crate::toxicity::ensemble::TwoStageToxicityScorer,
      pub onnx_stats: Arc<crate::observability::cache_stats::CacheStats>,
      pub classifier_stats: Arc<crate::observability::cache_stats::CacheStats>,
  }
  pub(crate) fn build_scan_scorers(models: &ScanModels, db: &Arc<dyn Database>) -> anyhow::Result<ScanScorers>;
  pub(crate) async fn record_scan_cache_stats(db: &dyn Database, user_did: &str, scorers: &ScanScorers);
  pub(crate) async fn embed_protected_posts(client: &PublicAtpClient, embedder: &crate::topics::embeddings::SentenceEmbedder, actor_handle: &str) -> Option<Vec<(String, Vec<f64>)>>;
  pub(crate) async fn pile_on_dids(db: &dyn Database, user_did: &str) -> anyhow::Result<HashSet<String>>;
  ```

- [ ] **Step 1: Write the failing test for `direct_pairs_for`**

Append to `tests/unit_discovery.rs` (or `tests/unit_likes.rs` — whichever already builds an `Arc<dyn Database>` over in-memory SQLite and inserts amplification events; check with `rg -n "insert_amplification_event" tests/`). If neither does, create `tests/unit_direct_pairs.rs`:

```rust
// #344 Task 8: the amplifier direct-pair loader, extracted from
// amplification::run so the refresh job can rebuild NLI pairs for a
// re-scored amplifier from stored events.

use std::sync::Arc;

use charcoal::db::schema::create_tables;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::pipeline::amplification::direct_pairs_for;
use rusqlite::Connection;

const USER: &str = "did:plc:pairsuser000000000000000";

#[tokio::test]
async fn pairs_are_deduplicated_and_empty_text_is_skipped() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));

    for (orig, amp) in [
        (Some("hello"), Some("lol no")),
        (Some("hello"), Some("lol no")), // duplicate event
        (Some("hello"), Some("")),       // empty commentary: not a pair
        (None, Some("orphan")),          // no original text: not a pair
        (Some("second post"), Some("also bad")),
    ] {
        db.insert_amplification_event(
            USER,
            "quote",
            "did:plc:amp",
            "amp.handle",
            "at://did:plc:me/app.bsky.feed.post/1",
            Some("at://did:plc:amp/app.bsky.feed.post/x"),
            amp,
            orig,
            None,
        )
        .await
        .unwrap();
    }

    let pairs = direct_pairs_for(&db, USER, "did:plc:amp").await;
    assert_eq!(
        pairs,
        vec![
            ("hello".to_string(), "lol no".to_string()),
            ("second post".to_string(), "also bad".to_string()),
        ]
    );
    assert!(direct_pairs_for(&db, USER, "did:plc:nobody").await.is_empty());
}
```

Check `insert_amplification_event`'s exact parameter order in `src/db/traits.rs:~590` before writing the call (it has `original_post_text` and `context_score` after `amplifier_text`); adjust the argument list to match.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test unit_direct_pairs`
Expected: compile error — `direct_pairs_for` missing.

- [ ] **Step 3: Extract `direct_pairs_for`**

In `src/pipeline/amplification.rs`, above `pub async fn run`:

```rust
/// Direct (original, amplifier) text pairs for one amplifier, from this
/// user's stored events. Deduplicated across scans (the same event can be
/// recorded more than once); pairs with an empty side are dropped. A read
/// error logs and returns empty rather than failing the scan — the account
/// then scores on the follower path (NLI gated at raw ≥ 8.0) instead of
/// Mode A, which is degraded, not wrong.
pub async fn direct_pairs_for(
    db: &Arc<dyn Database>,
    user_did: &str,
    amplifier_did: &str,
) -> Vec<(String, String)> {
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    match db.get_events_by_amplifier(user_did, amplifier_did).await {
        Ok(db_events) => {
            for ev in db_events {
                if let (Some(orig), Some(amp)) = (ev.original_post_text, ev.amplifier_text) {
                    if !orig.is_empty()
                        && !amp.is_empty()
                        && seen_pairs.insert((orig.clone(), amp.clone()))
                    {
                        pairs.push((orig, amp));
                    }
                }
            }
        }
        Err(e) => {
            warn!(
                amplifier_did = %amplifier_did,
                error = %e,
                "Failed to load stored events for amplifier; direct NLI pairs dropped"
            );
        }
    }
    pairs
}
```

and replace the block at `:340-366` inside the amplifier loop with `let pairs = direct_pairs_for(db, user_did, did).await;`.

- [ ] **Step 4: Extract the four `scan_job.rs` helpers**

In `src/web/scan_job.rs`, above `run_scan`:

```rust
/// The two-stage scorer plus its cache counters, built once per scan.
pub(crate) struct ScanScorers {
    pub scorer: crate::toxicity::ensemble::TwoStageToxicityScorer,
    pub onnx_stats: Arc<crate::observability::cache_stats::CacheStats>,
    pub classifier_stats: Arc<crate::observability::cache_stats::CacheStats>,
}

/// Wrap the boot-loaded models in the #343 Phase 1 cache decorators and the
/// two-stage ensemble. Errors only when no Stage-2 classifier is configured
/// (`build_from_env`) — there is deliberately no ONNX-only fallback.
pub(crate) fn build_scan_scorers(
    models: &ScanModels,
    db: &Arc<dyn Database>,
) -> anyhow::Result<ScanScorers> {
    // #343 Phase 1: stage-1 / clean-pass ONNX scores are cached by text hash
    // across users. Hit/miss counts are persisted at the end of the scan.
    let onnx_stats = Arc::new(crate::observability::cache_stats::CacheStats::default());
    let primary_scorer: Box<dyn ToxicityScorer> =
        Box::new(crate::toxicity::cached::CachedToxicityScorer::new(
            Box::new(Arc::clone(&models.toxicity)),
            Arc::clone(db),
            crate::toxicity::onnx::ONNX_MODEL_ID,
            Arc::clone(&onnx_stats),
        ));

    // Wrap in the two-stage scorer. ONNX runs as a clean-pass filter
    // (< 0.10 = cleared); posts at or above the threshold are sent to the
    // configured Stage-2 classifier (CHARCOAL_CLASSIFIER) for a binary verdict.
    // The classifier is required — build_from_env errors (and the scan fails
    // loudly) if unconfigured; there is no silent ONNX-only fallback.
    // #343 Phase 1: stage-2 verdicts are cached by (text hash, model, policy).
    let classifier_stats = Arc::new(crate::observability::cache_stats::CacheStats::default());
    let classifier: Arc<dyn crate::toxicity::classifier::ToxicityClassifier> =
        Arc::new(crate::toxicity::cached_classifier::CachedClassifier::new(
            crate::toxicity::classifier::build_from_env()?,
            Arc::clone(db),
            Arc::clone(&classifier_stats),
        ));
    info!(
        backend = classifier.name(),
        "Stage-2 toxicity classifier loaded — two-stage scoring enabled"
    );
    // Scan-start banner metric so log aggregation can attribute which backend
    // produced this scan's verdicts.
    crate::observability::classifier_metrics::record_backend_selected(classifier.name());

    // Concrete scorer (not boxed as `dyn`): the phased scan pipeline (#208)
    // needs the `TwoStageToxicityScorer`'s inherent `classifier()` accessor and
    // its `CleanPassScorer` impl, both of which a `dyn ToxicityScorer` erases.
    let scorer = crate::toxicity::ensemble::TwoStageToxicityScorer::new(primary_scorer, classifier);
    Ok(ScanScorers {
        scorer,
        onnx_stats,
        classifier_stats,
    })
}

/// Persist both scorers' hit/miss counters. Best-effort: warns, never fails.
pub(crate) async fn record_scan_cache_stats(db: &dyn Database, user_did: &str, s: &ScanScorers) {
    for (prefix, stats) in [("onnx", &s.onnx_stats), ("classifier", &s.classifier_stats)] {
        if let Err(e) =
            crate::observability::cache_stats::record_cache_stats(db, user_did, prefix, stats).await
        {
            tracing::warn!(error = %e, prefix, "could not record cache stats");
        }
    }
}

/// Per-post embeddings of the protected user's 50 most recent posts, for
/// follower NLI pair matching. `None` when the feed or the embedder fails —
/// callers then run followers without inferred pairs, as today.
pub(crate) async fn embed_protected_posts(
    client: &PublicAtpClient,
    embedder: &crate::topics::embeddings::SentenceEmbedder,
    actor_handle: &str,
) -> Option<Vec<(String, Vec<f64>)>> {
    let pp_texts: Vec<String> = crate::bluesky::posts::fetch_recent_posts(client, actor_handle, 50)
        .await
        .unwrap_or_default()
        .iter()
        .map(|p| p.text.clone())
        .collect();
    match embedder.embed_batch(&pp_texts).await {
        Ok(embeddings) => Some(pp_texts.into_iter().zip(embeddings).collect()),
        Err(e) => {
            warn!(error = %e, "Failed to embed protected posts for NLI pairs");
            None
        }
    }
}

/// DIDs in this user's pile-on set, from stored events.
pub(crate) async fn pile_on_dids(db: &dyn Database, user_did: &str) -> anyhow::Result<HashSet<String>> {
    let pile_on_refs = db.get_events_for_pile_on(user_did).await?;
    Ok(detect_pile_on_participants(
        &pile_on_refs
            .iter()
            .map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str()))
            .collect::<Vec<_>>(),
    ))
}
```

These bodies are the existing `run_scan` code (`:693-735`, `:875-900`, `:1045-1053`, `:1108-1128`) moved, not rewritten — delete the originals. `run_scan` then calls: `let scorers = build_scan_scorers(&models, &db)?; let scorer = &scorers.scorer;` (the pipeline call passes `Some(scorer)`), `let protected_posts_with_embeddings = match (&embedder, &nli_scorer) { (Some(emb), Some(_)) => embed_protected_posts(&client, emb, actor_handle).await, _ => None };`, `let pile_on_dids = pile_on_dids(db.as_ref(), user_did).await?;`, and `record_scan_cache_stats(db.as_ref(), user_did, &scorers).await;` in place of the two blocks. Keep every `set_progress` call where it is.

- [ ] **Step 5: Run the full suite — this task's contract is "nothing changed"**

Run: `cargo test --test unit_direct_pairs && CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:|test result"`
Expected: no `SKIP:` lines; every `test result: ok`. Clippy ×3 clean.

- [ ] **Step 6: Commit**

```bash
git add src/pipeline/amplification.rs src/web/scan_job.rs tests/unit_direct_pairs.rs
git commit -m 'refactor(344): extract scan setup helpers for reuse by the refresh job

direct_pairs_for (amplification.rs), ScanScorers/build_scan_scorers,
record_scan_cache_stats, embed_protected_posts, pile_on_dids (scan_job.rs).
Bodies moved verbatim; run_scan behaviour unchanged.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 9: `run_refresh` and kind dispatch

**Why:** The job itself. Reads candidates from Task 7, builds `CandidateInput`s with Task 8's helpers, runs the unchanged `run_phased_scan` through the cached fetcher, records the feed hit rate the Phase 2 pass number reads, and reports through the same `ScanManager`/queue lifecycle as a full scan.

**Guard:** `run_phased_scan` resumes from the `scan_phase` marker. If a full scan was cost-capped and left the marker at `burst`/`finalize`, a refresh entering the pipeline would resume **that** scan's staged rows and ignore its own candidates. So: if the marker is `burst` or `finalize`, the refresh is deferred (logged, row finishes `done` with a "deferred" message) and the user's `next_refresh_at` has already moved a full interval — by then the user's own re-run has drained the resume, or it is a wedge someone should see.

**Files:**
- Create: `src/web/refresh_scan.rs`
- Modify: `src/web/mod.rs` (`pub mod refresh_scan;`), `src/web/scan_job.rs:482-520` (`launch_scan` takes `kind`), `src/web/admitter.rs:497-520` (launcher passes `claim.kind`)
- Test: `src/web/refresh_scan.rs` inline tests (pure functions), `src/web/admitter.rs` (launcher records kinds — from Task 6)

**Interfaces:**
- Consumes: `Database::list_refresh_candidates` (7), `direct_pairs_for`, `build_scan_scorers`, `record_scan_cache_stats`, `embed_protected_posts`, `pile_on_dids` (8), `ScanKind` (5), `CachedPostFetcher`/`AtpPostFetcher`/`PhasedScanDeps`/`run_phased_scan` (existing, see `src/pipeline/amplification.rs:520-556` for the exact deps construction to copy), `set_progress`/`finish_scan` (`scan_job.rs`, make both `pub(crate)`).
- Produces:
  ```rust
  pub const REFRESH_HORIZON_DAYS: i64 = 2;
  pub fn refresh_is_deferred(scan_phase_marker: Option<&str>) -> bool;
  pub fn to_candidate(row: &RefreshCandidate, pairs: Vec<(String, String)>, pile_on: &HashSet<String>) -> CandidateInput;
  pub(crate) async fn run_refresh(config: Arc<Config>, db: Arc<dyn Database>, models: Arc<ScanModels>, scan_manager: Arc<RwLock<ScanManager>>, user_did: &str, actor_handle: &str, claim_id: &str) -> anyhow::Result<()>;
  ```
  `scan_state` keys written: `refresh_last_run_at` (RFC3339), `refresh_candidates`, `refresh_scored`, `refresh_feed_cache_hits`, `refresh_feed_cache_misses` (via `record_cache_stats(db, user_did, "refresh_feed", …)`), plus `onnx_cache_*`/`classifier_cache_*` through `record_scan_cache_stats`.
  `pub(crate) fn launch_scan(state, user_did, actor_handle, kind: ScanKind, slot, live)`.

- [ ] **Step 1: Write the failing pure-function tests**

Create `src/web/refresh_scan.rs` with the module doc and tests:

```rust
//! The refresh scan (#343 §4.4, #344): re-score this user's High/Elevated
//! rows that are about to expire or predate the current generation.
//!
//! What it shares with a full scan: the models, the two-stage scorer and
//! its caches, the fingerprint (read, never rebuilt), `run_phased_scan`,
//! the queue slot, the fencing token and the status entry. What it does
//! not do: Constellation, follower expansion, `classify_relationships` —
//! candidates and their graph distance come from `account_scores`.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bluesky::relationships::GraphDistance;
    use crate::db::RefreshCandidate;
    use std::collections::HashSet;

    #[test]
    fn a_resumable_full_scan_defers_the_refresh() {
        assert!(!refresh_is_deferred(None));
        assert!(!refresh_is_deferred(Some("done")));
        assert!(!refresh_is_deferred(Some("gather")), "a stale gather marker is wiped by a fresh start, as today");
        assert!(refresh_is_deferred(Some("burst")));
        assert!(refresh_is_deferred(Some("finalize")));
        // Unknown markers: run_phased_scan bails on them; deferring here means
        // the failure surfaces on the user's own scan, not a background job.
        assert!(refresh_is_deferred(Some("garbage")));
    }

    #[test]
    fn candidates_keep_graph_distance_pairs_and_pile_on() {
        let row = RefreshCandidate {
            did: "did:plc:a".into(),
            handle: "a.handle".into(),
            graph_distance: Some("Follows you".into()),
        };
        let pile_on: HashSet<String> = ["did:plc:a".to_string()].into();
        let c = to_candidate(&row, vec![("o".into(), "r".into())], &pile_on);
        assert_eq!(c.account_did, "did:plc:a");
        assert_eq!(c.account_handle, "a.handle");
        assert!(c.is_pile_on);
        assert_eq!(c.graph_distance, Some(GraphDistance::InboundFollow));
        assert_eq!(c.direct_pairs, Some(vec![("o".to_string(), "r".to_string())]));
    }

    /// No stored pairs ⇒ follower path (`direct_pairs: None`, NLI gated at
    /// raw ≥ 8.0), NOT `Some(vec![])`, which finalize would treat as an
    /// amplifier with nothing to say and skip NLI entirely.
    #[test]
    fn no_pairs_means_follower_mode_and_unparseable_distance_is_none() {
        let row = RefreshCandidate {
            did: "did:plc:b".into(),
            handle: "b.handle".into(),
            graph_distance: Some("not a distance".into()),
        };
        let c = to_candidate(&row, vec![], &HashSet::new());
        assert_eq!(c.direct_pairs, None);
        assert_eq!(c.graph_distance, None);
        assert!(!c.is_pile_on);
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web --lib web::refresh_scan`
Expected: compile error.

- [ ] **Step 3: Implement the pure functions and `run_refresh`**

Above the tests module in `src/web/refresh_scan.rs`:

```rust
use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::bluesky::client::PublicAtpClient;
use crate::bluesky::relationships::GraphDistance;
use crate::config::Config;
use crate::db::{Database, RefreshCandidate};
use crate::observability::cache_stats::{record_cache_stats, CacheStats};
use crate::pipeline::scan_phases::feed_cache::CachedPostFetcher;
use crate::pipeline::scan_phases::gather::AtpPostFetcher;
use crate::pipeline::scan_phases::staging::ScanPhase;
use crate::pipeline::scan_phases::{burst, run_phased_scan, CandidateInput, PhasedScanDeps};
use crate::scoring::threat::ThreatWeights;
use crate::topics::fingerprint::TopicFingerprint;
use crate::toxicity::clean_pass::CleanPassScorer;
use crate::toxicity::traits::ToxicityScorer;
use crate::web::scan_job::{
    build_scan_scorers, embed_protected_posts, finish_scan, pile_on_dids, record_scan_cache_stats,
    set_progress, ScanManager, ScanModels, WebScanPhase,
};

/// Rows whose `valid_until` is within this many days are refreshed early
/// (spec §4.4: "valid_until < now() + 2 d"), so the nightly job catches a
/// row before it expires rather than the night after.
pub const REFRESH_HORIZON_DAYS: i64 = 2;

/// True when entering `run_phased_scan` would resume someone else's work.
/// `burst`/`finalize` markers mean a cost-capped full scan is waiting for
/// its own re-run; an unknown marker makes `run_phased_scan` bail, which
/// should happen on the user's scan, not a background one.
pub fn refresh_is_deferred(scan_phase_marker: Option<&str>) -> bool {
    match scan_phase_marker.map(ScanPhase::from_value) {
        None => false,
        Some(Some(ScanPhase::Burst)) | Some(Some(ScanPhase::Finalize)) => true,
        Some(Some(ScanPhase::Gather)) | Some(Some(ScanPhase::Done)) => false,
        Some(None) => true,
    }
}

/// A stored row becomes a pipeline candidate. `pairs` non-empty ⇒ amplifier
/// (Mode A NLI); empty ⇒ follower path. An unparseable stored distance is
/// `None` (unclassified), never a guess.
pub fn to_candidate(
    row: &RefreshCandidate,
    pairs: Vec<(String, String)>,
    pile_on: &HashSet<String>,
) -> CandidateInput {
    CandidateInput {
        account_did: row.did.clone(),
        account_handle: row.handle.clone(),
        is_pile_on: pile_on.contains(&row.did),
        direct_pairs: if pairs.is_empty() { None } else { Some(pairs) },
        graph_distance: row.graph_distance.as_deref().and_then(GraphDistance::from_str),
    }
}

pub(crate) async fn run_refresh(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    models: Arc<ScanModels>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: &str,
    actor_handle: &str,
    claim_id: &str,
) -> anyhow::Result<()> {
    let result = run_refresh_inner(&config, &db, &models, &scan_manager, user_did, actor_handle, claim_id).await;
    finish_scan(&scan_manager, user_did, claim_id, result).await
}

async fn run_refresh_inner(
    config: &Config,
    db: &Arc<dyn Database>,
    models: &ScanModels,
    scan_manager: &Arc<RwLock<ScanManager>>,
    user_did: &str,
    actor_handle: &str,
    claim_id: &str,
) -> anyhow::Result<(usize, usize, bool)> {
    // Never resume a cost-capped full scan from a background job.
    let marker = db.get_scan_state(user_did, "scan_phase").await?;
    if refresh_is_deferred(marker.as_deref()) {
        warn!(user_did, marker = ?marker, "refresh deferred: a full scan is mid-resume");
        set_progress(scan_manager, user_did, claim_id, WebScanPhase::Done,
            "Refresh deferred — a previous scan is waiting to resume; run a scan to finish it").await;
        return Ok((0, 0, false));
    }

    set_progress(scan_manager, user_did, claim_id, WebScanPhase::Fingerprint,
        "Loading topic fingerprint…").await;
    // Read, never rebuilt: fingerprint age is a full-scan concern (#342).
    let Some((json, _, _)) = db.get_fingerprint(user_did).await? else {
        anyhow::bail!("no topic fingerprint — run a full scan first");
    };
    let fingerprint: TopicFingerprint =
        serde_json::from_str(&json).context("stored fingerprint is unreadable")?;
    let protected_embedding = db.get_embedding(user_did).await?;
    let protected_topic_centroids: Vec<Vec<f64>> = db
        .get_topic_centroids(user_did)
        .await?
        .iter()
        .map(|c| c.centroid.clone())
        .collect();

    let rows = db.list_refresh_candidates(user_did, REFRESH_HORIZON_DAYS).await?;
    db.set_scan_state(user_did, "refresh_candidates", &rows.len().to_string()).await?;
    if rows.is_empty() {
        info!(user_did, "refresh: nothing due");
        set_progress(scan_manager, user_did, claim_id, WebScanPhase::Done,
            "Refresh complete: nothing was due").await;
        db.set_scan_state(user_did, "refresh_last_run_at", &chrono::Utc::now().to_rfc3339()).await?;
        return Ok((0, 0, false));
    }

    set_progress(scan_manager, user_did, claim_id, WebScanPhase::LoadingModels,
        "Loading models…").await;
    let scorers = build_scan_scorers(models, db)?;
    let client = PublicAtpClient::new(&config.public_api_url)?;
    let protected_posts_with_embeddings =
        embed_protected_posts(&client, &models.embedder, actor_handle).await;
    let pile_on = pile_on_dids(db.as_ref(), user_did).await?;
    let median_engagement = db.get_median_engagement(user_did).await?;

    let mut candidates = Vec::with_capacity(rows.len());
    for row in &rows {
        let pairs = crate::pipeline::amplification::direct_pairs_for(db, user_did, &row.did).await;
        candidates.push(to_candidate(row, pairs, &pile_on));
    }

    set_progress(scan_manager, user_did, claim_id, WebScanPhase::Scoring,
        &format!("Refreshing {} High/Elevated scores…", candidates.len())).await;

    // Same deps construction as amplification::run (src/pipeline/amplification.rs:520-556).
    let source = AtpPostFetcher { client: &client };
    let feed_stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(db), Arc::clone(&feed_stats));
    let scorer = &scorers.scorer;
    let classifier = scorer.classifier();
    let weights = ThreatWeights::default();
    let deps = PhasedScanDeps {
        fetcher: &fetcher,
        scorer: scorer as &dyn ToxicityScorer,
        clean_pass: scorer as &dyn CleanPassScorer,
        classifier: &classifier,
        protected_fingerprint: &fingerprint,
        weights: &weights,
        embedder: Some(&*models.embedder),
        protected_embedding: protected_embedding.as_deref(),
        protected_topic_centroids: Some(&protected_topic_centroids),
        nli_scorer: Some(&*models.nli),
        protected_posts_with_embeddings: protected_posts_with_embeddings.as_deref(),
        data_dir: Some(config.data_dir()),
        median_engagement,
        gather_concurrency: 8,
        burst_concurrency: burst::burst_concurrency(),
        burst_batch: burst::burst_batch(),
    };

    let summary = run_phased_scan(db, user_did, &candidates, &deps).await?;

    // The Phase 2 pass number: the refresh set should hit the 24 h feed
    // snapshot cache ≥ 80 % (spec §6). Read from scan_state, not logs.
    if let Err(e) = record_cache_stats(db.as_ref(), user_did, "refresh_feed", &feed_stats).await {
        warn!(error = %e, "could not record refresh feed cache stats");
    }
    record_scan_cache_stats(db.as_ref(), user_did, &scorers).await;
    db.set_scan_state(user_did, "refresh_scored", &summary.accounts_scored.to_string()).await?;
    db.set_scan_state(user_did, "refresh_last_run_at", &chrono::Utc::now().to_rfc3339()).await?;

    Ok((0, summary.accounts_scored, summary.degraded))
}
```

Check and adapt against the real code as you write (the reviewer will): `AtpPostFetcher`'s field name and whether it borrows or owns the client (`src/pipeline/scan_phases/gather.rs`); the exact `PhasedScanDeps` field list (`src/pipeline/scan_phases/mod.rs:76-110`); `TwoStageToxicityScorer::classifier()`'s return type; `CleanPassScorer`'s path (`rg -n "trait CleanPassScorer" src`); `WebScanPhase` variant names (`rg -n "pub enum WebScanPhase" -A12 src/web/scan_job.rs`); and that `set_progress`, `finish_scan`, `ScanModels`, `WebScanPhase` are `pub(crate)` (make them so). The literal `8` for `gather_concurrency` matches `scan_job.rs:1092` today; Phase 3 replaces both with `gather_permits`.

`finish_scan`'s success message says "N events, M accounts scored" — for a refresh `events` is 0, which reads oddly. Add a `kind`-aware message: give `record_scan_outcome` a `label: &str` parameter (`"Completed"` / `"Refresh complete"`) threaded through `finish_scan(…, label)`; `run_scan` passes `"Completed"`, `run_refresh` passes `"Refresh complete"`. Two-line change; keep the existing slot-lifecycle tests compiling by updating their `finish_scan` calls if any exist (`rg -n "finish_scan(" src/web`).

`src/web/mod.rs`: `pub mod refresh_scan;`.

- [ ] **Step 4: Dispatch on kind**

`src/web/scan_job.rs` `launch_scan` (`:482-520`): add `kind: crate::db::ScanKind` after `actor_handle`; replace `let scan = run_scan(...)` with

```rust
        let scan = async {
            match kind {
                crate::db::ScanKind::Full => {
                    run_scan(config, db.clone(), models, scan_manager.clone(), &did, &handle, &claim_id).await
                }
                crate::db::ScanKind::Refresh => {
                    crate::web::refresh_scan::run_refresh(
                        config, db.clone(), models, scan_manager.clone(), &did, &handle, &claim_id,
                    )
                    .await
                }
            }
        };
```

(`config`, `models` are moved into whichever branch runs — both are `Arc`s already cloned at the top of `launch_scan`; if the borrow checker objects to a move inside a `match` arm within `async`, clone them into two locals before the block.) `src/web/admitter.rs` `AppStateLauncher::launch` (`:509-518`): pass `claim.kind` as the new argument. The admin manual-trigger path (`handlers/admin.rs:301`) and `handlers/access.rs:245` call `enqueue_scan` (full) and are unchanged. Log `kind = %claim.kind.as_str()` in the `info!("admitting queued scan")` line at `admitter.rs:406`.

- [ ] **Step 5: Run**

Run: `cargo test --features web --lib web::refresh_scan web::admitter web::scan_job && CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:|test result" && cargo build --features postgres`
Expected: green, no `SKIP:`. Clippy ×3 clean.

- [ ] **Step 6: Commit**

```bash
git add src/web/refresh_scan.rs src/web/mod.rs src/web/scan_job.rs src/web/admitter.rs
git commit -m 'feat(344): run_refresh — re-score expiring High/Elevated rows through the normal pipeline

A refresh claim runs run_refresh: fingerprint read (never rebuilt),
candidates from list_refresh_candidates with stored graph distance and
direct pairs rebuilt from events, then the unchanged run_phased_scan via
the cached fetcher. Deferred when a cost-capped full scan is mid-resume.
Records refresh_feed_cache_hits/misses (the Phase 2 pass number),
refresh_candidates/scored/last_run_at in scan_state.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 10: Docs, runbook, spec amendment, close #344

**Files:**
- Modify: `CHANGELOG.md` (`## [Unreleased]`), `README.md` (new short section after the scanning/usage section — find the heading that describes scan output with `rg -n "^## " README.md`), `docs/superpowers/specs/2026-09-08-343-scalability-onboarding-design.md` §4.4 and §6 Phase 2
- Create: `docs/runbooks/343-phase2-expiry-refresh.md`

- [ ] **Step 1: CHANGELOG**

Under `## [Unreleased]`:

```markdown
### Added
- #344 / #343 Phase 2 — score expiry and the nightly refresh. Every
  `account_scores` row now carries `scoring_generation` (a build-time
  constant, `src/scoring/generation.rs`) and `valid_until`
  (`scored_at` + 3/7/14 days by scoring confidence — the tiers that existed
  since #135 but were never wired). Tier lists, counts and the pipeline's
  "already scored" gate show only fresh rows; expired and old-generation
  rows are kept, hidden, and reported as `tier_counts.expired` (dashboard
  card, `charcoal status`). A nightly refresh (`scan_queue.kind =
  'refresh'`, `CHARCOAL_REFRESH_INTERVAL_HOURS`, default 24, `0` disables)
  re-scores each user's High/Elevated rows that expire within two days or
  predate the current generation, reading candidates from the table rather
  than the network; it runs on the existing admitter tick. Migration v18
  stamps pre-existing rows `legacy`, so they are hidden immediately and the
  High/Elevated ones return on the first tick after deploy. Postgres and
  SQLite; runbook in `docs/runbooks/343-phase2-expiry-refresh.md`.

### Changed
- Freshness reads in the scan pipeline are hard errors: a DB failure while
  reading the fresh-score set aborts the scan instead of silently
  re-scoring every candidate (#344).
- `charcoal migrate` copies only fresh scores and restamps them as the
  current generation; expired rows are left behind for the refresh job.
- The scan-queue ETA median counts full scans only; the 24 h scan cooldown
  anchors on the last *full* scan even after a refresh has reused the
  user's queue row.
```

- [ ] **Step 2: README**

Add, after the section that explains tiers/scan output:

```markdown
### Score expiry and refresh

Scores are not forever. Each one is valid for 3, 7 or 14 days depending on
how much data backed it, and every score records the *scoring generation*
it was computed under — a constant that changes when the formula, the
models or the moderation policy change. A score past its window or from an
older generation is hidden from the tiers (the dashboard shows how many
are hidden as "Expired") and comes back one of two ways: the account
engages again and a full scan re-scores it, or, for High and Elevated
accounts only, the nightly refresh re-scores it before it expires.

The refresh runs inside the web process; set
`CHARCOAL_REFRESH_INTERVAL_HOURS` (default `24`, `0` to disable) to change
its cadence.
```

- [ ] **Step 3: Runbook**

Create `docs/runbooks/343-phase2-expiry-refresh.md`:

```markdown
# #343 Phase 2 runbook — expiry + refresh on staging

Pass criteria (spec §6 Phase 2). Read every number from Postgres, not
Railway logs. Recipe for a query against staging:

    railway run -s Postgres -e staging -- sh -c 'psql "$DATABASE_PUBLIC_URL" -tAc "<SQL>"'

(filter the output for the line that is not the `postgres://` echo).

## 0. Before deploying

Note the pre-deploy tier counts for the gated account:

    SELECT COUNT(*) FILTER (WHERE threat_score >= 35) AS high,
           COUNT(*) FILTER (WHERE threat_score >= 15 AND threat_score < 35) AS elevated,
           COUNT(*) AS total
    FROM account_scores WHERE user_did = '<did>';

## 1. Deploy → every row expired and hidden

After the deploy lands (migration v18 runs in `db::open()`):

    SELECT scoring_generation, COUNT(*) FROM account_scores GROUP BY 1;
    -- expect: legacy | <total>

`GET /api/status` for the account: `tier_counts.high/elevated/watch/low`
all 0, `tier_counts.expired` = total. Dashboard shows the "Expired" card.
**Pass 1:** hidden count equals the row count; no tier shows a legacy row.

## 2. First tick → refresh enqueued and run

The migration set `users.next_refresh_at = NOW()` for every user with
scores, so within 30 s of boot:

    SELECT user_did, status, kind, enqueued_at FROM scan_queue;
    -- expect: kind = refresh, status queued → running → done

When done:

    SELECT key, value FROM scan_state WHERE user_did = '<did>'
      AND key IN ('refresh_candidates','refresh_scored','refresh_last_run_at',
                  'refresh_feed_cache_hits','refresh_feed_cache_misses');

**Pass 2:** `refresh_candidates` equals the pre-deploy `high + elevated`
count from step 0, and `refresh_scored` equals it (minus any account whose
feed failed — check `scan_skips`).

    SELECT COUNT(*) FROM account_scores
    WHERE user_did = '<did>' AND scoring_generation = 'legacy' AND threat_score >= 15;
    -- expect 0

**Pass 3:** zero legacy rows at or above Elevated. Legacy Watch/Low rows
remain and stay hidden — that is the design (decision 777).

## 3. Feed hit rate (the cache's designed consumer)

    hit rate = refresh_feed_cache_hits / (hits + misses)

On this first refresh the snapshots are ≤ 24 h old only if a full scan ran
today; otherwise expect misses. Trigger a second refresh within 24 h of a
full scan to measure the steady state:

    UPDATE users SET next_refresh_at = NOW() WHERE did = '<did>';

then re-read the two keys after it runs. **Pass 4:** ≥ 80 %. Below 50 %
means `SNAPSHOT_TTL` (24 h) and the refresh cadence are misaligned; tune
that before anything user-to-user.

## 4. The human still wins

With a refresh row `queued`, click Scan in the UI. `scan_queue.kind` must
flip to `full`. With a refresh `running`, the click must return 202 and
the row must stay `running`/`refresh` (the full scan is a no-op enqueue;
the user sees the refresh's progress). After the refresh finishes, the
cooldown must still be measured from the last *full* scan:
`scan_state.last_full_scan_finished_at`.

## 5. Bumping the generation (the ongoing procedure)

Change `SCORING_GENERATION` in `src/scoring/generation.rs`, deploy. Step 1
and 2 repeat: everything hides, High/Elevated come back on the next tick,
the rest on re-engagement. Announce it in the CHANGELOG entry that bumps it.
```

- [ ] **Step 4: Spec amendments**

In §4.4 of the spec, add at the end (dated):

```markdown
*Amended 2026-09-13 while planning Phase 2:* the migration is **v18** (v17
shipped with Phase 1's cache indexes). Queue order stays FIFO across kinds
(#271: one total order for admission, position and display); a full
enqueue upgrades a queued refresh, a refresh enqueue never touches a
queued or running row. The refresh tick is one `UPDATE users … RETURNING`
so replicas cannot double-claim. The 24 h scan cooldown anchors on
`scan_state.last_full_scan_finished_at` once a refresh has reused the
user's queue row. Refresh candidates are selected by score
(`threat_score ≥ 15`, `ThreatTier::ELEVATED_MIN`), not by the stored tier
string, so they agree with what the tier lists show. SQLite's
`valid_until` stays nullable (no post-hoc NOT NULL); NULL reads as expired.
A refresh is deferred when a cost-capped full scan is mid-resume. "Full
fortnightly" is **not** in Phase 2 — it is #342's remaining scope.
```

In §6 Phase 2, add a *Result* line pointing at the runbook once staging numbers exist (leave it as "*Result:* pending — see `docs/runbooks/343-phase2-expiry-refresh.md`" in this PR).

- [ ] **Step 5: Commit, PR, close**

```bash
git add CHANGELOG.md README.md docs/runbooks/343-phase2-expiry-refresh.md docs/superpowers/specs/2026-09-08-343-scalability-onboarding-design.md
git commit -m 'docs(344): CHANGELOG, README expiry section, Phase 2 runbook, spec §4.4 amendment

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

Push (background, HTTPS via `gh auth git-credential`, then `git ls-remote` to confirm), open the PR to `staging` with `gh pr create --base staging --body-file <file>` (no heredoc), loop on CodeRabbit until **APPROVED** (resolve every thread, never tag coderabbit, ≤ 5 reviews/hour), then `chainlink issue close 344 --no-changelog` and `chainlink issue comment 342 "Phase 2 (#344) shipped the refresh seam: users.next_refresh_at + the admitter tick + scan_queue.kind. Remaining for #342: the fortnightly FULL rescan (fingerprint age) — enqueue_scan(user) from the same tick when topic_fingerprint.updated_at > 14 d."`. Deciduous: outcome node linked from the Task 10 action, `deciduous sync`.

---

## Self-review (done while writing; kept so the executor sees the reasoning)

**Spec coverage (§4.4):** generation + valid_until columns → Task 2/3; fresh = both conditions, `get_fresh_scored_dids` without `max_age_days` → Task 3; hard-error read → Task 3; expired rows kept, hidden, counted → Task 3/4; re-entry by refresh of High/Elevated within 2 d → Task 7/9; `scan_queue.kind`, `users.next_refresh_at`, tick on the admitter's TICK, no second loop/lock → Task 5/6; migration backfill `legacy` / +14 d → Task 2; nightly default → Task 6. §6 Phase 2 pass + feed-hit counter → Task 9 records, runbook reads. §7 testing rules → every task has both-backend migration tests (2), knob clamp test (6), model-gated check (`SKIP:` grep) in every full-suite run, manual runbook (10). §8 "no second admitter loop or distributed lock" → honoured (tick + `UPDATE … RETURNING`).

**Type consistency:** `ScanKind` defined in `traits.rs` (Task 5), re-exported from `db` (`crate::db::ScanKind`) — used that way in Tasks 5, 6, 9. `RefreshCandidate` likewise (Task 7 → 9). `count_expired` (3 → 4). `schedule_refresh`/`claim_due_refresh_users` signatures identical in Task 6's trait block, impls and tests. `direct_pairs_for(db: &Arc<dyn Database>, …)` (8 → 9). `build_scan_scorers(&ScanModels, &Arc<dyn Database>)` (8 → 9). `finish_scan` gains a `label` in Task 9 — Task 9 Step 3 says so; nothing earlier calls it with the new arity.

**Known judgement calls for the reviewer/Bryan:** (1) `get_ranked_threats` going fresh-only changes `charcoal migrate` and the CLI report silently — recorded in CHANGELOG; (2) `Database::as_any` exists only for one web test — the alternative was widening the trait with a test-only generation setter, which is worse; (3) `gather_concurrency: 8` literal duplicated from `run_scan` — Phase 3 owns replacing both.

