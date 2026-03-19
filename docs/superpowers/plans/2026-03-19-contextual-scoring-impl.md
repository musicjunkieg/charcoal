# Phase 1.75: Contextual Scoring Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add contextual hostility scoring via NLI cross-encoder, expand engagement detection to likes and replies, build user feedback/labeling mechanism, and deploy to Railway staging environment.

**Architecture:** Zero-shot NLI model (DeBERTa-v3-xsmall, ~87MB ONNX) scores text pairs for hostile intent. Expanded Constellation queries detect likes; `getPostThread` detects drive-by replies. User labels stored in new `user_labels` table. All changes validated in Railway staging before production.

**Tech Stack:** Rust (ort + tokenizers for ONNX), Axum (web API), SvelteKit (frontend), SQLite/PostgreSQL (dual backend), Railway (staging + production)

**Spec:** `docs/superpowers/specs/2026-03-19-contextual-scoring-design.md`

**TDD mandate:** Write ALL tests BEFORE implementation code. Do NOT modify tests unless a dedicated review subagent confirms the test is faulty.

---

## CRITICAL: Corrections From Plan Review

**Read these before implementing any task. They override the code examples below.**

### 1. AccountScore and AmplificationEvent field types

The test code throughout this plan uses `Option` wrappers on fields that are
NOT optional in the actual structs. The real types from `src/db/models.rs`:

```rust
// AccountScore — these fields are NOT Option:
pub posts_analyzed: u32,     // Plan incorrectly uses Some(10)
pub scored_at: String,       // Plan incorrectly uses Some("...")

// AmplificationEvent — these fields are NOT Option:
pub id: i64,                 // Plan incorrectly uses Some(1)
pub detected_at: String,     // Plan incorrectly uses Some("...")
pub followers_fetched: bool, // Plan incorrectly uses Some(false)
pub followers_scored: bool,  // Plan incorrectly uses Some(false)
```

**Fix:** In ALL test code that constructs these structs, use the actual types:
- `posts_analyzed: 10` (not `Some(10)`)
- `scored_at: "2026-03-19T12:00:00Z".to_string()` (not `Some(...)`)
- `id: 1` (not `Some(1)`)
- `detected_at: "2026-03-19T12:00:00Z".to_string()` (not `Some(...)`)
- `followers_fetched: false` (not `Some(false)`)
- `followers_scored: false` (not `Some(false)`)

### 2. NLI score_pair input architecture is WRONG

The plan's `score_pair` method concatenates texts with `[SEP]`:
```rust
let premise = format!("{} [SEP] {}", original_text, response_text);
```
This is INCORRECT. NLI cross-encoders take two separate text segments — the
tokenizer handles `[SEP]` automatically via `encode((text_a, text_b), true)`.

**Fix:** `score_pair` should call `score_hypothesis` with the original text as
one segment. Restructure so that:
- Premise = `"{original_text} ||| {response_text}"` (concatenated as a single
  premise describing the interaction)
- Hypothesis = each of the 5 hostility templates

OR better: use the NLI model's native pair encoding:
- text_a = the combined context (original + response)
- text_b = the hypothesis template

The tokenizer call `encode((text_a, text_b), true)` produces
`[CLS] text_a [SEP] text_b [SEP]` which is the correct NLI input.

### 3. Missing call sites for insert_amplification_event

When the trait signature changes (Task 3), these additional callers need
`None, None` appended:
- `src/main.rs` (if it calls insert_amplification_event directly)
- `tests/db_postgres.rs`
- `src/db/sqlite.rs` — the `insert_amplification_event_raw` method also needs
  updating to persist the new fields

### 4. Missing files from breakage list (Task 1, Step 4)

These files also construct AccountScore and need `context_score: None` added:
- `src/output/markdown.rs`
- `tests/db_postgres.rs`

### 5. fetch_post_text already exists

`src/bluesky/posts.rs` has `pub async fn fetch_post_text(client, uri) -> Result<Option<String>>`.
Task 9 can use this directly — no new function needed. Verify the exact
import path before using.

### 6. getLikes fallback must be implemented

Task 7 must include a fallback path for when Constellation does not index
likes. Create `src/bluesky/likes.rs` with:

```rust
pub async fn fetch_likers_via_api(
    client: &PublicAtpClient, post_uri: &str, limit: usize,
) -> Result<Vec<String>> {
    // GET app.bsky.feed.getLikes?uri={uri}&limit={limit}
    // Parse response, return Vec of liker DIDs
}
```

The scan pipeline should try Constellation first, fall back to this if
Constellation returns an error or empty results for likes.

### 7. Task 10 web API tests must be real tests

The `todo!()` stubs in Task 10 are NOT acceptable. The implementer MUST:
1. Read `tests/web_oauth.rs` for the test app setup pattern
2. Write complete, runnable test bodies BEFORE implementing the handlers
3. Each test must assert specific HTTP status codes and response body shapes

### 8. NLI tensor construction guidance

`score_hypothesis` needs the actual ort inference code. DeBERTa-v3-xsmall:
- **Inputs:** `input_ids` (i64), `attention_mask` (i64), `token_type_ids` (i64)
  — all shape `[1, seq_len]`
- **Output:** logits shape `[1, 3]` → `[contradiction, neutral, entailment]`
- **Post-processing:** softmax the logits, return `entailment` probability

Follow the pattern from `src/toxicity/onnx.rs` but note:
- RoBERTa (Detoxify) does NOT use `token_type_ids` — DeBERTa DOES
- RoBERTa output is multi-label sigmoid — DeBERTa NLI output is 3-class softmax
- Use `ndarray` for tensor construction, same as existing code

### 9. Inferred pairs require per-post embeddings

`find_most_similar_posts` needs per-post embeddings for both the target and
the protected user. Currently `build_profile` computes a mean embedding.

**Fix:** When fetching target posts, also embed each post individually using
`SentenceEmbedder::embed()`. Store the post text + embedding pairs in memory
(not DB) for the duration of the scoring pass. For the protected user's posts,
embed their recent posts once at scan start and cache.

### 10. Follows set threading

`fetch_follows_set` should be called ONCE at the start of a scan and passed
as a parameter to the reply detection pipeline. Do NOT re-fetch per post.
Store in memory only (no database table needed). The spec's mention of a
`user_follows` table is optional — in-memory `HashSet<String>` is sufficient.

### 11. PublicAtpClient field access

`PublicAtpClient` wraps a `reqwest::Client`. Check the actual field name in
`src/bluesky/client.rs` before using `client.client` — it may be a different
name or may need a getter method. Read the file first.

---

## File Structure

### New Files
| File | Responsibility |
|------|---------------|
| `src/scoring/nli.rs` | NLI inference module — load DeBERTa ONNX model, run hypothesis scoring, compute hostility |
| `src/scoring/context.rs` | Contextual scoring orchestration — find pairs, run NLI, return context_score |
| `src/bluesky/replies.rs` | Reply thread fetching — `getPostThread` wrapper, drive-by filtering |
| `src/bluesky/likes.rs` | Like detection — Constellation likes query OR `getLikes` fallback |
| `src/web/handlers/labels.rs` | Label API endpoints — upsert, get, review queue, accuracy metrics |
| `src/web/handlers/review.rs` | Review queue endpoint — unlabeled accounts sorted by threat_score |
| `web/src/routes/(protected)/review/+page.svelte` | Triage review queue UI |
| `web/src/lib/components/LabelButtons.svelte` | Reusable 4-tier label button component |
| `tests/unit_nli.rs` | NLI module unit tests |
| `tests/unit_context.rs` | Context scoring unit tests |
| `tests/unit_labels.rs` | Label CRUD and accuracy metric tests |
| `tests/unit_replies.rs` | Reply detection and filtering tests |
| `migrations/postgres/0005_contextual_scoring.sql` | Postgres schema v5 migration |

### Modified Files
| File | Changes |
|------|---------|
| `src/db/models.rs` | Add `context_score` to `AccountScore`, `original_post_text`/`context_score` to `AmplificationEvent`, new `UserLabel`/`InferredPair`/`AccuracyMetrics` structs |
| `src/db/traits.rs` | Add 2 params to `insert_amplification_event`, add 6 new trait methods (labels + inferred pairs) |
| `src/db/schema.rs` | Add `migrate_v4_to_v5()` — new columns, new tables |
| `src/db/sqlite.rs` | Implement new/modified trait methods |
| `src/db/postgres.rs` | Implement new/modified trait methods |
| `src/db/queries.rs` | Add SQL for new operations |
| `src/scoring/mod.rs` | Export `nli` and `context` modules |
| `src/scoring/threat.rs` | Add `compute_threat_score_contextual()` with blended formula |
| `src/scoring/behavioral.rs` | Bypass benign gate when `context_score >= 0.5` |
| `src/scoring/profile.rs` | Integrate context scoring into `build_profile()` |
| `src/pipeline/amplification.rs` | Store `original_post_text`, score likes and replies, pass context scorer |
| `src/constellation/client.rs` | Add `get_likes()` query method |
| `src/bluesky/likes.rs` | Create: `getLikes` fallback when Constellation doesn't index likes |
| `src/output/markdown.rs` | Add `context_score: None` to AccountScore construction |
| `src/main.rs` | Update `insert_amplification_event` call sites with new params |
| `tests/db_postgres.rs` | Add `context_score` to AccountScore + update event call sites |
| `src/toxicity/download.rs` | Add NLI model download |
| `src/web/handlers/mod.rs` | Register label and review routes |
| `src/web/handlers/accounts.rs` | Add label data to account detail response |
| `web/src/routes/(protected)/accounts/[handle]/+page.svelte` | Add inline label buttons |
| `web/src/routes/(protected)/dashboard/+page.svelte` | Add accuracy metrics panel |
| `web/src/lib/types.ts` | Add label and accuracy metric types |
| `web/src/lib/api.ts` | Add label and review API calls |
| `Cargo.toml` | No new dependencies expected (ort + tokenizers already present) |

---

## Chunk 1: Schema & Data Models (Tasks 1-3)

### Task 1: Data Model Structs

**Files:**
- Modify: `src/db/models.rs`
- Test: `tests/unit_labels.rs` (new)

- [ ] **Step 1: Write failing tests for new data model structs**

Create `tests/unit_labels.rs`:

