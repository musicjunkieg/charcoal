# #343 Phase 0 + Phase 1 — Measure and Shared Cache Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Instrument the gather so we know inference cores, the Bluesky rate limit and whether extra ONNX sessions pay off (Phase 0), then add the shared feed/score/verdict cache so a second user in an overlapping community scans in ≤ 60 % of the cold baseline (Phase 1).

**Architecture:** Phase 0 adds three measurements (CPU sampler, `RateLimit-Limit` capture, `CHARCOAL_ONNX_SESSIONS` pool) without touching scoring logic, plus one correctness fix (stage-1 batches its forward passes). Phase 1 adds migration v16 with three `user_did`-free tables and three **decorators** — `CachedPostFetcher`, `CachedToxicityScorer`, `CachedClassifier` — wrapped around the existing fetcher/scorer/classifier at construction time in `run_scan`, so `gather.rs`/`profile.rs`/`burst.rs` scoring code is unchanged. Cache hit/miss counts land in `scan_state` so the Phase 1 pass number is read from the DB, not Railway logs.

**Tech Stack:** Rust 2021, tokio, async-trait, rusqlite 0.38 (SQLite) + sqlx-core/sqlx-postgres (Postgres), ort 2.0.0-rc.11, sha2 0.10 (new direct dep), wiremock 0.6 (dev), serde_json, tracing.

**Spec:** `docs/superpowers/specs/2026-09-08-343-scalability-onboarding-design.md` (§4.1 as amended 2026-09-08, §6 Phase 0–1, §7).

## Global Constraints

- **Branch/PR:** work on `feat/343-phase1-cache` off `staging`; PR to `staging`; done only at **CodeRabbit APPROVED**. CodeRabbit allows **5 reviews/hour — if it says wait, wait.** Batch fixes into fewer pushes.
- **Chainlink issue before code** (hook-enforced): `chainlink issue quick "343 Phase 0+1: measurement + shared feed/classification cache" -p high -l feature`. Close with `--no-changelog`; handwrite the CHANGELOG entry.
- **Git:** explicit `git add <paths>`; no `git add -A`; no heredocs; single-quoted multi-line commit messages ending with `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>` and `Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY`. `git merge/rebase/cherry-pick/reset/tag/branch -D` are blocked. Push in the background: `git -c credential.helper='!gh auth git-credential' push https://github.com/musicjunkieg/charcoal.git feat/343-phase1-cache`.
- **Deciduous:** log `action` (`--commit HEAD`) before and `outcome` after each task; link immediately. Parent action node for this plan is **788**.
- **TDD:** failing test first, then code (spec §7). Run unit tests with `CHARCOAL_MODEL_DIR=./models cargo test --features web`; model-gated tests must show **zero** lines matching `^\s*SKIP:` under `-- --show-output`.
- **Migrations** get a fresh-DB test and an upgrade-from-v15 test on **both** backends (`cargo test --features web` and `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres`). Postgres migrations **self-record** their version (`INSERT INTO schema_version (version) VALUES (16) ON CONFLICT DO NOTHING;`).
- **Every new env knob has a clamp/default test.** Knobs in this plan: `CHARCOAL_ONNX_SESSIONS` (default 1, clamp 1..=8).
- **Cache semantics (spec §4.1):** `SNAPSHOT_TTL` = 24 h; snapshot hit requires `fetched_at > now − SNAPSHOT_TTL`; tables carry **no `user_did`**; `delete_user_data` does not touch them; no backfill.
- **Text-hash key:** `text_sha256` = lowercase hex SHA-256 of the exact `&str` handed to the model. Stage 1 scores raw text; the clean pass scores the `format_parent_reply` envelope for replies. Cache stores no readable text.
- **Privacy:** never log tokens, DPoP proofs, request bodies, `CHARCOAL_TOKEN_KEY`, `SOOT_TOKEN`; never log post text; strip credentials from any `DATABASE_URL` printed.
- **Pass numbers (spec §6):** Phase 0 — CPU cores per worker known to ±1; a `RateLimit-Limit` number; N=4 sessions must cut gather wall ≥ 25 % on a fixed 200-candidate sample or the default stays 1. Phase 1 — second staging account: snapshot hit rate ≥ 50 % **and** wall ≤ 60 % of 13 m 42 s (≤ 8 m); hit rate < 20 % → re-cost before Phase 2.
- **Clippy clean** on `--features web`, `--features postgres`, and no features. CI clippy (1.98) sees lints local 1.94 does not; if CI fails on a lint, fix it rather than allow it, unless it is `result_large_err` on an async fn returning `Result<_, Response>` (existing module-level allows).
- Comments explain **why**; `?` for errors; `anyhow::Result` at application level.

---

## File map

| Path | Responsibility |
|---|---|
| `src/toxicity/ensemble.rs` | Task 1: `score_batch` override on `TwoStageToxicityScorer` (one forward pass per stage-1 batch). |
| `src/observability/cpu_sample.rs` (new) | Task 2: `/proc/self/stat` CPU-time sampler, logs `cpu_cores_busy` every 60 s during the gather. |
| `src/observability/mod.rs` | Task 2: `pub mod cpu_sample;`; Task 7: `pub mod cache_stats;` |
| `src/bluesky/client.rs` | Task 2: capture `RateLimit-Limit` header into `PublicAtpClient`. |
| `src/pipeline/scan_phases/mod.rs` | Task 2: start the CPU sampler in `run_gather`. |
| `src/web/scan_job.rs` | Task 2: persist `bluesky_ratelimit_limit` to `scan_state`; Tasks 8–9: wrap scorer/classifier with cache decorators. |
| `src/toxicity/onnx.rs` | Task 3: `ONNX_MODEL_ID`, `CHARCOAL_ONNX_SESSIONS`, session pool with round-robin. |
| `src/bluesky/posts.rs` | Task 4: `FeedKind`, `FeedPost`, `collect_feed_posts`, `sample_from_feed`; `fetch_posts_with_replies` delegates. |
| `src/pipeline/scan_phases/gather.rs` | Task 4: `FeedSource` trait; `PostFetcher::fetch_sample` gains `did`; `AtpPostFetcher` implements `FeedSource`. |
| `src/db/schema.rs` | Task 5: migration v16 (SQLite). |
| `migrations/postgres/0016_shared_cache.sql` (new) | Task 5: migration v16 (Postgres). |
| `src/db/postgres.rs` | Task 5: register migration 16; Task 6: cache methods. |
| `src/db/traits.rs` | Task 6: `FeedSnapshot`, `OnnxScoreRow`, `ClassifierVerdictRow` row types + six new `Database` trait methods. |
| `src/db/mod.rs` | Task 6: re-export the three row types alongside `Database`. |
| `src/db/sqlite.rs` + `src/db/queries.rs` | Task 6: SQLite implementations. |
| `src/observability/cache_stats.rs` (new) | Task 7: `CacheStats` (atomic hit/miss counters) shared by all three decorators + `record_cache_stats` (writes `{prefix}_cache_hits/misses` to `scan_state`). |
| `src/pipeline/scan_phases/feed_cache.rs` (new) | Task 7: `CachedPostFetcher`, `snapshot_is_fresh`, TTL/limit constants. |
| `src/pipeline/sweep.rs`, `src/pipeline/amplification.rs` | Task 7: build `CachedPostFetcher`, record feed stats after `run_phased_scan`. |
| `src/toxicity/cached.rs` (new) | Task 8: `text_sha256`, `CachedToxicityScorer`. |
| `src/toxicity/cached_classifier.rs` (new) | Task 9: `CachedClassifier`. |
| `src/toxicity/mod.rs` | Task 8: `pub mod cached;`; Task 9: `pub mod cached_classifier;` |
| `Cargo.toml` | Task 8: `sha2 = "0.10"`. |
| `tests/unit_ensemble.rs`, `tests/unit_cpu_sample.rs` (new), `tests/unit_scan_phases.rs`, `tests/unit_feed_cache.rs` (new), `tests/unit_cache_stats.rs` (new), `tests/unit_cached_scoring.rs` (new), `tests/unit_cached_classifier.rs` (new), `tests/db_postgres.rs` | Tests per task. |
| `CHANGELOG.md`, `README.md`, `docs/runbooks/343-phase0-session-experiment.md` (new), `docs/runbooks/343-phase1-hit-rate.md` (new) | Task 10: docs and manual runbooks. |

---

### Task 1: Stage 1 batches its forward passes

**Why:** `TwoStageToxicityScorer` (src/toxicity/ensemble.rs) implements `score_text`, `score_with_context` and `classify_batch_with_contexts` but not `score_batch`, so the trait default runs one forward pass per text. Stage 1 (`scorer.score_batch(&stage1_texts)` in `src/scoring/profile.rs:337`) therefore does 25 single-post passes per account. Deciduous observation 789 records this.

**Files:**
- Modify: `src/toxicity/ensemble.rs` (inside `impl ToxicityScorer for TwoStageToxicityScorer`, next to `score_text`)
- Test: `tests/unit_ensemble.rs`

**Interfaces:**
- Consumes: `ToxicityScorer` trait (`src/toxicity/traits.rs`): `async fn score_batch(&self, texts: &[String]) -> Result<Vec<ToxicityResult>>`; `TwoStageToxicityScorer::new(primary: Box<dyn ToxicityScorer>, classifier: Arc<dyn ToxicityClassifier>)`; `StubClassifier::with_script` (`src/toxicity/classifier.rs`).
- Produces: nothing new — behavioural change only. Later tasks' `CachedToxicityScorer` (Task 8) relies on `score_batch` being the single entry point for stage 1.

- [ ] **Step 1: Write the failing test**

Append to `tests/unit_ensemble.rs` (check the top of the file for existing `use` lines and add only what is missing):

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use charcoal::toxicity::classifier::StubClassifier;
use charcoal::toxicity::ensemble::TwoStageToxicityScorer;
use charcoal::toxicity::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

/// Records how many times `score_batch` is called and how large each batch was.
struct CountingScorer {
    calls: Arc<AtomicUsize>,
    batch_sizes: Arc<std::sync::Mutex<Vec<usize>>>,
}

#[async_trait]
impl ToxicityScorer for CountingScorer {
    async fn score_text(&self, _text: &str) -> anyhow::Result<ToxicityResult> {
        // The default `score_batch` would route here one text at a time.
        // We count it as a batch of size 1 so the test can tell the two apart.
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.batch_sizes.lock().unwrap().push(1);
        Ok(ToxicityResult { toxicity: 0.0, attributes: ToxicityAttributes::default() })
    }

    async fn score_batch(&self, texts: &[String]) -> anyhow::Result<Vec<ToxicityResult>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.batch_sizes.lock().unwrap().push(texts.len());
        Ok(texts
            .iter()
            .map(|_| ToxicityResult { toxicity: 0.0, attributes: ToxicityAttributes::default() })
            .collect())
    }
}

#[tokio::test]
async fn two_stage_score_batch_is_one_forward_pass() {
    let calls = Arc::new(AtomicUsize::new(0));
    let sizes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let primary = CountingScorer { calls: Arc::clone(&calls), batch_sizes: Arc::clone(&sizes) };
    let classifier = Arc::new(StubClassifier::with_script(vec![]));
    let scorer = TwoStageToxicityScorer::new(Box::new(primary), classifier);

    let texts: Vec<String> = (0..25).map(|i| format!("post number {i}")).collect();
    let out = scorer.score_batch(&texts).await.unwrap();

    assert_eq!(out.len(), 25);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "stage-1 must be a single primary call");
    assert_eq!(*sizes.lock().unwrap(), vec![25]);
}
```

If `StubClassifier::with_script` takes a different argument type in this checkout, open `src/toxicity/classifier.rs` and use whichever constructor builds an empty stub (`StubClassifier::default()` if it exists). Do not change the classifier.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --features web --test unit_ensemble two_stage_score_batch_is_one_forward_pass`
Expected: FAIL — `assertion failed: calls == 1` with `left: 25, right: 1` (the default loops `score_text`).

- [ ] **Step 3: Add the override**

In `src/toxicity/ensemble.rs`, inside `impl ToxicityScorer for TwoStageToxicityScorer`, add directly after `score_text`:

```rust
    /// Stage 1 hands us the whole 25-post sample at once. Without this
    /// override the trait default would call `score_text` per post — 25
    /// separate ONNX forward passes and 25 mutex acquisitions on the
    /// session — so we forward the batch to the primary scorer intact.
    async fn score_batch(&self, texts: &[String]) -> Result<Vec<ToxicityResult>> {
        self.primary.score_batch(texts).await
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --features web --test unit_ensemble`
Expected: all PASS, including the new test.

- [ ] **Step 5: Commit + deciduous**

```bash
git add src/toxicity/ensemble.rs tests/unit_ensemble.rs
git commit -m 'perf(343): forward stage-1 batches to the primary scorer intact

TwoStageToxicityScorer had no score_batch override, so stage 1 ran one
ONNX forward pass per post. Delegate to primary.score_batch.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY'
deciduous add action "T1: score_batch override on TwoStageToxicityScorer" -c 90 --commit HEAD
deciduous link 788 <new_id> -r "Phase 0 task 1"
deciduous add outcome "T1 done: stage-1 is one forward pass per account" -c 90 --commit HEAD
deciduous link <action_id> <outcome_id> -r "result"
```

### Task 2: Phase 0 logging — CPU sampler and `RateLimit-Limit`

**Why:** Spec §6 Phase 0 items 1–2. We need the inference-cores-per-worker number (±1) and Bluesky's advertised per-window limit before any concurrency knob is turned.

**Files:**
- Create: `src/observability/cpu_sample.rs`
- Modify: `src/observability/mod.rs` (add `pub mod cpu_sample;`)
- Modify: `src/bluesky/client.rs:74-77` (struct), `:96-99` (constructor), `:137` (after `let status = response.status();`)
- Modify: `src/pipeline/scan_phases/mod.rs:365` (`run_gather` — start the sampler)
- Modify: `src/web/scan_job.rs:1062` (persist the observed limit before `finish_scan`)
- Test: `tests/unit_cpu_sample.rs` (new), `src/bluesky/client.rs` inline `mod tests` (wiremock)

**Interfaces:**
- Produces: `charcoal::observability::cpu_sample::parse_proc_stat_cpu_ticks(stat: &str) -> Option<u64>`; `charcoal::observability::cpu_sample::spawn_cpu_sampler() -> CpuSamplerGuard` (dropping the guard stops sampling); `PublicAtpClient::observed_rate_limit(&self) -> Option<u64>`.
- Consumes: `db.set_scan_state(user_did, key, value)` (`src/db/mod.rs`, both backends), `scan_state` table.

- [ ] **Step 1: Write the failing parser test**

Create `tests/unit_cpu_sample.rs`:

```rust
use charcoal::observability::cpu_sample::parse_proc_stat_cpu_ticks;

/// A real `/proc/self/stat` line. Field 2 (`comm`) is parenthesised and may
/// contain spaces, so the parser must split after the last `)` — fields 14
/// (utime) and 15 (stime) are counted from field 1 = pid.
const SAMPLE: &str = "12345 (charcoal web) S 1 12345 12345 0 -1 4194560 8123 0 0 0 4200 1300 0 0 20 0 9 0 1234567 987654321 45678 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";

#[test]
fn parses_utime_plus_stime() {
    // utime = 4200, stime = 1300
    assert_eq!(parse_proc_stat_cpu_ticks(SAMPLE), Some(5500));
}

#[test]
fn comm_with_spaces_and_parens_does_not_shift_fields() {
    let weird = SAMPLE.replace("(charcoal web)", "(char (coal) )web)");
    assert_eq!(parse_proc_stat_cpu_ticks(&weird), Some(5500));
}

#[test]
fn short_or_garbage_input_is_none() {
    assert_eq!(parse_proc_stat_cpu_ticks(""), None);
    assert_eq!(parse_proc_stat_cpu_ticks("1 (x) S 1 2"), None);
    assert_eq!(parse_proc_stat_cpu_ticks("no parens at all 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15"), None);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --features web --test unit_cpu_sample`
Expected: compile error — `unresolved import charcoal::observability::cpu_sample`.

- [ ] **Step 3: Implement the sampler**

Create `src/observability/cpu_sample.rs`:

```rust
//! Process CPU-time sampler for the gather phase (#343 Phase 0).
//!
//! We need to know how many cores ONNX inference actually keeps busy per
//! worker before deciding whether extra sessions or extra replicas are the
//! right lever. Railway's dashboard averages over minutes and hides the
//! gather's burstiness, so the process samples its own CPU time from
//! `/proc/self/stat` once a minute and logs `cpu_cores_busy` — the ratio of
//! CPU-seconds consumed to wall-seconds elapsed since the previous sample.
//! Only Linux exposes `/proc`; elsewhere the sampler logs one notice and
//! exits, so local macOS runs are unaffected.

use std::time::{Duration, Instant};

use tokio::task::JoinHandle;
use tracing::{info, warn};

/// How often to sample. One minute matches the spec's "once a minute" and is
/// coarse enough that the log stays readable for a 10-minute gather.
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);

/// Linux `CLK_TCK` is 100 on every glibc target we ship to (Ubuntu 24.04 in
/// the Dockerfile). Hard-coding it avoids a `libc::sysconf` call and a new
/// dependency for a diagnostic.
const CLK_TCK: f64 = 100.0;

/// Extract `utime + stime` (clock ticks) from a `/proc/self/stat` line.
///
/// Field 2 (`comm`) is wrapped in parentheses and may itself contain spaces
/// and parentheses, so we split on the **last** `)` and count fields from
/// there: after the split, `state` is index 0, so `utime` (field 14 in the
/// man page's 1-based numbering) is index 11 and `stime` is index 12.
pub fn parse_proc_stat_cpu_ticks(stat: &str) -> Option<u64> {
    let after_comm = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some(utime + stime)
}

/// Dropping this stops the background sampler.
pub struct CpuSamplerGuard {
    handle: JoinHandle<()>,
}

impl Drop for CpuSamplerGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Start sampling on the current tokio runtime. The first log line appears
/// after one interval; a gather shorter than that produces no samples,
/// which is fine — it is also not the case we are trying to measure.
pub fn spawn_cpu_sampler() -> CpuSamplerGuard {
    let handle = tokio::spawn(async {
        if !cfg!(target_os = "linux") {
            info!(metric = "cpu_cores_busy", "CPU sampler unsupported off Linux; skipping");
            return;
        }
        let mut last_ticks = read_ticks();
        let mut last_at = Instant::now();
        loop {
            tokio::time::sleep(SAMPLE_INTERVAL).await;
            let now_ticks = read_ticks();
            let now = Instant::now();
            match (last_ticks, now_ticks) {
                (Some(prev), Some(cur)) => {
                    let cpu_secs = (cur.saturating_sub(prev)) as f64 / CLK_TCK;
                    let wall_secs = now.duration_since(last_at).as_secs_f64();
                    let cores_busy = if wall_secs > 0.0 { cpu_secs / wall_secs } else { 0.0 };
                    info!(
                        metric = "cpu_cores_busy",
                        cores_busy = format!("{cores_busy:.2}"),
                        cpu_secs = format!("{cpu_secs:.1}"),
                        wall_secs = format!("{wall_secs:.1}"),
                        "gather CPU sample (#343 Phase 0)"
                    );
                }
                _ => warn!(metric = "cpu_cores_busy", "could not read /proc/self/stat"),
            }
            last_ticks = now_ticks;
            last_at = now;
        }
    });
    CpuSamplerGuard { handle }
}

fn read_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    parse_proc_stat_cpu_ticks(&stat)
}
```

