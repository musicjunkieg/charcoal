# #343 Phase 2 — Score Expiry and Nightly Refresh Implementation Plan (rev 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Revision 2 (2026-09-14)** resolves Astra's plan review (REQUEST CHANGES on `c91afb8`, findings R01–R13). The review-resolution table is at the end. Every task below is the revised contract; the first draft's snippets it contradicted are gone, not annotated.

**Goal:** Every stored score carries a generation stamp and an expiry; tier lists show only current, unexpired rows; and a nightly refresh job, driven from the existing admitter tick, re-scores the High/Elevated set before it expires — closing #344 and giving #342 the scheduler seam it needs — without losing scores in migration, without stranding its own interrupted work, and without stamping stale inputs as current.

**Architecture:** Migration v18 adds `scoring_generation` + `valid_until` to `account_scores`, `kind` + `full_requested_at` to `scan_queue`, `next_refresh_at` + `refreshed_generation` to `users`, `embedding_model_id` to `topic_fingerprint`, and backfills the full-scan cooldown marker. The write path stamps scores from a build-time `SCORING_GENERATION` and `ScoringConfidence::staleness_days()`. One null-safe **fresh** predicate serves every tier read, count and pipeline gate; a separate **lossless** export/import serves `charcoal migrate`. The refresh is a second queue kind that reuses the admitter, lease/fencing and `run_phased_scan`; its candidates come from `account_scores`. Resumable staging records which run kind and generation owns it, so a refresh resumes its own work, a full scan drains a refresh's leftovers before gathering, and old-generation staging is discarded. Scheduling is one transaction per tick (claim + enqueue together, bounded), and a generation bump makes users due through a durable `refreshed_generation` column, not a one-off migration.

**Tech Stack:** Rust 2021, tokio, async-trait, rusqlite 0.38 (SQLite) + sqlx-core/sqlx-postgres (Postgres), chrono, serde_json, tracing; SvelteKit 5 + vitest for the frontend change.

**Spec:** `docs/superpowers/specs/2026-09-08-343-scalability-onboarding-design.md` §4.4 (S4), §6 Phase 2, §7. Deciduous decision **777** is the policy; plan action **872**, revision action **877**, revisit **878** (drops the 80 % hit-rate gate).

## Global Constraints

- **Branch/PR:** work on `feat/343-phase2-expiry-refresh` off `staging`; PR to `staging`; done only at **CodeRabbit APPROVED**. CodeRabbit allows **5 reviews/hour — if it says wait, wait.** Batch fixes into fewer pushes.
- **Chainlink issue before code** (hook-enforced): `chainlink session work 344`. Close with `chainlink issue close 344 --no-changelog`; handwrite the CHANGELOG entry (Task 10). Do **not** close #344 when this plan merges — only when its implementation ships and the runbook passes.
- **Git:** explicit `git add <paths>`; no `git add -A`; no heredocs anywhere; single-quoted multi-line commit messages ending with `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX`. `git merge/rebase/cherry-pick/reset/tag/branch -D` are blocked. Push in the background: `git -c credential.helper='!gh auth git-credential' push https://github.com/musicjunkieg/charcoal.git feat/343-phase2-expiry-refresh`.
- **Deciduous:** log `action` (`--commit HEAD`) before and `outcome` after each task; link each action from **877** immediately. Verify node IDs with `deciduous nodes | tail` before every link.
- **TDD:** failing test first, then code (spec §7). The verification commands below are the only accepted forms (R10):
  - `VERIFY_WEB` — the model-gated suite, failing on any skip:
    ```bash
    set -o pipefail
    CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | tee target/test-web.log
    ! grep -qE '^\s*SKIP:' target/test-web.log
    ```
    Both lines must succeed; `pipefail` keeps cargo's exit status, the negated grep fails the step on a `SKIP:` sentinel. Run it as three separate shell lines, never through a `| grep` alone.
  - `VERIFY_PG` — `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres` (`createdb charcoal_test` once if missing). Postgres tests must **fail**, not return, when `DATABASE_URL` is unset in CI (Task 2 adds the guard).
  - `VERIFY_CLIPPY` — `cargo clippy --features web --all-targets && cargo clippy --features postgres --all-targets && cargo clippy --all-targets`.
  - `VERIFY_FE` — `npm --prefix web run test && npm --prefix web run check && npm --prefix web run build`.
  - A single cargo test filter is one positional argument: `cargo test --test unit_staleness fresh_set` (substring). To run several unrelated tests, run several commands. "Run to verify it fails" steps must state the **expected failure kind** — a compile error in a test-first step is acceptable only where the step says so; a wrong assertion failure is never mistaken for a compile failure.
