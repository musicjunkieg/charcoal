# #343 Phase 2 — Score Expiry and Nightly Refresh Implementation Plan (rev 7)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Revision 2 (2026-09-14)** resolved Astra's first plan review (REQUEST CHANGES on `c91afb8`, R01–R13). **Revision 3 (2026-09-14)** resolved the second review (REQUEST CHANGES on `30cd7f4`, V2-01–V2-07). **Revision 4 (2026-09-14)** resolved the third (REQUEST CHANGES on `c40be7f`, V3-01–V3-06). **Revision 5 (2026-09-14)** resolved the fourth (changes requested on `fb713a3`, V4-01–V4-05). **Revision 6 (2026-09-14)** resolved the fifth (changes requested on `4b9459d`, V5-01–V5-03). **Revision 7 (2026-09-14)** resolves the sixth (changes requested on `86c4756`, V6-01–V6-02). All six resolution tables are at the end. Every task below is the current contract; superseded snippets are replaced, not annotated.

**Goal:** Every stored score carries a generation stamp and an expiry; tier lists show only current, unexpired rows; and a nightly refresh job, driven from the existing admitter tick, re-scores the High/Elevated set before it expires — closing #344 and giving #342 the scheduler seam it needs — without losing scores in migration, without stranding its own interrupted work, and without stamping stale inputs as current.

