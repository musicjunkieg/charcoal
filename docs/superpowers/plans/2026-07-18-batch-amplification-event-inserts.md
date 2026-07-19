# Batch Amplification Event Inserts Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace 359 sequential per-event `INSERT` round-trips in the amplification event loop with a single multi-row `INSERT`, cutting ~2m16s of a 28m24s scan to roughly one round-trip.

**Architecture:** Add one new method to the `Database` trait — `insert_amplification_events_batch` — implemented for both backends (SQLite via a chunked prepared statement inside a transaction; Postgres via `UNNEST`). Restructure `amplification::run`'s event loop into two passes: pass 1 builds an owned `Vec<NewAmplificationEvent>` in event order while doing the existing per-event fetch/score/NLI work, pass 2 inserts the whole vector in one call. The `println!` evidence output stays in pass 1, so it keeps emitting in original event order.

**Tech Stack:** Rust, `async_trait`, `sqlx` (Postgres), `rusqlite` (SQLite), `tokio`.

## Global Constraints

- Chainlink issue: **#216** (was #192 before the 2026-07-19 DB reconciliation — see note below). Log progress with `chainlink issue comment 216 "..."`.
- Branch: **`feat/parallel-amplification-events`**, off `staging`. Never commit to `staging` or `main`.
- Stage files explicitly by name. Never `git add -A`, `git add .`, `git add *`, or `git commit -am`.
- Never use heredocs (`<<EOF`) in shell commands — they break in zsh on this machine. Use single-quoted multi-line strings.
- Tests must pass with `cargo test --features web`. Clippy must be clean for `--features web`, default, and `--features postgres`.
- Use `?` for error propagation, not `.unwrap()`, in production code. `anyhow::Result` at this layer.
- Comments explain *why*, not *what*.
- **Do not modify `insert_amplification_event_raw`** (`src/db/traits.rs:116`). It is migrate-only and deliberately preserves `detected_at`.
- **Do not delete `insert_amplification_event`** (`src/db/traits.rs:81`). It stays for the existing tests and any future single-event caller.

## Behavior-Preservation Contract (acceptance criteria for the whole plan)

The batched path must produce **byte-identical database rows and identical stdout** compared to the serial path, for the same input:

1. **Same rows, same order.** For an input `Vec<AmplificationNotification>`, the rows in `amplification_events` must match the serial version field-for-field, and their auto-increment `id`s must ascend in input-event order.
2. **Same values.** `amplifier_text`, `original_post_text`, `context_score`, and every other column carry exactly the values the serial loop computed. Batching changes *when* rows are written, never *what*.
3. **Same stdout.** The evidence lines at `src/pipeline/amplification.rs:198-207` emit in original event order with identical text.
4. **Same return value.** `run()` still returns `(events_processed, accounts_scored, degraded)` with `events_processed == events.len()`.
5. **Empty input is a no-op.** Zero events must not issue a malformed `INSERT ... VALUES ()`.

---

## File Structure

| File | Responsibility | Task |
|---|---|---|
| `src/db/models.rs` | `NewAmplificationEvent` — owned, insertable row payload | T1 |
| `src/db/traits.rs` | `insert_amplification_events_batch` declaration | T1 |
| `src/db/queries.rs` | SQLite chunked multi-row insert (sync, rusqlite) | T2 |
| `src/db/sqlite.rs` | `SqliteDatabase` impl delegating to `queries.rs` | T2 |
| `src/db/postgres.rs` | `PgDatabase` impl via `UNNEST` | T3 |
| `src/pipeline/amplification.rs:84-209` | Two-pass restructure: collect then batch-insert | T4 |
| `tests/unit_labels.rs` | SQLite batch trait tests | T1, T2 |
| `tests/db_postgres.rs` | Postgres batch parity test (gated on `DATABASE_URL`) | T3 |
| `CHANGELOG.md` | `[Unreleased]` entry | T5 |

---

### Task 1: `NewAmplificationEvent` payload type + trait method

**Files:**
- Modify: `src/db/models.rs` (append the new struct)
- Modify: `src/db/traits.rs:92` (add method after `insert_amplification_event`)
- Modify: `src/db/sqlite.rs` (add a temporary `todo!()`-free stub impl so the crate compiles)
- Modify: `src/db/postgres.rs` (same stub)
- Test: `tests/unit_labels.rs`