- **Migrations** get a fresh-DB test and an upgrade-from-v17 test on **both** backends. Postgres migrations **self-record** their version (`INSERT INTO schema_version (version) VALUES (18) ON CONFLICT DO NOTHING;`). This is **v18** — v17 shipped in Phase 1.
- **Every new env knob has a clamp/default test.** Knobs: `CHARCOAL_REFRESH_INTERVAL_HOURS` (default 24, `0`/`off` disables, clamp 1..=168). Constants (not knobs): `REFRESH_RETRY_HOURS = 1`, `REFRESH_BATCH_PER_TICK = 25`, `REFRESH_HORIZON_DAYS = 2`.
- **The fresh predicate is the single source of truth and is boolean-explicit (R11).** SQLite: `scoring_generation = ? AND COALESCE(datetime(valid_until) > datetime('now'), 0)`. Postgres: `scoring_generation = $n AND valid_until > NOW()`. `NOT (fresh)` must be true for NULL **and malformed** `valid_until` on SQLite; `count_expired`, `get_ranked_threats`, `count_not_assessed`, `is_score_stale`, `get_fresh_scored_dids`, and topic-first discovery (R06) all use it. Candidate selection uses the complementary `COALESCE(datetime(valid_until) <= datetime('now', ?h), 1)` so malformed rows are eligible for refresh.
- **Lossless export/import is a separate contract (R01).** `export_scores` / `import_score` carry `scored_at`, `scoring_generation`, `valid_until` and NULL-score rows verbatim; `charcoal migrate` uses them and never `get_ranked_threats`. Importing never renews expiry.
- **Freshness reads are hard errors** (spec §4.4): the pipeline's `.unwrap_or_default()` calls become `?`.
- **Nothing is deleted.** Expired and legacy rows stay; they are hidden and counted as `expired`. Re-entry is by re-engagement (full scan) or by the refresh job (High/Elevated only — decision 777).
- **Staging ownership (R02/R03).** `scan_state` carries `scan_run_kind` (`full`|`refresh`) and `scan_run_generation` next to `scan_phase`. A resumable marker (`burst`/`finalize`) is resumed by its own kind, drained by a full scan before it gathers, and discarded (staging cleared) when its generation is not current.
- **Input compatibility (R03).** A current-generation score may be published only from: a fingerprint whose `embedding_model_id` equals `EMBEDDING_MODEL_ID` (or a keyword-only fingerprint with no embedding), an `AccountInput` blob whose `scoring_generation` is current, and verdict rows whose `policy_version` equals the running classifier's. Full scans rebuild an incompatible fingerprint; refreshes request a full scan instead (they never rebuild).
- **Generation bump procedure.** `SCORING_GENERATION` changes only for formula/weights, fingerprint format, or scoring-policy changes. Model changes are tracked by their own identities (`ONNX_MODEL_ID`, `EMBEDDING_MODEL_ID`, classifier `model_id`+`policy_version`) which key the caches and the compatibility checks above; a bump therefore does **not** invalidate the ONNX/classifier caches, and a model change does not require a bump to be safe. Rolling deploys: a generation bump is shipped as a single-replica deploy (Railway's default); during the seconds of overlap the old binary can still stamp its in-flight scan's rows with the old generation, which the new binary hides and the refresh re-scores — acceptable, documented in the runbook.
- **Queue order stays FIFO across kinds** (#271). A full enqueue over a queued refresh upgrades the row **in place, keeping `enqueued_at`** (R09). A full request during a *running* refresh is recorded durably in `scan_queue.full_requested_at` and becomes a queued full row when the refresh finishes.
- **Durable scheduling (R04).** Schedule advancement and queue-row creation happen in one transaction per backend, bounded to `REFRESH_BATCH_PER_TICK` users per tick. A failed transaction advances nothing and is retried on the next tick.
- **Errors are not absence (R05).** Helpers that load context return `Result`; a refresh that cannot obtain required context fails the run (row `failed`, retry in `REFRESH_RETRY_HOURS`) and writes no score. The existing High/Elevated row keeps its own expiry — a failed refresh never extends validity.
- **SQLite/Postgres divergence, deliberate:** Postgres `valid_until` is `NOT NULL` after backfill; SQLite stays nullable and NULL reads as expired.
- **Pass numbers (spec §6 Phase 2, as amended by Task 10):** after deploy every pre-existing row is hidden and `tier_counts.expired` = row count; the first tick enqueues a refresh for every user with scores (via `refreshed_generation`, not migration); the refresh re-scores exactly the High/Elevated set; no `legacy` row ≥ Elevated remains after one refresh per user; the feed-cache **functional** test passes (warm eligible candidate → hit, cold → miss, zero-candidate run → not applicable). The former ≥ 80 % hit-rate gate is withdrawn (R08, deciduous 878).
- **Privacy:** never log tokens, DPoP proofs, request bodies, `CHARCOAL_TOKEN_KEY`, `SOOT_TOKEN`; never log post text; strip credentials from any `DATABASE_URL` printed.
- **Clippy clean** on all three feature sets. Comments explain **why**; `?` for errors; `anyhow::Result` at application level.

---

## Dependency order

```
1 generation + identities ─┐
2 migration v18 ───────────┼─→ 3 fresh predicate + export/import ─→ 4 expired UI
                           │                                       ↘
5 queue kind + pending full┼─→ 6 durable scheduling ─→ 9 run_refresh ─→ 10 docs
                           │                     ↗            ↑
                           └─→ 7 candidates + index      8 helpers (Result)
```

Tasks 1–3 first, then 5 → 6, then 7 and 8 (independent), then 9, then 4 any time after 3, then 10.

## File map

| Path | Responsibility |
|---|---|
| `src/scoring/generation.rs` (new) | Task 1: `SCORING_GENERATION`, `LEGACY_GENERATION`, bump rule. |
| `src/topics/embeddings.rs` | Task 1: `EMBEDDING_MODEL_ID`. |
| `src/db/models.rs` | Task 1: `ScoringConfidence::from_label`/`staleness_days_for_label`; Task 7: `ThreatTier::ELEVATED_MIN`; Task 3: `StoredScore`. |
| `src/db/schema.rs`, `migrations/postgres/0018_score_expiry.sql` (new), `src/db/postgres.rs` | Task 2: migration v18 + backfills; version-list assertions → 18. |
| `src/db/traits.rs`, `src/db/queries.rs`, `src/db/sqlite.rs`, `src/db/postgres.rs`, `src/db/mod.rs` | Task 3: fresh predicate, `count_expired`, `export_scores`/`import_score`, `fingerprint_embedding_model`, `save_fingerprint_bundle` stamps model id. Task 5: `ScanKind`, `enqueue_refresh_scan`, `request_full_after_refresh`, finish flips. Task 6: `claim_and_enqueue_due_refreshes`, `schedule_refresh`, `mark_refreshed_generation`. Task 7: `RefreshCandidate`, `list_refresh_candidates`. |
| `src/main.rs` (`migrate`) | Task 3: lossless copy + schedule init. |
| `src/pipeline/sweep.rs`, `src/pipeline/amplification.rs` | Task 3: hard-error freshness, `run_topic_first` uses fresh set. Task 8: `direct_pairs_for -> Result`. |
| `src/pipeline/scan_phases/staging.rs`, `gather.rs`, `finalize.rs`, `mod.rs` | Task 1: blob `scoring_generation`, schema v3; Task 9: `RunIdentity`, ownership markers, verdict policy check. |
| `src/web/handlers/status.rs`, `src/status.rs`, `web/src/lib/types.ts`, `web/src/lib/dashboard-state.ts` (+test), `web/src/routes/(protected)/dashboard/+page.svelte` | Task 4: `expired` count. |
| `src/web/handlers/scan.rs`, `src/web/handlers/admin.rs`, `web/src/lib/types.ts` | Task 5: cooldown anchor, 202 body says what was queued, admin `kind`/`full_requested_at`. |
| `src/web/refresh.rs` (new), `src/web/admitter.rs` | Task 6: knob, tick, bounded transactional scheduling. |
| `src/web/scan_job.rs` | Task 5: `last_full_scan_finished_at`; Task 6: schedule + `refreshed_generation` after success; Task 8: extracted helpers; Task 9: `launch_scan(kind)`, full-drains-refresh transition. |
| `src/web/refresh_scan.rs` (new) | Task 9: `run_refresh`, `RefreshOutcome`, `refresh_plan`, `to_candidate`. |
| `tests/unit_scoring.rs`, `tests/unit_staleness.rs`, `tests/unit_score_export.rs` (new), `tests/unit_scan_kind.rs` (new), `tests/unit_refresh_candidates.rs` (new), `tests/unit_direct_pairs.rs` (new), `tests/unit_scan_phases.rs`, `tests/db_postgres.rs`, `tests/web_oauth.rs` | Tests per task. |
| `CHANGELOG.md`, `README.md`, `docs/runbooks/343-phase2-expiry-refresh.md` (new), spec §4.4/§6 | Task 10. |

---

### Task 1: Generation constant, model identities, and the confidence → staleness mapping

**Why:** Spec §4.4's build-time generation, plus the two identities R03 needs: an `EMBEDDING_MODEL_ID` so a stored fingerprint can say which model produced its vectors (384 dimensions is not an identity — a different model with the same width is incompatible), and a `scoring_generation` on the staged `AccountInput` so a resumed finalize cannot publish a current stamp from an old run's inputs.

**Files:**
- Create: `src/scoring/generation.rs`; Modify: `src/scoring/mod.rs`
- Modify: `src/topics/embeddings.rs:24` (next to `EMBEDDING_DIM`)
- Modify: `src/db/models.rs` (`impl ScoringConfidence`)
- Modify: `src/pipeline/scan_phases/staging.rs:22` (`ACCOUNT_INPUT_SCHEMA_VERSION` → 3) and `:131-160` (`AccountInput.scoring_generation`); `src/pipeline/scan_phases/gather.rs:511-523` (stamp it); `src/pipeline/scan_phases/finalize.rs:98-108` (reject mismatch)
- Test: `tests/unit_scoring.rs` (append); `tests/unit_scan_phases.rs` (append)

**Interfaces:**
- Produces: `charcoal::scoring::generation::{SCORING_GENERATION, LEGACY_GENERATION}`; `charcoal::topics::embeddings::EMBEDDING_MODEL_ID: &str = "all-MiniLM-L6-v2"`; `ScoringConfidence::from_label(&str) -> Option<Self>`; `ScoringConfidence::staleness_days_for_label(Option<&str>) -> i64`; `AccountInput.scoring_generation: String` (blob schema v3).

- [ ] **Step 1: Write the failing tests**

Append to `tests/unit_scoring.rs`:

```rust
// --- #343 Phase 2 / #344: generation stamp, model identity, label parsing ---

#[test]
fn scoring_generation_is_a_real_stamp_not_legacy() {
    use charcoal::scoring::generation::{LEGACY_GENERATION, SCORING_GENERATION};
    assert!(!SCORING_GENERATION.is_empty());
    assert_ne!(SCORING_GENERATION, LEGACY_GENERATION);
    assert_eq!(LEGACY_GENERATION, "legacy", "migration v18 backfills this literal");
    assert!(!SCORING_GENERATION.contains(char::is_whitespace));
}

#[test]
fn embedding_model_id_names_the_shipped_model() {
    use charcoal::topics::embeddings::EMBEDDING_MODEL_ID;
    // Migration v18 backfills this literal onto every fingerprint that has a
    // vector, because it is the only embedding model Charcoal has ever run.
    assert_eq!(EMBEDDING_MODEL_ID, "all-MiniLM-L6-v2");
}

#[test]
fn scoring_confidence_round_trips_through_its_label() {
    for c in [ScoringConfidence::Low, ScoringConfidence::Standard, ScoringConfidence::High] {
        assert_eq!(ScoringConfidence::from_label(c.as_str()), Some(c));
    }
    assert_eq!(ScoringConfidence::from_label("bogus"), None);
    assert_eq!(ScoringConfidence::from_label(""), None);
}

#[test]
fn staleness_days_for_label_falls_back_to_standard() {
    assert_eq!(ScoringConfidence::staleness_days_for_label(Some("low")), 3);
    assert_eq!(ScoringConfidence::staleness_days_for_label(Some("standard")), 7);
    assert_eq!(ScoringConfidence::staleness_days_for_label(Some("high")), 14);
    assert_eq!(ScoringConfidence::staleness_days_for_label(None), 7);
    assert_eq!(ScoringConfidence::staleness_days_for_label(Some("bogus")), 7);
}
```

Append to `tests/unit_scan_phases.rs` (inside the module that already has `open_db`, `astrophysics_fingerprint`, `CannedFetcher`, `FixedScorer`, `inputs` — the helpers used by `one_poisoned_post_no_longer_costs_the_whole_account`):

```rust
    /// R03: a staged blob from another scoring generation must not be
    /// finalized under the current stamp — Phase C rejects it and asks for a
    /// re-gather, exactly as it does for a schema-version mismatch.
    #[tokio::test]
    async fn finalize_rejects_a_blob_from_another_generation() {
        use charcoal::pipeline::scan_phases::staging::{AccountInput, ACCOUNT_INPUT_SCHEMA_VERSION};
        let db = open_db().await;
        let blob = AccountInput {
            schema_version: ACCOUNT_INPUT_SCHEMA_VERSION,
            scoring_generation: "1999-01-01".to_string(),
            account_handle: "old.handle".to_string(),
            sample: PostSample {
                originals: vec![],
                replies: vec![],
                quotes: vec![],
                reply_ratio: 0.0,
                quote_ratio: 0.0,
                total_posts: 0,
            },
            parent_texts: HashMap::new(),
            median_engagement: 0.0,
            is_pile_on: false,
            direct_pairs: None,
            graph_distance: None,
            fingerprint_quality: "normal".to_string(),
            target_embedding: None,
        };
        db.stash_account_input(TEST_USER, "did:plc:oldgen", &serde_json::to_string(&blob).unwrap())
            .await
            .unwrap();

        let outcome = finalize_account_for_test(&db, TEST_USER, "did:plc:oldgen").await;
        assert_eq!(outcome, charcoal::pipeline::scan_phases::finalize::FinalizeOutcome::NeedsRegather);
        assert!(
            db.fetch_account_input(TEST_USER, "did:plc:oldgen").await.unwrap().is_none(),
            "staging for the old-generation blob is cleared so re-gather starts clean"
        );
    }
```

`finalize_account_for_test` is a thin wrapper the file may already have for `finalize_account` (search `rg -n "finalize_account\(" tests/unit_scan_phases.rs`); if not, add one that passes the fingerprint from `astrophysics_fingerprint()`, `ThreatWeights::default()`, and `None` for embedder/embedding/centroids/NLI/data_dir. `fingerprint_quality`'s type is whatever `AccountInput` declares (`String` today) — match it.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --test unit_scoring generation` and `cargo test --test unit_scoring label` and `cargo test --features web --test unit_scan_phases another_generation`
Expected: compile errors (module, constant, field and method do not exist) — this step is expected to fail at compile time.

- [ ] **Step 3: Implement**

Create `src/scoring/generation.rs`:

```rust
//! The scoring generation stamp (#343 §4.4, #344).
//!
//! Every `account_scores` row records the generation it was scored under. A
//! row is *fresh* only if its generation equals [`SCORING_GENERATION`] **and**
//! its `valid_until` is in the future; anything else is hidden from tier lists
//! and re-scored by the refresh job (High/Elevated) or on the account's next
//! re-engagement (everything else).
//!
//! # When to bump
//!
//! Change the value when a stored score is no longer comparable to a freshly
//! computed one **for reasons the model identities do not already capture**:
//! - the threat formula or its weights (`src/scoring/threat.rs`)
//! - the topic-overlap math or the fingerprint JSON format (`src/topics/`)
//! - a scoring-policy change (tier thresholds, abstention rules)
//!
//! Do **not** bump for a model swap. Models carry their own identities, which
//! key the caches and the compatibility checks: `ONNX_MODEL_ID`
//! (`toxicity/onnx.rs`), `EMBEDDING_MODEL_ID` (`topics/embeddings.rs`, stored
//! on `topic_fingerprint.embedding_model_id`), and the classifier's
//! `model_id` + `policy_version` (stored on every verdict row). A generation
//! bump therefore never invalidates those caches, and a model change is safe
//! without a bump: an incompatible fingerprint is rebuilt by the next full
//! scan, and staged verdicts from another policy are re-classified.
//!
//! Bumping is a code change, not a config knob: two replicas disagreeing on
//! the generation would hide each other's scores. The value is opaque; the
//! date form is for humans reading `SELECT scoring_generation, COUNT(*) …`.
//!
//! # Rolling deploys
//!
//! A bump ships as a single-replica deploy. During the seconds both binaries
//! run, the old one may still stamp its in-flight scan's rows with the old
//! generation; the new binary hides those rows and the refresh job re-scores
//! the High/Elevated ones. Staged work the old binary left behind carries the
//! old generation in `scan_state.scan_run_generation` and is discarded on the
//! next run (see `pipeline::scan_phases::RunIdentity`).
pub const SCORING_GENERATION: &str = "2026-09-13";

/// The stamp migration v18 writes onto rows scored before generations
/// existed. Never equal to [`SCORING_GENERATION`].
pub const LEGACY_GENERATION: &str = "legacy";
```

`src/scoring/mod.rs`: `pub mod generation;`. `src/topics/embeddings.rs`, after `EMBEDDING_DIM`:

```rust
/// Identity of the embedding model behind every vector this module produces.
/// Stored on `topic_fingerprint.embedding_model_id` (migration v18) so a
/// fingerprint built by a different model — even one with the same
/// `EMBEDDING_DIM` — is recognised as incompatible and rebuilt rather than
/// compared against candidate vectors from this one (#344, R03).
pub const EMBEDDING_MODEL_ID: &str = "all-MiniLM-L6-v2";
```

`src/db/models.rs`, inside `impl ScoringConfidence`:

```rust
    /// Inverse of [`as_str`](Self::as_str). `None` for anything the enum
    /// never produced.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "low" => Some(ScoringConfidence::Low),
            "standard" => Some(ScoringConfidence::Standard),
            "high" => Some(ScoringConfidence::High),
            _ => None,
        }
    }

    /// Staleness window for a stored `scoring_confidence` label (#344).
    /// `None` (NotAssessed / insufficient-data rows) and unknown labels fall
    /// back to Standard (7 days) — never 0, never forever.
    pub fn staleness_days_for_label(label: Option<&str>) -> i64 {
        label
            .and_then(Self::from_label)
            .unwrap_or(ScoringConfidence::Standard)
            .staleness_days()
    }
```

`src/pipeline/scan_phases/staging.rs`: `pub const ACCOUNT_INPUT_SCHEMA_VERSION: u32 = 3;` and add to `AccountInput` after `schema_version`:

```rust
    /// The `SCORING_GENERATION` this blob was gathered under (schema v3,
    /// #344 R03). Phase C refuses to finalize a blob from another generation:
    /// its target embedding, sample selection and fingerprint quality were
    /// computed against inputs the current formula may not accept.
    pub scoring_generation: String,
```

`gather.rs:511`: `scoring_generation: crate::scoring::generation::SCORING_GENERATION.to_string(),` in the `AccountInput { … }` literal. `finalize.rs`, after the schema-version check (`:98-108`):

```rust
    if blob.scoring_generation != crate::scoring::generation::SCORING_GENERATION {
        warn!(
            account_did,
            blob_generation = %blob.scoring_generation,
            current = crate::scoring::generation::SCORING_GENERATION,
            "AccountInput scoring_generation mismatch — clearing staging and re-gathering"
        );
        db.clear_account_staging(user_did, account_did).await?;
        return Ok(FinalizeOutcome::NeedsRegather);
    }
```

Every other `AccountInput { … }` literal in tests (`rg -n "AccountInput \{" src tests`) gains `scoring_generation: SCORING_GENERATION.to_string()`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test unit_scoring` then `VERIFY_WEB` (the blob shape change touches model-gated tests).
Expected: all pass, zero `SKIP:`.

- [ ] **Step 5: Commit**

```bash
git add src/scoring/generation.rs src/scoring/mod.rs src/topics/embeddings.rs src/db/models.rs src/pipeline/scan_phases/staging.rs src/pipeline/scan_phases/gather.rs src/pipeline/scan_phases/finalize.rs tests/unit_scoring.rs tests/unit_scan_phases.rs
git commit -m 'feat(344): SCORING_GENERATION + EMBEDDING_MODEL_ID; staged blobs carry their generation

The generation constant every score row will carry (#343 §4.4), the
embedding model identity fingerprints will record, and blob schema v3 so
Phase C refuses to finalize inputs gathered under another generation.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 2: Migration v18 — expiry, queue kind, refresh schedule, model identity, cooldown backfill

**Why:** Spec §4.4's columns, plus what the review showed the first draft was missing: `users.refreshed_generation` so a generation bump makes users due durably (R07, replacing the one-off `next_refresh_at = NOW()` backfill); `scan_queue.full_requested_at` so a user's full-scan request during a running refresh survives (R09); `topic_fingerprint.embedding_model_id` (R03); a backfilled `scan_state.last_full_scan_finished_at` so the cooldown survives the first refresh reusing the queue row (R13); and an index chosen for the query that actually runs (R12).

**Files:**
- Modify: `src/db/schema.rs` (after the v17 block; the two `(1..=17)` assertions → 18; tests)
- Create: `migrations/postgres/0018_score_expiry.sql`; Modify: `src/db/postgres.rs:183-187`
- Test: `src/db/schema.rs` tests; `tests/db_postgres.rs`

**Interfaces (columns):**
- `account_scores.scoring_generation TEXT NOT NULL` (`DEFAULT 'legacy'` SQLite; default dropped on Postgres after backfill); `account_scores.valid_until` (`TEXT` nullable SQLite / `TIMESTAMPTZ NOT NULL` Postgres), format follows `scored_at`.
- Index `idx_account_scores_user_score (user_did, threat_score)` — serves `list_refresh_candidates` (`user_did = ? AND threat_score >= ?` then a small residual filter) and `get_ranked_threats` ordering. The first draft's `(user_did, threat_tier, valid_until)` is **not** created: the candidate query does not constrain `threat_tier`, and SQLite wraps `valid_until` in `datetime()`, so that index could serve neither filter (R12). Task 7 records `EXPLAIN` plans for both backends.
- `scan_queue.kind TEXT NOT NULL DEFAULT 'full'` (Postgres `CHECK (kind IN ('full','refresh'))`); `scan_queue.full_requested_at` (`TEXT`/`TIMESTAMPTZ` nullable).
- `users.next_refresh_at` (`TEXT` RFC3339 / `TIMESTAMPTZ`, nullable); `users.refreshed_generation TEXT` nullable — the generation under which this user's High/Elevated set was last refreshed or fully scanned. NULL after migration ⇒ due on the first tick.
- `topic_fingerprint.embedding_model_id TEXT` nullable; backfilled to `'all-MiniLM-L6-v2'` where `embedding_vector IS NOT NULL`.
- `scan_state (user_did, 'last_full_scan_finished_at')` backfilled from `scan_queue` rows with `status = 'done'` (every pre-v18 row was a full scan).

- [ ] **Step 1: Write the failing SQLite tests**

In `src/db/schema.rs` tests, after `test_migration_v17_upgrades_a_v16_database`:

```rust
    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})")).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1)).unwrap().map(Result::unwrap).collect()
    }

    fn has_column(conn: &Connection, table: &str, col: &str) -> bool {
        column_names(conn, table).iter().any(|c| c == col)
    }

    /// v18 (#344): expiry + generation on account_scores, kind +
    /// full_requested_at on scan_queue, next_refresh_at + refreshed_generation
    /// on users, embedding_model_id on topic_fingerprint, the (user_did,
    /// threat_score) index, and the version row.
    #[test]
    fn test_migration_v18_adds_expiry_and_refresh_columns() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        for (table, col) in [
            ("account_scores", "scoring_generation"),
            ("account_scores", "valid_until"),
            ("scan_queue", "kind"),
            ("scan_queue", "full_requested_at"),
            ("users", "next_refresh_at"),
            ("users", "refreshed_generation"),
            ("topic_fingerprint", "embedding_model_id"),
        ] {
            assert!(has_column(&conn, table, col), "{table}.{col}");
        }
        let has_index: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_account_scores_user_score'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(has_index);
        let recorded: bool = conn
            .query_row("SELECT COUNT(*) > 0 FROM schema_version WHERE version = 18", [], |r| r.get(0))
            .unwrap();
        assert!(recorded);
    }

    /// A v17 database with live data. The upgrade must: stamp every score
    /// 'legacy' with valid_until = scored_at + 14 d; default queue rows to
    /// 'full'; leave next_refresh_at NULL and refreshed_generation NULL (the
    /// tick makes users with scores due — R07); stamp fingerprints that have a
    /// vector with the embedding model id; and copy each done queue row's
    /// finished_at into scan_state.last_full_scan_finished_at (R13).
    #[test]
    fn test_migration_v18_upgrades_a_v17_database() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn.execute_batch(
            "DROP INDEX idx_account_scores_user_score;
             ALTER TABLE account_scores DROP COLUMN scoring_generation;
             ALTER TABLE account_scores DROP COLUMN valid_until;
             ALTER TABLE scan_queue DROP COLUMN kind;
             ALTER TABLE scan_queue DROP COLUMN full_requested_at;
             ALTER TABLE users DROP COLUMN next_refresh_at;
             ALTER TABLE users DROP COLUMN refreshed_generation;
             ALTER TABLE topic_fingerprint DROP COLUMN embedding_model_id;
             DELETE FROM scan_state WHERE key = 'last_full_scan_finished_at';
             DELETE FROM schema_version WHERE version = 18;",
        )
        .unwrap();
        conn.execute_batch(
            "INSERT INTO users (did, handle) VALUES ('did:plc:scored', 'scored.test');
             INSERT INTO users (did, handle) VALUES ('did:plc:unscored', 'unscored.test');
             INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at)
                 VALUES ('did:plc:scored', 'did:plc:acct', 'acct.test', 40.0, 'High', '2026-09-01 12:00:00');
             INSERT INTO account_scores (user_did, did, handle, threat_tier, scored_at)
                 VALUES ('did:plc:scored', 'did:plc:na', 'na.test', 'NotAssessed', '2026-09-02 12:00:00');
             INSERT INTO scan_queue (user_did, status, enqueued_at, started_at, finished_at)
                 VALUES ('did:plc:scored', 'done', '2026-09-01T11:00:00+00:00',
                         '2026-09-01T11:00:00+00:00', '2026-09-01T12:00:00+00:00');
             INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, embedding_vector)
                 VALUES ('did:plc:scored', '{\"clusters\":[],\"post_count\":0}', 0, '[0.1,0.2]');
             INSERT INTO topic_fingerprint (user_did, fingerprint_json, post_count, embedding_vector)
                 VALUES ('did:plc:unscored', '{\"clusters\":[],\"post_count\":0}', 0, NULL);",
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
        let na_valid: String = conn
            .query_row("SELECT valid_until FROM account_scores WHERE did = 'did:plc:na'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(na_valid, "2026-09-16 12:00:00", "NULL-score rows are backfilled too");

        let kind: String = conn
            .query_row("SELECT kind FROM scan_queue WHERE user_did = 'did:plc:scored'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "full");

        let (next, refreshed): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT next_refresh_at, refreshed_generation FROM users WHERE did = 'did:plc:scored'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert!(next.is_none() && refreshed.is_none(), "due-ness comes from refreshed_generation IS NULL, not a stamped time");

        let with_vec: Option<String> = conn
            .query_row("SELECT embedding_model_id FROM topic_fingerprint WHERE user_did = 'did:plc:scored'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(with_vec.as_deref(), Some("all-MiniLM-L6-v2"));
        let without_vec: Option<String> = conn
            .query_row("SELECT embedding_model_id FROM topic_fingerprint WHERE user_did = 'did:plc:unscored'", [], |r| r.get(0))
            .unwrap();
        assert!(without_vec.is_none(), "keyword-only fingerprints have no embedding model");

        let marker: String = conn
            .query_row(
                "SELECT value FROM scan_state WHERE user_did = 'did:plc:scored' AND key = 'last_full_scan_finished_at'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(marker, "2026-09-01T12:00:00+00:00", "cooldown anchor backfilled from the done row");

        let max: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(max, 18);
    }
```

Change both `(1..=17)` assertions and their comments to 18.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib db::schema::tests`
Expected: the two v18 tests fail with "no such column" (runtime failure, not compile), and the two version-list tests fail on `17 != 18`.

- [ ] **Step 3: Write the SQLite migration**

In `src/db/schema.rs`, after the v17 block:

```rust
    // v18 (#343 §4.4, #344): score expiry + refresh bookkeeping.
    //
    // account_scores: generation stamp + expiry. Existing rows become 'legacy'
    // (never the current value, so they are hidden at once) with
    // valid_until = scored_at + 14 d so the column is non-NULL for them; NULL
    // stays allowed here because SQLite cannot add NOT NULL post hoc, and the
    // fresh predicate reads NULL (and malformed text) as expired.
    //
    // users.refreshed_generation is deliberately left NULL: the refresh tick
    // treats "has scores AND refreshed_generation IS NOT the current one" as
    // due, which is what makes both this deploy AND every later generation
    // bump refresh users promptly, without a one-off time stamp (R07).
    //
    // scan_state.last_full_scan_finished_at is backfilled from every done
    // queue row — all pre-v18 rows were full scans — so the cooldown keeps
    // its anchor once a refresh reuses the row (R13).
    //
    // topic_fingerprint.embedding_model_id: all-MiniLM-L6-v2 is the only
    // embedding model Charcoal has ever run, so every stored vector is its.
    //
    // Index: (user_did, threat_score) serves the refresh-candidate query
    // (user + score floor, small residual filter) and ranked reads. See
    // Task 7 for the recorded plans.
    run_migration(conn, 18, |c| {
        c.execute_batch(
            "BEGIN;
             ALTER TABLE account_scores
                 ADD COLUMN scoring_generation TEXT NOT NULL DEFAULT 'legacy';
             ALTER TABLE account_scores ADD COLUMN valid_until TEXT;
             UPDATE account_scores
                 SET valid_until = datetime(scored_at, '+14 days')
                 WHERE valid_until IS NULL;
             CREATE INDEX IF NOT EXISTS idx_account_scores_user_score
                 ON account_scores (user_did, threat_score);
             ALTER TABLE scan_queue ADD COLUMN kind TEXT NOT NULL DEFAULT 'full';
             ALTER TABLE scan_queue ADD COLUMN full_requested_at TEXT;
             ALTER TABLE users ADD COLUMN next_refresh_at TEXT;
             ALTER TABLE users ADD COLUMN refreshed_generation TEXT;
             ALTER TABLE topic_fingerprint ADD COLUMN embedding_model_id TEXT;
             UPDATE topic_fingerprint
                 SET embedding_model_id = 'all-MiniLM-L6-v2'
                 WHERE embedding_vector IS NOT NULL AND embedding_model_id IS NULL;
             INSERT OR IGNORE INTO scan_state (user_did, key, value)
                 SELECT user_did, 'last_full_scan_finished_at', finished_at
                 FROM scan_queue
                 WHERE status = 'done' AND finished_at IS NOT NULL;
             COMMIT;",
        )
    })?;
```

Check `scan_state`'s SQLite primary key is `(user_did, key)` (schema.rs v4 block) so `INSERT OR IGNORE` is per user; if its columns are named differently, match them.

- [ ] **Step 4: Run the SQLite tests to verify they pass**

Run: `cargo test --lib db::schema::tests`
Expected: all pass.

- [ ] **Step 5: Postgres migration, registration, and tests**

Create `migrations/postgres/0018_score_expiry.sql`:

```sql
-- Migration v18 (#343 §4.4, #344): score expiry + refresh bookkeeping.
--
-- account_scores.scoring_generation / valid_until: fresh = generation is the
-- binary's SCORING_GENERATION AND valid_until > NOW(). Pre-existing rows are
-- stamped 'legacy' (hidden at once) with valid_until = scored_at + 14 d so
-- the column can be NOT NULL. The DEFAULT is dropped after the backfill on
-- purpose: a writer that forgets the stamp must fail, not write 'legacy'.
--
-- users.refreshed_generation stays NULL: the refresh tick treats a user
-- with scores whose refreshed_generation differs from the current one as
-- due, so this deploy AND every later generation bump refresh users
-- promptly (R07). No one-off next_refresh_at stamp.
--
-- scan_queue.kind: 'full' | 'refresh'. scan_queue.full_requested_at: a
-- full-scan request made while a refresh was running; the refresh's finish
-- turns the row into a queued full row dated by this column (R09).
--
-- scan_state.last_full_scan_finished_at is backfilled from every done queue
-- row (all pre-v18 rows are full scans) so the cooldown keeps its anchor
-- once a refresh reuses the row (R13).
--
-- topic_fingerprint.embedding_model_id: all-MiniLM-L6-v2 is the only model
-- that has ever produced a stored vector.

ALTER TABLE account_scores
    ADD COLUMN IF NOT EXISTS scoring_generation TEXT NOT NULL DEFAULT 'legacy';
ALTER TABLE account_scores ADD COLUMN IF NOT EXISTS valid_until TIMESTAMPTZ;
UPDATE account_scores SET valid_until = scored_at + INTERVAL '14 days' WHERE valid_until IS NULL;
ALTER TABLE account_scores ALTER COLUMN valid_until SET NOT NULL;
ALTER TABLE account_scores ALTER COLUMN scoring_generation DROP DEFAULT;

-- (user_did, threat_score): the refresh-candidate query is "this user, score
-- at or above the Elevated floor" with a small residual expiry/generation
-- filter, and ranked reads order by score. Plans recorded in the runbook.
CREATE INDEX IF NOT EXISTS idx_account_scores_user_score
    ON account_scores (user_did, threat_score);

ALTER TABLE scan_queue
    ADD COLUMN IF NOT EXISTS kind TEXT NOT NULL DEFAULT 'full'
    CHECK (kind IN ('full', 'refresh'));
ALTER TABLE scan_queue ADD COLUMN IF NOT EXISTS full_requested_at TIMESTAMPTZ;

ALTER TABLE users ADD COLUMN IF NOT EXISTS next_refresh_at TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS refreshed_generation TEXT;

ALTER TABLE topic_fingerprint ADD COLUMN IF NOT EXISTS embedding_model_id TEXT;
UPDATE topic_fingerprint
    SET embedding_model_id = 'all-MiniLM-L6-v2'
    WHERE embedding_vector IS NOT NULL AND embedding_model_id IS NULL;

INSERT INTO scan_state (user_did, key, value)
    SELECT user_did, 'last_full_scan_finished_at', to_char(finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"+00:00"')
    FROM scan_queue
    WHERE status = 'done' AND finished_at IS NOT NULL
ON CONFLICT (user_did, key) DO NOTHING;

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (18) ON CONFLICT DO NOTHING;
```

(`scan_state`'s Postgres primary key is `(user_did, key)` — confirm in `migrations/postgres/0004_multiuser.sql`; `embedding_vector`'s column name on Postgres may be `embedding` (pgvector, `0002_pgvector.sql`) — use the real name.) Register `(18, include_str!("../../migrations/postgres/0018_score_expiry.sql"))` after 17 in `src/db/postgres.rs`.

Add to `tests/db_postgres.rs` a guard so the suite **fails** in CI when the database is absent (Global Constraints, R10 spirit):

```rust
/// In CI the Postgres service is mandatory; a missing DATABASE_URL must fail
/// the job, not turn every test in this file into a silent early return.
fn database_url() -> Option<String> {
    let url = std::env::var("DATABASE_URL").ok().filter(|u| u.starts_with("postgres://"));
    if url.is_none() && std::env::var("CI").is_ok() {
        panic!("DATABASE_URL is required in CI for tests/db_postgres.rs");
    }
    url
}
```

(replace the existing `database_url` body; local runs without the variable still skip.) Then append the two v18 tests, mirroring the SQLite ones with the same fixture rows (`did:plc:v18scored` / `did:plc:v18unscored` / `did:plc:v18acct` / `did:plc:v18na`), asserting: `scoring_generation = 'legacy'`, `valid_until` = `scored_at + 14 d` (via `to_char(valid_until AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI:SS')`), `is_nullable = 'NO'` for `valid_until`, `kind = 'full'`, `next_refresh_at IS NULL AND refreshed_generation IS NULL`, `embedding_model_id` set only where a vector exists, the `scan_state` marker equal to the done row's `finished_at` rendered RFC3339, `idx_account_scores_user_score` present, and version 18 recorded exactly once. Use the `cache_test_lock()` guard and drop the columns/index/version row before reconnecting, as `test_pg_migration_v17_upgrades_from_v16` does; delete the fixture rows at the end.

- [ ] **Step 6: Run**

Run: `VERIFY_PG` (whole file — the upgrade test drops columns other tests use; the lock serialises them).
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add src/db/schema.rs migrations/postgres/0018_score_expiry.sql src/db/postgres.rs tests/db_postgres.rs
git commit -m 'feat(344): migration v18 — expiry, queue kind + full_requested_at, refresh schedule, embedding model id, cooldown backfill

account_scores.scoring_generation/valid_until (legacy backfill), the
(user_did, threat_score) index, scan_queue.kind/full_requested_at,
users.next_refresh_at/refreshed_generation (left NULL: due-ness is
generation-driven), topic_fingerprint.embedding_model_id, and
scan_state.last_full_scan_finished_at from done queue rows. Fresh +
upgrade-from-v17 tests on both backends; Postgres suite fails in CI
without DATABASE_URL.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 3: Stamp on write, null-safe fresh reads, lossless export/import, discovery gates

**Why:** The heart of #344, corrected for three review findings: the SQLite predicate must be boolean-explicit so malformed expiries count as expired everywhere (R11); `charcoal migrate` must copy every row verbatim rather than the fresh-only presentation set (R01); and topic-first discovery must use the fresh set like every other gate (R06).

**Files:**
- Modify: `src/db/traits.rs` (signatures + `StoredScore`, `export_scores`, `import_score`, `fingerprint_embedding_model`; `save_fingerprint_bundle` gains `embedding_model_id: Option<&str>`), `src/db/models.rs` (`StoredScore`), `src/db/queries.rs`, `src/db/sqlite.rs`, `src/db/postgres.rs`, `src/db/mod.rs`
- Modify: `src/pipeline/sweep.rs:132-140` and `:219-224`, `src/pipeline/amplification.rs:309-318`
- Modify: `src/main.rs` migrate command (`:1170-1176` scores; fingerprint bundle call; schedule init)
- Modify: existing tests that call the old signatures (`src/db/queries.rs:2704-2730`, `src/db/sqlite.rs:1055-1060`, `tests/db_postgres.rs:601-620,836-900`, every `save_fingerprint_bundle` caller)
- Test: `tests/unit_staleness.rs` (rewrite), `tests/unit_score_export.rs` (new), `tests/db_postgres.rs` (append)

**Interfaces:**
- Trait (`Database`):
  ```rust
  async fn is_score_stale(&self, user_did: &str, did: &str) -> Result<bool>;
  async fn get_fresh_scored_dids(&self, user_did: &str) -> Result<Vec<String>>;
  async fn count_expired(&self, user_did: &str) -> Result<i64>;
  /// Every row for the user, verbatim, for migration/export. Never filtered.
  async fn export_scores(&self, user_did: &str) -> Result<Vec<StoredScore>>;
  /// Write a row exactly as exported: scored_at, scoring_generation and
  /// valid_until are taken from `row`, never from the clock. Idempotent.
  async fn import_score(&self, user_did: &str, row: &StoredScore) -> Result<()>;
  async fn fingerprint_embedding_model(&self, user_did: &str) -> Result<Option<String>>;
  async fn save_fingerprint_bundle(&self, user_did: &str, fingerprint_json: &str, post_count: u32,
      embedding: Option<&[f64]>, embedding_model_id: Option<&str>, clusters: &[ClusterCentroid]) -> Result<()>;
  ```
- `StoredScore { score: AccountScore, scored_at: String /*RFC3339*/, scoring_generation: String, valid_until: Option<String> /*RFC3339; None only for a NULL SQLite row*/ }` in `models.rs`.
- `get_ranked_threats` and `count_not_assessed` become fresh-only (signatures unchanged). `upsert_account_score` stamps both columns from the clock (the scoring path); `import_score` never does.

- [ ] **Step 1: Rewrite `tests/unit_staleness.rs`**

```rust
// Score freshness (#213 Task 5, redefined by #344 / #343 §4.4).
//
// FRESH = scoring_generation == SCORING_GENERATION AND valid_until is a
// well-formed timestamp in the future. The predicate is boolean-explicit on
// SQLite (COALESCE(..., 0)) so a NULL or malformed valid_until is expired —
// hidden, stale, counted — rather than SQL-unknown (R11). These tests pin
// that the bulk set equals the complement of is_score_stale and that the
// write path stamps both columns from the confidence tier.

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

fn insert_raw(conn: &Connection, did: &str, valid_until_sql: &str, generation: &str) {
    conn.execute(
        &format!(
            "INSERT INTO account_scores
                 (user_did, did, handle, threat_score, threat_tier, scoring_generation, valid_until)
             VALUES (?1, ?2, ?3, 20.0, 'Elevated', ?4, {valid_until_sql})"
        ),
        params![USER, did, format!("{did}.handle"), generation],
    )
    .unwrap();
}

/// `valid_for_days` from now (negative = expired) under `generation`.
fn insert_score(conn: &Connection, did: &str, valid_for_days: i64, generation: &str) {
    insert_raw(conn, did, &format!("datetime('now', '{valid_for_days:+} days')"), generation);
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
fn fresh_set_is_exactly_the_non_stale_dids_including_null_and_malformed_expiry() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    insert_score(&conn, "did:plc:current", 5, SCORING_GENERATION);
    insert_score(&conn, "did:plc:expired", -1, SCORING_GENERATION);
    insert_score(&conn, "did:plc:legacy", 5, LEGACY_GENERATION);
    insert_score(&conn, "did:plc:oldgen", 5, "1999-01-01");
    insert_raw(&conn, "did:plc:nullvalid", "NULL", SCORING_GENERATION);
    insert_raw(&conn, "did:plc:malformed", "'not a timestamp'", SCORING_GENERATION);
    // Exact boundary: valid_until == now is NOT fresh (strict >).
    insert_raw(&conn, "did:plc:boundary", "datetime('now')", SCORING_GENERATION);

    let fresh = fresh_set(&conn);
    assert_eq!(fresh, HashSet::from(["did:plc:current".to_string()]));

    for did in [
        "did:plc:current", "did:plc:expired", "did:plc:legacy", "did:plc:oldgen",
        "did:plc:nullvalid", "did:plc:malformed", "did:plc:boundary", "did:plc:neverscored",
    ] {
        assert_eq!(fresh.contains(did), !is_score_stale(&conn, USER, did).unwrap(), "{did}");
    }
    // Every stored non-fresh row is COUNTED as expired — including NULL and
    // malformed, which a non-COALESCEd `NOT (...)` would have left unknown.
    assert_eq!(count_expired(&conn, USER).unwrap(), 6);
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
    assert_eq!(count_expired(&conn, USER).unwrap(), 0);
}

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
        assert!(!is_score_stale(&conn, USER, did).unwrap());
    }
}

#[test]
fn upsert_restamps_an_existing_legacy_row() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    insert_score(&conn, "did:plc:reborn", 5, LEGACY_GENERATION);
    assert!(is_score_stale(&conn, USER, "did:plc:reborn").unwrap());
    upsert_account_score(&conn, USER, &score_with_confidence("did:plc:reborn", Some("high"))).unwrap();
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
    for (did, days) in [("did:plc:na-expired", "-1"), ("did:plc:na-fresh", "+1")] {
        conn.execute(
            "INSERT INTO account_scores (user_did, did, handle, threat_tier, scoring_generation, valid_until)
             VALUES (?1, ?2, 'na.handle', 'NotAssessed', ?3, datetime('now', ?4 || ' days'))",
            params![USER, did, SCORING_GENERATION, days],
        )
        .unwrap();
    }
    let ranked = get_ranked_threats(&conn, USER, 0.0).unwrap();
    assert_eq!(ranked.iter().map(|a| a.did.as_str()).collect::<Vec<_>>(), ["did:plc:current"]);
    assert_eq!(count_not_assessed(&conn, USER).unwrap(), 1);
    assert_eq!(count_expired(&conn, USER).unwrap(), 3);
}

#[test]
fn staleness_days_are_three_seven_fourteen() {
    assert_eq!(ScoringConfidence::Low.staleness_days(), 3);
    assert_eq!(ScoringConfidence::Standard.staleness_days(), 7);
    assert_eq!(ScoringConfidence::High.staleness_days(), 14);
}
```

- [ ] **Step 2: Write the failing export/import tests**

Create `tests/unit_score_export.rs`:

```rust
// #344 R01: `charcoal migrate` must be lossless. The presentation queries hide
// expired/legacy rows; export/import carry every row with its original
// scored_at, generation and expiry, and importing never renews anything.

use charcoal::db::models::StoredScore;
use charcoal::db::schema::create_tables;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::scoring::generation::{LEGACY_GENERATION, SCORING_GENERATION};
use rusqlite::{params, Connection};
use std::sync::Arc;

const USER: &str = "did:plc:exportuser00000000000000";

fn seeded() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    // current, expired, legacy, and a NULL-score NotAssessed row.
    conn.execute_batch(&format!(
        "INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at, scoring_generation, valid_until) VALUES
           ('{USER}', 'did:plc:cur', 'cur.h', 40.0, 'High', '2026-09-10 00:00:00', '{SCORING_GENERATION}', '2026-09-24 00:00:00'),
           ('{USER}', 'did:plc:exp', 'exp.h', 20.0, 'Elevated', '2026-08-01 00:00:00', '{SCORING_GENERATION}', '2026-08-08 00:00:00'),
           ('{USER}', 'did:plc:leg', 'leg.h', 50.0, 'High', '2026-04-30 00:00:00', '{LEGACY_GENERATION}', '2026-05-14 00:00:00');
         INSERT INTO account_scores (user_did, did, handle, threat_tier, scored_at, scoring_generation, valid_until) VALUES
           ('{USER}', 'did:plc:na', 'na.h', 'NotAssessed', '2026-09-01 00:00:00', '{SCORING_GENERATION}', '2026-09-08 00:00:00');"
    ))
    .unwrap();
    conn
}

#[tokio::test]
async fn export_returns_every_row_with_its_provenance() {
    let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(seeded()));
    let rows = db.export_scores(USER).await.unwrap();
    assert_eq!(rows.len(), 4, "expired, legacy and NULL-score rows are exported");
    let leg = rows.iter().find(|r| r.score.did == "did:plc:leg").unwrap();
    assert_eq!(leg.scoring_generation, LEGACY_GENERATION);
    assert_eq!(leg.scored_at, "2026-04-30T00:00:00+00:00");
    assert_eq!(leg.valid_until.as_deref(), Some("2026-05-14T00:00:00+00:00"));
    let na = rows.iter().find(|r| r.score.did == "did:plc:na").unwrap();
    assert!(na.score.threat_score.is_none());
    assert_eq!(na.score.threat_tier.as_deref(), Some("NotAssessed"));
}

#[tokio::test]
async fn import_preserves_provenance_and_never_renews_expiry() {
    let src: Arc<dyn Database> = Arc::new(SqliteDatabase::new(seeded()));
    let dst_conn = Connection::open_in_memory().unwrap();
    create_tables(&dst_conn).unwrap();
    let dst: Arc<dyn Database> = Arc::new(SqliteDatabase::new(dst_conn));

    let rows = src.export_scores(USER).await.unwrap();
    for r in &rows {
        dst.import_score(USER, r).await.unwrap();
    }
    // Importing twice is a no-op, not a renewal.
    for r in &rows {
        dst.import_score(USER, r).await.unwrap();
    }

    let back = dst.export_scores(USER).await.unwrap();
    let mut a: Vec<StoredScore> = rows.clone();
    let mut b = back;
    a.sort_by(|x, y| x.score.did.cmp(&y.score.did));
    b.sort_by(|x, y| x.score.did.cmp(&y.score.did));
    assert_eq!(a, b, "round trip is byte-for-byte on scored_at, generation, valid_until and every score field");

    // The presentation layer agrees: only the current row is visible, the
    // other three are counted as expired — same as on the source.
    assert_eq!(dst.get_ranked_threats(USER, 0.0).await.unwrap().len(), 1);
    assert_eq!(dst.count_expired(USER).await.unwrap(), 3);
}

#[test]
fn v17_fixture_rows_survive_open_export_import() {
    // A database that stopped at v17 (no generation/expiry columns), opened
    // by this binary: v18 stamps 'legacy' + scored_at + 14 d, and export sees
    // exactly that — the migration must not lose or hide them from export.
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute_batch(
        "DROP INDEX idx_account_scores_user_score;
         ALTER TABLE account_scores DROP COLUMN scoring_generation;
         ALTER TABLE account_scores DROP COLUMN valid_until;
         DELETE FROM schema_version WHERE version = 18;",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at)
         VALUES (?1, 'did:plc:v17', 'v17.h', 40.0, 'High', '2026-09-01 12:00:00')",
        params![USER],
    )
    .unwrap();
    create_tables(&conn).unwrap(); // v18 applies
    let db = SqliteDatabase::new(conn);
    let rows = tokio::runtime::Runtime::new().unwrap().block_on(db.export_scores(USER)).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].scoring_generation, LEGACY_GENERATION);
    assert_eq!(rows[0].valid_until.as_deref(), Some("2026-09-15T12:00:00+00:00"));
}
```

`StoredScore` needs `#[derive(Debug, Clone, PartialEq)]`; `AccountScore` already derives `PartialEq` (check; add if missing — `ToxicPost` too). Timestamps are exported as RFC3339 on both backends (SQLite converts `YYYY-MM-DD HH:MM:SS` with `strftime('%Y-%m-%dT%H:%M:%S+00:00', col)`; Postgres with `to_char(... AT TIME ZONE 'UTC', ...)`), and imported back to each backend's native form (`datetime(?)` / `$n::timestamptz`).

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --test unit_staleness` and `cargo test --test unit_score_export`
Expected: compile errors (new methods/types missing) — expected at compile time for this step.

- [ ] **Step 4: Trait, models, SQLite**

`src/db/models.rs`:

```rust
/// One `account_scores` row with its provenance, for lossless export/import
/// (#344 R01). The presentation reads (`get_ranked_threats`, counts) hide
/// expired rows; this does not, and `import_score` writes it back verbatim.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredScore {
    pub score: AccountScore,
    /// RFC3339 UTC.
    pub scored_at: String,
    pub scoring_generation: String,
    /// RFC3339 UTC. `None` only for a SQLite row whose column is NULL.
    pub valid_until: Option<String>,
}
```

`src/db/traits.rs`: replace the two freshness signatures and add the new methods exactly as listed under Interfaces, with doc comments stating: freshness is generation + expiry; the read is a hard error for callers; `export_scores` is never filtered; `import_score` never touches the clock; `fingerprint_embedding_model` returns the stored model id (`None` = keyword-only fingerprint or pre-v18 row without a vector). Update `save_fingerprint_bundle` to take `embedding_model_id: Option<&str>` (callers pass `Some(EMBEDDING_MODEL_ID)` when `embedding.is_some()`, else `None` — `build_user_fingerprint` in `src/topics/` or `src/web/scan_job.rs`, the CLI `fingerprint` command in `main.rs`, the admin pre-seed, and `migrate`).

`src/db/queries.rs` — the predicate and its users:

```rust
/// The fresh predicate, SQLite spelling (#344 R11). Boolean-explicit:
/// `datetime(NULL)` and `datetime('garbage')` are both NULL, so without
/// COALESCE the comparison is SQL-unknown and `NOT (...)` stays unknown —
/// a hidden row that is never counted. COALESCE(…, 0) makes NULL and
/// malformed values read as "not fresh" in every consumer. Interpolated by
/// `format!` as a constant, never with user input; values still bind.
const FRESH_SQL: &str =
    "scoring_generation = {gen} AND COALESCE(datetime(valid_until) > datetime('now'), 0)";

fn fresh_sql(gen_param: &str) -> String {
    FRESH_SQL.replace("{gen}", gen_param)
}
```

then:

```rust
pub fn is_score_stale(conn: &Connection, user_did: &str, did: &str) -> Result<bool> {
    let fresh_rows: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM account_scores WHERE user_did = ?1 AND did = ?2 AND {}", fresh_sql("?3")),
        params![user_did, did, SCORING_GENERATION],
        |row| row.get(0),
    )?;
    Ok(fresh_rows == 0)
}

pub fn get_fresh_scored_dids(conn: &Connection, user_did: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT did FROM account_scores WHERE user_did = ?1 AND {}", fresh_sql("?2")
    ))?;
    let dids = stmt
        .query_map(params![user_did, SCORING_GENERATION], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(dids)
}

pub fn count_expired(conn: &Connection, user_did: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM account_scores WHERE user_did = ?1 AND NOT ({})", fresh_sql("?2")),
        params![user_did, SCORING_GENERATION],
        |row| row.get(0),
    )?;
    Ok(count)
}

pub fn count_not_assessed(conn: &Connection, user_did: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM account_scores WHERE user_did = ?1 AND threat_tier = 'NotAssessed' AND {}",
            fresh_sql("?2")
        ),
        params![user_did, SCORING_GENERATION],
        |row| row.get(0),
    )?;
    Ok(count)
}
```

`get_ranked_threats`: `WHERE user_did = ?1 AND threat_score >= ?2 AND {fresh_sql("?3")} ORDER BY threat_score DESC, did` with `params![user_did, min_score, SCORING_GENERATION]` (the `did` tie-breaker makes paging deterministic — #356 will rely on it).

`upsert_account_score`: as in the first draft — add `scoring_generation` (`?16` = `SCORING_GENERATION`) and `valid_until = datetime('now', ?17)` (`?17` = `format!("+{} days", ScoringConfidence::staleness_days_for_label(score.scoring_confidence.as_deref()))`) to both the INSERT and the `DO UPDATE SET` lists.

Export/import (SQLite):

```rust
/// Every row, verbatim, RFC3339 timestamps. See `Database::export_scores`.
pub fn export_scores(conn: &Connection, user_did: &str) -> Result<Vec<StoredScore>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                posts_analyzed, top_toxic_posts, behavioral_signals, graph_distance,
                fingerprint_quality, scoring_confidence, context_score, overlap_legacy,
                strftime('%Y-%m-%dT%H:%M:%S+00:00', scored_at),
                scoring_generation,
                strftime('%Y-%m-%dT%H:%M:%S+00:00', valid_until)
         FROM account_scores WHERE user_did = ?1 ORDER BY did",
    )?;
    let rows = stmt
        .query_map(params![user_did], |row| {
            let top_posts_json: String = row.get(7)?;
            let stored_tier: Option<String> = row.get(5)?;
            Ok(StoredScore {
                score: AccountScore {
                    did: row.get(0)?,
                    handle: row.get(1)?,
                    toxicity_score: row.get(2)?,
                    topic_overlap: row.get(3)?,
                    threat_score: row.get(4)?,
                    // Stored tier verbatim — export does not recompute (that is
                    // the presentation layer's job).
                    threat_tier: stored_tier,
                    posts_analyzed: row.get(6)?,
                    top_toxic_posts: serde_json::from_str(&top_posts_json).unwrap_or_default(),
                    scored_at: String::new(),
                    behavioral_signals: row.get(8)?,
                    graph_distance: row.get(9)?,
                    fingerprint_quality: row.get(10)?,
                    scoring_confidence: row.get(11)?,
                    context_score: row.get(12)?,
                    overlap_legacy: row.get(13)?,
                },
                scored_at: row.get(14)?,
                scoring_generation: row.get(15)?,
                valid_until: row.get(16)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Write an exported row back exactly. `datetime(?)` normalises the RFC3339
/// text to this backend's `YYYY-MM-DD HH:MM:SS` form. Idempotent: the
/// conflict branch rewrites the same values.
pub fn import_score(conn: &Connection, user_did: &str, row: &StoredScore) -> Result<()> {
    let s = &row.score;
    let top_posts_json = serde_json::to_string(&s.top_toxic_posts)?;
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
             posts_analyzed, top_toxic_posts, scored_at, behavioral_signals, context_score, graph_distance,
             fingerprint_quality, scoring_confidence, overlap_legacy, scoring_generation, valid_until)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime(?10), ?11, ?12, ?13, ?14, ?15, ?16, ?17, datetime(?18))
         ON CONFLICT(user_did, did) DO UPDATE SET
             handle = ?3, toxicity_score = ?4, topic_overlap = ?5, threat_score = ?6, threat_tier = ?7,
             posts_analyzed = ?8, top_toxic_posts = ?9, scored_at = datetime(?10), behavioral_signals = ?11,
             context_score = ?12, graph_distance = ?13, fingerprint_quality = ?14, scoring_confidence = ?15,
             overlap_legacy = ?16, scoring_generation = ?17, valid_until = datetime(?18)",
        params![
            user_did, s.did, s.handle, s.toxicity_score, s.topic_overlap, s.threat_score, s.threat_tier,
            s.posts_analyzed, top_posts_json, row.scored_at, s.behavioral_signals, s.context_score,
            s.graph_distance, s.fingerprint_quality, s.scoring_confidence, s.overlap_legacy,
            row.scoring_generation, row.valid_until,
        ],
    )?;
    Ok(())
}
```

(`top_toxic_posts` corruption-as-empty is a known defect tracked in #364; export inherits it and the CHANGELOG says so.) `fingerprint_embedding_model`: `SELECT embedding_model_id FROM topic_fingerprint WHERE user_did = ?1` (`.optional()?.flatten()`). `save_fingerprint_bundle` writes the new column. `src/db/sqlite.rs`: delegating impls for all new/changed methods.

- [ ] **Step 5: Postgres**

Same predicate, natively boolean: `scoring_generation = $n AND valid_until > NOW()` in `is_score_stale`, `get_fresh_scored_dids`, `count_expired` (`NOT (...)` — `valid_until` is NOT NULL on Postgres so no COALESCE is needed; say so in a comment), `count_not_assessed`, `get_ranked_threats` (+ `, did` tie-breaker). `upsert_account_score`: `$16` generation, `NOW() + make_interval(days => $17)`. `export_scores`: `to_char(scored_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"+00:00"')` and the same for `valid_until` (non-null → `Some`). `import_score`: `$10::timestamptz` / `$18::timestamptz`. `fingerprint_embedding_model` and the bundle column.

- [ ] **Step 6: Callers**

`sweep.rs:132-140` and `amplification.rs:309-318`: `db.get_fresh_scored_dids(user_did).await.context("reading fresh-score set …")?` with the comments from the first draft (hard error; a DB blip must not re-score the whole set). `sweep.rs:219-224` (`run_topic_first`, R06): replace `get_all_scored_dids` with `get_fresh_scored_dids` and reword the progress line to "{} accounts have fresh scores, searching for new discoveries…"; `get_all_scored_dids` stays on the trait for history/export uses (check `rg -n "get_all_scored_dids" src` — if `run_topic_first` was its only production caller, keep the method but note in its doc that it is not a scoring-eligibility gate).

`src/main.rs` migrate (`:1170-1176`): replace the `get_ranked_threats` + `upsert_account_score` loop with

```rust
            // 2. Migrate account scores — LOSSLESS (#344 R01). The presentation
            // query hides expired/legacy rows and the scoring upsert would
            // re-stamp what it copies as freshly scored; export/import carry
            // every row with its own scored_at, generation and expiry.
            let scores = sqlite_db.export_scores(&did).await?;
            for row in &scores {
                pg_db.import_score(&did, row).await?;
            }
            println!("  {} {} account scores migrated (all rows, provenance preserved)", "✓".green(), scores.len());
```

and after the fingerprint/scores/events/scan_state steps, initialise the destination schedule from the source's `users` columns:

```rust
            // 5. Refresh schedule: copy what the source knew, so a migrated
            // user is neither refreshed twice nor forgotten. A NULL
            // refreshed_generation on the destination makes the next tick
            // refresh them — the right default for a freshly migrated user.
            if let Some(at) = sqlite_db.next_refresh_at(&did).await? {
                pg_db.schedule_refresh(&did, &at).await?;
            }
            if let Some(g) = sqlite_db.refreshed_generation(&did).await? {
                pg_db.mark_refreshed_generation(&did, &g).await?;
            }
```

(`next_refresh_at`, `refreshed_generation`, `schedule_refresh`, `mark_refreshed_generation` are Task 6 trait methods; Task 6 lands before the migrate change is compiled — do this part of Task 3 as its final step after Task 6, or stub the four in Task 3 with the exact signatures Task 6 specifies.) The fingerprint step passes `Some(EMBEDDING_MODEL_ID)` when it migrates an embedding and copies `embedding_model_id` from the source via `fingerprint_embedding_model` if present.

Fix the existing tests to the new signatures (`rg -n "is_score_stale\(|get_fresh_scored_dids\(|save_fingerprint_bundle\(" src tests`).

- [ ] **Step 7: Postgres tests**

Append to `tests/db_postgres.rs`: the freshness twin (rows `pgfresh_current` +5 d / `pgfresh_expired` −1 d / `pgfresh_legacy` with `'legacy'`, plus the exact boundary `valid_until = NOW()` inserted in the same statement as the comparison is not testable deterministically — use `NOW() - INTERVAL '1 second'` and assert stale); `count_expired` = 3 of 4; the write-path stamp twin (`EXTRACT(EPOCH …)/86400.0)::float8`); and an export/import twin that seeds the four `unit_score_export` rows with explicit `scored_at`/`valid_until` timestamptz literals, exports, imports into the same database under a second user DID, exports that, and asserts equality after normalising the user; then repeats the import and asserts nothing changed.

- [ ] **Step 8: Run everything**

Run: `cargo test --test unit_staleness`, `cargo test --test unit_score_export`, `VERIFY_WEB`, `VERIFY_PG`, `VERIFY_CLIPPY`.
Expected: all green, zero `SKIP:`.

- [ ] **Step 9: Commit**

```bash
git add src/db/traits.rs src/db/models.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs src/db/mod.rs src/pipeline/sweep.rs src/pipeline/amplification.rs src/main.rs tests/unit_staleness.rs tests/unit_score_export.rs tests/db_postgres.rs
git commit -m 'feat(344): stamp scores; null-safe fresh-only reads; lossless export/import for migrate

upsert stamps scoring_generation + valid_until (3/7/14 d by confidence).
One boolean-explicit fresh predicate (COALESCE on SQLite so NULL and
malformed expiries are hidden AND counted) behind is_score_stale,
get_fresh_scored_dids, count_expired, count_not_assessed,
get_ranked_threats; topic-first discovery uses the fresh set too. The
pipeline propagates freshness-read errors. charcoal migrate now copies
every row verbatim through export_scores/import_score and never renews
expiry. Fingerprints record their embedding model id.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 4: "N expired" — status API, dashboard, CLI

**Why:** Spec §4.4: "the UI shows 'N expired' so a user can see why a list shrank." The day Phase 2 deploys every dashboard drops to zeros; this is the explanation.

**Files:**
- Modify: `src/web/handlers/status.rs:191-254`, `src/status.rs:51-60`, `web/src/lib/types.ts:32-42`, `web/src/lib/dashboard-state.ts:20-27`, `web/src/routes/(protected)/dashboard/+page.svelte:391-399`, `src/db/traits.rs` (`as_any`), `src/db/sqlite.rs` (`as_any`, `with_conn`), `src/db/postgres.rs` (`as_any`), `src/web/test_helpers.rs` (`expire_all_scores`)
- Test: `tests/web_oauth.rs` (append), `web/src/lib/dashboard-state.test.ts`

**Interfaces:**
- `GET /api/status` → `tier_counts.expired: number` (outside `total`, like `not_assessed`). `TierCounts.expired`.
- `Database::as_any(&self) -> &dyn std::any::Any`; `SqliteDatabase::with_conn<T>(&self, f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>) -> anyhow::Result<T>`; `charcoal::web::test_helpers::expire_all_scores(db: &Arc<dyn Database>, user_did: &str)` — test support only.

- [ ] **Step 1: Write the failing Rust test**

In `tests/web_oauth.rs`, next to `status_surfaces_not_assessed_count_in_tier_counts`:

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

`expire_all_scores` is test support. The `Database` trait exposes no raw SQL by design and `build_test_app_with_db` (`src/web/test_helpers.rs:71`) returns only an `Arc<dyn Database>`, so the helper downcasts to the SQLite backend every test app is built on. Add to `src/web/test_helpers.rs`:

```rust
/// Test-only: mark every one of `user_did`'s scores as scored under an older
/// generation — the shape migration v18 leaves pre-existing rows in (#344).
/// Lives here rather than on the `Database` trait because production has no
/// business rewriting generations.
pub async fn expire_all_scores(db: &Arc<dyn crate::db::Database>, user_did: &str) {
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

- [ ] **Step 2: Run the test to verify it fails**

Run: `set -o pipefail; CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_oauth status_reports_expired -- --show-output 2>&1 | tee target/test-expired.log; ! grep -qE '^\s*SKIP:' target/test-expired.log`
Expected: compile error (`expire_all_scores` / `as_any` missing) — expected at compile time. After Step 3's helpers exist but before the handler change, the assertion on `tier_counts.expired` fails (null).

- [ ] **Step 3: Implement**

`src/db/traits.rs`, in `pub trait Database`: `fn as_any(&self) -> &dyn std::any::Any;` implemented as `{ self }` on both backends. `src/db/sqlite.rs`, `impl SqliteDatabase`:

```rust
    /// Run a closure against the raw connection. Test support only: lets
    /// integration tests shape rows the trait deliberately cannot (age a
    /// score out of its generation, #344) without widening the trait.
    pub async fn with_conn<T>(
        &self,
        f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
    ) -> anyhow::Result<T> {
        let conn = self.conn.lock().await;
        Ok(f(&conn)?)
    }
```

`src/web/handlers/status.rs`, after the `not_assessed` block:

```rust
    // #344: rows hidden because they expired or predate the current scoring
    // generation. Reported so a shrunken list is explicable; NOT part of
    // `total`, which counts current results only. Same 500-on-error policy as
    // the two reads above — a silent 0 would hide the very thing this number
    // exists to show.
    let expired = match state.db.count_expired(&auth.effective_did).await {
        Ok(n) => n as u32,
        Err(e) => {
            tracing::error!(error = %e, "DB error counting expired scores in get_status");
            return api_error(StatusCode::INTERNAL_SERVER_ERROR, "Database error");
        }
    };
```

and `"expired": expired,` in the JSON body after `"not_assessed"`. `src/status.rs` after the "Scored accounts" line:

```rust
    let expired = db.count_expired(user_did).await?;
    if expired > 0 {
        println!(
            "  {expired} expired (scored under an older generation or past their window — \
             hidden until the refresh job or a re-engagement re-scores them)"
        );
    }
```

`web/src/lib/types.ts` `TierCounts`: `expired: number;` with the comment `// Rows hidden because they expired or predate the current scoring generation (#344). Not in total.` `web/src/lib/dashboard-state.ts:24`: add `|| status.tier_counts.expired > 0` to the `results` gate (comment: expired rows are results too — the grid explains why they are hidden). `dashboard-state.test.ts`: fixture gains `expired: 0`; add

```ts
	it('shows results (not welcome) when every score has expired', () => {
		expect(
			dashboardView(
				status({ tier_counts: { high: 0, elevated: 0, watch: 0, low: 0, not_assessed: 0, expired: 12, total: 0 } })
			)
		).toBe('results');
	});
```

`dashboard/+page.svelte`, after the not-assessed card:

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

Run the Svelte MCP `svelte-autofixer` on the edited component. `rg -n "not_assessed: 0" web/src` and add `expired: 0` to every fixture so `npm run check` passes.

- [ ] **Step 4: Run**

Run: `VERIFY_WEB` and `VERIFY_FE`.
Expected: green, zero `SKIP:`, `npm run check` clean for the files this task touches (the pre-existing five errors in `accounts/[handle]/+page.svelte` are #357/#364's; do not fix them here, and do not let them block: run `npm run check` and confirm the error count is unchanged at 5 — record the number in the task's outcome node).

- [ ] **Step 5: Commit**

```bash
git add src/db/traits.rs src/db/sqlite.rs src/db/postgres.rs src/web/test_helpers.rs src/web/handlers/status.rs src/status.rs tests/web_oauth.rs web/src/lib/types.ts web/src/lib/dashboard-state.ts web/src/lib/dashboard-state.test.ts "web/src/routes/(protected)/dashboard/+page.svelte"
git commit -m 'feat(344): report expired scores in tier_counts, dashboard card, CLI status

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 5: `scan_queue.kind` — refresh rows in the same queue, the user's request always honoured

**Why:** Spec §4.4's second queue kind, with the rules R09 and R13 require: a user's full-scan request is never dropped (a queued refresh is upgraded **in place**, a running refresh records the request and hands over when it finishes), the ETA median learns from full scans only, and the cooldown keeps its anchor.

**Rules:**
- `enqueue_scan(user)` (full): re-queues a `done`/`failed` row as `full` with `enqueued_at = now`; upgrades a `queued refresh` row to `full` **keeping `enqueued_at`** (the position the user already held); on a `running refresh` row sets `full_requested_at = COALESCE(full_requested_at, now)` and changes nothing else; no-op on `queued full` / `running full`. Returns `EnqueueOutcome { Queued, AlreadyQueued, AlreadyRunning, QueuedAfterRefresh }` so the handler can say which.
- `enqueue_refresh_scan(user)`: re-queues a `done`/`failed` row as `refresh`; never touches `queued`/`running` rows.
- `finish_queued_scan(user, claim, error)`: for a `refresh` row with `full_requested_at` set, the row becomes `queued`, `kind = 'full'`, `enqueued_at = full_requested_at`, `full_requested_at = NULL`, `started_at/finished_at/lease_expires/claim_id = NULL`, `last_error = error` (the refresh's own result is still recorded in `scan_state`, Task 9). Otherwise as today. Returns the existing `bool`.
- Admission order unchanged: `(enqueued_at, user_did)`.
- `scan_queue_entry`'s median: `kind = 'full'` rows only.
- Cooldown: `trigger_scan` anchors on the row's `finished_at` only for a `done full` row; otherwise on `scan_state.last_full_scan_finished_at` (backfilled by v18, written by `run_scan` on success).

**Files:**
- Modify: `src/db/traits.rs` (`ScanKind`, `EnqueueOutcome`, `ScanClaim.kind`, `ScanQueueRow.kind` + `full_requested_at`, `enqueue_scan -> Result<EnqueueOutcome>`, `enqueue_refresh_scan`), `src/db/mod.rs` (re-exports), `src/db/queries.rs:1466-1548,1567-1590,1634-1716`, `src/db/sqlite.rs`, `src/db/postgres.rs` (enqueue `:1644`, claim `:1667-1740`, finish `:1755-1777`, list `:1867-1910`, median `:1842-1855`)
- Modify: `src/web/handlers/scan.rs:63-120` (cooldown + 202 body), `src/web/handlers/admin.rs` (`scan_row_json`), `src/web/handlers/access.rs:245` and `admin.rs:301` (new return type), `src/web/scan_job.rs` (`run_scan` writes the marker), `web/src/lib/types.ts:168-177`
- Test: `tests/unit_scan_kind.rs` (new), `tests/db_postgres.rs` (append), `src/web/handlers/scan.rs` inline tests

**Interfaces:**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind { Full, Refresh }
impl ScanKind { pub fn as_str(&self) -> &'static str; pub fn from_str(s: &str) -> Option<Self>; }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome { Queued, AlreadyQueued, AlreadyRunning, QueuedAfterRefresh }
pub struct ScanClaim { pub user_did: String, pub claim_id: String, pub kind: ScanKind }
pub struct ScanQueueRow { …existing…, pub kind: ScanKind, pub full_requested_at: Option<String> }
async fn enqueue_scan(&self, user_did: &str) -> Result<EnqueueOutcome>;
async fn enqueue_refresh_scan(&self, user_did: &str) -> Result<()>;
```
`scan_state` key `last_full_scan_finished_at` (RFC3339), written by `run_scan` after `amplification::run` returns `Ok`.

- [ ] **Step 1: Write the failing SQLite tests**

Create `tests/unit_scan_kind.rs`:

```rust
// #343 Phase 2 / #344: a second queue kind, `refresh`, in the one-row-per-user
// scan_queue. The rules under test keep a nightly refresh from ever costing a
// human their scan: an upgrade keeps their place, a request during a running
// refresh is honoured when it finishes, a refresh never downgrades, and the
// ETA median ignores refresh durations.

use std::sync::Arc;

use charcoal::db::schema::create_tables;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::{Database, EnqueueOutcome, ScanKind};
use rusqlite::{params, Connection};

const USER: &str = "did:plc:kindtest0000000000000000";

fn db() -> Arc<SqliteDatabase> {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

async fn row(db: &SqliteDatabase, did: &str) -> charcoal::db::traits::ScanQueueRow {
    db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == did).expect("row exists")
}

#[tokio::test]
async fn refresh_enqueue_creates_a_refresh_row() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Refresh));
}

#[tokio::test]
async fn a_full_enqueue_upgrades_a_queued_refresh_in_place() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let before = row(&db, USER).await.enqueued_at;
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::Queued);
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
    assert_eq!(r.enqueued_at, before, "the user keeps the place the refresh held (R09)");
}

#[tokio::test]
async fn a_refresh_enqueue_never_downgrades_a_queued_full() {
    let db = db();
    db.enqueue_scan(USER).await.unwrap();
    db.enqueue_refresh_scan(USER).await.unwrap();
    assert_eq!(row(&db, USER).await.kind, ScanKind::Full);
}

#[tokio::test]
async fn a_full_request_during_a_running_refresh_is_recorded_and_runs_after_it() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().expect("claimed");
    assert_eq!(claim.kind, ScanKind::Refresh);

    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::QueuedAfterRefresh);
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("running", ScanKind::Refresh), "the refresh keeps running");
    let requested = r.full_requested_at.clone().expect("request recorded");
    // A second click does not move the request time.
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::QueuedAfterRefresh);
    assert_eq!(row(&db, USER).await.full_requested_at.as_deref(), Some(requested.as_str()));

    // The refresh finishes: the row becomes the user's queued full scan,
    // dated from the request, not from now.
    assert!(db.finish_queued_scan(USER, &claim.claim_id, None).await.unwrap());
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
    assert_eq!(r.enqueued_at, requested);
    assert!(r.full_requested_at.is_none());
    let claim = db.claim_next_scan(1, 60).await.unwrap().expect("the full scan is admitted");
    assert_eq!(claim.kind, ScanKind::Full);
}

#[tokio::test]
async fn a_failed_refresh_still_hands_over_to_the_requested_full_scan() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    db.enqueue_scan(USER).await.unwrap();
    assert!(db.finish_queued_scan(USER, &claim.claim_id, Some("refresh blew up")).await.unwrap());
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
    assert_eq!(r.last_error.as_deref(), Some("refresh blew up"), "the refresh's failure stays visible on the row");
}

#[tokio::test]
async fn enqueue_outcomes_name_the_state() {
    let db = db();
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::Queued);
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::AlreadyQueued);
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::AlreadyRunning);
    db.finish_queued_scan(USER, &claim.claim_id, None).await.unwrap();
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::Queued);
}

/// The ETA quoted to a queued user is a median of FULL scan durations.
#[tokio::test]
async fn eta_median_ignores_refresh_rows() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
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
    assert_eq!(entry.eta_seconds, Some(3600), "median over full scans only");
}

#[test]
fn scan_kind_round_trips() {
    for k in [ScanKind::Full, ScanKind::Refresh] {
        assert_eq!(ScanKind::from_str(k.as_str()), Some(k));
    }
    assert_eq!(ScanKind::from_str("nightly"), None);
}

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

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test unit_scan_kind`
Expected: compile error — `ScanKind`, `EnqueueOutcome`, `enqueue_refresh_scan`, `claim.kind` missing (expected at compile time).

- [ ] **Step 3: Types and trait**

`src/db/traits.rs`, above `ScanClaim`:

```rust
/// What a `scan_queue` row asks the admitter to run (#343 §4.4).
///
/// `Full` is the scan the user triggers. `Refresh` re-scores only this
/// user's High/Elevated rows that are about to expire or predate the current
/// scoring generation — candidates come from `account_scores`, never from
/// the network. Both run under the same claim/lease/fencing machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    Full,
    Refresh,
}

impl ScanKind {
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

/// What `enqueue_scan` did, so the handler can tell the user the truth
/// (#344 R09): a request made while a refresh is running is not dropped —
/// it is recorded on the row and runs when the refresh finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// A new queued full row, or a queued refresh upgraded in place.
    Queued,
    /// A full row was already queued; nothing changed.
    AlreadyQueued,
    /// A full scan is running; nothing changed.
    AlreadyRunning,
    /// A refresh is running; `full_requested_at` recorded (or already was).
    QueuedAfterRefresh,
}
```

Add `pub kind: ScanKind` to `ScanClaim` and `pub kind: ScanKind, pub full_requested_at: Option<String>` to `ScanQueueRow`. Trait:

```rust
    /// Queue a full scan. Re-queues `done`/`failed` rows; upgrades a queued
    /// `refresh` in place (keeps `enqueued_at`); records `full_requested_at`
    /// on a running `refresh` so it runs afterwards; no-op on queued/running
    /// `full`. Idempotent. See `EnqueueOutcome`.
    async fn enqueue_scan(&self, user_did: &str) -> Result<EnqueueOutcome>;

    /// Queue a refresh (#343 §4.4). Re-queues `done`/`failed` rows as
    /// `refresh`; never touches a queued or running row of either kind.
    async fn enqueue_refresh_scan(&self, user_did: &str) -> Result<()>;
```

Extend `finish_queued_scan`'s doc: "A `refresh` row with `full_requested_at` set does not go to `done`/`failed`: it becomes a queued `full` row dated from the request (the refresh's own outcome is recorded in `scan_state` by `run_refresh`)." `src/db/mod.rs`: re-export `ScanKind`, `EnqueueOutcome`.

- [ ] **Step 4: SQLite**

`src/db/queries.rs` — `enqueue_scan` becomes a small transaction so the outcome is exact:

```rust
pub fn enqueue_scan(conn: &Connection, user_did: &str) -> Result<EnqueueOutcome> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let now = chrono::Utc::now().to_rfc3339();
    let current: Option<(String, String)> = tx
        .query_row(
            "SELECT status, kind FROM scan_queue WHERE user_did = ?1",
            params![user_did],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let outcome = match current.as_ref().map(|(s, k)| (s.as_str(), k.as_str())) {
        None | Some(("done", _)) | Some(("failed", _)) => {
            tx.execute(
                "INSERT INTO scan_queue (user_did, status, kind, enqueued_at)
                 VALUES (?1, 'queued', 'full', ?2)
                 ON CONFLICT(user_did) DO UPDATE SET
                     status = 'queued', kind = 'full', enqueued_at = ?2,
                     started_at = NULL, finished_at = NULL, lease_expires = NULL,
                     last_error = NULL, claim_id = NULL, full_requested_at = NULL",
                params![user_did, now],
            )?;
            EnqueueOutcome::Queued
        }
        Some(("queued", "refresh")) => {
            // In place: the user keeps the position the refresh already held.
            tx.execute(
                "UPDATE scan_queue SET kind = 'full' WHERE user_did = ?1",
                params![user_did],
            )?;
            EnqueueOutcome::Queued
        }
        Some(("queued", _)) => EnqueueOutcome::AlreadyQueued,
        Some(("running", "refresh")) => {
            // Honoured when the refresh finishes (finish_queued_scan). The
            // first request's time wins so repeated clicks do not move it.
            tx.execute(
                "UPDATE scan_queue SET full_requested_at = COALESCE(full_requested_at, ?2)
                 WHERE user_did = ?1",
                params![user_did, now],
            )?;
            EnqueueOutcome::QueuedAfterRefresh
        }
        Some(("running", _)) => EnqueueOutcome::AlreadyRunning,
        Some((other, _)) => anyhow::bail!("scan_queue.status holds an unknown value {other:?}"),
    };
    tx.commit()?;
    Ok(outcome)
}

pub fn enqueue_refresh_scan(conn: &Connection, user_did: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO scan_queue (user_did, status, kind, enqueued_at)
         VALUES (?1, 'queued', 'refresh', ?2)
         ON CONFLICT(user_did) DO UPDATE SET
             status = 'queued', kind = 'refresh', enqueued_at = ?2,
             started_at = NULL, finished_at = NULL, lease_expires = NULL,
             last_error = NULL, claim_id = NULL, full_requested_at = NULL
         WHERE status IN ('done', 'failed')",
        params![user_did, now],
    )?;
    Ok(())
}
```

`finish_queued_scan`:

```rust
pub fn finish_queued_scan(conn: &Connection, user_did: &str, claim_id: &str, error: Option<&str>) -> Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let status = if error.is_none() { "done" } else { "failed" };
    // One statement, one WHERE (status='running' AND claim_id) — the fencing
    // rule is unchanged. A refresh with a pending full request hands the row
    // over instead of finishing (#344 R09).
    let changed = conn.execute(
        "UPDATE scan_queue
         SET status = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN 'queued' ELSE ?3 END,
             kind = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN 'full' ELSE kind END,
             enqueued_at = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN full_requested_at ELSE enqueued_at END,
             started_at = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE started_at END,
             finished_at = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE ?4 END,
             claim_id = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE claim_id END,
             full_requested_at = NULL,
             lease_expires = NULL,
             last_error = ?5
         WHERE user_did = ?1 AND status = 'running' AND claim_id = ?2",
        params![user_did, claim_id, status, now, error],
    )?;
    Ok(changed > 0)
}
```

(SQLite evaluates every `CASE` against the row's pre-update values, so the repeated condition is consistent within the statement.) `claim_next_scan`: select `user_did, kind`, carry `kind` into `ScanClaim` via `ScanKind::from_str(&kind).with_context(...)?`. `list_scan_queue`: select `kind, full_requested_at` and map them (an unknown kind is an error, not `Full`). Median (`:1712-1716`): `AND kind = 'full'`. `src/db/sqlite.rs`: delegate; `enqueue_scan` now returns `EnqueueOutcome`.

- [ ] **Step 5: Postgres**

Same semantics. `enqueue_scan`: `BEGIN; SELECT status, kind FROM scan_queue WHERE user_did = $1 FOR UPDATE;` then the matching statement per state; `COMMIT`. `enqueue_refresh_scan`: the INSERT … ON CONFLICT … WHERE `scan_queue.status IN ('done','failed')` with `kind = 'refresh'`. `finish_queued_scan`: the same `CASE` form with `$3::TEXT IS NULL` for the status and `NOW()` for `finished_at`. `claim_next_scan`: `SELECT user_did, kind … FOR UPDATE SKIP LOCKED`. `list_scan_queue`: `kind`, `full_requested_at` (`to_rfc3339()` on the `Option<DateTime<Utc>>`). Median: `AND kind = 'full'`.

- [ ] **Step 6: Handlers and the marker**

`src/web/handlers/scan.rs` — cooldown helper and use:

```rust
/// The instant the user's last FULL scan finished, for the cooldown.
///
/// `scan_queue` holds one row per user and since #344 that row may be a
/// refresh, so a `done full` row is the only row that anchors directly;
/// everything else defers to the `last_full_scan_finished_at` marker
/// (backfilled by migration v18, written by `run_scan` on success). A
/// refresh finishing therefore never starts or erases a cooldown (R13).
fn last_full_finished_at(row: Option<&crate::db::traits::ScanQueueRow>, marker: Option<String>) -> Option<String> {
    match row {
        Some(r) if r.status == "done" && r.kind == crate::db::ScanKind::Full => r.finished_at.clone().or(marker),
        _ => marker,
    }
}
```

Replace the cooldown block (`:63-95`) with: read the rows (warn + skip on error as today), read `state.db.get_scan_state(&auth.did, "last_full_scan_finished_at")` (warn + `None` on error), `let anchor = last_full_finished_at(row, marker);`, and the existing `cooldown_retry_at` / 429 logic on `anchor`. Replace the enqueue call (`:106`) with

```rust
    let outcome = match state.db.enqueue_scan(&auth.did).await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "enqueue failed");
            return api_error(StatusCode::SERVICE_UNAVAILABLE, "Could not queue the scan");
        }
    };
```

and add to the 202 body `"queued": match outcome { EnqueueOutcome::QueuedAfterRefresh => "after_refresh", EnqueueOutcome::AlreadyRunning => "already_running", _ => "now" }` with a comment that `after_refresh` means "a background refresh is running; your scan starts when it finishes" (the dashboard copy for it is #365's — leave the frontend reading `position`/`eta` as today). Inline tests in `scan.rs` for `last_full_finished_at`: done-full row wins; done-refresh row → marker; queued/running/failed/missing → marker; no marker → `None`. `handlers/access.rs:245` and `admin.rs:301`: bind the new return type (`let _ = …?` is enough where the outcome is not surfaced). `admin.rs` `scan_row_json`: `"kind": row.kind.as_str(), "full_requested_at": row.full_requested_at`. `web/src/lib/types.ts` admin row: `kind: 'full' | 'refresh'; full_requested_at: string | null;`.

`src/web/scan_job.rs` `run_scan`, right after `let result = crate::pipeline::amplification::run(...)`:

```rust
    // #344 R13: the queue row may later be reused by a refresh, so the
    // cooldown's anchor lives here, not on the row.
    if result.is_ok() {
        if let Err(e) = db
            .set_scan_state(user_did, "last_full_scan_finished_at", &chrono::Utc::now().to_rfc3339())
            .await
        {
            warn!(error = %format!("{e:#}"), "could not record last_full_scan_finished_at");
        }
    }
```

- [ ] **Step 7: Postgres twins**

Append to `tests/db_postgres.rs`: `test_pg_enqueue_outcomes_and_handover` (refresh enqueue → claim → full enqueue is `QueuedAfterRefresh` twice with an unchanged `full_requested_at` → finish → row is `queued full` dated from the request → claim returns `Full`), `test_pg_full_enqueue_upgrades_queued_refresh_keeping_position` (assert `enqueued_at` unchanged), `test_pg_refresh_never_downgrades`, and the median test (one done full 3600 s, one done refresh 60 s, `eta_seconds == Some(3600)`). Prefix DIDs `did:plc:pgkind_`, delete rows at the end; extend `cleanup_test_data` to `DELETE FROM scan_queue WHERE user_did LIKE 'did:plc:pg%'` if it does not already.

- [ ] **Step 8: Run**

Run: `cargo test --test unit_scan_kind`, `VERIFY_WEB`, `VERIFY_PG`, `VERIFY_FE` (type change), `VERIFY_CLIPPY`.
Expected: green. The admitter/slot-lifecycle inline tests still compile: they call `enqueue_scan(...).await.expect("enqueue")` and ignore the value.

- [ ] **Step 9: Commit**

```bash
git add src/db/traits.rs src/db/mod.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs src/web/handlers/scan.rs src/web/handlers/admin.rs src/web/handlers/access.rs src/web/scan_job.rs web/src/lib/types.ts tests/unit_scan_kind.rs tests/db_postgres.rs
git commit -m 'feat(344): scan_queue.kind — refresh rows share the queue; a user request is always honoured

ScanKind on claims/rows; enqueue_scan returns EnqueueOutcome and upgrades
a queued refresh IN PLACE (position kept) or records full_requested_at on
a running one, which finish_queued_scan hands over to a queued full row
dated from the request. Refresh enqueue never touches queued/running.
ETA median = full rows only; cooldown anchors on the backfilled
last_full_scan_finished_at marker.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 6: Durable, bounded, generation-aware refresh scheduling on the admitter tick

**Why:** Spec §4.4: the refresh tick runs on the admitter's `TICK`, no second loop, no second lock. R04: selecting due users and creating their queue rows must be one transaction, bounded per tick, retried on failure. R07: a generation bump must make users due through durable state the scheduler reads every tick, not a one-off migration stamp.

**Due rule:** a user is due when they have at least one score row, have **no queued or running `scan_queue` row of either kind**, and either `next_refresh_at <= now` or `refreshed_generation IS DISTINCT FROM <current>`. A user mid-scan is simply not selected: the completion path of whatever is running (`schedule_after_success` / `schedule_retry`) reschedules them, so the tick never has to reason about existing work. `refreshed_generation` is set to the current generation when a refresh completes (`Completed`/`NothingDue`, Task 9) or a full scan succeeds; `next_refresh_at` is set to `now + interval` on those same events and to `now + REFRESH_RETRY_HOURS` after `Deferred`/`Resumable`/failed outcomes. The tick advances `next_refresh_at` (to `now + interval`) **in the same transaction** that creates the queue row, so a crash cannot leave a rescheduled user with no job; it does **not** touch `refreshed_generation` (only a completed refresh proves the generation).

**Files:**
- Create: `src/web/refresh.rs`; Modify: `src/web/mod.rs`
- Modify: `src/db/traits.rs`, `src/db/queries.rs`, `src/db/sqlite.rs`, `src/db/postgres.rs` (five methods)
- Modify: `src/web/admitter.rs:441-497` (`run_admitter`), `:529-545`, test spawns `:889,:969,:1009`; `src/web/scan_job.rs` (`run_scan` success path)
- Test: `src/web/refresh.rs` inline; `src/web/admitter.rs` inline; `tests/db_postgres.rs`

**Interfaces:**
```rust
// trait Database
async fn next_refresh_at(&self, user_did: &str) -> Result<Option<String>>;
async fn refreshed_generation(&self, user_did: &str) -> Result<Option<String>>;
async fn schedule_refresh(&self, user_did: &str, at_rfc3339: &str) -> Result<()>;
async fn mark_refreshed_generation(&self, user_did: &str, generation: &str) -> Result<()>;
/// ONE transaction: select up to `limit` due users (see the due rule — users
/// with queued/running work are not due), create/reset each one's refresh
/// queue row and set next_refresh_at = `next`. Returns the DIDs claimed. A
/// failure rolls back everything, so the next tick sees the same users due.
async fn claim_and_enqueue_due_refreshes(&self, now_rfc3339: &str, next_rfc3339: &str,
    current_generation: &str, limit: usize) -> Result<Vec<String>>;

// crate::web::refresh
pub const REFRESH_INTERVAL_ENV: &str = "CHARCOAL_REFRESH_INTERVAL_HOURS";
pub const DEFAULT_REFRESH_INTERVAL_HOURS: u64 = 24;
pub const MAX_REFRESH_INTERVAL_HOURS: u64 = 168;
pub const REFRESH_RETRY_HOURS: u64 = 1;
pub const REFRESH_BATCH_PER_TICK: usize = 25;
pub fn parse_refresh_interval(raw: Option<&str>) -> Option<Duration>; // None = disabled
pub fn refresh_interval_from_env() -> Option<Duration>;
pub async fn enqueue_due_refreshes(db: &Arc<dyn Database>, now: DateTime<Utc>, interval: Duration) -> usize; // claimed count
pub async fn schedule_after_success(db: &dyn Database, user_did: &str, now: DateTime<Utc>); // next = now+interval (if enabled), refreshed_generation = current; best-effort, warns
pub async fn schedule_retry(db: &dyn Database, user_did: &str, now: DateTime<Utc>);          // next = now + REFRESH_RETRY_HOURS; best-effort, warns
```
`run_admitter(db, launcher, live, wake_rx, tick, cap, refresh: Option<Duration>)`.

- [ ] **Step 1: Write the failing tests**

Create `src/web/refresh.rs` with the module doc and tests:

```rust
//! The nightly refresh schedule (#343 §4.4, #344).
//!
//! Runs inside the admitter tick — no second loop, no second lock. Each
//! tick claims a bounded batch of due users and creates their refresh queue
//! rows in ONE transaction, so a crash between "decided" and "queued" cannot
//! happen (R04). Due-ness is `next_refresh_at <= now` OR "this user's scores
//! were last refreshed under another generation" (R07), so a generation
//! bump refreshes everyone promptly without any migration-time stamp.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::create_tables;
    use crate::db::sqlite::SqliteDatabase;
    use crate::db::{Database, ScanKind};
    use crate::scoring::generation::SCORING_GENERATION;
    use chrono::{Duration as ChronoDuration, Utc};
    use rusqlite::Connection;
    use std::sync::Arc;

    fn db() -> Arc<dyn Database> {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        Arc::new(SqliteDatabase::new(conn))
    }

    /// A user with one score row (so they are eligible), scheduled `at`, last
    /// refreshed under `generation`.
    async fn user(db: &Arc<dyn Database>, did: &str, at: Option<&str>, generation: Option<&str>) {
        db.upsert_user(did, &format!("{did}.handle")).await.unwrap();
        let mut score = crate::db::models::AccountScore::default_for_test(did);
        score.threat_score = Some(40.0);
        score.threat_tier = Some("High".into());
        db.upsert_account_score(did, &score).await.unwrap();
        if let Some(at) = at {
            db.schedule_refresh(did, at).await.unwrap();
        }
        if let Some(g) = generation {
            db.mark_refreshed_generation(did, g).await.unwrap();
        }
    }

    async fn queue_kind(db: &Arc<dyn Database>, did: &str) -> Option<(String, ScanKind)> {
        db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == did).map(|r| (r.status, r.kind))
    }

    #[test]
    fn interval_knob_defaults_disables_and_clamps() {
        let h = |n: u64| Duration::from_secs(n * 3600);
        assert_eq!(parse_refresh_interval(None), Some(h(24)));
        assert_eq!(parse_refresh_interval(Some("6")), Some(h(6)));
        assert_eq!(parse_refresh_interval(Some("0")), None);
        assert_eq!(parse_refresh_interval(Some("off")), None);
        assert_eq!(parse_refresh_interval(Some("500")), Some(h(168)));
        assert_eq!(parse_refresh_interval(Some("abc")), Some(h(24)));
        assert_eq!(parse_refresh_interval(Some(" 12 ")), Some(h(12)));
    }

    #[tokio::test]
    async fn due_by_time_or_by_generation_and_never_without_scores() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        let future = (now + ChronoDuration::hours(1)).to_rfc3339();
        user(&db, "did:plc:due-time", Some(&past), Some(SCORING_GENERATION)).await;
        user(&db, "did:plc:due-gen", Some(&future), Some("1999-01-01")).await;
        user(&db, "did:plc:due-never-refreshed", None, None).await; // v18-migrated shape
        user(&db, "did:plc:not-due", Some(&future), Some(SCORING_GENERATION)).await;
        db.upsert_user("did:plc:no-scores", "none.handle").await.unwrap(); // no rows ⇒ never due

        let n = enqueue_due_refreshes(&db, now, Duration::from_secs(24 * 3600)).await;
        assert_eq!(n, 3);
        for did in ["did:plc:due-time", "did:plc:due-gen", "did:plc:due-never-refreshed"] {
            assert_eq!(queue_kind(&db, did).await, Some(("queued".into(), ScanKind::Refresh)), "{did}");
        }
        assert_eq!(queue_kind(&db, "did:plc:not-due").await, None);
        assert_eq!(queue_kind(&db, "did:plc:no-scores").await, None);

        // A tick a minute later claims nobody: the time-due user was
        // rescheduled, and the generation-due users are still generation-due
        // (only a COMPLETED refresh proves the generation) but they now have
        // a queued row, which excludes them from selection until it runs.
        let n = enqueue_due_refreshes(&db, now + ChronoDuration::minutes(1), Duration::from_secs(24 * 3600)).await;
        assert_eq!(n, 0);
    }

    /// A user with queued or running work is not selected: their schedule is
    /// left alone (the completion path reschedules them) and their row is not
    /// touched — a queued full scan is never downgraded, a running scan never
    /// interrupted.
    #[tokio::test]
    async fn a_user_with_queued_or_running_work_is_not_claimed() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        user(&db, "did:plc:busy", Some(&past), Some(SCORING_GENERATION)).await;
        db.enqueue_scan("did:plc:busy").await.unwrap();
        user(&db, "did:plc:free", Some(&past), Some(SCORING_GENERATION)).await;

        let claimed = db
            .claim_and_enqueue_due_refreshes(
                &now.to_rfc3339(),
                &(now + ChronoDuration::hours(24)).to_rfc3339(),
                SCORING_GENERATION,
                25,
            )
            .await
            .unwrap();
        assert_eq!(claimed, vec!["did:plc:free".to_string()]);
        assert_eq!(queue_kind(&db, "did:plc:busy").await, Some(("queued".into(), ScanKind::Full)), "not downgraded");
        assert_eq!(db.next_refresh_at("did:plc:busy").await.unwrap().as_deref(), Some(past.as_str()), "schedule untouched");
    }

    #[tokio::test]
    async fn a_tick_is_bounded_and_the_rest_wait_for_the_next_one() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        for i in 0..30 {
            user(&db, &format!("did:plc:bulk{i:02}"), Some(&past), Some(SCORING_GENERATION)).await;
        }
        let first = enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await;
        let second = enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await;
        let third = enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await;
        assert_eq!((first, second, third), (REFRESH_BATCH_PER_TICK, 30 - REFRESH_BATCH_PER_TICK, 0));
    }

    /// The claim is one transaction: when creating the queue row fails, the
    /// schedule is NOT advanced, so the user is still due next tick (R04).
    #[tokio::test]
    async fn a_failed_enqueue_rolls_back_the_schedule_advance() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        // A trigger that refuses refresh rows simulates the write failing
        // mid-transaction.
        conn.execute_batch(
            "CREATE TRIGGER refuse_refresh BEFORE INSERT ON scan_queue
             WHEN NEW.kind = 'refresh' BEGIN SELECT RAISE(ABORT, 'disk on fire'); END;",
        )
        .unwrap();
        let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        user(&db, "did:plc:unlucky", Some(&past), Some(SCORING_GENERATION)).await;

        let n = enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await;
        assert_eq!(n, 0, "nothing claimed when the transaction fails");
        assert_eq!(db.next_refresh_at("did:plc:unlucky").await.unwrap().as_deref(), Some(past.as_str()), "still due");
        assert_eq!(queue_kind(&db, "did:plc:unlucky").await, None);
    }

    #[tokio::test]
    async fn success_and_retry_scheduling() {
        let db = db();
        let now = Utc::now();
        user(&db, "did:plc:u", None, None).await;
        std::env::remove_var(REFRESH_INTERVAL_ENV);
        schedule_after_success(db.as_ref(), "did:plc:u", now).await;
        let next = db.next_refresh_at("did:plc:u").await.unwrap().unwrap();
        assert_eq!(next, (now + ChronoDuration::hours(24)).to_rfc3339());
        assert_eq!(db.refreshed_generation("did:plc:u").await.unwrap().as_deref(), Some(SCORING_GENERATION));

        schedule_retry(db.as_ref(), "did:plc:u", now).await;
        let next = db.next_refresh_at("did:plc:u").await.unwrap().unwrap();
        assert_eq!(next, (now + ChronoDuration::hours(REFRESH_RETRY_HOURS as i64)).to_rfc3339());
        assert_eq!(db.refreshed_generation("did:plc:u").await.unwrap().as_deref(), Some(SCORING_GENERATION), "retry does not unset the generation");
    }
}
```

`AccountScore::default_for_test(did)` does not exist; add a `#[cfg(test)]`-gated constructor to `src/db/models.rs` that fills the struct with `did`, `"{did}.handle"`, and `None`/`0`/`vec![]` elsewhere, or inline the struct literal (the file already has one in `unit_staleness`).

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web --lib web::refresh`
Expected: compile errors — module empty, trait methods missing (expected at compile time).

- [ ] **Step 3: Trait + backends**

Trait methods as in Interfaces. SQLite (`queries.rs`):

```rust
pub fn next_refresh_at(conn: &Connection, user_did: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT next_refresh_at FROM users WHERE did = ?1", params![user_did], |r| r.get(0))
        .optional()?
        .flatten())
}