**Architecture:** Migration v18 adds `scoring_generation` + `valid_until` to `account_scores`, `kind` + `full_requested_at` to `scan_queue`, `next_refresh_at` + `refreshed_generation` to `users`, `embedding_model_id` to `topic_fingerprint`, and backfills the full-scan cooldown marker. The write path stamps scores from a build-time `scoring_revision()` and `ScoringConfidence::staleness_days()`. One null-safe **fresh** predicate serves every tier read, count and pipeline gate; a separate **lossless** export/import serves `charcoal migrate`. The refresh is a second queue kind that reuses the admitter, lease/fencing and `run_phased_scan`; its candidates come from `account_scores`. Resumable staging records which run kind and generation owns it, so a refresh resumes its own work, a full scan drains a refresh's leftovers before gathering, and old-generation staging is discarded. Scheduling is one transaction per tick (claim + enqueue together, bounded), and a generation bump makes users due through a durable `refreshed_generation` column, not a one-off migration.

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
  - `VERIFY_PG` — `DATABASE_URL=postgres://$USER@localhost/charcoal_test DATABASE_URL_MIGRATIONS=postgres://$USER@localhost/charcoal_test_migrations cargo test --all-targets --features postgres` (`createdb charcoal_test; createdb charcoal_test_migrations` once). The **migrations** database is the only one destructive fixtures ever touch (V3-06): `migrate_postgres_through` refuses any URL whose database name does not end in `_migrations`. Every test that opens the migrations database is serialized twice over (V4-05): a process-local `migrations_db_lock()` mutex for tests in one binary, and a Postgres session advisory lock (`pg_advisory_lock(hashtext('charcoal_migrations_fixture'))`, held on a dedicated connection for the test's duration) for separate processes or overlapping CI invocations; each such test drops-all under both locks at its start. Postgres tests must **fail**, not return, when either variable is unset in CI (Task 2 adds the guards). Task 10 runs the whole parallel suite ten times in a loop as the isolation evidence.
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
- **The stored stamp is the scoring revision (R03, V2-01).** `scoring_revision()` = `SCORING_GENERATION | ONNX_MODEL_ID | EMBEDDING_MODEL_ID | NLI_MODEL_ID`, composed once at first use (`LazyLock`). It is what `account_scores.scoring_generation`, `AccountInput.scoring_generation`, `scan_state.scan_run_generation` and `users.refresh*_generation` hold, and what every freshness read compares against. Swapping any in-binary model therefore expires every stored score automatically; the human `SCORING_GENERATION` component is bumped for formula/format/policy changes **and for a classifier model or policy change** (the classifier lives outside the binary — CoPE-B/Zentropi — so its identity cannot be composed in; the runbook's bump procedure covers it). The ONNX/embedding/classifier caches are keyed by their own model identities and are **not** invalidated by a revision change — compatible inputs stay reusable.
- **Input compatibility (R03, V2-01, V2-04, V3-01).** A current-revision score may be published only from: a fingerprint whose `embedding_model_id` equals `EMBEDDING_MODEL_ID` (or a keyword-only fingerprint with no embedding), an `AccountInput` blob whose `scoring_generation` equals the current revision, and done verdict rows whose recorded **producer** is one this binary runs — either the Stage-1 clean pass (`model_id = ONNX_MODEL_ID`, `policy_version = CLEAN_PASS_POLICY`) or the Stage-2 classifier (its `model_id` **and** `policy_version`, one string on every path: advertised, written on a miss, matched on a hit). Missing, foreign and decode-error-sentinel provenance are distinct and all rejected (→ bounded re-gather, never a skip). Full scans rebuild an incompatible fingerprint and **abort without scoring** if that rebuild fails (a stale-but-compatible fingerprint may still fall back, as today); refreshes request a full scan instead and never rebuild.
- **Revision change procedure.** In-binary model swaps change the revision by themselves. `SCORING_GENERATION` is bumped by hand for formula/weights, fingerprint format, scoring policy, and classifier model/policy changes; the runbook (§7) makes the classifier case a deploy checklist item. Rolling deploys: a revision change ships as a single-replica deploy (Railway's default); during the seconds of overlap the old binary can still stamp its in-flight scan's rows with the old revision, which the new binary hides and the refresh re-scores — acceptable, documented in the runbook. Staged work from the old binary is discarded by the `scan_run_generation` check.
- **Queue order stays FIFO across kinds** (#271). A full enqueue over a queued refresh upgrades the row **in place, keeping `enqueued_at`** (R09). A full request during a *running* refresh is recorded durably in `scan_queue.full_requested_at` and becomes a queued full row when the refresh finishes.
- **Durable scheduling (R04, V2-02, V2-03).** One transaction per tick, bounded to `REFRESH_BATCH_PER_TICK` users: select due users, then for each **conditionally** write the queue row (`INSERT … ON CONFLICT DO UPDATE … WHERE status IN ('done','failed')`) and advance `next_refresh_at` + set `refresh_attempted_generation` **only if that write affected a row**. The condition is evaluated at the write, so a full scan enqueued or admitted between the select and the write survives untouched (its user is simply not advanced and is reconsidered next tick). Lock order: the tick locks `users` rows then writes `scan_queue`; manual enqueue and finish lock only `scan_queue`; completion bookkeeping writes only `users` — no cycle. Due-ness separates *needs work* from *may attempt*: a user is eligible when they have score rows **or** their finished queue row owes a full scan (V4-01), and due when `next_refresh_at <= now` **or** `refresh_attempted_generation ≠ current revision` (a genuinely new revision is attempted promptly, once). Both `schedule_retry` and the tick stamp `refresh_attempted_generation`, so a retry after a failed/deferred/resumable attempt — from either scan kind — only becomes due by time. Users with neither scores nor an obligation are never selected. A failed transaction advances nothing and is retried next tick. **Owed full work is retried as full work (V3-03):** when a due user's finished row carries `full_requested_at`, the tick's conditional write re-queues it as `kind = 'full'`, never as a refresh.
- **Errors are not absence (R05).** Helpers that load context return `Result`; a refresh that cannot obtain required context fails the run (row `failed`, retry in `REFRESH_RETRY_HOURS`) and writes no score. The existing High/Elevated row keeps its own expiry — a failed refresh never extends validity. The failure path is a **mandatory deterministic test** through the `RefreshContextSource` boundary (Task 9), not a runbook step.
- **Completion is explicit (V2-05, V3-02).** `Ok(..)` from the pipeline is never "complete". Full scans classify into `ScanCompletion::{Complete, CompleteWithSkips{n}, Resumable}` and refreshes into `RefreshOutcome::{Completed, CompletedWithSkips{n}, NothingDue, Deferred(..), Resumable}`. Two different privileges: **revision proof + nightly schedule** go only to `Complete`/`Completed`/`NothingDue`; the **cooldown anchor** (`scan_state.last_full_scan_finished_at`) is written for `Complete`, `CompleteWithSkips` **and** `CompleteUnverified` — the user's request was fulfilled; skipped or unverifiable accounts are gaps the retry covers (V6-01). `Resumable` writes no marker and keeps the full obligation (V3-03). The queue row records the outcome durably (`scan_queue.completion`), and the cooldown reads **only** the marker — never a row's `done` status — so an interrupted attempt can be re-run at once. `CompleteWithSkips`/`CompletedWithSkips` show as degraded and schedule a retry. **Completion is classified from persisted state (V5-01):** the invocation's `degraded` flag describes one attempt, but `scan_skips` survives a burst/finalize resume, so a `done` marker with a positive persisted skip count is `CompleteWithSkips` even when the resuming invocation reports `degraded = false`; an **unavailable** skip count is `CompleteUnverified` — the request counts as fulfilled for the cooldown, but it is never clean completion and never proves the revision.
- **Full-scan bookkeeping boundary (V5-02).** `run_scan` is a thin wrapper, `run_scan_with(scan_manager, books: &dyn FullScanBookkeeping, …, run)`, around one captured outcome: scorer construction, the fingerprint rebuild (including its abort arm), discovery, the pipeline and classification all happen inside `run`. There is exactly one scheduling site, in the wrapper: complete → marker + nightly + proof; complete-with-skips/unverified → marker + hourly retry; resumable or **any error** → hourly retry, obligation kept, no marker, no proof. The slot lifecycle finishes the row and never schedules. **Deadlines are anchored on the attempt's end (V6-02):** both wrappers take an injected clock, read it once *after* the attempt returns, and pass that instant to the scheduler, so a 90-minute failed attempt still gets a full hour of backoff; the start instant is telemetry only. **A drain taints the run (V6-01):** when a full scan drains refresh-owned staging, the drain's completion is folded into the run's own with `ScanCompletion::worst`, so an unverified or skip-laden drain can never be reported as clean completion of the user's request — and a drain alone never completes it: the run's own gather still has to reach `done`.
- **Full-request lifecycle (R09, V2-05, V3-03).** `scan_queue.full_requested_at` means "a full scan is owed since T". Every user enqueue sets it (`COALESCE`, so clicks coalesce); a refresh handover keeps it; a resumable or failed full attempt keeps it; **only a full scan finishing `Complete`, `CompleteWithSkips` or `CompleteUnverified` clears it** (fulfilled, V6-01). The tick retries owed work as `kind = 'full'`, and an owed full scan is **eligible for that retry with zero score rows** (V4-01) — a first scan that failed before its first write is not stranded. All of this is in `scan_queue`/`scan_state`, so a worker restart changes nothing. A full scan **always enters `run_phased_scan`**, even with no fresh candidates (V4-02), so staged work is resumed or drained before anything can be called complete; a `burst`/`finalize` marker after the run is `Resumable` regardless of the `degraded` flag.
- **SQLite/Postgres divergence, deliberate:** Postgres `valid_until` is `NOT NULL` after backfill; SQLite stays nullable and NULL/malformed read as expired. **Cross-backend expiry conversion (V2-06):** export carries `ExportedExpiry::{At(rfc3339), Missing, Invalid(raw)}`; importing `Missing`/`Invalid` into Postgres writes `valid_until = scored_at` (expired the instant it was scored — never renewed, never omitted; the raw text is logged, not stored). **Precision:** Postgres keeps microseconds through export/import; SQLite stores whole seconds (its `datetime()` column form), so a Postgres → SQLite import truncates to the second — documented and tested, not "byte-for-byte".
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
| `src/scoring/generation.rs` (new) | Task 1: `scoring_revision()`, `LEGACY_GENERATION`, bump rule. |
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

### Task 1: Scoring revision, model identities, and the confidence → staleness mapping

**Why:** Spec §4.4's build-time generation, made safe against forgotten bumps (V2-01): the stamp actually stored is `scoring_revision()`, the generation composed with every in-binary model identity, so a model swap expires stored scores by itself. Plus the identities R03 needs: `EMBEDDING_MODEL_ID` so a stored fingerprint can say which model produced its vectors (384 dimensions is not an identity), `NLI_MODEL_ID` for the same reason, and a `scoring_generation` (holding the revision) on the staged `AccountInput` so a resumed finalize cannot publish a current stamp from an old run's inputs.

**Files:**
- Create: `src/scoring/generation.rs`; Modify: `src/scoring/mod.rs`
- Modify: `src/topics/embeddings.rs:24` (next to `EMBEDDING_DIM`); `src/scoring/nli.rs:71` (`NLI_MODEL_ID`)
- Modify: `src/db/models.rs` (`impl ScoringConfidence`)
- Modify: `src/pipeline/scan_phases/staging.rs:22` (`ACCOUNT_INPUT_SCHEMA_VERSION` → 3) and `:131-160` (`AccountInput.scoring_generation`); `src/pipeline/scan_phases/gather.rs:511-523` (stamp it); `src/pipeline/scan_phases/finalize.rs:98-108` (reject mismatch)
- Test: `tests/unit_scoring.rs` (append); `tests/unit_scan_phases.rs` (append)

**Interfaces:**
- Produces: `charcoal::scoring::generation::{SCORING_GENERATION, LEGACY_GENERATION, compose_revision, scoring_revision}` — `scoring_revision() -> &'static str` is the stored stamp used by every task; `charcoal::topics::embeddings::EMBEDDING_MODEL_ID: &str = "all-MiniLM-L6-v2"`; `charcoal::scoring::nli::NLI_MODEL_ID: &str = "nli-deberta-v3-xsmall-fp32"`; `ScoringConfidence::from_label(&str) -> Option<Self>`; `ScoringConfidence::staleness_days_for_label(Option<&str>) -> i64`; `AccountInput.scoring_generation: String` (blob schema v3, holds the revision).

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

/// V2-01: the STORED stamp is the composite revision — the human generation
/// plus every in-binary model identity — so a model swap invalidates stored
/// scores automatically, without anyone remembering to bump.
#[test]
fn scoring_revision_changes_when_any_component_changes() {
    use charcoal::scoring::generation::{compose_revision, scoring_revision, SCORING_GENERATION};
    use charcoal::scoring::nli::NLI_MODEL_ID;
    use charcoal::topics::embeddings::EMBEDDING_MODEL_ID;
    use charcoal::toxicity::onnx::ONNX_MODEL_ID;
    let base = compose_revision(SCORING_GENERATION, ONNX_MODEL_ID, EMBEDDING_MODEL_ID, NLI_MODEL_ID);
    assert_eq!(scoring_revision(), base);
    assert_ne!(compose_revision("other-gen", ONNX_MODEL_ID, EMBEDDING_MODEL_ID, NLI_MODEL_ID), base);
    assert_ne!(compose_revision(SCORING_GENERATION, "other-onnx", EMBEDDING_MODEL_ID, NLI_MODEL_ID), base);
    assert_ne!(compose_revision(SCORING_GENERATION, ONNX_MODEL_ID, "other-emb", NLI_MODEL_ID), base);
    assert_ne!(compose_revision(SCORING_GENERATION, ONNX_MODEL_ID, EMBEDDING_MODEL_ID, "other-nli"), base);
    assert_ne!(scoring_revision(), "legacy");
    assert!(!scoring_revision().contains(char::is_whitespace), "bound into SQL equality and shown in the UI");
    assert_eq!(base.matches('|').count(), 3, "delimited so component sets cannot collide by concatenation");
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

Run: `cargo test --test unit_scoring generation`, `cargo test --test unit_scoring revision`, `cargo test --test unit_scoring label`, and `cargo test --features web --test unit_scan_phases another_generation`
Expected: compile errors (module, constant, field and method do not exist) — this step is expected to fail at compile time.

- [ ] **Step 3: Implement**

Create `src/scoring/generation.rs`:

```rust
//! The scoring revision stamp (#343 §4.4, #344).
//!
//! Every `account_scores` row records the *revision* it was scored under:
//! the human-bumped [`SCORING_GENERATION`] composed with every in-binary model
//! identity. A row is *fresh* only if its stamp equals [`scoring_revision()`]
//! **and** its `valid_until` is in the future; anything else is hidden from
//! tier lists and re-scored by the refresh job (High/Elevated) or on the
//! account's next re-engagement.
//!
//! # Why a composite
//!
//! Model swaps change scores: a different toxicity, embedding or NLI model
//! makes yesterday's numbers incomparable with today's even when the formula
//! is untouched. Composing the identities in means a swap expires stored
//! scores by itself — nobody has to remember (V2-01). The caches are NOT
//! affected: `onnx_scores` and `classifier_verdicts` are keyed by their own
//! model ids and stay reusable across a revision change.
//!
//! # When to bump [`SCORING_GENERATION`] by hand
//!
//! - the threat formula or its weights (`src/scoring/threat.rs`)
//! - the topic-overlap math or the fingerprint JSON format (`src/topics/`)
//! - a scoring-policy change (tier thresholds, abstention rules)
//! - a **classifier** model or policy change (CoPE-B / Zentropi): the
//!   classifier lives outside this binary, so its identity is not composed
//!   in; the runbook's deploy checklist covers it. Staged verdicts from the
//!   old classifier are rejected at finalize regardless (model_id + policy).
//!
//! Bumping is a code change, not a config knob: two replicas disagreeing on
//! the revision would hide each other's scores. The value is opaque; the
//! date form of the generation is for humans reading
//! `SELECT scoring_generation, COUNT(*) …`.
//!
//! # Rolling deploys
//!
//! A revision change ships as a single-replica deploy. During the seconds
//! both binaries run, the old one may still stamp its in-flight scan's rows
//! with the old revision; the new binary hides those rows and the refresh
//! job re-scores the High/Elevated ones. Staged work the old binary left
//! behind carries the old revision in `scan_state.scan_run_generation` and
//! is discarded on the next run (`pipeline::scan_phases::RunIdentity`).
use std::sync::LazyLock;

pub const SCORING_GENERATION: &str = "2026-09-13";

/// The stamp migration v18 writes onto rows scored before revisions
/// existed. Never equal to [`scoring_revision()`].
pub const LEGACY_GENERATION: &str = "legacy";

/// Pure composition, so tests can build alternative revisions. `|` is the
/// delimiter; no identity contains it (asserted by the unit test).
pub fn compose_revision(generation: &str, onnx: &str, embedding: &str, nli: &str) -> String {
    debug_assert!(![generation, onnx, embedding, nli].iter().any(|s| s.contains('|')));
    format!("{generation}|onnx={onnx}|emb={embedding}|nli={nli}")
}

static SCORING_REVISION: LazyLock<String> = LazyLock::new(|| {
    compose_revision(
        SCORING_GENERATION,
        crate::toxicity::onnx::ONNX_MODEL_ID,
        crate::topics::embeddings::EMBEDDING_MODEL_ID,
        crate::scoring::nli::NLI_MODEL_ID,
    )
});

/// The revision this binary scores under. Bound into every freshness query
/// and written onto every score row, staged blob and run marker.
pub fn scoring_revision() -> &'static str {
    SCORING_REVISION.as_str()
}
```

`src/scoring/mod.rs`: `pub mod generation;`. `src/scoring/nli.rs`, near the model doc at `:71`: `pub const NLI_MODEL_ID: &str = "nli-deberta-v3-xsmall-fp32";` (the fp32 export — see CLAUDE.md on #231). `src/topics/embeddings.rs`, after `EMBEDDING_DIM`:

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
    /// The `scoring_revision()` this blob was gathered under (schema v3,
    /// #344 R03). Phase C refuses to finalize a blob from another revision:
    /// its target embedding, sample selection and fingerprint quality were
    /// computed against inputs the current formula may not accept.
    pub scoring_generation: String,
```

`gather.rs:511`: `scoring_generation: crate::scoring::generation::scoring_revision().to_string(),` in the `AccountInput { … }` literal. `finalize.rs`, after the schema-version check (`:98-108`):

```rust
    if blob.scoring_generation != crate::scoring::generation::scoring_revision() {
        warn!(
            account_did,
            blob_generation = %blob.scoring_generation,
            current = crate::scoring::generation::scoring_revision(),
            "AccountInput scoring_generation mismatch — clearing staging and re-gathering"
        );
        db.clear_account_staging(user_did, account_did).await?;
        return Ok(FinalizeOutcome::NeedsRegather);
    }
```

Every other `AccountInput { … }` literal in tests (`rg -n "AccountInput \{" src tests`) gains `scoring_generation: scoring_revision().to_string()`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --test unit_scoring` then `VERIFY_WEB` (the blob shape change touches model-gated tests).
Expected: all pass, zero `SKIP:`.

- [ ] **Step 5: Commit**

```bash
git add src/scoring/generation.rs src/scoring/mod.rs src/scoring/nli.rs src/topics/embeddings.rs src/db/models.rs src/pipeline/scan_phases/staging.rs src/pipeline/scan_phases/gather.rs src/pipeline/scan_phases/finalize.rs tests/unit_scoring.rs tests/unit_scan_phases.rs
git commit -m 'feat(344): scoring_revision() = generation + model ids; EMBEDDING/NLI model ids; staged blobs carry the revision

The composite stamp every score row will carry (#343 §4.4, V2-01) so a
model swap expires stored scores by itself, the embedding and NLI model
identities, and blob schema v3 so Phase C refuses to finalize inputs
gathered under another revision.

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
- `scan_queue.kind TEXT NOT NULL DEFAULT 'full'` (Postgres `CHECK (kind IN ('full','refresh'))`); `scan_queue.full_requested_at` (`TEXT`/`TIMESTAMPTZ` nullable) — the owed-full obligation (V3-03); `scan_queue.completion TEXT` nullable (Postgres `CHECK (completion IN ('complete','complete_with_skips','complete_unverified','resumable','failed'))`) — how the last finished attempt ended (V3-02), written by `finish_queued_scan`.
- `users.next_refresh_at` (`TEXT` RFC3339 / `TIMESTAMPTZ`, nullable); `users.refreshed_generation TEXT` nullable — the revision under which this user's High/Elevated set was last *proven* (a completed refresh or full scan); `users.refresh_attempted_generation TEXT` nullable — the revision for which the tick last *claimed* an attempt (V2-03). NULL after migration ⇒ due on the first tick; a failed attempt leaves `refreshed_generation` old but `refresh_attempted_generation` current, so the next attempt waits for `next_refresh_at`.
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
            ("scan_queue", "completion"),
            ("users", "next_refresh_at"),
            ("users", "refreshed_generation"),
            ("users", "refresh_attempted_generation"),
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
        // An AUTHENTIC v17 database: the migrations up to and including 17,
        // nothing reconstructed by dropping columns (V2-07).
        let conn = Connection::open_in_memory().unwrap();
        create_tables_through(&conn, 17).unwrap();
        let max: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(max, 17, "fixture is genuinely at v17 before the upgrade");
        assert!(!has_column(&conn, "account_scores", "valid_until"));
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

        let (next, refreshed, attempted): (Option<String>, Option<String>, Option<String>) = conn
            .query_row(
                "SELECT next_refresh_at, refreshed_generation, refresh_attempted_generation FROM users WHERE did = 'did:plc:scored'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert!(next.is_none() && refreshed.is_none() && attempted.is_none(), "due-ness comes from the NULL attempted generation, not a stamped time");

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

Change both `(1..=17)` assertions and their comments to 18. `create_tables_through` does not exist yet — add it in Step 3 (it is the test-support entry point every v17 fixture in this plan uses):

```rust
/// Apply migrations up to and including `max_version`. Test support for
/// building an AUTHENTIC older-schema database (V2-07): a fixture made by
/// dropping the newest columns from a current database is not the old
/// schema (the other new columns are still there and the migration
/// re-adding them fails on duplicates). Production always calls
/// `create_tables`, which is `create_tables_through(conn, i64::MAX)`.
pub fn create_tables_through(conn: &Connection, max_version: i64) -> Result<()>
```

Implement it by threading `max_version` into `run_migration` (skip when `version > max_version`) and making `create_tables` delegate.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib db::schema::tests`
Expected: compile error on `create_tables_through` (expected at compile time). After adding the helper but before the migration: the two v18 tests fail with "no such column" at runtime, and the two version-list tests fail on `17 != 18`.

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
    // users.refresh_attempted_generation / refreshed_generation are left
    // NULL: the tick treats "has scores AND refresh_attempted_generation IS
    // NOT the current revision" as due, which makes this deploy AND every
    // later revision change refresh users promptly, once, without a one-off
    // time stamp (R07, V2-03). refreshed_generation is the proof written
    // only by a completed refresh or full scan.
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
             ALTER TABLE scan_queue ADD COLUMN completion TEXT;
             ALTER TABLE users ADD COLUMN next_refresh_at TEXT;
             ALTER TABLE users ADD COLUMN refreshed_generation TEXT;
             ALTER TABLE users ADD COLUMN refresh_attempted_generation TEXT;
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
-- binary's scoring_revision() AND valid_until > NOW(). Pre-existing rows are
-- stamped 'legacy' (hidden at once) with valid_until = scored_at + 14 d so
-- the column can be NOT NULL. The DEFAULT is dropped after the backfill on
-- purpose: a writer that forgets the stamp must fail, not write 'legacy'.
--
-- users.refresh_attempted_generation / refreshed_generation stay NULL: the
-- tick treats a user with scores whose refresh_attempted_generation differs
-- from the current revision as due, so this deploy AND every later revision
-- change refresh users promptly, once (R07, V2-03). refreshed_generation is
-- the proof a completed refresh/full scan writes. No one-off time stamp.
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
ALTER TABLE scan_queue ADD COLUMN IF NOT EXISTS completion TEXT
    CHECK (completion IN ('complete', 'complete_with_skips', 'complete_unverified', 'resumable', 'failed'));

ALTER TABLE users ADD COLUMN IF NOT EXISTS next_refresh_at TIMESTAMPTZ;
ALTER TABLE users ADD COLUMN IF NOT EXISTS refreshed_generation TEXT;
ALTER TABLE users ADD COLUMN IF NOT EXISTS refresh_attempted_generation TEXT;

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

(replace the existing `database_url` body; local runs without the variable still skip.) Destructive fixtures get their **own database** (V3-06): add `migrations_database_url()` next to it, reading `DATABASE_URL_MIGRATIONS` with the same CI panic, and `pub async fn migrate_postgres_through(url: &str, max_version: i64) -> Result<()>` in `src/db/postgres.rs` that (1) refuses — `bail!` — unless the URL's database name ends in `_migrations`, (2) drops every table in that database, (3) runs the embedded migrations up to `max_version`. Test support, documented as such. The ordinary suite never opens that database, so it cannot observe the resets; the destructive tests **among themselves** are serialized (V4-05) by `migrations_fixture()` in `tests/db_postgres.rs`: it takes a process-local `migrations_db_lock()` (a `tokio::sync::Mutex<()>` in a `OnceLock`, like the existing locks), opens a dedicated connection and runs `SELECT pg_advisory_lock(hashtext('charcoal_migrations_fixture'))` on it (released when the connection drops at the end of the test — a session lock, so a second test binary or an overlapping CI invocation blocks rather than interleaves), then calls `migrate_postgres_through`. The drop-all happens at test **start** under both locks, so a failed test leaves nothing for the next to trip over. Both destructive tests (`test_pg_migration_v18_upgrades_from_v17`, `test_pg_migrate_from_sqlite_preserves_every_row`) go through `migrations_fixture()`; a third test asserts that two concurrent `migrations_fixture()` calls in one process run strictly one after the other (the second observes the first's tables gone and its own fixture in place). Then append the two v18 tests: the fresh-DB one as before against `DATABASE_URL`; the upgrade one against `DATABASE_URL_MIGRATIONS`: `migrate_postgres_through(&murl, 17)` (assert `MAX(version) = 17` and that `account_scores.valid_until` is absent in `information_schema.columns`), seed the same fixture rows as the SQLite test (`did:plc:v18scored` / `did:plc:v18unscored` / `did:plc:v18acct` / `did:plc:v18na`, a done full queue row, two fingerprints), `connect_postgres(&murl)` (v18 applies), and assert: `scoring_generation = 'legacy'`, `valid_until` = `scored_at + 14 d`, `is_nullable = 'NO'` for `valid_until`, `kind = 'full'`, `completion IS NULL`, `next_refresh_at`/`refreshed_generation`/`refresh_attempted_generation` all NULL, `embedding_model_id` set only where a vector exists, the `scan_state` marker equal to the done row's `finished_at` rendered RFC3339, `idx_account_scores_user_score` present, version 18 recorded exactly once. Add `migrate_postgres_through_refuses_a_non_migrations_database` asserting the `bail!` on `DATABASE_URL`.

- [ ] **Step 6: Run**

Run: `VERIFY_PG` (whole file — the upgrade test drops columns other tests use; the lock serialises them).
Expected: all pass.

- [ ] **Step 7: Commit**

```bash
git add src/db/schema.rs migrations/postgres/0018_score_expiry.sql src/db/postgres.rs tests/db_postgres.rs
git commit -m 'feat(344): migration v18 — expiry, queue kind + full_requested_at, refresh schedule, embedding model id, cooldown backfill

account_scores.scoring_generation/valid_until (legacy backfill), the
(user_did, threat_score) index, scan_queue.kind/full_requested_at,
users.next_refresh_at/refreshed_generation/refresh_attempted_generation
(left NULL: due-ness is revision-driven), topic_fingerprint.embedding_model_id,
create_tables_through / migrate_postgres_through for authentic old-schema
fixtures, and
scan_state.last_full_scan_finished_at from done queue rows. Fresh +
upgrade-from-v17 tests on both backends; Postgres suite fails in CI
without DATABASE_URL.

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 3: Stamp on write, null-safe fresh reads, lossless export/import, discovery gates

**Why:** The heart of #344, corrected for the review findings: the SQLite predicate must be boolean-explicit so malformed expiries count as expired everywhere (R11); `charcoal migrate` must copy every row rather than the fresh-only presentation set (R01), with a defined conversion for SQLite's NULL/malformed expiries into Postgres's NOT NULL column and a stated precision contract (V2-06), proven on the real SQLite → Postgres path from an authentic v17 fixture (V2-07); and topic-first discovery must use the fresh set like every other gate (R06).

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
- In `models.rs`:
  ```rust
  /// How a row's expiry left its backend (V2-06). Postgres rows are always `At`.
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum ExportedExpiry { At(String) /* RFC3339 UTC, fractional seconds kept */, Missing, Invalid(String) /* the raw SQLite text */ }
  #[derive(Debug, Clone, PartialEq)]
  pub struct StoredScore { pub score: AccountScore, pub scored_at: String /* RFC3339 UTC */, pub scoring_generation: String, pub valid_until: ExportedExpiry }
  ```
  Import rule: `At(t)` → `t`; `Missing`/`Invalid(_)` → `valid_until = scored_at` on both backends (expired the instant it was scored; never renewed; the raw text is `warn!`-logged with the DID, not stored). Precision: `At` carries whatever the source had (`%f` milliseconds on SQLite, microseconds on Postgres); Postgres stores it in full; SQLite's `datetime()` column form truncates to whole seconds — a documented, tested conversion.
- `get_ranked_threats` and `count_not_assessed` become fresh-only (signatures unchanged). `upsert_account_score` stamps both columns from the clock (the scoring path); `import_score` never does.

- [ ] **Step 1: Rewrite `tests/unit_staleness.rs`**

```rust
// Score freshness (#213 Task 5, redefined by #344 / #343 §4.4).
//
// FRESH = scoring_generation == scoring_revision() AND valid_until is a
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
use charcoal::scoring::generation::{scoring_revision, LEGACY_GENERATION};
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
    insert_score(&conn, "did:plc:current", 5, scoring_revision());
    insert_score(&conn, "did:plc:expired", -1, scoring_revision());
    insert_score(&conn, "did:plc:legacy", 5, LEGACY_GENERATION);
    insert_score(&conn, "did:plc:oldgen", 5, "1999-01-01");
    insert_raw(&conn, "did:plc:nullvalid", "NULL", scoring_revision());
    insert_raw(&conn, "did:plc:malformed", "'not a timestamp'", scoring_revision());
    // Exact boundary: valid_until == now is NOT fresh (strict >).
    insert_raw(&conn, "did:plc:boundary", "datetime('now')", scoring_revision());

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
        params![scoring_revision()],
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
        assert_eq!(generation, scoring_revision(), "{did}");
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
    insert_score(&conn, "did:plc:current", 5, scoring_revision());
    insert_score(&conn, "did:plc:expired", -1, scoring_revision());
    insert_score(&conn, "did:plc:legacy", 5, LEGACY_GENERATION);
    for (did, days) in [("did:plc:na-expired", "-1"), ("did:plc:na-fresh", "+1")] {
        conn.execute(
            "INSERT INTO account_scores (user_did, did, handle, threat_tier, scoring_generation, valid_until)
             VALUES (?1, ?2, 'na.handle', 'NotAssessed', ?3, datetime('now', ?4 || ' days'))",
            params![USER, did, scoring_revision(), days],
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

Create `tests/unit_score_export.rs` (SQLite → SQLite; the SQLite → Postgres path is in Step 7):

```rust
// #344 R01 / V2-06 / V2-07: `charcoal migrate` must be lossless. Presentation
// queries hide expired/legacy rows; export/import carry every row with its
// original scored_at, revision and expiry — including SQLite's NULL and
// malformed expiries, which import as "expired when scored" — and importing
// never renews anything. Fixtures are relative to now, never calendar dates.

use charcoal::db::models::{ExportedExpiry, StoredScore};
use charcoal::db::schema::{create_tables, create_tables_through};
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::scoring::generation::{scoring_revision, LEGACY_GENERATION};
use rusqlite::{params, Connection};
use std::sync::Arc;

const USER: &str = "did:plc:exportuser00000000000000";

/// current (+14 d), expired (−6 d), legacy, NotAssessed (NULL score),
/// NULL expiry, malformed expiry.
fn seeded() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn).unwrap();
    let rev = scoring_revision();
    let rows = [
        ("did:plc:cur", "40.0", "'High'", "datetime('now', '-4 days')", "datetime('now', '+10 days')", rev),
        ("did:plc:exp", "20.0", "'Elevated'", "datetime('now', '-13 days')", "datetime('now', '-6 days')", rev),
        ("did:plc:leg", "50.0", "'High'", "datetime('now', '-140 days')", "datetime('now', '-126 days')", LEGACY_GENERATION),
        ("did:plc:na", "NULL", "'NotAssessed'", "datetime('now', '-12 days')", "datetime('now', '-5 days')", rev),
        ("did:plc:nul", "30.0", "'Elevated'", "datetime('now', '-2 days')", "NULL", rev),
        ("did:plc:bad", "35.0", "'High'", "datetime('now', '-2 days')", "'not a timestamp'", rev),
    ];
    for (did, score, tier, scored_at, valid_until, generation) in rows {
        conn.execute(
            &format!(
                "INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at, scoring_generation, valid_until)
                 VALUES (?1, ?2, ?3, {score}, {tier}, {scored_at}, ?4, {valid_until})"
            ),
            params![USER, did, format!("{did}.h"), generation],
        )
        .unwrap();
    }
    conn
}

fn by_did<'a>(rows: &'a [StoredScore], did: &str) -> &'a StoredScore {
    rows.iter().find(|r| r.score.did == did).expect(did)
}

#[tokio::test]
async fn export_returns_every_row_with_its_provenance() {
    let db: Arc<dyn Database> = Arc::new(SqliteDatabase::new(seeded()));
    let rows = db.export_scores(USER).await.unwrap();
    assert_eq!(rows.len(), 6, "expired, legacy, NULL-score, NULL-expiry and malformed rows are all exported");
    assert_eq!(by_did(&rows, "did:plc:leg").scoring_generation, LEGACY_GENERATION);
    assert!(by_did(&rows, "did:plc:na").score.threat_score.is_none());
    assert_eq!(by_did(&rows, "did:plc:nul").valid_until, ExportedExpiry::Missing);
    assert_eq!(by_did(&rows, "did:plc:bad").valid_until, ExportedExpiry::Invalid("not a timestamp".to_string()));
    assert!(matches!(by_did(&rows, "did:plc:cur").valid_until, ExportedExpiry::At(_)));
    // RFC3339 with the millisecond field SQLite can render.
    assert!(by_did(&rows, "did:plc:cur").scored_at.ends_with("+00:00"));
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
    for r in &rows {
        dst.import_score(USER, r).await.unwrap(); // idempotent, not a renewal
    }
    let back = dst.export_scores(USER).await.unwrap();

    // Well-formed rows round-trip exactly (SQLite → SQLite: whole seconds in,
    // whole seconds out).
    for did in ["did:plc:cur", "did:plc:exp", "did:plc:leg", "did:plc:na"] {
        assert_eq!(by_did(&rows, did), by_did(&back, did), "{did}");
    }
    // NULL / malformed expiries import as "expired when scored": the
    // destination holds a real timestamp equal to scored_at.
    for did in ["did:plc:nul", "did:plc:bad"] {
        let imported = by_did(&back, did);
        assert_eq!(imported.valid_until, ExportedExpiry::At(imported.scored_at.clone()), "{did}");
        assert_eq!(imported.scored_at, by_did(&rows, did).scored_at, "{did} scored_at untouched");
    }
    // Presentation agrees on both sides: one visible row, five expired.
    assert_eq!(dst.get_ranked_threats(USER, 0.0).await.unwrap().len(), 1);
    assert_eq!(dst.count_expired(USER).await.unwrap(), 5);
    assert_eq!(src.count_expired(USER).await.unwrap(), 5);
}

/// A genuinely v17 database (migrations through 17 only), opened by this
/// binary: v18 stamps 'legacy' + scored_at + 14 d, and the export/import
/// path carries exactly that into a fresh database.
#[tokio::test]
async fn v17_fixture_rows_survive_open_export_import() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables_through(&conn, 17).unwrap();
    let max: i64 = conn.query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0)).unwrap();
    assert_eq!(max, 17);
    conn.execute(
        "INSERT INTO account_scores (user_did, did, handle, threat_score, threat_tier, scored_at)
         VALUES (?1, 'did:plc:v17', 'v17.h', 40.0, 'High', datetime('now', '-30 days'))",
        params![USER],
    )
    .unwrap();
    create_tables(&conn).unwrap(); // the binary opens it: v18 applies
    let src: Arc<dyn Database> = Arc::new(SqliteDatabase::new(conn));

    let rows = src.export_scores(USER).await.unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.scoring_generation, LEGACY_GENERATION);
    let ExportedExpiry::At(valid_until) = &row.valid_until else { panic!("backfilled expiry") };
    let scored = chrono::DateTime::parse_from_rfc3339(&row.scored_at).unwrap();
    let valid = chrono::DateTime::parse_from_rfc3339(valid_until).unwrap();
    assert_eq!(valid - scored, chrono::Duration::days(14));

    let dst_conn = Connection::open_in_memory().unwrap();
    create_tables(&dst_conn).unwrap();
    let dst: Arc<dyn Database> = Arc::new(SqliteDatabase::new(dst_conn));
    dst.import_score(USER, row).await.unwrap();
    assert_eq!(dst.export_scores(USER).await.unwrap(), rows);
    assert!(dst.is_score_stale(USER, "did:plc:v17").await.unwrap(), "legacy stays hidden after import");
    assert_eq!(dst.count_expired(USER).await.unwrap(), 1);
}
```

`AccountScore` and `ToxicPost` need `PartialEq` (add if missing). Timestamps are exported as RFC3339 UTC on both backends: SQLite with `strftime('%Y-%m-%dT%H:%M:%f+00:00', col)` (milliseconds — the most SQLite can render; its stored form is whole seconds anyway), Postgres with `to_char(col AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"+00:00"')` (microseconds). Import writes each backend's native form: `datetime(?)` on SQLite (whole seconds — the truncation V2-06 asks to be explicit about) and `$n::timestamptz` on Postgres (full precision).

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --test unit_staleness` and `cargo test --test unit_score_export`
Expected: compile errors (new methods/types missing) — expected at compile time for this step.

- [ ] **Step 4: Trait, models, SQLite**

`src/db/models.rs`:

```rust
/// One `account_scores` row with its provenance, for lossless export/import
/// (#344 R01). The presentation reads (`get_ranked_threats`, counts) hide
/// expired rows; this does not, and `import_score` writes it back verbatim.
/// How a row's expiry left its backend (V2-06). Postgres rows are always
/// `At`; SQLite can hold NULL or text `datetime()` cannot parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportedExpiry {
    /// RFC3339 UTC, fractional seconds as the source had them.
    At(String),
    Missing,
    /// The raw SQLite text, for the log line the importer writes.
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredScore {
    pub score: AccountScore,
    /// RFC3339 UTC.
    pub scored_at: String,
    pub scoring_generation: String,
    pub valid_until: ExportedExpiry,
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
        params![user_did, did, scoring_revision()],
        |row| row.get(0),
    )?;
    Ok(fresh_rows == 0)
}

pub fn get_fresh_scored_dids(conn: &Connection, user_did: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT did FROM account_scores WHERE user_did = ?1 AND {}", fresh_sql("?2")
    ))?;
    let dids = stmt
        .query_map(params![user_did, scoring_revision()], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(dids)
}

pub fn count_expired(conn: &Connection, user_did: &str) -> Result<i64> {
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM account_scores WHERE user_did = ?1 AND NOT ({})", fresh_sql("?2")),
        params![user_did, scoring_revision()],
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
        params![user_did, scoring_revision()],
        |row| row.get(0),
    )?;
    Ok(count)
}
```

`get_ranked_threats`: `WHERE user_did = ?1 AND threat_score >= ?2 AND {fresh_sql("?3")} ORDER BY threat_score DESC, did` with `params![user_did, min_score, scoring_revision()]` (the `did` tie-breaker makes paging deterministic — #356 will rely on it).

`upsert_account_score`: as in the first draft — add `scoring_generation` (`?16` = `scoring_revision()`) and `valid_until = datetime('now', ?17)` (`?17` = `format!("+{} days", ScoringConfidence::staleness_days_for_label(score.scoring_confidence.as_deref()))`) to both the INSERT and the `DO UPDATE SET` lists.

Export/import (SQLite):

```rust
/// Every row, verbatim, RFC3339 timestamps. See `Database::export_scores`.
pub fn export_scores(conn: &Connection, user_did: &str) -> Result<Vec<StoredScore>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, toxicity_score, topic_overlap, threat_score, threat_tier,
                posts_analyzed, top_toxic_posts, behavioral_signals, graph_distance,
                fingerprint_quality, scoring_confidence, context_score, overlap_legacy,
                strftime('%Y-%m-%dT%H:%M:%f+00:00', scored_at),
                scoring_generation,
                strftime('%Y-%m-%dT%H:%M:%f+00:00', valid_until),
                valid_until
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
                // strftime() of NULL is NULL; of unparseable text is NULL too —
                // the raw column (17) tells the two apart (V2-06).
                valid_until: match (row.get::<_, Option<String>>(16)?, row.get::<_, Option<String>>(17)?) {
                    (Some(t), _) => ExportedExpiry::At(t),
                    (None, None) => ExportedExpiry::Missing,
                    (None, Some(raw)) => ExportedExpiry::Invalid(raw),
                },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Write an exported row back. `datetime(?)` normalises the RFC3339 text to
/// this backend's `YYYY-MM-DD HH:MM:SS` form (whole seconds — SQLite's
/// column form; a Postgres source's microseconds are truncated here, and
/// only here). A `Missing`/`Invalid` expiry becomes `scored_at`: expired the
/// instant it was scored, never renewed, never dropped (V2-06). Idempotent.
pub fn import_score(conn: &Connection, user_did: &str, row: &StoredScore) -> Result<()> {
    let s = &row.score;
    let top_posts_json = serde_json::to_string(&s.top_toxic_posts)?;
    let valid_until = match &row.valid_until {
        ExportedExpiry::At(t) => t.clone(),
        ExportedExpiry::Missing => row.scored_at.clone(),
        ExportedExpiry::Invalid(raw) => {
            tracing::warn!(did = %s.did, raw, "invalid expiry on export — importing as expired-when-scored");
            row.scored_at.clone()
        }
    };
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
            row.scoring_generation, valid_until,
        ],
    )?;
    Ok(())
}
```

(`top_toxic_posts` corruption-as-empty is a known defect tracked in #364; export inherits it and the CHANGELOG says so.) `fingerprint_embedding_model`: `SELECT embedding_model_id FROM topic_fingerprint WHERE user_did = ?1` (`.optional()?.flatten()`). `save_fingerprint_bundle` writes the new column. `src/db/sqlite.rs`: delegating impls for all new/changed methods.

- [ ] **Step 5: Postgres**

Same predicate, natively boolean: `scoring_generation = $n AND valid_until > NOW()` in `is_score_stale`, `get_fresh_scored_dids`, `count_expired` (`NOT (...)` — `valid_until` is NOT NULL on Postgres so no COALESCE is needed; say so in a comment), `count_not_assessed`, `get_ranked_threats` (+ `, did` tie-breaker). `upsert_account_score`: `$16` generation, `NOW() + make_interval(days => $17)`. `export_scores`: `to_char(scored_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"+00:00"')` and the same for `valid_until` (always `ExportedExpiry::At` — the column is NOT NULL). `import_score`: `$10::timestamptz` / `$18::timestamptz`, with the same `Missing`/`Invalid` → `scored_at` mapping and warn line as SQLite (a SQLite source can hand Postgres either). `fingerprint_embedding_model` and the bundle column.

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
            // mark_refreshed_generation sets attempted = refreshed; if the
            // source had an attempt pending under a different revision, copy
            // that too so the destination does not re-attempt at once.
            if let Some(a) = sqlite_db.refresh_attempted_generation(&did).await? {
                pg_db.mark_refresh_attempted_generation(&did, &a).await?;
            }
```

(`next_refresh_at`, `refreshed_generation`, `refresh_attempted_generation`, `schedule_refresh`, `mark_refreshed_generation`, `mark_refresh_attempted_generation` are Task 6 trait methods — the last is a plain `UPDATE users SET refresh_attempted_generation = ?2 WHERE did = ?1`, used only by migrate; Task 6 lands before the migrate change is compiled — do this part of Task 3 as its final step after Task 6, or stub the four in Task 3 with the exact signatures Task 6 specifies.) The fingerprint step passes `Some(EMBEDDING_MODEL_ID)` when it migrates an embedding and copies `embedding_model_id` from the source via `fingerprint_embedding_model` if present.

Fix the existing tests to the new signatures (`rg -n "is_score_stale\(|get_fresh_scored_dids\(|save_fingerprint_bundle\(" src tests`).

- [ ] **Step 7: Postgres tests**

Append to `tests/db_postgres.rs`:
- the freshness twin (rows `pgfresh_current` +5 d / `pgfresh_expired` −1 d / `pgfresh_legacy` with `'legacy'`; the exact boundary via `NOW() - INTERVAL '1 second'` → stale); `count_expired` = 3 of 4; the write-path stamp twin (`EXTRACT(EPOCH …)/86400.0)::float8`);
- **`test_pg_migrate_from_sqlite_preserves_every_row` — the real path (V2-06, V2-07):** build the SQLite source exactly as `unit_score_export::seeded()` does but from a `create_tables_through(&conn, 17)` fixture for the legacy row and a v18 database for the others (so the source holds current, expired, legacy, NotAssessed, NULL-expiry and malformed-expiry rows), open it as `SqliteDatabase`, then run the same sequence `charcoal migrate` runs (`export_scores` on the source → `import_score` on `PgDatabase` for `did:plc:pgmig_user`). Assert with **direct SQL on Postgres**, not a re-export: 6 rows; `scoring_generation` per row; `threat_score IS NULL` for the NotAssessed row; `valid_until = scored_at` for the NULL and malformed rows; `valid_until - scored_at = INTERVAL '14 days'` for the legacy row; and `to_char(scored_at, …)` equal to the SQLite `strftime` of the same row (whole seconds — the source had no fraction). Then `count_expired` = 5 and `get_ranked_threats` = 1 on Postgres. Repeat the import loop; assert every `scored_at`/`valid_until` is unchanged;
- **`test_pg_export_import_keeps_microseconds`:** insert a row with `scored_at = '2026-09-01 12:00:00.123456+00'` and `valid_until = scored_at + INTERVAL '14 days'`, export, import under a second DID, and assert with direct SQL that both columns match to the microsecond;
- **`test_pg_import_into_sqlite_truncates_to_seconds`:** export that microsecond row from Postgres, import into an in-memory SQLite, and assert the stored value is `2026-09-01 12:00:00` — the documented one-way conversion.

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
- `enqueue_scan(user)` (full): every branch records the obligation `full_requested_at = COALESCE(full_requested_at, now)` (V3-03). Re-queues a `done`/`failed` row as `full` with `enqueued_at = now`; upgrades a `queued refresh` row to `full` **keeping `enqueued_at`** (the position the user already held); on a `running refresh` row records the obligation and changes nothing else; no-op on `queued full` / `running full` (obligation already recorded). Returns `EnqueueOutcome { Queued, AlreadyQueued, AlreadyRunning, QueuedAfterRefresh }` so the handler can say which.
- `enqueue_refresh_scan(user)`: re-queues a `done`/`failed` row — as `full` if `full_requested_at` is set (owed work), else as `refresh`; never touches `queued`/`running` rows. (The tick uses the same conditional statement, Task 6.)
- `finish_queued_scan(user, claim, completion: FinishCompletion, error)`: writes `completion`; for a `refresh` row with `full_requested_at` set, the row becomes `queued`, `kind = 'full'`, `enqueued_at = full_requested_at`, **`full_requested_at` kept**, `started_at/finished_at/lease_expires/claim_id = NULL`, `last_error = error`; for a `full` row finishing `Complete`/`CompleteWithSkips`/`CompleteUnverified`, `full_requested_at = NULL` (obligation fulfilled — V6-01); a `full` row finishing `Resumable`/`Failed` keeps it. Otherwise as today. Returns the existing `bool`.
- Admission order unchanged: `(enqueued_at, user_did)`.
- `scan_queue_entry`'s median: `kind = 'full'` rows with `completion = 'complete'` only.
- Cooldown: `trigger_scan` anchors **only** on `scan_state.last_full_scan_finished_at` (backfilled by v18 from historical done rows, written by `record_full_scan_completion` for `Complete` and `CompleteWithSkips`). The queue row is never consulted (V3-02).

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
pub struct ScanQueueRow { …existing…, pub kind: ScanKind, pub full_requested_at: Option<String>, pub completion: Option<FinishCompletion> }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishCompletion { Complete, CompleteWithSkips, CompleteUnverified, Resumable, Failed }  // as_str: complete | complete_with_skips | complete_unverified | resumable | failed
async fn enqueue_scan(&self, user_did: &str) -> Result<EnqueueOutcome>;
async fn enqueue_refresh_scan(&self, user_did: &str) -> Result<()>;
async fn finish_queued_scan(&self, user_did: &str, claim_id: &str, completion: FinishCompletion, error: Option<&str>) -> Result<bool>;
// src/web/scan_job.rs
pub(crate) async fn record_full_scan_completion(db: &dyn Database, user_did: &str, completion: ScanCompletion); // marker for Complete/CompleteWithSkips; best-effort
```
`scan_state` key `last_full_scan_finished_at` (RFC3339), written by `record_full_scan_completion` only for `Complete`/`CompleteWithSkips`.

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
use charcoal::db::{Database, EnqueueOutcome, FinishCompletion, ScanKind};
use rusqlite::{params, Connection};

const USER: &str = "did:plc:kindtest0000000000000000";

/// One High row so `USER` is refresh-eligible where a test needs the tick.
async fn seed_one_score(db: &SqliteDatabase, user: &str) {
    let mut s = charcoal::db::models::AccountScore::default_for_test("did:plc:seed");
    s.threat_score = Some(60.0);
    s.threat_tier = Some("High".into());
    db.upsert_account_score(user, &s).await.unwrap();
}

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
    // dated from the request, not from now. The obligation stays recorded
    // until a full scan COMPLETES (V3-03).
    assert!(db.finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Complete, None).await.unwrap());
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
    assert_eq!(r.enqueued_at, requested);
    assert_eq!(r.full_requested_at.as_deref(), Some(requested.as_str()), "obligation kept through the handover");
    let claim = db.claim_next_scan(1, 60).await.unwrap().expect("the full scan is admitted");
    assert_eq!(claim.kind, ScanKind::Full);
    assert!(db.finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Complete, None).await.unwrap());
    let r = row(&db, USER).await;
    assert!(r.full_requested_at.is_none(), "fulfilled");
    assert_eq!(r.completion, Some(FinishCompletion::Complete));
}