Add to `src/observability/mod.rs`, after `pub mod classifier_metrics;`:

```rust
pub mod cpu_sample;
```

- [ ] **Step 4: Run the parser tests**

Run: `cargo test --features web --test unit_cpu_sample`
Expected: 3 PASS.

- [ ] **Step 5: Start the sampler in `run_gather`**

In `src/pipeline/scan_phases/mod.rs`, at the top of `run_gather`'s body (line 371, before the long comment about mapping over indices), add:

```rust
    // Phase 0 measurement (#343): sample our own CPU time while the gather
    // runs. The guard aborts the sampler when this function returns.
    let _cpu_sampler = crate::observability::cpu_sample::spawn_cpu_sampler();
```

Run: `cargo test --features web --test unit_scan_phases`
Expected: all PASS (the sampler is inert inside a test's short gather; on macOS it logs once and exits).

- [ ] **Step 6: Write the failing `RateLimit-Limit` test**

In `src/bluesky/client.rs`, inside the existing `#[cfg(test)] mod tests` (line ~262+; it already imports `wiremock::{Mock, MockServer, ResponseTemplate}` and `matchers`), add:

```rust
    #[tokio::test]
    async fn captures_ratelimit_limit_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.actor.getProfile"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("RateLimit-Limit", "3000")
                    .set_body_json(serde_json::json!({"did": "did:plc:abc"})),
            )
            .mount(&server)
            .await;
        let client = PublicAtpClient::new(&server.uri()).unwrap();
        assert_eq!(client.observed_rate_limit(), None, "nothing observed before a call");

        let _: serde_json::Value = client
            .xrpc_get("app.bsky.actor.getProfile", &[("actor", "did:plc:abc")])
            .await
            .unwrap();

        assert_eq!(client.observed_rate_limit(), Some(3000));
    }

    #[tokio::test]
    async fn missing_ratelimit_header_stays_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/app.bsky.actor.getProfile"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"did": "x"})))
            .mount(&server)
            .await;
        let client = PublicAtpClient::new(&server.uri()).unwrap();
        let _: serde_json::Value = client
            .xrpc_get("app.bsky.actor.getProfile", &[("actor", "x")])
            .await
            .unwrap();
        assert_eq!(client.observed_rate_limit(), None);
    }
```

Run: `cargo test --features web --lib bluesky::client::tests`
Expected: compile error — `no method named observed_rate_limit`.

- [ ] **Step 7: Capture the header**

In `src/bluesky/client.rs`:

Add to the imports (after `use std::time::Duration;`):

```rust
use std::sync::atomic::{AtomicU64, Ordering};
```

Change the struct (line 74) to:

```rust
pub struct PublicAtpClient {
    client: reqwest::Client,
    base_url: String,
    /// Last `RateLimit-Limit` value the public API told us (#343 Phase 0).
    /// 0 means "not observed yet" — the API never advertises a zero limit.
    rate_limit_limit: AtomicU64,
}
```

In `with_timeout`, change the `Ok(Self { … })` to:

```rust
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            rate_limit_limit: AtomicU64::new(0),
        })
```

Add a method to `impl PublicAtpClient` (after `with_timeout`):

```rust
    /// The most recent `RateLimit-Limit` header seen on any response, if any.
    /// Bluesky advertises its per-window request budget on every reply; we
    /// record it so a scan can persist the number the spec asks for without
    /// anyone reading Railway logs.
    pub fn observed_rate_limit(&self) -> Option<u64> {
        match self.rate_limit_limit.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }
```

In `xrpc_get`, directly after `let status = response.status();` (line 137), add:

```rust
            // Headers are available before the body; read the limit even on
            // a 429 so a throttled scan still reports the number.
            if let Some(limit) = response
                .headers()
                .get("ratelimit-limit")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
            {
                self.rate_limit_limit.store(limit, Ordering::Relaxed);
            }
```

- [ ] **Step 8: Run the client tests**

Run: `cargo test --features web --lib bluesky::client::tests`
Expected: all PASS, including the two new tests.

- [ ] **Step 9: Persist the number once per scan**

In `src/web/scan_job.rs`, replace line 1062 (`finish_scan(&scan_manager, user_did, claim_id, result).await`) with:

```rust
    // Phase 0 (#343): one number per scan, read from scan_state, not logs.
    // Best-effort — a failure to record a diagnostic must not fail the scan.
    if let Some(limit) = client.observed_rate_limit() {
        if let Err(e) = db
            .set_scan_state(user_did, "bluesky_ratelimit_limit", &limit.to_string())
            .await
        {
            tracing::warn!(error = %e, "could not record bluesky_ratelimit_limit");
        }
    }

    finish_scan(&scan_manager, user_did, claim_id, result).await
```

Check `set_scan_state`'s exact signature in `src/db/mod.rs` before writing this (it takes `&str, &str, &str`); adjust the borrow if it differs.

Run: `cargo build --features web` then `cargo clippy --features web --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 10: Commit + deciduous**

```bash
git add src/observability/cpu_sample.rs src/observability/mod.rs src/bluesky/client.rs src/pipeline/scan_phases/mod.rs src/web/scan_job.rs tests/unit_cpu_sample.rs
git commit -m 'feat(343): Phase 0 measurements - CPU sampler and RateLimit-Limit capture

Logs cpu_cores_busy once a minute during the gather (Linux /proc/self/stat)
and records the observed Bluesky RateLimit-Limit to scan_state once per scan.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY'
deciduous add action "T2: CPU sampler + RateLimit-Limit capture" -c 90 --commit HEAD
deciduous link 788 <new_id> -r "Phase 0 task 2"
deciduous add outcome "T2 done: cpu_cores_busy logged, bluesky_ratelimit_limit in scan_state" -c 90 --commit HEAD
deciduous link <action_id> <outcome_id> -r "result"
```

### Task 3: `CHARCOAL_ONNX_SESSIONS` session pool

**Why:** Spec §6 Phase 0 item 3 — the experiment needs a knob to run N mutexed sessions instead of one. Default stays 1 until the runbook (Task 10) shows ≥ 25 % gather-wall improvement. Each session is a separate ~126 MB model load, so the clamp ceiling is 8.

**Files:**
- Modify: `src/toxicity/onnx.rs:44-53` (struct), `:59-133` (`load`), `:154` (`score_batch` session pick)
- Test: `src/toxicity/onnx.rs` inline `#[cfg(test)]` (pure parser), `tests/unit_toxicity_token_limit.rs` (model-gated equality)

**Interfaces:**
- Produces: `pub const ONNX_MODEL_ID: &str = "detoxify-unbiased-toxic-roberta-quantized";` (Task 8 uses it as the cache `model_id`); `pub fn onnx_sessions_from_env() -> usize`; `pub fn parse_onnx_sessions(raw: Option<&str>) -> usize`; `OnnxToxicityScorer::load_with_sessions(model_dir: &Path, sessions: usize) -> Result<Self>`; `OnnxToxicityScorer::load(model_dir)` now = `load_with_sessions(model_dir, onnx_sessions_from_env())`.

- [ ] **Step 1: Write the failing knob tests**

At the bottom of `src/toxicity/onnx.rs` add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onnx_sessions_default_is_one() {
        assert_eq!(parse_onnx_sessions(None), 1);
        assert_eq!(parse_onnx_sessions(Some("")), 1);
        assert_eq!(parse_onnx_sessions(Some("banana")), 1);
    }

    #[test]
    fn onnx_sessions_clamps_to_1_through_8() {
        assert_eq!(parse_onnx_sessions(Some("0")), 1);
        assert_eq!(parse_onnx_sessions(Some("-3")), 1);
        assert_eq!(parse_onnx_sessions(Some("4")), 4);
        assert_eq!(parse_onnx_sessions(Some("8")), 8);
        assert_eq!(parse_onnx_sessions(Some("64")), 8);
        assert_eq!(parse_onnx_sessions(Some(" 2 ")), 2);
    }
}
```

Run: `cargo test --features web --lib toxicity::onnx::tests`
Expected: compile error — `cannot find function parse_onnx_sessions`.

- [ ] **Step 2: Add the constant, the parser, and the pool**

In `src/toxicity/onnx.rs`:

After the imports, add:

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
```

Near the other module-level constants (after `OUTPUT_LABELS` or the max-length constant), add:

```rust
/// Stable identity of the toxicity model for cache keys (#343 §4.1). Bump
/// this string whenever the ONNX file or tokenizer changes, or cached
/// `onnx_scores` rows from the old model will be served for the new one.
pub const ONNX_MODEL_ID: &str = "detoxify-unbiased-toxic-roberta-quantized";

/// Env knob for the Phase 0 session experiment (#343 §6). Each extra session
/// is another full model load (~126 MB), hence the small ceiling.
pub const ONNX_SESSIONS_ENV: &str = "CHARCOAL_ONNX_SESSIONS";
const ONNX_SESSIONS_DEFAULT: usize = 1;
const ONNX_SESSIONS_MAX: usize = 8;

/// Pure parser so the clamp/default rule is testable without touching the
/// process environment. Same shape as `burst_concurrency()` in burst.rs.
pub fn parse_onnx_sessions(raw: Option<&str>) -> usize {
    match raw.and_then(|v| v.trim().parse::<i64>().ok()) {
        Some(v) => (v.max(1) as usize).clamp(1, ONNX_SESSIONS_MAX),
        None => ONNX_SESSIONS_DEFAULT,
    }
}

/// Read `CHARCOAL_ONNX_SESSIONS` from the environment.
pub fn onnx_sessions_from_env() -> usize {
    parse_onnx_sessions(std::env::var(ONNX_SESSIONS_ENV).ok().as_deref())
}
```

Replace the struct (lines 44–53) with:

```rust
/// Local ONNX-based toxicity scorer. Holds one or more model sessions and
/// the tokenizer behind Arc so inference can be offloaded to spawn_blocking
/// without blocking the async runtime.
pub struct OnnxToxicityScorer {
    // Arc+Mutex because:
    // 1. ort::Session::run takes &mut self, so we need interior mutability
    // 2. spawn_blocking requires 'static, so we need Arc for shared ownership
    // 3. We need Send+Sync for the ToxicityScorer trait
    //
    // More than one session (CHARCOAL_ONNX_SESSIONS > 1) lets concurrent
    // gather tasks run forward passes in parallel instead of queueing on a
    // single mutex. Round-robin selection is good enough: every batch is
    // roughly the same size, and a busy session just makes the next caller
    // wait exactly as it would have with one session.
    sessions: Vec<Arc<Mutex<Session>>>,
    next_session: AtomicUsize,
    tokenizer: Arc<Tokenizer>,
}
```

Change `load` to delegate, and add `load_with_sessions`. Replace the `pub fn load(model_dir: &Path) -> Result<Self> {` line and its body's session-building and final `Ok(Self { … })` as follows (keep the file-existence checks and the whole tokenizer/truncation block exactly as they are):

```rust
    /// Load the ONNX model and tokenizer from the given directory, with the
    /// number of sessions taken from `CHARCOAL_ONNX_SESSIONS` (default 1).
    ///
    /// Expects `model_quantized.onnx` and `tokenizer.json` to exist in `model_dir`.
    /// Call `download::download_model()` first if they don't.
    pub fn load(model_dir: &Path) -> Result<Self> {
        Self::load_with_sessions(model_dir, onnx_sessions_from_env())
    }

    /// As [`load`](Self::load) with an explicit session count (clamped to
    /// 1..=8). Exposed so the Phase 0 experiment and its test can pick the
    /// count without touching the environment.
    pub fn load_with_sessions(model_dir: &Path, sessions: usize) -> Result<Self> {
        let sessions = sessions.clamp(1, ONNX_SESSIONS_MAX);
        let model_path = model_dir.join("model_quantized.onnx");
        let tokenizer_path = model_dir.join("tokenizer.json");

        // … existing existence checks unchanged …

        let mut pool = Vec::with_capacity(sessions);
        for _ in 0..sessions {
            let session = Session::builder()
                .context("Failed to create ONNX session builder")?
                .commit_from_file(&model_path)
                .with_context(|| {
                    format!("Failed to load ONNX model from {}", model_path.display())
                })?;
            pool.push(Arc::new(Mutex::new(session)));
        }

        // … existing tokenizer + truncation block unchanged …

        debug!(
            sessions = sessions,
            "Loaded ONNX toxicity model from {}",
            model_dir.display()
        );

        Ok(Self {
            sessions: pool,
            next_session: AtomicUsize::new(0),
            tokenizer: Arc::new(tokenizer),
        })
    }

    /// Pick the next session round-robin.
    fn pick_session(&self) -> Arc<Mutex<Session>> {
        let i = self.next_session.fetch_add(1, Ordering::Relaxed) % self.sessions.len();
        Arc::clone(&self.sessions[i])
    }
```

In `score_batch`, change `let session = Arc::clone(&self.session);` to:

```rust
        let session = self.pick_session();
```

Everything downstream (`session.lock()` inside `spawn_blocking`) is unchanged.

- [ ] **Step 3: Run knob tests + full toxicity suite**

Run: `cargo test --features web --lib toxicity::onnx::tests` → 2 PASS.
Run: `cargo build --features web` → clean (the `.session` field no longer exists; the compiler will point at any missed use).

- [ ] **Step 4: Model-gated equality test**

Append to `tests/unit_toxicity_token_limit.rs` (it already has `model_dir_or_skip` and imports `ToxicityScorer`):

```rust
/// Two sessions must score exactly like one — the pool changes throughput,
/// never results. Model-gated; prints the `SKIP:` sentinel when absent.
#[tokio::test]
async fn session_pool_scores_identically_to_single_session() {
    let Some(dir) = model_dir_or_skip("session pool equality") else {
        eprintln!("SKIP: session pool equality — toxicity model not present");
        return;
    };
    let one = charcoal::toxicity::onnx::OnnxToxicityScorer::load_with_sessions(&dir, 1)
        .expect("single session loads");
    let two = charcoal::toxicity::onnx::OnnxToxicityScorer::load_with_sessions(&dir, 2)
        .expect("two sessions load");

    let texts: Vec<String> = vec![
        "what a lovely morning for a walk".into(),
        "you are a worthless idiot and everyone knows it".into(),
        "the meeting moved to thursday".into(),
        "shut up nobody asked you".into(),
    ];
    let a = one.score_batch(&texts).await.unwrap();
    // Call twice so both sessions in the pool are exercised.
    let b1 = two.score_batch(&texts).await.unwrap();
    let b2 = two.score_batch(&texts).await.unwrap();

    for ((x, y), z) in a.iter().zip(&b1).zip(&b2) {
        assert!((x.toxicity - y.toxicity).abs() < 1e-6, "{} vs {}", x.toxicity, y.toxicity);
        assert!((x.toxicity - z.toxicity).abs() < 1e-6, "{} vs {}", x.toxicity, z.toxicity);
    }
}
```

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test unit_toxicity_token_limit -- --show-output 2>&1 | grep -E "^\s*SKIP:|test result"`
Expected: `test result: ok` and **no** `SKIP:` line.

- [ ] **Step 5: Clippy, commit, deciduous**

Run: `cargo clippy --features web --all-targets -- -D warnings` → clean.

```bash
git add src/toxicity/onnx.rs tests/unit_toxicity_token_limit.rs
git commit -m 'feat(343): CHARCOAL_ONNX_SESSIONS session pool for the Phase 0 experiment

Default 1, clamp 1..=8, round-robin selection. Adds ONNX_MODEL_ID for the
Phase 1 cache key.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY'
deciduous add action "T3: ONNX session pool knob" -c 90 --commit HEAD
deciduous link 788 <new_id> -r "Phase 0 task 3"
deciduous add outcome "T3 done: CHARCOAL_ONNX_SESSIONS wired, equality test green" -c 90 --commit HEAD
deciduous link <action_id> <outcome_id> -r "result"
```

### Task 4: Feed refactor — `FeedPost`, `FeedSource`, `PostFetcher::fetch_sample(did, …)`

**Why:** To cache a feed we must store what was fetched **before** it is partitioned into a `PostSample` (the snapshot must serve any `limit`). Today `fetch_posts_with_replies` fetches and partitions in one loop, and the gather fetches 25 then 50 for ~60 % of accounts. This task splits fetch from sample, gives the fetcher the DID it needs as a cache key, and changes no behaviour: `fetch_posts_with_replies` returns the same `PostSample` as before.

**Files:**
- Modify: `src/bluesky/posts.rs:96-109` (types), `:274-427` (`fetch_posts_with_replies`)
- Modify: `src/pipeline/scan_phases/gather.rs:112-140` (traits), `:297`, `:327` (call sites)
- Modify: `tests/unit_scan_phases.rs` — every `impl PostFetcher` (lines ~641, 1123, 1240, 2869, 2894, 2907, 3494): add the `_did: &str` first parameter
- Test: `src/bluesky/posts.rs` inline `#[cfg(test)]` for `sample_from_feed`

