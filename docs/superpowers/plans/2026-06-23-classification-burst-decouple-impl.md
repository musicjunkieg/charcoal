# Classification Burst Decouple — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure the scan pipeline so all RunPod classifier calls happen in one contiguous burst (collect → burst → score), backed by a DB-staged work queue, making the `ScanCostMeter` (#206) honest by construction and the scan crash-/402-resumable.

**Architecture:** Split `build_profile` (the sole classifier chokepoint) along the classification seam into three phases. **Phase A (gather):** run the existing two-stage adaptive sampler; finalize the no-classification terminal outcomes (Insufficient Data, early-exit Low) directly; enqueue per-post rows + stash per-account inputs for Stage-2 survivors. **Phase B (burst):** one contiguous high-concurrency loop drains the queue — the only RunPod window. **Phase C (finalize):** per-account scoring from stored verdicts + stash. State lives in two new tables (schema v9) + a phase marker stored in the existing key/value `scan_state`. All behavior is guarded by a golden test written against a freshly-extracted, deterministic scoring core.

**Tech Stack:** Rust, tokio, `async_trait`, rusqlite (SQLite) + sqlx (Postgres), serde_json, futures `buffer_unordered`. Suite: `cargo test --features web`; clippy across `--features web` / default / `--features postgres`.

**Spec:** `docs/superpowers/specs/2026-06-23-classification-burst-decouple-design.md`