/// V3-03: the requested full scan survives an interrupted drain and runs
/// again automatically — no second click — and duplicate clicks in between
/// coalesce. A "restart" is a fresh SqliteDatabase over the same file.
#[tokio::test]
async fn a_requested_full_scan_survives_an_interrupted_drain_and_runs_without_another_click() {
    use charcoal::web::refresh::REFRESH_RETRY_HOURS;
    let file = tempfile::NamedTempFile::new().unwrap();
    let open = || {
        let conn = Connection::open(file.path()).unwrap();
        create_tables(&conn).unwrap();
        Arc::new(SqliteDatabase::new(conn))
    };
    let db = open();
    db.upsert_user(USER, "u.h").await.unwrap();
    seed_one_score(&db, USER).await; // a High row so the user is refresh-eligible
    // 1. A refresh is running; the user asks for a full scan.
    db.enqueue_refresh_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::QueuedAfterRefresh);
    assert_eq!(db.enqueue_scan(USER).await.unwrap(), EnqueueOutcome::QueuedAfterRefresh, "second click coalesces");
    let requested = row(&db, USER).await.full_requested_at.unwrap();
    // 2. Handover; the full scan is admitted and cost-caps while draining.
    db.finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Resumable, None).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    assert_eq!(claim.kind, ScanKind::Full);
    db.finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Resumable, None).await.unwrap();
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.completion), ("done", Some(FinishCompletion::Resumable)));
    assert_eq!(r.full_requested_at.as_deref(), Some(requested.as_str()), "still owed");
    let now = chrono::Utc::now();
    charcoal::web::refresh::schedule_retry(db.as_ref(), USER, now).await;
    // 3. Restart. The tick, after the deadline, re-queues OWED work as full.
    drop(db);
    let db = open();
    assert_eq!(charcoal::web::refresh::enqueue_due_refreshes(&db, now + chrono::Duration::seconds(30), std::time::Duration::from_secs(24 * 3600)).await, 0);
    assert_eq!(charcoal::web::refresh::enqueue_due_refreshes(&db, now + chrono::Duration::hours(REFRESH_RETRY_HOURS as i64 + 1), std::time::Duration::from_secs(24 * 3600)).await, 1);
    let r = row(&db, USER).await;
    assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full), "owed work is retried as a FULL scan, never a refresh");
    assert_eq!(r.full_requested_at.as_deref(), Some(requested.as_str()));
    // 4. This time it completes: the obligation is fulfilled.
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    db.finish_queued_scan(USER, &claim.claim_id, FinishCompletion::CompleteWithSkips, None).await.unwrap();
    let r = row(&db, USER).await;
    assert!(r.full_requested_at.is_none());
    assert_eq!(r.completion, Some(FinishCompletion::CompleteWithSkips));
}

#[tokio::test]
async fn a_failed_refresh_still_hands_over_to_the_requested_full_scan() {
    let db = db();
    db.enqueue_refresh_scan(USER).await.unwrap();
    let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
    db.enqueue_scan(USER).await.unwrap();
    assert!(db.finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Failed, Some("refresh blew up")).await.unwrap());
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
    db.finish_queued_scan(USER, &claim.claim_id, FinishCompletion::Complete, None).await.unwrap();
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

`finish_queued_scan` gains `completion: FinishCompletion` and its doc says: "Writes `completion`. A `refresh` row with `full_requested_at` set becomes a queued `full` row dated from the request, obligation kept. A `full` row finishing `Complete`/`CompleteWithSkips` clears `full_requested_at`; `Resumable`/`Failed` keep it (V3-03). The refresh's own outcome is recorded in `scan_state` by `run_refresh`." `src/db/mod.rs`: re-export `ScanKind`, `EnqueueOutcome`, `FinishCompletion`. Every existing caller of `finish_queued_scan` (`run_under_slot`, tests) passes a completion: `run_under_slot` maps `SlotExit::Completed` to the completion the scan future reported through the new `ScanReport { completion: FinishCompletion }` it returns (see Step 6), `Failed`/`Panicked` → `Failed`, and `Abandoned` never finishes the row (unchanged).

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
    // Every branch records the obligation (V3-03): the user asked for a full
    // scan, and only a full scan that COMPLETES may clear this.
    let outcome = match current.as_ref().map(|(s, k)| (s.as_str(), k.as_str())) {
        None | Some(("done", _)) | Some(("failed", _)) => {
            tx.execute(
                "INSERT INTO scan_queue (user_did, status, kind, enqueued_at, full_requested_at)
                 VALUES (?1, 'queued', 'full', ?2, ?2)
                 ON CONFLICT(user_did) DO UPDATE SET
                     status = 'queued', kind = 'full', enqueued_at = ?2,
                     started_at = NULL, finished_at = NULL, lease_expires = NULL,
                     last_error = NULL, claim_id = NULL, completion = NULL,
                     full_requested_at = COALESCE(scan_queue.full_requested_at, ?2)",
                params![user_did, now],
            )?;
            EnqueueOutcome::Queued
        }
        Some(("queued", "refresh")) => {
            // In place: the user keeps the position the refresh already held.
            tx.execute(
                "UPDATE scan_queue SET kind = 'full', full_requested_at = COALESCE(full_requested_at, ?2)
                 WHERE user_did = ?1",
                params![user_did, now],
            )?;
            EnqueueOutcome::Queued
        }
        Some(("queued", _)) => {
            tx.execute(
                "UPDATE scan_queue SET full_requested_at = COALESCE(full_requested_at, ?2) WHERE user_did = ?1",
                params![user_did, now],
            )?;
            EnqueueOutcome::AlreadyQueued
        }
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
        Some(("running", _)) => {
            tx.execute(
                "UPDATE scan_queue SET full_requested_at = COALESCE(full_requested_at, ?2) WHERE user_did = ?1",
                params![user_did, now],
            )?;
            EnqueueOutcome::AlreadyRunning
        }
        Some((other, _)) => anyhow::bail!("scan_queue.status holds an unknown value {other:?}"),
    };
    tx.commit()?;
    Ok(outcome)
}

/// Same statement the tick uses (`REFRESH_ENQUEUE_SQL`, Task 6): owed full
/// work is re-queued as full, otherwise a refresh; queued/running rows are
/// never touched.
pub fn enqueue_refresh_scan(conn: &Connection, user_did: &str) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(REFRESH_ENQUEUE_SQL, params![user_did, now])?;
    Ok(())
}
```

`finish_queued_scan`:

```rust
pub fn finish_queued_scan(
    conn: &Connection,
    user_did: &str,
    claim_id: &str,
    completion: FinishCompletion,
    error: Option<&str>,
) -> Result<bool> {
    let now = chrono::Utc::now().to_rfc3339();
    let status = if error.is_none() { "done" } else { "failed" };
    // Fulfilled = the user's request was carried out, verified or not (V6-01).
    let fulfilled = matches!(
        completion,
        FinishCompletion::Complete | FinishCompletion::CompleteWithSkips | FinishCompletion::CompleteUnverified
    );
    // One statement, one WHERE (status='running' AND claim_id) — the fencing
    // rule is unchanged. Three shapes, all decided from the row's PRE-update
    // values (SQLite evaluates every CASE against them):
    //   refresh + owed full  → hand over: queued full, obligation kept (R09)
    //   full + fulfilled     → done, obligation cleared (V3-03)
    //   anything else        → done/failed, obligation kept
    let changed = conn.execute(
        "UPDATE scan_queue
         SET status = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN 'queued' ELSE ?3 END,
             kind = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN 'full' ELSE kind END,
             enqueued_at = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN full_requested_at ELSE enqueued_at END,
             started_at = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE started_at END,
             finished_at = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE ?4 END,
             claim_id = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE claim_id END,
             completion = CASE WHEN kind = 'refresh' AND full_requested_at IS NOT NULL THEN NULL ELSE ?6 END,
             full_requested_at = CASE WHEN kind = 'full' AND ?7 THEN NULL ELSE full_requested_at END,
             lease_expires = NULL,
             last_error = ?5
         WHERE user_did = ?1 AND status = 'running' AND claim_id = ?2",
        params![user_did, claim_id, status, now, error, completion.as_str(), fulfilled],
    )?;
    Ok(changed > 0)
}
```

(SQLite evaluates every `CASE` against the row's pre-update values, so the repeated condition is consistent within the statement.) `claim_next_scan`: select `user_did, kind`, carry `kind` into `ScanClaim` via `ScanKind::from_str(&kind).with_context(...)?`. `list_scan_queue`: select `kind, full_requested_at, completion` and map them (an unknown kind or completion is an error, not a default). Median (`:1712-1716`): `AND kind = 'full' AND completion = 'complete'`. `src/db/sqlite.rs`: delegate; `enqueue_scan` now returns `EnqueueOutcome`.

- [ ] **Step 5: Postgres**

Same semantics, with one difference the SQLite immediate transaction hides (V4-03): `SELECT … FOR UPDATE` on an **absent** row locks nothing, so two first-time enqueues can race and the loser's `ON CONFLICT DO UPDATE` would reset a job the winner's worker has already claimed. Postgres `enqueue_scan` therefore starts with a per-user transaction advisory lock — `SELECT pg_advisory_xact_lock(hashtext('scan_queue:' || $1))` — before the state read (the same mechanism `claim_next_scan` already uses for admission), and its absent-row `INSERT … ON CONFLICT (user_did) DO UPDATE SET … WHERE scan_queue.status IN ('done','failed')` is conditional as defense in depth: if it affects 0 rows the function re-reads the row and returns the outcome for the state it actually found (`AlreadyQueued`/`AlreadyRunning`/`QueuedAfterRefresh`, with the obligation still recorded via the `COALESCE` update). Lock order is unchanged (advisory(user) → queue row; the tick takes users rows then queue rows and never the enqueue advisory lock; `finish_queued_scan` takes only the queue row) — no cycle. `enqueue_refresh_scan`: the INSERT … ON CONFLICT … WHERE `scan_queue.status IN ('done','failed')` with `kind = 'refresh'`. `finish_queued_scan`: the same `CASE` form with `$3::TEXT IS NULL` for the status, `NOW()` for `finished_at`, `$6` completion and `$7::boolean` fulfilled. `claim_next_scan`: `SELECT user_did, kind … FOR UPDATE SKIP LOCKED`. `list_scan_queue`: `kind`, `full_requested_at` (`to_rfc3339()` on the `Option<DateTime<Utc>>`). Median: `AND kind = 'full'`.

- [ ] **Step 6: Handlers and the marker**

`src/web/handlers/scan.rs` — the cooldown anchors on the completion marker **only** (V3-02). Replace the cooldown block (`:63-95`) with:

```rust
    // #258 cooldown, #344 R13/V3-02: the anchor is the completion marker
    // `record_full_scan_completion` writes for a fulfilled full scan. The
    // queue row is NOT consulted: since #344 it may be a refresh, and an
    // interrupted full attempt finishes `done` too — neither is a completed
    // full scan, and neither may block the user's immediate retry.
    let anchor = match state.db.get_scan_state(&auth.did, "last_full_scan_finished_at").await {
        Ok(m) => m,
        Err(e) => {
            // A cooldown is an abuse guard, not a correctness gate: on a DB
            // blip let the enqueue proceed rather than refuse service.
            tracing::warn!(error = %format!("{e:#}"), "cooldown marker unreadable — skipping the cooldown check");
            None
        }
    };
    if let Some(finished_at) = anchor {
        if let Some(retry_at) = cooldown_retry_at(&finished_at, chrono::Utc::now(), state.config.scan_cooldown_hours) {
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
```

(the `list_scan_queue` read the old block made for the cooldown is gone; the handler still reads the queue entry afterwards for the 202 body, as today). Replace the enqueue call (`:106`) with

```rust
    let outcome = match state.db.enqueue_scan(&auth.did).await {
        Ok(o) => o,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "enqueue failed");
            return api_error(StatusCode::SERVICE_UNAVAILABLE, "Could not queue the scan");
        }
    };
```

and add to the 202 body `"queued": match outcome { EnqueueOutcome::QueuedAfterRefresh => "after_refresh", EnqueueOutcome::AlreadyRunning => "already_running", _ => "now" }` with a comment that `after_refresh` means "a background refresh is running; your scan starts when it finishes" (the dashboard copy for it is #365's — leave the frontend reading `position`/`eta` as today). Composed tests (V3-02) in `tests/web_scan_queue.rs`, through the real slot lifecycle and the real handler — helper-level tests cannot see this defect:

```rust
/// Drive `run_under_slot` with a scan future that reports `completion` (and
/// writes the marker exactly as run_scan would), then POST /api/scan.
async fn finish_then_post(app: &axum::Router, db: &Arc<dyn Database>, did: &str, completion: ScanCompletion) -> StatusCode {
    db.enqueue_scan(did).await.unwrap();
    let claim = db.claim_next_scan(1, LEASE_SECS).await.unwrap().unwrap();
    let mgr = Arc::new(RwLock::new({ let mut m = ScanManager::new(); m.begin_admitted_scan(did, &claim.claim_id); m }));
    let slot = QueueSlot { claim_id: claim.claim_id.clone(), wake: tokio::sync::mpsc::channel(1).0 };
    let live = LiveScans::new().try_register(did).unwrap();
    let scan_db = db.clone();
    let scan_did = did.to_string();
    let exit = run_under_slot(
        async move {
            record_full_scan_completion(scan_db.as_ref(), &scan_did, completion).await;
            Ok(ScanReport { completion: completion.into() })
        },
        db.clone(), mgr, did.to_string(), slot, live, Duration::from_millis(10),
    ).await;
    assert_eq!(exit, SlotExit::Completed);
    post_scan(app, did).await.0
}

#[tokio::test]
async fn an_interrupted_full_scan_does_not_start_a_cooldown() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    assert_eq!(finish_then_post(&app, &db, USER_A, ScanCompletion::Resumable).await, StatusCode::ACCEPTED, "resume immediately");
    let row = db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == USER_A).unwrap();
    assert_eq!(row.status, "queued", "the retry click was accepted");
    assert!(db.get_scan_state(USER_A, "last_full_scan_finished_at").await.unwrap().is_none());
}

#[tokio::test]
async fn a_scan_completed_with_skips_starts_the_cooldown() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    assert_eq!(finish_then_post(&app, &db, USER_A, ScanCompletion::CompleteWithSkips { n: 2 }).await, StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_completed_full_scan_enforces_cooldown_and_a_refresh_does_not_reset_it() {
    let (app, db) = build_open_test_app_with_db().expect(MODELS_REQUIRED);
    assert_eq!(finish_then_post(&app, &db, USER_A, ScanCompletion::Complete).await, StatusCode::TOO_MANY_REQUESTS);
    let marker = db.get_scan_state(USER_A, "last_full_scan_finished_at").await.unwrap().unwrap();
    // A refresh runs and finishes; the marker and the 429 are unchanged.
    let claim = db.claim_next_scan(1, LEASE_SECS).await.unwrap().unwrap(); // the queued retry click from finish_then_post
    db.finish_queued_scan(USER_A, &claim.claim_id, FinishCompletion::Complete, None).await.unwrap();
    db.enqueue_refresh_scan(USER_A).await.unwrap();
    let claim = db.claim_next_scan(1, LEASE_SECS).await.unwrap().unwrap();
    assert_eq!(claim.kind, ScanKind::Refresh);
    db.finish_queued_scan(USER_A, &claim.claim_id, FinishCompletion::Complete, None).await.unwrap();
    assert_eq!(db.get_scan_state(USER_A, "last_full_scan_finished_at").await.unwrap().unwrap(), marker);
    assert_eq!(post_scan(&app, USER_A).await.0, StatusCode::TOO_MANY_REQUESTS);
}
```

(The transient-interruption and interrupted-drain cases are the same `Resumable` path: `classify_full_scan` yields `Resumable` for both, so `an_interrupted_full_scan_does_not_start_a_cooldown` covers them; the drain case is additionally exercised end-to-end by the V3-03 lifecycle test in `unit_scan_kind.rs`.) These tests need the model-backed test app; `VERIFY_WEB` fails on a skip. `handlers/access.rs:245` and `admin.rs:301`: bind the new return type (`let _ = …?` is enough where the outcome is not surfaced). `admin.rs` `scan_row_json`: `"kind": row.kind.as_str(), "full_requested_at": row.full_requested_at`. `web/src/lib/types.ts` admin row: `kind: 'full' | 'refresh'; full_requested_at: string | null;`.

`src/web/scan_job.rs` — completion is classified, never inferred from `Ok` (V2-05). Above `run_scan`:

```rust
/// How a full scan ended, for bookkeeping. `amplification::run` returns
/// `Ok((events, scored, degraded))` for cost-capped and partially skipped
/// scans alike, so `Ok` alone says nothing about completion (V2-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanCompletion {
    /// Every candidate scored; staging drained to `done`; zero persisted skips.
    Complete,
    /// Reached `done` but `n` accounts were skipped at some point in this
    /// staged run (persisted in `scan_skips`, which survives a resume).
    CompleteWithSkips { n: i64 },
    /// Reached `done` but the skip count could not be read: fulfilled for the
    /// cooldown, never clean, never proof (V5-01).
    CompleteUnverified,
    /// Cost-capped or interrupted: staging left at burst/finalize, re-run to resume.
    Resumable,
}

impl ScanCompletion {
    /// The user's request was carried out — verified or not. Drives the
    /// cooldown marker and clears the full obligation (V6-01). Only
    /// `Complete` additionally proves the revision.
    pub fn fulfilled(&self) -> bool {
        !matches!(self, ScanCompletion::Resumable)
    }

    /// Severity for folding a drain's outcome into the run's own (V6-01):
    /// Resumable > CompleteUnverified > CompleteWithSkips > Complete. Skip
    /// counts add.
    pub fn worst(self, other: ScanCompletion) -> ScanCompletion {
        use ScanCompletion::*;
        match (self, other) {
            (Resumable, _) | (_, Resumable) => Resumable,
            (CompleteUnverified, _) | (_, CompleteUnverified) => CompleteUnverified,
            (CompleteWithSkips { n: a }, CompleteWithSkips { n: b }) => CompleteWithSkips { n: a + b },
            (CompleteWithSkips { n }, Complete) | (Complete, CompleteWithSkips { n }) => CompleteWithSkips { n },
            (Complete, Complete) => Complete,
        }
    }
}

/// Pure classification from the pipeline result, the `scan_phase` marker
/// after the run, and the skip count. `Err` is not a completion at all and
/// is handled by the caller.
pub fn classify_full_scan(degraded: bool, scan_phase: Option<&str>, skipped: Option<i64>) -> ScanCompletion {
    // The marker is authoritative (V4-02): staging left at burst/finalize is
    // unfinished work whatever the summary's flag says, and an unreadable
    // marker is not proof of completion either. The skip count is PERSISTED
    // state (V5-01): `scan_skips` is cleared only at a fresh start, so a
    // resumed invocation that saw no new error still carries the earlier
    // skips — the flag alone would erase them.
    match (scan_phase, skipped, degraded) {
        (Some("burst") | Some("finalize") | None, _, _) => ScanCompletion::Resumable,
        (Some("done"), Some(n), _) if n > 0 => ScanCompletion::CompleteWithSkips { n },
        (Some("done"), Some(_), false) => ScanCompletion::Complete,
        // Degraded with no recorded skip: something else went wrong (decode
        // sentinels, #355). Fulfilled, not clean.
        (Some("done"), Some(_), true) => ScanCompletion::CompleteWithSkips { n: 0 },
        (Some("done"), None, _) => ScanCompletion::CompleteUnverified,
        (Some(_), _, _) => ScanCompletion::Resumable, // gather or unknown: not finished
    }
}
```

Inline tests in `scan_job.rs`: `fulfilled()` is true for `Complete`, `CompleteWithSkips{..}`, `CompleteUnverified` and false for `Resumable`; `worst()` is commutative, `Resumable` absorbs everything, `CompleteUnverified` beats `CompleteWithSkips`, and `CompleteWithSkips{2}.worst(CompleteWithSkips{3}) == CompleteWithSkips{5}`. For `classify_full_scan` (args `(degraded, phase, skipped)`): `(false, Some("done"), Some(0))` → `Complete`; `(true, Some("done"), Some(3))` → `CompleteWithSkips{3}`; **`(false, Some("done"), Some(2))` → `CompleteWithSkips{2}`** (V5-01: a clean resume over earlier persisted skips); `(true, Some("done"), Some(0))` → `CompleteWithSkips{0}`; **`(false, Some("done"), None)` and `(true, Some("done"), None)` → `CompleteUnverified`**; `(true, Some("burst"), Some(0))`, `(true, None, Some(0))`, `(false, Some("burst"), Some(0))`, `(false, Some("finalize"), Some(0))`, `(false, None, Some(0))`, `(false, Some("burst"), None)` → `Resumable` (V4-02/V5-01: unfinished staging is resumable whatever the flag or count).

plus the marker writer and the report type the slot lifecycle needs (V3-02):

```rust
/// What a scan future hands back to `run_under_slot`, so the durable queue
/// outcome (`scan_queue.completion`) keeps the distinction between a
/// fulfilled request and an interrupted attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanReport { pub completion: FinishCompletion }

/// The cooldown anchor. Written for Complete AND CompleteWithSkips (the
/// user's request was fulfilled; skipped accounts are per-account gaps the
/// retry covers), never for Resumable. Best-effort: a scan that completed
/// must not be reported failed because a marker write failed.
pub(crate) async fn record_full_scan_completion(db: &dyn Database, user_did: &str, completion: ScanCompletion) {
    if completion.fulfilled() {
        if let Err(e) = db
            .set_scan_state(user_did, "last_full_scan_finished_at", &chrono::Utc::now().to_rfc3339())
            .await
        {
            warn!(error = %format!("{e:#}"), "could not record last_full_scan_finished_at");
        }
    }
}
```

`run_scan` becomes a thin wrapper around one captured outcome (V5-02), mirroring the refresh wrapper:

```rust
/// What the full scan's inner run produces once it has gone as far as it can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullScanRun { pub events: usize, pub scored: usize, pub completion: ScanCompletion }

/// Everything a full scan writes about itself. Production impl is
/// `DbFullScanBookkeeping` over Task 5's marker and Task 6's schedulers.
#[async_trait]
pub trait FullScanBookkeeping: Send + Sync {
    /// Best-effort: the marker, written only for Complete / CompleteWithSkips / CompleteUnverified.
    async fn record_completion(&self, user_did: &str, completion: ScanCompletion);
    async fn schedule_success(&self, user_did: &str, now: DateTime<Utc>);
    async fn schedule_retry(&self, user_did: &str, now: DateTime<Utc>);
}
pub struct DbFullScanBookkeeping(pub Arc<dyn Database>);

/// ONE scheduling site for the full scan. `run` is the entire inner scan —
/// scorer construction, fingerprint rebuild (with its abort arm), discovery,
/// the pipeline, classification — so an early `?` anywhere in it lands here
/// as `Err` and still gets its retry (V5-02). The slot lifecycle only
/// finishes the row; it never schedules.
pub(crate) async fn run_scan_with<R, Fut>(
    scan_manager: Arc<RwLock<ScanManager>>,
    books: &dyn FullScanBookkeeping,
    clock: &dyn Fn() -> DateTime<Utc>,
    user_did: &str,
    claim_id: &str,
    run: R,
) -> anyhow::Result<ScanReport>
where
    R: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<FullScanRun>>,
{
    let started = clock(); // telemetry only — never a scheduling anchor (V6-02)
    let outcome = run().await;
    // Read the clock AFTER the attempt: a retry is "an hour after this attempt
    // ended", not "an hour after it began" (V6-02).
    let now = clock();
    debug!(user_did, attempt_secs = (now - started).num_seconds(), "full scan attempt finished");
    let completion = match &outcome {
        Ok(r) => Some(r.completion),
        Err(_) => None,
    };
    if let Some(c) = completion {
        books.record_completion(user_did, c).await; // no-op for Resumable
    }
    match completion {
        Some(ScanCompletion::Complete) => books.schedule_success(user_did, now).await,
        // CompleteWithSkips / CompleteUnverified: fulfilled (marker written,
        // obligation cleared by finish), not clean — retry, no proof.
        // Resumable and Err: obligation kept, retry.
        _ => books.schedule_retry(user_did, now).await,
    }
    let (result, finish) = match &outcome {
        Ok(r) => (Ok((r.events, r.scored, r.completion != ScanCompletion::Complete)), r.completion.into()),
        Err(e) => (Err(anyhow::anyhow!("{e:#}")), FinishCompletion::Failed),
    };
    finish_scan(&scan_manager, user_did, claim_id, result, finish, "Completed").await
}