pub fn refreshed_generation(conn: &Connection, user_did: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT refreshed_generation FROM users WHERE did = ?1", params![user_did], |r| r.get(0))
        .optional()?
        .flatten())
}

pub fn schedule_refresh(conn: &Connection, user_did: &str, at_rfc3339: &str) -> Result<()> {
    conn.execute("UPDATE users SET next_refresh_at = ?2 WHERE did = ?1", params![user_did, at_rfc3339])?;
    Ok(())
}

pub fn mark_refreshed_generation(conn: &Connection, user_did: &str, generation: &str) -> Result<()> {
    conn.execute("UPDATE users SET refreshed_generation = ?2 WHERE did = ?1", params![user_did, generation])?;
    Ok(())
}

/// See `Database::claim_and_enqueue_due_refreshes`. Immediate transaction:
/// the write lock is taken before the select so two ticks in one process
/// (or the CLI and the web) serialise here.
pub fn claim_and_enqueue_due_refreshes(
    conn: &Connection,
    now_rfc3339: &str,
    next_rfc3339: &str,
    current_generation: &str,
    limit: usize,
) -> Result<Vec<String>> {
    let tx = rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate)?;
    let due: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT u.did FROM users u
             WHERE EXISTS (SELECT 1 FROM account_scores s WHERE s.user_did = u.did)
               AND NOT EXISTS (SELECT 1 FROM scan_queue q
                               WHERE q.user_did = u.did AND q.status IN ('queued', 'running'))
               AND ((u.next_refresh_at IS NOT NULL AND u.next_refresh_at <= ?1)
                    OR u.refreshed_generation IS NULL
                    OR u.refreshed_generation != ?2)
             ORDER BY u.next_refresh_at, u.did
             LIMIT ?3",
        )?;
        stmt.query_map(params![now_rfc3339, current_generation, limit as i64], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for did in &due {
        // The row is done/failed/absent by construction (see the NOT EXISTS
        // above), so this always creates or resets it.
        tx.execute(
            "INSERT INTO scan_queue (user_did, status, kind, enqueued_at)
             VALUES (?1, 'queued', 'refresh', ?2)
             ON CONFLICT(user_did) DO UPDATE SET
                 status = 'queued', kind = 'refresh', enqueued_at = ?2,
                 started_at = NULL, finished_at = NULL, lease_expires = NULL,
                 last_error = NULL, claim_id = NULL, full_requested_at = NULL",
            params![did, now_rfc3339],
        )?;
        tx.execute("UPDATE users SET next_refresh_at = ?2 WHERE did = ?1", params![did, next_rfc3339])?;
    }
    tx.commit()?;
    Ok(due)
}
```

Postgres: the same in one `BEGIN … COMMIT` over `self.pool.begin()`: `SELECT u.did FROM users u WHERE EXISTS (…scores…) AND NOT EXISTS (SELECT 1 FROM scan_queue q WHERE q.user_did = u.did AND q.status IN ('queued','running')) AND ((next_refresh_at IS NOT NULL AND next_refresh_at <= $1::timestamptz) OR refreshed_generation IS DISTINCT FROM $2) ORDER BY next_refresh_at NULLS FIRST, did LIMIT $3 FOR UPDATE OF u SKIP LOCKED` (two replicas ticking together partition the due set instead of colliding), then per user the `INSERT … ON CONFLICT (user_did) DO UPDATE SET …` (no `WHERE` — the row is finished or absent by construction) and `UPDATE users SET next_refresh_at = $2::timestamptz WHERE did = $1`; commit. `next_refresh_at`/`refreshed_generation` getters render `to_rfc3339()`.

- [ ] **Step 4: `src/web/refresh.rs` implementation (above the tests)**

```rust
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::{error, info, warn};