**Interfaces:**
- Produces (in `charcoal::bluesky::posts`):
  ```rust
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  pub enum FeedKind { Original, Reply { parent_uri: String }, Quote }
  #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
  pub struct FeedPost { pub post: Post, pub kind: FeedKind }
  pub async fn collect_feed_posts(client: &PublicAtpClient, handle: &str, max_posts: usize) -> Result<Vec<FeedPost>>;
  pub fn sample_from_feed(feed: &[FeedPost], limit: usize) -> PostSample;
  ```
- Produces (in `charcoal::pipeline::scan_phases::gather`):
  ```rust
  #[async_trait]
  pub trait FeedSource: Send + Sync {
      async fn fetch_feed(&self, handle: &str, max_posts: usize) -> Result<Vec<FeedPost>>;
      async fn fetch_parents(&self, uris: &[String]) -> Result<HashMap<String, String>>;
  }
  #[async_trait]
  pub trait PostFetcher: Send + Sync {
      async fn fetch_sample(&self, did: &str, handle: &str, limit: usize) -> Result<PostSample>;
      async fn fetch_parents(&self, uris: &[String]) -> Result<HashMap<String, String>>;
  }
  ```
  `AtpPostFetcher<'a> { pub client: &'a PublicAtpClient }` implements **`FeedSource`** (its `PostFetcher` impl is removed; Task 7's `CachedPostFetcher` is the production `PostFetcher`). Until Task 7 lands, `AtpPostFetcher` also keeps a temporary `PostFetcher` impl (shown below) so `sweep.rs`/`amplification.rs` keep compiling — Task 7 deletes it.

- [ ] **Step 1: Write the failing `sample_from_feed` tests**

At the bottom of `src/bluesky/posts.rs`, add (or extend the existing `#[cfg(test)] mod tests` if one exists — check with `grep -n "cfg(test)" src/bluesky/posts.rs`):

```rust
#[cfg(test)]
mod feed_sample_tests {
    use super::*;

    fn post(uri: &str) -> Post {
        Post {
            uri: uri.to_string(),
            text: format!("text for {uri} long enough"),
            created_at: None,
            like_count: 0,
            repost_count: 0,
            quote_count: 0,
            is_quote: false,
            langs: vec![],
        }
    }

    fn feed() -> Vec<FeedPost> {
        vec![
            FeedPost { post: post("at://a/1"), kind: FeedKind::Original },
            FeedPost { post: post("at://a/2"), kind: FeedKind::Reply { parent_uri: "at://b/9".into() } },
            FeedPost { post: post("at://a/3"), kind: FeedKind::Quote },
            FeedPost { post: post("at://a/4"), kind: FeedKind::Reply { parent_uri: "at://b/8".into() } },
        ]
    }

    #[test]
    fn partitions_and_computes_ratios_over_the_whole_feed() {
        let s = sample_from_feed(&feed(), 50);
        assert_eq!(s.total_posts, 4);
        assert_eq!(s.originals.iter().map(|p| p.uri.as_str()).collect::<Vec<_>>(), ["at://a/1"]);
        assert_eq!(s.replies.len(), 2);
        assert_eq!(s.replies[0].parent_uri, "at://b/9");
        assert_eq!(s.quotes.len(), 1);
        assert!((s.reply_ratio - 0.5).abs() < 1e-9);
        assert!((s.quote_ratio - 0.25).abs() < 1e-9);
    }

    #[test]
    fn limit_takes_a_prefix_and_ratios_follow_the_prefix() {
        // Same rule as the paged fetch: stop after `limit` posts, ratios over
        // what was kept. First two = 1 original + 1 reply.
        let s = sample_from_feed(&feed(), 2);
        assert_eq!(s.total_posts, 2);
        assert_eq!(s.originals.len(), 1);
        assert_eq!(s.replies.len(), 1);
        assert_eq!(s.quotes.len(), 0);
        assert!((s.reply_ratio - 0.5).abs() < 1e-9);
        assert!((s.quote_ratio - 0.0).abs() < 1e-9);
    }

    #[test]
    fn empty_feed_has_zero_ratios() {
        let s = sample_from_feed(&[], 25);
        assert_eq!(s.total_posts, 0);
        assert_eq!(s.reply_ratio, 0.0);
        assert_eq!(s.quote_ratio, 0.0);
    }

    #[test]
    fn feed_post_round_trips_through_json() {
        let f = feed();
        let json = serde_json::to_string(&f).unwrap();
        let back: Vec<FeedPost> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }
}
```

Run: `cargo test --features web --lib bluesky::posts::feed_sample_tests`
Expected: compile error — `FeedPost`/`FeedKind`/`sample_from_feed` not found.

- [ ] **Step 2: Add the types and split the function**

In `src/bluesky/posts.rs`, after `PostSample` (line 109), add:

```rust
/// How a post sits in its author's feed. Kept alongside the post so a cached
/// feed (#343 §4.1) can be re-partitioned into a [`PostSample`] later without
/// refetching — the reply/quote classification is decided at fetch time from
/// feed metadata that the `Post` itself does not carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FeedKind {
    Original,
    Reply { parent_uri: String },
    Quote,
}

/// One authored (non-repost) post as it appeared in `getAuthorFeed`, in feed
/// order. This is the unit stored in `account_feed_snapshots.posts_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedPost {
    pub post: Post,
    pub kind: FeedKind,
}
```

Replace `fetch_posts_with_replies` (lines 274–427) with three functions:

```rust
/// Fetch up to `max_posts` authored posts (reposts skipped) in feed order,
/// each tagged with its [`FeedKind`]. This is the network half of
/// [`fetch_posts_with_replies`]; [`sample_from_feed`] is the pure half.
pub async fn collect_feed_posts(
    client: &PublicAtpClient,
    handle: &str,
    max_posts: usize,
) -> Result<Vec<FeedPost>> {
    let mut feed_posts: Vec<FeedPost> = Vec::new();
    let mut cursor: Option<String> = None;

    // How many to request per page (API max is 100).
    let page_size = max_posts.min(100).to_string();

    loop {
        let mut params: Vec<(&str, &str)> = vec![
            ("actor", handle),
            ("filter", "posts_with_replies"),
            ("limit", &page_size),
        ];
        if let Some(ref c) = cursor {
            params.push(("cursor", c));
        }

        let output: get_author_feed::Output = client
            .xrpc_get("app.bsky.feed.getAuthorFeed", &params)
            .await
            .with_context(|| format!("Failed to fetch feed for @{}", handle))?;

        for feed_item in &output.feed {
            // Skip reposts — we only want posts authored by this account.
            if feed_item.reason.is_some() {
                continue;
            }

            let post_view = &feed_item.post;

            // Decode the record to get the post text and reply reference.
            let record = match atrium_api::app::bsky::feed::post::Record::try_from_unknown(
                post_view.record.clone(),
            ) {
                Ok(r) => r,
                Err(_) => continue,
            };

            let text = sanitize_post_text(&record.data.text);
            let langs = extract_langs(&record);

            // Skip empty posts and very short posts (likely just links/images).
            if text.chars().count() < 15 {
                continue;
            }

            // Detect quote-posts by checking the embed type.
            let is_quote = post_view.embed.as_ref().is_some_and(|embed| {
                use atrium_api::types::Union;
                matches!(
                    embed,
                    Union::Refs(
                        atrium_api::app::bsky::feed::defs::PostViewEmbedRefs::AppBskyEmbedRecordView(_)
                            | atrium_api::app::bsky::feed::defs::PostViewEmbedRefs::AppBskyEmbedRecordWithMediaView(_)
                    )
                )
            });

            let post = Post {
                uri: post_view.uri.clone(),
                text,
                created_at: Some(post_view.indexed_at.as_ref().to_string()),
                like_count: post_view.like_count.unwrap_or(0),
                repost_count: post_view.repost_count.unwrap_or(0),
                quote_count: post_view.quote_count.unwrap_or(0),
                is_quote,
                langs,
            };

            // Classify: reply takes priority over quote (reply context is more
            // important for NLI pair scoring than the quote relationship).
            let kind = if feed_item.reply.is_some() {
                let parent_uri = record
                    .data
                    .reply
                    .as_ref()
                    .map(|r| r.parent.uri.clone())
                    .unwrap_or_default();
                if parent_uri.is_empty() {
                    // Edge case: feed says it's a reply but no parent URI in
                    // record. Treat as original.
                    FeedKind::Original
                } else {
                    FeedKind::Reply { parent_uri }
                }
            } else if is_quote {
                FeedKind::Quote
            } else {
                FeedKind::Original
            };

            feed_posts.push(FeedPost { post, kind });

            if feed_posts.len() >= max_posts {
                break;
            }
        }

        debug!(
            page_posts = output.feed.len(),
            total_collected = feed_posts.len(),
            "Fetched page of posts (with replies) for @{}",
            handle
        );

        if feed_posts.len() >= max_posts {
            break;
        }

        cursor = output.data.cursor.clone();
        if cursor.is_none() || output.feed.is_empty() {
            break;
        }
    }

    Ok(feed_posts)
}

/// Partition the first `limit` feed posts into a [`PostSample`]. Pure, so a
/// cached feed of 50 can serve a stage-1 sample of 25 with identical results
/// to fetching 25 directly (the paged fetch also stops after `limit`).
pub fn sample_from_feed(feed: &[FeedPost], limit: usize) -> PostSample {
    let mut originals = Vec::new();
    let mut replies = Vec::new();
    let mut quotes = Vec::new();
    let mut total_collected: usize = 0;

    for fp in feed.iter().take(limit) {
        total_collected += 1;
        match &fp.kind {
            FeedKind::Original => originals.push(fp.post.clone()),
            FeedKind::Reply { parent_uri } => replies.push(ReplyPost {
                post: fp.post.clone(),
                parent_uri: parent_uri.clone(),
            }),
            FeedKind::Quote => quotes.push(fp.post.clone()),
        }
    }

    let reply_ratio = if total_collected > 0 {
        replies.len() as f64 / total_collected as f64
    } else {
        0.0
    };
    let quote_ratio = if total_collected > 0 {
        quotes.len() as f64 / total_collected as f64
    } else {
        0.0
    };

    PostSample {
        originals,
        replies,
        quotes,
        reply_ratio,
        quote_ratio,
        total_posts: total_collected,
    }
}

/// Fetch recent posts with replies included, partitioned into a PostSample.
///
/// Uses the `posts_with_replies` filter to get both original posts and replies
/// from the same API call. Partitions into originals, replies (with parent URIs),
/// and quotes. Computes reply and quote ratios from the same data.
///
/// This replaces the pattern of calling `fetch_recent_posts` + `fetch_reply_ratio`
/// separately — one API call yields both toxicity-scoreable text AND behavioral ratios.
pub async fn fetch_posts_with_replies(
    client: &PublicAtpClient,
    handle: &str,
    max_posts: usize,
) -> Result<PostSample> {
    let feed = collect_feed_posts(client, handle, max_posts).await?;
    let sample = sample_from_feed(&feed, max_posts);

    info!(
        originals = sample.originals.len(),
        replies = sample.replies.len(),
        quotes = sample.quotes.len(),
        reply_ratio = format!("{:.2}", sample.reply_ratio),
        quote_ratio = format!("{:.2}", sample.quote_ratio),
        handle = handle,
        "Partitioned post sample"
    );

    Ok(sample)
}
```

Run: `cargo test --features web --lib bluesky::posts::feed_sample_tests` → 4 PASS.

- [ ] **Step 3: Update the gather traits and call sites**

In `src/pipeline/scan_phases/gather.rs`:

Change the import on line 28 to `use crate::bluesky::posts::{self, FeedPost, PostSample};`.

Replace lines 112–140 (`PostFetcher` trait + `AtpPostFetcher` + its impl) with:

```rust
/// The raw feed reads `gather_account` needs, over the concrete
/// `&PublicAtpClient` (which can't be mocked). The production
/// [`AtpPostFetcher`] forwards to `posts::collect_feed_posts` /
/// `posts::fetch_parent_posts`; the cache layer (`feed_cache.rs`) wraps this
/// and exposes [`PostFetcher`] to the gather.
#[async_trait]
pub trait FeedSource: Send + Sync {
    /// Fetch up to `max_posts` authored posts in feed order.
    async fn fetch_feed(&self, handle: &str, max_posts: usize) -> Result<Vec<FeedPost>>;

    /// Fetch parent post texts for the given AT URIs, keyed by URI.
    async fn fetch_parents(&self, uris: &[String]) -> Result<HashMap<String, String>>;
}

/// What `gather_account` consumes: a partitioned sample for a given account.
/// `did` is the cache key (#343 §4.1) — handles change, DIDs don't.
#[async_trait]
pub trait PostFetcher: Send + Sync {
    /// Fetch up to `limit` recent posts (with replies/quotes partitioned).
    async fn fetch_sample(&self, did: &str, handle: &str, limit: usize) -> Result<PostSample>;

    /// Fetch parent post texts for the given AT URIs, keyed by URI.
    async fn fetch_parents(&self, uris: &[String]) -> Result<HashMap<String, String>>;
}

/// Production [`FeedSource`] backed by the public AT Protocol client.
pub struct AtpPostFetcher<'a> {
    pub client: &'a PublicAtpClient,
}

#[async_trait]
impl FeedSource for AtpPostFetcher<'_> {
    async fn fetch_feed(&self, handle: &str, max_posts: usize) -> Result<Vec<FeedPost>> {
        posts::collect_feed_posts(self.client, handle, max_posts).await
    }

    async fn fetch_parents(&self, uris: &[String]) -> Result<HashMap<String, String>> {
        posts::fetch_parent_posts(self.client, uris).await
    }
}

// TEMPORARY until Task 7 lands `CachedPostFetcher`: keeps sweep.rs and
// amplification.rs compiling with today's uncached behaviour.
#[async_trait]
impl PostFetcher for AtpPostFetcher<'_> {
    async fn fetch_sample(&self, _did: &str, handle: &str, limit: usize) -> Result<PostSample> {
        posts::fetch_posts_with_replies(self.client, handle, limit).await
    }

    async fn fetch_parents(&self, uris: &[String]) -> Result<HashMap<String, String>> {
        posts::fetch_parent_posts(self.client, uris).await
    }
}
```

Change line 297 to `let r = fetcher.fetch_sample(inputs.account_did, inputs.account_handle, 25).await?;` and line 327 to `let r = fetcher.fetch_sample(inputs.account_did, inputs.account_handle, 50).await?;`.

- [ ] **Step 4: Update the test doubles**

In `tests/unit_scan_phases.rs`, every `async fn fetch_sample(&self, _handle: &str, …)` / `(&self, handle: &str, …)` gains a leading `_did: &str` parameter. Use:

```bash
grep -n "async fn fetch_sample(&self, " tests/unit_scan_phases.rs
```

and edit each (7 sites at ~641, 1123, 1240, 2869, 2894, 2907, 3494) to `async fn fetch_sample(&self, _did: &str, <existing params>)`. Do not change bodies.

Run: `cargo test --features web --test unit_scan_phases`
Expected: all PASS (behaviour unchanged).

Run: `cargo clippy --features web --all-targets -- -D warnings` → clean. Also run `cargo test --features web --lib bluesky::` to cover any wiremock tests of `fetch_posts_with_replies`.

- [ ] **Step 5: Commit + deciduous**

```bash
git add src/bluesky/posts.rs src/pipeline/scan_phases/gather.rs tests/unit_scan_phases.rs
git commit -m 'refactor(343): split feed fetch from sampling; PostFetcher takes the DID

collect_feed_posts + pure sample_from_feed replace the fused loop so a cached
feed can be re-partitioned. FeedSource is the raw network trait; PostFetcher
gains did for the cache key. No behaviour change.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY'
deciduous add action "T4: FeedPost/FeedSource refactor" -c 90 --commit HEAD
deciduous link 788 <new_id> -r "Phase 1 task 4"
deciduous add outcome "T4 done: feed fetch split from sampling, tests green" -c 90 --commit HEAD
deciduous link <action_id> <outcome_id> -r "result"
```

### Task 5: Migration v16 — `account_feed_snapshots`, `onnx_scores`, `classifier_verdicts`

**Why:** Spec §4.1 (amended). Both backends; fresh-DB and upgrade-from-v15 tests on both (spec §7). Postgres self-records.

**Files:**
- Modify: `src/db/schema.rs:530` (after the v15 `run_migration` block), `:662` and `:776` (`(1..=15)` → `(1..=16)`), `:767-772` (table count 17 → 20)
- Create: `migrations/postgres/0016_shared_cache.sql`
- Modify: `src/db/postgres.rs:181-184` (append `(16, include_str!(...))`)
- Test: `src/db/schema.rs` inline tests; `tests/db_postgres.rs`

**Interfaces:**
- Produces: the three tables below, identical column names on both backends. Timestamps are RFC3339 `TEXT` on **both** backends, computed in Rust — the trait convention (see the 0015 header). The 24 h TTL comparison happens in Rust, not SQL.

```
account_feed_snapshots(did TEXT PK, handle TEXT NOT NULL, posts_json TEXT NOT NULL, fetched_at TEXT NOT NULL, source TEXT NOT NULL)
onnx_scores(text_sha256 TEXT NOT NULL, model_id TEXT NOT NULL, score REAL NOT NULL, scored_at TEXT NOT NULL, PK(text_sha256, model_id))
classifier_verdicts(text_sha256 TEXT NOT NULL, model_id TEXT NOT NULL, policy_version TEXT NOT NULL, toxic_token BOOLEAN NOT NULL, confidence REAL NOT NULL, classified_at TEXT NOT NULL, PK(text_sha256, model_id, policy_version))
```

- [ ] **Step 1: Write the failing SQLite tests**

In `src/db/schema.rs` `mod tests`, change both `assert_eq!(versions, (1..=15).collect::<Vec<i64>>());` (lines ~662 and ~776) to `(1..=16)`. In `test_migration_v4_updates_table_count` change the comment to end `…, oauth_sessions, action_batches, actions, account_feed_snapshots, onnx_scores, classifier_verdicts = 20 tables (v16)` and the assertion to `assert_eq!(count, 20i64);`.

Then add after `test_migration_v14_creates_access_requests`:

```rust
    #[test]
    fn test_migration_v16_creates_cache_tables_on_a_fresh_database() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn.execute(
            "INSERT INTO account_feed_snapshots (did, handle, posts_json, fetched_at, source)
             VALUES ('did:plc:snap', 's.bsky.social', '[]', '2026-09-08T00:00:00Z', 'bluesky')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO onnx_scores (text_sha256, model_id, score, scored_at)
             VALUES ('ab', 'm1', 0.42, '2026-09-08T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO classifier_verdicts
                (text_sha256, model_id, policy_version, toxic_token, confidence, classified_at)
             VALUES ('ab', 'cope-b', 'v3', 1, 0.9, '2026-09-08T00:00:00Z')",
            [],
        )
        .unwrap();
        // Composite primary keys reject duplicates.
        assert!(conn
            .execute(
                "INSERT INTO onnx_scores (text_sha256, model_id, score, scored_at)
                 VALUES ('ab', 'm1', 0.5, '2026-09-08T00:00:01Z')",
                [],
            )
            .is_err());
    }

    /// Simulate a database that stopped at v15: drop the v16 tables and the
    /// version row, then re-run `create_tables` — the migration must apply.
    #[test]
    fn test_migration_v16_upgrades_a_v15_database() {
        let conn = Connection::open_in_memory().unwrap();
        create_tables(&conn).unwrap();
        conn.execute_batch(
            "DROP TABLE account_feed_snapshots;
             DROP TABLE onnx_scores;
             DROP TABLE classifier_verdicts;
             DELETE FROM schema_version WHERE version = 16;",
        )
        .unwrap();
        assert_eq!(table_count(&conn).unwrap(), 17i64, "v15 shape");

        create_tables(&conn).unwrap();

        assert_eq!(table_count(&conn).unwrap(), 20i64);
        let max: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(max, 16);
    }