```rust
//! Unit tests for user labels and contextual scoring data models.

use charcoal::db::models::{
    AccuracyMetrics, InferredPair, UserLabel,
};

#[test]
fn user_label_fields_accessible() {
    let label = UserLabel {
        user_did: "did:plc:user1".to_string(),
        target_did: "did:plc:target1".to_string(),
        label: "high".to_string(),
        labeled_at: "2026-03-19T12:00:00Z".to_string(),
        notes: Some("known troll".to_string()),
    };
    assert_eq!(label.label, "high");
    assert_eq!(label.notes, Some("known troll".to_string()));
}

#[test]
fn user_label_notes_optional() {
    let label = UserLabel {
        user_did: "did:plc:user1".to_string(),
        target_did: "did:plc:target1".to_string(),
        label: "safe".to_string(),
        labeled_at: "2026-03-19T12:00:00Z".to_string(),
        notes: None,
    };
    assert!(label.notes.is_none());
}

#[test]
fn inferred_pair_fields_accessible() {
    let pair = InferredPair {
        id: 1,
        user_did: "did:plc:user1".to_string(),
        target_did: "did:plc:target1".to_string(),
        target_post_text: "fatphobia is overblown".to_string(),
        target_post_uri: "at://did:plc:target1/app.bsky.feed.post/abc".to_string(),
        user_post_text: "fatphobia in healthcare is real".to_string(),
        user_post_uri: "at://did:plc:user1/app.bsky.feed.post/xyz".to_string(),
        similarity: 0.82,
        context_score: Some(0.71),
        created_at: "2026-03-19T12:00:00Z".to_string(),
    };
    assert_eq!(pair.similarity, 0.82);
    assert_eq!(pair.context_score, Some(0.71));
}

#[test]
fn accuracy_metrics_computation() {
    let metrics = AccuracyMetrics {
        total_labeled: 50,
        exact_matches: 35,
        overscored: 10,
        underscored: 5,
        accuracy: 0.70,
    };
    assert_eq!(metrics.total_labeled, 50);
    assert!((metrics.accuracy - 0.70).abs() < f64::EPSILON);
}

#[test]
fn account_score_has_context_score() {
    use charcoal::db::models::AccountScore;
    let score = AccountScore {
        did: "did:plc:test".to_string(),
        handle: "test.bsky.social".to_string(),
        toxicity_score: Some(0.5),
        topic_overlap: Some(0.3),
        threat_score: Some(25.0),
        threat_tier: Some("Elevated".to_string()),
        posts_analyzed: Some(10),
        top_toxic_posts: vec![],
        scored_at: Some("2026-03-19T12:00:00Z".to_string()),
        behavioral_signals: None,
        context_score: Some(0.65),
    };
    assert_eq!(score.context_score, Some(0.65));
}

#[test]
fn amplification_event_has_new_fields() {
    use charcoal::db::models::AmplificationEvent;
    let event = AmplificationEvent {
        id: Some(1),
        event_type: "quote".to_string(),
        amplifier_did: "did:plc:amp".to_string(),
        amplifier_handle: "amp.bsky.social".to_string(),
        original_post_uri: "at://did:plc:user/app.bsky.feed.post/abc".to_string(),
        amplifier_post_uri: Some("at://did:plc:amp/app.bsky.feed.post/def".to_string()),
        amplifier_text: Some("look at this idiot".to_string()),
        detected_at: Some("2026-03-19T12:00:00Z".to_string()),
        followers_fetched: Some(false),
        followers_scored: Some(false),
        original_post_text: Some("fatphobia in healthcare is real".to_string()),
        context_score: Some(0.85),
    };
    assert_eq!(event.original_post_text, Some("fatphobia in healthcare is real".to_string()));
    assert_eq!(event.context_score, Some(0.85));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit_labels -v 2>&1 | head -20`
Expected: FAIL — `UserLabel`, `InferredPair`, `AccuracyMetrics` not defined; `AccountScore` missing `context_score`; `AmplificationEvent` missing new fields.

- [ ] **Step 3: Add new structs and modify existing structs in models.rs**

Read `src/db/models.rs` first. Then add/modify:

```rust
// Add to existing AccountScore struct:
pub context_score: Option<f64>,

// Add to existing AmplificationEvent struct:
pub original_post_text: Option<String>,
pub context_score: Option<f64>,

// New structs:
#[derive(Debug, Clone)]
pub struct UserLabel {
    pub user_did: String,
    pub target_did: String,
    pub label: String,
    pub labeled_at: String,
    pub notes: Option<String>,
}

#[derive(Debug, Clone)]
pub struct InferredPair {
    pub id: i64,
    pub user_did: String,
    pub target_did: String,
    pub target_post_text: String,
    pub target_post_uri: String,
    pub user_post_text: String,
    pub user_post_uri: String,
    pub similarity: f64,
    pub context_score: Option<f64>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct AccuracyMetrics {
    pub total_labeled: i64,
    pub exact_matches: i64,
    pub overscored: i64,
    pub underscored: i64,
    pub accuracy: f64,
}
```

- [ ] **Step 4: Fix all compilation errors from adding new fields**

The new fields on `AccountScore` and `AmplificationEvent` will break existing code that constructs these structs. Find and update every construction site:
- `src/scoring/profile.rs` — `build_profile()` returns `AccountScore`
- `src/db/sqlite.rs` — row deserialization for `AccountScore` and `AmplificationEvent`
- `src/db/postgres.rs` — row deserialization (feature-gated)
- `src/db/queries.rs` — all query result mappings
- `src/web/handlers/accounts.rs` — JSON serialization
- `src/web/handlers/events.rs` — JSON serialization
- `tests/composition.rs` — test AccountScore construction
- `tests/unit_scoring.rs` — test AccountScore construction

For each site: add `context_score: None` to AccountScore constructions, add `original_post_text: None, context_score: None` to AmplificationEvent constructions.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --test unit_labels -v`
Expected: All 6 tests PASS.

Run: `cargo test --features web`
Expected: All existing 232 tests still pass (no regressions).

- [ ] **Step 6: Commit**

```bash
git add src/db/models.rs tests/unit_labels.rs src/scoring/profile.rs src/db/sqlite.rs src/db/queries.rs src/web/handlers/accounts.rs src/web/handlers/events.rs tests/composition.rs tests/unit_scoring.rs
git commit -m 'feat: add contextual scoring data models

Add UserLabel, InferredPair, AccuracyMetrics structs.
Add context_score to AccountScore.
Add original_post_text and context_score to AmplificationEvent.
All existing tests pass with new fields defaulting to None.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

### Task 2: Schema Migration v5 (SQLite)

**Files:**
- Modify: `src/db/schema.rs`
- Test: Existing migration test pattern + manual verification

- [ ] **Step 1: Write failing test for v5 migration**

Add to `tests/unit_labels.rs`:

```rust
#[test]
fn schema_v5_creates_user_labels_table() {
    use rusqlite::Connection;
    use charcoal::db::schema::run_migrations;

    let conn = Connection::open_in_memory().unwrap();
    run_migrations(&conn).unwrap();

    // Verify user_labels table exists
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='user_labels'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "user_labels table should exist after v5 migration");
}

#[test]
fn schema_v5_creates_inferred_pairs_table() {
    use rusqlite::Connection;
    use charcoal::db::schema::run_migrations;

    let conn = Connection::open_in_memory().unwrap();
    run_migrations(&conn).unwrap();

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='inferred_pairs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "inferred_pairs table should exist after v5 migration");
}

#[test]
fn schema_v5_adds_context_score_to_account_scores() {
    use rusqlite::Connection;
    use charcoal::db::schema::run_migrations;

    let conn = Connection::open_in_memory().unwrap();
    run_migrations(&conn).unwrap();

    // Verify context_score column exists by inserting a row with it
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, context_score) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params!["did:plc:user1", "did:plc:test", "test.bsky.social", 0.75],
    )
    .unwrap();

    let score: f64 = conn
        .query_row(
            "SELECT context_score FROM account_scores WHERE did = ?1 AND user_did = ?2",
            rusqlite::params!["did:plc:test", "did:plc:user1"],
            |row| row.get(0),
        )
        .unwrap();
    assert!((score - 0.75).abs() < f64::EPSILON);
}

#[test]
fn schema_v5_adds_columns_to_amplification_events() {
    use rusqlite::Connection;
    use charcoal::db::schema::run_migrations;

    let conn = Connection::open_in_memory().unwrap();
    run_migrations(&conn).unwrap();

    conn.execute(
        "INSERT INTO amplification_events (user_did, event_type, amplifier_did, amplifier_handle, original_post_uri, original_post_text, context_score)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            "did:plc:user1", "quote", "did:plc:amp", "amp.bsky.social",
            "at://did:plc:user1/app.bsky.feed.post/abc",
            "my original post text", 0.85
        ],
    )
    .unwrap();

    let (text, score): (Option<String>, Option<f64>) = conn
        .query_row(
            "SELECT original_post_text, context_score FROM amplification_events WHERE amplifier_did = ?1",
            rusqlite::params!["did:plc:amp"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(text, Some("my original post text".to_string()));
    assert!((score.unwrap() - 0.85).abs() < f64::EPSILON);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit_labels schema_v5 -v 2>&1 | head -20`
Expected: FAIL — `run_migrations` doesn't include v5, tables/columns don't exist.

- [ ] **Step 3: Implement v5 migration in schema.rs**

Read `src/db/schema.rs` first. Follow the existing pattern from `migrate_v3_to_v4`. Add:

```rust
fn migrate_v4_to_v5(conn: &Connection) -> Result<()> {
    // Add columns to amplification_events
    conn.execute_batch(
        "ALTER TABLE amplification_events ADD COLUMN original_post_text TEXT;
         ALTER TABLE amplification_events ADD COLUMN context_score REAL;"
    )?;

    // Add column to account_scores
    conn.execute_batch(
        "ALTER TABLE account_scores ADD COLUMN context_score REAL;"
    )?;

    // Create user_labels table
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS user_labels (
            user_did TEXT NOT NULL,
            target_did TEXT NOT NULL,
            label TEXT NOT NULL,
            labeled_at TEXT NOT NULL,
            notes TEXT,
            PRIMARY KEY (user_did, target_did)
        );"
    )?;

    // Create inferred_pairs table
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS inferred_pairs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_did TEXT NOT NULL,
            target_did TEXT NOT NULL,
            target_post_text TEXT NOT NULL,
            target_post_uri TEXT NOT NULL,
            user_post_text TEXT NOT NULL,
            user_post_uri TEXT NOT NULL,
            similarity REAL NOT NULL,
            context_score REAL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_inferred_pairs_target
            ON inferred_pairs(user_did, target_did);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_inferred_pairs_dedup
            ON inferred_pairs(user_did, target_did, target_post_uri, user_post_uri);"
    )?;

    Ok(())
}
```

Then register it in `run_migrations()`:
```rust
run_migration(conn, 5, migrate_v4_to_v5)?;
```

Also add the new tables to `create_tables()` so fresh databases get them directly.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test unit_labels -v`
Expected: All 10 tests PASS (6 model + 4 schema).

Run: `cargo test --features web`
Expected: All existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add src/db/schema.rs tests/unit_labels.rs
git commit -m 'feat: add schema migration v5 for contextual scoring

New tables: user_labels, inferred_pairs (with dedup index).
New columns: account_scores.context_score,
amplification_events.original_post_text and context_score.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

### Task 3: Database Trait & SQLite Implementation

**Files:**
- Modify: `src/db/traits.rs`
- Modify: `src/db/sqlite.rs`
- Modify: `src/db/queries.rs`
- Test: `tests/unit_labels.rs` (add trait method tests)

- [ ] **Step 1: Write failing tests for new trait methods**

Add to `tests/unit_labels.rs`:

```rust
use charcoal::db::{self, SqliteDatabase};
use std::sync::Arc;