**Branch:** `feat/cope-b-cost-guard` (extends PR #58; #208 builds on #206's `ScanCostMeter`).

**Project rules (apply to every commit):** stage files explicitly by name (never `git add -A`/`-u`/`.`); no heredocs in commit messages (single-quoted multi-line strings); `cargo fmt` before each commit (pre-commit hook blocks on fmt/clippy/tests); conventional commits ending in `(#208)`; never `git merge`/`rebase`/`reset` (hook-blocked).

**Test conventions (verified against the codebase — do NOT invent doubles):**
- DB tests use an **in-memory `SqliteDatabase`** (`Connection::open_in_memory()` pattern, see `src/db/sqlite.rs:300`, `tests/unit_admin.rs:88`, `tests/unit_labels.rs:310`). There is **no `impl Database` fake** — do not build one.
- The `ToxicityScorer` test double is **`FixedScorer`** (`tests/unit_ensemble.rs:17`); `NoopScorer` also exists (`src/db`... actually `traits.rs:48` region). The `ToxicityClassifier` double is **`StubClassifier`** (`src/toxicity/classifier.rs:56`). There is **no `StubScorer`** — if a richer scorer double is needed, author one modeled on `FixedScorer` as an explicit step.

---

## File Structure

**New files:**
- `src/pipeline/scan_phases/mod.rs` — phase orchestration (`run_phased_scan`), the resume state machine over the `scan_phase` key.
- `src/pipeline/scan_phases/gather.rs` — Phase A: `gather_account`.
- `src/pipeline/scan_phases/burst.rs` — Phase B: `run_burst`.
- `src/pipeline/scan_phases/finalize.rs` — Phase C: `finalize_account`.
- `src/pipeline/scan_phases/staging.rs` — staging value types: `QueueRow`, `VerdictRow`, `AccountInput` (serde, `schema_version`), `ScanPhase` enum (a typed wrapper over the `scan_phase` string value).
- `migrations/postgres/0009_classification_staging.sql` — Postgres v9 tables.
- `tests/unit_scan_phases.rs` — unit tests for gather/burst/finalize/orchestration (in-memory `SqliteDatabase`).
- `tests/golden_build_profile.rs` — behavior-preserving golden test against the extracted scoring core.

**Modified files:**
- `src/db/schema.rs` — `run_migration(conn, 9, …)` creating the two tables + indexes (NO `scan_state` column).
- `src/db/traits.rs` — 9 new `Database` methods (NOT a phase getter/setter — phase reuses `set_scan_state`/`get_scan_state`).
- `src/db/sqlite.rs`, `src/db/postgres.rs` — impls.
- `src/toxicity/ensemble.rs` — expose an **ONNX-clean-pass seam** (primary-only pass, independent of the classifier).
- `src/scoring/profile.rs` — extract `stage1_outcome` (sample-in) and `score_from_sample` (sample+verdicts-in) so `build_profile` and the phases share deterministic, fetch-free cores.
- `src/bluesky/posts.rs` — add `Serialize`/`Deserialize` to the post-sample structs if absent (needed to stash the sample).
- `src/pipeline/sweep.rs`, `src/pipeline/amplification.rs` — route through `run_phased_scan`.
- `src/pipeline/mod.rs` — `pub mod scan_phases;`.

---

## Chunk 1: Schema v9 + `Database` staging methods

Foundation: durable store + interface, tested on both backends. No pipeline behavior change.

### Task 1.1: Staging value types

**Files:** Create `src/pipeline/scan_phases/staging.rs` + `src/pipeline/scan_phases/mod.rs`; modify `src/pipeline/mod.rs`. Test: `tests/unit_scan_phases.rs`.

- [ ] **Step 1:** `src/pipeline/mod.rs` += `pub mod scan_phases;`. Create `scan_phases/mod.rs` with `pub mod staging;`.
- [ ] **Step 2: Failing test** in `tests/unit_scan_phases.rs`:
```rust
use charcoal::pipeline::scan_phases::staging::{AccountInput, ScanPhase, ACCOUNT_INPUT_SCHEMA_VERSION};

#[test]
fn scan_phase_roundtrips_through_str() {
    for p in [ScanPhase::Gather, ScanPhase::Burst, ScanPhase::Finalize, ScanPhase::Done] {
        assert_eq!(ScanPhase::from_value(p.as_str()), Some(p));
    }
    assert_eq!(ScanPhase::from_value("nonsense"), None);
}

#[test]
fn account_input_is_versioned_and_roundtrips() {
    let blob = AccountInput::new_for_test();
    let json = serde_json::to_string(&blob).unwrap();
    let back: AccountInput = serde_json::from_str(&json).unwrap();
    assert_eq!(back.schema_version, ACCOUNT_INPUT_SCHEMA_VERSION);
    assert_eq!(back, blob);
}
```
- [ ] **Step 3:** Run → FAIL (unresolved `staging`).
- [ ] **Step 4: Implement `staging.rs`:**
  - `pub const ACCOUNT_INPUT_SCHEMA_VERSION: u32 = 1;`
  - `pub enum ScanPhase { Gather, Burst, Finalize, Done }` + `as_str(&self) -> &'static str` (values `"gather"`/`"burst"`/`"finalize"`/`"done"`) + `from_value(&str) -> Option<Self>`. This is a typed wrapper over the `scan_state` value stored under `key="scan_phase"` — **not** a DB column.
  - `pub struct QueueRow { account_did, post_uri, text: String, context_text: Option<String>, post_kind: String, onnx_score: f64, status: String, toxic_token: Option<bool>, confidence: Option<f32>, model_id: Option<String>, policy_version: Option<String> }` (`#[derive(Debug, Clone, PartialEq)]`).
  - `pub struct VerdictRow { account_did, post_uri, toxic_token: bool, confidence: f32, model_id: String, policy_version: String }`.
  - `pub struct AccountInput { pub schema_version: u32, … }` (`#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]`). Fields cover every Stage-2 `build_profile` input that isn't a verdict: the 50-post sample (reuse the `posts` sample structs — add `#[derive(Serialize, Deserialize)]` to them in `src/bluesky/posts.rs` if missing), `parent_texts: HashMap<String,String>`, embeddings as needed, `median_engagement: f64`, `is_pile_on: bool` (precomputed `pile_on_dids.contains(account_did)` — store the bool, not the scan-global set), `direct_pairs: Option<Vec<(String,String)>>`, `graph_distance: Option<String>`, `fingerprint_quality: String`. Add `#[cfg(test)] pub fn new_for_test() -> Self`.
- [ ] **Step 5:** Run → PASS.
- [ ] **Step 6: Commit.** `git add src/pipeline/mod.rs src/pipeline/scan_phases/mod.rs src/pipeline/scan_phases/staging.rs tests/unit_scan_phases.rs` (+ `src/bluesky/posts.rs` if touched) → `git commit -m 'feat(decouple): staging value types — QueueRow/VerdictRow/AccountInput(versioned)/ScanPhase (#208)'`.

### Task 1.2: SQLite schema v9 (two tables, NO scan_state column)

**Files:** `src/db/schema.rs`. Test: `tests/unit_scan_phases.rs`.

- [ ] **Step 1: Failing test** — open in-memory `SqliteDatabase`; assert the two tables exist (e.g. `SELECT name FROM sqlite_master WHERE type='table' AND name IN ('classification_queue','scan_account_input')` returns 2). Model after existing schema-presence checks (`grep -rn "sqlite_master\|table_count" tests/ src/db`).
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3:** Add after the v8 block (`schema.rs:280`), via the existing `run_migration(conn, 9, |c| { … })` helper, the two `CREATE TABLE IF NOT EXISTS` + index from the spec's Data model (`classification_queue` PK `(user_did,account_did,post_uri)` + `idx_clsq_pending ON (user_did,status)`; `scan_account_input` PK `(user_did,account_did)`, `payload_json TEXT NOT NULL`). **Do NOT alter `scan_state`** — the phase marker is a key/value row.
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5: Commit.** `git add src/db/schema.rs tests/unit_scan_phases.rs` → `git commit -m 'feat(decouple): SQLite schema v9 — classification_queue + scan_account_input (#208)'`.

### Task 1.3: Postgres schema v9

**Files:** Create `migrations/postgres/0009_classification_staging.sql`; register in the Postgres migration list (`grep -rn "include_str!\|0008" src/db/postgres.rs`).

- [ ] **Step 1:** SQL file mirroring Task 1.2 with Postgres types (`DOUBLE PRECISION` reals, `BOOLEAN` toxic_token, `REAL` confidence, `JSONB` payload_json, same PKs + index). NO `scan_state` change.
- [ ] **Step 2:** Register next to `0008_*`.
- [ ] **Step 3:** `cargo build --features postgres` compiles. (Live run in Chunk 7.)
- [ ] **Step 4: Commit.** `git add migrations/postgres/0009_classification_staging.sql src/db/postgres.rs` → `git commit -m 'feat(decouple): Postgres schema v9 migration (#208)'`.

### Task 1.4: `Database` trait methods + SQLite impl

**Files:** `src/db/traits.rs`, `src/db/sqlite.rs`. Test: `tests/unit_scan_phases.rs`.

- [ ] **Step 1: Add 9 methods** to `traits.rs` (async, `user_did` first), under `// --- Classification staging (#208) ---`:
```rust
async fn enqueue_classifications(&self, user_did: &str, rows: &[QueueRow]) -> Result<()>;
async fn stash_account_input(&self, user_did: &str, account_did: &str, payload_json: &str) -> Result<()>;
async fn fetch_pending_classifications(&self, user_did: &str, limit: i64) -> Result<Vec<QueueRow>>;
async fn record_classification_verdicts(&self, user_did: &str, verdicts: &[VerdictRow]) -> Result<()>;
async fn list_scan_accounts(&self, user_did: &str) -> Result<Vec<String>>;
async fn fetch_account_verdicts(&self, user_did: &str, account_did: &str) -> Result<Vec<QueueRow>>;
async fn fetch_account_input(&self, user_did: &str, account_did: &str) -> Result<Option<String>>;
async fn count_pending_classifications(&self, user_did: &str) -> Result<i64>;
async fn clear_scan_staging(&self, user_did: &str) -> Result<()>;
```
  Import `QueueRow`/`VerdictRow` at top of `traits.rs`. **No phase getter/setter** — the phase marker uses the existing `set_scan_state(user_did, "scan_phase", v)` / `get_scan_state(user_did, "scan_phase")` (traits.rs ~36–39).
- [ ] **Step 2: Failing tests** (`#[tokio::test]`, in-memory `SqliteDatabase`; `upsert_user` first): enqueue→fetch_pending honors `pending`/`done`; `record_classification_verdicts` flips to `done`+stores verdict; enqueue UPSERT (same PK twice → one row); stash/fetch round-trip; `count_pending_classifications`; `clear_scan_staging` empties both tables; `list_scan_accounts` distinct; **and** that the phase marker works through the *existing* API: `set_scan_state(did,"scan_phase","burst")` then `get_scan_state(did,"scan_phase") == Some("burst")`.
- [ ] **Step 3:** Run → FAIL.
- [ ] **Step 4: Implement in `sqlite.rs`** (existing `Mutex<Connection>` pattern; batch writes in one lock; `INSERT … ON CONFLICT(user_did,account_did,post_uri) DO UPDATE SET …`). `fetch_pending` → `WHERE user_did=?1 AND status='pending' LIMIT ?2`. `record_…verdicts` → batched `UPDATE … SET status='done', toxic_token=?, confidence=?, model_id=?, policy_version=? WHERE …`.
- [ ] **Step 5:** Run → PASS (`cargo test --features web --test unit_scan_phases`).
- [ ] **Step 6: Commit.** `git add src/db/traits.rs src/db/sqlite.rs tests/unit_scan_phases.rs` → `git commit -m 'feat(decouple): Database staging methods + SQLite impl (#208)'`.

### Task 1.5: Postgres impl

**Files:** `src/db/postgres.rs`. Test: `tests/db_postgres.rs` (`--features postgres`).

- [ ] **Step 1:** Implement all 9 in `PgDatabase` via sqlx (JSONB payload, `ON CONFLICT … DO UPDATE`, `BOOLEAN` binding).
- [ ] **Step 2:** Add a Postgres round-trip test to `tests/db_postgres.rs` mirroring Task 1.4 (gated `--features postgres` + `DATABASE_URL`).
- [ ] **Step 3:** `cargo build --features postgres` + clippy clean. (Live run Chunk 7.)
- [ ] **Step 4: Commit.** `git add src/db/postgres.rs tests/db_postgres.rs` → `git commit -m 'feat(decouple): Postgres impl of staging methods (#208)'`.

### Chunk 1 review gate. `cargo test --features web` + clippy matrix green.

---

## Chunk 2: Enabling seam extraction + golden test

`build_profile` (a) fetches posts internally (can't feed canned samples) and (b) only holds a `&dyn ToxicityScorer` whose ONNX-clean-pass is fused with the RunPod call inside `TwoStageToxicityScorer::classify_post` (ensemble.rs:116–160). Both block the phase split and a deterministic golden test. This chunk extracts those seams **behavior-identically** (verified by the existing suite), then writes the golden test against the extracted, fetch-free core.

### Task 2.1: ONNX-clean-pass seam on the ensemble

**Files:** `src/toxicity/ensemble.rs`. Test: `tests/unit_ensemble.rs`.

- [ ] **Step 1: Failing test** in `tests/unit_ensemble.rs`: construct a `TwoStageToxicityScorer` with a `FixedScorer` primary whose scores straddle `ONNX_CLEAN_THRESHOLD` (0.10); assert a new method `onnx_clean_pass(&[String]) -> Result<Vec<f64>>` returns the primary ONNX scores **without** invoking the classifier (use a classifier double that panics if called, or assert via a call counter).
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3: Implement** `pub async fn onnx_clean_pass(&self, texts: &[String]) -> Result<Vec<f64>>` on `TwoStageToxicityScorer` that runs only `self.primary.score_batch` (the ONNX path) and returns per-text scores. Refactor `classify_post`/`classify_batch_with_contexts` to call this same helper for their clean-pass decision so the two paths cannot drift (DRY). Existing ensemble + composition tests stay green (behavior identical).
- [ ] **Step 4:** Run → PASS (`cargo test --features web --test unit_ensemble` + the broader suite for the refactor).
- [ ] **Step 5: Commit.** `git add src/toxicity/ensemble.rs tests/unit_ensemble.rs` → `git commit -m 'feat(decouple): expose ONNX-clean-pass seam on TwoStageToxicityScorer (#208)'`.

### Task 2.2: Extract `stage1_outcome` and `score_from_sample` from `build_profile`

**Files:** `src/scoring/profile.rs`. Test: existing suite (behavior identical) + new direct unit tests.

- [ ] **Step 1: Refactor `build_profile`** into:
  - `pub async fn stage1_outcome(stage1_sample: &PostSample, scorer: &dyn ToxicityScorer, protected_fingerprint, weights, …) -> Result<Stage1Outcome>` where `enum Stage1Outcome { Terminal(AccountScore), Proceed { stage1_overlap: Option<f64> } }`. Contains the `<5` and `should_early_exit_stage1` logic and builds the two terminal `AccountScore`s. **Takes the sample — no fetch.**
  - `pub async fn score_from_sample(stage2_sample: &PostSample, parent_texts, verdicts: &[ClassifierVerdict-or-bool], inputs…) -> Result<AccountScore>` — everything after `classify_batch_with_contexts` (reply-weighted tox rate → behavioral → context/NLI two-pass gate → graph distance → tier). **Takes the sample + verdicts — no fetch, no classify.**
  - `build_profile` keeps its existing signature and fetches 25 → `stage1_outcome` → (Proceed) fetch 50 → `classify_batch_with_contexts` → `score_from_sample`. **Net behavior identical.**
- [ ] **Step 2:** Run the **existing** test suite (composition, scoring, behavioral, golden-not-yet) → all green. This proves the extraction is behavior-preserving before any golden test exists.
- [ ] **Step 3: Add focused unit tests** for `stage1_outcome` (the two terminal branches) and `score_from_sample` (a survivor) against canned `PostSample`s + `FixedScorer`.
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5: Commit.** `git add src/scoring/profile.rs tests/...` → `git commit -m 'refactor(decouple): extract stage1_outcome + score_from_sample from build_profile (behavior-identical) (#208)'`.

### Task 2.3: Golden test against the extracted core

**Files:** Create `tests/golden_build_profile.rs` (+ `tests/fixtures/golden/` if canned samples are large).

- [ ] **Step 1: Author the golden cases** against `stage1_outcome` + `score_from_sample` (now fetch-free + deterministic) using `FixedScorer` and canned verdicts. Cover all four behaviors: (a) `<5 posts` → "Insufficient Data"; (b) early-exit → "Low"/`toxicity_score=0`/`scoring_confidence="low"`; (c) Stage-2 survivor with mixed toxic/clean verdicts → full score; (d) amplifier follower whose `raw_score >= 8.0` triggers the NLI two-pass gate (pass NLI scorer + pairs + `data_dir`). Snapshot exact `AccountScore` fields.
- [ ] **Step 2:** Run → PASS against current code (it describes today's behavior; must stay green through Chunks 3–6).
- [ ] **Step 3: Commit.** `git add tests/golden_build_profile.rs tests/fixtures/golden/` → `git commit -m 'test(decouple): golden baseline for stage1_outcome + score_from_sample + two-pass NLI (#208)'`.

### Chunk 2 review gate. Reviewer confirms the seams are behavior-identical and the golden covers all four behaviors.

---

## Chunk 3: Phase A — gather

### Task 3.1: `gather_account`

**Files:** Create `src/pipeline/scan_phases/gather.rs`; declare in `scan_phases/mod.rs`. Test: `tests/unit_scan_phases.rs`.

- [ ] **Step 1: Failing tests** (in-memory `SqliteDatabase` + `FixedScorer` + `StubClassifier` + canned samples via the fetch indirection — see Step 3):
  - `<5 posts` → `upsert_account_score` tier "Insufficient Data"; enqueues nothing; stashes nothing.
  - early-exit → `upsert_account_score` tier "Low"/`conf=low`; enqueues nothing.
  - survivor → enqueues per-post rows (clean→`done`+clean verdict, survivor→`pending`) using the **`onnx_clean_pass` seam** (Task 2.1); `stash_account_input` once (versioned blob); **no** `upsert_account_score` (deferred to Phase C).
  - idempotency: gather twice → one queue row per post (UPSERT).
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3: Implement `gather_account(db: &Arc<dyn Database>, user_did, client, scorer, classifier-not-needed, inputs…) -> Result<()>`:** fetch 25 → `stage1_outcome`; on `Terminal(score)` → `db.upsert_account_score`; on `Proceed` → fetch 50, run `scorer.onnx_clean_pass(&texts)` to split each post into clean(`done` with a clean verdict)/survivor(`pending`) `QueueRow`s, build the `AccountInput` blob (`is_pile_on`, `median_engagement`, `direct_pairs`, `graph_distance`, `fingerprint_quality`, `parent_texts`, sample), then `enqueue_classifications` + `stash_account_input`. Phase A does the fetching (it is the I/O phase); to keep tests deterministic, the fetch goes through the same `posts::` functions the test seam can feed canned data to — if those aren't injectable, add a thin `PostFetcher` indirection in this task and note it.
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5: Commit.** `git add src/pipeline/scan_phases/gather.rs src/pipeline/scan_phases/mod.rs tests/unit_scan_phases.rs` → `git commit -m 'feat(decouple): Phase A gather_account (#208)'`.

### Chunk 3 review gate.

---

## Chunk 4: Phase B — burst

### Task 4.1: `run_burst`

**Files:** Create `src/pipeline/scan_phases/burst.rs`. Test: `tests/unit_scan_phases.rs`.

- [ ] **Step 1: Failing tests** (in-memory `SqliteDatabase` + `StubClassifier`): drains all `pending` → all `done` + verdicts, `count_pending=0`; batching (limit < total → loops to empty); ceiling/402 (classifier returns `CostCeilingExceeded` mid-batch → loop stops, un-recorded rows stay `pending`, returns `BurstOutcome::CostCapped`; already-recorded verdicts persist).
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3: Implement** `run_burst(db, user_did, classifier: &Arc<dyn ToxicityClassifier>, burst_concurrency, burst_batch) -> Result<BurstOutcome>` with `enum BurstOutcome { Complete, CostCapped }`: loop { `fetch_pending_classifications(limit=burst_batch)`; break if empty; classify via `stream::iter(...).buffer_unordered(burst_concurrency)`; collect successful `VerdictRow`s; `record_classification_verdicts`; on `CostCeilingExceeded` stop → `CostCapped` }. Add `CHARCOAL_BURST_CONCURRENCY` (default ~16, clamped) + `BURST_BATCH` (default 500) reader helpers. Classifier is the existing `build_from_env()` value, so the #206 `ScanCostMeter` is inherited unchanged.
- [ ] **Step 4:** Run → PASS.
- [ ] **Step 5: Commit.** `git add src/pipeline/scan_phases/burst.rs src/pipeline/scan_phases/mod.rs tests/unit_scan_phases.rs` → `git commit -m 'feat(decouple): Phase B run_burst — contiguous loop, cost-capped resume (#208)'`.

### Chunk 4 review gate.

---

## Chunk 5: Phase C — finalize

### Task 5.1: `finalize_account`

**Files:** Create `src/pipeline/scan_phases/finalize.rs`. Test: `tests/unit_scan_phases.rs` + `tests/golden_build_profile.rs` stays green.

- [ ] **Step 1: Failing tests:** reads `fetch_account_verdicts` + `fetch_account_input`, deserializes blob, calls `score_from_sample` (Chunk 2), `upsert_account_score`. Cases: survivor with mixed verdicts → `AccountScore` matching the golden survivor; **two-pass NLI gate** — `raw_score >= 8.0` runs NLI with `nli_scorer` + pairs + `data_dir`; blob deserialize/`schema_version` mismatch → discard this account's staging rows, return `FinalizeOutcome::NeedsRegather`.
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3: Implement** `finalize_account(db, user_did, account_did, nli_scorer, data_dir, …) -> Result<FinalizeOutcome>` with `enum FinalizeOutcome { Scored, NeedsRegather }`. On deserialize/version mismatch: delete this account's `classification_queue` + `scan_account_input` rows (a targeted clear — add a small `clear_account_staging(user_did, account_did)` only if needed, else reuse a `DELETE … WHERE account_did`), return `NeedsRegather`. On success: `score_from_sample` (NLI two-pass gate here, using stashed pairs + `data_dir`), `upsert_account_score`.
- [ ] **Step 4:** Run → PASS (`unit_scan_phases` + `golden_build_profile`).
- [ ] **Step 5: Commit.** `git add src/pipeline/scan_phases/finalize.rs tests/unit_scan_phases.rs` → `git commit -m 'feat(decouple): Phase C finalize_account — score_from_sample + NLI gate + re-gather on version mismatch (#208)'`.

### Chunk 5 review gate.

---

## Chunk 6: Orchestration + call-site rewiring + resume

### Task 6.1: `run_phased_scan` state machine

**Files:** `src/pipeline/scan_phases/mod.rs`. Test: `tests/unit_scan_phases.rs`.

- [ ] **Step 1: Failing tests** (in-memory `SqliteDatabase` + stubs): fresh scan walks Gather→Burst→Finalize→Done, writing the `scan_phase` key (via `set_scan_state`) at each boundary; resume with `scan_phase="burst"` skips gather, re-selects `pending`; resume after `CostCapped` finalizes only all-`done` accounts and leaves the rest; `NeedsRegather` re-runs gather for that account then re-bursts; clean Done → `clear_scan_staging`.
- [ ] **Step 2:** Run → FAIL.
- [ ] **Step 3: Implement** `run_phased_scan(db, user_did, candidates, deps…) -> Result<ScanSummary>`: read `get_scan_state(user_did,"scan_phase")` → `ScanPhase::from_value`; dispatch; Phase A iterates candidates through `gather_account` with `buffer_unordered(concurrency)` (staleness gate applied by the caller's candidate filter — Task 6.2); `set_scan_state(…, "scan_phase","burst")`; `run_burst`; if `CostCapped` mark degraded + stop (resumable); `"finalize"`; iterate `list_scan_accounts` → `finalize_account` (skip accounts with leftover `pending`; handle `NeedsRegather`); `"done"`; `clear_scan_staging` (also clears the `scan_phase` key).
- [ ] **Step 4:** Run → PASS. **Step 5: Commit.**

### Task 6.2: Rewire `sweep::run` and `sweep::run_topic_first`

**Files:** `src/pipeline/sweep.rs`. Test: existing sweep tests + golden.

- [ ] **Step 1:** Replace the `buffer_unordered(build_profile-stream)` (sweep.rs ~140–168 and the topic-first variant) with candidate collection → `run_phased_scan`. Preserve the staleness entry gate as the candidate filter: `run` keeps `is_score_stale(...,7)` (sweep.rs:110); `run_topic_first` keeps `get_all_scored_dids` dedup (sweep.rs:205). Terminal accounts persist in Phase A; survivors in Phase C — incremental persistence preserved.
- [ ] **Step 2:** Full suite + golden green. **Commit.**

### Task 6.3: Rewire `amplification.rs`

**Files:** `src/pipeline/amplification.rs`. Test: existing amplification tests + golden.

- [ ] **Step 1:** Keep the event-recording loop (amplification.rs ~77–194: ONNX-only `score_with_context` + `nli.score_pair` + `insert_amplification_event`) in place (Phase-A-time, no RunPod). Replace the per-account `build_profile` calls — including the explicit pass-1/pass-2 follower loop at ~355–425 — with `run_phased_scan`, passing amplifiers' `direct_pairs` into the stashed `AccountInput`. The two-pass NLI gate now lives in `finalize_account` (Chunk 5). Drop the stale `#[allow(dead_code)]` on `is_scan_running_for` if that file is touched (spec advisory).
- [ ] **Step 2:** Full suite + golden green. **Commit.**

### Task 6.4: Observability

**Files:** `src/pipeline/scan_phases/mod.rs`.

- [ ] **Step 1:** Phase-transition `tracing` banners (gather/burst/finalize/done counts) + burst progress via `count_pending_classifications`. Reuse the existing `scan_cost_*`/classifier metrics — no new module. **Commit.**

### Chunk 6 review gate.

---

## Chunk 7: Full verification

- [ ] **Step 1:** `cargo test --features web` — all green (unit + golden + existing).
- [ ] **Step 2:** Clippy: `cargo clippy --features web --all-targets -- -D warnings`; `cargo clippy --all-targets -- -D warnings`; `cargo clippy --features postgres --all-targets -- -D warnings`.
- [ ] **Step 3:** Postgres: `DATABASE_URL=… cargo test --all-targets --features postgres` (staging round-trips + migration applies).
- [ ] **Step 4:** `cargo fmt --all`; commit any formatting (explicit names).
- [ ] **Step 5:** Manual smoke: small local `cargo run -- scan …` against a low-volume handle; confirm logs show the three phases in order, the burst as one contiguous window, a re-run skips fresh accounts, and the meter estimate now tracks the burst window (not the whole scan).
- [ ] **Step 6:** `CHANGELOG.md` Unreleased → Added entry referencing (#208). Commit.

---

## Out of scope (do NOT implement here)

- Onboarding wall-clock reduction / Phase A I/O parallelization / progressive results → #207.
- Any change to the meter's accounting model, scoring formulas, thresholds, the two-stage ONNX filter semantics, or UI.
- A `scan_id`/run-identifier — the single-in-flight-scan-per-user invariant (enforced globally by `ScanJobManager::try_start_scan`) makes it unnecessary; `clear_scan_staging` on fresh start is the backstop.

## Notes for the executor

- **Golden test is the contract:** if `tests/golden_build_profile.rs` goes red during Chunks 3–6, the restructure changed behavior — stop and reconcile, don't edit the golden values.
- **Why Chunk 2 comes before the phases:** `build_profile` fetches internally and fuses the ONNX clean-pass with the RunPod call. The seam extractions (ONNX-clean-pass on the ensemble; `stage1_outcome`/`score_from_sample` taking samples) are what make both the golden test deterministic and the phase split possible. They are behavior-identical and verified by the existing suite *before* the golden test is written.
- `build_profile` keeps working throughout (it shares `stage1_outcome` + `score_from_sample` + `onnx_clean_pass` with the phases) until Chunk 6 rewires the call sites.
- **Test doubles:** in-memory `SqliteDatabase` (no `Database` fake); `FixedScorer` for `ToxicityScorer`; `StubClassifier` for `ToxicityClassifier`. There is no `StubScorer` — author one from `FixedScorer` only if a case needs richer behavior.
- **Phase marker is a `scan_state` key/value row** (`key="scan_phase"`), via the existing `set_scan_state`/`get_scan_state` — NOT a column, NOT a new trait method.
- Batch every staging write (`enqueue_classifications`, `record_classification_verdicts`) — per-row awaits serialize hard on SQLite's `Mutex<Connection>`.
- The classifier in Phase B is `build_from_env()` unchanged, so the #206 cost backstop + `ScanCostMeter` are inherited with zero new wiring.