```

Run: `cargo test --features web --lib db::schema::tests`
Expected: FAIL — `no such table: account_feed_snapshots`, and the `(1..=16)`/`20` assertions fail.

- [ ] **Step 2: Add the SQLite migration**

In `src/db/schema.rs`, after the v15 `run_migration(...)?;` block (line ~530) and before `Ok(())`, add:

```rust
    // v16 (#343 §4.1): the shared cache. None of these tables carry a
    // user_did — a post's toxicity is a property of the post, so one user's
    // scan can serve the next. Scores and verdicts are keyed by the SHA-256
    // of the exact text the model saw (stage 1 scores raw text, the clean
    // pass scores the parent+reply envelope, so post_uri is not a valid key)
    // and store no readable text. delete_user_data does not touch them.
    run_migration(conn, 16, |c| {
        c.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS account_feed_snapshots (
                 did TEXT PRIMARY KEY,
                 handle TEXT NOT NULL,
                 posts_json TEXT NOT NULL,
                 fetched_at TEXT NOT NULL,
                 source TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS onnx_scores (
                 text_sha256 TEXT NOT NULL,
                 model_id TEXT NOT NULL,
                 score REAL NOT NULL,
                 scored_at TEXT NOT NULL,
                 PRIMARY KEY (text_sha256, model_id)
             );
             CREATE TABLE IF NOT EXISTS classifier_verdicts (
                 text_sha256 TEXT NOT NULL,
                 model_id TEXT NOT NULL,
                 policy_version TEXT NOT NULL,
                 toxic_token INTEGER NOT NULL,
                 confidence REAL NOT NULL,
                 classified_at TEXT NOT NULL,
                 PRIMARY KEY (text_sha256, model_id, policy_version)
             );
             COMMIT;",
        )
    })?;
```

Run: `cargo test --features web --lib db::schema::tests` → all PASS.

- [ ] **Step 3: Postgres migration file**

Create `migrations/postgres/0016_shared_cache.sql`:

```sql
-- #343 §4.1: the shared cache. No user_did on any of these — a post's
-- toxicity is a property of the post, so one user's scan serves the next.
-- onnx_scores / classifier_verdicts are keyed by the SHA-256 (hex) of the
-- exact text the model saw: stage 1 scores raw text, the clean pass scores
-- the "[Parent post]: …\n\n[Reply]: …" envelope, so post_uri is not a valid
-- key. They store no readable text. NOT cascaded by delete_user_data.
-- Timestamps are RFC3339 TEXT computed in Rust (trait convention).

CREATE TABLE IF NOT EXISTS account_feed_snapshots (
    did TEXT PRIMARY KEY,
    handle TEXT NOT NULL,
    posts_json TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    source TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS onnx_scores (
    text_sha256 TEXT NOT NULL,
    model_id TEXT NOT NULL,
    score DOUBLE PRECISION NOT NULL,
    scored_at TEXT NOT NULL,
    PRIMARY KEY (text_sha256, model_id)
);

CREATE TABLE IF NOT EXISTS classifier_verdicts (
    text_sha256 TEXT NOT NULL,
    model_id TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    toxic_token BOOLEAN NOT NULL,
    confidence DOUBLE PRECISION NOT NULL,
    classified_at TEXT NOT NULL,
    PRIMARY KEY (text_sha256, model_id, policy_version)
);

-- The runner does NOT record the version for you. A migration that omits
-- this re-runs on every boot, forever.
INSERT INTO schema_version (version) VALUES (16) ON CONFLICT DO NOTHING;
```

In `src/db/postgres.rs`, after the `(15, include_str!("../../migrations/postgres/0015_actions.sql")),` entry add:

```rust
                (
                    16,
                    include_str!("../../migrations/postgres/0016_shared_cache.sql"),
                ),
```

- [ ] **Step 4: Postgres fresh + upgrade tests**

Append to `tests/db_postgres.rs`:

```rust
/// v16 (#343): fresh connect creates the three cache tables and records 16.
#[tokio::test]
async fn test_pg_migration_v16_creates_cache_tables() {
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();

    let names: Vec<String> = sqlx_core::query::query(
        "SELECT table_name::text FROM information_schema.tables
         WHERE table_schema = 'public'
           AND table_name IN ('account_feed_snapshots','onnx_scores','classifier_verdicts')
         ORDER BY table_name",
    )
    .fetch_all(&pool)
    .await
    .unwrap()
    .into_iter()
    .map(|r| r.get::<String, _>(0))
    .collect();
    assert_eq!(names, ["account_feed_snapshots", "classifier_verdicts", "onnx_scores"]);

    let recorded: bool =
        sqlx_core::query::query("SELECT COUNT(*) > 0 FROM schema_version WHERE version = 16")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert!(recorded, "0016 must self-record its version");
}

/// Simulate a v15 database (drop the v16 tables and version row), reconnect,
/// and prove the migration re-applies exactly once.
#[tokio::test]
async fn test_pg_migration_v16_upgrades_from_v15() {
    let Some(url) = database_url() else {
        return;
    };
    use sqlx_core::pool::Pool;
    use sqlx_core::row::Row;
    use sqlx_postgres::Postgres;

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();
    let pool = Pool::<Postgres>::connect(&url).await.unwrap();
    sqlx_core::query::query(
        "DROP TABLE IF EXISTS account_feed_snapshots, onnx_scores, classifier_verdicts;
         DELETE FROM schema_version WHERE version = 16;",
    )
    .execute(&pool)
    .await
    .unwrap();

    let _db = charcoal::db::connect_postgres(&url).await.unwrap();

    let count: i64 = sqlx_core::query::query(
        "SELECT COUNT(*) FROM information_schema.tables
         WHERE table_schema = 'public'
           AND table_name IN ('account_feed_snapshots','onnx_scores','classifier_verdicts')",
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .get(0);
    assert_eq!(count, 3);
    let versions: i64 =
        sqlx_core::query::query("SELECT COUNT(*) FROM schema_version WHERE version = 16")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get(0);
    assert_eq!(versions, 1);
}
```

These two tests mutate shared schema; they must not run concurrently with each other. If `tests/db_postgres.rs` already uses `serial_test`, add `#[serial_test::serial]` to both; otherwise run the pg suite with `-- --test-threads=1` (add that to the command below).

Run: `createdb charcoal_test 2>/dev/null; DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres --test db_postgres -- --test-threads=1 migration_v16`
Expected: 2 PASS.

- [ ] **Step 5: Clippy on all three feature sets, commit, deciduous**

```bash
cargo clippy --features web --all-targets -- -D warnings
cargo clippy --features postgres --all-targets -- -D warnings
cargo clippy --all-targets -- -D warnings
git add src/db/schema.rs src/db/postgres.rs migrations/postgres/0016_shared_cache.sql tests/db_postgres.rs
git commit -m 'feat(343): migration v16 - shared cache tables on both backends

account_feed_snapshots, onnx_scores, classifier_verdicts. No user_did;
keyed by DID or text SHA-256. Fresh + upgrade-from-v15 tests on SQLite and
Postgres; 0016 self-records its version.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY'
deciduous add action "T5: migration v16 shared cache tables" -c 90 --commit HEAD
deciduous link 788 <new_id> -r "Phase 1 task 5"
deciduous add outcome "T5 done: v16 on both backends, 4 migration tests green" -c 90 --commit HEAD
deciduous link <action_id> <outcome_id> -r "result"
```

### Task 6: `Database` trait — six cache methods on both backends

**Why:** The decorators (Tasks 7–9) need a backend-agnostic way to read and write the three v16 tables. Only two `Database` impls exist (`src/db/sqlite.rs:41`, `src/db/postgres.rs:228`).

**Files:**
- Modify: `src/db/traits.rs` (row types after `AccessRequestRow` at line ~167; methods at the end of the trait, after the OAuth/actions section)
- Modify: `src/db/queries.rs` (SQLite SQL, at the end), `src/db/sqlite.rs` (delegations, at the end of the impl), `src/db/postgres.rs` (at the end of the impl)
- Test: `tests/unit_feed_cache.rs` (new — SQLite, also used by Task 7), `tests/db_postgres.rs`

**Interfaces:**
- Produces (re-exported from `charcoal::db` via `src/db/mod.rs:20`):
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub struct FeedSnapshot { pub did: String, pub handle: String, pub posts_json: String, pub fetched_at: String /* RFC3339 */, pub source: String }
  #[derive(Debug, Clone, PartialEq)]
  pub struct OnnxScoreRow { pub text_sha256: String, pub score: f64 }
  #[derive(Debug, Clone, PartialEq)]
  pub struct ClassifierVerdictRow { pub text_sha256: String, pub toxic_token: bool, pub confidence: f64 }

  async fn get_feed_snapshot(&self, did: &str) -> Result<Option<FeedSnapshot>>;
  async fn upsert_feed_snapshot(&self, snapshot: &FeedSnapshot) -> Result<()>;
  async fn get_onnx_scores(&self, model_id: &str, hashes: &[String]) -> Result<HashMap<String, f64>>;
  async fn upsert_onnx_scores(&self, model_id: &str, rows: &[OnnxScoreRow]) -> Result<()>;
  async fn get_classifier_verdicts(&self, model_id: &str, policy_version: &str, hashes: &[String]) -> Result<HashMap<String, ClassifierVerdictRow>>;
  async fn upsert_classifier_verdicts(&self, model_id: &str, policy_version: &str, rows: &[ClassifierVerdictRow]) -> Result<()>;
  ```
  `scored_at`/`classified_at` are written by the backend as `chrono::Utc::now().to_rfc3339()`; callers do not supply them. Empty `hashes`/`rows` return `Ok` immediately without touching the DB.

- [ ] **Step 1: Write the failing SQLite tests**

Create `tests/unit_feed_cache.rs`:

```rust
//! Shared-cache DB methods (#343 §4.1) and the CachedPostFetcher decorator.

use std::collections::HashMap;
use std::sync::Arc;

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::{ClassifierVerdictRow, Database, FeedSnapshot, OnnxScoreRow};
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

#[tokio::test]
async fn feed_snapshot_round_trip_and_overwrite() {
    let db = setup_db();
    assert!(db.get_feed_snapshot("did:plc:a").await.unwrap().is_none());

    let snap = FeedSnapshot {
        did: "did:plc:a".into(),
        handle: "a.bsky.social".into(),
        posts_json: "[]".into(),
        fetched_at: "2026-09-08T00:00:00+00:00".into(),
        source: "bluesky".into(),
    };
    db.upsert_feed_snapshot(&snap).await.unwrap();
    assert_eq!(db.get_feed_snapshot("did:plc:a").await.unwrap(), Some(snap.clone()));

    // Same DID, new handle and time: the row is replaced, not duplicated.
    let newer = FeedSnapshot {
        handle: "renamed.bsky.social".into(),
        fetched_at: "2026-09-09T00:00:00+00:00".into(),
        posts_json: "[{}]".into(),
        ..snap
    };
    db.upsert_feed_snapshot(&newer).await.unwrap();
    assert_eq!(db.get_feed_snapshot("did:plc:a").await.unwrap(), Some(newer));
}

#[tokio::test]
async fn onnx_scores_lookup_is_scoped_by_model_and_returns_only_hits() {
    let db = setup_db();
    let rows = vec![
        OnnxScoreRow { text_sha256: "h1".into(), score: 0.1 },
        OnnxScoreRow { text_sha256: "h2".into(), score: 0.9 },
    ];
    db.upsert_onnx_scores("model-a", &rows).await.unwrap();

    let got = db
        .get_onnx_scores("model-a", &["h1".into(), "h2".into(), "h3".into()])
        .await
        .unwrap();
    let want: HashMap<String, f64> = [("h1".to_string(), 0.1), ("h2".to_string(), 0.9)].into();
    assert_eq!(got, want);

    // A different model id sees nothing.
    assert!(db
        .get_onnx_scores("model-b", &["h1".into()])
        .await
        .unwrap()
        .is_empty());

    // Empty lookup is a no-op.
    assert!(db.get_onnx_scores("model-a", &[]).await.unwrap().is_empty());
    db.upsert_onnx_scores("model-a", &[]).await.unwrap();
}

#[tokio::test]
async fn onnx_scores_upsert_overwrites_same_key() {
    let db = setup_db();
    db.upsert_onnx_scores("m", &[OnnxScoreRow { text_sha256: "h".into(), score: 0.2 }])
        .await
        .unwrap();
    db.upsert_onnx_scores("m", &[OnnxScoreRow { text_sha256: "h".into(), score: 0.7 }])
        .await
        .unwrap();
    let got = db.get_onnx_scores("m", &["h".into()]).await.unwrap();
    assert_eq!(got["h"], 0.7);
}

#[tokio::test]
async fn classifier_verdicts_scoped_by_model_and_policy() {
    let db = setup_db();
    let rows = vec![
        ClassifierVerdictRow { text_sha256: "h1".into(), toxic_token: true, confidence: 0.95 },
        ClassifierVerdictRow { text_sha256: "h2".into(), toxic_token: false, confidence: 0.6 },
    ];
    db.upsert_classifier_verdicts("cope-b", "v3", &rows).await.unwrap();

    let got = db
        .get_classifier_verdicts("cope-b", "v3", &["h1".into(), "h2".into(), "zzz".into()])
        .await
        .unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got["h1"], rows[0]);
    assert_eq!(got["h2"], rows[1]);

    // Policy bump invalidates.
    assert!(db
        .get_classifier_verdicts("cope-b", "v4", &["h1".into()])
        .await
        .unwrap()
        .is_empty());
    // Empty inputs are no-ops.
    assert!(db.get_classifier_verdicts("cope-b", "v3", &[]).await.unwrap().is_empty());
    db.upsert_classifier_verdicts("cope-b", "v3", &[]).await.unwrap();
}
```

Run: `cargo test --features web --test unit_feed_cache`
Expected: compile error — the row types and methods do not exist.

- [ ] **Step 2: Row types + trait methods**

In `src/db/traits.rs`, after `AccessRequestRow` (line ~167), add:

```rust
/// One `account_feed_snapshots` row (#343 §4.1): an account's recent feed as
/// fetched, so the next scan that meets this account can skip Bluesky.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedSnapshot {
    pub did: String,
    pub handle: String,
    /// `serde_json` of `Vec<bluesky::posts::FeedPost>`.
    pub posts_json: String,
    /// RFC3339, like every other timestamp on this trait.
    pub fetched_at: String,
    /// "bluesky" today; "soot" once Phase 4 lands.
    pub source: String,
}

/// One `onnx_scores` row minus the key columns the caller already holds.
#[derive(Debug, Clone, PartialEq)]
pub struct OnnxScoreRow {
    /// Lowercase hex SHA-256 of the exact text the model scored.
    pub text_sha256: String,
    pub score: f64,
}