async fn setup_test_db() -> Arc<dyn db::traits::Database> {
    let db = SqliteDatabase::open_in_memory().await.unwrap();
    Arc::new(db)
}

#[tokio::test]
async fn upsert_and_get_user_label() {
    let db = setup_test_db().await;
    db.upsert_user("did:plc:user1", "user1.bsky.social").await.unwrap();

    db.upsert_user_label("did:plc:user1", "did:plc:target1", "high", Some("known troll"))
        .await
        .unwrap();

    let label = db.get_user_label("did:plc:user1", "did:plc:target1").await.unwrap();
    assert!(label.is_some());
    let label = label.unwrap();
    assert_eq!(label.label, "high");
    assert_eq!(label.notes, Some("known troll".to_string()));
}

#[tokio::test]
async fn upsert_user_label_overwrites() {
    let db = setup_test_db().await;
    db.upsert_user("did:plc:user1", "user1.bsky.social").await.unwrap();

    db.upsert_user_label("did:plc:user1", "did:plc:target1", "high", None)
        .await
        .unwrap();
    db.upsert_user_label("did:plc:user1", "did:plc:target1", "safe", Some("actually a friend"))
        .await
        .unwrap();

    let label = db.get_user_label("did:plc:user1", "did:plc:target1").await.unwrap();
    let label = label.unwrap();
    assert_eq!(label.label, "safe");
    assert_eq!(label.notes, Some("actually a friend".to_string()));
}

#[tokio::test]
async fn get_user_label_returns_none_when_missing() {
    let db = setup_test_db().await;
    let label = db.get_user_label("did:plc:user1", "did:plc:nobody").await.unwrap();
    assert!(label.is_none());
}

#[tokio::test]
async fn get_unlabeled_accounts_returns_scored_without_labels() {
    let db = setup_test_db().await;
    db.upsert_user("did:plc:user1", "user1.bsky.social").await.unwrap();

    // Insert two scored accounts
    let score1 = charcoal::db::models::AccountScore {
        did: "did:plc:a".to_string(),
        handle: "a.bsky.social".to_string(),
        toxicity_score: Some(0.8),
        topic_overlap: Some(0.5),
        threat_score: Some(40.0),
        threat_tier: Some("High".to_string()),
        posts_analyzed: Some(10),
        top_toxic_posts: vec![],
        scored_at: Some("2026-03-19T12:00:00Z".to_string()),
        behavioral_signals: None,
        context_score: None,
    };
    let score2 = charcoal::db::models::AccountScore {
        did: "did:plc:b".to_string(),
        handle: "b.bsky.social".to_string(),
        toxicity_score: Some(0.3),
        topic_overlap: Some(0.2),
        threat_score: Some(10.0),
        threat_tier: Some("Watch".to_string()),
        posts_analyzed: Some(5),
        top_toxic_posts: vec![],
        scored_at: Some("2026-03-19T12:00:00Z".to_string()),
        behavioral_signals: None,
        context_score: None,
    };
    db.upsert_account_score("did:plc:user1", &score1).await.unwrap();
    db.upsert_account_score("did:plc:user1", &score2).await.unwrap();

    // Label only the first one
    db.upsert_user_label("did:plc:user1", "did:plc:a", "high", None).await.unwrap();

    // Get unlabeled — should only return score2
    let unlabeled = db.get_unlabeled_accounts("did:plc:user1", 10).await.unwrap();
    assert_eq!(unlabeled.len(), 1);
    assert_eq!(unlabeled[0].did, "did:plc:b");
}

#[tokio::test]
async fn get_unlabeled_accounts_sorted_by_threat_score_desc() {
    let db = setup_test_db().await;
    db.upsert_user("did:plc:user1", "user1.bsky.social").await.unwrap();

    // Insert accounts with different scores
    for (did, score) in [("did:plc:low", 5.0), ("did:plc:high", 40.0), ("did:plc:mid", 20.0)] {
        let s = charcoal::db::models::AccountScore {
            did: did.to_string(),
            handle: format!("{}.bsky.social", did.split(':').last().unwrap()),
            toxicity_score: Some(0.5),
            topic_overlap: Some(0.3),
            threat_score: Some(score),
            threat_tier: Some("Watch".to_string()),
            posts_analyzed: Some(5),
            top_toxic_posts: vec![],
            scored_at: Some("2026-03-19T12:00:00Z".to_string()),
            behavioral_signals: None,
            context_score: None,
        };
        db.upsert_account_score("did:plc:user1", &s).await.unwrap();
    }

    let unlabeled = db.get_unlabeled_accounts("did:plc:user1", 10).await.unwrap();
    assert_eq!(unlabeled.len(), 3);
    assert_eq!(unlabeled[0].did, "did:plc:high");
    assert_eq!(unlabeled[1].did, "did:plc:mid");
    assert_eq!(unlabeled[2].did, "did:plc:low");
}

#[tokio::test]
async fn accuracy_metrics_correct() {
    let db = setup_test_db().await;
    db.upsert_user("did:plc:user1", "user1.bsky.social").await.unwrap();

    // Create accounts with predicted tiers, then label some differently
    let tiers = [
        ("did:plc:a", 40.0, "High", "high"),       // match
        ("did:plc:b", 20.0, "Elevated", "safe"),    // overscored
        ("did:plc:c", 5.0, "Low", "elevated"),      // underscored
        ("did:plc:d", 10.0, "Watch", "watch"),       // match
    ];
    for (did, score, tier, label) in tiers {
        let s = charcoal::db::models::AccountScore {
            did: did.to_string(),
            handle: format!("{}.bsky.social", did.split(':').last().unwrap()),
            toxicity_score: Some(0.5),
            topic_overlap: Some(0.3),
            threat_score: Some(score),
            threat_tier: Some(tier.to_string()),
            posts_analyzed: Some(5),
            top_toxic_posts: vec![],
            scored_at: Some("2026-03-19T12:00:00Z".to_string()),
            behavioral_signals: None,
            context_score: None,
        };
        db.upsert_account_score("did:plc:user1", &s).await.unwrap();
        db.upsert_user_label("did:plc:user1", did, label, None).await.unwrap();
    }

    let metrics = db.get_accuracy_metrics("did:plc:user1").await.unwrap();
    assert_eq!(metrics.total_labeled, 4);
    assert_eq!(metrics.exact_matches, 2);  // a and d match
    assert_eq!(metrics.overscored, 1);     // b: predicted Elevated, labeled safe
    assert_eq!(metrics.underscored, 1);    // c: predicted Low, labeled elevated
    assert!((metrics.accuracy - 0.5).abs() < f64::EPSILON);
}

#[tokio::test]
async fn inferred_pair_crud() {
    let db = setup_test_db().await;
    db.upsert_user("did:plc:user1", "user1.bsky.social").await.unwrap();

    let id = db.insert_inferred_pair(
        "did:plc:user1", "did:plc:target1",
        "fatphobia is overblown", "at://did:plc:target1/app.bsky.feed.post/abc",
        "fatphobia in healthcare is real", "at://did:plc:user1/app.bsky.feed.post/xyz",
        0.82, Some(0.71),
    ).await.unwrap();
    assert!(id > 0);

    let pairs = db.get_inferred_pairs("did:plc:user1", "did:plc:target1").await.unwrap();
    assert_eq!(pairs.len(), 1);
    assert!((pairs[0].similarity - 0.82).abs() < f64::EPSILON);
    assert_eq!(pairs[0].context_score, Some(0.71));
}

#[tokio::test]
async fn delete_inferred_pairs_removes_for_target() {
    let db = setup_test_db().await;
    db.upsert_user("did:plc:user1", "user1.bsky.social").await.unwrap();

    // Insert two pairs for same target
    db.insert_inferred_pair(
        "did:plc:user1", "did:plc:target1",
        "post1", "at://t1/p/1", "user post", "at://u/p/1", 0.8, None,
    ).await.unwrap();
    db.insert_inferred_pair(
        "did:plc:user1", "did:plc:target1",
        "post2", "at://t1/p/2", "user post 2", "at://u/p/2", 0.7, None,
    ).await.unwrap();

    // Insert pair for different target (should not be deleted)
    db.insert_inferred_pair(
        "did:plc:user1", "did:plc:other",
        "other post", "at://other/p/1", "user post", "at://u/p/1", 0.6, None,
    ).await.unwrap();

    db.delete_inferred_pairs("did:plc:user1", "did:plc:target1").await.unwrap();

    let pairs_target = db.get_inferred_pairs("did:plc:user1", "did:plc:target1").await.unwrap();
    assert_eq!(pairs_target.len(), 0);

    let pairs_other = db.get_inferred_pairs("did:plc:user1", "did:plc:other").await.unwrap();
    assert_eq!(pairs_other.len(), 1);
}

#[tokio::test]
async fn insert_amplification_event_with_new_fields() {
    let db = setup_test_db().await;
    db.upsert_user("did:plc:user1", "user1.bsky.social").await.unwrap();

    let id = db.insert_amplification_event(
        "did:plc:user1", "quote", "did:plc:amp", "amp.bsky.social",
        "at://did:plc:user1/app.bsky.feed.post/abc",
        Some("at://did:plc:amp/app.bsky.feed.post/def"),
        Some("look at this idiot"),
        Some("fatphobia in healthcare is real"),
        Some(0.85),
    ).await.unwrap();
    assert!(id > 0);

    let events = db.get_recent_events("did:plc:user1", 10).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].original_post_text, Some("fatphobia in healthcare is real".to_string()));
    assert_eq!(events[0].context_score, Some(0.85));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit_labels -v 2>&1 | head -30`
Expected: FAIL — trait methods don't exist yet.

- [ ] **Step 3: Update Database trait with new method signatures**

Read `src/db/traits.rs`. Modify `insert_amplification_event` to add two new parameters. Add 6 new trait methods:

```rust
// Modified signature (add original_post_text and context_score):
async fn insert_amplification_event(
    &self, user_did: &str, event_type: &str, amplifier_did: &str,
    amplifier_handle: &str, original_post_uri: &str,
    amplifier_post_uri: Option<&str>, amplifier_text: Option<&str>,
    original_post_text: Option<&str>, context_score: Option<f64>,
) -> Result<i64>;