use crate::db::Database;
use crate::scoring::generation::SCORING_GENERATION;

pub const REFRESH_INTERVAL_ENV: &str = "CHARCOAL_REFRESH_INTERVAL_HOURS";
pub const DEFAULT_REFRESH_INTERVAL_HOURS: u64 = 24;
pub const MAX_REFRESH_INTERVAL_HOURS: u64 = 168;
/// After a deferred, resumable or failed refresh: try again soon, not at the
/// next nightly. Separate from the cadence so a stuck user shows up in the
/// logs hourly rather than daily.
pub const REFRESH_RETRY_HOURS: u64 = 1;
/// Users claimed per tick. Bounded so a large due population (a generation
/// bump makes everyone due at once) cannot hold the admitter pass — lease
/// reclaim and admission run on the same loop — for more than one short
/// transaction; the rest are claimed on following ticks, 30 s apart.
pub const REFRESH_BATCH_PER_TICK: usize = 25;

/// `None` disables the tick (`0`/`off`); unparseable → default so a typo in
/// Railway does not silently switch refreshes off.
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

fn plus(now: DateTime<Utc>, d: Duration) -> String {
    (now + chrono::Duration::from_std(d).expect("bounded")).to_rfc3339()
}

/// One tick: claim up to REFRESH_BATCH_PER_TICK due users and queue them in
/// one transaction. Returns how many were claimed (schedule advanced), which
/// includes users whose row was left alone because they already had work.
/// Errors are logged, never propagated — this runs on the admitter loop.
pub async fn enqueue_due_refreshes(db: &Arc<dyn Database>, now: DateTime<Utc>, interval: Duration) -> usize {
    match db
        .claim_and_enqueue_due_refreshes(&now.to_rfc3339(), &plus(now, interval), SCORING_GENERATION, REFRESH_BATCH_PER_TICK)
        .await
    {
        Ok(claimed) => {
            if !claimed.is_empty() {
                info!(claimed = claimed.len(), "refresh tick enqueued due users");
            }
            claimed.len()
        }
        Err(e) => {
            // Nothing advanced (single transaction); the same users are due
            // on the next tick. Loud: a persistent failure here is a wedge.
            error!(error = %format!("{e:#}"), "refresh tick failed — nothing scheduled, retrying next tick");
            crate::observability::refresh_metrics::record_tick_failure();
            0
        }
    }
}