/// Production: the existing run_scan body becomes `run_scan_inner` and
/// returns `FullScanRun`; its last lines, right after
/// `let result = crate::pipeline::amplification::run(...)`:
///
///     let (events, scored, degraded) = result?;
///     let phase = db.get_scan_state(user_did, "scan_phase").await?;
///     let skipped = db.count_scan_skips(user_did).await.ok(); // None = unverified (V5-01)
///     Ok(FullScanRun { events, scored, completion: classify_full_scan(degraded, phase.as_deref(), skipped) })
///
/// (`get_scan_state` failing here is an `Err` → retry, not a guess.)
pub(crate) async fn run_scan(config, db, models, scan_manager, user_did, actor_handle, claim_id) -> anyhow::Result<ScanReport> {
    let books = DbFullScanBookkeeping(Arc::clone(&db));
    run_scan_with(scan_manager.clone(), &books, &chrono::Utc::now, user_did, claim_id, || {
        run_scan_inner(config, db, models, scan_manager, user_did, actor_handle, claim_id)
    })
    .await
}
```

Test, in `scan_job.rs` (no models — the inner run is a closure):

```rust
    /// V5-02: a scheduler-created owed full scan whose setup fails before any
    /// score write still gets exactly one hourly retry, keeps its obligation,
    /// writes no marker and proves nothing — and the tick honours THAT
    /// deadline, not the nightly one it set when it queued the job.
    #[tokio::test]
    async fn a_full_scan_setup_failure_schedules_the_hourly_retry_and_keeps_the_obligation() {
        let db = test_db();
        db.upsert_user("did:plc:owed", "owed.h").await.unwrap();
        let now = chrono::Utc::now();
        // The user asked for a full scan earlier; the tick queued the retry
        // and set the NIGHTLY deadline (as claim_and_enqueue_due_refreshes does).
        db.enqueue_scan("did:plc:owed").await.unwrap();
        db.schedule_refresh("did:plc:owed", &(now + chrono::Duration::hours(24)).to_rfc3339()).await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        let obligation = db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == "did:plc:owed").unwrap().full_requested_at.unwrap();
        let mgr = manager_with_running_scan(&claim.claim_id);
        let books = CountingFullBooks { inner: DbFullScanBookkeeping(db.clone()), retries: 0.into(), successes: 0.into(), markers: 0.into() };

        for failure in ["fingerprint rebuild required (IncompatibleModel) and failed — scan aborted before scoring", "CHARCOAL_CLASSIFIER is unset — build_from_env failed"] {
            let result = run_scan_with(mgr.clone(), &books, &chrono::Utc::now, "did:plc:owed", &claim.claim_id, || async move { anyhow::bail!("{failure}") }).await;
            assert!(result.is_err());
        }
        assert_eq!(books.retries.load(SeqCst), 2, "one retry per failed attempt — no other scheduling call");
        assert_eq!(books.successes.load(SeqCst), 0);
        assert_eq!(books.markers.load(SeqCst), 0, "no completion marker");
        assert!(db.get_scan_state("did:plc:owed", "last_full_scan_finished_at").await.unwrap().is_none());
        assert_ne!(db.refreshed_generation("did:plc:owed").await.unwrap().as_deref(), Some(scoring_revision()), "no proof");
        let row = db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == "did:plc:owed").unwrap();
        assert_eq!(row.full_requested_at.as_deref(), Some(obligation.as_str()), "obligation kept");
        // The retry deadline REPLACED the nightly one.
        let next = chrono::DateTime::parse_from_rfc3339(&db.next_refresh_at("did:plc:owed").await.unwrap().unwrap()).unwrap();
        let delta = next.signed_duration_since(chrono::Utc::now());
        assert!(delta > chrono::Duration::minutes(55) && delta <= chrono::Duration::hours(1), "hourly, not nightly: {delta}");
        // Finish the row as the slot would after the second failure, then tick.
        db.finish_queued_scan("did:plc:owed", &claim.claim_id, FinishCompletion::Failed, Some("setup failed")).await.unwrap();
        assert_eq!(crate::web::refresh::enqueue_due_refreshes(&db, now + chrono::Duration::seconds(30), std::time::Duration::from_secs(24 * 3600)).await, 0);
        assert_eq!(crate::web::refresh::enqueue_due_refreshes(&db, now + chrono::Duration::hours(2), std::time::Duration::from_secs(24 * 3600)).await, 1);
        let row = db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == "did:plc:owed").unwrap();
        assert_eq!((row.status.as_str(), row.kind), ("queued", crate::db::ScanKind::Full));
    }

    /// A settable clock for the wrappers: the `run` closure advances it, so
    /// a 90-minute attempt takes no wall time (V6-02).
    struct FakeClock(std::sync::Arc<std::sync::Mutex<DateTime<Utc>>>);
    impl FakeClock {
        fn now(&self) -> DateTime<Utc> { *self.0.lock().unwrap() }
    }

    /// V6-02: the retry deadline is attempt END + REFRESH_RETRY_HOURS, even
    /// when the attempt itself outlasts the retry window.
    #[tokio::test]
    async fn a_failed_full_scan_is_retried_an_hour_after_it_ended_not_began() {
        use crate::web::refresh::REFRESH_RETRY_HOURS;
        let db = test_db();
        db.upsert_user("did:plc:slow", "slow.h").await.unwrap();
        db.enqueue_scan("did:plc:slow").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        let mgr = manager_with_running_scan(&claim.claim_id);
        let books = CountingFullBooks { inner: DbFullScanBookkeeping(db.clone()), retries: 0.into(), successes: 0.into(), markers: 0.into() };
        let t0 = chrono::Utc::now();
        let clock = FakeClock(std::sync::Arc::new(std::sync::Mutex::new(t0)));
        let advance = clock.0.clone();
        let result = run_scan_with(mgr, &books, &|| clock.now(), "did:plc:slow", &claim.claim_id, move || async move {
            *advance.lock().unwrap() += chrono::Duration::minutes(90); // the attempt takes 90 min
            anyhow::bail!("classifier down for the whole attempt")
        })
        .await;
        assert!(result.is_err());
        let end = t0 + chrono::Duration::minutes(90);
        let deadline = chrono::DateTime::parse_from_rfc3339(&db.next_refresh_at("did:plc:slow").await.unwrap().unwrap()).unwrap();
        assert_eq!(deadline, end + chrono::Duration::hours(REFRESH_RETRY_HOURS as i64), "anchored on the END of the attempt");
        assert_eq!(books.retries.load(SeqCst), 1);
        db.finish_queued_scan("did:plc:slow", &claim.claim_id, FinishCompletion::Failed, Some("down")).await.unwrap();
        // The tick honours that deadline.
        assert_eq!(crate::web::refresh::enqueue_due_refreshes(&db, end + chrono::Duration::minutes(30), std::time::Duration::from_secs(24 * 3600)).await, 0, "still inside the backoff");
        assert_eq!(crate::web::refresh::enqueue_due_refreshes(&db, end + chrono::Duration::minutes(61), std::time::Duration::from_secs(24 * 3600)).await, 1);
        let row = db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == "did:plc:slow").unwrap();
        assert_eq!((row.status.as_str(), row.kind), ("queued", crate::db::ScanKind::Full), "owed work retried as full");
    }

    /// V6-01: an unverified completion is FULFILLED — marker written,
    /// obligation cleared, durable completion `complete_unverified` — but it
    /// is not proof: the retry is scheduled, the revision is not marked.
    #[tokio::test]
    async fn an_unverified_completion_is_fulfilled_but_not_proof() {
        let db = test_db();
        db.upsert_user("did:plc:unv", "unv.h").await.unwrap();
        db.enqueue_scan("did:plc:unv").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        let mgr = manager_with_running_scan(&claim.claim_id);
        let books = CountingFullBooks { inner: DbFullScanBookkeeping(db.clone()), retries: 0.into(), successes: 0.into(), markers: 0.into() };
        let report = run_scan_with(mgr, &books, &chrono::Utc::now, "did:plc:unv", &claim.claim_id, || async {
            Ok(FullScanRun { events: 3, scored: 3, completion: ScanCompletion::CompleteUnverified })
        })
        .await
        .unwrap();
        assert_eq!(report.completion, FinishCompletion::CompleteUnverified);
        assert_eq!((books.markers.load(SeqCst), books.retries.load(SeqCst), books.successes.load(SeqCst)), (1, 1, 0));
        assert!(db.get_scan_state("did:plc:unv", "last_full_scan_finished_at").await.unwrap().is_some(), "cooldown anchored: the request was carried out");
        assert_ne!(db.refreshed_generation("did:plc:unv").await.unwrap().as_deref(), Some(scoring_revision()), "no proof");
        // The slot finishes the row from the report:
        db.finish_queued_scan("did:plc:unv", &claim.claim_id, report.completion, None).await.unwrap();
        let row = db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == "did:plc:unv").unwrap();
        assert_eq!(row.completion, Some(FinishCompletion::CompleteUnverified));
        assert!(row.full_requested_at.is_none(), "obligation cleared — fulfilled");
    }
```

Postgres twin for the finalization half of V6-01: `test_pg_finish_unverified_clears_the_obligation` (enqueue → claim → `finish_queued_scan(…, CompleteUnverified, None)` → `completion = 'complete_unverified'`, `full_requested_at IS NULL`, `status = 'done'`; and the same for `Resumable` keeps it).

`CountingFullBooks` is the full-scan twin of Task 9's `CountingBooks` (delegates to `DbFullScanBookkeeping`, counts `record_completion` calls that actually wrote — i.e. non-`Resumable` — as `markers`). The slot lifecycle still finishes the row from `ScanReport`:



`run_under_slot<F: Future<Output = anyhow::Result<ScanReport>>>` passes `report.completion` (or `FinishCompletion::Failed` for `Err`/panic) to `finish_queued_scan`. `impl From<ScanCompletion> for FinishCompletion`: Complete → Complete, CompleteWithSkips → CompleteWithSkips, CompleteUnverified → CompleteUnverified, Resumable → Resumable.

: `(false, Some("done"), 0)` → `Complete`; `(true, Some("done"), 3)` → `CompleteWithSkips{3}`; `(true, Some("burst"), 0)` and `(true, None, 0)` → `Resumable`. Task 6 uses `completion` for scheduling and Task 9 reuses the same classification for the drain transition.

- [ ] **Step 7: Postgres twins**

Append to `tests/db_postgres.rs`: `test_pg_enqueue_outcomes_and_handover` (refresh enqueue → claim → full enqueue is `QueuedAfterRefresh` twice with an unchanged `full_requested_at` → finish → row is `queued full` dated from the request → claim returns `Full`), `test_pg_full_enqueue_upgrades_queued_refresh_keeping_position` (assert `enqueued_at` unchanged), `test_pg_refresh_never_downgrades`, and the median test (one done full 3600 s, one done refresh 60 s, `eta_seconds == Some(3600)`). Prefix DIDs `did:plc:pgkind_`, delete rows at the end; extend `cleanup_test_data` to `DELETE FROM scan_queue WHERE user_did LIKE 'did:plc:pg%'` if it does not already.

- [ ] **Step 8: Run**

Run: `cargo test --test unit_scan_kind`, `VERIFY_WEB`, `VERIFY_PG`, `VERIFY_FE` (type change), `VERIFY_CLIPPY`.
Expected: green. The admitter/slot-lifecycle inline tests still compile: they call `enqueue_scan(...).await.expect("enqueue")` and ignore the value.

- [ ] **Step 9: Commit**

```bash
git add src/db/traits.rs src/db/mod.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs src/web/handlers/scan.rs src/web/handlers/admin.rs src/web/handlers/access.rs src/web/scan_job.rs web/src/lib/types.ts tests/unit_scan_kind.rs tests/db_postgres.rs
git commit -m 'feat(344): scan_queue.kind — refresh rows share the queue; a user request is always honoured; completion classified

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

**Why:** Spec §4.4: the refresh tick runs on the admitter's `TICK`, no second loop, no second lock. R04: selecting due users and creating their queue rows must be one transaction, bounded per tick, retried on failure. R07: a revision change must make users due through durable state the scheduler reads every tick, not a one-off migration stamp. V2-02: the queue write must be conditional at the write itself, because manual enqueue and admission lock the queue row, not the user row, and can run between the tick's select and its write. V2-03: "this revision needs work" and "another attempt is allowed now" are different facts; a failed attempt must wait for its retry deadline.

**Due rule:** a user is due when they have at least one score row, have **no queued or running `scan_queue` row of either kind**, and either `next_refresh_at <= now` or `refresh_attempted_generation IS DISTINCT FROM <current revision>`. Two generation columns, two facts (V2-03): `refresh_attempted_generation` is set by the **tick** in the claiming transaction ("an attempt for this revision has been scheduled"), so a failed/deferred/resumable attempt does not make the user due again by revision — only its `next_refresh_at` retry deadline does; `refreshed_generation` is set by a **completed** refresh or full scan ("this revision is proven") and is observability, not a scheduling input. A new revision arriving during a backoff differs from `refresh_attempted_generation` and is attempted promptly. The tick advances `next_refresh_at` and sets `refresh_attempted_generation` **only for users whose queue write affected a row** (V2-02), in the same transaction; a user whose row turned out to be queued/running at write time is left entirely alone and reconsidered next tick.

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
async fn refresh_attempted_generation(&self, user_did: &str) -> Result<Option<String>>;
async fn schedule_refresh(&self, user_did: &str, at_rfc3339: &str) -> Result<()>;
/// Retry: sets next_refresh_at AND refresh_attempted_generation in one statement (V4-01).
async fn schedule_retry_at(&self, user_did: &str, at_rfc3339: &str, attempted_generation: &str) -> Result<()>;
/// Proof: sets BOTH refreshed_generation and refresh_attempted_generation.
async fn mark_refreshed_generation(&self, user_did: &str, generation: &str) -> Result<()>;
/// ONE transaction: select up to `limit` due users (see the due rule), then
/// for each CONDITIONALLY write the refresh queue row (ON CONFLICT … WHERE
/// status IN ('done','failed')) and, only if that write affected a row, set
/// next_refresh_at = `next` and refresh_attempted_generation = `current`.
/// Returns the DIDs actually delivered. A failure rolls back everything.
async fn claim_and_enqueue_due_refreshes(&self, now_rfc3339: &str, next_rfc3339: &str,
    current_generation: &str, limit: usize) -> Result<Vec<String>>;
/// The two statements, public so the V2-02 interleaving test can run them
/// on its own connections around a concurrent manual enqueue.
pub const REFRESH_DUE_SQL: &str;      // the SELECT … (FOR UPDATE SKIP LOCKED on Postgres)
pub const REFRESH_ENQUEUE_SQL: &str;  // the conditional INSERT … ON CONFLICT … WHERE

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
    use crate::scoring::generation::scoring_revision;
    use chrono::{Duration as ChronoDuration, Utc};
    use rusqlite::Connection;
    use std::sync::Arc;

    fn db() -> Arc<dyn Database> {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        Arc::new(SqliteDatabase::new(conn))
    }

    /// A user with one score row (so they are eligible), scheduled `at`,
    /// with `attempted` as the revision last attempted/proven (None = never).
    async fn user(db: &Arc<dyn Database>, did: &str, at: Option<&str>, attempted: Option<&str>) {
        db.upsert_user(did, &format!("{did}.handle")).await.unwrap();
        let mut score = crate::db::models::AccountScore::default_for_test(did);
        score.threat_score = Some(40.0);
        score.threat_tier = Some("High".into());
        db.upsert_account_score(did, &score).await.unwrap();
        if let Some(at) = at {
            db.schedule_refresh(did, at).await.unwrap();
        }
        if let Some(g) = attempted {
            // mark_refreshed_generation sets both columns — a proven revision
            // is also an attempted one.
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
        user(&db, "did:plc:due-time", Some(&past), Some(scoring_revision())).await;
        user(&db, "did:plc:due-gen", Some(&future), Some("1999-01-01")).await;
        user(&db, "did:plc:due-never-refreshed", None, None).await; // v18-migrated shape
        user(&db, "did:plc:not-due", Some(&future), Some(scoring_revision())).await;
        db.upsert_user("did:plc:no-scores", "none.handle").await.unwrap(); // no rows ⇒ never due

        let n = enqueue_due_refreshes(&db, now, Duration::from_secs(24 * 3600)).await;
        assert_eq!(n, 3);
        for did in ["did:plc:due-time", "did:plc:due-gen", "did:plc:due-never-refreshed"] {
            assert_eq!(queue_kind(&db, did).await, Some(("queued".into(), ScanKind::Refresh)), "{did}");
        }
        assert_eq!(queue_kind(&db, "did:plc:not-due").await, None);
        assert_eq!(queue_kind(&db, "did:plc:no-scores").await, None);

        // A tick a minute later claims nobody: every claimed user was
        // rescheduled AND stamped refresh_attempted_generation = current, so
        // neither clause fires again until their deadline.
        let n = enqueue_due_refreshes(&db, now + ChronoDuration::minutes(1), Duration::from_secs(24 * 3600)).await;
        assert_eq!(n, 0);
        for did in ["did:plc:due-time", "did:plc:due-gen", "did:plc:due-never-refreshed"] {
            assert_eq!(db.refresh_attempted_generation(did).await.unwrap().as_deref(), Some(scoring_revision()), "{did}");
            assert_ne!(db.refreshed_generation(did).await.unwrap().as_deref(), Some(scoring_revision()), "{did}: the tick never PROVES a revision");
        }
    }

    /// V2-03: a failed attempt under a new revision waits for its retry
    /// deadline; it is not re-claimed on the next tick just because the
    /// revision is still unproven. A NEWER revision during the backoff is
    /// attempted promptly.
    #[tokio::test]
    async fn a_failed_attempt_waits_for_its_retry_deadline() {
        let db = db();
        let now = Utc::now();
        user(&db, "did:plc:retry", None, None).await; // v18-migrated shape: never attempted
        assert_eq!(enqueue_due_refreshes(&db, now, Duration::from_secs(24 * 3600)).await, 1);
        // The refresh runs and fails: the row finishes, the retry is scheduled.
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        db.finish_queued_scan("did:plc:retry", &claim.claim_id, Some("boom")).await.unwrap();
        schedule_retry(db.as_ref(), "did:plc:retry", now).await;

        assert_eq!(enqueue_due_refreshes(&db, now + ChronoDuration::seconds(30), Duration::from_secs(24 * 3600)).await, 0, "30 s later: no new job");
        assert_eq!(enqueue_due_refreshes(&db, now + ChronoDuration::minutes(61), Duration::from_secs(24 * 3600)).await, 1, "after the deadline: exactly one");

        // A newer revision arrives while a fresh backoff is pending.
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        db.finish_queued_scan("did:plc:retry", &claim.claim_id, Some("boom again")).await.unwrap();
        let t = now + ChronoDuration::minutes(62);
        schedule_retry(db.as_ref(), "did:plc:retry", t).await;
        let promptly = db
            .claim_and_enqueue_due_refreshes(&(t + ChronoDuration::seconds(30)).to_rfc3339(), &(t + ChronoDuration::hours(24)).to_rfc3339(), "rev-B", 25)
            .await
            .unwrap();
        assert_eq!(promptly, vec!["did:plc:retry".to_string()], "a new revision is attempted at once, once");
        assert_eq!(db.refresh_attempted_generation("did:plc:retry").await.unwrap().as_deref(), Some("rev-B"));
    }

    /// A user with queued or running work is not delivered: their schedule
    /// and attempted revision are left alone (the completion path reschedules
    /// them) and their row is not touched — a queued full scan is never
    /// downgraded, a running scan never interrupted. On SQLite the immediate
    /// transaction makes the select and the write atomic; the write is still
    /// conditional so the two backends share one contract (V2-02).
    #[tokio::test]
    async fn a_user_with_queued_or_running_work_is_not_claimed() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        user(&db, "did:plc:busy", Some(&past), Some(scoring_revision())).await;
        db.enqueue_scan("did:plc:busy").await.unwrap();
        user(&db, "did:plc:free", Some(&past), Some(scoring_revision())).await;

        let claimed = db
            .claim_and_enqueue_due_refreshes(
                &now.to_rfc3339(),
                &(now + ChronoDuration::hours(24)).to_rfc3339(),
                scoring_revision(),
                25,
            )
            .await
            .unwrap();
        assert_eq!(claimed, vec!["did:plc:free".to_string()]);
        assert_eq!(queue_kind(&db, "did:plc:busy").await, Some(("queued".into(), ScanKind::Full)), "not downgraded");
        assert_eq!(db.next_refresh_at("did:plc:busy").await.unwrap().as_deref(), Some(past.as_str()), "schedule untouched");
    }

    /// V2-02 at the SQL level: the conditional write refuses to clobber a
    /// row that changed between the select and the write. Simulated on one
    /// connection by running the select, then a manual full enqueue, then
    /// the tick's write statement.
    #[tokio::test]
    async fn the_queue_write_is_conditional_on_the_row_state_at_write_time() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn.execute("INSERT INTO users (did, handle) VALUES ('did:plc:race', 'race.h')", []).unwrap();
        conn.execute(
            "INSERT INTO account_scores (user_did, did, handle, threat_score, scoring_generation, valid_until)
             VALUES ('did:plc:race', 'did:plc:x', 'x.h', 40.0, 'legacy', datetime('now', '-1 day'))",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO scan_queue (user_did, status, kind, enqueued_at, finished_at)
             VALUES ('did:plc:race', 'done', 'full', '2026-09-10T00:00:00+00:00', '2026-09-10T01:00:00+00:00')",
            [],
        )
        .unwrap();
        // 1. The tick selects: the user is due (never attempted) and has a done row.
        let due: Vec<String> = conn
            .prepare(crate::db::queries::REFRESH_DUE_SQL).unwrap()
            .query_map(rusqlite::params![Utc::now().to_rfc3339(), scoring_revision(), 25i64], |r| r.get(0)).unwrap()
            .map(Result::unwrap).collect();
        assert_eq!(due, vec!["did:plc:race".to_string()]);
        // 2. A manual full enqueue lands in between.
        crate::db::queries::enqueue_scan(&conn, "did:plc:race").unwrap();
        // 3. The tick's conditional write affects nothing; the full request survives.
        let affected = conn
            .execute(crate::db::queries::REFRESH_ENQUEUE_SQL, rusqlite::params!["did:plc:race", Utc::now().to_rfc3339()])
            .unwrap();
        assert_eq!(affected, 0);
        let (status, kind): (String, String) = conn
            .query_row("SELECT status, kind FROM scan_queue WHERE user_did = 'did:plc:race'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((status.as_str(), kind.as_str()), ("queued", "full"));
    }

    /// V3-04 through the real success path: a manually queued full scan for
    /// a never-attempted user proves BOTH columns, and the next tick stays
    /// quiet until the scheduled deadline.
    #[tokio::test]
    async fn a_completed_manual_full_scan_proves_both_columns_and_the_tick_stays_quiet() {
        let db = db();
        let now = Utc::now();
        user(&db, "did:plc:manual", None, None).await; // never attempted
        db.enqueue_scan("did:plc:manual").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        // What run_scan does on Complete (Task 5 + this task):
        crate::web::scan_job::record_full_scan_completion(db.as_ref(), "did:plc:manual", crate::web::scan_job::ScanCompletion::Complete).await;
        std::env::remove_var(REFRESH_INTERVAL_ENV);
        schedule_after_success(db.as_ref(), "did:plc:manual", now).await;
        db.finish_queued_scan("did:plc:manual", &claim.claim_id, crate::db::FinishCompletion::Complete, None).await.unwrap();

        assert_eq!(db.refreshed_generation("did:plc:manual").await.unwrap().as_deref(), Some(scoring_revision()));
        assert_eq!(db.refresh_attempted_generation("did:plc:manual").await.unwrap().as_deref(), Some(scoring_revision()));
        assert_eq!(enqueue_due_refreshes(&db, now + ChronoDuration::seconds(30), Duration::from_secs(24 * 3600)).await, 0, "no unnecessary refresh");
        assert_eq!(enqueue_due_refreshes(&db, now + ChronoDuration::hours(25), Duration::from_secs(24 * 3600)).await, 1);
    }

    /// V3-03 at the tick: a finished row that owes a full scan is re-queued
    /// as FULL, not as a refresh.
    #[tokio::test]
    async fn owed_full_work_is_retried_as_a_full_scan() {
        let db = db();
        let now = Utc::now();
        user(&db, "did:plc:owed", Some(&(now - ChronoDuration::hours(1)).to_rfc3339()), Some(scoring_revision())).await;
        db.enqueue_scan("did:plc:owed").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        db.finish_queued_scan("did:plc:owed", &claim.claim_id, crate::db::FinishCompletion::Resumable, None).await.unwrap();
        assert_eq!(enqueue_due_refreshes(&db, now, Duration::from_secs(3600)).await, 1);
        assert_eq!(queue_kind(&db, "did:plc:owed").await, Some(("queued".into(), ScanKind::Full)));
    }

    #[tokio::test]
    async fn a_tick_is_bounded_and_the_rest_wait_for_the_next_one() {
        let db = db();
        let now = Utc::now();
        let past = (now - ChronoDuration::hours(1)).to_rfc3339();
        for i in 0..30 {
            user(&db, &format!("did:plc:bulk{i:02}"), Some(&past), Some(scoring_revision())).await;
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
        user(&db, "did:plc:unlucky", Some(&past), Some(scoring_revision())).await;

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
        assert_eq!(db.refreshed_generation("did:plc:u").await.unwrap().as_deref(), Some(scoring_revision()));

        schedule_retry(db.as_ref(), "did:plc:u", now).await;
        let next = db.next_refresh_at("did:plc:u").await.unwrap().unwrap();
        assert_eq!(next, (now + ChronoDuration::hours(REFRESH_RETRY_HOURS as i64)).to_rfc3339());
        assert_eq!(db.refreshed_generation("did:plc:u").await.unwrap().as_deref(), Some(scoring_revision()), "retry does not unset the proof");
        assert_eq!(db.refresh_attempted_generation("did:plc:u").await.unwrap().as_deref(), Some(scoring_revision()), "retry stamps the attempt (V4-01)");
    }

    /// V4-01: a first full scan that failed before writing a single score is
    /// still retried — as a full scan, after its deadline, once — across a
    /// simulated restart. A user with neither scores nor an obligation is
    /// never selected.
    #[tokio::test]
    async fn an_owed_first_scan_with_no_scores_is_retried_after_its_deadline() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let open = || -> Arc<dyn Database> {
            let conn = Connection::open(file.path()).unwrap();
            create_tables(&conn).unwrap();
            Arc::new(SqliteDatabase::new(conn))
        };
        let db = open();
        db.upsert_user("did:plc:first", "first.h").await.unwrap();
        db.upsert_user("did:plc:nobody", "nobody.h").await.unwrap(); // no scores, no request: never due
        let now = Utc::now();
        // 1–2. Their first full scan is claimed and fails before any write.
        db.enqueue_scan("did:plc:first").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        db.finish_queued_scan("did:plc:first", &claim.claim_id, crate::db::FinishCompletion::Failed, Some("fingerprint build failed")).await.unwrap();
        schedule_retry(db.as_ref(), "did:plc:first", now).await;
        let owed = db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == "did:plc:first").unwrap();
        let requested = owed.full_requested_at.clone().expect("obligation retained on failure");
        assert_eq!(db.export_scores("did:plc:first").await.unwrap().len(), 0, "zero scores — the case V4-01 is about");
        // 3. Restart.
        drop(db);
        let db = open();
        // 4. Before the deadline: nothing.
        assert_eq!(enqueue_due_refreshes(&db, now + ChronoDuration::seconds(30), Duration::from_secs(24 * 3600)).await, 0);
        // 5. After it: exactly one FULL job, same obligation timestamp.
        assert_eq!(enqueue_due_refreshes(&db, now + ChronoDuration::hours(REFRESH_RETRY_HOURS as i64 + 1), Duration::from_secs(24 * 3600)).await, 1);
        let r = db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == "did:plc:first").unwrap();
        assert_eq!((r.status.as_str(), r.kind), ("queued", ScanKind::Full));
        assert_eq!(r.full_requested_at.as_deref(), Some(requested.as_str()));
        // 6. The user with nothing owed and nothing scored was not selected.
        assert!(db.list_scan_queue().await.unwrap().iter().all(|r| r.user_did != "did:plc:nobody"));
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

pub fn schedule_retry_at(conn: &Connection, user_did: &str, at_rfc3339: &str, attempted_generation: &str) -> Result<()> {
    conn.execute(
        "UPDATE users SET next_refresh_at = ?2, refresh_attempted_generation = ?3 WHERE did = ?1",
        params![user_did, at_rfc3339, attempted_generation],
    )?;
    Ok(())
}

/// Proof. Sets BOTH columns (V3-04): a proven revision is also an attempted
/// one, so the tick's revision clause stays quiet after a manual full scan.
pub fn mark_refreshed_generation(conn: &Connection, user_did: &str, generation: &str) -> Result<()> {
    conn.execute(
        "UPDATE users SET refreshed_generation = ?2, refresh_attempted_generation = ?2 WHERE did = ?1",
        params![user_did, generation],
    )?;
    Ok(())
}

/// Attempt only (used by `migrate` to copy a pending attempt verbatim).
pub fn mark_refresh_attempted_generation(conn: &Connection, user_did: &str, generation: &str) -> Result<()> {
    conn.execute("UPDATE users SET refresh_attempted_generation = ?2 WHERE did = ?1", params![user_did, generation])?;
    Ok(())
}

/// Params: ?1 now (RFC3339), ?2 current revision, ?3 limit.
pub const REFRESH_DUE_SQL: &str =
    "SELECT u.did FROM users u
     WHERE (EXISTS (SELECT 1 FROM account_scores s WHERE s.user_did = u.did)
            OR EXISTS (SELECT 1 FROM scan_queue o
                       WHERE o.user_did = u.did AND o.full_requested_at IS NOT NULL))
       AND NOT EXISTS (SELECT 1 FROM scan_queue q
                       WHERE q.user_did = u.did AND q.status IN ('queued', 'running'))
       AND ((u.next_refresh_at IS NOT NULL AND u.next_refresh_at <= ?1)
            OR u.refresh_attempted_generation IS NULL
            OR u.refresh_attempted_generation != ?2)
     ORDER BY u.next_refresh_at, u.did
     LIMIT ?3";
     // Eligibility (V4-01): score rows OR an owed full scan. A first scan that
     // failed before its first write has no scores but does have
     // full_requested_at on its finished row — it must be retried. Users with
     // neither are never selected. Timing is unchanged: schedule_retry stamps
     // refresh_attempted_generation, so the revision clause is quiet until
     // the deadline for retries of either kind.