/// One `classifier_verdicts` row minus the key columns the caller holds.
#[derive(Debug, Clone, PartialEq)]
pub struct ClassifierVerdictRow {
    pub text_sha256: String,
    pub toxic_token: bool,
    pub confidence: f64,
}
```

At the end of the `Database` trait body (after the last actions/OAuth method), add:

```rust
    // --- Shared cache (#343 §4.1) ---
    //
    // None of these take a user_did: a post's toxicity is a property of the
    // post. Lookups take a batch of hashes and return only the hits, so a
    // decorator can score the misses in one pass.

    async fn get_feed_snapshot(&self, did: &str) -> Result<Option<FeedSnapshot>>;

    /// Insert or replace every column for `snapshot.did`.
    async fn upsert_feed_snapshot(&self, snapshot: &FeedSnapshot) -> Result<()>;

    /// Scores for `model_id` keyed by hash — absent hashes are simply absent.
    async fn get_onnx_scores(
        &self,
        model_id: &str,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, f64>>;

    /// Insert or replace; `scored_at` is set by the backend.
    async fn upsert_onnx_scores(&self, model_id: &str, rows: &[OnnxScoreRow]) -> Result<()>;

    async fn get_classifier_verdicts(
        &self,
        model_id: &str,
        policy_version: &str,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, ClassifierVerdictRow>>;

    /// Insert or replace; `classified_at` is set by the backend.
    async fn upsert_classifier_verdicts(
        &self,
        model_id: &str,
        policy_version: &str,
        rows: &[ClassifierVerdictRow],
    ) -> Result<()>;
```

In `src/db/mod.rs`, line 20 is `pub use traits::Database;`. Change it to:

```rust
pub use traits::{ClassifierVerdictRow, Database, FeedSnapshot, OnnxScoreRow};
```

- [ ] **Step 3: SQLite implementation**

Append to `src/db/queries.rs`:

```rust
// --- Shared cache (#343 §4.1) ---

pub fn get_feed_snapshot(conn: &Connection, did: &str) -> Result<Option<FeedSnapshot>> {
    let mut stmt = conn.prepare(
        "SELECT did, handle, posts_json, fetched_at, source
         FROM account_feed_snapshots WHERE did = ?1",
    )?;
    let row = stmt
        .query_row(params![did], |r| {
            Ok(FeedSnapshot {
                did: r.get(0)?,
                handle: r.get(1)?,
                posts_json: r.get(2)?,
                fetched_at: r.get(3)?,
                source: r.get(4)?,
            })
        })
        .optional()?;
    Ok(row)
}

pub fn upsert_feed_snapshot(conn: &Connection, s: &FeedSnapshot) -> Result<()> {
    conn.execute(
        "INSERT INTO account_feed_snapshots (did, handle, posts_json, fetched_at, source)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(did) DO UPDATE SET
             handle = excluded.handle,
             posts_json = excluded.posts_json,
             fetched_at = excluded.fetched_at,
             source = excluded.source",
        params![s.did, s.handle, s.posts_json, s.fetched_at, s.source],
    )?;
    Ok(())
}

/// One prepared statement, looped under the caller's lock. Batches are ≤ 50
/// hashes (one account's sample), so a `WHERE IN (...)` builder buys nothing.
pub fn get_onnx_scores(
    conn: &Connection,
    model_id: &str,
    hashes: &[String],
) -> Result<HashMap<String, f64>> {
    let mut out = HashMap::with_capacity(hashes.len());
    if hashes.is_empty() {
        return Ok(out);
    }
    let mut stmt =
        conn.prepare("SELECT score FROM onnx_scores WHERE text_sha256 = ?1 AND model_id = ?2")?;
    for h in hashes {
        if let Some(score) = stmt
            .query_row(params![h, model_id], |r| r.get::<_, f64>(0))
            .optional()?
        {
            out.insert(h.clone(), score);
        }
    }
    Ok(out)
}

pub fn upsert_onnx_scores(conn: &Connection, model_id: &str, rows: &[OnnxScoreRow]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now().to_rfc3339();
    let mut stmt = conn.prepare(
        "INSERT INTO onnx_scores (text_sha256, model_id, score, scored_at)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(text_sha256, model_id) DO UPDATE SET
             score = excluded.score, scored_at = excluded.scored_at",
    )?;
    for r in rows {
        stmt.execute(params![r.text_sha256, model_id, r.score, now])?;
    }
    Ok(())
}

pub fn get_classifier_verdicts(
    conn: &Connection,
    model_id: &str,
    policy_version: &str,
    hashes: &[String],
) -> Result<HashMap<String, ClassifierVerdictRow>> {
    let mut out = HashMap::with_capacity(hashes.len());
    if hashes.is_empty() {
        return Ok(out);
    }
    let mut stmt = conn.prepare(
        "SELECT toxic_token, confidence FROM classifier_verdicts
         WHERE text_sha256 = ?1 AND model_id = ?2 AND policy_version = ?3",
    )?;
    for h in hashes {
        if let Some((toxic, conf)) = stmt
            .query_row(params![h, model_id, policy_version], |r| {
                Ok((r.get::<_, bool>(0)?, r.get::<_, f64>(1)?))
            })
            .optional()?
        {
            out.insert(
                h.clone(),
                ClassifierVerdictRow { text_sha256: h.clone(), toxic_token: toxic, confidence: conf },
            );
        }
    }
    Ok(out)
}

pub fn upsert_classifier_verdicts(
    conn: &Connection,
    model_id: &str,
    policy_version: &str,
    rows: &[ClassifierVerdictRow],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let now = chrono::Utc::now().to_rfc3339();
    let mut stmt = conn.prepare(
        "INSERT INTO classifier_verdicts
             (text_sha256, model_id, policy_version, toxic_token, confidence, classified_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(text_sha256, model_id, policy_version) DO UPDATE SET
             toxic_token = excluded.toxic_token,
             confidence = excluded.confidence,
             classified_at = excluded.classified_at",
    )?;
    for r in rows {
        stmt.execute(params![
            r.text_sha256,
            model_id,
            policy_version,
            r.toxic_token,
            r.confidence,
            now
        ])?;
    }
    Ok(())
}
```

Add `use std::collections::HashMap;` and extend the `use super::traits::{…}` / models import at the top of `queries.rs` with `ClassifierVerdictRow, FeedSnapshot, OnnxScoreRow` (they live in `traits.rs`; check what `queries.rs` already imports from `super::traits`).

Append inside `impl Database for SqliteDatabase` in `src/db/sqlite.rs` (add the three row types to its `use super::traits::{…}` line):

```rust
    // --- Shared cache (#343 §4.1) ---

    async fn get_feed_snapshot(&self, did: &str) -> Result<Option<FeedSnapshot>> {
        let conn = self.conn.lock().await;
        super::queries::get_feed_snapshot(&conn, did)
    }

    async fn upsert_feed_snapshot(&self, snapshot: &FeedSnapshot) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_feed_snapshot(&conn, snapshot)
    }

    async fn get_onnx_scores(
        &self,
        model_id: &str,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, f64>> {
        let conn = self.conn.lock().await;
        super::queries::get_onnx_scores(&conn, model_id, hashes)
    }

    async fn upsert_onnx_scores(&self, model_id: &str, rows: &[OnnxScoreRow]) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_onnx_scores(&conn, model_id, rows)
    }

    async fn get_classifier_verdicts(
        &self,
        model_id: &str,
        policy_version: &str,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, ClassifierVerdictRow>> {
        let conn = self.conn.lock().await;
        super::queries::get_classifier_verdicts(&conn, model_id, policy_version, hashes)
    }

    async fn upsert_classifier_verdicts(
        &self,
        model_id: &str,
        policy_version: &str,
        rows: &[ClassifierVerdictRow],
    ) -> Result<()> {
        let conn = self.conn.lock().await;
        super::queries::upsert_classifier_verdicts(&conn, model_id, policy_version, rows)
    }
```

Run: `cargo test --features web --test unit_feed_cache` → 4 PASS. (Postgres won't compile yet with `--features postgres`; that's Step 4.)

- [ ] **Step 4: Postgres implementation**

Append inside `impl Database for PgDatabase` in `src/db/postgres.rs` (add the three row types to its `use super::traits::{…}` line). `= ANY($n)` with a `Vec<String>` bind is one round trip per lookup; writes loop inside one transaction like `record_classification_verdicts` does.

```rust
    // --- Shared cache (#343 §4.1) ---

    async fn get_feed_snapshot(&self, did: &str) -> Result<Option<FeedSnapshot>> {
        let row = sqlx_core::query::query(
            "SELECT did, handle, posts_json, fetched_at, source
             FROM account_feed_snapshots WHERE did = $1",
        )
        .bind(did)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| FeedSnapshot {
            did: r.get::<String, _>(0),
            handle: r.get::<String, _>(1),
            posts_json: r.get::<String, _>(2),
            fetched_at: r.get::<String, _>(3),
            source: r.get::<String, _>(4),
        }))
    }

    async fn upsert_feed_snapshot(&self, s: &FeedSnapshot) -> Result<()> {
        sqlx_core::query::query(
            "INSERT INTO account_feed_snapshots (did, handle, posts_json, fetched_at, source)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (did) DO UPDATE SET
                 handle = EXCLUDED.handle,
                 posts_json = EXCLUDED.posts_json,
                 fetched_at = EXCLUDED.fetched_at,
                 source = EXCLUDED.source",
        )
        .bind(&s.did)
        .bind(&s.handle)
        .bind(&s.posts_json)
        .bind(&s.fetched_at)
        .bind(&s.source)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get_onnx_scores(
        &self,
        model_id: &str,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, f64>> {
        if hashes.is_empty() {
            return Ok(Default::default());
        }
        let rows = sqlx_core::query::query(
            "SELECT text_sha256, score FROM onnx_scores
             WHERE model_id = $1 AND text_sha256 = ANY($2)",
        )
        .bind(model_id)
        .bind(hashes.to_vec())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.get::<String, _>(0), r.get::<f64, _>(1)))
            .collect())
    }

    async fn upsert_onnx_scores(&self, model_id: &str, rows: &[OnnxScoreRow]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = self.pool.begin().await?;
        for r in rows {
            sqlx_core::query::query(
                "INSERT INTO onnx_scores (text_sha256, model_id, score, scored_at)
                 VALUES ($1, $2, $3, $4)
                 ON CONFLICT (text_sha256, model_id) DO UPDATE SET
                     score = EXCLUDED.score, scored_at = EXCLUDED.scored_at",
            )
            .bind(&r.text_sha256)
            .bind(model_id)
            .bind(r.score)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn get_classifier_verdicts(
        &self,
        model_id: &str,
        policy_version: &str,
        hashes: &[String],
    ) -> Result<std::collections::HashMap<String, ClassifierVerdictRow>> {
        if hashes.is_empty() {
            return Ok(Default::default());
        }
        let rows = sqlx_core::query::query(
            "SELECT text_sha256, toxic_token, confidence FROM classifier_verdicts
             WHERE model_id = $1 AND policy_version = $2 AND text_sha256 = ANY($3)",
        )
        .bind(model_id)
        .bind(policy_version)
        .bind(hashes.to_vec())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| {
                let h = r.get::<String, _>(0);
                (
                    h.clone(),
                    ClassifierVerdictRow {
                        text_sha256: h,
                        toxic_token: r.get::<bool, _>(1),
                        confidence: r.get::<f64, _>(2),
                    },
                )
            })
            .collect())
    }

    async fn upsert_classifier_verdicts(
        &self,
        model_id: &str,
        policy_version: &str,
        rows: &[ClassifierVerdictRow],
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = self.pool.begin().await?;
        for r in rows {
            sqlx_core::query::query(
                "INSERT INTO classifier_verdicts
                     (text_sha256, model_id, policy_version, toxic_token, confidence, classified_at)
                 VALUES ($1, $2, $3, $4, $5, $6)
                 ON CONFLICT (text_sha256, model_id, policy_version) DO UPDATE SET
                     toxic_token = EXCLUDED.toxic_token,
                     confidence = EXCLUDED.confidence,
                     classified_at = EXCLUDED.classified_at",
            )
            .bind(&r.text_sha256)
            .bind(model_id)
            .bind(policy_version)
            .bind(r.toxic_token)
            .bind(r.confidence)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
```

Append to `tests/db_postgres.rs` a parity test (same assertions as the SQLite test, against `PgDatabase`; uses unique hashes so concurrent runs don't collide):

```rust
/// Parity with tests/unit_feed_cache.rs against Postgres.
#[tokio::test]
async fn test_pg_shared_cache_round_trips() {
    let Some(url) = database_url() else {
        return;
    };
    use charcoal::db::{ClassifierVerdictRow, FeedSnapshot, OnnxScoreRow};
    let db = charcoal::db::connect_postgres(&url).await.unwrap();

    let snap = FeedSnapshot {
        did: "did:plc:pgcache_snap000000000000".into(),
        handle: "cache.bsky.social".into(),
        posts_json: "[]".into(),
        fetched_at: "2026-09-08T00:00:00+00:00".into(),
        source: "bluesky".into(),
    };
    db.upsert_feed_snapshot(&snap).await.unwrap();
    let newer = FeedSnapshot { handle: "renamed.bsky.social".into(), ..snap.clone() };
    db.upsert_feed_snapshot(&newer).await.unwrap();
    assert_eq!(db.get_feed_snapshot(&snap.did).await.unwrap(), Some(newer));

    db.upsert_onnx_scores(
        "pgtest-model",
        &[
            OnnxScoreRow { text_sha256: "pgh1".into(), score: 0.1 },
            OnnxScoreRow { text_sha256: "pgh2".into(), score: 0.9 },
        ],
    )
    .await
    .unwrap();
    db.upsert_onnx_scores("pgtest-model", &[OnnxScoreRow { text_sha256: "pgh2".into(), score: 0.8 }])
        .await
        .unwrap();
    let got = db
        .get_onnx_scores("pgtest-model", &["pgh1".into(), "pgh2".into(), "nope".into()])
        .await
        .unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got["pgh2"], 0.8);
    assert!(db.get_onnx_scores("other-model", &["pgh1".into()]).await.unwrap().is_empty());
    assert!(db.get_onnx_scores("pgtest-model", &[]).await.unwrap().is_empty());

    let v = ClassifierVerdictRow { text_sha256: "pgh1".into(), toxic_token: true, confidence: 0.95 };
    db.upsert_classifier_verdicts("pgtest-clf", "v1", std::slice::from_ref(&v))
        .await
        .unwrap();
    let got = db
        .get_classifier_verdicts("pgtest-clf", "v1", &["pgh1".into(), "nope".into()])
        .await
        .unwrap();
    assert_eq!(got.get("pgh1"), Some(&v));
    assert!(db
        .get_classifier_verdicts("pgtest-clf", "v2", &["pgh1".into()])
        .await
        .unwrap()
        .is_empty());
}
```

Run: `DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres --test db_postgres -- --test-threads=1 shared_cache`
Expected: 1 PASS.

- [ ] **Step 5: Clippy (three feature sets), commit, deciduous**

```bash
cargo clippy --features web --all-targets -- -D warnings
cargo clippy --features postgres --all-targets -- -D warnings
cargo clippy --all-targets -- -D warnings
git add src/db/traits.rs src/db/queries.rs src/db/sqlite.rs src/db/postgres.rs src/db/mod.rs tests/unit_feed_cache.rs tests/db_postgres.rs
git commit -m 'feat(343): Database trait methods for the shared cache

get/upsert feed snapshots, onnx scores and classifier verdicts on SQLite
and Postgres. Batch lookups return only hits; empty inputs are no-ops.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY'
deciduous add action "T6: shared-cache Database trait methods" -c 90 --commit HEAD
deciduous link 788 <new_id> -r "Phase 1 task 6"
deciduous add outcome "T6 done: six trait methods on both backends, tests green" -c 90 --commit HEAD
deciduous link <action_id> <outcome_id> -r "result"
```

### Task 7: `CacheStats` + `CachedPostFetcher` — feed snapshots go live

**Why:** This is the read-through layer the spec calls `CachedPostFetcher` (§4.1). After this task, the second scan that meets an account fetches nothing from Bluesky for it (for 24 h), and the 25-then-50 double fetch collapses to one 50-post fetch.

**Files:**
- Create: `src/observability/cache_stats.rs`, `src/pipeline/scan_phases/feed_cache.rs`
- Modify: `src/observability/mod.rs`, `src/pipeline/scan_phases/mod.rs` (add `pub mod feed_cache;`), `src/pipeline/scan_phases/gather.rs` (delete the TEMPORARY `impl PostFetcher for AtpPostFetcher`), `src/pipeline/sweep.rs:329-351`, `src/pipeline/amplification.rs:523-549`
- Test: `tests/unit_feed_cache.rs` (append), `tests/unit_cache_stats.rs` (new)

**Interfaces:**
- Consumes: `FeedSource`/`PostFetcher`/`AtpPostFetcher` (Task 4), `FeedPost`/`sample_from_feed` (Task 4), `FeedSnapshot` + `get_feed_snapshot`/`upsert_feed_snapshot` (Task 6), `Database::set_scan_state`.
- Produces:
  ```rust
  // charcoal::observability::cache_stats
  #[derive(Debug, Default)]
  pub struct CacheStats { /* hits, misses: AtomicU64 */ }
  impl CacheStats {
      pub fn hit(&self, n: u64); pub fn miss(&self, n: u64);
      pub fn hits(&self) -> u64; pub fn misses(&self) -> u64;
  }
  /// Writes scan_state `{prefix}_cache_hits` / `{prefix}_cache_misses`.
  pub async fn record_cache_stats(db: &dyn Database, user_did: &str, prefix: &str, stats: &CacheStats) -> Result<()>;

  // charcoal::pipeline::scan_phases::feed_cache
  pub const SNAPSHOT_TTL: Duration = Duration::from_secs(24 * 60 * 60);
  pub const SNAPSHOT_FETCH_LIMIT: usize = 50;
  pub const SNAPSHOT_SOURCE_BLUESKY: &str = "bluesky";
  pub fn snapshot_is_fresh(fetched_at: &str, now: DateTime<Utc>) -> bool;
  pub struct CachedPostFetcher<'a> { /* source: &'a dyn FeedSource, db: Arc<dyn Database>, stats: Arc<CacheStats> */ }
  impl<'a> CachedPostFetcher<'a> { pub fn new(source: &'a dyn FeedSource, db: Arc<dyn Database>, stats: Arc<CacheStats>) -> Self; }
  impl PostFetcher for CachedPostFetcher<'_> { … }
  ```
  Tasks 8 and 9 reuse `CacheStats` + `record_cache_stats` with prefixes `onnx` and `classifier`; this task uses prefix `feed`.

- [ ] **Step 1: Write the failing `CacheStats` tests**

Create `tests/unit_cache_stats.rs`:

```rust
use std::sync::Arc;

use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::observability::cache_stats::{record_cache_stats, CacheStats};
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

#[test]
fn counters_start_at_zero_and_accumulate() {
    let s = CacheStats::default();
    assert_eq!((s.hits(), s.misses()), (0, 0));
    s.hit(3);
    s.miss(1);
    s.hit(2);
    assert_eq!((s.hits(), s.misses()), (5, 1));
}

#[tokio::test]
async fn record_writes_prefixed_scan_state_keys() {
    let db = setup_db();
    let s = CacheStats::default();
    s.hit(7);
    s.miss(2);
    record_cache_stats(db.as_ref(), "did:plc:u", "feed", &s).await.unwrap();
    assert_eq!(
        db.get_scan_state("did:plc:u", "feed_cache_hits").await.unwrap().as_deref(),
        Some("7")
    );
    assert_eq!(
        db.get_scan_state("did:plc:u", "feed_cache_misses").await.unwrap().as_deref(),
        Some("2")
    );
}
```

Run: `cargo test --features web --test unit_cache_stats`
Expected: compile error — module missing.

- [ ] **Step 2: Implement `CacheStats`**

Create `src/observability/cache_stats.rs`:

```rust
//! Hit/miss counters for the #343 shared caches.
//!
//! One `CacheStats` per decorator per scan. The counters are atomics so the
//! decorators can be shared across the gather's concurrent tasks without a
//! lock, and they are persisted to `scan_state` at the end of the scan so
//! the hit rate is readable from the DB (the runbook reads it there, not
//! from Railway logs).

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

use crate::db::Database;

#[derive(Debug, Default)]
pub struct CacheStats {
    hits: AtomicU64,
    misses: AtomicU64,
}

impl CacheStats {
    pub fn hit(&self, n: u64) {
        self.hits.fetch_add(n, Ordering::Relaxed);
    }

    pub fn miss(&self, n: u64) {
        self.misses.fetch_add(n, Ordering::Relaxed);
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
}

/// Persist the counters as `scan_state` rows `{prefix}_cache_hits` and
/// `{prefix}_cache_misses` for `user_did`. Prefixes in use: `feed`, `onnx`,
/// `classifier`.
pub async fn record_cache_stats(
    db: &dyn Database,
    user_did: &str,
    prefix: &str,
    stats: &CacheStats,
) -> Result<()> {
    db.set_scan_state(
        user_did,
        &format!("{prefix}_cache_hits"),
        &stats.hits().to_string(),
    )
    .await?;
    db.set_scan_state(
        user_did,
        &format!("{prefix}_cache_misses"),
        &stats.misses().to_string(),
    )
    .await?;
    Ok(())
}
```

Add `pub mod cache_stats;` to `src/observability/mod.rs` next to `pub mod classifier_metrics;`.

Run: `cargo test --features web --test unit_cache_stats` → 2 PASS.

- [ ] **Step 3: Write the failing `CachedPostFetcher` tests**

Append to `tests/unit_feed_cache.rs` (the file Task 6 created). Add these `use` lines to its header — the file already imports `HashMap`, `Arc`, `SqliteDatabase`, `Database`, `FeedSnapshot`, and `Connection` from Task 6, so do NOT repeat those (rustc rejects duplicate imports):

```rust
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use charcoal::bluesky::posts::{FeedKind, FeedPost, Post};
use charcoal::observability::cache_stats::CacheStats;
use charcoal::pipeline::scan_phases::feed_cache::{
    snapshot_is_fresh, CachedPostFetcher, SNAPSHOT_FETCH_LIMIT, SNAPSHOT_SOURCE_BLUESKY,
};
use charcoal::pipeline::scan_phases::gather::{FeedSource, PostFetcher};
use chrono::{Duration as ChronoDuration, Utc};

fn make_post(n: usize) -> Post {
    Post {
        uri: format!("at://did:plc:a/app.bsky.feed.post/{n}"),
        text: format!("post {n}"),
        created_at: None,
        like_count: 0,
        repost_count: 0,
        quote_count: 0,
        is_quote: false,
        langs: vec!["en".into()],
    }
}

/// 60 originals — more than SNAPSHOT_FETCH_LIMIT so the fetch limit is observable.
fn canned_feed() -> Vec<FeedPost> {
    (0..60)
        .map(|n| FeedPost { post: make_post(n), kind: FeedKind::Original })
        .collect()
}

struct FakeSource {
    feed: Vec<FeedPost>,
    calls: AtomicUsize,
    last_max_posts: Mutex<Option<usize>>,
}

impl FakeSource {
    fn new() -> Self {
        Self { feed: canned_feed(), calls: AtomicUsize::new(0), last_max_posts: Mutex::new(None) }
    }
}

#[async_trait]
impl FeedSource for FakeSource {
    async fn fetch_feed(&self, _handle: &str, max_posts: usize) -> anyhow::Result<Vec<FeedPost>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_max_posts.lock().unwrap() = Some(max_posts);
        Ok(self.feed.iter().take(max_posts).cloned().collect())
    }

    async fn fetch_parents(&self, _uris: &[String]) -> anyhow::Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }
}

#[tokio::test]
async fn miss_fetches_snapshot_limit_and_stores_a_snapshot() {
    let db = setup_db();
    let source = FakeSource::new();
    let stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(&db), Arc::clone(&stats));

    let sample = fetcher.fetch_sample("did:plc:a", "a.bsky.social", 25).await.unwrap();

    // The caller asked for 25 but the source was asked for the snapshot size.
    assert_eq!(sample.total_posts, 25);
    assert_eq!(*source.last_max_posts.lock().unwrap(), Some(SNAPSHOT_FETCH_LIMIT));
    assert_eq!((stats.hits(), stats.misses()), (0, 1));

    let snap = db.get_feed_snapshot("did:plc:a").await.unwrap().expect("snapshot stored");
    assert_eq!(snap.handle, "a.bsky.social");
    assert_eq!(snap.source, SNAPSHOT_SOURCE_BLUESKY);
    let stored: Vec<FeedPost> = serde_json::from_str(&snap.posts_json).unwrap();
    assert_eq!(stored.len(), SNAPSHOT_FETCH_LIMIT);
    assert!(snapshot_is_fresh(&snap.fetched_at, Utc::now()));
}