// New methods:
async fn upsert_user_label(
    &self, user_did: &str, target_did: &str, label: &str, notes: Option<&str>,
) -> Result<()>;
async fn get_user_label(
    &self, user_did: &str, target_did: &str,
) -> Result<Option<UserLabel>>;
async fn get_unlabeled_accounts(
    &self, user_did: &str, limit: i64,
) -> Result<Vec<AccountScore>>;
async fn get_accuracy_metrics(
    &self, user_did: &str,
) -> Result<AccuracyMetrics>;
async fn delete_inferred_pairs(
    &self, user_did: &str, target_did: &str,
) -> Result<()>;
async fn insert_inferred_pair(
    &self, user_did: &str, target_did: &str,
    target_post_text: &str, target_post_uri: &str,
    user_post_text: &str, user_post_uri: &str,
    similarity: f64, context_score: Option<f64>,
) -> Result<i64>;
async fn get_inferred_pairs(
    &self, user_did: &str, target_did: &str,
) -> Result<Vec<InferredPair>>;
```

- [ ] **Step 4: Implement all new methods in SqliteDatabase**

Read `src/db/sqlite.rs` and `src/db/queries.rs`. Add implementations for all 7 new/modified methods. Key SQL:

- `upsert_user_label`: `INSERT INTO user_labels ... ON CONFLICT(user_did, target_did) DO UPDATE SET ...`
- `get_user_label`: `SELECT * FROM user_labels WHERE user_did = ? AND target_did = ?`
- `get_unlabeled_accounts`: `SELECT a.* FROM account_scores a LEFT JOIN user_labels l ON a.user_did = l.user_did AND a.did = l.target_did WHERE a.user_did = ? AND l.label IS NULL ORDER BY a.threat_score DESC LIMIT ?`
- `get_accuracy_metrics`: Join `user_labels` on `account_scores`, compare `threat_tier` (lowercased) to `label`, count matches/overscored/underscored
- `delete_inferred_pairs`: `DELETE FROM inferred_pairs WHERE user_did = ? AND target_did = ?`
- `insert_inferred_pair`: `INSERT INTO inferred_pairs ...`
- `get_inferred_pairs`: `SELECT * FROM inferred_pairs WHERE user_did = ? AND target_did = ?`

Also update `insert_amplification_event` to include the two new columns, and update `get_recent_events` to read them back.

- [ ] **Step 5: Update all callers of insert_amplification_event**

The signature change adds 2 new parameters. Find all call sites and add `None, None` for the new params (they'll be populated later when the NLI pipeline is integrated):
- `src/pipeline/amplification.rs` — main caller
- Any test helpers or test code that calls it

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test unit_labels -v`
Expected: All tests PASS.

Run: `cargo test --features web`
Expected: All 232+ tests still pass.

- [ ] **Step 7: Commit**

```bash
git add src/db/traits.rs src/db/sqlite.rs src/db/queries.rs src/pipeline/amplification.rs tests/unit_labels.rs
git commit -m 'feat: implement Database trait methods for labels and inferred pairs

Add upsert_user_label, get_user_label, get_unlabeled_accounts,
get_accuracy_metrics, insert/get/delete_inferred_pairs.
Update insert_amplification_event with original_post_text and
context_score parameters.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

## Chunk 2: NLI Model & Contextual Scoring (Tasks 4-6)

### Task 4: NLI Model Download

**Files:**
- Modify: `src/toxicity/download.rs`
- Test: Manual verification (model download is I/O-bound)

- [ ] **Step 1: Write a test that checks NLI model file detection**

Add `tests/unit_nli.rs`:

```rust
//! Unit tests for NLI model integration.

use std::path::Path;

#[test]
fn nli_files_present_returns_false_for_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    assert!(!charcoal::toxicity::download::nli_files_present(dir.path()));
}

#[test]
fn nli_files_present_returns_true_when_both_files_exist() {
    let dir = tempfile::tempdir().unwrap();
    let nli_dir = dir.path().join("nli-deberta-v3-xsmall");
    std::fs::create_dir_all(&nli_dir).unwrap();
    std::fs::write(nli_dir.join("model_quantized.onnx"), b"fake model").unwrap();
    std::fs::write(nli_dir.join("tokenizer.json"), b"fake tokenizer").unwrap();
    assert!(charcoal::toxicity::download::nli_files_present(dir.path()));
}