/// Params: ?1 user_did, ?2 now (RFC3339). CONDITIONAL: the WHERE is evaluated
/// against the row as it is at write time, so a full row queued or admitted
/// after the select is never clobbered (V2-02). Affects 0 rows in that case.
/// A finished row that still owes a full scan is re-queued as FULL (V3-03).
pub const REFRESH_ENQUEUE_SQL: &str =
    "INSERT INTO scan_queue (user_did, status, kind, enqueued_at)
     VALUES (?1, 'queued', 'refresh', ?2)
     ON CONFLICT(user_did) DO UPDATE SET
         status = 'queued',
         kind = CASE WHEN scan_queue.full_requested_at IS NOT NULL THEN 'full' ELSE 'refresh' END,
         enqueued_at = ?2,
         started_at = NULL, finished_at = NULL, lease_expires = NULL,
         last_error = NULL, claim_id = NULL, completion = NULL
     WHERE status IN ('done', 'failed')";
     // full_requested_at is deliberately NOT in the SET list: owed work stays
     // owed until a full scan completes (V3-03), and the CASE turns the
     // retry into a full scan instead of a refresh.

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
        let mut stmt = tx.prepare(REFRESH_DUE_SQL)?;
        stmt.query_map(params![now_rfc3339, current_generation, limit as i64], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut delivered = Vec::with_capacity(due.len());
    for did in due {
        let affected = tx.execute(REFRESH_ENQUEUE_SQL, params![did, now_rfc3339])?;
        if affected == 0 {
            // The row changed under us (queued/running now). Leave the
            // schedule alone: whatever is running reschedules on completion.
            continue;
        }
        tx.execute(
            "UPDATE users SET next_refresh_at = ?2, refresh_attempted_generation = ?3 WHERE did = ?1",
            params![did, next_rfc3339, current_generation],
        )?;
        delivered.push(did);
    }
    tx.commit()?;
    Ok(delivered)
}
```

Postgres: the same two statements as `pub const`s in `postgres.rs` (`$1::timestamptz`, the owed-full `OR EXISTS` eligibility, `refresh_attempted_generation IS DISTINCT FROM $2`, the owed-full `CASE`, `mark_refreshed_generation` setting both columns, `schedule_retry_at` setting both `next_refresh_at` and `refresh_attempted_generation`, `ORDER BY next_refresh_at NULLS FIRST, did LIMIT $3 FOR UPDATE OF u SKIP LOCKED` — two replicas ticking together partition the due set), in one `BEGIN … COMMIT` over `self.pool.begin()`; per user the conditional `INSERT … ON CONFLICT (user_did) DO UPDATE SET … WHERE scan_queue.status IN ('done','failed')`, and the `users` update only when `rows_affected() == 1`. Lock order: `users` (FOR UPDATE) then `scan_queue`; `enqueue_scan`/`finish_queued_scan` lock only `scan_queue`; `schedule_*` only `users` — no cycle. The `NOT EXISTS` in the select is an optimisation; the conditional write is the guarantee. `next_refresh_at`/`refreshed_generation` getters render `to_rfc3339()`.

- [ ] **Step 4: `src/web/refresh.rs` implementation (above the tests)**

```rust
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tracing::{error, info, warn};

use crate::db::Database;
use crate::scoring::generation::scoring_revision;

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
        .claim_and_enqueue_due_refreshes(&now.to_rfc3339(), &plus(now, interval), scoring_revision(), REFRESH_BATCH_PER_TICK)
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
    // Proof: sets refreshed_generation AND refresh_attempted_generation.
    if let Err(e) = db.mark_refreshed_generation(user_did, scoring_revision()).await {
        warn!(error = %format!("{e:#}"), "could not record refreshed_generation");
    }
}