#[tokio::test]
async fn second_call_is_served_from_the_snapshot() {
    let db = setup_db();
    let source = FakeSource::new();
    let stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(&db), Arc::clone(&stats));

    let first = fetcher.fetch_sample("did:plc:a", "a.bsky.social", 25).await.unwrap();
    // The stage-2 re-fetch at 50 is the whole point: it must not hit the network.
    let second = fetcher.fetch_sample("did:plc:a", "a.bsky.social", 50).await.unwrap();

    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert_eq!(first.total_posts, 25);
    assert_eq!(second.total_posts, 50);
    assert_eq!((stats.hits(), stats.misses()), (1, 1));
}

#[tokio::test]
async fn expired_snapshot_is_refetched() {
    let db = setup_db();
    let stale = FeedSnapshot {
        did: "did:plc:a".into(),
        handle: "old.bsky.social".into(),
        posts_json: serde_json::to_string(&canned_feed()).unwrap(),
        fetched_at: (Utc::now() - ChronoDuration::hours(25)).to_rfc3339(),
        source: SNAPSHOT_SOURCE_BLUESKY.into(),
    };
    db.upsert_feed_snapshot(&stale).await.unwrap();

    let source = FakeSource::new();
    let stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(&db), Arc::clone(&stats));
    fetcher.fetch_sample("did:plc:a", "new.bsky.social", 25).await.unwrap();

    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (0, 1));
    let snap = db.get_feed_snapshot("did:plc:a").await.unwrap().unwrap();
    assert_eq!(snap.handle, "new.bsky.social");
    assert!(snapshot_is_fresh(&snap.fetched_at, Utc::now()));
}

#[tokio::test]
async fn undecodable_snapshot_is_treated_as_a_miss() {
    let db = setup_db();
    db.upsert_feed_snapshot(&FeedSnapshot {
        did: "did:plc:a".into(),
        handle: "a.bsky.social".into(),
        posts_json: "this is not json".into(),
        fetched_at: Utc::now().to_rfc3339(),
        source: SNAPSHOT_SOURCE_BLUESKY.into(),
    })
    .await
    .unwrap();

    let source = FakeSource::new();
    let stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(&db), Arc::clone(&stats));
    let sample = fetcher.fetch_sample("did:plc:a", "a.bsky.social", 25).await.unwrap();

    assert_eq!(sample.total_posts, 25);
    assert_eq!(source.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (0, 1));
    // And the bad row was repaired.
    let snap = db.get_feed_snapshot("did:plc:a").await.unwrap().unwrap();
    assert!(serde_json::from_str::<Vec<FeedPost>>(&snap.posts_json).is_ok());
}

#[test]
fn snapshot_freshness_is_a_24h_window() {
    let now = Utc::now();
    assert!(snapshot_is_fresh(&(now - ChronoDuration::hours(23)).to_rfc3339(), now));
    assert!(!snapshot_is_fresh(&(now - ChronoDuration::hours(25)).to_rfc3339(), now));
    assert!(!snapshot_is_fresh("garbage", now));
}
```

Run: `cargo test --features web --test unit_feed_cache`
Expected: compile error — `feed_cache` module missing.

- [ ] **Step 4: Implement `CachedPostFetcher`**

Create `src/pipeline/scan_phases/feed_cache.rs`:

```rust
//! Read-through feed snapshot cache (#343 §4.1).
//!
//! Wraps a raw [`FeedSource`] and exposes the [`PostFetcher`] the gather
//! consumes. On a miss it fetches [`SNAPSHOT_FETCH_LIMIT`] posts — the
//! larger of the two sample sizes the gather asks for — and stores the whole
//! feed, so the stage-2 re-fetch (and every other scan that meets this
//! account within [`SNAPSHOT_TTL`]) is served from Postgres instead of
//! Bluesky. The cache is keyed by DID, not handle: handles change.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tracing::warn;

use crate::bluesky::posts::{sample_from_feed, FeedPost, PostSample};
use crate::db::{Database, FeedSnapshot};
use crate::observability::cache_stats::CacheStats;

use super::gather::{FeedSource, PostFetcher};

/// A snapshot older than this is refetched (spec §4.1).
pub const SNAPSHOT_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Always fetch the larger sample so one miss serves both gather stages.
pub const SNAPSHOT_FETCH_LIMIT: usize = 50;

/// `account_feed_snapshots.source` for feeds read from the public AppView.
pub const SNAPSHOT_SOURCE_BLUESKY: &str = "bluesky";

/// True when `fetched_at` parses as RFC3339 and is newer than
/// `now − SNAPSHOT_TTL`. Unparseable timestamps count as stale.
pub fn snapshot_is_fresh(fetched_at: &str, now: DateTime<Utc>) -> bool {
    match DateTime::parse_from_rfc3339(fetched_at) {
        Ok(t) => {
            let ttl = chrono::Duration::from_std(SNAPSHOT_TTL).expect("24h fits chrono");
            t.with_timezone(&Utc) > now - ttl
        }
        Err(_) => false,
    }
}

pub struct CachedPostFetcher<'a> {
    source: &'a dyn FeedSource,
    db: Arc<dyn Database>,
    stats: Arc<CacheStats>,
}

impl<'a> CachedPostFetcher<'a> {
    pub fn new(source: &'a dyn FeedSource, db: Arc<dyn Database>, stats: Arc<CacheStats>) -> Self {
        Self { source, db, stats }
    }

    /// Fresh, decodable snapshot for `did`, or `None` (a stale or corrupt row
    /// is a miss — the refetch overwrites it).
    async fn fresh_feed(&self, did: &str) -> Result<Option<Vec<FeedPost>>> {
        let Some(snap) = self.db.get_feed_snapshot(did).await? else {
            return Ok(None);
        };
        if !snapshot_is_fresh(&snap.fetched_at, Utc::now()) {
            return Ok(None);
        }
        match serde_json::from_str::<Vec<FeedPost>>(&snap.posts_json) {
            Ok(feed) => Ok(Some(feed)),
            Err(e) => {
                // Never log the JSON itself — it contains post text.
                warn!(did, error = %e, "feed snapshot did not decode; refetching");
                Ok(None)
            }
        }
    }
}

#[async_trait]
impl PostFetcher for CachedPostFetcher<'_> {
    async fn fetch_sample(&self, did: &str, handle: &str, limit: usize) -> Result<PostSample> {
        if let Some(feed) = self.fresh_feed(did).await? {
            self.stats.hit(1);
            return Ok(sample_from_feed(&feed, limit));
        }

        self.stats.miss(1);
        let feed = self
            .source
            .fetch_feed(handle, limit.max(SNAPSHOT_FETCH_LIMIT))
            .await?;
        let snapshot = FeedSnapshot {
            did: did.to_string(),
            handle: handle.to_string(),
            posts_json: serde_json::to_string(&feed).context("serialising feed snapshot")?,
            fetched_at: Utc::now().to_rfc3339(),
            source: SNAPSHOT_SOURCE_BLUESKY.to_string(),
        };
        // A cache write failure is a real DB failure — surface it rather than
        // silently running uncached (the runbook's hit-rate numbers would lie).
        self.db.upsert_feed_snapshot(&snapshot).await?;
        Ok(sample_from_feed(&feed, limit))
    }

    async fn fetch_parents(&self, uris: &[String]) -> Result<HashMap<String, String>> {
        self.source.fetch_parents(uris).await
    }
}
```

Add `pub mod feed_cache;` to `src/pipeline/scan_phases/mod.rs` next to `pub mod gather;`.

Run: `cargo test --features web --test unit_feed_cache` → all PASS (4 from Task 6 + 5 new).

- [ ] **Step 5: Wire the production call sites, delete the temporary impl**

In `src/pipeline/scan_phases/gather.rs`, delete the block that begins `// TEMPORARY until Task 7 lands` through the end of that `impl PostFetcher for AtpPostFetcher<'_>` (Task 4 Step 3). `AtpPostFetcher` now implements only `FeedSource`.

In `src/pipeline/sweep.rs`, add imports:

```rust
use crate::observability::cache_stats::{record_cache_stats, CacheStats};
use crate::pipeline::scan_phases::feed_cache::CachedPostFetcher;
```

Replace line 329 `let fetcher = AtpPostFetcher { client };` with:

```rust
    let source = AtpPostFetcher { client };
    let feed_stats = Arc::new(CacheStats::default());
    let fetcher = CachedPostFetcher::new(&source, Arc::clone(db), Arc::clone(&feed_stats));
```

and after line 351 `let summary = run_phased_scan(db, user_did, candidates, &deps).await?;` add:

```rust
    record_cache_stats(db.as_ref(), user_did, "feed", &feed_stats).await?;
```

Make the identical two edits in `src/pipeline/amplification.rs` at lines 523 and 549 (same imports at the top; the code sits inside the `Some(scorer) => { … }` arm, so indent to match).

Run: `cargo build --features web` → compiles. `grep -n "impl PostFetcher for" src/` must list only `feed_cache.rs`.

- [ ] **Step 6: Full test pass, clippy, commit, deciduous**

```bash
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:|test result"
cargo clippy --features web --all-targets -- -D warnings
cargo clippy --features postgres --all-targets -- -D warnings
cargo clippy --all-targets -- -D warnings
git add src/observability/cache_stats.rs src/observability/mod.rs src/pipeline/scan_phases/feed_cache.rs src/pipeline/scan_phases/mod.rs src/pipeline/scan_phases/gather.rs src/pipeline/sweep.rs src/pipeline/amplification.rs tests/unit_feed_cache.rs tests/unit_cache_stats.rs
git commit -m 'feat(343): CachedPostFetcher — 24h feed snapshots keyed by DID

Read-through decorator over FeedSource. A miss fetches 50 posts once and
stores the feed; the stage-2 re-fetch and later scans are served from
the DB. Hit/miss counts land in scan_state feed_cache_hits/misses.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY'
deciduous add action "T7: CachedPostFetcher + CacheStats wired into sweep/amplification" -c 90 --commit HEAD
deciduous link 788 <new_id> -r "Phase 1 task 7"
deciduous add outcome "T7 done: feed snapshots live, temporary PostFetcher impl removed" -c 90 --commit HEAD
deciduous link <action_id> <outcome_id> -r "result"
```

### Task 8: `CachedToxicityScorer` — ONNX scores by text hash

**Why:** Stage-1 ONNX was 41% of the gather. A post's ONNX score never changes for a given model, so score each distinct text once, ever. The key is the SHA-256 of the exact text the model saw (raw text at stage 1, the `format_parent_reply` envelope at the clean pass) — no readable text is stored.

**Files:**
- Modify: `Cargo.toml` (add `sha2 = "0.10"` under `[dependencies]`)
- Create: `src/toxicity/cached.rs`
- Modify: `src/toxicity/mod.rs` (add `pub mod cached;`), `src/web/scan_job.rs:668` and the `finish_scan` tail from Task 2 Step 9
- Test: `tests/unit_cached_scoring.rs`

**Interfaces:**
- Consumes: `ToxicityScorer`/`ToxicityResult`/`ToxicityAttributes` (`src/toxicity/traits.rs`), `OnnxScoreRow` + `get_onnx_scores`/`upsert_onnx_scores` (Task 6), `CacheStats`/`record_cache_stats` (Task 7), `ONNX_MODEL_ID` (Task 3).
- Produces (`charcoal::toxicity::cached`):
  ```rust
  /// Lowercase hex SHA-256 of the UTF-8 bytes.
  pub fn text_sha256(text: &str) -> String;
  pub struct CachedToxicityScorer { /* inner: Box<dyn ToxicityScorer>, db: Arc<dyn Database>, model_id: &'static str, stats: Arc<CacheStats> */ }
  impl CachedToxicityScorer {
      pub fn new(inner: Box<dyn ToxicityScorer>, db: Arc<dyn Database>, model_id: &'static str, stats: Arc<CacheStats>) -> Self;
  }
  impl ToxicityScorer for CachedToxicityScorer { /* score_text, score_batch */ }
  ```
  Task 9 reuses `text_sha256`.

- [ ] **Step 1: Write the failing tests**

Create `tests/unit_cached_scoring.rs`:

```rust
//! CachedToxicityScorer (#343 §4.1): score each distinct text once per model.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::observability::cache_stats::CacheStats;
use charcoal::toxicity::cached::{text_sha256, CachedToxicityScorer};
use charcoal::toxicity::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

/// Scores `text.len() / 100` so results are distinguishable, records every
/// batch it is asked to score, and sets an attribute so cached-vs-fresh
/// results are distinguishable too.
#[derive(Default)]
struct CountingScorer {
    batches: Mutex<Vec<Vec<String>>>,
    calls: AtomicUsize,
}

#[async_trait]
impl ToxicityScorer for CountingScorer {
    async fn score_text(&self, text: &str) -> anyhow::Result<ToxicityResult> {
        Ok(self.score_batch(std::slice::from_ref(&text.to_string())).await?.remove(0))
    }

    async fn score_batch(&self, texts: &[String]) -> anyhow::Result<Vec<ToxicityResult>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.batches.lock().unwrap().push(texts.to_vec());
        Ok(texts
            .iter()
            .map(|t| ToxicityResult {
                toxicity: t.len() as f64 / 100.0,
                attributes: ToxicityAttributes { insult: Some(0.5), ..Default::default() },
            })
            .collect())
    }
}

fn cached(db: &Arc<dyn Database>, inner: Arc<CountingScorer>, model: &'static str) -> (CachedToxicityScorer, Arc<CacheStats>) {
    let stats = Arc::new(CacheStats::default());
    let scorer = CachedToxicityScorer::new(Box::new(inner), Arc::clone(db), model, Arc::clone(&stats));
    (scorer, stats)
}

fn texts(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn text_sha256_is_lowercase_hex_of_the_utf8_bytes() {
    assert_eq!(
        text_sha256("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_ne!(text_sha256("abc"), text_sha256("abc "));
}

#[tokio::test]
async fn first_batch_scores_everything_and_persists() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");

    let out = scorer.score_batch(&texts(&["a", "bb", "ccc"])).await.unwrap();

    assert_eq!(out.iter().map(|r| r.toxicity).collect::<Vec<_>>(), vec![0.01, 0.02, 0.03]);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (0, 3));
    let rows = db
        .get_onnx_scores("m", &[text_sha256("a"), text_sha256("bb"), text_sha256("ccc")])
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[&text_sha256("bb")], 0.02);
}

#[tokio::test]
async fn second_batch_scores_only_misses_and_keeps_order() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");
    scorer.score_batch(&texts(&["a", "bb", "ccc"])).await.unwrap();

    let out = scorer.score_batch(&texts(&["ccc", "dddd", "a"])).await.unwrap();

    assert_eq!(out.iter().map(|r| r.toxicity).collect::<Vec<_>>(), vec![0.03, 0.04, 0.01]);
    // Only "dddd" went to the model.
    assert_eq!(inner.batches.lock().unwrap()[1], texts(&["dddd"]));
    assert_eq!((stats.hits(), stats.misses()), (2, 4));
    // Hits carry no attribute breakdown (only the score is cached); the
    // fresh one keeps the model's attributes.
    assert_eq!(out[0].attributes.insult, None);
    assert_eq!(out[1].attributes.insult, Some(0.5));
}

#[tokio::test]
async fn all_hits_never_calls_the_model() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");
    scorer.score_batch(&texts(&["a", "bb"])).await.unwrap();

    let out = scorer.score_batch(&texts(&["bb", "a"])).await.unwrap();

    assert_eq!(out.iter().map(|r| r.toxicity).collect::<Vec<_>>(), vec![0.02, 0.01]);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (2, 2));
}

#[tokio::test]
async fn duplicate_texts_in_one_batch_are_scored_once() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");

    let out = scorer.score_batch(&texts(&["a", "a", "bb", "a"])).await.unwrap();

    assert_eq!(out.iter().map(|r| r.toxicity).collect::<Vec<_>>(), vec![0.01, 0.01, 0.02, 0.01]);
    assert_eq!(inner.batches.lock().unwrap()[0], texts(&["a", "bb"]));
    assert_eq!((stats.hits(), stats.misses()), (0, 4));
}

#[tokio::test]
async fn score_text_uses_the_cache() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");

    let first = scorer.score_text("hello").await.unwrap();
    let second = scorer.score_text("hello").await.unwrap();

    assert_eq!(first.toxicity, second.toxicity);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (1, 1));
}

#[tokio::test]
async fn empty_batch_is_a_no_op() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer, stats) = cached(&db, Arc::clone(&inner), "m");
    assert!(scorer.score_batch(&[]).await.unwrap().is_empty());
    assert_eq!(inner.calls.load(Ordering::SeqCst), 0);
    assert_eq!((stats.hits(), stats.misses()), (0, 0));
}

#[tokio::test]
async fn cache_is_scoped_by_model_id() {
    let db = setup_db();
    let inner = Arc::new(CountingScorer::default());
    let (scorer_a, _) = cached(&db, Arc::clone(&inner), "model-a");
    let (scorer_b, stats_b) = cached(&db, Arc::clone(&inner), "model-b");

    scorer_a.score_batch(&texts(&["a"])).await.unwrap();
    scorer_b.score_batch(&texts(&["a"])).await.unwrap();

    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    assert_eq!((stats_b.hits(), stats_b.misses()), (0, 1));
}
```

Run: `cargo test --features web --test unit_cached_scoring`
Expected: compile error — `charcoal::toxicity::cached` missing.

- [ ] **Step 2: Add the dependency and implement**

In `Cargo.toml` `[dependencies]`, add (alphabetically among the others):

```toml
sha2 = "0.10"
```

Create `src/toxicity/cached.rs`:

```rust
//! Read-through ONNX score cache (#343 §4.1).
//!
//! A text's toxicity score is a property of (text, model), not of the scan
//! that met it, so it is scored once and shared by every protected user. The
//! key is the SHA-256 of the exact string the model scored — never the text
//! itself — so the table holds no readable content. Only the headline
//! `toxicity` is cached: it is the only number the two-stage scorer reads
//! from stage 1, and the attribute breakdown would multiply the row size
//! for nothing.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;
use sha2::{Digest, Sha256};

use crate::db::{Database, OnnxScoreRow};
use crate::observability::cache_stats::CacheStats;

use super::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

/// Lowercase hex SHA-256 of `text`'s UTF-8 bytes — the cache key for both
/// `onnx_scores` and `classifier_verdicts`.
pub fn text_sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

pub struct CachedToxicityScorer {
    inner: Box<dyn ToxicityScorer>,
    db: Arc<dyn Database>,
    model_id: &'static str,
    stats: Arc<CacheStats>,
}

impl CachedToxicityScorer {
    pub fn new(
        inner: Box<dyn ToxicityScorer>,
        db: Arc<dyn Database>,
        model_id: &'static str,
        stats: Arc<CacheStats>,
    ) -> Self {
        Self { inner, db, model_id, stats }
    }
}

#[async_trait]
impl ToxicityScorer for CachedToxicityScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        let mut out = self.score_batch(std::slice::from_ref(&text.to_string())).await?;
        match out.pop() {
            Some(r) => Ok(r),
            None => bail!("score_batch returned no result for a single text"),
        }
    }

    /// Look every hash up in one query, send only the distinct misses to the
    /// inner scorer in one batch, persist them, and reassemble in input order.
    async fn score_batch(&self, texts: &[String]) -> Result<Vec<ToxicityResult>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let hashes: Vec<String> = texts.iter().map(|t| text_sha256(t)).collect();
        let cached = self.db.get_onnx_scores(self.model_id, &hashes).await?;

        // Distinct misses, in first-seen order. `miss_slot[hash]` is the index
        // into `miss_texts`/`fresh` so duplicates within a batch score once.
        let mut miss_slot: HashMap<&str, usize> = HashMap::new();
        let mut miss_texts: Vec<String> = Vec::new();
        for (text, hash) in texts.iter().zip(&hashes) {
            if !cached.contains_key(hash) && !miss_slot.contains_key(hash.as_str()) {
                miss_slot.insert(hash.as_str(), miss_texts.len());
                miss_texts.push(text.clone());
            }
        }

        let fresh = if miss_texts.is_empty() {
            Vec::new()
        } else {
            self.inner.score_batch(&miss_texts).await?
        };
        if fresh.len() != miss_texts.len() {
            bail!(
                "inner scorer returned {} results for {} texts",
                fresh.len(),
                miss_texts.len()
            );
        }

        let rows: Vec<OnnxScoreRow> = miss_slot
            .iter()
            .map(|(hash, &slot)| OnnxScoreRow {
                text_sha256: (*hash).to_string(),
                score: fresh[slot].toxicity,
            })
            .collect();
        self.db.upsert_onnx_scores(self.model_id, &rows).await?;

        let mut hits = 0u64;
        let mut out = Vec::with_capacity(texts.len());
        for hash in &hashes {
            if let Some(&score) = cached.get(hash) {
                hits += 1;
                out.push(ToxicityResult {
                    toxicity: score,
                    attributes: ToxicityAttributes::default(),
                });
            } else {
                out.push(fresh[miss_slot[hash.as_str()]].clone());
            }
        }
        self.stats.hit(hits);
        self.stats.miss(texts.len() as u64 - hits);
        Ok(out)
    }
}
```

Add `pub mod cached;` to `src/toxicity/mod.rs` (alphabetically, after `pub mod classifier;`).

Run: `cargo test --features web --test unit_cached_scoring` → 8 PASS.

- [ ] **Step 3: Wire it into the scan**

In `src/web/scan_job.rs`, replace line 668:

```rust
    let primary_scorer: Box<dyn ToxicityScorer> = Box::new(Arc::clone(&models.toxicity));
```

with:

```rust
    // #343 Phase 1: stage-1 / clean-pass ONNX scores are cached by text hash
    // across users. Hit/miss counts are persisted at the end of the scan.
    let onnx_cache_stats = Arc::new(crate::observability::cache_stats::CacheStats::default());
    let primary_scorer: Box<dyn ToxicityScorer> =
        Box::new(crate::toxicity::cached::CachedToxicityScorer::new(
            Box::new(Arc::clone(&models.toxicity)),
            Arc::clone(&db),
            crate::toxicity::onnx::ONNX_MODEL_ID,
            Arc::clone(&onnx_cache_stats),
        ));
```

In the tail of `run_scan` (Task 2 Step 9 added the `bluesky_ratelimit_limit` block before `finish_scan`), add directly after that block:

```rust
    if let Err(e) = crate::observability::cache_stats::record_cache_stats(
        db.as_ref(),
        user_did,
        "onnx",
        &onnx_cache_stats,
    )
    .await
    {
        tracing::warn!(error = %e, "could not record onnx cache stats");
    }
```

Run: `cargo build --features web` → compiles.

- [ ] **Step 4: Full test pass, clippy, commit, deciduous**

```bash
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:|test result"
cargo clippy --features web --all-targets -- -D warnings
cargo clippy --features postgres --all-targets -- -D warnings
cargo clippy --all-targets -- -D warnings
git add Cargo.toml Cargo.lock src/toxicity/cached.rs src/toxicity/mod.rs src/web/scan_job.rs tests/unit_cached_scoring.rs
git commit -m 'feat(343): CachedToxicityScorer — ONNX scores cached by text SHA-256

Distinct misses go to the model in one batch; hits return the cached
toxicity with default attributes. Keyed by (text hash, model id), shared
across users. Counts land in scan_state onnx_cache_hits/misses.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY'
deciduous add action "T8: CachedToxicityScorer wired as primary scorer" -c 90 --commit HEAD
deciduous link 788 <new_id> -r "Phase 1 task 8"
deciduous add outcome "T8 done: ONNX score cache live" -c 90 --commit HEAD
deciduous link <action_id> <outcome_id> -r "result"
```

### Task 9: `CachedClassifier` — stage-2 verdicts by text hash

**Why:** Stage-2 (RunPod CoPE-B) is the paid, remote step. A verdict for a given (text, model, policy) is final until the policy changes, so it is cached with `policy_version` in the key — bumping the policy invalidates without a migration.

**Files:**
- Create: `src/toxicity/cached_classifier.rs`
- Modify: `src/toxicity/mod.rs` (add `pub mod cached_classifier;`), `src/web/scan_job.rs:676` and the `run_scan` tail
- Test: `tests/unit_cached_classifier.rs`

**Interfaces:**
- Consumes: `ToxicityClassifier`/`ClassifierVerdict`/`ItemOutcome` (`src/toxicity/classifier.rs`), `ClassifierVerdictRow` + `get_classifier_verdicts`/`upsert_classifier_verdicts` (Task 6), `text_sha256` (Task 8), `CacheStats`/`record_cache_stats` (Task 7).
- Produces (`charcoal::toxicity::cached_classifier`):
  ```rust
  pub struct CachedClassifier { /* inner: Arc<dyn ToxicityClassifier>, db: Arc<dyn Database>, stats: Arc<CacheStats> */ }
  impl CachedClassifier { pub fn new(inner: Arc<dyn ToxicityClassifier>, db: Arc<dyn Database>, stats: Arc<CacheStats>) -> Self; }
  impl ToxicityClassifier for CachedClassifier { /* classify, classify_batch, max_batch_size, name, model_id, policy_version, threshold all forward/cached */ }
  ```
  Cache policy: only `ItemOutcome::Verdict` is stored; `ItemOutcome::Error` slots and request-level `Err`s are passed through untouched (so `CostCeilingExceeded` / `ClassifierTransientError` still downcast in the burst). A hit is rebuilt with `latency_ms: 0` and the inner's current `model_id()`/`policy_version()`.

- [ ] **Step 1: Write the failing tests**

Create `tests/unit_cached_classifier.rs`:

```rust
//! CachedClassifier (#343 §4.1): one stage-2 verdict per (text, model, policy).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use charcoal::db::sqlite::SqliteDatabase;
use charcoal::db::Database;
use charcoal::observability::cache_stats::CacheStats;
use charcoal::toxicity::cached::text_sha256;
use charcoal::toxicity::cached_classifier::CachedClassifier;
use charcoal::toxicity::classifier::{ClassifierVerdict, ItemOutcome, ToxicityClassifier};
use rusqlite::Connection;

fn setup_db() -> Arc<dyn Database> {
    let conn = Connection::open_in_memory().unwrap();
    charcoal::db::schema::create_tables(&conn).unwrap();
    Arc::new(SqliteDatabase::new(conn))
}

/// Toxic iff the text contains "bad"; texts containing "undecodable" come back
/// as `ItemOutcome::Error`; texts containing "boom" fail the whole request.
struct FakeClassifier {
    policy: &'static str,
    batches: Mutex<Vec<Vec<String>>>,
    calls: AtomicUsize,
}

impl FakeClassifier {
    fn new(policy: &'static str) -> Self {
        Self { policy, batches: Mutex::new(Vec::new()), calls: AtomicUsize::new(0) }
    }

    fn verdict(&self, toxic: bool) -> ClassifierVerdict {
        ClassifierVerdict {
            toxic_token: toxic,
            confidence: if toxic { 0.9 } else { 0.2 },
            latency_ms: 42,
            model_id: self.model_id().to_string(),
            policy_version: self.policy_version().to_string(),
        }
    }
}

#[async_trait]
impl ToxicityClassifier for FakeClassifier {
    async fn classify(&self, content: &str) -> anyhow::Result<ClassifierVerdict> {
        match self.classify_batch(std::slice::from_ref(&content.to_string())).await?.remove(0) {
            ItemOutcome::Verdict(v) => Ok(v),
            ItemOutcome::Error(e) => anyhow::bail!("{e}"),
        }
    }

    async fn classify_batch(&self, contents: &[String]) -> anyhow::Result<Vec<ItemOutcome>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.batches.lock().unwrap().push(contents.to_vec());
        if contents.iter().any(|c| c.contains("boom")) {
            anyhow::bail!("request failed");
        }
        Ok(contents
            .iter()
            .map(|c| {
                if c.contains("undecodable") {
                    ItemOutcome::Error("slot did not decode".into())
                } else {
                    ItemOutcome::Verdict(self.verdict(c.contains("bad")))
                }
            })
            .collect())
    }

    fn max_batch_size(&self) -> usize {
        16
    }
    fn name(&self) -> &'static str {
        "fake"
    }
    fn model_id(&self) -> &'static str {
        "fake-model"
    }
    fn policy_version(&self) -> &'static str {
        self.policy
    }
    fn threshold(&self) -> f32 {
        0.5
    }
}

fn cached(db: &Arc<dyn Database>, inner: Arc<FakeClassifier>) -> (CachedClassifier, Arc<CacheStats>) {
    let stats = Arc::new(CacheStats::default());
    let c = CachedClassifier::new(inner, Arc::clone(db), Arc::clone(&stats));
    (c, stats)
}

fn texts(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn toxic_flags(out: &[ItemOutcome]) -> Vec<Option<bool>> {
    out.iter()
        .map(|o| match o {
            ItemOutcome::Verdict(v) => Some(v.toxic_token),
            ItemOutcome::Error(_) => None,
        })
        .collect()
}

#[tokio::test]
async fn first_batch_forwards_everything_and_persists_verdicts() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, stats) = cached(&db, Arc::clone(&inner));

    let out = c.classify_batch(&texts(&["fine", "so bad", "undecodable"])).await.unwrap();

    assert_eq!(toxic_flags(&out), vec![Some(false), Some(true), None]);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (0, 3));
    let rows = db
        .get_classifier_verdicts(
            "fake-model",
            "v1",
            &[text_sha256("fine"), text_sha256("so bad"), text_sha256("undecodable")],
        )
        .await
        .unwrap();
    // Only decodable verdicts are cached.
    assert_eq!(rows.len(), 2);
    assert!(rows[&text_sha256("so bad")].toxic_token);
    assert!((rows[&text_sha256("so bad")].confidence - 0.9).abs() < 1e-6);
}

#[tokio::test]
async fn hits_skip_the_backend_and_keep_order() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, stats) = cached(&db, Arc::clone(&inner));
    c.classify_batch(&texts(&["fine", "so bad"])).await.unwrap();

    let out = c.classify_batch(&texts(&["so bad", "new text", "fine"])).await.unwrap();

    assert_eq!(toxic_flags(&out), vec![Some(true), Some(false), Some(false)]);
    assert_eq!(inner.batches.lock().unwrap()[1], texts(&["new text"]));
    assert_eq!((stats.hits(), stats.misses()), (2, 3));
    let ItemOutcome::Verdict(hit) = &out[0] else { panic!("expected verdict") };
    assert_eq!(hit.latency_ms, 0);
    assert_eq!(hit.model_id, "fake-model");
    assert_eq!(hit.policy_version, "v1");
    assert!((hit.confidence - 0.9).abs() < 1e-6);
}

#[tokio::test]
async fn all_hits_never_calls_the_backend() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, _) = cached(&db, Arc::clone(&inner));
    c.classify_batch(&texts(&["a", "b"])).await.unwrap();
    c.classify_batch(&texts(&["b", "a"])).await.unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn policy_bump_invalidates() {
    let db = setup_db();
    let (c1, _) = cached(&db, Arc::new(FakeClassifier::new("v1")));
    c1.classify_batch(&texts(&["fine"])).await.unwrap();

    let inner2 = Arc::new(FakeClassifier::new("v2"));
    let (c2, stats2) = cached(&db, Arc::clone(&inner2));
    c2.classify_batch(&texts(&["fine"])).await.unwrap();

    assert_eq!(inner2.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats2.hits(), stats2.misses()), (0, 1));
}

#[tokio::test]
async fn request_level_error_passes_through_and_caches_nothing() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, stats) = cached(&db, Arc::clone(&inner));

    let err = c.classify_batch(&texts(&["fine", "boom"])).await.unwrap_err();

    assert!(err.to_string().contains("request failed"));
    assert_eq!((stats.hits(), stats.misses()), (0, 2));
    assert!(db
        .get_classifier_verdicts("fake-model", "v1", &[text_sha256("fine")])
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn single_classify_uses_the_cache() {
    let db = setup_db();
    let inner = Arc::new(FakeClassifier::new("v1"));
    let (c, stats) = cached(&db, Arc::clone(&inner));

    let a = c.classify("so bad").await.unwrap();
    let b = c.classify("so bad").await.unwrap();

    assert_eq!((a.toxic_token, b.toxic_token), (true, true));
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!((stats.hits(), stats.misses()), (1, 1));
}

#[tokio::test]
async fn metadata_forwards_to_the_inner_classifier() {
    let db = setup_db();
    let (c, _) = cached(&db, Arc::new(FakeClassifier::new("v1")));
    assert_eq!(c.max_batch_size(), 16);
    assert_eq!(c.name(), "fake");
    assert_eq!(c.model_id(), "fake-model");
    assert_eq!(c.policy_version(), "v1");
    assert_eq!(c.threshold(), 0.5);
    assert!(c.classify_batch(&[]).await.unwrap().is_empty());
}
```