/// After a completed refresh or a successful full scan: next nightly, and
/// this generation is proven for the user. Best-effort — the scan already
/// succeeded; a scheduling failure is logged and the user is caught by the
/// generation rule (refreshed_generation still differs) on a later tick.
pub async fn schedule_after_success(db: &dyn Database, user_did: &str, now: DateTime<Utc>) {
    if let Some(interval) = refresh_interval_from_env() {
        if let Err(e) = db.schedule_refresh(user_did, &plus(now, interval)).await {
            warn!(error = %format!("{e:#}"), "could not schedule the next refresh");
        }
    }
    if let Err(e) = db.mark_refreshed_generation(user_did, SCORING_GENERATION).await {
        warn!(error = %format!("{e:#}"), "could not record refreshed_generation");
    }
}

/// After a deferred, resumable or failed refresh: retry soon. Does not touch
/// refreshed_generation — only a completed refresh proves the generation.
pub async fn schedule_retry(db: &dyn Database, user_did: &str, now: DateTime<Utc>) {
    if let Err(e) = db
        .schedule_refresh(user_did, &plus(now, Duration::from_secs(REFRESH_RETRY_HOURS * 3600)))
        .await
    {
        warn!(error = %format!("{e:#}"), "could not schedule the refresh retry");
    }
}
```

`crate::observability::refresh_metrics` (new, `src/observability/refresh_metrics.rs`): a `record_tick_failure()` counter in the style of `classifier_metrics` (tracing event + atomic), so a failing tick is observable beyond the error line.

- [ ] **Step 5: Wire the tick and the full-scan success path**

`run_admitter` gains `refresh: Option<Duration>`; at the top of the loop body:

```rust
        // #343 §4.4: the refresh schedule rides this tick — one bounded
        // transaction, before admit so a user claimed now starts now when a
        // slot is free. None = CHARCOAL_REFRESH_INTERVAL_HOURS disabled.
        if let Some(interval) = refresh {
            crate::web::refresh::enqueue_due_refreshes(&db, chrono::Utc::now(), interval).await;
        }