/// After a deferred, resumable, partially completed or failed attempt: retry
/// at the deadline. Sets next_refresh_at AND stamps refresh_attempted_generation
/// (V2-03, V4-01): an attempt happened — whether the tick claimed it or the
/// user clicked — so the revision clause stays quiet until the deadline.
/// refreshed_generation stays unproven so the runbook can see the user is
/// behind. One statement on both backends.
pub async fn schedule_retry(db: &dyn Database, user_did: &str, now: DateTime<Utc>) {
    if let Err(e) = db
        .schedule_retry_at(user_did, &plus(now, Duration::from_secs(REFRESH_RETRY_HOURS * 3600)), scoring_revision())
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

`spawn_admitter` passes `crate::web::refresh::refresh_interval_from_env()`; the three test spawns pass `None`. Add the admitter test `the_tick_enqueues_and_admits_a_due_refresh` (user with one score row, `refreshed_generation` NULL, tick 20 ms, `RecordingLauncher::notifying`; assert the launched DID and, via a new `kinds()` accessor on `RecordingLauncher`, `ScanKind::Refresh`). `src/web/scan_job.rs`: `DbFullScanBookkeeping::schedule_success` = `schedule_after_success`, `::schedule_retry` = `schedule_retry`, `::record_completion` = `record_full_scan_completion`. The wrapper `run_scan_with` (Task 5) is the only caller (V5-02): `Complete` → success; `CompleteWithSkips`/`CompleteUnverified` → retry (fulfilled for the cooldown, not proof of the revision); `Resumable` and **every `Err` from the inner run — scorer setup, fingerprint rebuild, discovery, pipeline** → retry (the obligation stays on the row, so the retry runs as a full scan — V3-03). A full scan proves the revision only when it truly completed (V2-05).

- [ ] **Step 6: Postgres twins**

Append to `tests/db_postgres.rs`:
- `test_pg_claim_and_enqueue_is_one_transaction_and_bounded` (seed 3 due users + 1 not due + 1 without scores + 1 due-by-revision + 1 due-by-time with a queued full row; claim with limit 2 → 2 DIDs, rows queued refresh, `next_refresh_at` and `refresh_attempted_generation` set for exactly those 2; claim again → the remaining 2 (never the busy one); claim again → 0);
- `test_pg_two_schedulers_partition_the_due_set` (two `PgDatabase`s over two pools, `tokio::join!` the claim with limit 25 over 10 due users — union is the 10, intersection empty; `SKIP LOCKED`);
- Postgres twins of `a_completed_manual_full_scan_proves_both_columns_and_the_tick_stays_quiet` (V3-04) and `owed_full_work_is_retried_as_a_full_scan` (V3-03), through the same functions;
- **`test_pg_first_enqueues_serialize_on_the_user_lock` (V4-03, V5-03):** two `PgDatabase`s over two pools for a user with no queue row. Connection A: `BEGIN; SELECT pg_advisory_xact_lock(hashtext('scan_queue:' || $1))`. Task B: `enqueue_scan(did)`. Establish that B is **waiting on that lock** deterministically — poll `pg_locks` (`SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' AND NOT granted AND objid = hashtext('scan_queue:' || $1)::oid`) until it is 1, no sleep-based inference — then A inserts the full row and `COMMIT`s. B returns: assert `AlreadyQueued` (A's row is `queued full`), exactly one row, one `full_requested_at` (A's, coalesced). No claim is involved, so there is no race to win;
- **`test_pg_enqueue_preserves_a_committed_running_claim` (V4-03, V5-03):** establish the state first, then compete: A: `enqueue_scan(did)` → `Queued`; A: `claim_next_scan(1, 60)` → running with `claim_id` and `lease_expires` (both committed). Then B: `enqueue_scan(did)` → `AlreadyRunning`; assert status `running`, A's `claim_id`, `lease_expires` unchanged, and the original `full_requested_at`. Two more B clicks → `AlreadyRunning`, obligation unchanged. Variant: run the raw absent-row `INSERT … ON CONFLICT … WHERE` from the plan against that running row directly → `rows_affected() == 0`, state preserved (the defense-in-depth clause);
- **`test_pg_scheduler_write_does_not_clobber_a_concurrent_full_enqueue` (V2-02):** connection A: `BEGIN`, run `REFRESH_DUE_SQL` (locks the user row, returns `did:plc:pgrace`); connection B (a separate `PgDatabase`): `enqueue_scan("did:plc:pgrace")` — commits, the done row is now `queued full`; connection A: run `REFRESH_ENQUEUE_SQL` for the DID → `rows_affected() == 0`; skip the `users` update; `COMMIT`. Assert the row is still `queued full`, `next_refresh_at` and `refresh_attempted_generation` unchanged. Variant 2: after B's enqueue, B also `claim_next_scan(1, 60)` (the full scan is admitted, `running` with a claim id and lease) before A's write — assert kind/status/claim_id/lease_expires unchanged. Variant 3: no queue row exists at select time; B enqueues (inserts) between; A's INSERT conflicts, the WHERE sees `queued`, 0 rows. All three run the real statements because they are `pub const`.

- [ ] **Step 7: Run**

Run: `cargo test --features web --lib web::refresh`, `cargo test --features web --lib web::admitter`, `VERIFY_WEB`, `VERIFY_PG`, `VERIFY_CLIPPY`.
Expected: green.

- [ ] **Step 8: Commit**

```bash
git add src/web/refresh.rs src/web/mod.rs src/observability/refresh_metrics.rs src/observability/mod.rs src/db/traits.rs src/db/models.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs src/web/admitter.rs src/web/scan_job.rs tests/db_postgres.rs
git commit -m 'feat(344): durable, bounded, revision-aware refresh scheduling with conditional queue writes

claim_and_enqueue_due_refreshes: one transaction per tick (SKIP LOCKED
on Postgres, immediate tx on SQLite) that selects up to 25 due users
and writes each refresh row CONDITIONALLY (ON CONFLICT … WHERE done/failed),
advancing next_refresh_at + refresh_attempted_generation only for rows it
actually wrote — a concurrent manual enqueue or admission survives. Due =
next_refresh_at passed OR refresh_attempted_generation is not the current
revision, so a revision change is attempted promptly, once, and a failed
attempt waits for its retry deadline. Completed refreshes and truly
complete full scans prove the revision; everything else retries in an hour.

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
use charcoal::scoring::generation::{scoring_revision, LEGACY_GENERATION};
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
    insert(&conn, USER, "did:plc:high-expiring", 60.0, &days(1), scoring_revision(), Some("Stranger"));
    insert(&conn, USER, "did:plc:high-expired", 40.0, &days(-3), scoring_revision(), Some("Follows you"));
    insert(&conn, USER, "did:plc:high-null", 45.0, "NULL", scoring_revision(), None);
    insert(&conn, USER, "did:plc:high-malformed", 42.0, "'yesterday-ish'", scoring_revision(), None);
    insert(&conn, USER, "did:plc:elevated-legacy", 20.0, &days(10), LEGACY_GENERATION, None);
    insert(&conn, USER, "did:plc:elevated-floor", 15.0, &days(1), scoring_revision(), None);
    // Excluded:
    insert(&conn, USER, "did:plc:high-fresh", 50.0, &days(10), scoring_revision(), None);
    insert(&conn, USER, "did:plc:watch-legacy", 14.99, &days(1), LEGACY_GENERATION, None);
    insert(&conn, "did:plc:otheruser", "did:plc:high-other", 60.0, &days(1), scoring_revision(), None);
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
    insert(&conn, USER, "did:plc:soon", 60.0, &days(1), scoring_revision(), None);
    insert(&conn, USER, "did:plc:gone", 60.0, &days(-1), scoring_revision(), None);
    insert(&conn, USER, "did:plc:boundary", 60.0, "datetime('now')", scoring_revision(), None);
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
        .query_map(params![USER, 15.0, "+2 days", scoring_revision()], |r| r.get::<_, String>(3))
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
/// Params: ?1 user_did, ?2 ThreatTier::ELEVATED_MIN, ?3 "+N days", ?4 scoring_revision().
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
            params![user_did, ThreatTier::ELEVATED_MIN, format!("+{horizon_days} days"), scoring_revision()],
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

**Why:** `run_refresh` (Task 9) needs the scorer bundle, protected-post embeddings, pile-on set and direct-pair loading that `run_scan` and `amplification::run` build inline. R05: the extracted helpers must **return errors**. Today `direct_pairs_for`'s equivalent swallows a read failure into an empty list (which flips the account to the follower path) and the embedding step swallows into `None` (missing context). A full scan may still choose to degrade at its call site — that is today's behaviour and stays — but the helper itself must not decide that for the refresh. V2-04: this task also makes the full scan's fingerprint **rebuild decision** (`scan_job.rs:807-861`) check the embedding model identity, and forbids the existing fall-back-to-the-old-fingerprint path when the reason for rebuilding is model incompatibility.

**Files:**
- Modify: `src/pipeline/amplification.rs:340-366` → `pub async fn direct_pairs_for(...) -> Result<Vec<(String, String)>>`
- Modify: `src/web/scan_job.rs:693-735` → `ScanScorers` + `build_scan_scorers`; `:1108-1128` → `record_scan_cache_stats`; `:875-900` → `embed_protected_posts(...) -> Result<Vec<(String, Vec<f64>)>>`; `:1045-1053` → `pile_on_dids`; `:807-861` → `RebuildReason` + `rebuild_decision` + no-fallback-on-incompatible (V2-04)
- Test: `tests/unit_direct_pairs.rs` (new); `src/web/scan_job.rs` inline tests for `rebuild_decision`; existing suites for "nothing changed"

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

/// Why the protected fingerprint must be rebuilt before this scan (V2-04).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildReason { Absent, Unreadable, Stale, Format, IncompatibleModel }
impl RebuildReason {
    /// Only age/format rebuilds may fall back to the stored fingerprint when
    /// the rebuild fails; incompatible vectors must never be scored against.
    pub fn fallback_allowed(&self) -> bool { matches!(self, RebuildReason::Stale | RebuildReason::Format) }
}
/// Pure decision: `None` = the stored fingerprint is usable as-is.
pub fn rebuild_decision(stored: Option<&(String, String)> /* (json, updated_at) */, parsed_ok: bool,
    has_embedding: bool, stored_model_id: Option<&str>, centroid_rows: usize, json_clusters: usize,
    now: chrono::NaiveDateTime) -> Option<RebuildReason>;
```

- [ ] **Step 0: Write the failing rebuild-decision tests (inline, `scan_job.rs`)**

```rust
    #[test]
    fn rebuild_decision_orders_reasons_and_flags_model_incompatibility() {
        let now = chrono::Utc::now().naive_utc();
        let fresh = (now - chrono::Duration::days(1)).format("%Y-%m-%d %H:%M:%S").to_string();
        let old = (now - chrono::Duration::days(20)).format("%Y-%m-%d %H:%M:%S").to_string();
        let stored = |t: &str| Some(("{}".to_string(), t.to_string()));
        assert_eq!(rebuild_decision(None, false, false, None, 0, 0, now), Some(RebuildReason::Absent));
        assert_eq!(rebuild_decision(stored(&fresh).as_ref(), false, true, Some(EMBEDDING_MODEL_ID), 3, 3, now), Some(RebuildReason::Unreadable));
        // Same dimensions, same cluster count, different model: incompatible.
        assert_eq!(rebuild_decision(stored(&fresh).as_ref(), true, true, Some("other-model"), 3, 3, now), Some(RebuildReason::IncompatibleModel));
        // A vector with NO recorded model is incompatible too — never assume.
        assert_eq!(rebuild_decision(stored(&fresh).as_ref(), true, true, None, 3, 3, now), Some(RebuildReason::IncompatibleModel));
        assert_eq!(rebuild_decision(stored(&old).as_ref(), true, true, Some(EMBEDDING_MODEL_ID), 3, 3, now), Some(RebuildReason::Stale));
        assert_eq!(rebuild_decision(stored(&fresh).as_ref(), true, true, Some(EMBEDDING_MODEL_ID), 0, 3, now), Some(RebuildReason::Format));
        assert_eq!(rebuild_decision(stored(&fresh).as_ref(), true, true, Some(EMBEDDING_MODEL_ID), 3, 3, now), None);
        // Keyword-only fingerprints have no vectors to be incompatible.
        assert_eq!(rebuild_decision(stored(&fresh).as_ref(), true, false, None, 0, 3, now), None);
        assert!(RebuildReason::Stale.fallback_allowed());
        assert!(!RebuildReason::IncompatibleModel.fallback_allowed());
        assert!(!RebuildReason::Absent.fallback_allowed());
    }
```

Then in `run_scan` (`:807-861`) replace the `needs_rebuild` boolean with `rebuild_decision(...)` (model id from `db.fingerprint_embedding_model(user_did)`), and the rebuild-failure arm becomes:

```rust
            Err(e) if reason.fallback_allowed() && stored_fingerprint.is_some() => {
                warn!(error = %e, ?reason, "Fingerprint refresh failed; using the stored (compatible) fingerprint");
                stored_fingerprint.expect("checked")
            }
            Err(e) => {
                // Absent, unreadable, or built by another embedding model: there is
                // nothing safe to fall back to. Abort before any account is scored
                // (V2-04) — a score against incompatible vectors would be stamped
                // current and hide the real state until it expired.
                // Inside run_scan_inner: this Err reaches run_scan_with's
                // single scheduling site (V5-02) — the obligation is kept and
                // the hourly retry is written before the row is finished.
                return Err(e).with_context(|| format!("fingerprint rebuild required ({reason:?}) and failed — scan aborted before scoring"));
            }
```

`build_user_fingerprint` and every `save_fingerprint_bundle` caller pass `Some(EMBEDDING_MODEL_ID)` when an embedding is written; `migrate` copies the source's `fingerprint_embedding_model` verbatim (`None` stays `None` — the v18 backfill is the only place a missing id is ever filled, and only because one model has ever existed; an import never labels vectors). The refresh → full handover (Task 9, `request_full_after_refresh`) lands in exactly this path: the queued full scan runs `run_scan`, whose `rebuild_decision` returns `IncompatibleModel` and rebuilds or aborts.

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

Run: `cargo test --test unit_direct_pairs`, `cargo test --features web --lib web::scan_job rebuild_decision`, `VERIFY_WEB`, `VERIFY_CLIPPY`.
Expected: green, zero `SKIP:`.

- [ ] **Step 5: Commit**

```bash
git add src/pipeline/amplification.rs src/web/scan_job.rs tests/unit_direct_pairs.rs
git commit -m 'refactor(344): extract scan setup helpers; context loaders return errors; rebuild decision checks the embedding model and never falls back to incompatible vectors

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01XXbqRMsMWnxFpWxs3XRqSX'
```

---

### Task 9: `run_refresh` — ownership, resume, compatibility, honest outcomes

**Why:** The job itself, built on the contracts both reviews forced: staging carries its owner (R02), inputs must be compatible before a current stamp is published — fingerprint model, blob revision, and the classifier's **model and policy** (R03, V2-01), missing context fails the run instead of lowering a score and that failure path is a mandatory deterministic test through an injectable boundary (R05, V2), the cache metric is per-run and only claims what it measured (R08), and every outcome — including "completed but some accounts were skipped" — drives the schedule explicitly (R02/R04, V2-05).

**Ownership markers.** `run_phased_scan` gains a `RunIdentity { kind: ScanKind, generation: &'static str }` parameter. At a fresh start it writes `scan_run_kind` and `scan_run_generation` to `scan_state` together with `scan_phase = gather`. On entry with a resumable marker it reads both and:
- generation ≠ current → `clear_scan_staging`, clear the three markers, fresh start (old binary's leftovers; R03);
- kind = caller's kind → resume as today;
- kind ≠ caller's kind → return `Err(PhasedScanError::OwnedByOtherKind(kind))` (a typed error the callers match on — never resume another kind's staging blindly).

**Transitions.**
- Refresh finds `OwnedByOtherKind(Full)` → `Deferred(FullScanResumable)`; the user's own scan (or the admin) drains it. Retry in an hour.
- Full scan finds `OwnedByOtherKind(Refresh)` → **drain first**: call `run_phased_scan` with `RunIdentity::refresh()` and the refresh's re-derived candidates (`list_refresh_candidates`) until it returns `Done` (if it cost-caps again, the full scan reports itself degraded/resumable exactly as today and stops — the next full scan drains again), then clear markers and fresh-start its own gather. The refresh's `refreshed_generation` is **not** marked by this path (the refresh did not complete under its own claim); the full scan's own success marks it.
- Refresh finds its own kind → resume with the current `list_refresh_candidates` as the candidate list (finalize does not need candidates; recovery for an account no longer in the list is a documented skip, as `recover_account` already handles "missing candidate").

**Evidence provenance (R03, V2-01, V3-01).** A staged verdict row is valid finalization evidence only when it names its producer and that producer is the one this binary runs:

```rust
// src/pipeline/scan_phases/staging.rs
/// The Stage-1 clean pass writes this policy label onto the rows it settles
/// (V3-01): a clean-pass row is evidence produced by the ONNX model, not by
/// the classifier, and is validated against the ONNX identity.
pub const CLEAN_PASS_POLICY: &str = "onnx-clean-pass";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassifierIdentity<'a> { pub model_id: &'a str, pub policy_version: &'a str }

/// What finalize accepts as evidence for THIS run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceContract<'a> { pub onnx_model_id: &'a str, pub classifier: ClassifierIdentity<'a> }

/// Who produced a done row, decided from its recorded provenance alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance { CleanPass, Classifier, DecodeErrorSentinel, Missing, Foreign }

impl EvidenceContract<'_> {
    pub fn provenance(&self, model_id: Option<&str>, policy_version: Option<&str>) -> Provenance {
        match (model_id, policy_version) {
            (Some(m), Some(p)) if m == self.onnx_model_id && p == CLEAN_PASS_POLICY => Provenance::CleanPass,
            (Some(m), Some(p)) if m == self.classifier.model_id && p == self.classifier.policy_version => Provenance::Classifier,
            (Some("decode-error"), _) => Provenance::DecodeErrorSentinel,
            (None, _) | (_, None) => Provenance::Missing,
            _ => Provenance::Foreign,
        }
    }
    /// Only evidence from a known, current producer counts.
    pub fn accepts(&self, model_id: Option<&str>, policy_version: Option<&str>) -> bool {
        matches!(self.provenance(model_id, policy_version), Provenance::CleanPass | Provenance::Classifier)
    }
}
```

- `gather.rs::mark_clean` (`:570-576`) writes `model_id = Some(ONNX_MODEL_ID.to_string())`, `policy_version = Some(CLEAN_PASS_POLICY.to_string())` instead of `None`/`None` — a clean-pass row now says who settled it. `confidence` stays `None`.
- `finalize.rs::verdict_for(row_by_uri, post_uri, evidence: &EvidenceContract)` returns `None` (→ `NeedsRegather`) unless `status == "done"`, `toxic_token.is_some()`, and `evidence.accepts(row.model_id, row.policy_version)`. `Missing` (pre-v18 clean rows from an old binary, or a corrupt row), `Foreign` (another model or policy) and `DecodeErrorSentinel` (#355) are all rejected; re-gather recreates clean rows **with** provenance, so a Missing row costs one bounded re-gather and never a skip.
- `PhasedScanDeps` gains `pub evidence: EvidenceContract<'a>` (replacing rev 3's `expected_classifier`). `amplification.rs` and `sweep.rs` build it from `ONNX_MODEL_ID` and the classifier's `model_id()`/`policy_version()`.
- **Zentropi identity (V3-01):** `ZentropiClassifier::policy_version()` must return the same string its verdicts carry. Store `policy_version: &'static str` at construction — `labeler_version_id.map(|s| Box::leak(s.into_boxed_str()) as &'static str).unwrap_or("zentropi-labeler")` (one leak per process; the trait returns `&'static str`) — and make `classify()` use `self.policy_version()` for the verdict's `policy_version` (drop the `labeler_version_id.clone().unwrap_or_else(...)` branch at `zentropi.rs:378-386`). `CachedClassifier` already keys hits and misses on `inner.policy_version()`, so both paths now agree with what finalize expects. Same audit for the CoPE-B RunPod classifier and the stub: `rg -n "policy_version" src/toxicity/*.rs` — every producer's advertised identity must equal what it writes.

Mandatory tests (`tests/unit_scan_phases.rs` unless noted):

```rust
    /// V3-01 (1): a staged account with one ONNX-cleared post and one
    /// classifier-settled post finalizes — no re-gather, no skip.
    #[tokio::test]
    async fn finalize_accepts_clean_pass_and_classifier_evidence_together() {
        let db = open_db().await;
        stage_account(&db, "did:plc:mixed", &[
            staged_row("at://p/1", Some(ONNX_MODEL_ID), Some(CLEAN_PASS_POLICY), false),
            staged_row("at://p/2", Some("stub"), Some("policy-1"), true),
        ]).await;
        let evidence = EvidenceContract { onnx_model_id: ONNX_MODEL_ID, classifier: ClassifierIdentity { model_id: "stub", policy_version: "policy-1" } };
        assert_eq!(finalize_account_with(&db, TEST_USER, "did:plc:mixed", evidence).await, FinalizeOutcome::Scored);
        assert!(db.get_account_by_did(TEST_USER, "did:plc:mixed").await.unwrap().is_some(), "a score was persisted");
    }

    /// V3-01 (2): change either producer's revision and its evidence is rejected.
    #[tokio::test]
    async fn finalize_rejects_evidence_from_another_producer_revision() {
        let db = open_db().await;
        stage_account(&db, "did:plc:mixed", &[
            staged_row("at://p/1", Some(ONNX_MODEL_ID), Some(CLEAN_PASS_POLICY), false),
            staged_row("at://p/2", Some("stub"), Some("policy-1"), true),
        ]).await;
        let other_classifier = EvidenceContract { onnx_model_id: ONNX_MODEL_ID, classifier: ClassifierIdentity { model_id: "stub", policy_version: "policy-2" } };
        assert_eq!(finalize_account_with(&db, TEST_USER, "did:plc:mixed", other_classifier).await, FinalizeOutcome::NeedsRegather);
        let other_onnx = EvidenceContract { onnx_model_id: "other-onnx", classifier: ClassifierIdentity { model_id: "stub", policy_version: "policy-1" } };
        assert_eq!(finalize_account_with(&db, TEST_USER, "did:plc:mixed", other_onnx).await, FinalizeOutcome::NeedsRegather);
    }

    /// V3-01 (4): missing or corrupt provenance and the decode-error sentinel
    /// are never accepted as clean-pass evidence.
    #[test]
    fn provenance_distinguishes_missing_foreign_and_sentinel() {
        let e = EvidenceContract { onnx_model_id: "onnx", classifier: ClassifierIdentity { model_id: "cls", policy_version: "p" } };
        assert_eq!(e.provenance(Some("onnx"), Some(CLEAN_PASS_POLICY)), Provenance::CleanPass);
        assert_eq!(e.provenance(Some("cls"), Some("p")), Provenance::Classifier);
        assert_eq!(e.provenance(None, None), Provenance::Missing);
        assert_eq!(e.provenance(Some("onnx"), None), Provenance::Missing);
        assert_eq!(e.provenance(Some("decode-error"), Some("p")), Provenance::DecodeErrorSentinel);
        assert_eq!(e.provenance(Some("cls"), Some("q")), Provenance::Foreign);
        assert_eq!(e.provenance(Some("onnx"), Some("p")), Provenance::Foreign, "ONNX model with the classifier's policy is not a clean-pass row");
        for (m, p) in [(None, None), (Some("decode-error"), Some("p")), (Some("cls"), Some("q"))] {
            assert!(!e.accepts(m, p));
        }
    }

    /// V3-01 (1, gather side): mark_clean records the ONNX producer.
    #[tokio::test]
    async fn clean_pass_rows_carry_onnx_provenance() {
        let db = open_db().await;
        let fp = astrophysics_fingerprint();
        let weights = ThreatWeights::default();
        let originals: Vec<_> = (0..20).map(|i| make_post(&format!("at://c/{i}"), "supernova remnants and pulsars")).collect();
        let sample = PostSample { total_posts: originals.len(), originals, replies: vec![], quotes: vec![], reply_ratio: 0.0, quote_ratio: 0.0 };
        let fetcher = CannedFetcher { sample, parents: HashMap::new() };
        // Non-clean Stage 1 (so no early exit) but a clean pass that clears
        // every post: every staged row is settled by the ONNX producer.
        let (outcome, _) = gather_account(&db, TEST_USER, &fetcher, &FixedScorer(0.9), &FixedCleanPass(0.0), &inputs(&fp, &weights)).await.unwrap();
        assert_eq!(outcome, charcoal::pipeline::scan_phases::gather::GatherOutcome::Enqueued);
        for row in db.fetch_account_verdicts(TEST_USER, "did:plc:clean").await.unwrap() {
            assert_eq!(row.model_id.as_deref(), Some(ONNX_MODEL_ID));
            assert_eq!(row.policy_version.as_deref(), Some(CLEAN_PASS_POLICY));
            assert_eq!(row.toxic_token, Some(false));
        }
    }
```

`tests/unit_classifier.rs` (V3-01 (3)):

```rust
/// A configured labeler version is the classifier's policy identity on every
/// path: advertised, written on cache misses, matched on cache hits.
#[tokio::test]
async fn zentropi_advertised_policy_matches_written_policy_on_miss_and_hit() {
    let server = wiremock::MockServer::start().await;   // the file's existing Zentropi mock pattern
    mock_zentropi_verdict(&server, /* toxic */ false).await;
    let inner = ZentropiClassifier::new_for_test(server.uri(), Some("labeler-v42".to_string()));
    assert_eq!(inner.policy_version(), "labeler-v42");
    let db = test_db();
    let stats = Arc::new(CacheStats::default());
    let cached = CachedClassifier::new(Box::new(inner), db.clone(), stats.clone());
    let miss = cached.classify_batch(&["hello".to_string()]).await.unwrap();
    let hit = cached.classify_batch(&["hello".to_string()]).await.unwrap();
    for outcome in [&miss[0], &hit[0]] {
        let ItemOutcome::Verdict(v) = outcome else { panic!("verdict") };
        assert_eq!(v.policy_version, "labeler-v42");
        assert_eq!(v.model_id, cached.model_id());
    }
    assert_eq!((stats.hits(), stats.misses()), (1, 1));
    // …and finalize accepts a row written from either.
    let e = EvidenceContract { onnx_model_id: ONNX_MODEL_ID, classifier: ClassifierIdentity { model_id: cached.model_id(), policy_version: cached.policy_version() } };
    assert!(e.accepts(Some(cached.model_id()), Some("labeler-v42")));
}
```

(`new_for_test` is whatever constructor the file already uses to point the client at wiremock — reuse it; if the labeler version is only settable through `Config`, add a `#[cfg(test)]` setter.)

**Outcomes (V2-05).**
```rust
pub enum RefreshOutcome {
    Completed { candidates: usize, scored: usize },              // every candidate re-scored, staging drained
    CompletedWithSkips { candidates: usize, scored: usize, skipped: usize }, // drained, but skipped accounts stay expired
    NothingDue,
    Deferred(DeferReason),   // FullScanResumable | NoFingerprint | IncompatibleFingerprint
    Resumable,               // cost cap / transient interruption: markers left, own kind
}
pub struct Bookkeeping { pub prove_revision: bool, pub retry: bool, pub request_full: bool, pub degraded: bool }
impl RefreshOutcome { pub fn bookkeeping(&self) -> Bookkeeping; pub fn label(&self) -> String; }
```
`Completed`/`NothingDue` → `prove_revision` (`schedule_after_success`: nightly + `refreshed_generation`). `CompletedWithSkips` → `retry` (skipped High/Elevated rows are still expired candidates; the hourly retry picks them up; successful writes are kept), `degraded`. `Deferred(_)`/`Resumable` → `retry`, `degraded`; `NoFingerprint`/`IncompatibleFingerprint` additionally `request_full` (Task 5's `full_requested_at` on the refresh's own row, so the user's next admitted run is a full scan that rebuilds under Task 8's `rebuild_decision`). An `Err` from the run → row `failed`, `schedule_retry`. `degraded` is what `finish_scan` shows in the status, so completed-but-skipped work does not read as clean. The full-scan cooldown marker is never written by a refresh.

**Files:**
- Create: `src/web/refresh_scan.rs`; Modify: `src/web/mod.rs`
- Modify: `src/pipeline/scan_phases/mod.rs:149-215` (`RunIdentity`, markers, `PhasedScanError`, `ScanSummary.final_phase`/`skipped`), `staging.rs` (`CLEAN_PASS_POLICY`, `ClassifierIdentity`, `EvidenceContract`, `Provenance`), `gather.rs:570-576` (`mark_clean` provenance), `finalize.rs:252-268` (`verdict_for` provenance check), `PhasedScanDeps` (`evidence`), `src/toxicity/zentropi.rs:378-408` (`policy_version` = configured labeler id, used by `classify`), `src/pipeline/amplification.rs:520-556` (pass identity + evidence contract), `src/pipeline/sweep.rs` (same), `src/web/scan_job.rs` (`launch_scan(kind)`, drain transition classified via `classify_full_scan`, `set_progress`/`finish_scan` pub(crate), `record_scan_outcome` label), `src/web/admitter.rs:497-520`, `src/db/traits.rs` (+ `request_full_after_refresh(user_did)`)
- Test: `src/web/refresh_scan.rs` inline (pure functions **and** the mandatory missing-context failure test through `RefreshContextSource`); `tests/unit_scan_phases.rs` (append); `src/web/admitter.rs` (kinds)

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
/// The fallible context loads, behind a trait so the failure path is testable
/// without models (V2 mandatory test). Production impl wraps Task 8's helpers.
#[async_trait]
pub trait RefreshContextSource: Send + Sync {
    async fn fingerprint(&self, user_did: &str) -> anyhow::Result<Option<(TopicFingerprint, Option<Vec<f64>>, Vec<Vec<f64>>, Option<String> /*embedding_model_id*/)>>;
    async fn candidates(&self, user_did: &str) -> anyhow::Result<Vec<RefreshCandidate>>;
    async fn protected_posts_embeddings(&self, actor_handle: &str) -> anyhow::Result<Vec<(String, Vec<f64>)>>;
    async fn pile_on(&self, user_did: &str) -> anyhow::Result<HashSet<String>>;
    async fn direct_pairs(&self, user_did: &str, amplifier_did: &str) -> anyhow::Result<Vec<(String, String)>>;
    async fn median_engagement(&self, user_did: &str) -> anyhow::Result<f64>;
}
pub struct RefreshPlan { pub fingerprint: TopicFingerprint, pub protected_embedding: Option<Vec<f64>>, pub centroids: Vec<Vec<f64>>, pub protected_posts: Vec<(String, Vec<f64>)>, pub median_engagement: f64, pub candidates: Vec<CandidateInput> }
/// Everything before the pipeline. Ok(Ok(plan)) | Ok(Err(defer reason)) | Err (missing context — nothing is written).
pub async fn prepare_refresh(ctx: &dyn RefreshContextSource, user_did: &str, actor_handle: &str) -> anyhow::Result<Result<RefreshPlan, DeferReason>>;
/// Generic over the pipeline so a test can pass one that must not be called.
/// Every fallible step inside one captured outcome (V3-05). `setup` builds the
/// context source AND the pipeline (scorers, client) — a failure there is a
/// failed refresh with a retry, not a bare slot failure.
#[async_trait] pub trait RefreshBookkeeping: Send + Sync { /* reset_markers, record_outcome, request_full, schedule_success, schedule_retry — see below */ }
pub struct DbBookkeeping(pub Arc<dyn Database>);
pub(crate) async fn run_refresh_with<S, F, Fut>(scan_manager: Arc<RwLock<ScanManager>>, books: &dyn RefreshBookkeeping, clock: &dyn Fn() -> DateTime<Utc>,
    user_did: &str, actor_handle: &str, claim_id: &str, setup: S) -> anyhow::Result<ScanReport>  // same report type as run_scan (V4-04); clock read after the attempt (V6-02)
    where S: FnOnce() -> anyhow::Result<(Box<dyn RefreshContextSource>, F)>, F: FnOnce(RefreshPlan) -> Fut, Fut: Future<Output = anyhow::Result<ScanSummary>>;
pub fn classify_refresh(candidates: usize, summary: &ScanSummary) -> RefreshOutcome;
/// Production: real context source + the real run_phased_scan, both built inside `setup`.
pub(crate) async fn run_refresh(config, db, models, scan_manager, user_did, actor_handle, claim_id) -> anyhow::Result<ScanReport>;
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
        assert_eq!(RefreshOutcome::Completed { candidates: 3, scored: 3 }.label(), "completed");
        assert_eq!(RefreshOutcome::CompletedWithSkips { candidates: 3, scored: 2, skipped: 1 }.label(), "completed_with_skips");
    }

    /// V2-05: only complete work proves the revision; everything else
    /// retries and shows as degraded; missing/incompatible fingerprints also
    /// request a full scan.
    #[test]
    fn bookkeeping_follows_the_completion_contract() {
        let b = |o: RefreshOutcome| o.bookkeeping();
        let ok = b(RefreshOutcome::Completed { candidates: 1, scored: 1 });
        assert!(ok.prove_revision && !ok.retry && !ok.request_full && !ok.degraded);
        let none = b(RefreshOutcome::NothingDue);
        assert!(none.prove_revision && !none.retry && !none.degraded);
        let skips = b(RefreshOutcome::CompletedWithSkips { candidates: 3, scored: 2, skipped: 1 });
        assert!(!skips.prove_revision && skips.retry && !skips.request_full && skips.degraded);
        let res = b(RefreshOutcome::Resumable);
        assert!(!res.prove_revision && res.retry && res.degraded);
        let full = b(RefreshOutcome::Deferred(DeferReason::FullScanResumable));
        assert!(!full.prove_revision && full.retry && !full.request_full);
        for r in [DeferReason::NoFingerprint, DeferReason::IncompatibleFingerprint] {
            let d = b(RefreshOutcome::Deferred(r));
            assert!(d.retry && d.request_full && !d.prove_revision);
        }
    }

    /// Counting bookkeeping stub: records what the run asked for, can fail
    /// `reset_markers` on cue, and otherwise delegates to the database so the
    /// assertions below read real rows.
    struct CountingBooks { inner: DbBookkeeping, fail_reset: bool, retries: std::sync::atomic::AtomicUsize, successes: std::sync::atomic::AtomicUsize }
    #[async_trait]
    impl RefreshBookkeeping for CountingBooks {
        async fn reset_markers(&self, u: &str, c: &str) -> anyhow::Result<()> {
            if self.fail_reset { anyhow::bail!("scan_state write failed") } else { self.inner.reset_markers(u, c).await }
        }
        async fn record_outcome(&self, u: &str, l: &str, at: DateTime<Utc>) -> anyhow::Result<()> { self.inner.record_outcome(u, l, at).await }
        async fn request_full(&self, u: &str) -> anyhow::Result<()> { self.inner.request_full(u).await }
        async fn schedule_success(&self, u: &str, now: DateTime<Utc>) { self.successes.fetch_add(1, std::sync::atomic::Ordering::SeqCst); self.inner.schedule_success(u, now).await }
        async fn schedule_retry(&self, u: &str, now: DateTime<Utc>) { self.retries.fetch_add(1, std::sync::atomic::Ordering::SeqCst); self.inner.schedule_retry(u, now).await }
    }

    /// A High row, a claimed refresh and a status entry — the state every
    /// failure-path test starts from.
    async fn refresh_in_flight(db: &Arc<dyn Database>) -> (String, Arc<RwLock<ScanManager>>, Vec<crate::db::models::StoredScore>) {
        db.upsert_user("did:plc:u", "u.h").await.unwrap();
        let mut high = crate::db::models::AccountScore::default_for_test("did:plc:high");
        high.threat_score = Some(60.0);
        high.threat_tier = Some("High".into());
        high.scoring_confidence = Some("high".into());
        db.upsert_account_score("did:plc:u", &high).await.unwrap();
        let before = db.export_scores("did:plc:u").await.unwrap();
        db.enqueue_refresh_scan("did:plc:u").await.unwrap();
        let claim = db.claim_next_scan(1, 60).await.unwrap().unwrap();
        (claim.claim_id.clone(), manager_with_running_scan(&claim.claim_id), before)
    }

    /// Shared assertions for every failure path: exactly one scheduling call
    /// and it is the retry; nothing written; outcome recorded; one-hour
    /// deadline; revision not proven; queue row completion = failed.
    async fn assert_failed_with_one_retry(db: &Arc<dyn Database>, books: &CountingBooks, before: &[crate::db::models::StoredScore]) {
        let retries = books.retries.load(std::sync::atomic::Ordering::SeqCst);
        let successes = books.successes.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!((retries, successes), (1, 0), "exactly one scheduling call, and it is the retry");
        assert_eq!(db.export_scores("did:plc:u").await.unwrap(), before, "no score written or restamped");
        assert_eq!(db.get_scan_state("did:plc:u", "refresh_last_outcome").await.unwrap().as_deref(), Some("failed"));
        let next = db.next_refresh_at("did:plc:u").await.unwrap().expect("retry scheduled");
        let next = chrono::DateTime::parse_from_rfc3339(&next).unwrap();
        let delta = next.signed_duration_since(chrono::Utc::now());
        assert!(delta > chrono::Duration::minutes(55) && delta <= chrono::Duration::hours(1), "one-hour retry, got {delta}");
        assert_ne!(db.refreshed_generation("did:plc:u").await.unwrap().as_deref(), Some(scoring_revision()), "not proven");
        let row = db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == "did:plc:u").unwrap();
        assert_eq!(row.completion, Some(crate::db::FinishCompletion::Failed));
    }

    /// R05, mandatory (V2): a context load failure fails the run — no score
    /// is written, the pipeline is never entered, the outcome is recorded as
    /// failed, and the retry is scheduled. Runs without models.
    #[tokio::test]
    async fn missing_context_fails_the_refresh_without_writing() {
        struct Failing;
        #[async_trait]
        impl RefreshContextSource for Failing {
            async fn fingerprint(&self, _: &str) -> anyhow::Result<Option<(TopicFingerprint, Option<Vec<f64>>, Vec<Vec<f64>>, Option<String>)>> {
                Ok(Some((TopicFingerprint { clusters: vec![], post_count: 0 }, None, vec![], None)))
            }
            async fn candidates(&self, _: &str) -> anyhow::Result<Vec<RefreshCandidate>> {
                Ok(vec![RefreshCandidate { did: "did:plc:high".into(), handle: "high.h".into(), graph_distance: None }])
            }
            async fn protected_posts_embeddings(&self, _: &str) -> anyhow::Result<Vec<(String, Vec<f64>)>> {
                anyhow::bail!("getAuthorFeed: 502 from the AppView")
            }
            async fn pile_on(&self, _: &str) -> anyhow::Result<HashSet<String>> { Ok(HashSet::new()) }
            async fn direct_pairs(&self, _: &str, _: &str) -> anyhow::Result<Vec<(String, String)>> { Ok(vec![]) }
            async fn median_engagement(&self, _: &str) -> anyhow::Result<f64> { Ok(0.0) }
        }
        let db = test_db();
        let (claim_id, mgr, before) = refresh_in_flight(&db).await;
        let books = CountingBooks { inner: DbBookkeeping(db.clone()), fail_reset: false, retries: 0.into(), successes: 0.into() };
        let result = run_refresh_with(mgr, &books, &chrono::Utc::now, "did:plc:u", "u.h", &claim_id, || {
            let ctx: Box<dyn RefreshContextSource> = Box::new(Failing);
            Ok((ctx, |_plan: RefreshPlan| async { panic!("the pipeline must not run when context is missing") }))
        })
        .await;
        assert!(result.is_err());
        assert_failed_with_one_retry(&db, &books, &before).await;
    }

    /// V3-05 (1): a scorer/client setup failure BEFORE context preparation
    /// gets the same lifecycle — failed, one retry, nothing written.
    #[tokio::test]
    async fn setup_failure_records_failed_and_schedules_the_retry() {
        let db = test_db();
        let (claim_id, mgr, before) = refresh_in_flight(&db).await;
        let books = CountingBooks { inner: DbBookkeeping(db.clone()), fail_reset: false, retries: 0.into(), successes: 0.into() };
        let result = run_refresh_with::<_, fn(RefreshPlan) -> std::future::Ready<anyhow::Result<ScanSummary>>, _>(mgr, &books, &chrono::Utc::now, "did:plc:u", "u.h", &claim_id, || {
            anyhow::bail!("CHARCOAL_CLASSIFIER is unset — build_from_env failed")
        })
        .await;
        assert!(result.is_err());
        assert_failed_with_one_retry(&db, &books, &before).await;
    }

    /// V6-02, refresh side: a 90-minute failed attempt is retried an hour
    /// after it ENDED. Same FakeClock as scan_job's test; the setup closure
    /// advances it before failing.
    #[tokio::test]
    async fn a_failed_refresh_is_retried_an_hour_after_it_ended_not_began() {
        use crate::web::refresh::REFRESH_RETRY_HOURS;
        let db = test_db();
        let (claim_id, mgr, before) = refresh_in_flight(&db).await;
        let books = CountingBooks { inner: DbBookkeeping(db.clone()), fail_reset: false, retries: 0.into(), successes: 0.into() };
        let t0 = chrono::Utc::now();
        let clock = std::sync::Arc::new(std::sync::Mutex::new(t0));
        let advance = clock.clone();
        let now_fn = { let c = clock.clone(); move || *c.lock().unwrap() };
        let result = run_refresh_with::<_, fn(RefreshPlan) -> std::future::Ready<anyhow::Result<ScanSummary>>, _>(mgr, &books, &now_fn, "did:plc:u", "u.h", &claim_id, move || {
            *advance.lock().unwrap() += chrono::Duration::minutes(90);
            anyhow::bail!("AppView unreachable for the whole attempt")
        })
        .await;
        assert!(result.is_err());
        let end = t0 + chrono::Duration::minutes(90);
        let deadline = chrono::DateTime::parse_from_rfc3339(&db.next_refresh_at("did:plc:u").await.unwrap().unwrap()).unwrap();
        assert_eq!(deadline, end + chrono::Duration::hours(REFRESH_RETRY_HOURS as i64));
        assert_eq!(db.export_scores("did:plc:u").await.unwrap(), before);
        db.finish_queued_scan("did:plc:u", &claim_id, crate::db::FinishCompletion::Failed, Some("down")).await.unwrap();
        assert_eq!(crate::web::refresh::enqueue_due_refreshes(&db, end + chrono::Duration::minutes(30), std::time::Duration::from_secs(24 * 3600)).await, 0);
        assert_eq!(crate::web::refresh::enqueue_due_refreshes(&db, end + chrono::Duration::minutes(61), std::time::Duration::from_secs(24 * 3600)).await, 1);
        let row = db.list_scan_queue().await.unwrap().into_iter().find(|r| r.user_did == "did:plc:u").unwrap();
        assert_eq!(row.kind, ScanKind::Refresh, "no full obligation ⇒ retried as a refresh");
    }

    /// V3-05 (2): a marker-initialisation failure with the database
    /// otherwise available gets the same lifecycle.
    #[tokio::test]
    async fn marker_reset_failure_records_failed_and_schedules_the_retry() {
        let db = test_db();
        let (claim_id, mgr, before) = refresh_in_flight(&db).await;
        let books = CountingBooks { inner: DbBookkeeping(db.clone()), fail_reset: true, retries: 0.into(), successes: 0.into() };
        let result = run_refresh_with::<_, fn(RefreshPlan) -> std::future::Ready<anyhow::Result<ScanSummary>>, _>(mgr, &books, &chrono::Utc::now, "did:plc:u", "u.h", &claim_id, || {
            panic!("setup must not run when the markers could not be reset")
        })
        .await;
        assert!(result.is_err());
        assert_failed_with_one_retry(&db, &books, &before).await;
    }

    /// A successful lookup that legitimately finds no pairs is NOT a failure
    /// (the other half of R05).
    #[tokio::test]
    async fn no_pairs_is_a_valid_plan() {
        struct Empty;
        #[async_trait]
        impl RefreshContextSource for Empty {
            async fn fingerprint(&self, _: &str) -> anyhow::Result<Option<(TopicFingerprint, Option<Vec<f64>>, Vec<Vec<f64>>, Option<String>)>> {
                Ok(Some((TopicFingerprint { clusters: vec![], post_count: 0 }, None, vec![], None)))
            }
            async fn candidates(&self, _: &str) -> anyhow::Result<Vec<RefreshCandidate>> {
                Ok(vec![RefreshCandidate { did: "did:plc:a".into(), handle: "a.h".into(), graph_distance: None }])
            }
            async fn protected_posts_embeddings(&self, _: &str) -> anyhow::Result<Vec<(String, Vec<f64>)>> { Ok(vec![]) }
            async fn pile_on(&self, _: &str) -> anyhow::Result<HashSet<String>> { Ok(HashSet::new()) }
            async fn direct_pairs(&self, _: &str, _: &str) -> anyhow::Result<Vec<(String, String)>> { Ok(vec![]) }
            async fn median_engagement(&self, _: &str) -> anyhow::Result<f64> { Ok(0.0) }
        }
        let plan = prepare_refresh(&Empty, "did:plc:u", "u.h").await.unwrap().expect("not deferred");
        assert_eq!(plan.candidates.len(), 1);
        assert_eq!(plan.candidates[0].direct_pairs, None, "follower path, not an error");
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
        assert_eq!(db.get_scan_state(TEST_USER, RUN_GENERATION_KEY).await.unwrap().as_deref(), Some(scoring_revision()));

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
            scoring_generation: scoring_revision().to_string(),
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

Write the two `one_candidate_deps_*` helpers (a `PhasedScanDeps` over `CannedFetcher` + `FixedScorer(0.9)` + a `StubClassifier::with_script` that yields `CostCeilingExceeded` on its first call, resp. a benign verdict) and `finalize_account_with(db, user, account, evidence: EvidenceContract)` (a wrapper over `finalize_account`), `stage_account(db, did, rows)` and `staged_row(uri, model_id, policy, toxic)` (stash a v3 blob whose sample lists the rows' posts, enqueue the rows, mark them done with the given provenance) against the file's existing fixtures; the exact trait-method names for staging rows (`enqueue_classifications`, `record_classification_verdicts`) are in `src/db/traits.rs` — match their signatures if they differ from the sketch above.

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

`PhasedScanDeps` gains `pub evidence: EvidenceContract<'a>` (see "Evidence provenance" above); `finalize.rs` `verdict_for` takes it and returns `None` unless `evidence.accepts(row.model_id.as_deref(), row.policy_version.as_deref())`. `amplification.rs` and `sweep.rs` pass `RunIdentity::full()` and an `EvidenceContract` built from `ONNX_MODEL_ID` and the classifier's identity. **`amplification::run` always enters `run_phased_scan` when a scorer is present (V4-02):** the `Some(_) if candidates.is_empty() => (0, false)` arm at `amplification.rs:516` is removed — with an empty candidate list a fresh start is a no-op gather → empty burst → empty finalize → `Done` (cheap), and a resumable marker is resumed (own kind) or surfaces `OwnedByOtherKind` (refresh-owned, so `run_scan` drains) exactly as with candidates. The `None` (no scorer, CLI `--analyze` off) arm stays. Tests in `tests/unit_scan_phases.rs`: **`a_resumed_run_keeps_its_earlier_skips` (V5-01):** two candidates, a `CannedFetcher` that fails the feed for the first (a recorded gather skip) and a `StubClassifier` scripted to cost-cap on the first chunk; first `run_phased_scan(…, RunIdentity::full())` → `degraded`, marker `burst`, `scan_skips = 1`; second call with a classifier that succeeds → the summary's `degraded` is `false` but `final_phase = Some("done")` and `skipped = Some(1)`, so `classify_full_scan` = `CompleteWithSkips { n: 1 }` and `classify_refresh` (same summary, one candidate) = `CompletedWithSkips { skipped: 1, .. }` → `bookkeeping()` retries and does not prove; a third case reads the summary with `skipped = None` (simulate by a `count_scan_skips` failure through `SqliteDatabase::with_conn` dropping the `scan_skips` table before the second call) → `CompleteUnverified` / `CompletedUnverified`, no proof. `empty_discovery_resumes_full_owned_staging` (seed a full-owned `burst` marker with one pending row via the canned classifier; call `run_phased_scan(db, user, &[], deps, RunIdentity::full())`; assert the row is finalized and the marker reaches `done`), `empty_discovery_surfaces_refresh_owned_staging` (refresh-owned marker → `Err(OwnedByOtherKind(Refresh))`, marker untouched), `empty_discovery_with_no_staging_completes` (no marker → `Done`, `ScanSummary { degraded: false, final_phase: Some("done"), .. }` → `classify_full_scan` = `Complete`). And `run_scan` records the completion from `classify_full_scan` on the summary's `final_phase`, so a drain or resume that is still interrupted after an empty discovery is `Resumable`: no marker, no proof, retry, obligation kept. `ScanSummary` gains `final_phase: Option<String>` (the `scan_phase` marker at return) and `skipped: Option<i64>` (`count_scan_skips` at return; `None` when the count could not be read — never `-1`, never `0`), filled by `run_phased_scan`, so completion can be classified from the summary alone. Because `scan_skips` is cleared only at a fresh start (`mod.rs:195`), the count a resumed invocation reads includes the skips of every earlier attempt in the same staged run — which is exactly what V5-01 needs it to. The rev-3 tests `finalize_rejects_verdicts_from_another_policy` / `…_classifier_model` are subsumed by `finalize_rejects_evidence_from_another_producer_revision`.

- [ ] **Step 4: `run_refresh`**

`src/web/refresh_scan.rs` (module doc: what a refresh shares with a full scan and what it does not — Constellation, follower expansion, `classify_relationships`, fingerprint rebuilds):

```rust
pub const REFRESH_HORIZON_DAYS: i64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferReason { FullScanResumable, NoFingerprint, IncompatibleFingerprint }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshOutcome {
    Completed { candidates: usize, scored: usize },
    CompletedWithSkips { candidates: usize, scored: usize, skipped: usize },
    /// Drained to `done` but the skip count could not be read (V5-01): fulfilled, never proof.
    CompletedUnverified { candidates: usize, scored: usize },
    NothingDue,
    Deferred(DeferReason),
    Resumable,
}

/// What an outcome does to the schedule, the proof and the status (V2-05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bookkeeping {
    pub prove_revision: bool,
    pub retry: bool,
    pub request_full: bool,
    pub degraded: bool,
}

impl RefreshOutcome {
    /// Stable `scan_state.refresh_last_outcome` value; the runbook greps it.
    pub fn label(&self) -> String {
        match self {
            RefreshOutcome::Completed { .. } => "completed".to_string(),
            RefreshOutcome::CompletedWithSkips { .. } => "completed_with_skips".to_string(),
            RefreshOutcome::CompletedUnverified { .. } => "completed_unverified".to_string(),
            RefreshOutcome::NothingDue => "nothing_due".to_string(),
            RefreshOutcome::Deferred(DeferReason::FullScanResumable) => "deferred:full_scan_resumable".to_string(),
            RefreshOutcome::Deferred(DeferReason::NoFingerprint) => "deferred:no_fingerprint".to_string(),
            RefreshOutcome::Deferred(DeferReason::IncompatibleFingerprint) => "deferred:incompatible_fingerprint".to_string(),
            RefreshOutcome::Resumable => "resumable".to_string(),
        }
    }

    pub fn scored(&self) -> usize {
        match self {
            RefreshOutcome::Completed { scored, .. } | RefreshOutcome::CompletedWithSkips { scored, .. } | RefreshOutcome::CompletedUnverified { scored, .. } => *scored,
            _ => 0,
        }
    }

    pub fn bookkeeping(&self) -> Bookkeeping {
        match self {
            RefreshOutcome::Completed { .. } | RefreshOutcome::NothingDue => Bookkeeping { prove_revision: true, retry: false, request_full: false, degraded: false },
            RefreshOutcome::CompletedWithSkips { .. } | RefreshOutcome::CompletedUnverified { .. } | RefreshOutcome::Resumable => Bookkeeping { prove_revision: false, retry: true, request_full: false, degraded: true },
            RefreshOutcome::Deferred(DeferReason::FullScanResumable) => Bookkeeping { prove_revision: false, retry: true, request_full: false, degraded: true },
            RefreshOutcome::Deferred(DeferReason::NoFingerprint | DeferReason::IncompatibleFingerprint) => Bookkeeping { prove_revision: false, retry: true, request_full: true, degraded: true },
        }
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

/// Everything before the pipeline, through the injectable boundary.
/// `Err` = required context could not be loaded (R05) — the caller fails
/// the run and writes nothing. `Ok(Err(reason))` = defer.
pub async fn prepare_refresh(
    ctx: &dyn RefreshContextSource,
    user_did: &str,
    actor_handle: &str,
) -> anyhow::Result<Result<RefreshPlan, DeferReason>> {
    // 1. Fingerprint: read, never rebuilt. Missing or incompatible ⇒ defer and
    //    ask for a full scan (R03, V2-04: the full scan's rebuild_decision
    //    enforces compatibility; this is only the request).
    let Some((fingerprint, protected_embedding, centroids, model_id)) = ctx.fingerprint(user_did).await? else {
        return Ok(Err(DeferReason::NoFingerprint));
    };
    if protected_embedding.is_some() && model_id.as_deref() != Some(EMBEDDING_MODEL_ID) {
        warn!(user_did, ?model_id, "fingerprint embeddings are from another model — deferring to a full scan");
        return Ok(Err(DeferReason::IncompatibleFingerprint));
    }
    // 2. Candidates from the table.
    let rows = ctx.candidates(user_did).await?;
    // 3. Required context (R05): any failure here is an Err. Missing context
    //    would systematically LOWER every High/Elevated score we are about to
    //    overwrite.
    let protected_posts = ctx.protected_posts_embeddings(actor_handle).await?;
    let pile_on = ctx.pile_on(user_did).await?;
    let median_engagement = ctx.median_engagement(user_did).await?;
    let mut candidates = Vec::with_capacity(rows.len());
    for row in &rows {
        let pairs = ctx.direct_pairs(user_did, &row.did).await?;
        candidates.push(to_candidate(row, pairs, &pile_on));
    }
    Ok(Ok(RefreshPlan { fingerprint, protected_embedding, centroids, protected_posts, median_engagement, candidates }))
}

/// Everything a refresh writes about itself, behind a trait so the failure
/// paths are testable without a database that fails on cue (V3-05). The
/// production impl is `DbBookkeeping` over the Task 6 functions.
#[async_trait]
pub trait RefreshBookkeeping: Send + Sync {
    /// Zero the eight `refresh_*` keys and record `refresh_last_run_id` (R08).
    async fn reset_markers(&self, user_did: &str, claim_id: &str) -> anyhow::Result<()>;
    async fn record_outcome(&self, user_did: &str, label: &str, at: DateTime<Utc>) -> anyhow::Result<()>;
    async fn request_full(&self, user_did: &str) -> anyhow::Result<()>;
    /// Best-effort by contract (Task 6): logs and counts a failure, never returns it.
    /// `now` is the attempt's END instant, read from the injected clock (V6-02).
    async fn schedule_success(&self, user_did: &str, now: DateTime<Utc>);
    async fn schedule_retry(&self, user_did: &str, now: DateTime<Utc>);
}

pub struct DbBookkeeping(pub Arc<dyn Database>);

#[async_trait]
impl RefreshBookkeeping for DbBookkeeping {
    async fn reset_markers(&self, user_did: &str, claim_id: &str) -> anyhow::Result<()> {
        for (key, value) in [
            ("refresh_last_run_id", claim_id),
            ("refresh_last_outcome", "running"),
            ("refresh_last_run_at", ""),
            ("refresh_candidates", "0"),
            ("refresh_scored", "0"),
            ("refresh_feed_cache_hits", "0"),
            ("refresh_feed_cache_misses", "0"),
            ("refresh_feed_cache_applicable", "0"),
        ] {
            self.0.set_scan_state(user_did, key, value).await?;
        }
        Ok(())
    }
    async fn record_outcome(&self, user_did: &str, label: &str, at: DateTime<Utc>) -> anyhow::Result<()> {
        self.0.set_scan_state(user_did, "refresh_last_outcome", label).await?;
        self.0.set_scan_state(user_did, "refresh_last_run_at", &at.to_rfc3339()).await
    }
    async fn request_full(&self, user_did: &str) -> anyhow::Result<()> {
        self.0.request_full_after_refresh(user_did).await
    }
    async fn schedule_success(&self, user_did: &str, now: DateTime<Utc>) {
        crate::web::refresh::schedule_after_success(self.0.as_ref(), user_did, now).await
    }
    async fn schedule_retry(&self, user_did: &str, now: DateTime<Utc>) {
        crate::web::refresh::schedule_retry(self.0.as_ref(), user_did, now).await
    }
}

/// The run. EVERY fallible step — marker reset, scorer/client construction,
/// context loads, the pipeline — happens inside one captured outcome, so a
/// failure anywhere reaches exactly one scheduling site (V3-05). `setup`
/// builds both the context source and the pipeline; a test passes a setup
/// that fails, or one whose pipeline panics if reached.
pub(crate) async fn run_refresh_with<S, F, Fut>(
    scan_manager: Arc<RwLock<ScanManager>>,
    books: &dyn RefreshBookkeeping,
    clock: &dyn Fn() -> DateTime<Utc>,
    user_did: &str,
    actor_handle: &str,
    claim_id: &str,
    setup: S,
) -> anyhow::Result<ScanReport>
where
    S: FnOnce() -> anyhow::Result<(Box<dyn RefreshContextSource>, F)>,
    F: FnOnce(RefreshPlan) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<ScanSummary>>,
{
    let started = clock(); // telemetry only (V6-02)
    let outcome: anyhow::Result<RefreshOutcome> = async {
        books.reset_markers(user_did, claim_id).await.context("resetting refresh markers")?;
        let (ctx, pipeline) = setup().context("refresh setup")?;
        let plan = match prepare_refresh(ctx.as_ref(), user_did, actor_handle).await? {
            Ok(plan) => plan,
            Err(reason) => return Ok(RefreshOutcome::Deferred(reason)),
        };
        let candidates = plan.candidates.len();
        // Ownership is enforced inside run_phased_scan (R02). NothingDue is
        // decided AFTER it on purpose: a refresh with no candidates still
        // resumes its own leftover staging.
        let summary = match pipeline(plan).await {
            Ok(s) => s,
            Err(e) if matches!(e.downcast_ref::<PhasedScanError>(), Some(PhasedScanError::OwnedByOtherKind(ScanKind::Full))) => {
                return Ok(RefreshOutcome::Deferred(DeferReason::FullScanResumable));
            }
            Err(e) => return Err(e),
        };
        Ok(classify_refresh(candidates, &summary))
    }
    .await;

    // Read the clock AFTER the attempt: the retry deadline and the recorded
    // run time both describe when this attempt ENDED (V6-02).
    let now = clock();
    debug!(user_did, attempt_secs = (now - started).num_seconds(), "refresh attempt finished");
    let (result, label, books_for) = match &outcome {
        Ok(o) => {
            let b = o.bookkeeping();
            (Ok((0, o.scored(), b.degraded)), o.label(), Some(b))
        }
        Err(e) => (Err(anyhow::anyhow!("{e:#}")), "failed".to_string(), None),
    };
    // Record before scheduling so an operator reading scan_state sees the
    // outcome that produced the schedule. If the database is down for THIS
    // write it is down for the schedule too: log + count, do not pretend.
    if let Err(e) = books.record_outcome(user_did, &label, now).await {
        error!(user_did, error = %format!("{e:#}"), "could not record the refresh outcome");
        crate::observability::refresh_metrics::record_bookkeeping_failure();
    }
    // Exactly one scheduling site. The slot lifecycle (run_under_slot) does
    // NOT schedule; it only finishes the queue row.
    match books_for {
        Some(b) if b.prove_revision => books.schedule_success(user_did, now).await,
        Some(b) => {
            if b.request_full {
                if let Err(e) = books.request_full(user_did).await {
                    warn!(error = %format!("{e:#}"), "could not request the follow-up full scan");
                }
            }
            books.schedule_retry(user_did, now).await;
        }
        None => books.schedule_retry(user_did, now).await,
    }
    let completion = match &outcome {
        Ok(o) => o.finish_completion(),
        Err(_) => FinishCompletion::Failed,
    };
    finish_scan(&scan_manager, user_did, claim_id, result, completion, "Refresh").await
}

/// Pure: candidates + the pipeline's summary → outcome. `run_refresh_with`
/// reads the staging marker and skip count through the summary so this
/// stays testable without a database.
pub fn classify_refresh(candidates: usize, summary: &ScanSummary) -> RefreshOutcome {
    match crate::web::scan_job::classify_full_scan(summary.degraded, summary.final_phase.as_deref(), summary.skipped) {
        crate::web::scan_job::ScanCompletion::Resumable => RefreshOutcome::Resumable,
        crate::web::scan_job::ScanCompletion::CompleteWithSkips { n } => RefreshOutcome::CompletedWithSkips { candidates, scored: summary.accounts_scored, skipped: n as usize },
        crate::web::scan_job::ScanCompletion::CompleteUnverified => RefreshOutcome::CompletedUnverified { candidates, scored: summary.accounts_scored },
        crate::web::scan_job::ScanCompletion::Complete if candidates == 0 => RefreshOutcome::NothingDue,
        crate::web::scan_job::ScanCompletion::Complete => RefreshOutcome::Completed { candidates, scored: summary.accounts_scored },
    }
}

/// Production entry: everything fallible is inside `setup`. Returns the same
/// `ScanReport` as `run_scan` so `launch_scan`'s two arms have one type and
/// `run_under_slot` finishes the queue row with the refresh's completion (V4-04).
pub(crate) async fn run_refresh(
    config: Arc<Config>,
    db: Arc<dyn Database>,
    models: Arc<ScanModels>,
    scan_manager: Arc<RwLock<ScanManager>>,
    user_did: &str,
    actor_handle: &str,
    claim_id: &str,
) -> anyhow::Result<ScanReport> {
    let books = DbBookkeeping(Arc::clone(&db));
    let setup_db = Arc::clone(&db);
    let setup = move || -> anyhow::Result<(Box<dyn RefreshContextSource>, _)> {
        let scorers = build_scan_scorers(&models, &setup_db)?;
        let client = Arc::new(PublicAtpClient::new(&config.public_api_url)?);
        let ctx: Box<dyn RefreshContextSource> = Box::new(LiveRefreshContext {
            db: Arc::clone(&setup_db),
            client: Arc::clone(&client),
            models: Arc::clone(&models),
        });
        let run_db = Arc::clone(&setup_db);
        let run_models = Arc::clone(&models);
        let uid = user_did.to_string();
        let pipeline = move |plan: RefreshPlan| async move {
            // Same deps construction as amplification.rs:520-556.
            let source = AtpPostFetcher { client: &client };
            let feed_stats = Arc::new(CacheStats::default());
            let fetcher = CachedPostFetcher::new(&source, Arc::clone(&run_db), Arc::clone(&feed_stats));
            let scorer = &scorers.scorer;
            let classifier = scorer.classifier();
            let weights = ThreatWeights::default();
            let deps = PhasedScanDeps {
                fetcher: &fetcher,
                scorer: scorer as &dyn ToxicityScorer,
                clean_pass: scorer as &dyn CleanPassScorer,
                classifier: &classifier,
                protected_fingerprint: &plan.fingerprint,
                weights: &weights,
                embedder: Some(&*run_models.embedder),
                protected_embedding: plan.protected_embedding.as_deref(),
                protected_topic_centroids: Some(&plan.centroids),
                nli_scorer: Some(&*run_models.nli),
                protected_posts_with_embeddings: Some(&plan.protected_posts),
                data_dir: Some(config.data_dir()),
                median_engagement: plan.median_engagement,
                gather_concurrency: 8, // same literal run_scan uses today; Phase 3 replaces both
                burst_concurrency: burst::burst_concurrency(),
                burst_batch: burst::burst_batch(),
                evidence: EvidenceContract {
                    onnx_model_id: crate::toxicity::onnx::ONNX_MODEL_ID,
                    classifier: ClassifierIdentity { model_id: classifier.model_id(), policy_version: classifier.policy_version() },
                },
            };
            let candidates = plan.candidates.len();
            run_db.set_scan_state(&uid, "refresh_candidates", &candidates.to_string()).await?;
            run_db.set_scan_state(&uid, "refresh_feed_cache_applicable", if candidates == 0 { "0" } else { "1" }).await?;
            let summary = run_phased_scan(&run_db, &uid, &plan.candidates, &deps, RunIdentity::refresh()).await?;
            run_db.set_scan_state(&uid, "refresh_scored", &summary.accounts_scored.to_string()).await?;
            // The feed-cache functional number (R08): recorded per run.
            if let Err(e) = record_cache_stats(run_db.as_ref(), &uid, "refresh_feed", &feed_stats).await {
                warn!(error = %e, "could not record refresh feed cache stats");
            }
            record_scan_cache_stats(run_db.as_ref(), &uid, &scorers).await;
            Ok(summary)
        };
        Ok((ctx, pipeline))
    };
    run_refresh_with(scan_manager, &books, &chrono::Utc::now, user_did, actor_handle, claim_id, setup).await
}

/// Task 8's helpers behind the boundary. Owns its client (built inside
/// `setup`, so construction failure is inside the outcome).
struct LiveRefreshContext { db: Arc<dyn Database>, client: Arc<PublicAtpClient>, models: Arc<ScanModels> }

#[async_trait]
impl RefreshContextSource for LiveRefreshContext {
    async fn fingerprint(&self, user_did: &str) -> anyhow::Result<Option<(TopicFingerprint, Option<Vec<f64>>, Vec<Vec<f64>>, Option<String>)>> {
        let Some((json, _, _)) = self.db.get_fingerprint(user_did).await? else { return Ok(None) };
        let fingerprint: TopicFingerprint = serde_json::from_str(&json).context("stored fingerprint is unreadable")?;
        let embedding = self.db.get_embedding(user_did).await?;
        let centroids = self.db.get_topic_centroids(user_did).await?.iter().map(|c| c.centroid.clone()).collect();
        let model_id = self.db.fingerprint_embedding_model(user_did).await?;
        Ok(Some((fingerprint, embedding, centroids, model_id)))
    }
    async fn candidates(&self, user_did: &str) -> anyhow::Result<Vec<RefreshCandidate>> {
        self.db.list_refresh_candidates(user_did, REFRESH_HORIZON_DAYS).await
    }
    async fn protected_posts_embeddings(&self, actor_handle: &str) -> anyhow::Result<Vec<(String, Vec<f64>)>> {
        embed_protected_posts(&self.client, &self.models.embedder, actor_handle).await
    }
    async fn pile_on(&self, user_did: &str) -> anyhow::Result<HashSet<String>> { pile_on_dids(self.db.as_ref(), user_did).await }
    async fn direct_pairs(&self, user_did: &str, amplifier_did: &str) -> anyhow::Result<Vec<(String, String)>> {
        crate::pipeline::amplification::direct_pairs_for(&self.db, user_did, amplifier_did).await
    }
    async fn median_engagement(&self, user_did: &str) -> anyhow::Result<f64> { self.db.get_median_engagement(user_did).await }
}
```

`request_full_after_refresh(user_did)`: `UPDATE scan_queue SET full_requested_at = COALESCE(full_requested_at, now) WHERE user_did = ? AND status = 'running' AND kind = 'refresh'` on both backends (Task 5's finish hands over). `DbBookkeeping::reset_markers` writes the eight `refresh_*` keys. `RefreshPlan`, `RefreshContextSource`, `RefreshBookkeeping`, `DbBookkeeping` live in `refresh_scan.rs`; `ClassifierIdentity`, `EvidenceContract`, `CLEAN_PASS_POLICY` in `staging.rs`. `finish_scan(mgr, user, claim, result: anyhow::Result<(usize, usize, bool)>, completion: FinishCompletion, label: &str) -> anyhow::Result<ScanReport>` records the outcome in the status entry and returns `Ok(ScanReport { completion })` for an `Ok` result, `Err` otherwise — the single tail call for **both** `run_scan` and `run_refresh_with` (V4-04), so `launch_scan`'s `match kind { … }` has one future type `anyhow::Result<ScanReport>` and `run_under_slot` finishes the row with the reported completion for either kind. `RefreshOutcome::finish_completion()` maps Completed → Complete, CompletedWithSkips → CompleteWithSkips, CompletedUnverified → CompleteUnverified, NothingDue → Complete, Deferred/Resumable → Resumable; an `Err` from `run_refresh_with` reaches `run_under_slot` as `SlotExit::Failed` → `FinishCompletion::Failed`. The slot-lifecycle tests (`scan_job.rs::slot_lifecycle_tests`) drive `run_under_slot` with futures of exactly this type for both kinds: a refresh future returning `Ok(ScanReport { completion: Resumable })` must leave the row `done` with `completion = resumable`, one returning `Err` → `failed`.

`launch_scan(state, user_did, actor_handle, kind, slot, live)`: `match kind { Full => run_scan(...), Refresh => run_refresh(...) }` inside the spawned future. `run_scan`'s pipeline call passes `RunIdentity::full()`; on `PhasedScanError::OwnedByOtherKind(Refresh)` it performs the **drain** (see Transitions): the drain's `ScanSummary` is classified with `classify_full_scan`; if it is `Resumable` (cost-capped/interrupted again), `run_scan` returns that as its own completion — no cooldown marker, `schedule_retry`, the row finishes `done` with `completion = resumable` and **`full_requested_at` kept**, so the retry tick re-queues the user's owed **full** scan automatically and the next attempt resumes the drain before its own gather (V3-02, V3-03: a partial drain earns no full-scan bookkeeping and loses no obligation) — and a `Complete`/`CompleteWithSkips`/`CompleteUnverified` drain clears the markers and proceeds to the full gather, **remembering its outcome**: `run_scan_inner` folds it into the run's own completion with `ScanCompletion::worst` (V6-01), so a drain that was unverified or skip-laden yields `CompleteUnverified`/`CompleteWithSkips` for the whole run — fulfilled (marker, obligation cleared) but not proof — and a drain alone never completes the request: only the run's own gather reaching `done` does. Test `tests/unit_scan_phases.rs::an_unverified_drain_taints_the_full_scan`: refresh-owned `finalize` staging whose `scan_skips` table is dropped (count unreadable) → drain classifies `CompleteUnverified` → the run's own empty gather completes cleanly → `run_scan_inner` returns `FullScanRun { completion: CompleteUnverified, .. }`; the wrapper writes the marker, schedules the retry, and `finish` clears the obligation with `completion = complete_unverified`. `AppStateLauncher::launch` passes `claim.kind`; the admit log line gains `kind`. `record_scan_outcome`/`finish_scan` gain a `label: &str` ("Completed" / "Refresh complete") so a refresh's status message does not read "0 events".

- [ ] **Step 5: Run**

Run: `cargo test --features web --lib web::refresh_scan` (must include `missing_context_fails_the_refresh_without_writing`, `setup_failure_records_failed_and_schedules_the_retry`, `marker_reset_failure_records_failed_and_schedules_the_retry` and `bookkeeping_follows_the_completion_contract` passing — not skipped), `cargo test --features web --test unit_classifier zentropi_advertised`, `cargo test --features web --test unit_scan_phases`, `VERIFY_WEB`, `cargo build --features postgres`, `VERIFY_CLIPPY`.
Expected: green.

- [ ] **Step 6: Commit**

```bash
git add src/web/refresh_scan.rs src/web/mod.rs src/web/scan_job.rs src/web/admitter.rs src/pipeline/scan_phases/mod.rs src/pipeline/scan_phases/staging.rs src/pipeline/scan_phases/gather.rs src/pipeline/scan_phases/finalize.rs src/pipeline/amplification.rs src/pipeline/sweep.rs src/toxicity/zentropi.rs src/db/traits.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs tests/unit_scan_phases.rs tests/unit_classifier.rs
git commit -m 'feat(344): run_refresh with staging ownership, resume, input compatibility and honest outcomes; one ScanReport contract for both scan kinds

run_phased_scan records scan_run_kind/scan_run_generation; a refresh
resumes its own work, refuses a full scan'"'"'s (Deferred), a full scan
drains a refresh'"'"'s leftovers before gathering (a partial drain is
Resumable, not complete), and other-revision staging is discarded.
Verdicts from another classifier model OR policy are incomplete. Context
loads sit behind RefreshContextSource; a failure fails the run (no score
written) and is covered by a deterministic test. Outcomes drive the
schedule: completed/nothing-due → nightly + revision proven;
completed-with-skips/deferred/resumable/failed → retry in an hour;
missing or incompatible fingerprint also requests a follow-up full scan.
Per-run cache counters are reset at start and flagged not-applicable
when nothing was due.

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
  7. **Revision change procedure:** an in-binary model swap changes the revision by itself; bump `SCORING_GENERATION` by hand for formula/format/policy changes **and for any CoPE-B/Zentropi classifier model or policy change** (deploy checklist item — the classifier's identity is not composed into the revision). Deploy single-replica; steps 2–3 repeat automatically via `refresh_attempted_generation`; a failed first attempt retries hourly, not on every tick (`SELECT did, next_refresh_at, refresh_attempted_generation, refreshed_generation FROM users`); with refreshes disabled (`0`) the lists stay hidden until users re-engage — state this in the deploy notes for that change.
  7b. **Completion bookkeeping check (V2-05, V3-02, V3-03):** with the marker as the only cooldown anchor — cost-cap a full scan; `scan_state.last_full_scan_finished_at` must be unchanged and the user's next click must resume without a 429. Request a full scan during a refresh, cost-cap the drain; `full_requested_at` handover must still yield a queued full row and the cooldown must not treat the drain as a completed full scan. Skip one account (simulate a feed 4xx via the runbook's fixture account) in an otherwise completed refresh; `refresh_last_outcome = completed_with_skips`, `refreshed_generation` unchanged, `next_refresh_at` ≈ now + 1 h, the other accounts' new scores present.
  8. **Index plans:** paste Task 7's `EXPLAIN` output and timings for both backends.
  8b. **Suite isolation (V3-06):** `for i in $(seq 10); do VERIFY_PG || break; done` with both databases configured; paste the ten results. No test may see another's schema reset.
  9. **Migration rehearsal (R01, V2-06):** on a copy of the prod SQLite (`backups/`), `charcoal migrate` into a scratch Postgres; compare `SELECT COUNT(*), COUNT(*) FILTER (WHERE threat_score IS NULL), MIN(scored_at), MAX(valid_until)` source vs destination; count the source's NULL/malformed `valid_until` rows (`SELECT COUNT(*) FROM account_scores WHERE datetime(valid_until) IS NULL`) and verify the destination has that many rows with `valid_until = scored_at`; repeat the migrate; nothing changes.

- [ ] **Step 4: Spec** — §4.4 amendment (dated 2026-09-14): v18; `refreshed_generation` replaces the migration stamp; ownership markers and the transitions; the compatibility contract; `full_requested_at` and in-place upgrade; lossless migrate; the tick's single transaction and batch bound; SQLite nullable/malformed expiry semantics; retry cadence; "full fortnightly" remains #342. §6 Phase 2: replace the ≥ 80 % feed-hit pass with the functional test + recorded share (reference deciduous 878).

- [ ] **Step 5: Commit, PR, CodeRabbit loop.** Do **not** close #344 until the runbook has been executed on staging and its readings are pasted into the runbook (a separate docs commit on the same PR or a follow-up).

---

## Review-resolution table — first review (Astra, `c91afb8`, R01–R13)

| ID | Disposition | Revised location | Acceptance test (planned, not yet executed) | Remaining limitation |
|---|---|---|---|---|
| R01 | addressed in design (rev 3: + V2-06, V2-07) | Task 3 (`export_scores`/`import_score`, `ExportedExpiry`, `migrate`), Global Constraints | `tests/unit_score_export.rs` (6-row round trip incl. NULL-score, NULL and malformed expiry, repeat import, authentic v17 fixture); `test_pg_migrate_from_sqlite_preserves_every_row` (real path, direct SQL); precision tests; runbook §9 | `top_toxic_posts` corruption still exports as empty (#364); Postgres → SQLite truncates to whole seconds (documented) |
| R02 | addressed in design (rev 3: + V2-05) | Task 9 (`RunIdentity`, markers, `PhasedScanError`, `RefreshOutcome` incl. `CompletedWithSkips`, transitions), Task 5 (`ScanCompletion`), Task 6 (`schedule_retry`) | `staging_ownership_is_recorded_and_enforced`, `bookkeeping_follows_the_completion_contract`, `classify_full_scan` tests; runbook §5, §7b | A refresh's resume uses the *current* candidate list; a staged account no longer eligible is finalized from staging but cannot be re-gathered (documented skip) |
| R03 | addressed in design (rev 3: + V2-01, V2-04) | Task 1 (`scoring_revision()` composite, `EMBEDDING_MODEL_ID`, `NLI_MODEL_ID`, blob revision), Task 2 (`embedding_model_id` column), Task 8 (`rebuild_decision`, no fallback on incompatible), Task 9 (verdict model + policy check, revision-mismatch discard) | `scoring_revision_changes_when_any_component_changes`, `rebuild_decision_orders_reasons…`, `finalize_rejects_a_blob_from_another_generation`, `old_generation_staging_is_discarded_on_resume`, `finalize_rejects_verdicts_from_another_policy` + `…_classifier_model` | Classifier model/policy changes still require a manual `SCORING_GENERATION` bump (identity lives outside the binary); rolling-deploy overlap of seconds accepted |
| R04 | addressed in design (rev 3: + V2-02, V2-03) | Task 6 (conditional `REFRESH_ENQUEUE_SQL`, advance-only-on-write, `refresh_attempted_generation`, batch bound, metrics) | `a_failed_enqueue_rolls_back_the_schedule_advance`, `the_queue_write_is_conditional_on_the_row_state_at_write_time`, `a_failed_attempt_waits_for_its_retry_deadline`, `a_tick_is_bounded…`, Postgres partition + three interleaving variants | The Postgres interleaving test drives the two `pub const` statements directly rather than pausing the Rust function mid-transaction |
| R05 | addressed in design (rev 3: mandatory test) | Task 8 (`Result` helpers), Task 9 (`RefreshContextSource`, `prepare_refresh`, `run_refresh_with`) | `a_read_failure_is_an_error_not_an_empty_list`, **`missing_context_fails_the_refresh_without_writing`** (deterministic, no models), `no_pairs_is_a_valid_plan` | Per-account gather failures inside `run_phased_scan` are skips → `CompletedWithSkips` (retry, degraded), not run failures |
| R06 | addressed in design | Task 3 Step 6 (`run_topic_first`) | Extend `tests/unit_discovery.rs` (or the sweep test that covers `run_topic_first`) with an expired row that becomes eligible | none |
| R07 | addressed in design (rev 3: + V2-03) | Task 2 (`refresh_attempted_generation` + `refreshed_generation`, both NULL), Task 6 (due rule) | `due_by_time_or_by_generation_and_never_without_scores`, `a_failed_attempt_waits_for_its_retry_deadline`; runbook §7 | Disabled refreshes ⇒ no prompt recovery after a revision change (stated in runbook) |
| R08 | addressed in design | Task 9 (per-run reset, `applicable` flag), Task 10 (runbook §6, spec §6), deciduous 878 | Runbook §6 (a)(b)(c) | Steady-state share is recorded, not gated |
| R09 | addressed in design (rev 3: + V2-02) | Task 5 (`full_requested_at`, in-place upgrade, `EnqueueOutcome`, handover in `finish_queued_scan`), Task 6 (conditional write never clobbers it) | `a_full_enqueue_upgrades_a_queued_refresh_in_place`, `a_full_request_during_a_running_refresh_is_recorded_and_runs_after_it`, `a_failed_refresh_still_hands_over…`, `test_pg_scheduler_write_does_not_clobber_a_concurrent_full_enqueue`; runbook §4 | Dashboard copy for `queued: "after_refresh"` is #365's |
| R10 | addressed in design (rev 3: + V2-07) | Global Constraints (`VERIFY_*`), every task's Run steps; authentic fixtures via `create_tables_through` / `migrate_postgres_through` | Each command is a valid single-filter invocation; `VERIFY_WEB` fails on `SKIP:`; every "verify it fails" step names the failure kind; no calendar-dependent fixture remains | none |
| R11 | addressed in design | Task 3 (`FRESH_SQL` COALESCE), Task 7 (candidate COALESCE) | `fresh_set_is_exactly…including_null_and_malformed_expiry` (count = 6), `selects_high_and_elevated…malformed…` | Postgres cannot hold a malformed value; only SQLite is exercised |
| R12 | addressed in design | Task 2 (index changed to `(user_did, threat_score)`), Task 7 Step 4 | `candidate_query_uses_the_user_score_index` (SQLite plan assertion); runbook §8 plans + timings on 3 000-row users | Plans on a 3 000-row table may prefer a seq scan; the runbook says how to show index usability without adding a partial index prematurely |
| R13 | addressed in design | Task 2 (backfill), Task 5 (`last_full_finished_at` anchor) | `test_migration_v18_upgrades_a_v17_database` (marker = done row's `finished_at`); `scan.rs` inline tests; runbook §4 | none |

**Architectural changes relative to rev 1:** ownership markers on staging + a typed `PhasedScanError`; a compatibility contract (embedding model id on fingerprints, revision on blobs, classifier identity on verdicts); scheduling as one bounded transaction with revision-driven due-ness; a durable full-scan request on the queue row; lossless export/import as a separate contract from presentation; error-returning context helpers.

**Unresolved product decisions (Bryan):** none new. Decision 777 (expire, refresh High/Elevated only) stands; the fortnightly full rescan stays in #342.

## Review-resolution table — second review (Astra, `30cd7f4`, V2-01–V2-07)

Dispositions: *addressed in design* = corrected contract + test specified; *partially addressed* = a stated path remains; *verified* = an executed test with environment and result; *disputed with evidence*. Nothing in this table is *verified*: this is a plan revision and no planned test has been run.

| ID | Prior | Disposition | Revised location | Mandatory acceptance tests (planned) | Remaining limitation |
|---|---|---|---|---|---|
| V2-01 | R03 | addressed in design; **partially** for the out-of-binary classifier | Global Constraints ("stored stamp is the scoring revision"), Task 1 (`compose_revision`/`scoring_revision()`, `NLI_MODEL_ID`), Task 9 (`ClassifierIdentity` check), runbook §7 | (1) `scoring_revision_changes_when_any_component_changes` — a different ONNX/embedding/NLI id yields a different stamp, so rows stamped under model A fail the fresh predicate under model B with the policy unchanged (`fresh_set_is_exactly…` binds `scoring_revision()`); (2) `finalize_rejects_verdicts_from_another_classifier_model` (same policy, different model); (3) a generation-only bump changes the stamp while `onnx_scores`/`classifier_verdicts` cache keys (text hash + model id) are untouched — assert in `unit_cached_scoring` that a cached row is still hit after `SCORING_GENERATION` changes | A CoPE-B/Zentropi classifier model or policy change still needs a manual `SCORING_GENERATION` bump — its identity is outside the binary and cannot be composed in without threading a runtime value through every freshness read. Mitigations: verdict rows always carry model+policy and are rejected on resume; runbook §7 checklist |
| V2-02 | R04, R09 | addressed in design | Task 6 (`REFRESH_DUE_SQL`, conditional `REFRESH_ENQUEUE_SQL`, advance-only-on-write, lock order) | `the_queue_write_is_conditional_on_the_row_state_at_write_time` (SQLite, real statements), `test_pg_scheduler_write_does_not_clobber_a_concurrent_full_enqueue` with three variants (enqueued between, admitted between, absent row), `test_pg_two_schedulers_partition_the_due_set` | The Postgres interleaving is driven at the statement level on two connections, not by pausing the Rust function — the statements are `pub const` precisely so the test exercises what production runs |
| V2-03 | R04, R07 | addressed in design | Task 2 (`refresh_attempted_generation`), Task 6 (due rule; tick sets attempted; `schedule_retry` touches only time; `mark_refreshed_generation` sets both), Task 9 bookkeeping | `a_failed_attempt_waits_for_its_retry_deadline` (fail → finish → retry; +30 s no job; after deadline one job; newer revision during backoff attempted at once); the Postgres bounded-claim test asserts both columns | none identified |
| V2-04 | R03 | addressed in design | Task 8 (`RebuildReason`, `rebuild_decision`, `fallback_allowed`, abort-before-scoring), Task 3 (`save_fingerprint_bundle` id; import copies verbatim, never labels) | `rebuild_decision_orders_reasons_and_flags_model_incompatibility` (same dims/cluster count, other model → rebuild; missing id with a vector → rebuild; failure with `IncompatibleModel` → no fallback); runbook §7 for the handover reaching this path | The abort-on-failed-rebuild arm runs inside `run_scan` (model-gated); the decision and the fallback rule are pure and unit-tested, the arm is exercised on staging |
| V2-05 | R02, R04, R05, R13 | addressed in design | Task 5 (`ScanCompletion`, `classify_full_scan`, marker only on completion), Task 6 (success vs retry scheduling by completion), Task 9 (`CompletedWithSkips`, `Bookkeeping`, drain classified) | `classify_full_scan` cases; `bookkeeping_follows_the_completion_contract`; runbook §7b (cost-capped full scan → no marker, immediate re-run; drain cost-cap → pending full still honoured, no marker; one skipped account → `completed_with_skips`, retry, writes retained) | Retry of skipped accounts relies on the hourly refresh picking them up as still-expired candidates; a skipped Low/Watch account in a full scan has no refresh path (existing behaviour, #236/#355 territory) |
| V2-06 | R01 | addressed in design | Global Constraints (conversion + precision), Task 3 (`ExportedExpiry`, `%f`/`.US` export, `scored_at` mapping, warn line) | `export_returns_every_row_with_its_provenance` (Missing/Invalid variants), `import_preserves_provenance_and_never_renews_expiry` (NULL/malformed → `scored_at`), `test_pg_migrate_from_sqlite_preserves_every_row` (direct SQL on Postgres incl. the two conversions), `test_pg_export_import_keeps_microseconds`, `test_pg_import_into_sqlite_truncates_to_seconds` | Postgres → SQLite truncates to whole seconds by SQLite's column form — deliberate, documented; the raw malformed text is logged, not preserved in the destination |
| V2-07 | R01, R10 | addressed in design | Task 2 (`create_tables_through`, `migrate_postgres_through`), Task 3 tests (relative dates; real import; authentic v17) | `test_migration_v18_upgrades_a_v17_database` asserts `MAX(version) = 17` and the missing column before upgrading; `v17_fixture_rows_survive_open_export_import` imports into a second database and asserts; `test_pg_migrate_from_sqlite_preserves_every_row`; no fixture uses a calendar date | The Postgres authentic fixture rebuilds the whole test database and therefore serialises behind `cache_test_lock()` |

**Invariants chosen (rev 3):**
- *Compatibility:* a stored score is comparable to a fresh one iff its stamp equals `scoring_revision()` (generation ⊕ every in-binary model id); staged inputs carry the same stamp; verdict rows carry the classifier's model id and policy and are rejected on resume if either differs; fingerprints carry their embedding model id and are rebuilt (never fallen back to) when it differs. Caches keep their own identities.
- *Concurrency:* the tick's write is conditional on the queue row's state at write time and advances a user's schedule only when it delivered; manual enqueue, admission and finish lock only the queue row; completion bookkeeping touches only `users`. No lock cycle; a race resolves to "the user's own work wins, the refresh is reconsidered next tick".
- *Retry:* `refresh_attempted_generation` (set by the tick) decides whether a revision still needs a first attempt; `next_refresh_at` decides when the next attempt may happen; `refreshed_generation` (set only by complete work) is proof. A new revision is attempted promptly once; failures wait for their deadline.
- *Completion:* `Ok` is classified, never trusted. Only `Complete`/`Completed`/`NothingDue` earn the cooldown marker, the nightly schedule and the revision proof; skips, resumable interruptions and deferrals keep their successful writes, retry in an hour, and read as degraded.

## Review-resolution table — third review (Astra, `c40be7f`, V3-01–V3-06)

Dispositions as for the second table. Nothing here is *verified*: this is a plan revision and no planned test has been run.

| ID | Prior | Disposition | Revised location | Mandatory acceptance tests (planned) | Remaining limitation |
|---|---|---|---|---|---|
| V3-01 | R03, V2-01 | addressed in design | Task 9 "Evidence provenance" (`CLEAN_PASS_POLICY`, `EvidenceContract`, `Provenance`, `mark_clean` writes the ONNX producer, `verdict_for` accepts CleanPass or Classifier only, Zentropi `policy_version()` = configured labeler id used on every path) | `finalize_accepts_clean_pass_and_classifier_evidence_together`, `finalize_rejects_evidence_from_another_producer_revision`, `provenance_distinguishes_missing_foreign_and_sentinel`, `clean_pass_rows_carry_onnx_provenance`, `zentropi_advertised_policy_matches_written_policy_on_miss_and_hit` (miss + hit through `CachedClassifier`) | Pre-v18 clean rows (no provenance) cost one bounded re-gather on first resume — intended; the decode-error sentinel is rejected here and re-classified, its removal is #355 |
| V3-02 | R02, R13, V2-05 | addressed in design | Task 5 (cooldown reads only `scan_state.last_full_scan_finished_at`; `record_full_scan_completion`; `scan_queue.completion` written by `finish_queued_scan`), Global Constraints (cooldown policy: `Complete` and `CompleteWithSkips` anchor the cooldown — the user's request was fulfilled; only `Complete` proves the revision), runbook §7b | `tests/web_scan_queue.rs::an_interrupted_full_scan_does_not_start_a_cooldown` (enqueue → claim → `run_under_slot` with a scan future that records `Resumable` → row `done`/`completion = resumable` → real `POST /api/scan` → 202), the transient-interruption and interrupted-drain variants, `…complete_with_skips_starts_the_cooldown` (→ 429), `…a_completed_full_scan_enforces_cooldown_and_a_refresh_does_not_reset_it` (marker + `retry_at` unchanged after a refresh finishes) | The web tests need the model-backed test app (CI has models; `VERIFY_WEB` fails on a skip) |
| V3-03 | R02, R09, V2-05 | addressed in design | Task 5 (`full_requested_at` = "a full scan is owed since T": set by every user request, kept through handover and resumable finishes, cleared only when a full scan completes), Task 6 (`REFRESH_ENQUEUE_SQL` retries owed work as `kind = 'full'`), Task 9 (drain interruption is `Resumable`, obligation retained) | `tests/unit_scan_kind.rs::a_requested_full_scan_survives_an_interrupted_drain_and_runs_without_another_click` (request during refresh → handover → full claimed → finish `Resumable` → owed → `schedule_retry` → tick after deadline creates `kind = 'full'` → finish `Complete` clears `full_requested_at`); duplicate clicks in between leave one row and one `full_requested_at`; the same across a simulated restart (new `SqliteDatabase` over the same file); Postgres twin | The owed retry waits `REFRESH_RETRY_HOURS` unless the user clicks (which re-queues at once) — automatic, but not instant |
| V3-04 | R07, V2-03 | addressed in design | Task 6 (`mark_refreshed_generation` SQL sets both columns on both backends; `mark_refresh_attempted_generation` sets one) | `a_completed_manual_full_scan_proves_both_columns_and_the_tick_stays_quiet` (user with NULL attempted → enqueue → claim → `record_full_scan_completion(Complete)` → `schedule_after_success` → both columns = current, `next_refresh_at` ≈ +24 h → tick 30 s later creates no job); Postgres twin through the same functions | none identified |
| V3-05 | R04, R05, V2-03 | addressed in design | Task 9 (`RefreshBookkeeping` boundary; `setup` builds context + pipeline inside the captured outcome; `reset_markers` inside; one scheduling site; bookkeeping-write failure logged + counted, never reported as scheduled) | `setup_failure_records_failed_and_schedules_the_retry` (setup returns `Err`; stub bookkeeping counts exactly one `schedule_retry`, zero `schedule_success`), `marker_reset_failure_records_failed_and_schedules_the_retry` (stub `reset_markers` errs, others delegate), `missing_context_fails_the_refresh_without_writing` retained; all three run without models | If the database is unreachable for the bookkeeping write itself, the retry cannot be scheduled either; the failure is logged at error level and counted (`refresh_metrics::record_bookkeeping_failure`), and the user is next selected by time |
| V3-06 | R10, V2-07 | addressed in design (rev 5: + V4-05) | Task 2 (`migrate_postgres_through` runs only against `DATABASE_URL_MIGRATIONS`, `_migrations` suffix guard; `migrations_fixture()` serializes destructive tests with a process mutex **and** a Postgres session advisory lock; drop-all at test *start* under both), Global Constraints (`VERIFY_PG`) | the two destructive tests go through `migrations_fixture()`; the suffix guard test; a two-concurrent-fixtures test proves strict ordering; the runbook's ten-run loop | none identified |

**Evidence-provenance contract (rev 4):** every done queue row names its producer; the Stage-1 clean pass is a producer (ONNX model + `CLEAN_PASS_POLICY`), the Stage-2 classifier is a producer (its `model_id` + `policy_version`, one string on every path — advertised, written, cached). Finalize accepts a row only from a producer this binary runs; missing, foreign and sentinel provenance are distinct and all rejected. Re-gather recreates evidence with provenance, so a rejected row is a bounded retry, never a skip.

**Full-request lifecycle contract (rev 4):** a user's request is a durable obligation, `scan_queue.full_requested_at`, set by every user enqueue and cleared only when a full scan **completes** (`Complete`/`CompleteWithSkips`). Attempts are queue rows and claims; interruptions finish the row with `completion = resumable` and keep the obligation; the retry tick queues owed work as `kind = 'full'`, never as a refresh; a refresh handover turns the row into the owed full row; repeated clicks coalesce; a worker restart changes nothing because every part of the state is in `scan_queue` and `scan_state`. Cooldown anchors only on the completion marker, so an interrupted attempt never starts one.

## Review-resolution table — fourth review (Astra, `fb713a3`, V4-01–V4-05)

Dispositions as before. Nothing here is *verified*: plan revision only.

| ID | Prior | Disposition | Revised location | Mandatory acceptance tests (planned) | Remaining limitation |
|---|---|---|---|---|---|
| V4-01 | R04, V2-03, V3-03 | addressed in design | Task 6 (`REFRESH_DUE_SQL` eligibility = scores **or** owed full; `schedule_retry_at` stamps `refresh_attempted_generation`; Postgres twin), Global Constraints | `an_owed_first_scan_with_no_scores_is_retried_after_its_deadline` (zero scores → claim → fail before write → retry scheduled → restart → +30 s nothing → after deadline exactly one **full** job with the original obligation timestamp; a user with neither scores nor request never selected); `success_and_retry_scheduling` asserts the attempt stamp; Postgres twin | none identified |
| V4-02 | R02, V2-05, V3-02, V3-03 | addressed in design (rev 6: + V5-01) | Task 9 (`amplification::run` removes the empty-candidate shortcut — always `run_phased_scan` with a scorer), Task 5 (`classify_full_scan`: `burst`/`finalize`/missing marker ⇒ `Resumable` regardless of `degraded`) | `empty_discovery_resumes_full_owned_staging`, `empty_discovery_surfaces_refresh_owned_staging` (→ `run_scan` drains), `empty_discovery_with_no_staging_completes`, the extended `classify_full_scan` cases | A CLI scan without `--analyze` (no scorer) still skips the pipeline, as today — it never scores and never owns staging |
| V4-03 | R09, V2-02, V3-03 | addressed in design (rev 6: tests split, V5-03) | Task 5 (Postgres `enqueue_scan`: `pg_advisory_xact_lock(hashtext('scan_queue:' \|\| $1))` before the state read; conditional absent-row insert with re-read as defense in depth; lock order stated) | `test_pg_first_enqueues_serialize_on_the_user_lock` (B observed waiting in `pg_locks`, then `AlreadyQueued`) and `test_pg_enqueue_preserves_a_committed_running_claim` (claim committed first, then `AlreadyRunning`; claim id, lease, status, obligation intact; clicks coalesce) + the raw-statement variant | SQLite needs no change: its immediate transaction already serializes the absent-row case |
| V4-04 | R02, V3-02, V3-05 | addressed in design | Task 9 (`run_refresh_with` / `run_refresh` return `anyhow::Result<ScanReport>`; `finish_scan` returns `Result<ScanReport>` and is the single tail for both kinds), Task 5 (`launch_scan` arms share one future type) | `cargo build --features web,postgres` with both dispatch arms; slot-lifecycle tests driving `run_under_slot` with refresh-shaped `ScanReport` futures (`Resumable` → `completion = resumable`, `Err` → `failed`); the V3-05 tests assert `completion = failed` on the row | none identified |
| V4-05 | R10, V2-07, V3-06 | addressed in design | Task 2 (`migrations_fixture()`: process mutex + Postgres session advisory lock; drop-all under both; both destructive tests use it; concurrent-fixtures ordering test), Global Constraints (`VERIFY_PG` wording) | the ordering test; both destructive tests under the suite's normal parallelism, ten-run loop (runbook §8b) | A single process mutex would not cover two overlapping CI invocations — that is what the Postgres session advisory lock is for |

## Review-resolution table — fifth review (Astra, `4b9459d`, V5-01–V5-03)

Dispositions as before. Nothing here is *verified*: plan revision only.

| ID | Prior | Disposition | Revised location | Mandatory acceptance tests (planned) | Remaining limitation |
|---|---|---|---|---|---|
| V5-01 | V2-05, V3-02, V4-02 | addressed in design (rev 7: + V6-01 wiring) | Task 5 (`classify_full_scan(degraded, phase, skipped: Option<i64>)`: `done` + persisted skips > 0 ⇒ `CompleteWithSkips` regardless of the flag; `None` ⇒ `CompleteUnverified`; `ScanCompletion::CompleteUnverified`, `FinishCompletion::CompleteUnverified`, CHECK list), Task 9 (`ScanSummary.skipped: Option<i64>`, `RefreshOutcome::CompletedUnverified`, `classify_refresh`), Global Constraints | the extended direct classifier cases incl. `(false, done, Some(2))` and `(_, done, None)`; `a_resumed_run_keeps_its_earlier_skips` (gather skip → burst cost-cap → clean resume ⇒ `CompleteWithSkips{1}` for both kinds, retry, no proof; dropped `scan_skips` ⇒ `CompleteUnverified`) | `CompleteUnverified` still writes the cooldown marker (the request was fulfilled); it never proves the revision |
| V5-02 | R04, V2-03, V4-01 | addressed in design (rev 7: + V6-02 clock) | Task 5 (`run_scan_with` + `FullScanBookkeeping` + `DbFullScanBookkeeping`; `run_scan_inner` holds every fallible step), Task 6 (bookkeeping wiring), Task 8 (abort arm sits inside the boundary) | `a_full_scan_setup_failure_schedules_the_hourly_retry_and_keeps_the_obligation` (scheduler-created owed full scan with a nightly deadline; injected fingerprint failure and injected scorer failure through the boundary; exactly one retry per attempt, failed row, obligation kept, no marker, no proof, hourly deadline replaced the nightly one; tick quiet before, one full job after) — no manual `schedule_retry`, no models | none identified |
| V5-03 | V4-03 | addressed in design | Task 5 Postgres tests (serialization proven by observing B's ungranted advisory lock in `pg_locks`, then `AlreadyQueued`; claim preservation with the running claim committed **before** the competing enqueue, then `AlreadyRunning`), V4-03 table row | the two named tests + the raw-statement variant | none identified |

## Review-resolution table — sixth review (Astra, `86c4756`, V6-01–V6-02)

Dispositions as before. Nothing here is *verified*: plan revision only.

| ID | Prior | Disposition | Revised location | Mandatory acceptance tests (planned) | Remaining limitation |
|---|---|---|---|---|---|
| V6-01 | V5-01, V3-02, V3-03 | addressed in design (policy kept: unverified = fulfilled, not proof) | Task 5 (`ScanCompletion::fulfilled()` drives `record_full_scan_completion`; `finish_queued_scan`'s `fulfilled` includes `CompleteUnverified` on both backends; `worst()`), Task 9 drain paragraph (`run_scan_inner` folds the drain's completion into the run's; a drain alone never completes the request), Global Constraints | `an_unverified_completion_is_fulfilled_but_not_proof` (wrapper + finalization: marker written, `complete_unverified` durable, obligation cleared, retry scheduled, no proof); `test_pg_finish_unverified_clears_the_obligation`; `an_unverified_drain_taints_the_full_scan`; `fulfilled()`/`worst()` unit cases; existing Complete/WithSkips/Resumable/Failed tests unchanged | none identified |
| V6-02 | R04, V2-03, V3-05, V5-02 | addressed in design | Task 5 (`run_scan_with(…, clock: &dyn Fn() -> DateTime<Utc>, …)`, clock read after `run`), Task 9 (`run_refresh_with` likewise; `started` is telemetry only), Global Constraints | `a_failed_full_scan_is_retried_an_hour_after_it_ended_not_began` and `a_failed_refresh_is_retried_an_hour_after_it_ended_not_began` (FakeClock advanced 90 min inside the attempt; deadline = end + retry; tick quiet 30 min after the end, one job 61 min after; owed full retried as full, refresh as refresh) | none identified |

**Validation performed for revisions 2–7:** none of the planned tests has been executed — this is a plan. Verified in-session for rev 2: the code paths the first review cited. Verified in-session for rev 3 (sqlite3): `datetime()` drops fractional seconds, malformed text yields NULL, `%f` renders milliseconds; `amplification::run` returns `Ok` for cost-capped scans; the rev-2 v17 fixture dropped two of nine v18 columns. Verified in-session for rev 4 by code inspection: `gather.rs:570-576` `mark_clean` writes no provenance; `zentropi.rs:378-386` vs `:402-408` write and advertise different policy strings; `cached_classifier.rs:68-73` keys on the advertised one; `tests/db_postgres.rs:972,982` are two separate locks. Verified in-session for rev 5: `amplification.rs:516` bypasses `run_phased_scan`; the rev-4 `REFRESH_DUE_SQL` required an `account_scores` row; the rev-4 Postgres `enqueue_scan` began with `SELECT … FOR UPDATE` on a possibly absent row; rev 4 declared `run_refresh` as `Result<()>`; rev 4 assigned both destructive tests to one unlocked database. Verified in-session for rev 6: `mod.rs:175` builds `ScanSummary::default()` per invocation and `mod.rs:195` clears `scan_skips` only at a fresh start; rev 5's `run_scan` returned early on setup errors; rev 5's Postgres test released A's lock at commit before A's claim. Verified in-session for rev 7 against the rev-6 text: the marker writer and `fulfilled` predicate listed only `Complete`/`CompleteWithSkips` while the contract said `CompleteUnverified` fulfils; both wrappers captured `now` before `run()`/`setup()` and scheduled from it. No Rust or Postgres test was run.