Run: `cargo test --features web --test unit_cached_classifier`
Expected: compile error — module missing.

- [ ] **Step 2: Implement `CachedClassifier`**

Create `src/toxicity/cached_classifier.rs`:

```rust
//! Read-through stage-2 verdict cache (#343 §4.1).
//!
//! Wraps the configured `ToxicityClassifier` (RunPod CoPE-B in production).
//! Keyed by (text hash, model id, policy version) so a policy bump simply
//! stops matching old rows. Only decodable verdicts are stored — an
//! `ItemOutcome::Error` slot or a request-level `Err` is handed back exactly
//! as the inner classifier produced it, so the burst phase's downcasts on
//! `CostCeilingExceeded` / `ClassifierTransientError` keep working.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{bail, Result};
use async_trait::async_trait;

use crate::db::{ClassifierVerdictRow, Database};
use crate::observability::cache_stats::CacheStats;

use super::cached::text_sha256;
use super::classifier::{ClassifierVerdict, ItemOutcome, ToxicityClassifier};

pub struct CachedClassifier {
    inner: Arc<dyn ToxicityClassifier>,
    db: Arc<dyn Database>,
    stats: Arc<CacheStats>,
}

impl CachedClassifier {
    pub fn new(inner: Arc<dyn ToxicityClassifier>, db: Arc<dyn Database>, stats: Arc<CacheStats>) -> Self {
        Self { inner, db, stats }
    }

    /// A cached row rebuilt as a verdict. `latency_ms` is 0: nothing was
    /// called. Model/policy are the inner's current values — by construction
    /// the row was looked up under exactly those.
    fn verdict_from_row(&self, row: &ClassifierVerdictRow) -> ClassifierVerdict {
        ClassifierVerdict {
            toxic_token: row.toxic_token,
            confidence: row.confidence as f32,
            latency_ms: 0,
            model_id: self.inner.model_id().to_string(),
            policy_version: self.inner.policy_version().to_string(),
        }
    }
}

#[async_trait]
impl ToxicityClassifier for CachedClassifier {
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
        let mut out = self.classify_batch(std::slice::from_ref(&content.to_string())).await?;
        match out.pop() {
            Some(ItemOutcome::Verdict(v)) => Ok(v),
            Some(ItemOutcome::Error(e)) => bail!("classifier slot did not decode: {e}"),
            None => bail!("classify_batch returned no result for a single text"),
        }
    }

    async fn classify_batch(&self, contents: &[String]) -> Result<Vec<ItemOutcome>> {
        if contents.is_empty() {
            return Ok(Vec::new());
        }
        let model_id = self.inner.model_id();
        let policy_version = self.inner.policy_version();
        let hashes: Vec<String> = contents.iter().map(|c| text_sha256(c)).collect();
        let cached = self
            .db
            .get_classifier_verdicts(model_id, policy_version, &hashes)
            .await?;

        // Distinct misses in first-seen order (duplicates classify once).
        let mut miss_slot: HashMap<&str, usize> = HashMap::new();
        let mut miss_texts: Vec<String> = Vec::new();
        for (content, hash) in contents.iter().zip(&hashes) {
            if !cached.contains_key(hash) && !miss_slot.contains_key(hash.as_str()) {
                miss_slot.insert(hash.as_str(), miss_texts.len());
                miss_texts.push(content.clone());
            }
        }

        let hits = hashes.iter().filter(|h| cached.contains_key(*h)).count() as u64;
        self.stats.hit(hits);
        self.stats.miss(contents.len() as u64 - hits);

        // Request-level errors propagate untouched (see module docs).
        let fresh = if miss_texts.is_empty() {
            Vec::new()
        } else {
            self.inner.classify_batch(&miss_texts).await?
        };
        if fresh.len() != miss_texts.len() {
            bail!(
                "inner classifier returned {} outcomes for {} texts",
                fresh.len(),
                miss_texts.len()
            );
        }

        let rows: Vec<ClassifierVerdictRow> = miss_slot
            .iter()
            .filter_map(|(hash, &slot)| match &fresh[slot] {
                ItemOutcome::Verdict(v) => Some(ClassifierVerdictRow {
                    text_sha256: (*hash).to_string(),
                    toxic_token: v.toxic_token,
                    confidence: f64::from(v.confidence),
                }),
                ItemOutcome::Error(_) => None,
            })
            .collect();
        self.db
            .upsert_classifier_verdicts(model_id, policy_version, &rows)
            .await?;

        let out = hashes
            .iter()
            .map(|hash| match cached.get(hash) {
                Some(row) => ItemOutcome::Verdict(self.verdict_from_row(row)),
                None => fresh[miss_slot[hash.as_str()]].clone(),
            })
            .collect();
        Ok(out)
    }

    fn max_batch_size(&self) -> usize {
        self.inner.max_batch_size()
    }
    fn name(&self) -> &'static str {
        self.inner.name()
    }
    fn model_id(&self) -> &'static str {
        self.inner.model_id()
    }
    fn policy_version(&self) -> &'static str {
        self.inner.policy_version()
    }
    fn threshold(&self) -> f32 {
        self.inner.threshold()
    }
}
```

Add `pub mod cached_classifier;` to `src/toxicity/mod.rs` after `pub mod cached;`.

Run: `cargo test --features web --test unit_cached_classifier` → 7 PASS.

- [ ] **Step 3: Wire it into the scan**

In `src/web/scan_job.rs`, replace line 676:

```rust
    let classifier = crate::toxicity::classifier::build_from_env()?;
```

with:

```rust
    // #343 Phase 1: stage-2 verdicts are cached by (text hash, model, policy).
    let classifier_cache_stats =
        Arc::new(crate::observability::cache_stats::CacheStats::default());
    let classifier: Arc<dyn crate::toxicity::classifier::ToxicityClassifier> =
        Arc::new(crate::toxicity::cached_classifier::CachedClassifier::new(
            crate::toxicity::classifier::build_from_env()?,
            Arc::clone(&db),
            Arc::clone(&classifier_cache_stats),
        ));
```

The `info!(backend = classifier.name(), …)` and `record_backend_selected(classifier.name())` lines that follow keep working — `name()` forwards to the inner backend.

In the `run_scan` tail, directly after the `onnx` block Task 8 added:

```rust
    if let Err(e) = crate::observability::cache_stats::record_cache_stats(
        db.as_ref(),
        user_did,
        "classifier",
        &classifier_cache_stats,
    )
    .await
    {
        tracing::warn!(error = %e, "could not record classifier cache stats");
    }
```

Run: `cargo build --features web` → compiles.

- [ ] **Step 4: Full test pass, clippy, commit, deciduous**

```bash
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:|test result"
cargo clippy --features web --all-targets -- -D warnings
cargo clippy --features postgres --all-targets -- -D warnings
cargo clippy --all-targets -- -D warnings
git add src/toxicity/cached_classifier.rs src/toxicity/mod.rs src/web/scan_job.rs tests/unit_cached_classifier.rs
git commit -m 'feat(343): CachedClassifier — stage-2 verdicts cached by (hash, model, policy)

Only decodable verdicts are stored; slot errors and request errors pass
through untouched so the burst downcasts still work. Counts land in
scan_state classifier_cache_hits/misses.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY'
deciduous add action "T9: CachedClassifier wired around build_from_env" -c 90 --commit HEAD
deciduous link 788 <new_id> -r "Phase 1 task 9"
deciduous add outcome "T9 done: classifier verdict cache live" -c 90 --commit HEAD
deciduous link <action_id> <outcome_id> -r "result"
```

### Task 10: Docs — CHANGELOG, README knobs, two runbooks

**Why:** Spec §6 says the Phase 0 experiment and the Phase 1 pass test are manual runbooks, and §7 says they read from the DB, not Railway logs. Nothing here is code; it is a docs-only commit (the pre-commit hook skips the cargo gates for it).

**Files:**
- Modify: `CHANGELOG.md` (under `## [Unreleased]`, line 7 — add an `### Added` block above the existing `### Fixed`), `README.md:48-54` (optional settings list)
- Create: `docs/runbooks/343-phase0-session-experiment.md`, `docs/runbooks/343-phase1-hit-rate.md`

- [ ] **Step 1: CHANGELOG**

Insert after line 7 (`## [Unreleased]`) and its blank line, before `### Fixed`:

```markdown
### Added
- #343 Phase 0 + Phase 1 — measurement hooks and the shared cache. The
  gather now logs `cpu_cores_busy` once a minute and records the observed
  Bluesky `RateLimit-Limit` to `scan_state` once per scan, so the first
  real numbers on inference headroom and the API ceiling come from the DB
  rather than guesses. `CHARCOAL_ONNX_SESSIONS` (default 1, clamp 1–8)
  builds a round-robin pool of ONNX sessions for the session experiment.
  Three new user-independent tables (schema v16) cache what never changes
  between users: an account's recent feed for 24 h keyed by DID, stage-1
  ONNX scores keyed by the SHA-256 of the exact text scored, and stage-2
  verdicts keyed by (text hash, model, policy version). No readable post
  text is stored in the score tables, and `delete_user_data` leaves the
  cache alone because none of it belongs to a user. The two-stage scorer
  also gained a `score_batch` override — stage 1 was doing 25 single
  forward passes per account. Hit/miss counts land in `scan_state` as
  `{feed,onnx,classifier}_cache_{hits,misses}`.

```

- [ ] **Step 2: README knobs**

In `README.md`, after the `- \`RUST_LOG\` — …` line (line 54), add:

```markdown
- `CHARCOAL_ONNX_SESSIONS` — number of ONNX toxicity sessions to pool
  (default `1`, clamped to 1–8; each is a separate ~126 MB model load).
  Only worth raising if the #343 Phase 0 runbook shows a gain.
```

- [ ] **Step 3: Phase 0 runbook**

Create `docs/runbooks/343-phase0-session-experiment.md`:

```markdown
# #343 Phase 0 — ONNX session experiment (staging)

**Question:** does a pool of N ONNX sessions cut gather wall time by ≥ 25 %
on the same candidate set, or does the single mutexed session stay and the
CPU headroom go to more concurrent scans instead? (Spec §6, Phase 0 item 3.)

**Read this first:** the Phase 1 caches ship in the same PR. A second scan
of the same account is served from `account_feed_snapshots` and
`onnx_scores`, which makes its gather wall meaningless for this experiment.
Every run below starts from a **cold cache** — truncate first.

## Setup

1. Pick one staging account with ~200 candidates (the 2026-09-07 baseline
   account had 934; a smaller one keeps each run under ~4 min). Record its
   DID as `$DID`.
2. Staging Postgres shell (the web service's `DATABASE_URL` is
   `postgres.railway.internal`, unreachable locally — go through the
   Postgres service):

   ```
   railway run -s Postgres -e staging -- sh -c 'psql "$DATABASE_PUBLIC_URL"'
   ```

   Never paste the URL into a transcript; it carries credentials.

## One run

1. Cold the cache (staging only — this is destructive by design):

   ```sql
   TRUNCATE account_feed_snapshots, onnx_scores, classifier_verdicts;
   ```

2. Set the knob on the staging web service and wait for the redeploy:

   ```
   railway variables --set CHARCOAL_ONNX_SESSIONS=<N> -s charcoal-web -e staging
   ```

3. Trigger a scan for `$DID` from the dashboard and wait for
   `scan_queue.status = 'done'`.
4. Record, from the DB:

   ```sql
   SELECT finished_at::timestamptz - started_at::timestamptz AS wall
     FROM scan_queue WHERE user_did = '<DID>' ORDER BY enqueued_at DESC LIMIT 1;
   SELECT key, value FROM scan_state
    WHERE user_did = '<DID>' AND key IN ('bluesky_ratelimit_limit', 'candidates_total');
   ```

5. Record, from the logs (these two are logged by design — spec §6 items
   1 and 2 say "log it"). Railway MCP `get_logs` with `search` alone (the
   `since` + `deployment_id` combination returns nothing):
   - search `Phase A timing split` → `total_ms`, `fetch_ms`,
     `stage1_onnx_ms`, `inference_pct` — **`total_ms` is the gather wall.**
   - search `cpu_cores_busy` → one sample per minute; note the median.

## Runs

| N | gather `total_ms` | `stage1_onnx_ms` | median `cpu_cores_busy` | RSS (Railway metrics) |
|---|---|---|---|---|
| 1 | | | | |
| 4 | | | | |
| 1 (repeat) | | | | |

Run N=1 twice so run-to-run noise is visible; Bluesky latency varies.

## Decision

- N=4 `total_ms` ≤ 0.75 × mean of the two N=1 runs → keep the pool, set
  `CHARCOAL_ONNX_SESSIONS=4` on staging, record the numbers in spec §1.
- Otherwise → leave the default at 1, record the numbers, and note in the
  spec that CPU headroom goes to scan concurrency (Phase 3).

Either way, file the result as a deciduous outcome under node 788 and put
the table above in the PR body.
```

- [ ] **Step 4: Phase 1 runbook**

Create `docs/runbooks/343-phase1-hit-rate.md`:

```markdown
# #343 Phase 1 — shared-cache pass test (staging)

**Pass (spec §6, Phase 1):** a second staging account whose community
overlaps the first shows a feed-snapshot hit rate ≥ 50 % **and** a scan
wall ≤ 60 % of the cold baseline (13 m 42 s → ≤ 8 m). Hit rate < 20 % →
re-cost before Phase 2.

Everything below is read from the DB. Railway logs are not part of the
pass/fail.

## Accounts

- **A** — the baseline account (the one scanned on 2026-09-07).
- **B** — an account that follows / is followed by much of A's community.
  Both must be on the staging allowlist.

## Steps

1. Cold the cache once so A's run is a true baseline:

   ```sql
   TRUNCATE account_feed_snapshots, onnx_scores, classifier_verdicts;
   ```

2. Scan **A**. Wait for `scan_queue.status = 'done'`. Record its wall and
   its cache counters — expect misses only:

   ```sql
   SELECT finished_at::timestamptz - started_at::timestamptz AS wall
     FROM scan_queue WHERE user_did = '<A>' ORDER BY enqueued_at DESC LIMIT 1;
   SELECT key, value FROM scan_state
    WHERE user_did = '<A>' AND key LIKE '%_cache_%' ORDER BY key;
   ```

3. Scan **B**. Same two queries with `<B>`.
4. Compute for B:

   - feed hit rate = `feed_cache_hits / (feed_cache_hits + feed_cache_misses)`
   - onnx hit rate = `onnx_cache_hits / (onnx_cache_hits + onnx_cache_misses)`
   - classifier hit rate = same shape
   - wall ratio = B's wall ÷ A's wall

5. Sanity-check the tables grew and hold no readable text:

   ```sql
   SELECT count(*) FROM account_feed_snapshots;
   SELECT count(*) FROM onnx_scores;
   SELECT count(*) FROM classifier_verdicts;
   SELECT text_sha256 FROM onnx_scores LIMIT 3;   -- 64 hex chars, nothing else
   ```

## Result

| | wall | feed hit rate | onnx hit rate | classifier hit rate |
|---|---|---|---|---|
| A (cold) | | 0 | 0 | 0 |
| B (warm) | | | | |

**Pass:** feed hit rate ≥ 0.50 and B's wall ≤ 0.60 × A's wall.
**Re-cost:** feed hit rate < 0.20 — write the number into spec §4.1 and
stop before planning Phase 2.

Record the table as a deciduous outcome under node 788 and in the PR body.
Also note `bluesky_ratelimit_limit` from either account's `scan_state` in
spec §4.3 — Phase 3 needs it.
```

- [ ] **Step 5: Commit, deciduous**

```bash
git add CHANGELOG.md README.md docs/runbooks/343-phase0-session-experiment.md docs/runbooks/343-phase1-hit-rate.md
git commit -m 'docs(343): CHANGELOG, CHARCOAL_ONNX_SESSIONS in README, Phase 0/1 runbooks

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01UnZQUWhbcAg4gMZpmSrYqY'
deciduous add action "T10: docs + runbooks for Phase 0 experiment and Phase 1 pass test" -c 90 --commit HEAD
deciduous link 788 <new_id> -r "Phase 0+1 task 10"
deciduous add outcome "T10 done: docs committed; branch ready for PR to staging" -c 90 --commit HEAD
deciduous link <action_id> <outcome_id> -r "result"
```

---

## After the last task

1. Full gates one more time from a clean state:
   ```bash
   cargo fmt --all -- --check
   cargo clippy --features web --all-targets -- -D warnings
   cargo clippy --features postgres --all-targets -- -D warnings
   cargo clippy --all-targets -- -D warnings
   CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "^\s*SKIP:|test result"
   DATABASE_URL=postgres://$USER@localhost/charcoal_test cargo test --all-targets --features postgres -- --test-threads=1 2>&1 | grep "test result"
   ```
   Zero `SKIP:` lines; every `test result` line `0 failed`.
2. Push `feat/343-phase1-cache` (HTTPS via `gh auth git-credential`, in the background) and open a PR to `staging` with `--body-file`. Body: what changed per task, the three cache tables and why they hold no user data, the `scan_state` keys, and links to the two runbooks.
3. Loop until CodeRabbit **APPROVED**. Five reviews per hour — if it says wait, wait. Batch fixes into as few pushes as possible.
4. After Bryan merges: run both runbooks on staging, record the numbers as deciduous outcomes under 788 and in the spec (§1 and §6), then `chainlink issue close <n> --no-changelog` — the CHANGELOG entry is already handwritten in Task 10.
5. Phase 2 (expiry + refresh, #344/#342) gets its own plan once the Phase 1 pass number exists.