```

`spawn_admitter` passes `crate::web::refresh::refresh_interval_from_env()`; the three test spawns pass `None`. Add the admitter test `the_tick_enqueues_and_admits_a_due_refresh` (user with one score row, `refreshed_generation` NULL, tick 20 ms, `RecordingLauncher::notifying`; assert the launched DID and, via a new `kinds()` accessor on `RecordingLauncher`, `ScanKind::Refresh`). `src/web/scan_job.rs` `run_scan`, in the `if result.is_ok()` block from Task 5: `crate::web::refresh::schedule_after_success(db.as_ref(), user_did, chrono::Utc::now()).await;`.

- [ ] **Step 6: Postgres twins**

Append to `tests/db_postgres.rs`: `test_pg_claim_and_enqueue_is_one_transaction_and_bounded` (seed 3 due users + 1 not due + 1 without scores + 1 due-by-generation + 1 due-by-time with a queued full row; claim with limit 2 → 2 DIDs, rows queued refresh, `next_refresh_at` advanced for exactly those 2; claim again → the remaining 2 (never the busy one); claim again → 0) and `test_pg_two_schedulers_partition_the_due_set` (two connections, `BEGIN` both, run the claim in each with limit 25 over 10 due users concurrently via `tokio::join!` — assert the union is the 10 users and the intersection is empty; `SKIP LOCKED` is what makes this hold).

- [ ] **Step 7: Run**

Run: `cargo test --features web --lib web::refresh`, `cargo test --features web --lib web::admitter`, `VERIFY_WEB`, `VERIFY_PG`, `VERIFY_CLIPPY`.
Expected: green.

- [ ] **Step 8: Commit**

```bash
git add src/web/refresh.rs src/web/mod.rs src/observability/refresh_metrics.rs src/observability/mod.rs src/db/traits.rs src/db/models.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs src/web/admitter.rs src/web/scan_job.rs tests/db_postgres.rs
git commit -m 'feat(344): durable, bounded, generation-aware refresh scheduling on the admitter tick

claim_and_enqueue_due_refreshes: one transaction per tick (SKIP LOCKED
on Postgres, immediate tx on SQLite) that selects up to 25 due users
with no queued/running work, creates their refresh rows and advances
next_refresh_at together — a failure advances nothing. Due = next_refresh_at passed OR
refreshed_generation is not current, so a generation bump refreshes
everyone without a migration stamp. Completed refreshes and successful
full scans schedule the next nightly and prove the generation; deferred
or failed ones retry in an hour.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 7: `list_refresh_candidates` — candidates from the table, malformed expiry included, index measured

**Why:** #344's second gap: the staleness gate must become a *source* of candidates. Spec §4.4: High/Elevated rows with `valid_until < now + 2 d`, plus old-generation rows. R11: a malformed SQLite expiry must be eligible, not silently skipped. R12: the index has to be justified by the plan the query actually produces.

**Tier test:** `threat_score >= ThreatTier::ELEVATED_MIN` (15.0), not the stored tier string — `get_ranked_threats` recomputes the tier from the score, so this agrees with what the UI shows.

**Files:**
- Modify: `src/db/models.rs` (`ThreatTier::ELEVATED_MIN`, used by `from_score`), `src/db/traits.rs` (`RefreshCandidate`, method), `src/db/mod.rs`, `src/db/queries.rs`, `src/db/sqlite.rs`, `src/db/postgres.rs`
- Test: `tests/unit_refresh_candidates.rs` (new), `tests/db_postgres.rs` (append)
- Evidence: `docs/runbooks/343-phase2-expiry-refresh.md` §"Index plans" (Task 10 creates the file; this task produces the numbers)

**Interfaces:**
```rust
impl ThreatTier { pub const ELEVATED_MIN: f64 = 15.0; }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshCandidate { pub did: String, pub handle: String, pub graph_distance: Option<String> }
async fn list_refresh_candidates(&self, user_did: &str, horizon_days: i64) -> Result<Vec<RefreshCandidate>>;
```
Ordered `threat_score DESC, did`.

- [ ] **Step 1: Write the failing tests**

Create `tests/unit_refresh_candidates.rs`:

```rust
// #344: the refresh job's candidate source. Rows come FROM account_scores —
// High/Elevated by score, and either expiring within the horizon, already
// expired (including NULL/malformed expiry, R11), or stamped with an older
// generation. Most dangerous first.

use charcoal::db::queries::list_refresh_candidates;
use charcoal::db::schema::create_tables;
use charcoal::scoring::generation::{LEGACY_GENERATION, SCORING_GENERATION};
use rusqlite::{params, Connection};

const USER: &str = "did:plc:refreshuser0000000000000";

fn insert(conn: &Connection, user: &str, did: &str, score: f64, valid_until_sql: &str, generation: &str, graph: Option<&str>) {
    conn.execute(
        &format!(
            "INSERT INTO account_scores
                 (user_did, did, handle, threat_score, threat_tier, scoring_generation, valid_until, graph_distance)
             VALUES (?1, ?2, ?3, ?4, 'x', ?5, {valid_until_sql}, ?6)"
        ),
        params![user, did, format!("{did}.handle"), score, generation, graph],
    )
    .unwrap();
}

fn days(n: i64) -> String {
    format!("datetime('now', '{n:+} days')")
}

#[test]
fn selects_high_and_elevated_rows_that_are_expiring_expired_malformed_or_old_generation() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    // Included:
    insert(&conn, USER, "did:plc:high-expiring", 60.0, &days(1), SCORING_GENERATION, Some("Stranger"));
    insert(&conn, USER, "did:plc:high-expired", 40.0, &days(-3), SCORING_GENERATION, Some("Follows you"));
    insert(&conn, USER, "did:plc:high-null", 45.0, "NULL", SCORING_GENERATION, None);
    insert(&conn, USER, "did:plc:high-malformed", 42.0, "'yesterday-ish'", SCORING_GENERATION, None);
    insert(&conn, USER, "did:plc:elevated-legacy", 20.0, &days(10), LEGACY_GENERATION, None);
    insert(&conn, USER, "did:plc:elevated-floor", 15.0, &days(1), SCORING_GENERATION, None);
    // Excluded:
    insert(&conn, USER, "did:plc:high-fresh", 50.0, &days(10), SCORING_GENERATION, None);
    insert(&conn, USER, "did:plc:watch-legacy", 14.99, &days(1), LEGACY_GENERATION, None);
    insert(&conn, "did:plc:otheruser", "did:plc:high-other", 60.0, &days(1), SCORING_GENERATION, None);
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, threat_tier, scoring_generation, valid_until)
         VALUES (?1, 'did:plc:na', 'na.handle', 'NotAssessed', ?2, datetime('now', '-1 days'))",
        params![USER, LEGACY_GENERATION],
    )
    .unwrap();

    let rows = list_refresh_candidates(&conn, USER, 2).unwrap();
    let dids: Vec<&str> = rows.iter().map(|r| r.did.as_str()).collect();
    assert_eq!(
        dids,
        [
            "did:plc:high-expiring",   // 60
            "did:plc:high-null",       // 45
            "did:plc:high-malformed",  // 42
            "did:plc:high-expired",    // 40
            "did:plc:elevated-legacy", // 20
            "did:plc:elevated-floor",  // 15
        ],
        "most dangerous first; NULL and malformed expiry are eligible"
    );
    assert_eq!(rows[0].graph_distance.as_deref(), Some("Stranger"));
    assert_eq!(rows[3].graph_distance.as_deref(), Some("Follows you"));
}

#[test]
fn horizon_zero_means_only_already_expired_or_old_generation() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    insert(&conn, USER, "did:plc:soon", 60.0, &days(1), SCORING_GENERATION, None);
    insert(&conn, USER, "did:plc:gone", 60.0, &days(-1), SCORING_GENERATION, None);
    insert(&conn, USER, "did:plc:boundary", 60.0, "datetime('now')", SCORING_GENERATION, None);
    let dids: Vec<String> = list_refresh_candidates(&conn, USER, 0).unwrap().into_iter().map(|r| r.did).collect();
    assert_eq!(dids, ["did:plc:gone", "did:plc:boundary"], "valid_until == now is expired (fresh is strict >)");
}

#[test]
fn elevated_floor_matches_from_score() {
    use charcoal::db::models::ThreatTier;
    assert_eq!(ThreatTier::from_score(ThreatTier::ELEVATED_MIN), ThreatTier::Elevated);
    assert_eq!(ThreatTier::from_score(ThreatTier::ELEVATED_MIN - 0.01), ThreatTier::Watch);
}

/// R12: the query must use the (user_did, threat_score) index, not scan the
/// table. Asserted on the plan text so a future edit that drops the index
/// or rewrites the predicate into something unindexable fails here.
#[test]
fn candidate_query_uses_the_user_score_index() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    let plan: Vec<String> = conn
        .prepare(&format!(
            "EXPLAIN QUERY PLAN {}",
            charcoal::db::queries::REFRESH_CANDIDATES_SQL
        ))
        .unwrap()
        .query_map(params![USER, 15.0, "+2 days", SCORING_GENERATION], |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter().any(|line| line.contains("idx_account_scores_user_score")),
        "plan: {plan:?}"
    );
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test unit_refresh_candidates`
Expected: compile error (expected at compile time — `list_refresh_candidates`, `REFRESH_CANDIDATES_SQL`, `ELEVATED_MIN` missing).

- [ ] **Step 3: Implement**

`models.rs`: `pub const ELEVATED_MIN: f64 = 15.0;` inside `impl ThreatTier`, and `from_score` uses it. `traits.rs`: `RefreshCandidate` + the method, doc: "by score (`ELEVATED_MIN`), eligible when expiring within `horizon_days`, already expired, NULL/malformed expiry (SQLite), or old generation; NULL-score rows never; most dangerous first". `mod.rs`: re-export. `queries.rs`:

```rust
/// Public so the index test can EXPLAIN exactly this statement (R12).
/// Params: ?1 user_did, ?2 ThreatTier::ELEVATED_MIN, ?3 "+N days", ?4 SCORING_GENERATION.
/// COALESCE(…, 1): a NULL or malformed valid_until is expired, hence eligible
/// — the mirror of FRESH_SQL's COALESCE(…, 0) (R11).
pub const REFRESH_CANDIDATES_SQL: &str =
    "SELECT did, handle, graph_distance FROM account_scores
     WHERE user_did = ?1
       AND threat_score >= ?2
       AND (scoring_generation != ?4
            OR COALESCE(datetime(valid_until) <= datetime('now', ?3), 1))
     ORDER BY threat_score DESC, did";

pub fn list_refresh_candidates(conn: &Connection, user_did: &str, horizon_days: i64) -> Result<Vec<RefreshCandidate>> {
    let mut stmt = conn.prepare(REFRESH_CANDIDATES_SQL)?;
    let rows = stmt
        .query_map(
            params![user_did, ThreatTier::ELEVATED_MIN, format!("+{horizon_days} days"), SCORING_GENERATION],
            |r| Ok(RefreshCandidate { did: r.get(0)?, handle: r.get(1)?, graph_distance: r.get(2)? }),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
```

Postgres: `WHERE user_did = $1 AND threat_score >= $2 AND (scoring_generation <> $4 OR valid_until <= NOW() + make_interval(days => $3)) ORDER BY threat_score DESC, did` (no COALESCE — the column is NOT NULL and typed).

- [ ] **Step 4: Record the plans (R12 evidence)**

Against the local Postgres test database, load a representative volume (one user with 3 000 score rows, ~95 % fresh, 3 % expiring, 2 % legacy; a second user with 3 000 rows all `legacy` — the generation-bump workload) using a throwaway script in the scratchpad (not committed), then capture:

```
EXPLAIN (ANALYZE, BUFFERS) SELECT did, handle, graph_distance FROM account_scores
  WHERE user_did = $1 AND threat_score >= 15
    AND (scoring_generation <> $current OR valid_until <= NOW() + INTERVAL '2 days')
  ORDER BY threat_score DESC, did;
```

for both users, and `EXPLAIN QUERY PLAN` on SQLite for the same shape. Expected: an index scan on `idx_account_scores_user_score` with the residual filter applied to the ≤ 3 000 rows of that user, single-digit milliseconds. Paste both plans and timings into the runbook's "Index plans" section (Task 10). If the planner chooses a seq scan on the 3 000-row table because the table is small, note that and re-run with `SET enable_seqscan = off` to show the index is usable — do not add a partial index unless the measured plan without one exceeds 50 ms.

- [ ] **Step 5: Postgres twin test**

Append to `tests/db_postgres.rs` a test seeding the same nine rows (with `NOW() + make_interval(days => $n)` and `'legacy'`; no NULL/malformed rows — the column forbids them) and asserting the ordered DID list `[high-expiring, high-expired, elevated-legacy, elevated-floor]` and graph distances. Prefix DIDs `did:plc:pgrc_`; delete afterwards.

- [ ] **Step 6: Run and commit**

Run: `cargo test --test unit_refresh_candidates`, `cargo test --test unit_scoring`, `VERIFY_PG`, `VERIFY_CLIPPY`. Expected: green.

```bash
git add src/db/models.rs src/db/traits.rs src/db/mod.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs tests/unit_refresh_candidates.rs tests/db_postgres.rs
git commit -m 'feat(344): list_refresh_candidates — High/Elevated by score, expiring/expired/malformed/old-generation, index-backed

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 8: Extract the shared scan setup — and make missing context an error, not an absence

**Why:** `run_refresh` (Task 9) needs the scorer bundle, protected-post embeddings, pile-on set and direct-pair loading that `run_scan` and `amplification::run` build inline. R05: the extracted helpers must **return errors**. Today `direct_pairs_for`'s equivalent swallows a read failure into an empty list (which flips the account to the follower path) and the embedding step swallows into `None` (missing context). A full scan may still choose to degrade at its call site — that is today's behaviour and stays — but the helper itself must not decide that for the refresh.

**Files:**
- Modify: `src/pipeline/amplification.rs:340-366` → `pub async fn direct_pairs_for(...) -> Result<Vec<(String, String)>>`
- Modify: `src/web/scan_job.rs:693-735` → `ScanScorers` + `build_scan_scorers`; `:1108-1128` → `record_scan_cache_stats`; `:875-900` → `embed_protected_posts(...) -> Result<Vec<(String, Vec<f64>)>>`; `:1045-1053` → `pile_on_dids`
- Test: `tests/unit_direct_pairs.rs` (new); existing suites for "nothing changed"

**Interfaces:**
```rust
// src/pipeline/amplification.rs
/// Deduplicated (original, amplifier) text pairs from this user's stored
/// events for `amplifier_did`. Ok(empty) = the lookup succeeded and there
/// are no usable pairs; Err = the lookup failed (R05 — callers decide).
pub async fn direct_pairs_for(db: &Arc<dyn Database>, user_did: &str, amplifier_did: &str) -> anyhow::Result<Vec<(String, String)>>;

// src/web/scan_job.rs
pub(crate) struct ScanScorers { pub scorer: TwoStageToxicityScorer, pub onnx_stats: Arc<CacheStats>, pub classifier_stats: Arc<CacheStats> }
pub(crate) fn build_scan_scorers(models: &ScanModels, db: &Arc<dyn Database>) -> anyhow::Result<ScanScorers>;
pub(crate) async fn record_scan_cache_stats(db: &dyn Database, user_did: &str, s: &ScanScorers);
/// The protected user's recent posts embedded for follower NLI pairing.
/// Err on fetch or embedding failure; Ok(empty) when the user has no posts.
pub(crate) async fn embed_protected_posts(client: &PublicAtpClient, embedder: &SentenceEmbedder, actor_handle: &str) -> anyhow::Result<Vec<(String, Vec<f64>)>>;
pub(crate) async fn pile_on_dids(db: &dyn Database, user_did: &str) -> anyhow::Result<HashSet<String>>;
```

- [ ] **Step 1: Write the failing test**

Create `tests/unit_direct_pairs.rs`:

```rust
// #344 Task 8 / R05: the amplifier direct-pair loader. "No pairs" and "could
// not read pairs" are different answers, and only the first is Ok.

use std::sync::Arc;

use charcoal::db::schema::create_tables;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::pipeline::amplification::direct_pairs_for;
use rusqlite::Connection;

const USER: &str = "did:plc:pairsuser000000000000000";

fn db_with_events() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

