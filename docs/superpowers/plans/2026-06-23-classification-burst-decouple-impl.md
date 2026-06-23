# Classification Burst Decouple — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure the scan pipeline so all RunPod classifier calls happen in one contiguous burst (collect → burst → score), backed by a DB-staged work queue, making the `ScanCostMeter` (#206) honest by construction and the scan crash-/402-resumable.

**Architecture:** Split `build_profile` (the sole classifier chokepoint) along the classification seam into three phases. **Phase A (gather):** run the existing two-stage adaptive sampler; finalize the no-classification terminal outcomes (Insufficient Data, early-exit Low) directly; enqueue per-post rows + stash per-account inputs for Stage-2 survivors. **Phase B (burst):** one contiguous high-concurrency loop drains the queue — the only RunPod window. **Phase C (finalize):** per-account scoring from stored verdicts + stash. State lives in two new tables + a `scan_state.phase` column (schema v9). All behavior is guarded by a golden test written *first*.

**Tech Stack:** Rust, tokio, `async_trait`, rusqlite (SQLite) + sqlx (Postgres), serde_json, futures `buffer_unordered`. Suite: `cargo test --features web`; clippy across `--features web` / default / `--features postgres`.

**Spec:** `docs/superpowers/specs/2026-06-23-classification-burst-decouple-design.md`