**Interfaces:**
- Produces: `charcoal::db::models::NewAmplificationEvent` with public fields
  `event_type: String`, `amplifier_did: String`, `amplifier_handle: String`,
  `original_post_uri: String`, `amplifier_post_uri: Option<String>`,
  `amplifier_text: Option<String>`, `original_post_text: Option<String>`,
  `context_score: Option<f64>`.
- Produces: `Database::insert_amplification_events_batch(&self, user_did: &str, events: &[NewAmplificationEvent]) -> Result<usize>` returning the number of rows inserted.

**Why owned `String` not `&str`:** the caller builds these across `.await` points inside a loop that also borrows `event`; owned fields keep the payload `'static` and avoid a borrow tangle. This mirrors how `CandidateInput` is built in the same file.

**Why `Result<usize>` not `Result<Vec<i64>>`:** the only production caller (`src/pipeline/amplification.rs:178`) discards the returned id today (`.await?;`). Returning ids would force us to guarantee `RETURNING` ordering for no consumer. YAGNI.

- [ ] **Step 1: Write the failing test**

Append to `tests/unit_labels.rs`:

```rust
#[tokio::test]
async fn trait_batch_insert_assigns_ids_in_input_order() {
    let db = test_db().await;

    let events = vec![
        charcoal::db::models::NewAmplificationEvent {
            event_type: "quote".to_string(),
            amplifier_did: "did:plc:amp1".to_string(),
            amplifier_handle: "first.bsky.social".to_string(),
            original_post_uri: "at://did:plc:me/app.bsky.feed.post/abc".to_string(),
            amplifier_post_uri: Some("at://did:plc:amp1/app.bsky.feed.post/q1".to_string()),
            amplifier_text: Some("look at this".to_string()),
            original_post_text: Some("my original post".to_string()),
            context_score: Some(0.85),
        },
        charcoal::db::models::NewAmplificationEvent {
            event_type: "repost".to_string(),
            amplifier_did: "did:plc:amp2".to_string(),
            amplifier_handle: "second.bsky.social".to_string(),
            original_post_uri: "at://did:plc:me/app.bsky.feed.post/abc".to_string(),
            amplifier_post_uri: None,
            amplifier_text: None,
            original_post_text: None,
            context_score: None,
        },
    ];

    let n = db
        .insert_amplification_events_batch(TEST_USER, &events)
        .await
        .unwrap();
    assert_eq!(n, 2);

    // get_recent_events orders by detected_at DESC. All rows in one batch share
    // a detected_at, so assert on the set and on id ordering instead.
    let stored = db.get_recent_events(TEST_USER, 10).await.unwrap();
    assert_eq!(stored.len(), 2);

    let first = stored
        .iter()
        .find(|e| e.amplifier_handle == "first.bsky.social")
        .expect("first event missing");
    let second = stored
        .iter()
        .find(|e| e.amplifier_handle == "second.bsky.social")
        .expect("second event missing");

    // Determinism contract: ids ascend in input order.
    assert!(first.id < second.id, "ids must ascend in input order");

    assert_eq!(first.event_type, "quote");
    assert_eq!(first.original_post_text, Some("my original post".to_string()));
    assert_eq!(first.context_score, Some(0.85));
    assert_eq!(second.event_type, "repost");
    assert_eq!(second.amplifier_text, None);
    assert_eq!(second.context_score, None);
}

#[tokio::test]
async fn trait_batch_insert_empty_slice_is_noop() {
    let db = test_db().await;

    let n = db
        .insert_amplification_events_batch(TEST_USER, &[])
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(db.get_recent_events(TEST_USER, 10).await.unwrap().len(), 0);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features web --test unit_labels trait_batch_insert`

Expected: FAIL to compile — `no function or associated item named insert_amplification_events_batch`, and `NewAmplificationEvent` not found.

- [ ] **Step 3: Add the struct**

Append to `src/db/models.rs`:

```rust
/// An amplification event that has not been written to the database yet.
///
/// Owned rather than borrowed because the amplification pipeline builds these
/// across `.await` points while iterating borrowed events — owned fields keep
/// the payload `'static` and sidestep the borrow tangle. `detected_at` is
/// deliberately absent: the database default stamps it, matching the
/// single-row `insert_amplification_event` path.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAmplificationEvent {
    pub event_type: String,
    pub amplifier_did: String,
    pub amplifier_handle: String,
    pub original_post_uri: String,
    pub amplifier_post_uri: Option<String>,
    pub amplifier_text: Option<String>,
    pub original_post_text: Option<String>,
    pub context_score: Option<f64>,
}
```

- [ ] **Step 4: Declare the trait method**

In `src/db/traits.rs`, immediately after the `insert_amplification_event` declaration that ends at line 92, add:

```rust
    /// Insert many amplification events for a user in a single statement.
    ///
    /// Returns the number of rows inserted. Exists because the amplification
    /// pipeline was issuing one network round-trip per event (359 events ≈ 2m16s
    /// of a 28m scan, chainlink #216); one multi-row statement collapses that to
    /// a single round-trip.
    ///
    /// Rows MUST be inserted in slice order so auto-increment ids ascend in
    /// input order — downstream evidence output and tests depend on it.
    /// An empty slice is a no-op returning `Ok(0)`.
    async fn insert_amplification_events_batch(
        &self,
        user_did: &str,
        events: &[NewAmplificationEvent],
    ) -> Result<usize>;
```

Add `NewAmplificationEvent` to the existing `use super::models::{...}` import list at the top of `src/db/traits.rs`.

- [ ] **Step 5: Add compiling stubs to both backends**

The trait now has an unimplemented method, so both impls must exist for the crate to build. These are placeholders that Tasks 2 and 3 replace.

In `src/db/sqlite.rs`, after the `insert_amplification_event` impl that ends at line 137:

```rust
    async fn insert_amplification_events_batch(
        &self,
        user_did: &str,
        events: &[NewAmplificationEvent],
    ) -> Result<usize> {
        let conn = self.conn.lock().await;
        super::queries::insert_amplification_events_batch(&conn, user_did, events)
    }
```

In `src/db/postgres.rs`, after the `insert_amplification_event` impl that ends at line 469:

```rust
    async fn insert_amplification_events_batch(
        &self,
        user_did: &str,
        events: &[NewAmplificationEvent],
    ) -> Result<usize> {
        if events.is_empty() {
            return Ok(0);
        }
        let mut inserted = 0usize;
        for e in events {
            self.insert_amplification_event(
                user_did,
                &e.event_type,
                &e.amplifier_did,
                &e.amplifier_handle,
                &e.original_post_uri,
                e.amplifier_post_uri.as_deref(),
                e.amplifier_text.as_deref(),
                e.original_post_text.as_deref(),
                e.context_score,
            )
            .await?;
            inserted += 1;
        }
        Ok(inserted)
    }