#[tokio::test]
async fn pairs_are_deduplicated_and_empty_text_is_skipped() {
    let db = db_with_events();
    for (orig, amp) in [
        (Some("hello"), Some("lol no")),
        (Some("hello"), Some("lol no")), // duplicate event
        (Some("hello"), Some("")),       // empty commentary: not a pair
        (None, Some("orphan")),          // no original text: not a pair
        (Some("second post"), Some("also bad")),
    ] {
        db.insert_amplification_event(
            USER, "quote", "did:plc:amp", "amp.handle",
            "at://did:plc:me/app.bsky.feed.post/1",
            Some("at://did:plc:amp/app.bsky.feed.post/x"),
            amp, orig, None,
        )
        .await
        .unwrap();
    }
    let pairs = direct_pairs_for(&db, USER, "did:plc:amp").await.unwrap();
    assert_eq!(pairs, vec![("hello".to_string(), "lol no".to_string()), ("second post".to_string(), "also bad".to_string())]);
    assert!(direct_pairs_for(&db, USER, "did:plc:nobody").await.unwrap().is_empty(), "a successful lookup with no events is Ok(empty)");
}

/// A read failure is an Err, not an empty list. Simulated by dropping the
/// events table out from under the query.
#[tokio::test]
async fn a_read_failure_is_an_error_not_an_empty_list() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    conn.execute_batch("DROP TABLE amplification_events;").unwrap();
    let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));
    assert!(direct_pairs_for(&db, USER, "did:plc:amp").await.is_err());
}
```

Check `insert_amplification_event`'s exact parameter order in `src/db/traits.rs` before writing the call.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --test unit_direct_pairs`
Expected: compile error (function missing) — expected at compile time.

- [ ] **Step 3: Extract**

`amplification.rs`, above `pub async fn run`:

```rust
pub async fn direct_pairs_for(
    db: &Arc<dyn Database>,
    user_did: &str,
    amplifier_did: &str,
) -> anyhow::Result<Vec<(String, String)>> {
    let db_events = db
        .get_events_by_amplifier(user_did, amplifier_did)
        .await
        .with_context(|| format!("loading stored events for amplifier {amplifier_did}"))?;
    let mut seen_pairs: HashSet<(String, String)> = HashSet::new();
    let mut pairs: Vec<(String, String)> = Vec::new();
    for ev in db_events {
        if let (Some(orig), Some(amp)) = (ev.original_post_text, ev.amplifier_text) {
            if !orig.is_empty() && !amp.is_empty() && seen_pairs.insert((orig.clone(), amp.clone())) {
                pairs.push((orig, amp));
            }
        }
    }
    Ok(pairs)
}
```

and in the amplifier loop (`:340-366`) keep today's degrade-at-the-call-site behaviour explicitly:

```rust
            // A full scan degrades here on purpose (unchanged behaviour): the
            // account still scores on the follower path. The refresh job
            // (web::refresh_scan) does NOT — see R05.
            let pairs = match direct_pairs_for(db, user_did, did).await {
                Ok(p) => p,
                Err(e) => {
                    warn!(amplifier_did = %did, error = %format!("{e:#}"), "direct NLI pairs dropped for this scan");
                    Vec::new()
                }
            };
```

`scan_job.rs` helpers — bodies moved verbatim from `run_scan` (`:700-735` scorer construction incl. `info!`/`record_backend_selected`; `:1108-1128` the two `record_cache_stats` blocks; `:1045-1053` pile-on), except `embed_protected_posts`, which becomes:

```rust
pub(crate) async fn embed_protected_posts(
    client: &PublicAtpClient,
    embedder: &crate::topics::embeddings::SentenceEmbedder,
    actor_handle: &str,
) -> anyhow::Result<Vec<(String, Vec<f64>)>> {
    let pp_texts: Vec<String> = crate::bluesky::posts::fetch_recent_posts(client, actor_handle, 50)
        .await
        .context("fetching the protected user's recent posts for NLI pairing")?
        .iter()
        .map(|p| p.text.clone())
        .collect();
    if pp_texts.is_empty() {
        return Ok(Vec::new());
    }
    let embeddings = embedder
        .embed_batch(&pp_texts)
        .await
        .context("embedding the protected user's posts for NLI pairing")?;
    Ok(pp_texts.into_iter().zip(embeddings).collect())
}
```

`run_scan` keeps its degrade behaviour at the call site:

```rust
    let protected_posts_with_embeddings: Option<Vec<(String, Vec<f64>)>> = match (&embedder, &nli_scorer) {
        (Some(emb), Some(_)) => match embed_protected_posts(&client, emb, actor_handle).await {
            Ok(v) => Some(v),
            Err(e) => {
                warn!(error = %format!("{e:#}"), "protected-post embeddings unavailable; followers score without inferred pairs");
                None
            }
        },
        _ => None,
    };
```

(Note: `fetch_recent_posts` used to be called with `.unwrap_or_default()` here; the change from "empty on error" to "warn + None" is behaviour-preserving for scoring — `None` and `Some(vec![])` both mean "no inferred pairs" in `score_from_sample` — and gains a log line.) Then `let scorers = build_scan_scorers(&models, &db)?;`, `let pile_on_dids = pile_on_dids(db.as_ref(), user_did).await?;`, `record_scan_cache_stats(db.as_ref(), user_did, &scorers).await;`.

- [ ] **Step 4: Run — this task's contract is "nothing changed" for full scans**

Run: `cargo test --test unit_direct_pairs`, `VERIFY_WEB`, `VERIFY_CLIPPY`.
Expected: green, zero `SKIP:`.

- [ ] **Step 5: Commit**