**Branch:** `feat/cope-b-cost-guard` (extends PR #58; #208 builds on #206's `ScanCostMeter`).

**Project rules (apply to every commit):** stage files explicitly by name (never `git add -A`/`-u`/`.`); no heredocs in commit messages (single-quoted multi-line strings); `cargo fmt` before each commit (pre-commit hook blocks on fmt/clippy/tests); conventional commits ending in `(#208)`; never `git merge`/`rebase`/`reset` (hook-blocked).

---

## File Structure

**New files:**
- `src/pipeline/scan_phases/mod.rs` — phase orchestration module (`run_phased_scan`), the resume state machine over `scan_state.phase`.
- `src/pipeline/scan_phases/gather.rs` — Phase A: `gather_account` (two-stage sampler → enqueue/finalize-terminal + stash).
- `src/pipeline/scan_phases/burst.rs` — Phase B: `run_burst` (the contiguous classify loop).
- `src/pipeline/scan_phases/finalize.rs` — Phase C: `finalize_account` (score from stash + verdicts).
- `src/pipeline/scan_phases/staging.rs` — the staging value types: `QueueRow`, `VerdictRow`, `AccountInput` (serde, `schema_version`), `ScanPhase` enum.
- `migrations/postgres/0009_classification_staging.sql` — Postgres v9 objects.
- `tests/unit_scan_phases.rs` — unit tests for gather/burst/finalize against a fake `Database` + `StubScorer`.
- `tests/golden_build_profile.rs` — the behavior-preserving golden test (written FIRST, Chunk 2).

**Modified files:**
- `src/db/schema.rs` — add `run_migration(conn, 9, …)` creating the two tables + `scan_state.phase` column + indexes.
- `src/db/traits.rs` — ~10 new `Database` methods.
- `src/db/sqlite.rs` — SQLite impls.
- `src/db/postgres.rs` — Postgres impls.
- `src/db/models.rs` — re-export / house shared row structs if the trait needs them (else they live in `staging.rs`).
- `src/scoring/profile.rs` — extract the two-stage sampler boundary into reusable pieces `build_profile` and the phases both call (no behavior change in this file's own path).
- `src/pipeline/sweep.rs` — `run` + `run_topic_first` call the phased orchestrator instead of streaming `build_profile`.
- `src/pipeline/amplification.rs` — amplifier + follower paths feed the phased orchestrator; two-pass NLI gate moves to finalize.
- `src/pipeline/mod.rs` — `pub mod scan_phases;`.
- `src/observability/` — phase banners + burst progress (reuse existing `scan_cost_*` metrics module).

---

## Chunk 1: Schema v9 + `Database` trait surface

Foundation. No pipeline behavior changes yet — just the durable store and its interface, fully tested on both backends.

### Task 1.1: Staging value types

**Files:**
- Create: `src/pipeline/scan_phases/staging.rs`
- Modify: `src/pipeline/scan_phases/mod.rs` (new — declare submodules), `src/pipeline/mod.rs`
- Test: `tests/unit_scan_phases.rs` (new)

- [ ] **Step 1: Declare the module tree.** In `src/pipeline/mod.rs` add `pub mod scan_phases;`. Create `src/pipeline/scan_phases/mod.rs` with `pub mod staging;` (other submodules added in later chunks).

- [ ] **Step 2: Write the failing test** in `tests/unit_scan_phases.rs`:
```rust
use charcoal::pipeline::scan_phases::staging::{AccountInput, ScanPhase, ACCOUNT_INPUT_SCHEMA_VERSION};

#[test]
fn scan_phase_roundtrips_through_str() {
    for p in [ScanPhase::Gather, ScanPhase::Burst, ScanPhase::Finalize, ScanPhase::Done] {
        assert_eq!(ScanPhase::from_str_lenient(p.as_str()), Some(p));
    }
    assert_eq!(ScanPhase::from_str_lenient("nonsense"), None);
}

#[test]
fn account_input_is_versioned_and_roundtrips() {
    let blob = AccountInput::new_for_test(); // helper returns a minimal populated value
    let json = serde_json::to_string(&blob).unwrap();
    let back: AccountInput = serde_json::from_str(&json).unwrap();
    assert_eq!(back.schema_version, ACCOUNT_INPUT_SCHEMA_VERSION);
    assert_eq!(back, blob);
}
```

- [ ] **Step 3: Run, verify fail.** `cargo test --test unit_scan_phases` → FAIL (unresolved `staging`).

- [ ] **Step 4: Implement `staging.rs`.** Define:
  - `pub const ACCOUNT_INPUT_SCHEMA_VERSION: u32 = 1;`
  - `pub enum ScanPhase { Gather, Burst, Finalize, Done }` with `as_str(&self) -> &'static str` and `from_str_lenient(&str) -> Option<Self>` (used by the `scan_state.phase` column).
  - `pub struct QueueRow { pub account_did: String, pub post_uri: String, pub text: String, pub context_text: Option<String>, pub post_kind: String, pub onnx_score: f64, pub status: String, pub toxic_token: Option<bool>, pub confidence: Option<f32>, pub model_id: Option<String>, pub policy_version: Option<String> }` (derive `Debug, Clone, PartialEq`).
  - `pub struct VerdictRow { pub account_did: String, pub post_uri: String, pub toxic_token: bool, pub confidence: f32, pub model_id: String, pub policy_version: String }`.
  - `pub struct AccountInput { pub schema_version: u32, … }` — `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`. Fields cover every Stage-2 `build_profile` input that isn't a verdict: the 50-post sample (serialize the existing post-sample types — reuse `posts::PostSample`/reply/quote structs; if they aren't `Serialize`, add `#[derive(Serialize, Deserialize)]` to them in `src/bluesky/posts.rs` as part of this task), `parent_texts: std::collections::HashMap<String,String>`, `embeddings`/`protected_embedding` as needed, `median_engagement: f64`, `is_pile_on: bool` (precomputed `pile_on_dids.contains(account_did)` — store the boolean, not the whole set), `direct_pairs: Option<Vec<(String,String)>>`, `graph_distance: Option<String>`, `fingerprint_quality: String`. Provide `#[cfg(test)] pub fn new_for_test() -> Self`.

  Document WHY `is_pile_on` is stored as a precomputed bool: the pile-on set is scan-global and large; Phase C only needs this account's membership (spec §Data model).

- [ ] **Step 5: Run, verify pass.** `cargo test --test unit_scan_phases`.

- [ ] **Step 6: Commit.** `git add src/pipeline/mod.rs src/pipeline/scan_phases/mod.rs src/pipeline/scan_phases/staging.rs tests/unit_scan_phases.rs` (+ `src/bluesky/posts.rs` if serde derives added) → `git commit -m 'feat(decouple): staging value types — QueueRow/VerdictRow/AccountInput(versioned)/ScanPhase (#208)'`.

### Task 1.2: SQLite schema v9 migration

**Files:** Modify `src/db/schema.rs`. Test: `tests/unit_scan_phases.rs` (migration smoke via an in-memory SqliteDatabase).

- [ ] **Step 1: Write the failing test** — open an in-memory SqliteDatabase, assert the new tables/column exist (e.g. a `table_count()` bump, or a targeted `PRAGMA table_info(scan_state)` containing `phase`). Model the test after existing schema tests in `tests/` (locate with `grep -rn "schema\|migration\|PRAGMA" tests/`).

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Add the migration** after the v8 block (`src/db/schema.rs:280`), using the existing `run_migration(conn, 9, |c| { … })` helper:
```sql
CREATE TABLE IF NOT EXISTS classification_queue (
    user_did     TEXT NOT NULL,
    account_did  TEXT NOT NULL,
    post_uri     TEXT NOT NULL,
    text         TEXT NOT NULL,
    context_text TEXT,
    post_kind    TEXT NOT NULL,
    onnx_score   REAL NOT NULL,
    status       TEXT NOT NULL,           -- 'pending' | 'done'
    toxic_token  INTEGER,
    confidence   REAL,
    model_id     TEXT,
    policy_version TEXT,
    PRIMARY KEY (user_did, account_did, post_uri)
);
CREATE INDEX IF NOT EXISTS idx_clsq_pending ON classification_queue(user_did, status);

CREATE TABLE IF NOT EXISTS scan_account_input (
    user_did     TEXT NOT NULL,
    account_did  TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    PRIMARY KEY (user_did, account_did)
);

ALTER TABLE scan_state ADD COLUMN phase TEXT;  -- NULL = no phased scan in progress
```
  (Match the migration closure's existing error handling; `ALTER TABLE … ADD COLUMN` is idempotent-safe under the versioned `run_migration` guard, which only runs once per version.)

- [ ] **Step 4: Run, verify pass.**

- [ ] **Step 5: Commit.** `git add src/db/schema.rs tests/unit_scan_phases.rs` → `git commit -m 'feat(decouple): SQLite schema v9 — classification_queue, scan_account_input, scan_state.phase (#208)'`.

### Task 1.3: Postgres schema v9 migration

**Files:** Create `migrations/postgres/0009_classification_staging.sql`; modify `src/db/postgres.rs` if migrations are listed in an `include_str!` array (locate with `grep -rn "include_str!\|0008" src/db/postgres.rs`).

- [ ] **Step 1:** Create the SQL file mirroring Task 1.2 with Postgres types: `TEXT`, `DOUBLE PRECISION` for reals, `BOOLEAN` for `toxic_token`, `REAL` for confidence, `JSONB` for `payload_json`, and the same PKs + index. Add `ALTER TABLE scan_state ADD COLUMN IF NOT EXISTS phase TEXT;`.

- [ ] **Step 2:** Register the file in the Postgres migration list next to `0008_*`.

- [ ] **Step 3: Verify it compiles** (`cargo build --features postgres`). Live Postgres integration is exercised in Chunk 7's `--features postgres` run (requires `DATABASE_URL`).

- [ ] **Step 4: Commit.** `git add migrations/postgres/0009_classification_staging.sql src/db/postgres.rs` → `git commit -m 'feat(decouple): Postgres schema v9 migration (#208)'`.

### Task 1.4: `Database` trait methods + SQLite impl

**Files:** Modify `src/db/traits.rs`, `src/db/sqlite.rs`. Test: `tests/unit_scan_phases.rs`.

- [ ] **Step 1: Add the trait methods** to `src/db/traits.rs` (all `async`, `user_did: &str` first), grouped with a `// --- Classification staging (#208) ---` comment:
```rust
async fn enqueue_classifications(&self, user_did: &str, rows: &[QueueRow]) -> Result<()>;
async fn stash_account_input(&self, user_did: &str, account_did: &str, payload_json: &str) -> Result<()>;
async fn set_scan_phase(&self, user_did: &str, phase: &str) -> Result<()>;
async fn get_scan_phase(&self, user_did: &str) -> Result<Option<String>>;
async fn fetch_pending_classifications(&self, user_did: &str, limit: i64) -> Result<Vec<QueueRow>>;
async fn record_classification_verdicts(&self, user_did: &str, verdicts: &[VerdictRow]) -> Result<()>;
async fn list_scan_accounts(&self, user_did: &str) -> Result<Vec<String>>;
async fn fetch_account_verdicts(&self, user_did: &str, account_did: &str) -> Result<Vec<QueueRow>>;
async fn fetch_account_input(&self, user_did: &str, account_did: &str) -> Result<Option<String>>;
async fn count_pending_classifications(&self, user_did: &str) -> Result<i64>;
async fn clear_scan_staging(&self, user_did: &str) -> Result<()>;
```
  Import `QueueRow`/`VerdictRow` at the top of `traits.rs` (`use crate::pipeline::scan_phases::staging::{QueueRow, VerdictRow};`).

- [ ] **Step 2: Write failing tests** (in `tests/unit_scan_phases.rs`, async via `#[tokio::test]`, against an in-memory `SqliteDatabase`): round-trip enqueue→fetch_pending (statuses respected), `record_classification_verdicts` flips rows to `done` + stores verdict, `enqueue_classifications` is an UPSERT (enqueue same PK twice → one row, no dup), `stash`/`fetch_account_input` round-trip, `set`/`get_scan_phase`, `count_pending_classifications`, `clear_scan_staging` empties both tables, `list_scan_accounts` returns distinct dids. Use `upsert_user` first to satisfy any FK/user expectations.

- [ ] **Step 3: Run, verify fail.**

- [ ] **Step 4: Implement in `src/db/sqlite.rs`** (follow the existing `Mutex<Connection>` pattern; batch writes inside one `conn` lock; use `INSERT … ON CONFLICT(user_did,account_did,post_uri) DO UPDATE SET …` for `enqueue`). `fetch_pending_classifications` → `WHERE user_did=?1 AND status='pending' LIMIT ?2`. `record_classification_verdicts` → batch `UPDATE … SET status='done', toxic_token=?, confidence=?, model_id=?, policy_version=? WHERE user_did=? AND account_did=? AND post_uri=?`.

- [ ] **Step 5: Run, verify pass.** `cargo test --features web --test unit_scan_phases`.

- [ ] **Step 6: Commit.** `git add src/db/traits.rs src/db/sqlite.rs tests/unit_scan_phases.rs` → `git commit -m 'feat(decouple): Database trait staging methods + SQLite impl (#208)'`.

### Task 1.5: Postgres impl of the new methods

**Files:** Modify `src/db/postgres.rs`. Test: covered by `tests/db_postgres.rs` under `--features postgres`.

- [ ] **Step 1:** Implement all 11 methods in `PgDatabase` via sqlx, mirroring the SQLite semantics (JSONB for `payload_json`, `ON CONFLICT … DO UPDATE`, `BOOLEAN` binding for `toxic_token`).
- [ ] **Step 2:** Add a Postgres integration test to `tests/db_postgres.rs` mirroring Task 1.4's round-trips (gated on `--features postgres` + `DATABASE_URL`).
- [ ] **Step 3: Verify** `cargo build --features postgres` compiles and clippy is clean. (Live run in Chunk 7.)
- [ ] **Step 4: Commit.** `git add src/db/postgres.rs tests/db_postgres.rs` → `git commit -m 'feat(decouple): Postgres impl of staging methods (#208)'`.

### Chunk 1 review gate
Dispatch the plan/code review per the review loop before Chunk 2. Run `cargo test --features web` + clippy matrix; both green.

---

## Chunk 2: Golden test (TDD baseline — write BEFORE any restructure)

This locks `build_profile`'s observable behavior so the Phase A/B/C split is provably behavior-preserving. Per the spec, this is the first behavioral chunk and must exist before Chunk 3 touches the pipeline.

### Task 2.1: Capture build_profile golden outputs

**Files:** Create `tests/golden_build_profile.rs`. May add a small fixtures dir `tests/fixtures/golden/` with canned post samples.

- [ ] **Step 1: Inventory the behaviors to pin.** Read `src/scoring/profile.rs:38–187` and the amplification two-pass (`src/pipeline/amplification.rs:355–425`). The golden cases MUST include: (a) `<5 posts` → tier "Insufficient Data"; (b) `should_early_exit_stage1` true → tier "Low", `toxicity_score=Some(0.0)`, `scoring_confidence="low"`; (c) a Stage-2 survivor scored full (toxic + clean mix); (d) an amplifier follower whose pass-1 `raw_score >= 8.0` triggers the NLI pass.

- [ ] **Step 2: Build deterministic harness.** Use a `StubScorer`/`StubClassifier` (already exists in tests) + canned `PostSample`s so outputs are deterministic (no network, no model). For each case, call today's `build_profile` and snapshot the resulting `AccountScore` (tier, scores, confidence, posts_analyzed, behavioral flags). Assert exact values.

- [ ] **Step 3: Run, verify the golden tests PASS against current code** (`cargo test --features web --test golden_build_profile`). They describe today's behavior — green now, and must stay green after the restructure.

- [ ] **Step 4: Commit.** `git add tests/golden_build_profile.rs tests/fixtures/golden/` → `git commit -m 'test(decouple): golden behavior baseline for build_profile + amplification two-pass (#208)'`.

### Chunk 2 review gate
Reviewer confirms the golden cases cover all four terminal/branch behaviors. These tests are the contract for Chunks 3–5.

---

## Chunk 3: Phase A — gather

Extract a reusable `gather_account` that runs the two-stage sampler, finalizing terminal outcomes and enqueuing survivors. `build_profile` stays intact (the golden test still drives it); we add the gather path alongside, then later route the pipeline through it.

### Task 3.1: Factor the sampler so gather and `build_profile` share it

**Files:** Modify `src/scoring/profile.rs`. Test: `tests/golden_build_profile.rs` (must stay green).

- [ ] **Step 1:** Extract Stage-1 logic (fetch 25, ONNX early-exit decision, `<5` and early-exit `AccountScore` construction) into a pure-ish helper `stage1_outcome(...) -> Stage1Outcome` where `enum Stage1Outcome { Terminal(AccountScore), Proceed { stage1_overlap: Option<f64>, … } }`. `build_profile` calls it and returns early on `Terminal`. Run the golden test — still green (pure refactor).
- [ ] **Step 2: Commit.** `git add src/scoring/profile.rs` → `git commit -m 'refactor(decouple): extract stage1_outcome from build_profile (no behavior change) (#208)'`.

### Task 3.2: `gather_account`

**Files:** Create `src/pipeline/scan_phases/gather.rs`; declare in `scan_phases/mod.rs`. Test: `tests/unit_scan_phases.rs`.

- [ ] **Step 1: Write failing tests** for `gather_account` against a fake `Database` + `StubScorer` + canned samples:
  - `<5 posts` → calls `upsert_account_score` with tier "Insufficient Data"; enqueues NOTHING; stashes NOTHING.
  - early-exit → `upsert_account_score` tier "Low"/`conf=low`; enqueues nothing.
  - survivor → enqueues per-post rows (clean→`done` with verdict, survivor→`pending`); `stash_account_input` called once with a versioned blob; NO `upsert_account_score` (deferred to Phase C).
  - idempotency: calling `gather_account` twice for the same survivor → queue still has one row per post (UPSERT).

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement `gather_account`.** Signature mirrors `build_profile`'s inputs but takes `db: &Arc<dyn Database>` and `user_did`, and returns `Result<()>`. Logic: run `stage1_outcome`; on `Terminal(score)` → `db.upsert_account_score(user_did, &score)`; on `Proceed` → fetch Stage-2 50-post sample, run the per-post ONNX clean-pass to classify each post into clean(`done`)/survivor(`pending`) `QueueRow`s (reuse the ensemble's clean-pass threshold logic — extract a helper if needed so gather and `build_profile` agree), build the `AccountInput` blob (incl. `is_pile_on`, `median_engagement`, `direct_pairs`, graph_distance, fingerprint_quality, parent_texts), then `enqueue_classifications` + `stash_account_input`.

- [ ] **Step 4: Run, verify pass.**

- [ ] **Step 5: Commit.** `git add src/pipeline/scan_phases/gather.rs src/pipeline/scan_phases/mod.rs tests/unit_scan_phases.rs` → `git commit -m 'feat(decouple): Phase A gather_account — sampler→enqueue/finalize-terminal+stash (#208)'`.

### Chunk 3 review gate.

---

## Chunk 4: Phase B — burst

### Task 4.1: `run_burst`

**Files:** Create `src/pipeline/scan_phases/burst.rs`. Test: `tests/unit_scan_phases.rs`.

- [ ] **Step 1: Write failing tests** against a fake `Database` + `StubClassifier`:
  - drains all `pending` rows → all become `done` with verdicts; `count_pending_classifications` → 0.
  - batches: with `limit` smaller than total pending, the loop iterates until empty.
  - ceiling/402: a classifier that returns `CostCeilingExceeded` mid-batch → loop stops, un-recorded rows stay `pending`, function returns a "degraded" signal (e.g. `Ok(BurstOutcome::CostCapped)` vs `Ok(BurstOutcome::Complete)`); verdicts already recorded persist.

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement `run_burst(db, user_did, classifier, burst_concurrency, burst_batch) -> Result<BurstOutcome>`:** loop { `fetch_pending_classifications(limit=burst_batch)`; break if empty; classify via `futures::stream::iter(...).buffer_unordered(burst_concurrency)`; collect successful `VerdictRow`s, `record_classification_verdicts` (batch); if a `CostCeilingExceeded` surfaces, stop and return `CostCapped` (verdicts gathered so far are still written) }. Add `BURST_CONCURRENCY` (env `CHARCOAL_BURST_CONCURRENCY`, default ~16, clamp) and `BURST_BATCH` (default 500) reading helpers. The classifier is the existing `Arc<dyn ToxicityClassifier>` from `build_from_env()` — so the #206 `ScanCostMeter` rides along unchanged.

- [ ] **Step 4: Run, verify pass.**

- [ ] **Step 5: Commit.** `git add src/pipeline/scan_phases/burst.rs src/pipeline/scan_phases/mod.rs tests/unit_scan_phases.rs` → `git commit -m 'feat(decouple): Phase B run_burst — contiguous classify loop, cost-capped resume (#208)'`.

### Chunk 4 review gate.

---

## Chunk 5: Phase C — finalize

### Task 5.1: `finalize_account`

**Files:** Create `src/pipeline/scan_phases/finalize.rs`. Modify `src/scoring/profile.rs` to expose the post-classification scoring as a callable that takes verdicts + inputs (factor Stage-2's "after classify" tail out of `build_profile`). Test: `tests/unit_scan_phases.rs` + `tests/golden_build_profile.rs`.

- [ ] **Step 1: Factor the Stage-2 tail.** Extract everything in `build_profile` *after* `classify_batch_with_contexts` (reply-weighted toxicity rate → behavioral → context/NLI → graph distance → tier) into `score_from_verdicts(inputs, verdicts, …) -> AccountScore`. `build_profile` calls it with freshly-computed verdicts. Golden test stays green (pure refactor). Commit.

- [ ] **Step 2: Write failing tests** for `finalize_account`: reads `fetch_account_verdicts` + `fetch_account_input`, deserializes the blob, calls `score_from_verdicts`, `upsert_account_score`. Cases: a survivor with mixed verdicts → matching `AccountScore`; **two-pass NLI gate** — when `raw_score >= 8.0`, the NLI pass runs (pass `nli_scorer`, `ppwe`, AND `data_dir` for audit logging — spec §amplification); blob deserialize failure / `schema_version` mismatch → discard this account's staging rows and signal re-gather (e.g. `FinalizeOutcome::NeedsRegather`).

- [ ] **Step 3: Run, verify fail.**

- [ ] **Step 4: Implement `finalize_account(db, user_did, account_did, nli_scorer, data_dir, …) -> Result<FinalizeOutcome>`.** On deserialize/version mismatch: `clear` this account's `classification_queue` + `scan_account_input` rows and return `NeedsRegather` (the orchestrator re-runs gather for it). On success: `score_from_verdicts` (run the NLI two-pass gate here using the stashed `direct_pairs`/pairs + `data_dir`), `upsert_account_score`.

- [ ] **Step 5: Run, verify pass.** Both `unit_scan_phases` and `golden_build_profile` green.

- [ ] **Step 6: Commit.** `git add src/scoring/profile.rs src/pipeline/scan_phases/finalize.rs tests/unit_scan_phases.rs` → `git commit -m 'feat(decouple): Phase C finalize_account — score_from_verdicts + two-pass NLI + re-gather-on-version-mismatch (#208)'`.

### Chunk 5 review gate.

---

## Chunk 6: Orchestration + call-site rewiring + resume

### Task 6.1: `run_phased_scan` state machine

**Files:** `src/pipeline/scan_phases/mod.rs`. Test: `tests/unit_scan_phases.rs`.

- [ ] **Step 1: Write failing tests** (fake DB + stubs): a fresh scan walks Gather→Burst→Finalize→Done, advancing `scan_state.phase` at each boundary; a resume that starts with `phase='burst'` skips gather and re-selects `pending`; a resume with leftover `pending` after a `CostCapped` burst finalizes only all-`done` accounts and leaves the rest; `NeedsRegather` from finalize re-runs gather for that account then re-bursts. On clean Done → `clear_scan_staging`.

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement `run_phased_scan(db, user_did, candidates, deps…) -> Result<ScanSummary>`:** read `get_scan_phase`; dispatch to the right phase; Phase A iterates candidates through `gather_account` (respecting the staleness entry gate — see Task 6.2) with the existing `buffer_unordered(concurrency)`; set phase `burst`; `run_burst`; if `CostCapped`, mark degraded + stop (resumable); set phase `finalize`; iterate `list_scan_accounts` → `finalize_account` (skip accounts with leftover `pending`; handle `NeedsRegather`); set phase `done`; `clear_scan_staging`.

- [ ] **Step 4: Run, verify pass. Step 5: Commit.**

### Task 6.2: Rewire `sweep::run` and `sweep::run_topic_first`

**Files:** `src/pipeline/sweep.rs`. Test: existing sweep tests + `golden_build_profile`.

- [ ] **Step 1:** Replace the `buffer_unordered(score_account-stream)` (sweep.rs ~140–168 and the topic-first variant ~278+) with collection of candidate accounts → `run_phased_scan`. Preserve the **staleness entry gate**: `run` keeps `is_score_stale(...,7)` (sweep.rs:110); `run_topic_first` keeps `get_all_scored_dids` dedup (sweep.rs:204) — apply the gate *before* gather enqueues, in Phase A's candidate filter. Keep incremental persistence semantics (Phase A writes terminal accounts immediately; Phase C writes survivors as finalized).

- [ ] **Step 2:** Run the full suite; golden + sweep tests green. **Commit.**

### Task 6.3: Rewire `amplification.rs`

**Files:** `src/pipeline/amplification.rs`. Test: existing amplification tests + golden.

- [ ] **Step 1:** Keep the event-recording loop (amplification.rs ~77–194: `score_with_context` ONNX-only + `nli.score_pair` + `insert_amplification_event`) in place — it's Phase-A-time work, no RunPod. Replace the per-account `build_profile` calls (amplifier path + the follower two-pass at ~355–425) with `run_phased_scan`, passing the amplifiers' `direct_pairs` into the stashed `AccountInput`. The two-pass NLI gate now lives in `finalize_account` (Chunk 5), so the explicit pass-1/pass-2 re-score loop here is removed. **Drop the stale `#[allow(dead_code)]` on `is_scan_running_for` if touched** (spec advisory).

- [ ] **Step 2:** Full suite + golden green. **Commit.**

### Task 6.4: Observability

**Files:** `src/pipeline/scan_phases/mod.rs`, reuse `src/observability/`.

- [ ] **Step 1:** Emit phase-transition `tracing` banners (gather/burst/finalize/done counts) and burst progress via `count_pending_classifications`. No new metrics module — reuse `scan_cost_*`/classifier metrics. **Commit.**

### Chunk 6 review gate.

---

## Chunk 7: Full verification

- [ ] **Step 1:** `cargo test --features web` — all green (unit + golden + existing).
- [ ] **Step 2:** Clippy matrix: `cargo clippy --features web --all-targets -- -D warnings`; `cargo clippy --all-targets -- -D warnings`; `cargo clippy --features postgres --all-targets -- -D warnings`.
- [ ] **Step 3:** Postgres integration: `DATABASE_URL=… cargo test --all-targets --features postgres` (staging methods round-trip + migration applies).
- [ ] **Step 4:** `cargo fmt --all`; commit any formatting (explicit file names).
- [ ] **Step 5:** Manual smoke: run a small local scan (`cargo run -- scan …` against a low-volume handle) and confirm logs show the three phases in order, the burst as one contiguous window, and re-running skips fresh accounts. Confirm the meter estimate now tracks the burst window (not the whole scan).
- [ ] **Step 6:** Update `CHANGELOG.md` (Unreleased → Added) with the decouple entry referencing (#208). Commit.

---

## Out of scope (do NOT implement here)

- Onboarding wall-clock reduction / Phase A I/O parallelization / progressive results → #207.
- Any change to the meter's accounting model, scoring formulas, thresholds, the two-stage ONNX filter semantics, or UI.
- A `scan_id`/run-identifier — the single-in-flight-scan-per-user invariant (enforced globally by `ScanJobManager::try_start_scan`) makes it unnecessary; `clear_scan_staging` on fresh start is the backstop.

## Notes for the executor

- The golden test (Chunk 2) is the contract: if it goes red during Chunks 3–6, the restructure changed behavior — stop and reconcile, don't edit the golden values.
- `build_profile` is kept working throughout (it shares `stage1_outcome` + `score_from_verdicts` with the phases) until the call sites are rewired in Chunk 6; only then does the streaming path go away.
- Batch every staging write (`enqueue_classifications`, `record_classification_verdicts`) — per-row awaits serialize hard on SQLite's `Mutex<Connection>`.
- The classifier in Phase B is `build_from_env()` unchanged, so the #206 cost backstop and its `ScanCostMeter` are inherited with zero new wiring.