```

The Postgres stub is a correct-but-slow loop; Task 3 replaces it with `UNNEST`. The SQLite stub forward-references `queries::insert_amplification_events_batch`, which Task 2 writes — so the crate will not compile until Task 2 lands. That is intentional: Tasks 1 and 2 are one commit.

Add `NewAmplificationEvent` to the model imports in both `src/db/sqlite.rs` and `src/db/postgres.rs`.

- [ ] **Step 6: Proceed directly to Task 2**

Do not run tests or commit yet — the crate does not compile until Task 2 supplies `queries::insert_amplification_events_batch`. Tasks 1+2 share a commit.

---

### Task 2: SQLite chunked multi-row insert

**Files:**
- Modify: `src/db/queries.rs` (add after `insert_amplification_event`, which ends at line 324)
- Test: `tests/unit_labels.rs` (the tests from Task 1)

**Interfaces:**
- Consumes: `NewAmplificationEvent` from Task 1.
- Produces: `pub fn insert_amplification_events_batch(conn: &Connection, user_did: &str, events: &[NewAmplificationEvent]) -> Result<usize>`.

**Why chunking:** SQLite's default `SQLITE_MAX_VARIABLE_NUMBER` is 999 on older builds. Each row binds 9 parameters, so 999 / 9 = 111 rows per statement. Chunk at 100 for headroom. A single transaction wraps all chunks so the batch stays atomic.

- [ ] **Step 1: Write the implementation**

Append to `src/db/queries.rs` after line 324:

```rust
/// Insert many amplification events in one transaction, batched into
/// multi-row `INSERT` statements.
///
/// SQLite binds a maximum of 999 parameters per statement on older builds and
/// each row uses 9, so rows are chunked at 100 (999/9 = 111, minus headroom).
/// All chunks share one transaction, so the batch is atomic and ids ascend in
/// slice order.
pub fn insert_amplification_events_batch(
    conn: &Connection,
    user_did: &str,
    events: &[NewAmplificationEvent],
) -> Result<usize> {
    if events.is_empty() {
        return Ok(0);
    }

    const ROWS_PER_STATEMENT: usize = 100;
    const COLS: usize = 9;

    let tx = conn.unchecked_transaction()?;
    let mut inserted = 0usize;

    for chunk in events.chunks(ROWS_PER_STATEMENT) {
        // Build "(?1,?2,...,?9),(?10,...)" with 1-based positional parameters.
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|row| {
                let base = row * COLS;
                let slots: Vec<String> =
                    (1..=COLS).map(|c| format!("?{}", base + c)).collect();
                format!("({})", slots.join(","))
            })
            .collect();

        let sql = format!(
            "INSERT INTO amplification_events
                (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri,
                 amplifier_post_uri, amplifier_text, original_post_text, context_score)
             VALUES {}",
            placeholders.join(",")
        );

        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(chunk.len() * COLS);
        for e in chunk {
            params.push(Box::new(user_did.to_string()));
            params.push(Box::new(e.event_type.clone()));
            params.push(Box::new(e.amplifier_did.clone()));
            params.push(Box::new(e.amplifier_handle.clone()));
            params.push(Box::new(e.original_post_uri.clone()));
            params.push(Box::new(e.amplifier_post_uri.clone()));
            params.push(Box::new(e.amplifier_text.clone()));
            params.push(Box::new(e.original_post_text.clone()));
            params.push(Box::new(e.context_score));
        }

        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        inserted += tx.execute(&sql, param_refs.as_slice())?;
    }

    tx.commit()?;
    Ok(inserted)
}
```

Add `NewAmplificationEvent` to the `use super::models::{...}` import list at the top of `src/db/queries.rs`.

- [ ] **Step 2: Run the tests to verify they now pass**

Run: `cargo test --features web --test unit_labels trait_batch_insert`

Expected: PASS, 2 tests (`trait_batch_insert_assigns_ids_in_input_order`, `trait_batch_insert_empty_slice_is_noop`).

- [ ] **Step 3: Add a chunk-boundary regression test**

The chunking arithmetic is the one place this can silently corrupt data — an off-by-one in the placeholder base index would bind the wrong values. Append to `tests/unit_labels.rs`:

```rust
#[tokio::test]
async fn trait_batch_insert_spans_chunk_boundary_in_order() {
    let db = test_db().await;

    // 250 rows forces three chunks at ROWS_PER_STATEMENT = 100, exercising the
    // placeholder base-index arithmetic across boundaries.
    let events: Vec<charcoal::db::models::NewAmplificationEvent> = (0..250)
        .map(|i| charcoal::db::models::NewAmplificationEvent {
            event_type: "repost".to_string(),
            amplifier_did: format!("did:plc:amp{:04}", i),
            amplifier_handle: format!("amp{:04}.bsky.social", i),
            original_post_uri: "at://did:plc:me/app.bsky.feed.post/abc".to_string(),
            amplifier_post_uri: None,
            amplifier_text: Some(format!("text-{}", i)),
            original_post_text: None,
            context_score: Some(i as f64 / 1000.0),
        })
        .collect();

    let n = db
        .insert_amplification_events_batch(TEST_USER, &events)
        .await
        .unwrap();
    assert_eq!(n, 250);

    let stored = db.get_recent_events(TEST_USER, 1000).await.unwrap();
    assert_eq!(stored.len(), 250);

    // Every row must keep its own field values — a base-index bug would smear
    // values across rows. Check by id order, which is input order.
    let mut by_id = stored.clone();
    by_id.sort_by_key(|e| e.id);
    for (i, e) in by_id.iter().enumerate() {
        assert_eq!(e.amplifier_handle, format!("amp{:04}.bsky.social", i));
        assert_eq!(e.amplifier_text, Some(format!("text-{}", i)));
        assert_eq!(e.context_score, Some(i as f64 / 1000.0));
    }
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test --features web --test unit_labels trait_batch_insert`

Expected: PASS, 3 tests.

- [ ] **Step 5: Run the full suite and clippy**

Run: `cargo test --features web`
Expected: all green, no new failures.

Run: `cargo clippy --features web --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/db/models.rs src/db/traits.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs tests/unit_labels.rs
git commit -m 'feat(db): add insert_amplification_events_batch trait method + SQLite impl (#192)'
```

- [ ] **Step 7: Log progress**

```bash
chainlink issue comment 192 'Tasks 1+2 complete: NewAmplificationEvent + trait method + SQLite chunked multi-row insert. 3 tests green incl. a 250-row chunk-boundary test. Postgres still on the temporary per-row loop stub.'
```

---

### Task 3: Postgres `UNNEST` implementation

**Files:**
- Modify: `src/db/postgres.rs` (replace the Task 1 stub)
- Test: `tests/db_postgres.rs`

**Interfaces:**
- Consumes: `NewAmplificationEvent`, the trait method from Task 1.
- Produces: nothing new — replaces the stub body.

**Why `UNNEST` over a built VALUES list:** Postgres caps a statement at 65535 bind parameters. `UNNEST` binds 9 arrays regardless of row count, so it is one round-trip for any batch size with no chunking arithmetic. `context_score` needs an explicit `float8[]` cast because an all-`NULL` array is otherwise untyped.

- [ ] **Step 1: Write the failing test**

Append to `tests/db_postgres.rs`:

```rust
#[tokio::test]
async fn test_pg_batch_insert_matches_serial() {
    let Some(url) = database_url() else {
        return;
    };
    cleanup_test_data(&url).await.unwrap();
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let events = vec![
        charcoal::db::models::NewAmplificationEvent {
            event_type: "quote".to_string(),
            amplifier_did: "did:plc:pgbatch1".to_string(),
            amplifier_handle: "pgbatch1.bsky.social".to_string(),
            original_post_uri: "at://did:plc:me/app.bsky.feed.post/b1".to_string(),
            amplifier_post_uri: Some("at://did:plc:pgbatch1/app.bsky.feed.post/q1".to_string()),
            amplifier_text: Some("batched quote".to_string()),
            original_post_text: Some("the original".to_string()),
            context_score: Some(0.42),
        },
        charcoal::db::models::NewAmplificationEvent {
            event_type: "repost".to_string(),
            amplifier_did: "did:plc:pgbatch2".to_string(),
            amplifier_handle: "pgbatch2.bsky.social".to_string(),
            original_post_uri: "at://did:plc:me/app.bsky.feed.post/b1".to_string(),
            amplifier_post_uri: None,
            amplifier_text: None,
            original_post_text: None,
            context_score: None,
        },
    ];

    let n = db
        .insert_amplification_events_batch(TEST_USER, &events)
        .await
        .unwrap();
    assert_eq!(n, 2);

    let stored = db.get_recent_events(TEST_USER, 10).await.unwrap();
    assert_eq!(stored.len(), 2);

    let first = stored
        .iter()
        .find(|e| e.amplifier_handle == "pgbatch1.bsky.social")
        .expect("first event missing");
    let second = stored
        .iter()
        .find(|e| e.amplifier_handle == "pgbatch2.bsky.social")
        .expect("second event missing");

    assert!(first.id < second.id, "ids must ascend in input order");
    assert_eq!(first.amplifier_text, Some("batched quote".to_string()));
    assert_eq!(first.original_post_text, Some("the original".to_string()));
    assert_eq!(first.context_score, Some(0.42));
    assert_eq!(second.amplifier_text, None);
    assert_eq!(second.context_score, None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `DATABASE_URL=postgres://bryan.guffey@localhost/charcoal cargo test --all-targets --features postgres --test db_postgres test_pg_batch_insert`

Expected: FAIL on the `first.id < second.id` assertion is *possible* but not guaranteed — the Task 1 stub loops sequentially and would satisfy it. **If the test passes against the stub, that is expected and fine**: this test's job is to lock the contract before the `UNNEST` rewrite, which is the change that could break ordering. Record that it passed, then proceed. Do not weaken the test.

If `DATABASE_URL` is unset the test returns early and reports as passed — check the output says it actually ran.

- [ ] **Step 3: Replace the stub with `UNNEST`**

In `src/db/postgres.rs`, replace the entire `insert_amplification_events_batch` body added in Task 1 with:

```rust
    async fn insert_amplification_events_batch(
        &self,
        user_did: &str,
        events: &[NewAmplificationEvent],
    ) -> Result<usize> {
        if events.is_empty() {
            return Ok(0);
        }

        // UNNEST binds 9 arrays regardless of row count, so this is one
        // round-trip for any batch size and never approaches Postgres's
        // 65535-parameter statement cap. `WITH ORDINALITY` is not needed:
        // Postgres expands UNNEST in array order, so serial ids ascend in
        // slice order, which the determinism contract requires.
        let event_types: Vec<String> = events.iter().map(|e| e.event_type.clone()).collect();
        let amplifier_dids: Vec<String> =
            events.iter().map(|e| e.amplifier_did.clone()).collect();
        let amplifier_handles: Vec<String> =
            events.iter().map(|e| e.amplifier_handle.clone()).collect();
        let original_post_uris: Vec<String> =
            events.iter().map(|e| e.original_post_uri.clone()).collect();
        let amplifier_post_uris: Vec<Option<String>> =
            events.iter().map(|e| e.amplifier_post_uri.clone()).collect();
        let amplifier_texts: Vec<Option<String>> =
            events.iter().map(|e| e.amplifier_text.clone()).collect();
        let original_post_texts: Vec<Option<String>> =
            events.iter().map(|e| e.original_post_text.clone()).collect();
        let context_scores: Vec<Option<f64>> =
            events.iter().map(|e| e.context_score).collect();

        // The explicit float8[] cast is required: an all-NULL context_score
        // array is otherwise untyped and Postgres rejects it.
        let result = sqlx_core::query::query(
            "INSERT INTO amplification_events
                (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri,
                 amplifier_post_uri, amplifier_text, original_post_text, context_score)
             SELECT $1, t.event_type, t.amplifier_did, t.amplifier_handle, t.original_post_uri,
                    t.amplifier_post_uri, t.amplifier_text, t.original_post_text, t.context_score
             FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[],
                         $6::text[], $7::text[], $8::text[], $9::float8[])
                  AS t(event_type, amplifier_did, amplifier_handle, original_post_uri,
                       amplifier_post_uri, amplifier_text, original_post_text, context_score)",
        )
        .bind(user_did)
        .bind(&event_types)
        .bind(&amplifier_dids)
        .bind(&amplifier_handles)
        .bind(&original_post_uris)
        .bind(&amplifier_post_uris)
        .bind(&amplifier_texts)
        .bind(&original_post_texts)
        .bind(&context_scores)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() as usize)
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `DATABASE_URL=postgres://bryan.guffey@localhost/charcoal cargo test --all-targets --features postgres --test db_postgres test_pg_batch_insert`

Expected: PASS. If it fails on the `float8[]` bind, confirm `sqlx-postgres` array binding is in scope; do not remove the cast.

- [ ] **Step 5: Run clippy for the postgres feature**

Run: `cargo clippy --features postgres --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add src/db/postgres.rs tests/db_postgres.rs
git commit -m 'feat(db): Postgres UNNEST impl for batched amplification inserts (#192)'
```

- [ ] **Step 7: Log progress**

```bash
chainlink issue comment 192 'Task 3 complete: Postgres UNNEST impl replaces the per-row stub. One round-trip for any batch size. Parity test green against local Postgres.'
```

---

### Task 4: Two-pass restructure of the amplification event loop

**Files:**
- Modify: `src/pipeline/amplification.rs:84-209`
- Test: `tests/composition.rs`

**Interfaces:**
- Consumes: `NewAmplificationEvent` (T1), `insert_amplification_events_batch` (T1/T2/T3).
- Produces: no new public API. `run()`'s signature and return type are unchanged.

**The change:** the existing loop does per-event work *and* inserts. Split it. Pass 1 keeps every existing behavior — the `quote`/`reply` fetch gate, scoring, NLI, audit logging, and the `println!` evidence output — but pushes an owned `NewAmplificationEvent` onto a `Vec` instead of inserting. Pass 2 inserts the whole `Vec` in one call.

**Critical:** pass 1 stays sequential. Do not parallelize it in this task. Only ~6 of 359 events do network work, so parallelizing buys almost nothing here and would reorder the `println!` output. Batching the inserts is the entire win.

- [ ] **Step 1: Write the failing test**

Append to `tests/composition.rs`:

```rust
/// The determinism contract for #216: whatever order the pipeline builds
/// events in, the batch payload must preserve input order so ids ascend
/// with it. This tests the ordering invariant directly against the DB,
/// independent of the network-dependent pipeline.
#[tokio::test]
async fn batched_amplification_events_preserve_input_order() {
    use charcoal::db::models::NewAmplificationEvent;
    use charcoal::db::Database;

    let conn = rusqlite::Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    let db = charcoal::db::sqlite::SqliteDatabase::new(conn);
    let user = "did:plc:testuser000000000000";

    // Deliberately non-alphabetical handles: if the implementation ever sorts
    // or reorders internally, id order would stop matching input order.
    let order = ["zulu", "alpha", "mike", "bravo"];
    let events: Vec<NewAmplificationEvent> = order
        .iter()
        .map(|h| NewAmplificationEvent {
            event_type: "repost".to_string(),
            amplifier_did: format!("did:plc:{}", h),
            amplifier_handle: format!("{}.bsky.social", h),
            original_post_uri: "at://did:plc:me/app.bsky.feed.post/x".to_string(),
            amplifier_post_uri: None,
            amplifier_text: None,
            original_post_text: None,
            context_score: None,
        })
        .collect();

    db.insert_amplification_events_batch(user, &events)
        .await
        .unwrap();

    let mut stored = db.get_recent_events(user, 100).await.unwrap();
    stored.sort_by_key(|e| e.id);

    let stored_order: Vec<String> = stored
        .iter()
        .map(|e| e.amplifier_handle.replace(".bsky.social", ""))
        .collect();
    assert_eq!(stored_order, order.to_vec());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features web --test composition batched_amplification_events_preserve_input_order`

Expected: it may PASS immediately, because Tasks 1–3 already implement the ordering guarantee. That is acceptable here — this test guards Task 4's restructure against regressing ordering. Record the result and continue. Do not weaken it.

- [ ] **Step 3: Restructure the loop**

In `src/pipeline/amplification.rs`, change the comment and loop opening at lines 84-86 from:

```rust
    // Store each event in the database, fetching quote text when available.
    // Look up original post text from the cache for all event types.
    for event in &events {
```

to:

```rust
    // Build the full set of rows first, then write them in ONE statement.
    //
    // This loop used to insert per event, which cost one DB round-trip each —
    // 359 events ≈ 2m16s of a 28m scan (#216). The per-event work below is
    // unchanged; only the write is deferred. The loop stays sequential on
    // purpose: the `fetch_post_text` call is gated on quote/reply, so only a
    // handful of events do network work, and going concurrent here would
    // reorder the evidence output below for no meaningful gain.
    let mut pending_events: Vec<crate::db::models::NewAmplificationEvent> =
        Vec::with_capacity(events.len());

    for event in &events {
```

- [ ] **Step 4: Replace the insert call with a push**

Replace lines 178-189 (the `db.insert_amplification_event(...).await?;` call) with:

```rust
        pending_events.push(crate::db::models::NewAmplificationEvent {
            event_type: event.event_type.clone(),
            amplifier_did: event.amplifier_did.clone(),
            amplifier_handle: event.amplifier_handle.clone(),
            original_post_uri: event
                .original_post_uri
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            amplifier_post_uri: Some(event.amplifier_post_uri.clone()),
            amplifier_text: amplifier_text.clone(),
            original_post_text: original_post_text.map(|s| s.to_string()),
            context_score,
        });
```

Note the `unwrap_or_else(|| "unknown".to_string())` preserves the existing
`.unwrap_or("unknown")` behavior at line 183 exactly.

The `println!` block at lines 191-208 stays exactly where it is, inside the
loop, so evidence output order is untouched.

- [ ] **Step 5: Add the batch write after the loop**

Immediately after the loop's closing brace at line 209, before the
`// Phase B:` comment, add:

```rust
    // One round-trip for the whole batch (#216).
    let inserted = db
        .insert_amplification_events_batch(user_did, &pending_events)
        .await?;
    info!(
        events = inserted,
        "Recorded amplification events in one batch"
    );
```

- [ ] **Step 6: Run the tests**

Run: `cargo test --features web`

Expected: all green. If `composition.rs` or `unit_labels.rs` fail, the
restructure changed behavior — fix the code, not the tests.

- [ ] **Step 7: Verify clippy across all three feature sets**

Run: `cargo clippy --features web --all-targets -- -D warnings`
Run: `cargo clippy --all-targets -- -D warnings`
Run: `cargo clippy --features postgres --all-targets -- -D warnings`

Expected: all clean.

- [ ] **Step 8: Commit**

```bash
git add src/pipeline/amplification.rs tests/composition.rs
git commit -m 'perf(amplification): batch event inserts into one statement (#192)'
```

- [ ] **Step 9: Log progress**

```bash
chainlink issue comment 192 'Task 4 complete: amplification event loop restructured to two passes — collect then batch-insert. 359 round-trips become 1. println evidence output still emits in event order inside pass 1; loop deliberately left sequential.'
```

---

### Task 5: Changelog

**Files:**
- Modify: `CHANGELOG.md`

- [ ] **Step 1: Add the entry**

Under the `## [Unreleased]` heading's `### Changed` section in `CHANGELOG.md`, add as the first bullet:

```markdown
- Batch amplification event inserts into a single multi-row statement instead of one round-trip per event — the event loop was 2m16s of a 28m24s scan at 359 sequential inserts (#216)
```

- [ ] **Step 2: Commit**

```bash
git add CHANGELOG.md
git commit -m 'docs(changelog): note batched amplification inserts (#192)'
```

---

## Final Verification (run after all tasks)

**REQUIRED SUB-SKILL:** `superpowers:verification-before-completion`

- [ ] `cargo test --features web` — full suite green, zero failures
- [ ] `DATABASE_URL=postgres://bryan.guffey@localhost/charcoal cargo test --all-targets --features postgres` — Postgres tests actually ran (not silently skipped)
- [ ] `cargo clippy --features web --all-targets -- -D warnings` — clean
- [ ] `cargo clippy --all-targets -- -D warnings` — clean
- [ ] `cargo clippy --features postgres --all-targets -- -D warnings` — clean
- [ ] `cargo fmt --check` — clean
- [ ] Behavior-preservation contract items 1–5 each map to a passing test
- [ ] `git log --oneline staging..HEAD` shows 4 commits, none on `staging` or `main`

**Not in scope, do not attempt:** pushing, opening a PR, or merging. Report back for review.

---

## Self-Review Notes

- **Spec coverage:** contract item 1 (order) → T1 Step 1 + T2 Step 3 + T4 Step 1; item 2 (values) → T2 Step 3 chunk-boundary test; item 3 (stdout) → T4 Step 4 leaves the `println!` block untouched and in place; item 4 (return value) → `run()` is not modified beyond the loop body; item 5 (empty) → T1 Step 1 `trait_batch_insert_empty_slice_is_noop` plus early returns in both impls.
- **Known wrinkle, called out deliberately:** Tasks 1 and 2 do not compile independently — Task 1's SQLite stub forward-references the function Task 2 writes. They share one commit. Task 1 Step 6 says so explicitly so an implementer does not try to test a non-compiling tree.
- **Two tests may pass on first run** (T3 Step 2, T4 Step 2) because earlier tasks already satisfy the invariant they guard. Both steps say so and instruct the implementer to record it and continue rather than weaken the test. This is a deliberate departure from strict red-green: these are regression guards for a *later* task's refactor, not drivers of new behavior.
- **`detected_at` is intentionally absent** from `NewAmplificationEvent`. The single-row path lets the DB default stamp it; the batch path must match. All rows in a batch will therefore share a timestamp, which is why the ordering tests assert on `id`, not `detected_at`.

---

## Note: issue renumbering, 2026-07-19

This work was executed as chainlink **#192** and is now **#216**. The chainlink DB
had been silently clobbered three times (2026-06-23, 2026-07-12, and again at the
start of this session) by `main` tracking `.chainlink/issues.db` while `staging`
did not — every `git checkout main` restored a stale DB and the pre-commit hook
exported it over the good one. That destroyed issues #189–#212 and caused work to
be re-created on numbers already in use.

The reconciliation restored the original #189–#212 from the last verified-good
export (commit `2d479dc`, 212 issues) and moved this session's five issues to
#213–#217. **Commit messages on this branch still say `(#192)`** — they are the
historical record and were not rewritten. The `main`-tracking bug is fixed in PR #75.