#[test]
fn nli_files_present_returns_false_when_model_missing() {
    let dir = tempfile::tempdir().unwrap();
    let nli_dir = dir.path().join("nli-deberta-v3-xsmall");
    std::fs::create_dir_all(&nli_dir).unwrap();
    std::fs::write(nli_dir.join("tokenizer.json"), b"fake tokenizer").unwrap();
    // model_quantized.onnx missing
    assert!(!charcoal::toxicity::download::nli_files_present(dir.path()));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit_nli -v`
Expected: FAIL — `nli_files_present` function doesn't exist.

- [ ] **Step 3: Implement nli_files_present and download_nli_model**

Read `src/toxicity/download.rs`. Follow the pattern from `model_files_present()` and `download_model()`. Add:

```rust
pub fn nli_files_present(dir: &Path) -> bool {
    let nli_dir = dir.join("nli-deberta-v3-xsmall");
    nli_dir.join("model_quantized.onnx").exists()
        && nli_dir.join("tokenizer.json").exists()
}

pub async fn download_nli_model(dir: &Path) -> Result<()> {
    let nli_dir = dir.join("nli-deberta-v3-xsmall");
    std::fs::create_dir_all(&nli_dir)?;

    // Download from Xenova/nli-deberta-v3-xsmall on HuggingFace
    let base_url = "https://huggingface.co/Xenova/nli-deberta-v3-xsmall/resolve/main/onnx";
    download_file(
        &format!("{}/model_quantized.onnx", base_url),
        &nli_dir.join("model_quantized.onnx"),
    ).await?;

    let tokenizer_url = "https://huggingface.co/Xenova/nli-deberta-v3-xsmall/resolve/main/tokenizer.json";
    download_file(tokenizer_url, &nli_dir.join("tokenizer.json")).await?;

    Ok(())
}
```

Update the main `download_model()` function to also call `download_nli_model()`.

Update the `serve` startup auto-download check to include `nli_files_present()`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test unit_nli -v`
Expected: All 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/toxicity/download.rs tests/unit_nli.rs
git commit -m 'feat: add NLI model download support

Download Xenova/nli-deberta-v3-xsmall quantized ONNX (~87MB)
alongside toxicity and embedding models. Add nli_files_present
check for serve startup auto-download.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

### Task 5: NLI Inference Module

**Files:**
- Create: `src/scoring/nli.rs`
- Modify: `src/scoring/mod.rs`
- Test: `tests/unit_nli.rs` (extend)

- [ ] **Step 1: Write failing tests for hostility score computation**

The NLI model produces entailment scores per hypothesis. We test the *hostility derivation logic* with mock entailment scores (not the model itself — that's an integration test for staging).

Add to `tests/unit_nli.rs`:

```rust
use charcoal::scoring::nli::{
    compute_hostility_score, HypothesisScores,
};

#[test]
fn hostile_quote_scores_high() {
    let scores = HypothesisScores {
        attack: 0.85,
        contempt: 0.60,
        misrepresent: 0.30,
        good_faith_disagree: 0.05,
        support: 0.02,
    };
    let hostility = compute_hostility_score(&scores);
    // max(0.85, 0.60, 0.30) - max(0.05*0.5, 0.02*0.8) = 0.85 - 0.025 = 0.825
    assert!(hostility > 0.8, "Hostile quote should score high, got {}", hostility);
    assert!(hostility <= 1.0);
}

#[test]
fn supportive_reply_scores_low() {
    let scores = HypothesisScores {
        attack: 0.05,
        contempt: 0.03,
        misrepresent: 0.02,
        good_faith_disagree: 0.10,
        support: 0.90,
    };
    let hostility = compute_hostility_score(&scores);
    // max(0.05, 0.03, 0.02) - max(0.10*0.5, 0.90*0.8) = 0.05 - 0.72 = -0.67 → clamp → 0.0
    assert!(hostility < 0.01, "Supportive reply should score near zero, got {}", hostility);
}

#[test]
fn good_faith_disagreement_scores_moderate() {
    let scores = HypothesisScores {
        attack: 0.20,
        contempt: 0.15,
        misrepresent: 0.10,
        good_faith_disagree: 0.70,
        support: 0.05,
    };
    let hostility = compute_hostility_score(&scores);
    // max(0.20, 0.15, 0.10) - max(0.70*0.5, 0.05*0.8) = 0.20 - 0.35 = -0.15 → clamp → 0.0
    assert!(hostility < 0.01, "Good-faith disagreement should score low, got {}", hostility);
}

#[test]
fn concern_trolling_with_contempt_scores_moderate() {
    let scores = HypothesisScores {
        attack: 0.30,
        contempt: 0.65,
        misrepresent: 0.40,
        good_faith_disagree: 0.20,
        support: 0.15,
    };
    let hostility = compute_hostility_score(&scores);
    // max(0.30, 0.65, 0.40) - max(0.20*0.5, 0.15*0.8) = 0.65 - 0.12 = 0.53
    assert!(hostility > 0.4, "Concern trolling should score moderate+, got {}", hostility);
    assert!(hostility < 0.7);
}

#[test]
fn neutral_response_scores_near_zero() {
    let scores = HypothesisScores {
        attack: 0.10,
        contempt: 0.08,
        misrepresent: 0.05,
        good_faith_disagree: 0.15,
        support: 0.12,
    };
    let hostility = compute_hostility_score(&scores);
    // max(0.10, 0.08, 0.05) - max(0.15*0.5, 0.12*0.8) = 0.10 - 0.096 = 0.004
    assert!(hostility < 0.1, "Neutral response should score near zero, got {}", hostility);
}

#[test]
fn hostility_score_clamped_to_zero_one() {
    // All hostile signals maxed
    let scores = HypothesisScores {
        attack: 1.0,
        contempt: 1.0,
        misrepresent: 1.0,
        good_faith_disagree: 0.0,
        support: 0.0,
    };
    let hostility = compute_hostility_score(&scores);
    assert!(hostility <= 1.0);
    assert!(hostility >= 0.0);

    // All supportive signals maxed
    let scores_support = HypothesisScores {
        attack: 0.0,
        contempt: 0.0,
        misrepresent: 0.0,
        good_faith_disagree: 1.0,
        support: 1.0,
    };
    let hostility_support = compute_hostility_score(&scores_support);
    assert!(hostility_support >= 0.0);
    assert!(hostility_support <= 1.0);
}

#[test]
fn max_context_score_from_multiple_pairs() {
    use charcoal::scoring::nli::max_context_score;
    let scores = vec![0.3, 0.7, 0.5, 0.2];
    assert!((max_context_score(&scores) - 0.7).abs() < f64::EPSILON);
}

#[test]
fn max_context_score_empty_returns_none() {
    use charcoal::scoring::nli::max_context_score_opt;
    let scores: Vec<f64> = vec![];
    assert!(max_context_score_opt(&scores).is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit_nli -v`
Expected: FAIL — `scoring::nli` module doesn't exist.

- [ ] **Step 3: Implement NLI module**

Create `src/scoring/nli.rs`:

```rust
//! NLI-based contextual hostility scoring.
//!
//! Uses a DeBERTa-v3-xsmall cross-encoder to score text pairs
//! for hostile engagement patterns (attack, contempt, misrepresentation)
//! vs. supportive/good-faith signals.

/// Raw entailment scores from running NLI hypotheses on a text pair.
#[derive(Debug, Clone)]
pub struct HypothesisScores {
    pub attack: f64,
    pub contempt: f64,
    pub misrepresent: f64,
    pub good_faith_disagree: f64,
    pub support: f64,
}

/// Compute contextual hostility score from NLI hypothesis entailment scores.
///
/// Formula:
///   hostile_signal = max(attack, contempt, misrepresent)
///   supportive_signal = max(good_faith * 0.5, support * 0.8)
///   hostility = clamp(hostile_signal - supportive_signal, 0.0, 1.0)
pub fn compute_hostility_score(scores: &HypothesisScores) -> f64 {
    let hostile_signal = scores.attack.max(scores.contempt).max(scores.misrepresent);
    let supportive_signal = (scores.good_faith_disagree * 0.5)
        .max(scores.support * 0.8);
    (hostile_signal - supportive_signal).clamp(0.0, 1.0)
}

/// Return the maximum context score from multiple pairs.
/// Used when an account has multiple interactions — one hostile
/// interaction is sufficient signal.
pub fn max_context_score(scores: &[f64]) -> f64 {
    scores.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

/// Return the maximum context score, or None if no scores provided.
pub fn max_context_score_opt(scores: &[f64]) -> Option<f64> {
    if scores.is_empty() {
        None
    } else {
        Some(max_context_score(scores))
    }
}

/// The 5 hypothesis templates used for NLI inference.
pub const HYPOTHESES: &[(&str, &str)] = &[
    ("attack", "The second text attacks or mocks the author of the first text"),
    ("contempt", "The second text dismisses the first text with contempt"),
    ("misrepresent", "The second text misrepresents what the first text says"),
    ("good_faith_disagree", "The second text respectfully disagrees with the first text"),
    ("support", "The second text supports or agrees with the first text"),
];
```

Then add the ONNX model loading and inference functions. These use `ort` and `tokenizers` following the same pattern as `src/toxicity/onnx.rs`:

```rust
use anyhow::Result;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct NliScorer {
    session: Arc<Mutex<ort::Session>>,
    tokenizer: tokenizers::Tokenizer,
}

impl NliScorer {
    pub fn new(model_dir: &Path) -> Result<Self> {
        let nli_dir = model_dir.join("nli-deberta-v3-xsmall");
        let session = ort::Session::builder()?
            .with_optimization_level(ort::GraphOptimizationLevel::Level3)?
            .commit_from_file(nli_dir.join("model_quantized.onnx"))?;
        let tokenizer = tokenizers::Tokenizer::from_file(
            nli_dir.join("tokenizer.json")
        ).map_err(|e| anyhow::anyhow!("Failed to load NLI tokenizer: {}", e))?;

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            tokenizer,
        })
    }

    /// Score a single text pair against one hypothesis.
    /// Returns the entailment probability (0.0-1.0).
    async fn score_hypothesis(
        &self, premise: &str, hypothesis: &str,
    ) -> Result<f64> {
        // NLI cross-encoders take [CLS] premise [SEP] hypothesis [SEP]
        let encoding = self.tokenizer
            .encode((premise, hypothesis), true)
            .map_err(|e| anyhow::anyhow!("Tokenization failed: {}", e))?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&id| id as i64).collect();
        let attention_mask: Vec<i64> = encoding.get_attention_mask().iter().map(|&m| m as i64).collect();
        let token_type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&t| t as i64).collect();
        let seq_len = input_ids.len();

        let session = self.session.lock().await;
        let outputs = tokio::task::spawn_blocking({
            let input_ids = input_ids.clone();
            let attention_mask = attention_mask.clone();
            let token_type_ids = token_type_ids.clone();
            // Note: actual spawn_blocking needs owned session or Arc pattern
            // Implementation will follow the existing onnx.rs pattern
            move || -> Result<Vec<f32>> {
                // Build tensors and run inference
                // Output: [contradiction, neutral, entailment] logits
                // Return softmax probabilities
                todo!("Implement with actual ort session — see Step 4")
            }
        }).await??;

        // outputs[2] is the entailment probability
        Ok(outputs[2] as f64)
    }

    /// Score a text pair against all 5 hypotheses and compute hostility.
    pub async fn score_pair(
        &self, original_text: &str, response_text: &str,
    ) -> Result<f64> {
        let premise = format!("{} [SEP] {}", original_text, response_text);

        let mut hypothesis_scores = HypothesisScores {
            attack: 0.0, contempt: 0.0, misrepresent: 0.0,
            good_faith_disagree: 0.0, support: 0.0,
        };

        for (name, hypothesis) in HYPOTHESES {
            let score = self.score_hypothesis(&premise, hypothesis).await?;
            match *name {
                "attack" => hypothesis_scores.attack = score,
                "contempt" => hypothesis_scores.contempt = score,
                "misrepresent" => hypothesis_scores.misrepresent = score,
                "good_faith_disagree" => hypothesis_scores.good_faith_disagree = score,
                "support" => hypothesis_scores.support = score,
                _ => {}
            }
        }

        Ok(compute_hostility_score(&hypothesis_scores))
    }
}
```

Add `pub mod nli;` to `src/scoring/mod.rs`.

**Note:** The `score_hypothesis` method's `spawn_blocking` body needs the actual ort tensor construction. Follow the exact pattern from `src/toxicity/onnx.rs` — create ndarray tensors from input_ids/attention_mask/token_type_ids, run the session, apply softmax to logits. The implementer should read `src/toxicity/onnx.rs` line by line and adapt.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test unit_nli -v`
Expected: All 11 tests PASS (3 download + 8 hostility).

- [ ] **Step 5: Commit**

```bash
git add src/scoring/nli.rs src/scoring/mod.rs tests/unit_nli.rs
git commit -m 'feat: add NLI inference module for contextual hostility scoring

Compute hostility from 5 NLI hypotheses (attack, contempt,
misrepresent, good-faith disagree, support). Pure function
tests for score derivation. ONNX inference via ort + tokenizers.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

### Task 6: Blended Scoring Formula & Benign Gate Bypass

**Files:**
- Modify: `src/scoring/threat.rs`
- Modify: `src/scoring/behavioral.rs`
- Test: `tests/unit_scoring.rs` (extend), `tests/unit_behavioral.rs` (extend)

- [ ] **Step 1: Write failing tests for blended scoring formula**

Add to `tests/unit_scoring.rs`:

```rust
#[test]
fn blended_score_uses_context_when_available() {
    use charcoal::scoring::threat::{compute_threat_score_contextual, ThreatWeights};

    let weights = ThreatWeights::default();
    // toxicity=0.3, context_score=0.8, overlap=0.5
    // blended = 0.3 * 0.6 + 0.8 * 0.4 = 0.18 + 0.32 = 0.50
    // threat = 0.50 * 70 * (1 + 0.5 * 1.5) = 35.0 * 1.75 = 61.25
    let (score, tier) = compute_threat_score_contextual(0.3, 0.5, Some(0.8), &weights);
    assert!(score > 50.0, "Blended score should be high when context hostile, got {}", score);
    assert_eq!(tier.as_str(), "High");
}

#[test]
fn blended_score_falls_back_without_context() {
    use charcoal::scoring::threat::{compute_threat_score, compute_threat_score_contextual, ThreatWeights};

    let weights = ThreatWeights::default();
    // Without context: should produce same result as compute_threat_score
    let (score_ctx, tier_ctx) = compute_threat_score_contextual(0.5, 0.3, None, &weights);
    let (score_orig, tier_orig) = compute_threat_score(0.5, 0.3, &weights);
    assert!((score_ctx - score_orig).abs() < f64::EPSILON);
    assert_eq!(tier_ctx.as_str(), tier_orig.as_str());
}

#[test]
fn supportive_context_lowers_threat() {
    use charcoal::scoring::threat::{compute_threat_score, compute_threat_score_contextual, ThreatWeights};

    let weights = ThreatWeights::default();
    // toxicity=0.5, context_score=0.05 (supportive), overlap=0.5
    // blended = 0.5 * 0.6 + 0.05 * 0.4 = 0.30 + 0.02 = 0.32
    let (with_ctx, _) = compute_threat_score_contextual(0.5, 0.5, Some(0.05), &weights);
    let (without_ctx, _) = compute_threat_score(0.5, 0.5, &weights);
    assert!(with_ctx < without_ctx, "Supportive context should lower score: {} vs {}", with_ctx, without_ctx);
}
```

- [ ] **Step 2: Write failing tests for benign gate bypass**

Add to `tests/unit_behavioral.rs`:

```rust
#[test]
fn benign_gate_bypassed_when_context_score_high() {
    use charcoal::scoring::behavioral::apply_behavioral_modifier_contextual;

    // Account looks benign in isolation (low quote/reply ratio, not pile-on, good engagement)
    // But context_score is 0.7 (hostile in direct interactions)
    let (score, benign_gate) = apply_behavioral_modifier_contextual(
        30.0,    // raw_score (Elevated)
        0.05,    // quote_ratio (low — benign)
        0.10,    // reply_ratio (low — benign)
        false,   // pile_on (no)
        5.0,     // avg_engagement (above median)
        3.0,     // median_engagement
        Some(0.7), // context_score — HIGH, should bypass gate
    );
    // Without context, benign gate would cap at 12.0
    // With high context_score, gate should be bypassed
    assert!(score > 12.0, "Benign gate should be bypassed with high context_score, got {}", score);
    assert!(!benign_gate, "benign_gate should be false when context bypasses it");
}

#[test]
fn benign_gate_still_applies_when_context_score_low() {
    use charcoal::scoring::behavioral::apply_behavioral_modifier_contextual;

    // Benign account with low context_score — gate should apply
    let (score, benign_gate) = apply_behavioral_modifier_contextual(
        30.0, 0.05, 0.10, false, 5.0, 3.0,
        Some(0.3), // context_score below 0.5 threshold
    );
    assert_eq!(score, 12.0, "Benign gate should apply when context_score < 0.5");
    assert!(benign_gate);
}

#[test]
fn benign_gate_applies_when_no_context_score() {
    use charcoal::scoring::behavioral::apply_behavioral_modifier_contextual;

    // No context data — existing behavior preserved
    let (score, benign_gate) = apply_behavioral_modifier_contextual(
        30.0, 0.05, 0.10, false, 5.0, 3.0,
        None,
    );
    assert_eq!(score, 12.0);
    assert!(benign_gate);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test unit_scoring blended -v && cargo test --test unit_behavioral benign_gate_bypassed -v`
Expected: FAIL — new functions don't exist.

- [ ] **Step 4: Implement compute_threat_score_contextual**

Read `src/scoring/threat.rs`. Add new function alongside existing `compute_threat_score` (don't modify the original — it's used in tests):

```rust
pub fn compute_threat_score_contextual(
    toxicity: f64,
    topic_overlap: f64,
    context_score: Option<f64>,
    weights: &ThreatWeights,
) -> (f64, ThreatTier) {
    let effective_toxicity = match context_score {
        Some(ctx) => {
            let pair_weight = 0.4;
            toxicity * (1.0 - pair_weight) + ctx * pair_weight
        }
        None => toxicity,
    };
    compute_threat_score(effective_toxicity, topic_overlap, weights)
}
```

- [ ] **Step 5: Implement apply_behavioral_modifier_contextual**

Read `src/scoring/behavioral.rs`. Add new function that wraps existing logic with context bypass:

```rust
pub fn apply_behavioral_modifier_contextual(
    raw_score: f64,
    quote_ratio: f64,
    reply_ratio: f64,
    pile_on: bool,
    avg_engagement: f64,
    median_engagement: f64,
    context_score: Option<f64>,
) -> (f64, bool) {
    // Bypass benign gate if contextual evidence shows hostility
    let context_overrides_gate = context_score
        .map(|cs| cs >= 0.5)
        .unwrap_or(false);

    if context_overrides_gate {
        // Skip benign gate check, but still apply hostile multiplier
        let boost = compute_behavioral_boost(quote_ratio, reply_ratio, pile_on);
        (raw_score * boost, false)
    } else {
        apply_behavioral_modifier(
            raw_score, quote_ratio, reply_ratio, pile_on,
            avg_engagement, median_engagement,
        )
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --test unit_scoring -v && cargo test --test unit_behavioral -v`
Expected: All tests PASS (new + existing).

Run: `cargo test --features web`
Expected: Full suite passes.

- [ ] **Step 7: Commit**

```bash
git add src/scoring/threat.rs src/scoring/behavioral.rs tests/unit_scoring.rs tests/unit_behavioral.rs
git commit -m 'feat: add blended scoring formula and benign gate bypass

compute_threat_score_contextual blends toxicity with context_score
(40% weight) when pair data available. Benign gate bypassed when
context_score >= 0.5 (hostile in direct interactions despite
benign-looking isolation metrics).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

## Chunk 3: Engagement Detection Expansion (Tasks 7-9)

### Task 7: Like Detection via Constellation

**Files:**
- Create: `src/bluesky/likes.rs`
- Modify: `src/constellation/client.rs`
- Modify: `src/bluesky/mod.rs`
- Test: `tests/unit_nli.rs` (extend with like detection tests) or new `tests/unit_likes.rs`

- [ ] **Step 1: Write failing tests for Constellation likes query**

Create `tests/unit_likes.rs` or add to existing constellation tests:

```rust
//! Tests for like detection.

#[test]
fn likes_source_path_is_correct() {
    // The Constellation source path for likes should be:
    // app.bsky.feed.like:subject.uri
    assert_eq!(
        charcoal::constellation::client::LIKES_SOURCE,
        "app.bsky.feed.like:subject.uri"
    );
}
```

This is a minimal test — the actual Constellation query uses the same `get_backlinks` method. The real validation happens in staging when we confirm Constellation indexes likes. If it doesn't, we fall back to the public API `getLikes`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test unit_likes -v`
Expected: FAIL — `LIKES_SOURCE` constant doesn't exist.

- [ ] **Step 3: Add likes source constant and query method to ConstellationClient**

Read `src/constellation/client.rs`. Add:

```rust
pub const LIKES_SOURCE: &str = "app.bsky.feed.like:subject.uri";

impl ConstellationClient {
    /// Find accounts that liked the given post URIs.
    pub async fn find_likers(
        &self, post_uris: &[String],
    ) -> Vec<AmplificationNotification> {
        let mut results = Vec::new();
        for uri in post_uris {
            match self.get_backlinks(uri, LIKES_SOURCE, 100).await {
                Ok(response) => {
                    for record in response.records {
                        results.push(AmplificationNotification {
                            event_type: "like".to_string(),
                            amplifier_did: record.did.clone(),
                            amplifier_handle: String::new(), // resolved later
                            original_post_uri: uri.clone(),
                            amplifier_post_uri: None, // likes don't have their own post
                            indexed_at: None,
                        });
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to get likes for {}: {}", uri, e);
                }
            }
        }
        // Dedup by (amplifier_did, original_post_uri)
        results.sort_by(|a, b| (&a.amplifier_did, &a.original_post_uri)
            .cmp(&(&b.amplifier_did, &b.original_post_uri)));
        results.dedup_by(|a, b| a.amplifier_did == b.amplifier_did
            && a.original_post_uri == b.original_post_uri);
        results
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test unit_likes -v`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/constellation/client.rs tests/unit_likes.rs src/bluesky/mod.rs
git commit -m 'feat: add like detection via Constellation backlinks

Query app.bsky.feed.like:subject.uri for likers of protected
user posts. Deduplicates by (did, post_uri). Falls back gracefully
if Constellation does not index likes.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

### Task 8: Drive-By Reply Detection

**Files:**
- Create: `src/bluesky/replies.rs`
- Modify: `src/bluesky/mod.rs`
- Test: `tests/unit_replies.rs` (new)

- [ ] **Step 1: Write failing tests for reply filtering**

Create `tests/unit_replies.rs`:

```rust
//! Tests for drive-by reply detection.

use charcoal::bluesky::replies::filter_drive_by_replies;
use std::collections::HashSet;

#[test]
fn filters_out_followed_accounts() {
    let follows: HashSet<String> = ["did:plc:friend1", "did:plc:friend2"]
        .iter().map(|s| s.to_string()).collect();

    let reply_dids = vec![
        "did:plc:friend1".to_string(),  // followed — should be filtered
        "did:plc:stranger".to_string(), // not followed — drive-by
        "did:plc:friend2".to_string(),  // followed — should be filtered
        "did:plc:rando".to_string(),    // not followed — drive-by
    ];

    let drive_bys = filter_drive_by_replies(&reply_dids, &follows);
    assert_eq!(drive_bys.len(), 2);
    assert!(drive_bys.contains(&"did:plc:stranger".to_string()));
    assert!(drive_bys.contains(&"did:plc:rando".to_string()));
}

#[test]
fn empty_follows_treats_all_as_drive_by() {
    let follows: HashSet<String> = HashSet::new();
    let reply_dids = vec!["did:plc:a".to_string(), "did:plc:b".to_string()];
    let drive_bys = filter_drive_by_replies(&reply_dids, &follows);
    assert_eq!(drive_bys.len(), 2);
}

#[test]
fn empty_replies_returns_empty() {
    let follows: HashSet<String> = ["did:plc:friend"].iter().map(|s| s.to_string()).collect();
    let reply_dids: Vec<String> = vec![];
    let drive_bys = filter_drive_by_replies(&reply_dids, &follows);
    assert!(drive_bys.is_empty());
}

#[test]
fn filters_out_protected_user_self_replies() {
    use charcoal::bluesky::replies::filter_drive_by_replies_excluding_self;

    let follows: HashSet<String> = HashSet::new();
    let protected_did = "did:plc:protected";
    let reply_dids = vec![
        "did:plc:protected".to_string(),  // self-reply — exclude
        "did:plc:stranger".to_string(),   // drive-by
    ];

    let drive_bys = filter_drive_by_replies_excluding_self(&reply_dids, &follows, protected_did);
    assert_eq!(drive_bys.len(), 1);
    assert_eq!(drive_bys[0], "did:plc:stranger");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit_replies -v`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement reply detection module**

Create `src/bluesky/replies.rs`:

```rust
//! Drive-by reply detection.
//!
//! Fetches reply threads on the protected user's posts and filters
//! out followed accounts. Remaining repliers are "drive-by" candidates
//! for scoring.

use std::collections::HashSet;
use anyhow::Result;
use crate::bluesky::client::PublicAtpClient;

/// Filter reply DIDs to only non-followed accounts.
pub fn filter_drive_by_replies(
    reply_dids: &[String],
    follows: &HashSet<String>,
) -> Vec<String> {
    reply_dids.iter()
        .filter(|did| !follows.contains(did.as_str()))
        .cloned()
        .collect()
}

/// Filter reply DIDs, also excluding the protected user's own DID.
pub fn filter_drive_by_replies_excluding_self(
    reply_dids: &[String],
    follows: &HashSet<String>,
    protected_did: &str,
) -> Vec<String> {
    reply_dids.iter()
        .filter(|did| {
            did.as_str() != protected_did && !follows.contains(did.as_str())
        })
        .cloned()
        .collect()
}

/// Fetch the protected user's follows list (paginated).
/// Returns a HashSet of followed DIDs for fast lookup.
pub async fn fetch_follows_set(
    client: &PublicAtpClient,
    protected_did: &str,
) -> Result<HashSet<String>> {
    let mut follows = HashSet::new();
    let mut cursor: Option<String> = None;

    loop {
        let url = format!(
            "https://public.api.bsky.app/xrpc/app.bsky.graph.getFollows?actor={}&limit=100{}",
            protected_did,
            cursor.as_ref().map(|c| format!("&cursor={}", c)).unwrap_or_default(),
        );
        let resp: serde_json::Value = client.client
            .get(&url)
            .send().await?
            .json().await?;

        if let Some(follows_arr) = resp["follows"].as_array() {
            for follow in follows_arr {
                if let Some(did) = follow["did"].as_str() {
                    follows.insert(did.to_string());
                }
            }
        }

        cursor = resp["cursor"].as_str().map(String::from);
        if cursor.is_none() {
            break;
        }
    }

    Ok(follows)
}

/// Fetch direct replies to a post via getPostThread.
/// Returns Vec of (replier_did, reply_text, reply_uri).
pub async fn fetch_replies_to_post(
    client: &PublicAtpClient,
    post_uri: &str,
) -> Result<Vec<(String, String, String)>> {
    let url = format!(
        "https://public.api.bsky.app/xrpc/app.bsky.feed.getPostThread?uri={}&depth=1",
        percent_encoding::utf8_percent_encode(post_uri, percent_encoding::NON_ALPHANUMERIC),
    );
    let resp: serde_json::Value = client.client
        .get(&url)
        .send().await?
        .json().await?;

    let mut replies = Vec::new();
    if let Some(thread_replies) = resp["thread"]["replies"].as_array() {
        for reply in thread_replies {
            let did = reply["post"]["author"]["did"].as_str().unwrap_or_default();
            let text = reply["post"]["record"]["text"].as_str().unwrap_or_default();
            let uri = reply["post"]["uri"].as_str().unwrap_or_default();
            if !did.is_empty() && !text.is_empty() {
                replies.push((did.to_string(), text.to_string(), uri.to_string()));
            }
        }
    }

    Ok(replies)
}
```

Add `pub mod replies;` to `src/bluesky/mod.rs`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --test unit_replies -v`
Expected: All 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add src/bluesky/replies.rs src/bluesky/mod.rs tests/unit_replies.rs
git commit -m 'feat: add drive-by reply detection

Filter reply threads to find non-followed repliers. Fetch
follows list via paginated getFollows, fetch replies via
getPostThread depth=1. Pure filtering functions tested.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

### Task 9: Expand Amplification Pipeline

**Files:**
- Modify: `src/pipeline/amplification.rs`
- Modify: `src/scoring/profile.rs`
- Test: `tests/composition.rs` (extend)

- [ ] **Step 1: Write failing test for expanded pipeline accepting likes and replies**

Add to `tests/composition.rs`:

```rust
#[test]
fn amplification_event_types_include_like_and_reply() {
    // Verify the pipeline accepts "like" and "reply" event types
    use charcoal::db::models::AmplificationEvent;

    let like_event = AmplificationEvent {
        id: Some(1),
        event_type: "like".to_string(),
        amplifier_did: "did:plc:liker".to_string(),
        amplifier_handle: "liker.bsky.social".to_string(),
        original_post_uri: "at://did:plc:user/app.bsky.feed.post/abc".to_string(),
        amplifier_post_uri: None,
        amplifier_text: None,
        detected_at: Some("2026-03-19T12:00:00Z".to_string()),
        followers_fetched: Some(false),
        followers_scored: Some(false),
        original_post_text: Some("my post about fat liberation".to_string()),
        context_score: None,
    };
    assert_eq!(like_event.event_type, "like");
    assert!(like_event.amplifier_post_uri.is_none()); // likes don't have posts

    let reply_event = AmplificationEvent {
        id: Some(2),
        event_type: "reply".to_string(),
        amplifier_did: "did:plc:replier".to_string(),
        amplifier_handle: "replier.bsky.social".to_string(),
        original_post_uri: "at://did:plc:user/app.bsky.feed.post/abc".to_string(),
        amplifier_post_uri: Some("at://did:plc:replier/app.bsky.feed.post/def".to_string()),
        amplifier_text: Some("have you tried not being fat".to_string()),
        detected_at: Some("2026-03-19T12:00:00Z".to_string()),
        followers_fetched: Some(false),
        followers_scored: Some(false),
        original_post_text: Some("my post about fat liberation".to_string()),
        context_score: Some(0.82),
    };
    assert_eq!(reply_event.event_type, "reply");
    assert!(reply_event.amplifier_text.is_some());
    assert!(reply_event.context_score.is_some());
}
```

- [ ] **Step 2: Run test to verify it passes (data model already supports this)**

Run: `cargo test --test composition amplification_event_types -v`
Expected: PASS (the model structs already have the fields from Task 1).

- [ ] **Step 3: Integrate likes and replies into the scan pipeline**

Read `src/pipeline/amplification.rs` and `src/web/scan_job.rs`. The scan pipeline needs to:

1. After fetching Constellation backlinks for quotes/reposts, also call `find_likers()` for likes
2. Fetch reply threads for the protected user's recent posts
3. Filter replies for drive-bys using the follows set
4. Store all new event types in the DB
5. Score followers of ALL engagers (not just quoters)

This is primarily pipeline wiring — connecting the new detection functions from Tasks 7-8 into the existing scan flow. The specific integration points are:

- In `src/web/scan_job.rs` (or wherever the scan orchestration happens): after `find_amplification_events()`, also call `find_likers()` and `fetch_replies_to_post()` + `filter_drive_by_replies()`
- Pass all events (quotes + reposts + likes + replies) to the amplification pipeline
- Store `original_post_text` when recording events (fetch the protected user's post text via `fetch_post_text()`)

**This task is pipeline integration — no new pure-function tests needed.** The components are individually tested. End-to-end validation happens in staging.

- [ ] **Step 4: Run full test suite**

Run: `cargo test --features web`
Expected: All tests pass (no regressions).

- [ ] **Step 5: Commit**

```bash
git add src/pipeline/amplification.rs src/web/scan_job.rs
git commit -m 'feat: expand scan pipeline to detect likes and drive-by replies

Scan now fetches likers via Constellation and reply threads via
getPostThread. Drive-by replies filtered by follows set. All
engagement types stored with original_post_text. Followers of
all engager types scored.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

## Chunk 4: Web API & Frontend (Tasks 10-13)

### Task 10: Label API Endpoints

**Files:**
- Create: `src/web/handlers/labels.rs`
- Modify: `src/web/handlers/mod.rs`
- Modify: `src/web/handlers/accounts.rs`
- Test: `tests/unit_labels.rs` (extend with API tests)

- [ ] **Step 1: Write failing tests for label API**

Add to `tests/unit_labels.rs` (or create `tests/web_labels.rs` if gated on `--features web`):

```rust
// These tests require --features web
#[cfg(feature = "web")]
mod web_tests {
    use axum::http::StatusCode;
    // Use the existing test helper pattern from web_oauth.rs

    #[tokio::test]
    async fn label_account_returns_200() {
        // POST /api/accounts/{did}/label with body { "label": "high", "notes": "known troll" }
        // Should return 200 with the created label
        // Use build_test_app_with_db helper from test_helpers
        todo!("Implement using existing test app pattern")
    }

    #[tokio::test]
    async fn label_account_updates_existing() {
        // POST same endpoint twice with different label
        // Second call should update, not duplicate
        todo!("Implement using existing test app pattern")
    }

    #[tokio::test]
    async fn get_review_queue_returns_unlabeled() {
        // GET /api/review?limit=10
        // Should return unlabeled accounts sorted by threat_score desc
        todo!("Implement using existing test app pattern")
    }

    #[tokio::test]
    async fn get_accuracy_returns_metrics() {
        // GET /api/accuracy
        // Should return accuracy metrics when labels exist
        todo!("Implement using existing test app pattern")
    }

    #[tokio::test]
    async fn label_requires_auth() {
        // POST /api/accounts/{did}/label without auth cookie
        // Should return 401
        todo!("Implement using existing test app pattern")
    }
}
```

**Note:** The `todo!()` placeholders above are intentional for the plan — the implementer should follow the exact test patterns from `tests/web_oauth.rs` which uses `build_test_app_with_db()` to create a test Axum app with an in-memory SQLite database. The tests must be fully fleshed out before writing the handler code.

- [ ] **Step 2: Implement label handler**

Create `src/web/handlers/labels.rs`:

```rust
use axum::{extract::{Path, State}, http::StatusCode, Json};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct LabelRequest {
    pub label: String,
    pub notes: Option<String>,
}

#[derive(Serialize)]
pub struct LabelResponse {
    pub user_did: String,
    pub target_did: String,
    pub label: String,
    pub labeled_at: String,
    pub notes: Option<String>,
    pub predicted_tier: Option<String>,
}

pub async fn upsert_label(
    State(state): State<AppState>,
    user_did: String, // extracted from auth middleware
    Path(target_did): Path<String>,
    Json(body): Json<LabelRequest>,
) -> Result<Json<LabelResponse>, StatusCode> {
    // Validate label is one of: high, elevated, watch, safe
    // Call db.upsert_user_label()
    // Return the label with predicted_tier from account_scores
    todo!("Implement")
}

pub async fn get_review_queue(
    State(state): State<AppState>,
    user_did: String,
    query: axum::extract::Query<ReviewQuery>,
) -> Result<Json<Vec<ReviewItem>>, StatusCode> {
    // Call db.get_unlabeled_accounts()
    // Return accounts with their scores and any interaction pairs
    todo!("Implement")
}

pub async fn get_accuracy(
    State(state): State<AppState>,
    user_did: String,
) -> Result<Json<AccuracyResponse>, StatusCode> {
    // Call db.get_accuracy_metrics()
    todo!("Implement")
}
```

Register routes in `src/web/handlers/mod.rs`.

- [ ] **Step 3: Add label data to account detail endpoint**

Modify `src/web/handlers/accounts.rs` to include the user's label (if any) in the account detail response. Also include whether the label matches the predicted tier.

- [ ] **Step 4: Run tests**

Run: `cargo test --features web`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/web/handlers/labels.rs src/web/handlers/mod.rs src/web/handlers/accounts.rs tests/unit_labels.rs
git commit -m 'feat: add label API endpoints

POST /api/accounts/{did}/label — upsert user label
GET /api/review — unlabeled accounts sorted by threat_score
GET /api/accuracy — scoring accuracy metrics
Account detail now includes user label and match status.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

### Task 11: Label Buttons Svelte Component

**Files:**
- Create: `web/src/lib/components/LabelButtons.svelte`
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/lib/api.ts`

**Note:** Always invoke the svelte MCP skill when creating or editing `.svelte` files.

- [ ] **Step 1: Add TypeScript types**

Add to `web/src/lib/types.ts`:

```typescript
export interface UserLabel {
    user_did: string;
    target_did: string;
    label: 'high' | 'elevated' | 'watch' | 'safe';
    labeled_at: string;
    notes: string | null;
    predicted_tier: string | null;
}

export interface AccuracyMetrics {
    total_labeled: number;
    exact_matches: number;
    overscored: number;
    underscored: number;
    accuracy: number;
}

export interface ReviewItem {
    account: AccountScore;
    interaction_pairs: InteractionPair[];
}

export interface InteractionPair {
    original_text: string;
    response_text: string;
    context_score: number | null;
    event_type: string;
}
```

- [ ] **Step 2: Add API functions**

Add to `web/src/lib/api.ts`:

```typescript
export async function labelAccount(did: string, label: string, notes?: string): Promise<UserLabel> {
    const res = await fetch(`/api/accounts/${encodeURIComponent(did)}/label`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ label, notes }),
    });
    if (!res.ok) throw new Error(`Label failed: ${res.status}`);
    return res.json();
}

export async function getReviewQueue(limit = 20): Promise<ReviewItem[]> {
    const res = await fetch(`/api/review?limit=${limit}`);
    if (!res.ok) throw new Error(`Review queue failed: ${res.status}`);
    return res.json();
}

export async function getAccuracy(): Promise<AccuracyMetrics> {
    const res = await fetch('/api/accuracy');
    if (!res.ok) throw new Error(`Accuracy failed: ${res.status}`);
    return res.json();
}
```

- [ ] **Step 3: Create LabelButtons component**

Create `web/src/lib/components/LabelButtons.svelte` — a reusable component with 4 tier buttons. Props: `targetDid`, `currentLabel` (nullable), `predictedTier`. Emits on label change. Shows acknowledgment when label differs from prediction.

- [ ] **Step 4: Build SPA**

Run: `npm --prefix web run build`
Expected: Build succeeds.

- [ ] **Step 5: Commit**

```bash
git add web/src/lib/types.ts web/src/lib/api.ts web/src/lib/components/LabelButtons.svelte web/build/
git commit -m 'feat: add label buttons Svelte component and API client

LabelButtons component with 4-tier inline labeling. Shows
discrepancy acknowledgment when user label differs from
predicted tier. API client functions for label, review, accuracy.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

### Task 12: Inline Labels on Account Detail + Review Queue Page

**Files:**
- Modify: `web/src/routes/(protected)/accounts/[handle]/+page.svelte`
- Create: `web/src/routes/(protected)/review/+page.svelte`

- [ ] **Step 1: Add LabelButtons to account detail page**

Read the existing account detail page. Add the LabelButtons component below the account header, showing the current label (if any) and the predicted tier. When labeled, show the discrepancy acknowledgment.

- [ ] **Step 2: Create review queue page**

Create `web/src/routes/(protected)/review/+page.svelte` — loads unlabeled accounts from `/api/review`, displays them as cards with:
- Handle + predicted tier + score
- Top toxic posts
- Interaction pairs (if any)
- LabelButtons component
- After labeling, auto-loads next account
- Progress indicator

- [ ] **Step 3: Build SPA**

Run: `npm --prefix web run build`
Expected: Build succeeds.

- [ ] **Step 4: Run full Rust test suite**

Run: `cargo test --features web`
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add web/src/routes/ web/build/
git commit -m 'feat: add inline label buttons and triage review queue page

Account detail page shows label buttons with discrepancy
acknowledgment. New /review page for bulk labeling: cards
with scores, evidence, interaction pairs, and auto-advance.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

### Task 13: Accuracy Dashboard Panel

**Files:**
- Modify: `web/src/routes/(protected)/dashboard/+page.svelte`

- [ ] **Step 1: Add accuracy metrics to dashboard**

Read the existing dashboard page. Add a conditional panel that appears when 20+ labels exist, showing:
- Overall accuracy percentage
- Overscored / underscored counts
- Brief guidance ("You've corrected 8 accounts — 5 overscored, 3 underscored")

- [ ] **Step 2: Build SPA**

Run: `npm --prefix web run build`

- [ ] **Step 3: Run full test suite**

Run: `cargo test --features web`

- [ ] **Step 4: Commit**

```bash
git add web/src/routes/(protected)/dashboard/+page.svelte web/build/
git commit -m 'feat: add accuracy metrics panel to dashboard

Shows scoring accuracy when 20+ labels exist: overall match
rate, overscored/underscored counts. Helps users see where
the scoring system is failing.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

## Chunk 5: Postgres Migration & Staging Deploy (Tasks 14-16)

### Task 14: Postgres Migration

**Files:**
- Create: `migrations/postgres/0005_contextual_scoring.sql`
- Modify: `src/db/postgres.rs`

- [ ] **Step 1: Write Postgres migration SQL**

Create `migrations/postgres/0005_contextual_scoring.sql`:

```sql
-- Schema v5: Contextual scoring support

ALTER TABLE amplification_events ADD COLUMN original_post_text TEXT;
ALTER TABLE amplification_events ADD COLUMN context_score DOUBLE PRECISION;

ALTER TABLE account_scores ADD COLUMN context_score DOUBLE PRECISION;

CREATE TABLE IF NOT EXISTS user_labels (
    user_did TEXT NOT NULL,
    target_did TEXT NOT NULL,
    label TEXT NOT NULL,
    labeled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    notes TEXT,
    PRIMARY KEY (user_did, target_did)
);

CREATE TABLE IF NOT EXISTS inferred_pairs (
    id BIGSERIAL PRIMARY KEY,
    user_did TEXT NOT NULL,
    target_did TEXT NOT NULL,
    target_post_text TEXT NOT NULL,
    target_post_uri TEXT NOT NULL,
    user_post_text TEXT NOT NULL,
    user_post_uri TEXT NOT NULL,
    similarity DOUBLE PRECISION NOT NULL,
    context_score DOUBLE PRECISION,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_inferred_pairs_target
    ON inferred_pairs(user_did, target_did);
CREATE UNIQUE INDEX IF NOT EXISTS idx_inferred_pairs_dedup
    ON inferred_pairs(user_did, target_did, target_post_uri, user_post_uri);
```

- [ ] **Step 2: Implement new trait methods in PgDatabase**

Read `src/db/postgres.rs`. Add implementations for all 7 new/modified methods, matching the SQLite implementations but using sqlx query syntax and Postgres types (`DOUBLE PRECISION`, `TIMESTAMPTZ`).

- [ ] **Step 3: Run Postgres tests (if local Postgres available)**

Run: `DATABASE_URL=postgres://bryan.guffey@localhost/charcoal_test cargo test --all-targets --features postgres -v`
Expected: All Postgres tests pass.

- [ ] **Step 4: Commit**

```bash
git add migrations/postgres/0005_contextual_scoring.sql src/db/postgres.rs
git commit -m 'feat: add Postgres migration and trait implementations for v5

New tables: user_labels, inferred_pairs. New columns on
amplification_events and account_scores. All new Database
trait methods implemented for PgDatabase.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

### Task 15: Railway Staging Environment

**Files:** None (infrastructure setup via Railway CLI/MCP)

- [ ] **Step 1: Create staging environment on Railway**

Use Railway MCP tools to:
1. Create a new `staging` environment on the charcoal project
2. Provision a new Postgres addon for staging
3. Configure the staging environment to deploy from `feat/contextual-scoring` branch
4. Set up environment variables (copy from production, adjust as needed):
   - `CHARCOAL_SESSION_SECRET` (generate new one for staging)
   - `CHARCOAL_OAUTH_CLIENT_ID` (may need staging-specific)
   - `CHARCOAL_ALLOWED_DID` (empty for open access, or Bryan + testers)
   - `DATABASE_URL` (auto-set by Postgres addon)
   - `BLUESKY_HANDLE` (Bryan's handle)

- [ ] **Step 2: Generate domain for staging**

Use Railway MCP `generate-domain` or set up `staging.charcoal.watch` subdomain.

- [ ] **Step 3: Push branch and trigger deploy**

```bash
git push -u origin feat/contextual-scoring
```

- [ ] **Step 4: Verify staging deployment**

1. Check Railway logs for successful build
2. Verify model auto-download (all 3 models)
3. Verify schema migration v5 runs
4. Test OAuth login flow
5. Run a scan and verify new engagement types appear
6. Test label UI

- [ ] **Step 5: Document staging URL**

Add staging URL to the spec document and share with testers.

---

### Task 16: Integration Testing & Context Scoring Pipeline Wiring

**Files:**
- Modify: `src/scoring/profile.rs`
- Create: `src/scoring/context.rs`
- Test: `tests/unit_context.rs` (new)

This is the final wiring task — connecting the NLI scorer into the profile building pipeline.

- [ ] **Step 1: Write failing tests for context scoring orchestration**

Create `tests/unit_context.rs`:

```rust
//! Tests for contextual scoring orchestration.

#[test]
fn find_most_similar_posts_returns_top_n() {
    use charcoal::scoring::context::find_most_similar_posts;

    // Mock embeddings: user post embedding vs target post embeddings
    let user_embedding = vec![1.0, 0.0, 0.0]; // simplified 3-dim
    let target_posts = vec![
        ("post1".to_string(), vec![0.9, 0.1, 0.0]),  // high sim
        ("post2".to_string(), vec![0.0, 1.0, 0.0]),  // low sim
        ("post3".to_string(), vec![0.8, 0.2, 0.1]),  // medium sim
        ("post4".to_string(), vec![0.95, 0.05, 0.0]), // highest sim
    ];

    let top = find_most_similar_posts(&user_embedding, &target_posts, 2);
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].0, "post4"); // highest first
    assert_eq!(top[1].0, "post1"); // second highest
}

#[test]
fn find_most_similar_posts_returns_empty_for_no_posts() {
    use charcoal::scoring::context::find_most_similar_posts;
    let user_embedding = vec![1.0, 0.0, 0.0];
    let target_posts: Vec<(String, Vec<f64>)> = vec![];
    let top = find_most_similar_posts(&user_embedding, &target_posts, 5);
    assert!(top.is_empty());
}

#[test]
fn find_most_similar_posts_respects_limit() {
    use charcoal::scoring::context::find_most_similar_posts;
    let user_embedding = vec![1.0, 0.0, 0.0];
    let target_posts = vec![
        ("p1".to_string(), vec![0.9, 0.1, 0.0]),
        ("p2".to_string(), vec![0.8, 0.2, 0.0]),
        ("p3".to_string(), vec![0.7, 0.3, 0.0]),
    ];
    let top = find_most_similar_posts(&user_embedding, &target_posts, 1);
    assert_eq!(top.len(), 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test unit_context -v`
Expected: FAIL — module doesn't exist.

- [ ] **Step 3: Implement context scoring orchestration**

Create `src/scoring/context.rs`:

```rust
//! Contextual scoring orchestration.
//!
//! Finds the best text pairs for NLI scoring and computes
//! context_score for an account.

use crate::topics::embeddings::cosine_similarity_embeddings;

/// Find the N most similar posts from target to the user embedding.
/// Returns (post_text, similarity) sorted by similarity descending.
pub fn find_most_similar_posts(
    user_embedding: &[f64],
    target_posts: &[(String, Vec<f64>)],
    top_n: usize,
) -> Vec<(String, f64)> {
    let mut scored: Vec<(String, f64)> = target_posts.iter()
        .map(|(text, emb)| {
            let sim = cosine_similarity_embeddings(user_embedding, emb);
            (text.clone(), sim)
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_n);
    scored
}
```

Add `pub mod context;` to `src/scoring/mod.rs`.

- [ ] **Step 4: Wire NLI scoring into build_profile**

Read `src/scoring/profile.rs`. Modify `build_profile()` to:
1. Accept an optional `&NliScorer` parameter
2. If NLI scorer available AND interaction pairs exist for this account: score pairs, use `max_context_score`
3. If NLI scorer available AND no direct pairs: use `find_most_similar_posts` to find inferred pairs, score those
4. Pass `context_score` to `compute_threat_score_contextual` instead of `compute_threat_score`
5. Pass `context_score` to `apply_behavioral_modifier_contextual` instead of `apply_behavioral_modifier`

- [ ] **Step 5: Run full test suite**

Run: `cargo test --features web`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/scoring/context.rs src/scoring/mod.rs src/scoring/profile.rs tests/unit_context.rs
git commit -m 'feat: wire NLI contextual scoring into profile building pipeline

find_most_similar_posts matches target posts to user fingerprint.
build_profile now accepts optional NliScorer, computes context_score
from direct or inferred pairs, and uses blended formula + benign
gate bypass.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>'
```

---

## Post-Implementation Checklist

- [ ] Run full test suite: `cargo test --features web`
- [ ] Run clippy: `cargo clippy --features web -- -D warnings`
- [ ] Build SPA: `npm --prefix web run build`
- [ ] Push to `feat/contextual-scoring`
- [ ] Verify staging deployment succeeds
- [ ] Test OAuth login on staging
- [ ] Run a full scan on staging
- [ ] Label 5+ accounts via inline buttons
- [ ] Label 5+ accounts via review queue
- [ ] Verify accuracy metrics appear after 20+ labels
- [ ] Verify discrepancy acknowledgment shows when label differs from prediction
- [ ] Check Railway memory usage with 3 models loaded
- [ ] Update CLAUDE.md with new test count and feature description