```bash
git add src/pipeline/amplification.rs src/web/scan_job.rs tests/unit_direct_pairs.rs
git commit -m 'refactor(344): extract scan setup helpers; context loaders return errors, callers decide

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 9: `run_refresh` — ownership, resume, compatibility, honest outcomes

**Why:** The job itself, built on the contracts the review forced: staging carries its owner (R02), inputs must be compatible before a current stamp is published (R03), missing context fails the run instead of lowering a score (R05), the cache metric is per-run and only claims what it measured (R08), and every outcome drives the schedule (R02/R04).

**Ownership markers.** `run_phased_scan` gains a `RunIdentity { kind: ScanKind, generation: &'static str }` parameter. At a fresh start it writes `scan_run_kind` and `scan_run_generation` to `scan_state` together with `scan_phase = gather`. On entry with a resumable marker it reads both and:
- generation ≠ current → `clear_scan_staging`, clear the three markers, fresh start (old binary's leftovers; R03);
- kind = caller's kind → resume as today;
- kind ≠ caller's kind → return `Err(PhasedScanError::OwnedByOtherKind(kind))` (a typed error the callers match on — never resume another kind's staging blindly).

**Transitions.**
- Refresh finds `OwnedByOtherKind(Full)` → `Deferred(FullScanResumable)`; the user's own scan (or the admin) drains it. Retry in an hour.
- Full scan finds `OwnedByOtherKind(Refresh)` → **drain first**: call `run_phased_scan` with `RunIdentity::refresh()` and the refresh's re-derived candidates (`list_refresh_candidates`) until it returns `Done` (if it cost-caps again, the full scan reports itself degraded/resumable exactly as today and stops — the next full scan drains again), then clear markers and fresh-start its own gather. The refresh's `refreshed_generation` is **not** marked by this path (the refresh did not complete under its own claim); the full scan's own success marks it.
- Refresh finds its own kind → resume with the current `list_refresh_candidates` as the candidate list (finalize does not need candidates; recovery for an account no longer in the list is a documented skip, as `recover_account` already handles "missing candidate").

**Verdict compatibility (R03).** `finalize_account` receives `expected_policy_version: &str` through `PhasedScanDeps` (the running classifier's `policy_version()`); a `done` row whose `policy_version` differs is treated like an incomplete row → `NeedsRegather` (the decode-error sentinel's own `policy_version` is the classifier's, so #355's sentinel rows are unaffected here).

**Outcomes.**
```rust
pub enum RefreshOutcome {
    Completed { candidates: usize, scored: usize, degraded_accounts: usize },
    NothingDue,
    Deferred(DeferReason),   // FullScanResumable | NoFingerprint | IncompatibleFingerprint
    Resumable,               // cost cap / transient interruption: markers left, own kind
}
```
`Completed`/`NothingDue` → `schedule_after_success` (next nightly, generation proven). `Deferred`/`Resumable` → `schedule_retry` (1 h); `IncompatibleFingerprint` and `NoFingerprint` additionally call `request_full_after_refresh` (Task 5's `full_requested_at`, on its own row) so the user's next admitted run is a full scan that rebuilds; an `Err` from `run_refresh` → row `failed`, `schedule_retry`. The full-scan cooldown marker is never written by a refresh.

**Files:**
- Create: `src/web/refresh_scan.rs`; Modify: `src/web/mod.rs`
- Modify: `src/pipeline/scan_phases/mod.rs:149-215` (`RunIdentity`, markers, `PhasedScanError`), `finalize.rs:252-268` (`verdict_for` policy check), `PhasedScanDeps` (`expected_policy_version`), `src/pipeline/amplification.rs:520-556` (pass identity + policy), `src/pipeline/sweep.rs` (same), `src/web/scan_job.rs` (`launch_scan(kind)`, drain transition, `set_progress`/`finish_scan` pub(crate), `record_scan_outcome` label), `src/web/admitter.rs:497-520`, `src/db/traits.rs` (+ `request_full_after_refresh(user_did)`)
- Test: `src/web/refresh_scan.rs` inline; `tests/unit_scan_phases.rs` (append); `src/web/admitter.rs` (kinds)

**Interfaces:**
```rust
// pipeline::scan_phases
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunIdentity { pub kind: ScanKind, pub generation: &'static str }
impl RunIdentity { pub fn full() -> Self; pub fn refresh() -> Self; }
#[derive(Debug, thiserror::Error)]
pub enum PhasedScanError { #[error("resumable staging is owned by a {0:?} run")] OwnedByOtherKind(ScanKind) }
pub async fn run_phased_scan(db, user_did, candidates, deps, identity: RunIdentity) -> Result<ScanSummary>; // Err downcasts to PhasedScanError
pub const RUN_KIND_KEY: &str = "scan_run_kind"; pub const RUN_GENERATION_KEY: &str = "scan_run_generation";

// web::refresh_scan
pub const REFRESH_HORIZON_DAYS: i64 = 2;
pub enum RefreshOutcome { … } // above
pub fn to_candidate(row: &RefreshCandidate, pairs: Vec<(String, String)>, pile_on: &HashSet<String>) -> CandidateInput;
pub(crate) async fn run_refresh(config, db, models, scan_manager, user_did, actor_handle, claim_id) -> anyhow::Result<()>;
```
`scan_state` keys written by a refresh: `refresh_last_run_id` (= claim_id), `refresh_last_outcome` (`completed|nothing_due|deferred:<reason>|resumable|failed`), `refresh_last_run_at`, `refresh_candidates`, `refresh_scored`, `refresh_feed_cache_hits`, `refresh_feed_cache_misses`, `refresh_feed_cache_applicable` (`1` only when candidates > 0). All eight are (re)written at the **start** of every run (zeros / `running`) so a previous run's numbers can never be read as this run's (R08).

- [ ] **Step 1: Write the failing tests**

`src/web/refresh_scan.rs` inline tests (pure functions):

```rust
    #[test]
    fn candidates_keep_graph_distance_pairs_and_pile_on() {
        let row = RefreshCandidate { did: "did:plc:a".into(), handle: "a.handle".into(), graph_distance: Some("Follows you".into()) };
        let pile_on: HashSet<String> = ["did:plc:a".to_string()].into();
        let c = to_candidate(&row, vec![("o".into(), "r".into())], &pile_on);
        assert_eq!(c.account_did, "did:plc:a");
        assert!(c.is_pile_on);
        assert_eq!(c.graph_distance, Some(GraphDistance::InboundFollow));
        assert_eq!(c.direct_pairs, Some(vec![("o".to_string(), "r".to_string())]));
    }

    /// No stored pairs ⇒ follower path (`direct_pairs: None`), NOT
    /// `Some(vec![])`, which finalize would treat as an amplifier with nothing
    /// to say and skip NLI entirely.
    #[test]
    fn no_pairs_means_follower_mode_and_unparseable_distance_is_none() {
        let row = RefreshCandidate { did: "did:plc:b".into(), handle: "b.handle".into(), graph_distance: Some("not a distance".into()) };
        let c = to_candidate(&row, vec![], &HashSet::new());
        assert_eq!(c.direct_pairs, None);
        assert_eq!(c.graph_distance, None);
    }

    #[test]
    fn outcome_labels_are_stable_scan_state_values() {
        assert_eq!(RefreshOutcome::NothingDue.label(), "nothing_due");
        assert_eq!(RefreshOutcome::Deferred(DeferReason::FullScanResumable).label(), "deferred:full_scan_resumable");
        assert_eq!(RefreshOutcome::Resumable.label(), "resumable");
        assert_eq!(RefreshOutcome::Completed { candidates: 3, scored: 3, degraded_accounts: 0 }.label(), "completed");
    }
```

`tests/unit_scan_phases.rs` — the ownership contract, driven through the real `run_phased_scan` with the file's canned fetcher/scorers (no models needed; the burst uses `StubClassifier`):

```rust
    /// R02: a fresh start records who owns the staging; a resume by the same
    /// kind proceeds; a resume by the other kind is refused with a typed
    /// error and touches nothing.
    #[tokio::test]
    async fn staging_ownership_is_recorded_and_enforced() {
        let db = open_db().await;
        let (deps, candidates) = one_candidate_deps_that_cost_cap_in_burst(); // helper: a StubClassifier scripted to return CostCeilingExceeded on the first chunk
        let summary = run_phased_scan(&db, TEST_USER, &candidates, &deps, RunIdentity::refresh()).await.unwrap();
        assert!(summary.degraded, "cost-capped ⇒ resumable");
        assert_eq!(db.get_scan_state(TEST_USER, "scan_phase").await.unwrap().as_deref(), Some("burst"));
        assert_eq!(db.get_scan_state(TEST_USER, RUN_KIND_KEY).await.unwrap().as_deref(), Some("refresh"));
        assert_eq!(db.get_scan_state(TEST_USER, RUN_GENERATION_KEY).await.unwrap().as_deref(), Some(SCORING_GENERATION));

        let err = run_phased_scan(&db, TEST_USER, &candidates, &deps, RunIdentity::full()).await.unwrap_err();
        assert!(matches!(err.downcast_ref::<PhasedScanError>(), Some(PhasedScanError::OwnedByOtherKind(ScanKind::Refresh))));
        assert_eq!(db.get_scan_state(TEST_USER, "scan_phase").await.unwrap().as_deref(), Some("burst"), "untouched");

        let (deps_ok, _) = one_candidate_deps_that_succeed();
        let summary = run_phased_scan(&db, TEST_USER, &candidates, &deps_ok, RunIdentity::refresh()).await.unwrap();
        assert!(!summary.degraded);
        assert_eq!(db.get_scan_state(TEST_USER, "scan_phase").await.unwrap().as_deref(), Some("done"));
        assert_eq!(db.get_scan_state(TEST_USER, RUN_KIND_KEY).await.unwrap(), None, "markers cleared on Done");
    }

    /// R03: staging left by another generation is discarded, not resumed.
    #[tokio::test]
    async fn old_generation_staging_is_discarded_on_resume() {
        let db = open_db().await;
        let (deps, candidates) = one_candidate_deps_that_cost_cap_in_burst();
        run_phased_scan(&db, TEST_USER, &candidates, &deps, RunIdentity::refresh()).await.unwrap();
        db.set_scan_state(TEST_USER, RUN_GENERATION_KEY, "1999-01-01").await.unwrap();
        let pending_before = db.count_pending_classifications(TEST_USER).await.unwrap();
        assert!(pending_before > 0);

        let (deps_ok, _) = one_candidate_deps_that_succeed();
        let summary = run_phased_scan(&db, TEST_USER, &candidates, &deps_ok, RunIdentity::full()).await.unwrap();
        // A fresh start re-gathered: the old pending rows were cleared first,
        // and the run completed under the current generation.
        assert!(!summary.degraded);
        assert_eq!(db.get_scan_state(TEST_USER, "scan_phase").await.unwrap().as_deref(), Some("done"));
    }

    /// R03: a done verdict row from another classifier policy is incomplete —
    /// finalize must not fold it into a current-generation score.
    #[tokio::test]
    async fn finalize_rejects_verdicts_from_another_policy() {
        use charcoal::pipeline::scan_phases::staging::{AccountInput, QueueRow, VerdictRow, ACCOUNT_INPUT_SCHEMA_VERSION};
        let db = open_db().await;
        let post = make_post("at://p/1", "some original post text that is long enough");
        let sample = PostSample {
            originals: vec![post.clone()],
            replies: vec![],
            quotes: vec![],
            reply_ratio: 0.0,
            quote_ratio: 0.0,
            total_posts: 1,
        };
        let blob = AccountInput {
            schema_version: ACCOUNT_INPUT_SCHEMA_VERSION,
            scoring_generation: SCORING_GENERATION.to_string(),
            account_handle: "acct.handle".to_string(),
            sample,
            parent_texts: HashMap::new(),
            median_engagement: 0.0,
            is_pile_on: false,
            direct_pairs: None,
            graph_distance: None,
            fingerprint_quality: "normal".to_string(),
            target_embedding: None,
        };
        db.stash_account_input(TEST_USER, "did:plc:oldpolicy", &serde_json::to_string(&blob).unwrap())
            .await
            .unwrap();
        db.enqueue_classifications(
            TEST_USER,
            &[QueueRow {
                account_did: "did:plc:oldpolicy".into(),
                post_uri: post.uri.clone(),
                text: post.text.clone(),
                context_text: None,
                post_kind: "original".into(),
                onnx_score: 0.5,
                status: "pending".into(),
                toxic_token: None,
                confidence: None,
                model_id: None,
                policy_version: None,
            }],
        )
        .await
        .unwrap();
        db.record_classification_verdicts(
            TEST_USER,
            &[VerdictRow {
                account_did: "did:plc:oldpolicy".into(),
                post_uri: post.uri.clone(),
                toxic_token: false,
                confidence: 0.9,
                model_id: "stub".into(),
                policy_version: "old-policy".into(),
            }],
        )
        .await
        .unwrap();

        // Finalize under a classifier whose policy is "current".
        let outcome = finalize_account_with_policy(&db, TEST_USER, "did:plc:oldpolicy", "current").await;
        assert_eq!(outcome, charcoal::pipeline::scan_phases::finalize::FinalizeOutcome::NeedsRegather);
        // And under the matching policy the same row is complete.
        let outcome = finalize_account_with_policy(&db, TEST_USER, "did:plc:oldpolicy", "old-policy").await;
        assert_eq!(outcome, charcoal::pipeline::scan_phases::finalize::FinalizeOutcome::Scored);
    }
```

Write the two `one_candidate_deps_*` helpers (a `PhasedScanDeps` over `CannedFetcher` + `FixedScorer(0.9)` + a `StubClassifier::with_script` that yields `CostCeilingExceeded` on its first call, resp. a benign verdict) and `finalize_account_with_policy` (a wrapper over `finalize_account` passing `expected_policy_version`) against the file's existing fixtures; the exact trait-method names for staging rows (`enqueue_classifications`, `record_classification_verdicts`) are in `src/db/traits.rs` — match their signatures if they differ from the sketch above.

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --features web --lib web::refresh_scan` and `cargo test --features web --test unit_scan_phases ownership`
Expected: compile errors (expected at compile time).

- [ ] **Step 3: Pipeline changes**

`mod.rs`: define `RunIdentity`, `PhasedScanError`, the two key consts. In `run_phased_scan`, after reading `raw_phase`:

```rust
    // #344 R02/R03: who owns whatever is staged, and under which generation.
    let owner_kind = db.get_scan_state(user_did, RUN_KIND_KEY).await?.and_then(|k| ScanKind::from_str(&k));
    let owner_generation = db.get_scan_state(user_did, RUN_GENERATION_KEY).await?;
    let resumable = matches!(phase, Some(ScanPhase::Burst) | Some(ScanPhase::Finalize));
    let phase = if resumable && owner_generation.as_deref() != Some(identity.generation) {
        // Left by another binary. Its blobs would fail the generation check
        // one by one in finalize; clearing here is the same decision made
        // once, up front, without a re-gather per account.
        warn!(user_did, ?owner_generation, "resumable staging from another generation — discarding");
        db.clear_scan_staging(user_did).await?;
        None
    } else if resumable && owner_kind.is_some() && owner_kind != Some(identity.kind) {
        return Err(PhasedScanError::OwnedByOtherKind(owner_kind.expect("checked")).into());
    } else {
        phase
    };
```

(a resumable marker with **no** owner recorded is pre-v18 staging from the same binary lineage — treat it as owned by `Full`, since only full scans existed). At the fresh-start branch, next to `scan_phase = gather`: `db.set_scan_state(user_did, RUN_KIND_KEY, identity.kind.as_str())` and `RUN_GENERATION_KEY = identity.generation`. At `Done`: delete both keys (`delete_scan_state` if the trait has it; otherwise set them to `""` and treat empty as absent in the reads above — check `rg -n "fn delete_scan_state" src/db/traits.rs`).

`PhasedScanDeps` gains `pub expected_policy_version: &'a str`; `finalize.rs` `verdict_for` gains it and returns `None` when `row.policy_version.as_deref() != Some(expected)`. `amplification.rs` and `sweep.rs` pass `RunIdentity::full()` and `classifier.policy_version()`.

- [ ] **Step 4: `run_refresh`**

`src/web/refresh_scan.rs` (module doc: what a refresh shares with a full scan and what it does not — Constellation, follower expansion, `classify_relationships`, fingerprint rebuilds):

```rust
pub const REFRESH_HORIZON_DAYS: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferReason { FullScanResumable, NoFingerprint, IncompatibleFingerprint }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    Completed { candidates: usize, scored: usize, degraded_accounts: usize },
    NothingDue,
    Deferred(DeferReason),
    Resumable,
}

impl RefreshOutcome {
    /// Stable `scan_state.refresh_last_outcome` value; the runbook greps it.
    pub fn label(&self) -> String {
        match self {
            RefreshOutcome::Completed { .. } => "completed".to_string(),
            RefreshOutcome::NothingDue => "nothing_due".to_string(),
            RefreshOutcome::Deferred(DeferReason::FullScanResumable) => "deferred:full_scan_resumable".to_string(),
            RefreshOutcome::Deferred(DeferReason::NoFingerprint) => "deferred:no_fingerprint".to_string(),
            RefreshOutcome::Deferred(DeferReason::IncompatibleFingerprint) => "deferred:incompatible_fingerprint".to_string(),
            RefreshOutcome::Resumable => "resumable".to_string(),
        }
    }

    /// Accounts re-scored by this run (0 unless Completed).
    pub fn scored(&self) -> usize {
        match self {
            RefreshOutcome::Completed { scored, .. } => *scored,
            _ => 0,
        }
    }

    /// True when staging was left for a later run — reported to
    /// `finish_scan` as the "degraded" flag so the status copy says so.
    pub fn is_resumable(&self) -> bool {
        matches!(self, RefreshOutcome::Resumable)
    }
}

pub fn to_candidate(row: &RefreshCandidate, pairs: Vec<(String, String)>, pile_on: &HashSet<String>) -> CandidateInput {
    CandidateInput {
        account_did: row.did.clone(),
        account_handle: row.handle.clone(),
        is_pile_on: pile_on.contains(&row.did),
        direct_pairs: if pairs.is_empty() { None } else { Some(pairs) },
        graph_distance: row.graph_distance.as_deref().and_then(GraphDistance::from_str),
    }
}

pub(crate) async fn run_refresh(
    config: Arc<Config>, db: Arc<dyn Database>, models: Arc<ScanModels>,
    scan_manager: Arc<RwLock<ScanManager>>, user_did: &str, actor_handle: &str, claim_id: &str,
) -> anyhow::Result<()> {
    reset_run_markers(db.as_ref(), user_did, claim_id).await?; // the eight keys → running/0 (R08)
    let now = chrono::Utc::now();
    let outcome = run_refresh_inner(&config, &db, &models, &scan_manager, user_did, actor_handle, claim_id).await;
    let (result, label) = match &outcome {
        Ok(o) => (Ok((0, o.scored(), o.is_resumable())), o.label()),
        Err(e) => (Err(anyhow::anyhow!("{e:#}")), "failed".to_string()),
    };
    // Record before scheduling so an operator reading scan_state sees the
    // outcome that produced the schedule.
    let _ = db.set_scan_state(user_did, "refresh_last_outcome", &label).await;
    let _ = db.set_scan_state(user_did, "refresh_last_run_at", &now.to_rfc3339()).await;
    match &outcome {
        Ok(RefreshOutcome::Completed { .. }) | Ok(RefreshOutcome::NothingDue) => {
            crate::web::refresh::schedule_after_success(db.as_ref(), user_did, now).await
        }
        Ok(RefreshOutcome::Deferred(reason)) => {
            if matches!(reason, DeferReason::NoFingerprint | DeferReason::IncompatibleFingerprint) {
                // The next admitted run for this user must be a full scan,
                // which rebuilds. Recorded on our own row; finish hands over.
                if let Err(e) = db.request_full_after_refresh(user_did).await {
                    warn!(error = %format!("{e:#}"), "could not request the follow-up full scan");
                }
            }
            crate::web::refresh::schedule_retry(db.as_ref(), user_did, now).await
        }
        Ok(RefreshOutcome::Resumable) | Err(_) => crate::web::refresh::schedule_retry(db.as_ref(), user_did, now).await,
    }
    finish_scan(&scan_manager, user_did, claim_id, result, "Refresh").await
}

async fn run_refresh_inner(…) -> anyhow::Result<RefreshOutcome> {
    // 1. Fingerprint: read, never rebuilt. Missing or incompatible ⇒ defer
    //    and ask for a full scan (R03).
    let Some((json, _, _)) = db.get_fingerprint(user_did).await? else {
        return Ok(RefreshOutcome::Deferred(DeferReason::NoFingerprint));
    };
    let fingerprint: TopicFingerprint = serde_json::from_str(&json).context("stored fingerprint is unreadable")?;
    let protected_embedding = db.get_embedding(user_did).await?;
    let model_id = db.fingerprint_embedding_model(user_did).await?;
    if protected_embedding.is_some() && model_id.as_deref() != Some(EMBEDDING_MODEL_ID) {
        warn!(user_did, ?model_id, "fingerprint embeddings are from another model — deferring to a full scan");
        return Ok(RefreshOutcome::Deferred(DeferReason::IncompatibleFingerprint));
    }
    let protected_topic_centroids: Vec<Vec<f64>> = db.get_topic_centroids(user_did).await?.iter().map(|c| c.centroid.clone()).collect();

    // 2. Candidates from the table.
    let rows = db.list_refresh_candidates(user_did, REFRESH_HORIZON_DAYS).await?;
    db.set_scan_state(user_did, "refresh_candidates", &rows.len().to_string()).await?;
    db.set_scan_state(user_did, "refresh_feed_cache_applicable", if rows.is_empty() { "0" } else { "1" }).await?;

    // 3. Required context (R05): any failure here is an Err — no score is
    //    written, the row fails, retry in an hour. Missing context would
    //    systematically LOWER every High/Elevated score we are about to
    //    overwrite.
    let scorers = build_scan_scorers(models, db)?;
    let client = PublicAtpClient::new(&config.public_api_url)?;
    let protected_posts_with_embeddings = embed_protected_posts(&client, &models.embedder, actor_handle).await?;
    let pile_on = pile_on_dids(db.as_ref(), user_did).await?;
    let median_engagement = db.get_median_engagement(user_did).await?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in &rows {
        let pairs = direct_pairs_for(db, user_did, &row.did).await?;
        candidates.push(to_candidate(row, pairs, &pile_on));
    }

    // 4. Resume-or-start through the shared pipeline. Ownership is enforced
    //    inside run_phased_scan (R02).
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
        protected_posts_with_embeddings: Some(&protected_posts_with_embeddings),
        data_dir: Some(config.data_dir()),
        median_engagement,
        gather_concurrency: 8, // same literal run_scan uses today; Phase 3 replaces both
        burst_concurrency: burst::burst_concurrency(),
        burst_batch: burst::burst_batch(),
        expected_policy_version: classifier.policy_version(),
    };
    let summary = match run_phased_scan(db, user_did, &candidates, &deps, RunIdentity::refresh()).await {
        Ok(s) => s,
        Err(e) if matches!(e.downcast_ref::<PhasedScanError>(), Some(PhasedScanError::OwnedByOtherKind(ScanKind::Full))) => {
            return Ok(RefreshOutcome::Deferred(DeferReason::FullScanResumable));
        }
        Err(e) => return Err(e),
    };
    // NothingDue is decided AFTER the ownership check on purpose: a refresh
    // with no candidates still resumes its own leftover staging.
    if rows.is_empty() && !summary.degraded {
        return Ok(RefreshOutcome::NothingDue);
    }

    record_cache_stats(db.as_ref(), user_did, "refresh_feed", &feed_stats).await?;
    record_scan_cache_stats(db.as_ref(), user_did, &scorers).await;
    db.set_scan_state(user_did, "refresh_scored", &summary.accounts_scored.to_string()).await?;
    if summary.degraded && db.get_scan_state(user_did, "scan_phase").await?.as_deref() != Some("done") {
        return Ok(RefreshOutcome::Resumable);
    }
    Ok(RefreshOutcome::Completed { candidates: rows.len(), scored: summary.accounts_scored, degraded_accounts: db.count_scan_skips(user_did).await?.max(0) as usize })
}
```

`request_full_after_refresh(user_did)`: `UPDATE scan_queue SET full_requested_at = COALESCE(full_requested_at, now) WHERE user_did = ? AND status = 'running' AND kind = 'refresh'` on both backends (Task 5's finish hands over).

`launch_scan(state, user_did, actor_handle, kind, slot, live)`: `match kind { Full => run_scan(...), Refresh => run_refresh(...) }` inside the spawned future. `run_scan`'s pipeline call passes `RunIdentity::full()`; on `PhasedScanError::OwnedByOtherKind(Refresh)` it performs the **drain** (see Transitions) and then proceeds. `AppStateLauncher::launch` passes `claim.kind`; the admit log line gains `kind`. `record_scan_outcome`/`finish_scan` gain a `label: &str` ("Completed" / "Refresh complete") so a refresh's status message does not read "0 events".

- [ ] **Step 5: Run**

Run: `cargo test --features web --lib web::refresh_scan`, `cargo test --features web --test unit_scan_phases`, `VERIFY_WEB`, `cargo build --features postgres`, `VERIFY_CLIPPY`.
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add src/web/refresh_scan.rs src/web/mod.rs src/web/scan_job.rs src/web/admitter.rs src/pipeline/scan_phases/mod.rs src/pipeline/scan_phases/finalize.rs src/pipeline/amplification.rs src/pipeline/sweep.rs src/db/traits.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs tests/unit_scan_phases.rs
git commit -m 'feat(344): run_refresh with staging ownership, resume, input compatibility and honest outcomes

run_phased_scan records scan_run_kind/scan_run_generation; a refresh
resumes its own work, refuses a full scan'"'"'s (Deferred), a full scan
drains a refresh'"'"'s leftovers before gathering, and other-generation
staging is discarded. Verdicts from another classifier policy are
incomplete. Missing context is an error (no score written). Outcomes
drive the schedule: completed/nothing-due → nightly + generation proven;
deferred/resumable/failed → retry in an hour; missing or incompatible
fingerprint also requests a follow-up full scan. Per-run cache counters
are reset at start and flagged not-applicable when nothing was due.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 10: Docs, runbook, spec amendment

**Files:** `CHANGELOG.md`, `README.md`, `docs/runbooks/343-phase2-expiry-refresh.md` (new), spec §4.4 and §6 Phase 2.

- [ ] **Step 1: CHANGELOG** — under `## [Unreleased]`:

```markdown
### Added
- #344 / #343 Phase 2 — score expiry and the nightly refresh. Every
  `account_scores` row carries `scoring_generation` (build-time constant,
  `src/scoring/generation.rs`) and `valid_until` (3/7/14 days by scoring
  confidence — the tiers that existed since #135, finally wired). Tier
  lists, counts and the pipeline's already-scored gates show fresh rows
  only; expired and old-generation rows are kept, hidden and reported as
  `tier_counts.expired`. A refresh queue kind (`CHARCOAL_REFRESH_INTERVAL_HOURS`,
  default 24, `0` disables) re-scores each user's High/Elevated rows that
  expire within two days or predate the current generation, from the
  table, on the existing admitter tick — one bounded transaction per tick,
  so a crash cannot lose a scheduled refresh. A generation bump makes every
  user with scores due again (`users.refreshed_generation`). Staged scan
  work records its owning run kind and generation: a refresh resumes its
  own interrupted work, never a user's, and never publishes a current stamp
  from another generation's inputs or another classifier policy's verdicts.
  Fingerprints record their embedding model; a refresh whose fingerprint is
  missing or incompatible asks for a full scan instead of rebuilding.

### Changed
- `charcoal migrate` copies every score row verbatim (`export_scores` /
  `import_score`), including expired, legacy and NotAssessed rows, with
  their original `scored_at`, generation and expiry — and never renews
  expiry. (It previously copied through the ranked-threats query, which
  omits NULL-score rows and would now omit hidden ones.) Corrupt evidence
  JSON still reads as empty on export (#364).
- Freshness reads in the pipeline are hard errors; a refresh that cannot
  load its required context (stored events, protected posts, embeddings)
  fails and retries rather than scoring without it.
- A user's full-scan request made while a refresh is running is recorded
  and runs when the refresh finishes; upgrading a queued refresh keeps the
  user's queue position. The 24 h cooldown anchors on the last *full* scan
  (backfilled by migration v18).
```

- [ ] **Step 2: README** — a "Score expiry and refresh" section (plain language: validity windows, generation, what "Expired" means, how the nightly refresh works, `CHARCOAL_REFRESH_INTERVAL_HOURS`, and that a refresh never replaces a scan you asked for).

- [ ] **Step 3: Runbook** — create `docs/runbooks/343-phase2-expiry-refresh.md` with these sections, each with the exact `psql` (via `railway run -s Postgres -e staging -- sh -c 'psql "$DATABASE_PUBLIC_URL" -tAc "…"'`) or curl to run and the expected reading:
  1. **Before deploy:** pre-deploy High/Elevated counts per user.
  2. **Deploy → hidden:** `SELECT scoring_generation, COUNT(*) …` all `legacy`; `/api/status` tiers 0, `expired` = total.
  3. **First tick → refresh:** within 30 s `scan_queue` shows `kind = refresh` for every user with scores (bounded: 25 per tick); after each finishes, `scan_state` keys `refresh_last_run_id`, `refresh_last_outcome = completed`, `refresh_candidates` = pre-deploy High+Elevated, `refresh_scored` = same minus `scan_skips`; `users.refreshed_generation` = current; zero `legacy` rows with `threat_score >= 15`.
  4. **The human still wins:** click Scan while a refresh is queued → row flips to `full`, `enqueued_at` unchanged; while running → 202 with `queued: "after_refresh"`, `full_requested_at` set, and after the refresh the row is `queued full`; cooldown still measured from `scan_state.last_full_scan_finished_at`.
  5. **Interruption:** with `CHARCOAL_RUNPOD_COST_CAP` (or the existing cost-cap knob — name it from `src/pipeline/scan_phases/burst.rs`) set low, force a refresh to cost-cap; verify `scan_state.scan_phase = burst`, `scan_run_kind = refresh`, `refresh_last_outcome = resumable`, `next_refresh_at` ≈ now + 1 h; raise the cap, force the tick (`UPDATE users SET next_refresh_at = NOW()`), verify the same claim resumes to `completed` with no re-gather (the `refresh_candidates` count is the staged set).
  6. **Feed cache — functional, not a rate (R08):** (a) warm: run a full scan, then within 24 h `UPDATE account_scores SET valid_until = NOW() WHERE user_did = … AND threat_score >= 15` and force the tick — expect `refresh_feed_cache_applicable = 1` and `hits ≥ misses` for those candidates; (b) cold: `DELETE FROM account_feed_snapshots` then the same — expect `hits = 0`; (c) no-op: force the tick with nothing due — expect `refresh_last_outcome = nothing_due`, `refresh_feed_cache_applicable = 0`, counters 0. Record the steady-state hit share over the first two weeks as a number in this file; there is **no** threshold. The arithmetic that withdrew the 80 % gate: High rows (14 d) enter the 2 d horizon ~12 d after scoring, snapshots live 24 h, so a steady-state refresh hits only for candidates active in the last day. Do not raise `SNAPSHOT_TTL` to move this number.
  7. **Generation bump procedure:** change `SCORING_GENERATION`, deploy single-replica; steps 2–3 repeat automatically via `refreshed_generation`; with refreshes disabled (`0`) the lists stay hidden until users re-engage — state this in the deploy notes for that bump.
  8. **Index plans:** paste Task 7's `EXPLAIN` output and timings for both backends.
  9. **Migration rehearsal (R01):** on a copy of the prod SQLite (`backups/`), `charcoal migrate` into a scratch Postgres; compare `SELECT COUNT(*), COUNT(*) FILTER (WHERE threat_score IS NULL), MIN(scored_at), MAX(valid_until)` source vs destination; repeat the migrate; counts unchanged.

- [ ] **Step 4: Spec** — §4.4 amendment (dated 2026-09-14): v18; `refreshed_generation` replaces the migration stamp; ownership markers and the transitions; the compatibility contract; `full_requested_at` and in-place upgrade; lossless migrate; the tick's single transaction and batch bound; SQLite nullable/malformed expiry semantics; retry cadence; "full fortnightly" remains #342. §6 Phase 2: replace the ≥ 80 % feed-hit pass with the functional test + recorded share (reference deciduous 878).

- [ ] **Step 5: Commit, PR, CodeRabbit loop.** Do **not** close #344 until the runbook has been executed on staging and its readings are pasted into the runbook (a separate docs commit on the same PR or a follow-up).

---

## Review-resolution table (Astra plan review of `c91afb8`)

| ID | Disposition | Revised location | Acceptance test (planned, not yet executed) | Remaining limitation |
|---|---|---|---|---|
| R01 | addressed | Task 3 (`export_scores`/`import_score`, `migrate`), Global Constraints | `tests/unit_score_export.rs` (4-row round trip incl. NULL-score, repeat import, v17 fixture); Postgres twin; runbook §9 rehearsal on a prod copy | `top_toxic_posts` corruption still exports as empty (#364) |
| R02 | addressed | Task 9 (`RunIdentity`, markers, `PhasedScanError`, outcomes, transitions), Task 6 (`schedule_retry`) | `staging_ownership_is_recorded_and_enforced`; runbook §5 cost-cap + resume | A refresh's resume uses the *current* candidate list; a staged account no longer eligible is finalized from staging but cannot be re-gathered (documented skip) |
| R03 | addressed | Task 1 (`EMBEDDING_MODEL_ID`, blob generation, bump rule), Task 2 (`embedding_model_id` column), Task 9 (fingerprint check, verdict policy check, generation-mismatch discard) | `finalize_rejects_a_blob_from_another_generation`, `old_generation_staging_is_discarded_on_resume`, `finalize_rejects_verdicts_from_another_policy` | Two binaries with different generations overlapping for seconds on a rolling deploy can each stamp one scan; accepted and documented |
| R04 | addressed | Task 6 (`claim_and_enqueue_due_refreshes`, batch bound, retry, metrics) | `a_failed_enqueue_rolls_back_the_schedule_advance` (trigger-induced failure), `a_tick_is_bounded…`, Postgres two-scheduler partition test | Delivery is at-least-once per tick, not exactly-once across process kills mid-transaction — the transaction rolls back, the next tick re-selects |
| R05 | addressed | Task 8 (`Result` helpers), Task 9 (fail the run, `schedule_retry`, no write) | `a_read_failure_is_an_error_not_an_empty_list`; run_refresh returns `Err` on protected-feed/embedding failure (covered by the outcome→schedule path; add an injected-failure test with a failing fetcher in `unit_scan_phases` if the executor can construct `ScanModels` without models — otherwise runbook) | Per-account gather failures inside `run_phased_scan` are still skips (existing behaviour), not run failures |
| R06 | addressed | Task 3 Step 6 (`run_topic_first`) | Extend `tests/unit_discovery.rs` (or the sweep test that covers `run_topic_first`) with an expired row that becomes eligible | none |
| R07 | addressed | Task 2 (no time stamp; `refreshed_generation` NULL), Task 6 (due rule) | `due_by_time_or_by_generation_and_never_without_scores`; runbook §7 | Disabled refreshes ⇒ no prompt recovery after a bump (stated in runbook) |
| R08 | addressed | Task 9 (per-run reset, `applicable` flag), Task 10 (runbook §6, spec §6), deciduous 878 | Runbook §6 (a)(b)(c) | Steady-state share is recorded, not gated |
| R09 | addressed | Task 5 (`full_requested_at`, in-place upgrade, `EnqueueOutcome`, handover in `finish_queued_scan`) | `a_full_enqueue_upgrades_a_queued_refresh_in_place`, `a_full_request_during_a_running_refresh_is_recorded_and_runs_after_it`, `a_failed_refresh_still_hands_over…`, Postgres twins; runbook §4 | Dashboard copy for `queued: "after_refresh"` is #365's |
| R10 | addressed | Global Constraints (`VERIFY_*`), every task's Run steps | Each command is a valid single-filter invocation; `VERIFY_WEB` fails on `SKIP:` via `pipefail` + negated grep | none |
| R11 | addressed | Task 3 (`FRESH_SQL` COALESCE), Task 7 (candidate COALESCE) | `fresh_set_is_exactly…including_null_and_malformed_expiry` (count = 6), `selects_high_and_elevated…malformed…` | Postgres cannot hold a malformed value; only SQLite is exercised |
| R12 | addressed | Task 2 (index changed to `(user_did, threat_score)`), Task 7 Step 4 | `candidate_query_uses_the_user_score_index` (SQLite plan assertion); runbook §8 plans + timings on 3 000-row users | Plans on a 3 000-row table may prefer a seq scan; the runbook says how to show index usability without adding a partial index prematurely |
| R13 | addressed | Task 2 (backfill), Task 5 (`last_full_finished_at` anchor) | `test_migration_v18_upgrades_a_v17_database` (marker = done row's `finished_at`); `scan.rs` inline tests; runbook §4 | none |

**Architectural changes relative to rev 1:** ownership markers on staging + a typed `PhasedScanError`; a compatibility contract (embedding model id on fingerprints, generation on blobs, policy on verdicts) instead of "bump the generation for everything"; scheduling as one bounded transaction with generation-driven due-ness; a durable full-scan request on the queue row; lossless export/import as a separate contract from presentation; error-returning context helpers.

**Unresolved product decisions (Bryan):** none new. Decision 777 (expire, refresh High/Elevated only) stands; the fortnightly full rescan stays in #342.

**Validation performed for this revision:** none of the above tests has been executed — this is a plan. Verified in this session: Astra's three isolated SQLite reproductions were re-derived from the rev-1 text and the code paths it cited (`main.rs:1170-1176` migrate, `mod.rs:215-330` resume, `finalize.rs:77-110` blob check, `staging.rs:73-120` row provenance, `queries.rs:1466-1590` queue SQL, `sweep.rs:219-224` topic-first gate).

