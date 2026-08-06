# Scan Admission Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the process-wide single-flight scan gate with a durable queue that admits up to N scans concurrently, so no user is ever refused because someone else is scanning.

**Architecture:** Three changes. Models move from per-scan loading into `AppState` at boot, so concurrency costs 500MB once instead of per scan. A `scan_queue` table (schema v11) replaces the `any_running` bool, making admission a database question that survives redeploys. A background admitter loop claims queued rows while `running_count < CHARCOAL_SCAN_CONCURRENCY`.

**Tech Stack:** Rust, axum, tokio, sqlx (Postgres) + rusqlite (SQLite), `backon` for retry, ONNX via `ort`, SvelteKit frontend.

**Spec:** `docs/superpowers/specs/2026-08-05-scan-admission-design.md`

## Global Constraints

- **Chainlink issue before any code.** `chainlink issue quick "..."` or `chainlink session work <id>` before the first Write/Edit/Bash. The PreToolUse hook blocks otherwise.
- **Branch first.** `git checkout -b <name>` before the first commit. Never commit to `staging` or `main` directly.
- **Stage files explicitly by name.** Never `git add -A`, `git add .`, `git commit -am`, or `git add *`.
- **Never use heredocs** (`<<EOF`) in shell commands — they break in zsh on this machine. Use single-quoted multi-line strings or `--body-file`.
- **Never use `git merge`/`rebase`/`cherry-pick`/`reset`/`tag`/`branch -D`** — blocked by `work-check.py` as human-reserved.
- **Test command is `CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output`.** Test binaries never load `.env`, so without `CHARCOAL_MODEL_DIR` the model-gated tests silently skip while printing `ok`.
- **Postgres tests need `DATABASE_URL`:** `DATABASE_URL=postgres://bryan.guffey@localhost/charcoal_test cargo test --features postgres`. Production runs Postgres; a SQLite-only test proves nothing about the deployed path.
- **Postgres migrations must self-record their version** — `INSERT INTO schema_version (version) VALUES (N) ON CONFLICT DO NOTHING;` as the last statement. The runner does not do it, and a migration that omits it re-runs forever.
- **Clippy must be clean on all three feature sets:** `cargo clippy --all-targets --features web`, `cargo clippy --all-targets`, `cargo clippy --all-targets --features postgres`.
- **Verify the population can fail.** Every regression test must be observed failing against the pre-fix code before it is trusted.
- **`CHARCOAL_SCAN_CONCURRENCY` defaults to 2.** Env-tunable, mirroring `CHARCOAL_BURST_CONCURRENCY`.

---

## File Structure

**Task 1 — #182 backoff**
- Modify: `src/bluesky/client.rs` — wrap `xrpc_get` in `backon` retry

**Task 2 — #264 instrumentation**
- Modify: `src/pipeline/scan_phases/gather.rs` — time fetch vs clean pass
- Modify: `src/pipeline/scan_phases/mod.rs` — aggregate and log at Phase A end

**Task 3 — models to AppState**
- Modify: `src/web/mod.rs` — `AppState` fields + load in `serve()`
- Modify: `src/web/scan_job.rs` — accept `Arc`'d models instead of loading
- Modify: `src/web/test_helpers.rs` — test `AppState` construction

**Task 4 — scan_queue storage**
- Create: `migrations/postgres/0011_scan_queue.sql`
- Modify: `src/db/schema.rs` — SQLite v11
- Modify: `src/db/traits.rs` — `ScanQueueEntry` + five trait methods
- Modify: `src/db/postgres.rs` — impl with `FOR UPDATE SKIP LOCKED` + register migration
- Modify: `src/db/queries.rs`, `src/db/sqlite.rs` — impl (single-process, write transaction)

**Task 5 — admitter loop**
- Create: `src/web/admitter.rs`
- Modify: `src/web/mod.rs` — spawn admitter in `serve()`

**Task 6 — API surface**
- Modify: `src/web/scan_job.rs` — `WebScanPhase::Queued`
- Modify: `src/web/handlers/scan.rs` — enqueue instead of refuse
- Modify: `src/web/handlers/status.rs` — queue block in the JSON

**Task 7 — frontend**
- Modify: `web/src/lib/types.ts`, `web/src/lib/components/ScanProgress.svelte`

---

### Task 1: Retry Bluesky reads on 429 and 5xx (#182)

Blocking dependency. Two concurrent scans mean 16 simultaneous Bluesky requests where `constellation/client.rs:22` documents a ~8 ceiling "until #182 lands". With no backoff, a 429 surfaces as a fetch failure, and #236 shows gather failures can cost a whole account.

**Files:**
- Modify: `src/bluesky/client.rs`
- Test: `src/bluesky/client.rs` (inline `#[cfg(test)]`, wiremock)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `PublicAtpClient::xrpc_get` retries transparently. Signature unchanged: `pub async fn xrpc_get<T: DeserializeOwned>(&self, nsid: &str, params: &[(&str, &str)]) -> Result<T>`.

- [ ] **Step 1: Create the chainlink issue and branch**

```bash
chainlink session work 182
git checkout -b fix/182-bluesky-backoff
```

- [ ] **Step 2: Write the failing tests**

Append to `src/bluesky/client.rs`:

```rust
#[cfg(test)]
mod retry_tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[derive(serde::Deserialize, Debug)]
    struct Probe {
        ok: bool,
    }

    /// A 429 must be retried, not surfaced as a failure. Bluesky publishes no
    /// backoff contract for the public read API, so a rate-limited gather
    /// currently loses the account outright (#236 shows the cost).
    #[tokio::test]
    async fn xrpc_get_retries_429_then_succeeds() {
        let server = MockServer::start().await;
        // First call 429, subsequent calls 200. `up_to_n_times(1)` makes the
        // 429 mock fire once; the 200 mock then serves the retry.
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(429))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .mount(&server)
            .await;

        let client = PublicAtpClient::new(&server.uri()).unwrap();
        let got: Probe = client.xrpc_get("probe.test", &[]).await.unwrap();
        assert!(got.ok, "the retry must return the successful body");
    }

    /// A 400 is the caller's fault and will never succeed. Retrying it wastes
    /// the budget and delays the real error.
    #[tokio::test]
    async fn xrpc_get_does_not_retry_4xx_other_than_429() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(400).set_body_string("bad request"))
            .expect(1) // exactly one attempt — a retry would make this 2+
            .mount(&server)
            .await;

        let client = PublicAtpClient::new(&server.uri()).unwrap();
        let err = client
            .xrpc_get::<Probe>("probe.test", &[])
            .await
            .expect_err("400 must surface");
        assert!(format!("{err:#}").contains("400"), "error must name the status");
        // MockServer asserts `.expect(1)` on drop.
    }

    /// 5xx is the server failing transiently — retry it.
    #[tokio::test]
    async fn xrpc_get_retries_5xx_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/xrpc/probe.test"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"ok":true}"#))
            .mount(&server)
            .await;

        let client = PublicAtpClient::new(&server.uri()).unwrap();
        let got: Probe = client.xrpc_get("probe.test", &[]).await.unwrap();
        assert!(got.ok);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test --features web --lib bluesky::client::retry_tests -- --nocapture`

Expected: `xrpc_get_retries_429_then_succeeds` FAILS — the 429 surfaces as an error rather than being retried. Record which tests fail; that is the population-can-fail check for this task.

- [ ] **Step 4: Add the typed error and retry wrapper**

In `src/bluesky/client.rs`, add imports at the top of the file (next to the existing `use` block):

```rust
use std::time::Duration;

use backon::{ExponentialBuilder, Retryable};
```

Add above `impl PublicAtpClient`:

```rust
/// Retry policy for public Bluesky reads (#182).
///
/// The public API publishes no backoff contract, and `DISCOVERY_CONCURRENCY`
/// in `constellation/client.rs` is capped at ~8 explicitly "until #182 lands".
/// Scan concurrency (#257) raises in-flight requests past that ceiling, so a
/// rate-limited read has to recover rather than fail — #236 shows a gather
/// failure can cost the whole account.
const XRPC_MAX_RETRIES: usize = 4;
const XRPC_MIN_BACKOFF_MS: u64 = 250;
const XRPC_MAX_BACKOFF_MS: u64 = 8_000;

/// Why an XRPC attempt failed, so the retry filter can be explicit rather than
/// string-matching an `anyhow` chain.
#[derive(Debug)]
enum XrpcAttemptError {
    /// Transport failure, 429, or 5xx — worth another attempt.
    Transient(anyhow::Error),
    /// 4xx other than 429, or a deserialization failure. Retrying cannot help.
    Permanent(anyhow::Error),
}

impl XrpcAttemptError {
    fn is_retryable(&self) -> bool {
        matches!(self, Self::Transient(_))
    }

    fn into_inner(self) -> anyhow::Error {
        match self {
            Self::Transient(e) | Self::Permanent(e) => e,
        }
    }
}
```

Now replace the body of `xrpc_get` with:

```rust
    pub async fn xrpc_get<T: DeserializeOwned>(
        &self,
        nsid: &str,
        params: &[(&str, &str)],
    ) -> Result<T> {
        let url = format!("{}/xrpc/{}", self.base_url, nsid);

        debug!(nsid = nsid, "XRPC GET request");

        let attempt = || async {
            let response = self
                .client
                .get(&url)
                .query(params)
                .send()
                .await
                .map_err(|e| {
                    // Transport-level failure: DNS, connect, timeout. Transient.
                    XrpcAttemptError::Transient(
                        anyhow::Error::new(e).context(format!("XRPC request failed: {nsid}")),
                    )
                })?;

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                let err = anyhow::anyhow!("XRPC {nsid} returned {status}: {body}");
                // 429 and 5xx recover on their own; every other 4xx is our bug.
                return Err(if status.as_u16() == 429 || status.is_server_error() {
                    XrpcAttemptError::Transient(err)
                } else {
                    XrpcAttemptError::Permanent(err)
                });
            }

            response.json::<T>().await.map_err(|e| {
                XrpcAttemptError::Permanent(
                    anyhow::Error::new(e).context(format!("Failed to deserialize {nsid} response")),
                )
            })
        };

        attempt
            .retry(
                ExponentialBuilder::default()
                    .with_min_delay(Duration::from_millis(XRPC_MIN_BACKOFF_MS))
                    .with_max_delay(Duration::from_millis(XRPC_MAX_BACKOFF_MS))
                    .with_max_times(XRPC_MAX_RETRIES)
                    .with_jitter(),
            )
            .when(|e: &XrpcAttemptError| e.is_retryable())
            .await
            .map_err(XrpcAttemptError::into_inner)
    }
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --features web --lib bluesky::client::retry_tests -- --nocapture`

Expected: PASS, 3 tests.

- [ ] **Step 6: Verify nothing else broke**

```bash
cargo clippy --all-targets --features web
cargo clippy --all-targets
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "test result:|SKIP:"
```

Expected: clippy silent; all suites `ok`; zero `SKIP:` lines.

- [ ] **Step 7: Commit**

```bash
git add src/bluesky/client.rs
git commit -m 'fix(182): retry Bluesky reads on 429 and 5xx

constellation/client.rs:22 caps discovery at ~8 in-flight "until #182
lands", and scan concurrency (#257) raises that to 16. With no backoff a
429 surfaces as a fetch failure, and #236 shows a gather failure can cost
the whole account.

Typed XrpcAttemptError so the retry filter is explicit rather than
string-matching an anyhow chain: transport errors, 429 and 5xx are
Transient; other 4xx and deserialization failures are Permanent and
returned on the first attempt.

4 retries, 250ms to 8s exponential with jitter, matching the RunPod
client convention in runpod_cope_b.rs.

Closes #182'
```

---

### Task 2: Instrument Phase A fetch vs inference time (#264)

The #257 concurrency default rests on an estimate. An earlier spec draft claimed Phase A was "~100% Bluesky I/O"; it is not — `gather.rs` runs the ONNX clean pass in the same phase, so two concurrent scans contend on the shared model mutex by an unmeasured amount.

**Files:**
- Modify: `src/pipeline/scan_phases/gather.rs`
- Modify: `src/pipeline/scan_phases/mod.rs`
- Test: `src/pipeline/scan_phases/gather.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: nothing from Task 1.
- Produces: `pub struct GatherTiming { pub fetch_ms: u64, pub clean_pass_ms: u64, pub total_ms: u64 }` with `GatherTiming::add(&mut self, other: &GatherTiming)`, exported from `scan_phases::gather`.

- [ ] **Step 1: Chainlink issue and branch**

```bash
chainlink session work 264
git checkout -b perf/264-phase-a-timing
```

- [ ] **Step 2: Write the failing test**

Append to `src/pipeline/scan_phases/gather.rs`:

```rust
#[cfg(test)]
mod timing_tests {
    use super::GatherTiming;

    /// Aggregation must be additive across accounts so the Phase A total is
    /// the sum of per-account work, not the last account's numbers.
    #[test]
    fn gather_timing_accumulates() {
        let mut total = GatherTiming::default();
        total.add(&GatherTiming {
            fetch_ms: 1000,
            clean_pass_ms: 200,
            total_ms: 1300,
        });
        total.add(&GatherTiming {
            fetch_ms: 500,
            clean_pass_ms: 100,
            total_ms: 700,
        });

        assert_eq!(total.fetch_ms, 1500);
        assert_eq!(total.clean_pass_ms, 300);
        assert_eq!(total.total_ms, 2000);
    }

    /// The number that decides the #257 concurrency default is the inference
    /// share. Under ~10% means concurrency 2 approaches 2x; 30%+ means the
    /// shared mutex is the ceiling.
    #[test]
    fn inference_share_is_reported_as_a_percentage() {
        let t = GatherTiming {
            fetch_ms: 900,
            clean_pass_ms: 100,
            total_ms: 1000,
        };
        assert_eq!(t.inference_pct(), 10);
    }

    /// A scan that gathered nothing must not divide by zero.
    #[test]
    fn inference_share_of_empty_scan_is_zero() {
        assert_eq!(GatherTiming::default().inference_pct(), 0);
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `cargo test --features web --lib scan_phases::gather::timing_tests`

Expected: FAIL — `cannot find struct GatherTiming`.

- [ ] **Step 4: Add the struct**

Near the top of `src/pipeline/scan_phases/gather.rs`, after the `use` block:

```rust
/// Per-account split of Phase A work (#264).
///
/// Phase A is not pure network wait: `onnx_clean_pass` runs here too, so two
/// concurrent scans (#257) contend on the shared model mutex. This measures by
/// how much, so the concurrency default is set from data. Mirrors the
/// delayTime/executionTime split that made the burst phase tunable (#61).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GatherTiming {
    /// Time in `fetch_sample` / `fetch_parent_posts` — the getAuthorFeed and
    /// getPosts round trips.
    pub fetch_ms: u64,
    /// Time in `onnx_clean_pass`.
    pub clean_pass_ms: u64,
    /// Wall clock for the whole account, including work in neither bucket.
    pub total_ms: u64,
}

impl GatherTiming {
    /// Accumulate another account's timings into this total.
    pub fn add(&mut self, other: &GatherTiming) {
        self.fetch_ms += other.fetch_ms;
        self.clean_pass_ms += other.clean_pass_ms;
        self.total_ms += other.total_ms;
    }

    /// Inference as a whole-number percentage of total Phase A time.
    ///
    /// This is the number that decides the #257 concurrency default: under
    /// ~10% and concurrency 2 should approach 2x; at 30%+ the shared model
    /// mutex is the ceiling and the lever is a session pool instead.
    pub fn inference_pct(&self) -> u64 {
        if self.total_ms == 0 {
            return 0;
        }
        self.clean_pass_ms * 100 / self.total_ms
    }
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test --features web --lib scan_phases::gather::timing_tests`

Expected: PASS, 3 tests.

- [ ] **Step 6: Populate the timings in `gather_account`**

In `src/pipeline/scan_phases/gather.rs`, change `gather_account`'s return type to carry the timing. Find the signature at line ~230 (`pub async fn gather_account(`) and change its return type from `Result<GatherOutcome>` to `Result<(GatherOutcome, GatherTiming)>`.

At the top of the function body add:

```rust
    let account_start = std::time::Instant::now();
    let mut timing = GatherTiming::default();
```

Wrap each fetch call. Replace `let stage1_sample = fetcher.fetch_sample(inputs.account_handle, 25).await?;` with:

```rust
    let stage1_sample = {
        let t = std::time::Instant::now();
        let r = fetcher.fetch_sample(inputs.account_handle, 25).await?;
        timing.fetch_ms += t.elapsed().as_millis() as u64;
        r
    };
```

Apply the same pattern to the `fetch_sample(inputs.account_handle, 50)` call and to every `fetch_parent_posts` call in this function.

Wrap the clean pass. Every call site of `clean_pass.onnx_clean_pass(...)` inside this function becomes:

```rust
    let scored = {
        let t = std::time::Instant::now();
        let r = clean_pass.onnx_clean_pass(texts).await;
        timing.clean_pass_ms += t.elapsed().as_millis() as u64;
        r
    };
```

At every `return`/tail expression, set `timing.total_ms = account_start.elapsed().as_millis() as u64;` and return `Ok((outcome, timing))`.

- [ ] **Step 7: Aggregate and log in Phase A**

In `src/pipeline/scan_phases/mod.rs`, the `buffer_unordered` stream at line ~400 now yields `(GatherOutcome, GatherTiming)` per account. Accumulate into a `GatherTiming::default()` as results arrive, then after the stream completes and before the phase banner, add:

```rust
    info!(
        phase = "gather",
        fetch_ms = phase_a_timing.fetch_ms,
        clean_pass_ms = phase_a_timing.clean_pass_ms,
        total_ms = phase_a_timing.total_ms,
        inference_pct = phase_a_timing.inference_pct(),
        "Phase A timing split (#264)"
    );
```

Fix every other call site of `gather_account` (there is a second at `mod.rs:572`) to destructure the tuple.

- [ ] **Step 8: Verify the whole suite**

```bash
cargo clippy --all-targets --features web
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "test result:|SKIP:"
```

Expected: clippy silent; all suites `ok`; zero `SKIP:`.

- [ ] **Step 9: Commit**

```bash
git add src/pipeline/scan_phases/gather.rs src/pipeline/scan_phases/mod.rs
git commit -m 'perf(264): record Phase A fetch vs clean-pass split

The #257 concurrency default rested on an estimate. An earlier draft of
the spec claimed Phase A was "~100% Bluesky I/O"; tracing the call sites
showed gather.rs runs the ONNX clean pass in the same phase, so two
concurrent scans contend on the shared model mutex by an unmeasured
amount.

gather_account now returns GatherTiming alongside its outcome, and Phase
A logs the aggregate with inference_pct. Under ~10% means concurrency 2
should approach 2x; 30%+ means the mutex is the ceiling and the lever is
a session pool rather than more concurrency.

Mirrors the delayTime/executionTime split from #61 that made the burst
phase tunable.

Closes #264'
```

- [ ] **Step 10: Capture the number**

Run a real scan on staging, then read the log line and record the result as a comment on #257 and #264:

```bash
railway logs -s charcoal-web -e staging 2>&1 | grep "Phase A timing split"
```

This number sets `CHARCOAL_SCAN_CONCURRENCY` in Task 5. Do not skip it.

---

### Task 3: Load models once at boot into AppState

Models currently load per scan (`scan_job.rs:292`, `:336`, `:372`) — deliberately, so they are not held while idle. With the fp32 NLI model (#231) that is ~500MB per concurrent scan against a 1.11GB production peak, which is what makes concurrency unsafe today.

**Files:**
- Modify: `src/web/mod.rs`
- Modify: `src/web/scan_job.rs`
- Modify: `src/web/test_helpers.rs`

**Interfaces:**
- Consumes: nothing from Tasks 1–2.
- Produces: `AppState` gains `pub models: Arc<ScanModels>`, where
  ```rust
  pub struct ScanModels {
      pub toxicity: Arc<OnnxToxicityScorer>,
      pub embedder: Arc<SentenceEmbedder>,
      pub nli: Arc<NliScorer>,
  }
  ```
  and `launch_scan` takes `models: Arc<ScanModels>` as a new parameter after `db`.

- [ ] **Step 1: Chainlink issue and branch**

```bash
chainlink issue quick "Load ONNX models once at boot into AppState (#52 slice for #257)" -p high -l refactor
git checkout -b feat/257-models-in-appstate
```

- [ ] **Step 2: Write the failing test**

Append to `src/web/scan_job.rs`:

```rust
#[cfg(test)]
mod model_sharing_tests {
    use super::*;

    /// Two scans must share one model instance, not load their own. This is the
    /// memory precondition for concurrency: per-scan loading costs ~500MB each
    /// (284 fp32 NLI + 126 toxicity + 90 embedding) against a 1.11GB prod peak.
    #[test]
    fn scan_models_are_shared_by_arc_not_cloned() {
        let base = crate::toxicity::download::resolve_model_dir();
        if !crate::toxicity::download::nli_files_present(&base) {
            eprintln!("SKIP: models not present at {}", base.display());
            return;
        }
        let models = Arc::new(ScanModels::load(&base).expect("load models"));
        let a = Arc::clone(&models);
        let b = Arc::clone(&models);

        // Same allocation behind both handles — a clone would be a second load.
        assert!(
            Arc::ptr_eq(&a.nli, &b.nli),
            "concurrent scans must share one NLI instance"
        );
        assert!(Arc::ptr_eq(&a.toxicity, &b.toxicity));
        assert!(Arc::ptr_eq(&a.embedder, &b.embedder));
        assert_eq!(Arc::strong_count(&models), 3, "models + a + b");
    }
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --lib web::scan_job::model_sharing_tests -- --show-output`

Expected: FAIL — `cannot find type ScanModels`. If it prints `SKIP:`, stop: models are missing and the test proves nothing. Run `cargo run --features web -- download-model` first.

- [ ] **Step 4: Add `ScanModels`**

In `src/web/scan_job.rs`, after the `use` block:

```rust
/// The three ONNX models a scan needs, loaded once and shared.
///
/// These used to load per scan so they were not held while idle. Concurrency
/// (#257) makes that cost linear in concurrent scans — ~500MB each since #231
/// moved NLI to the fp32 export — so they move to `AppState` and stay resident.
/// Accepted trade: production idles ~0.776GB today and ~1.28GB after, which
/// raises the metered RAM floor (#188) even on days nobody scans.
pub struct ScanModels {
    pub toxicity: Arc<OnnxToxicityScorer>,
    pub embedder: Arc<crate::topics::embeddings::SentenceEmbedder>,
    pub nli: Arc<crate::scoring::nli::NliScorer>,
}

impl ScanModels {
    /// Load all three models from `model_dir`.
    ///
    /// Fails hard rather than degrading: a server that cannot score is not
    /// usefully up, and production auto-downloads missing models before this
    /// point. The per-scan path used to degrade when a model was absent; at
    /// boot that would hide a broken deploy behind a green healthcheck.
    pub fn load(model_dir: &std::path::Path) -> anyhow::Result<Self> {
        let toxicity = OnnxToxicityScorer::load(model_dir)
            .context("failed to load the toxicity model at boot")?;
        let embedder = crate::topics::embeddings::SentenceEmbedder::load(
            &crate::toxicity::download::embedding_model_dir(model_dir),
        )
        .context("failed to load the embedding model at boot")?;
        let nli = crate::scoring::nli::NliScorer::load(model_dir)
            .context("failed to load the NLI model at boot")?;

        Ok(Self {
            toxicity: Arc::new(toxicity),
            embedder: Arc::new(embedder),
            nli: Arc::new(nli),
        })
    }
}
```

Add `use anyhow::Context;` to the imports if not already present.

- [ ] **Step 5: Run to verify it passes**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --lib web::scan_job::model_sharing_tests -- --show-output`

Expected: PASS, and no `SKIP:` line.

- [ ] **Step 6: Wire into AppState**

In `src/web/mod.rs`, add to the `AppState` struct:

```rust
    /// ONNX models, loaded once at boot and shared by every scan (#257).
    pub models: Arc<scan_job::ScanModels>,
```

In `serve()`, before the `let state = AppState {` block:

```rust
    // Load models before binding the port. Fail-fast: a server that cannot
    // score is not usefully up, and this surfaces a broken model volume as a
    // failed deploy rather than as scans that fail one by one later.
    let models = Arc::new(
        scan_job::ScanModels::load(&config.model_dir)
            .context("model load failed at boot — the server will not start")?,
    );
    info!("Loaded ONNX models (toxicity + embedding + NLI) into shared state");
```

Add `models,` to the `AppState { … }` initializer.

- [ ] **Step 7: Consume them in the scan**

In `src/web/scan_job.rs`, add `models: Arc<ScanModels>` as a parameter to `launch_scan` (after `db`) and to `run_scan`. Then delete the three per-scan load sites and replace their bindings:

- At the old `OnnxToxicityScorer::load` site (~line 292), use `Arc::clone(&models.toxicity)`.
- At the old `SentenceEmbedder::load` site (~line 336), use `Arc::clone(&models.embedder)`.
- At the old `NliScorer::load` site (~line 372), use `Arc::clone(&models.nli)`.

Remove the now-unused `model_files_present` / `embedding_files_present` / `nli_files_present` guards in this file — presence is proven by boot.

In `src/web/handlers/scan.rs`, pass `state.models.clone()` to `launch_scan`. Do the same at the admin call site in `src/web/handlers/admin.rs:158`.

- [ ] **Step 8: Fix test helpers**

In `src/web/test_helpers.rs`, add `models` to the test `AppState`. Because tests must not require model files, load lazily and skip when absent — mirror however the file already handles optional fixtures, and if there is no precedent, gate the helper behind `nli_files_present` and have callers skip.

- [ ] **Step 9: Verify**

```bash
cargo clippy --all-targets --features web
cargo clippy --all-targets
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "test result:|SKIP:"
```

Expected: clippy silent; all suites `ok`; zero `SKIP:`.

- [ ] **Step 10: Commit**

```bash
git add src/web/mod.rs src/web/scan_job.rs src/web/test_helpers.rs src/web/handlers/scan.rs src/web/handlers/admin.rs
git commit -m 'feat(257): load ONNX models once at boot into AppState

Models loaded per scan so they were not held while idle. Since #231 moved
NLI to the fp32 export that is ~500MB per concurrent scan against a
1.11GB production peak, which is what makes concurrency unsafe rather
than merely untidy — the global scan gate was implicitly protecting
memory, not just leftover from single-user days.

ScanModels holds all three behind Arc; scans clone handles. Their
existing Arc<Mutex<Session>> is untouched: this changes ownership and
lifetime, not concurrency semantics.

Fail-fast at boot rather than degrading per scan. The per-scan path
degraded when a model was absent; at boot that would hide a broken model
volume behind a green healthcheck.

Accepted cost: prod idles ~0.776GB today and ~1.28GB after, raising the
metered RAM floor (#188) even on days nobody scans.

Refs #257 #52'
```

---

### Task 4: `scan_queue` table and Database trait methods (schema v11)

> **⚠️ THIS TASK'S CODE AS WRITTEN BELOW IS DEFECTIVE. Superseded 2026-08-06.**
>
> Task 4 shipped as `7e8ec24` and its review found two Criticals that are
> defects in **this plan**, not in the implementation — the implementer
> transcribed Step 7 faithfully. A fix commit follows `7e8ec24`; read the
> code on the branch, not the listings below.
>
> 1. **Step 7's `claim_next_scan` cannot enforce the cap.** The
>    `COUNT(*) WHERE status='running'` takes no lock and the pool is READ
>    COMMITTED, so two admitters both read the pre-claim count, both pass
>    the guard, and both admit. `FOR UPDATE SKIP LOCKED` hands them
>    *different rows* — which is why it cannot bound the total. Reproduced
>    at cap 1: `running_after = 2`. Fixed with `pg_advisory_xact_lock`
>    before the count. See the spec's "Dual backend" section.
> 2. **Step 2's cap test cannot catch that.** Three sequential `.await`s on
>    one connection never contend a row. Step 10's negative control passed
>    and proved only that the guard *expression* was live — not the
>    guarantee the test's own doc comment claims. The replacement is a
>    genuinely concurrent `JoinSet` test, written red first.
>
> Also corrected in the fix commit: `enqueued_at` rendered
> backend-inconsistently (`::TEXT` vs `to_rfc3339`, and the Postgres form
> varies with connection `TimeZone` and is rejected by
> `parse_from_rfc3339`); `finish_queued_scan`/`heartbeat_scan` lacked a
> status guard and a fencing token, so a zombie worker whose lease lapsed
> could stomp its successor's row; `eta_seconds` returned `Some(0)` rather
> than `None` for a running scan and ignored the cap in its arithmetic.
>
> **Task 5 consumes these signatures — see the note at Task 5.**

**Files:**
- Create: `migrations/postgres/0011_scan_queue.sql`
- Modify: `src/db/schema.rs`, `src/db/traits.rs`, `src/db/postgres.rs`, `src/db/queries.rs`, `src/db/sqlite.rs`
- Test: `tests/db_postgres.rs`

**Interfaces:**
- Consumes: nothing from Tasks 1–3.
- Produces, on the `Database` trait:
  ```rust
  async fn enqueue_scan(&self, user_did: &str) -> Result<()>;
  async fn claim_next_scan(&self, limit: usize, lease_secs: i64) -> Result<Option<String>>;
  async fn heartbeat_scan(&self, user_did: &str, lease_secs: i64) -> Result<()>;
  async fn finish_queued_scan(&self, user_did: &str, error: Option<&str>) -> Result<()>;
  async fn reclaim_expired_scans(&self) -> Result<usize>;
  async fn scan_queue_entry(&self, user_did: &str) -> Result<Option<ScanQueueEntry>>;
  ```
  and `pub struct ScanQueueEntry { pub user_did: String, pub status: String, pub position: i64, pub eta_seconds: Option<i64>, pub enqueued_at: String }`

- [ ] **Step 1: Chainlink issue and branch**

```bash
chainlink session work 257
git checkout -b feat/257-scan-queue-storage
```

- [ ] **Step 2: Write the failing Postgres test**

Append to `tests/db_postgres.rs` (note the existing convention: its own user DID, because these tests share one database in parallel):

```rust
/// Admission must never exceed the cap, even when two admitters claim at once.
/// This is the guarantee FOR UPDATE SKIP LOCKED exists to provide, and the
/// reason the queue lives in the database rather than in a process-local bool.
#[tokio::test]
async fn test_pg_claim_respects_the_concurrency_cap() {
    const A: &str = "did:plc:pgtest_q_aaaaaaaaaaaaa";
    const B: &str = "did:plc:pgtest_q_bbbbbbbbbbbbb";
    const C: &str = "did:plc:pgtest_q_ccccccccccccc";

    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    for d in [A, B, C] {
        db.delete_user_data(d).await.unwrap();
        db.upsert_user(d, "q.bsky.social").await.unwrap();
        db.enqueue_scan(d).await.unwrap();
    }

    // Cap of 2: two claims succeed, the third is refused.
    let first = db.claim_next_scan(2, 120).await.unwrap();
    let second = db.claim_next_scan(2, 120).await.unwrap();
    let third = db.claim_next_scan(2, 120).await.unwrap();

    assert!(first.is_some(), "first claim must succeed");
    assert!(second.is_some(), "second claim must succeed");
    assert!(third.is_none(), "third claim must be refused at cap 2");

    for d in [A, B, C] {
        db.delete_user_data(d).await.unwrap();
    }
}

/// Enqueue is keyed by user_did, so a double-click cannot double-book.
#[tokio::test]
async fn test_pg_enqueue_is_idempotent() {
    const U: &str = "did:plc:pgtest_q_ddddddddddddd";
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "q.bsky.social").await.unwrap();

    db.enqueue_scan(U).await.unwrap();
    db.enqueue_scan(U).await.unwrap();

    let entry = db.scan_queue_entry(U).await.unwrap().expect("queued");
    assert_eq!(entry.status, "queued");
    assert_eq!(entry.position, 1, "one row, so position 1 — not two rows");

    db.delete_user_data(U).await.unwrap();
}

/// A scan orphaned by a redeploy must return to the queue, not vanish.
/// Combined with #208's scan_phase the reclaimed scan resumes rather than
/// restarting, so nobody re-pays for completed work.
#[tokio::test]
async fn test_pg_expired_lease_is_reclaimed() {
    const U: &str = "did:plc:pgtest_q_eeeeeeeeeeeee";
    let Some(url) = database_url() else {
        return;
    };
    let db = charcoal::db::connect_postgres(&url).await.unwrap();
    db.delete_user_data(U).await.unwrap();
    db.upsert_user(U, "q.bsky.social").await.unwrap();
    db.enqueue_scan(U).await.unwrap();

    // Claim with a lease that has already expired.
    let claimed = db.claim_next_scan(2, -1).await.unwrap();
    assert_eq!(claimed.as_deref(), Some(U));

    let reclaimed = db.reclaim_expired_scans().await.unwrap();
    assert_eq!(reclaimed, 1, "the expired running row must be re-queued");

    let entry = db.scan_queue_entry(U).await.unwrap().expect("present");
    assert_eq!(entry.status, "queued", "reclaimed back to queued");

    db.delete_user_data(U).await.unwrap();
}
```

- [ ] **Step 3: Run to verify it fails**

Run: `DATABASE_URL=postgres://bryan.guffey@localhost/charcoal_test cargo test --features postgres --test db_postgres scan_queue`

Expected: compile failure — `no method named enqueue_scan`. That is the correct red state.

- [ ] **Step 4: Write the Postgres migration**

Create `migrations/postgres/0011_scan_queue.sql`:

```sql
-- Migration v11: scan_queue — durable scan admission (#257).
--
-- Replaces ScanManager's process-local `any_running` bool. Admission becomes
-- "how many rows are running?", which survives a redeploy, is correct with more
-- than one replica, and makes queue position and ETA real rather than guessed.
--
-- user_did is the PK: one queued-or-running scan per user, so a double-click
-- cannot double-book. lease_expires is what makes a Railway redeploy safe — a
-- killed scan's lease lapses and the next boot re-queues it, and #208's
-- scan_phase means it resumes rather than restarts.

CREATE TABLE IF NOT EXISTS scan_queue (
    user_did TEXT PRIMARY KEY,
    status TEXT NOT NULL CHECK (status IN ('queued', 'running', 'done', 'failed')),
    enqueued_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    lease_expires TIMESTAMPTZ,
    last_error TEXT
);

-- Admission scans for the oldest queued row and counts running rows; both are
-- hot on every admitter tick.
CREATE INDEX IF NOT EXISTS idx_scan_queue_status_enqueued
    ON scan_queue (status, enqueued_at);

INSERT INTO schema_version (version) VALUES (11) ON CONFLICT DO NOTHING;
```

Register it in `src/db/postgres.rs` by appending to the `migrations` array (after the `(10, …)` entry at line ~136):

```rust
                (
                    11,
                    include_str!("../../migrations/postgres/0011_scan_queue.sql"),
                ),
```

- [ ] **Step 5: Write the SQLite migration**

In `src/db/schema.rs`, after the v10 `run_migration` block (ends ~line 361) and before `Ok(())`:

```rust
    // v11 — scan_queue: durable scan admission (#257).
    //
    // Mirrors migrations/postgres/0011_scan_queue.sql. SQLite stores timestamps
    // as TEXT where Postgres uses TIMESTAMPTZ.
    //
    // The SQLite implementation is deliberately minimal — single process, no
    // SKIP LOCKED needed — because #263 will delete this backend entirely.
    // Written to be removed, not maintained.
    run_migration(conn, 11, |c| {
        c.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS scan_queue (
                user_did        TEXT    NOT NULL PRIMARY KEY,
                status          TEXT    NOT NULL,
                enqueued_at     TEXT    NOT NULL,
                started_at      TEXT,
                finished_at     TEXT,
                lease_expires   TEXT,
                last_error      TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_scan_queue_status_enqueued
                ON scan_queue (status, enqueued_at);
            ",
        )
    })?;
```

- [ ] **Step 6: Add the type and trait methods**

In `src/db/traits.rs`, next to the `ScanSkip` struct:

```rust
/// A user's position in the scan queue (#257).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanQueueEntry {
    pub user_did: String,
    /// "queued" | "running" | "done" | "failed"
    pub status: String,
    /// 1-based position among queued rows; 0 when running or finished.
    pub position: i64,
    /// position x rolling median scan duration. None until enough scans have
    /// finished to have a median.
    pub eta_seconds: Option<i64>,
    pub enqueued_at: String,
}
```

Add to the `Database` trait:

```rust
    /// Add a user to the scan queue. Idempotent — a second call while queued or
    /// running is a no-op, so a double-click cannot double-book.
    async fn enqueue_scan(&self, user_did: &str) -> Result<()>;

    /// Claim the oldest queued scan if fewer than `limit` are running.
    /// Returns the claimed user_did, or None when at capacity or empty.
    /// `lease_secs` sets how long the claim is valid before it can be reclaimed.
    async fn claim_next_scan(&self, limit: usize, lease_secs: i64) -> Result<Option<String>>;

    /// Extend a running scan's lease. Called periodically while it runs.
    async fn heartbeat_scan(&self, user_did: &str, lease_secs: i64) -> Result<()>;

    /// Mark a scan done (error None) or failed (error Some), releasing its slot.
    async fn finish_queued_scan(&self, user_did: &str, error: Option<&str>) -> Result<()>;

    /// Return running rows whose lease has lapsed to 'queued'. Called at boot.
    /// Returns how many were reclaimed.
    async fn reclaim_expired_scans(&self) -> Result<usize>;

    /// A user's queue entry with position and ETA, or None if not queued.
    async fn scan_queue_entry(&self, user_did: &str) -> Result<Option<ScanQueueEntry>>;
```

- [ ] **Step 7: Implement for Postgres**

In `src/db/postgres.rs`, inside `impl Database for PgDatabase`:

```rust
    async fn enqueue_scan(&self, user_did: &str) -> Result<()> {
        // ON CONFLICT DO NOTHING when already queued or running; a finished row
        // is reset so a user can scan again.
        sqlx_core::query::query(
            "INSERT INTO scan_queue (user_did, status, enqueued_at)
             VALUES ($1, 'queued', NOW())
             ON CONFLICT (user_did) DO UPDATE
               SET status = 'queued', enqueued_at = NOW(),
                   started_at = NULL, finished_at = NULL,
                   lease_expires = NULL, last_error = NULL
             WHERE scan_queue.status IN ('done', 'failed')",
        )
        .bind(user_did)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn claim_next_scan(&self, limit: usize, lease_secs: i64) -> Result<Option<String>> {
        let mut tx = self.pool.begin().await?;

        let running: i64 =
            sqlx_core::query::query("SELECT COUNT(*) FROM scan_queue WHERE status = 'running'")
                .fetch_one(&mut *tx)
                .await?
                .get(0);
        if running >= limit as i64 {
            tx.commit().await?;
            return Ok(None);
        }

        // SKIP LOCKED so two admitters (or two replicas) never claim the same
        // row, and neither blocks waiting for the other.
        let row = sqlx_core::query::query(
            "SELECT user_did FROM scan_queue
             WHERE status = 'queued'
             ORDER BY enqueued_at
             LIMIT 1
             FOR UPDATE SKIP LOCKED",
        )
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let did: String = row.get(0);

        sqlx_core::query::query(
            "UPDATE scan_queue
             SET status = 'running', started_at = NOW(),
                 lease_expires = NOW() + make_interval(secs => $2)
             WHERE user_did = $1",
        )
        .bind(&did)
        .bind(lease_secs as f64)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(Some(did))
    }

    async fn heartbeat_scan(&self, user_did: &str, lease_secs: i64) -> Result<()> {
        sqlx_core::query::query(
            "UPDATE scan_queue
             SET lease_expires = NOW() + make_interval(secs => $2)
             WHERE user_did = $1 AND status = 'running'",
        )
        .bind(user_did)
        .bind(lease_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn finish_queued_scan(&self, user_did: &str, error: Option<&str>) -> Result<()> {
        sqlx_core::query::query(
            "UPDATE scan_queue
             SET status = CASE WHEN $2::TEXT IS NULL THEN 'done' ELSE 'failed' END,
                 finished_at = NOW(), lease_expires = NULL, last_error = $2
             WHERE user_did = $1",
        )
        .bind(user_did)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn reclaim_expired_scans(&self) -> Result<usize> {
        let result = sqlx_core::query::query(
            "UPDATE scan_queue
             SET status = 'queued', started_at = NULL, lease_expires = NULL
             WHERE status = 'running' AND lease_expires < NOW()",
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() as usize)
    }

    async fn scan_queue_entry(&self, user_did: &str) -> Result<Option<ScanQueueEntry>> {
        let row = sqlx_core::query::query(
            "SELECT status, enqueued_at::TEXT,
                    (SELECT COUNT(*) FROM scan_queue q2
                      WHERE q2.status = 'queued'
                        AND q2.enqueued_at <= q.enqueued_at) AS position
             FROM scan_queue q WHERE user_did = $1",
        )
        .bind(user_did)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = row else { return Ok(None) };
        let status: String = row.get(0);
        let enqueued_at: String = row.get(1);
        let position: i64 = if status == "queued" { row.get(2) } else { 0 };

        // Rolling median over the last 20 completed scans. NULL until any
        // finish, so ETA is absent rather than fabricated on a fresh install.
        let median: Option<f64> = sqlx_core::query::query(
            "SELECT PERCENTILE_CONT(0.5) WITHIN GROUP (
                 ORDER BY EXTRACT(EPOCH FROM (finished_at - started_at))
             )
             FROM (SELECT started_at, finished_at FROM scan_queue
                   WHERE status = 'done' AND started_at IS NOT NULL
                   ORDER BY finished_at DESC LIMIT 20) recent",
        )
        .fetch_one(&self.pool)
        .await?
        .get(0);

        Ok(Some(ScanQueueEntry {
            user_did: user_did.to_string(),
            status,
            position,
            eta_seconds: median.map(|m| (m * position as f64) as i64),
            enqueued_at,
        }))
    }
```

Add `use sqlx_core::row::Row;` to the imports if not already present.

- [ ] **Step 8: Implement for SQLite**

In `src/db/queries.rs`, add free functions mirroring the Postgres semantics using `conn.unchecked_transaction()` for `claim_next_scan` (single process, so no `SKIP LOCKED` is needed), and wire them through `src/db/sqlite.rs`'s `impl Database for SqliteDatabase`. Timestamps are `TEXT` via `chrono::Utc::now().to_rfc3339()`; compare lexicographically, which is correct for RFC3339 in UTC.

Keep this implementation minimal and comment it as slated for deletion by #263.

- [ ] **Step 9: Run the Postgres tests**

Run twice, to catch cross-test interference:

```bash
for i in 1 2; do
  DATABASE_URL=postgres://bryan.guffey@localhost/charcoal_test \
    cargo test --features postgres --test db_postgres 2>&1 | grep "test result:"
done
```

Expected: `ok` both runs, including the three new tests.

- [ ] **Step 10: Negative-control the cap test**

Temporarily change `claim_next_scan`'s guard from `running >= limit as i64` to `running >= 99`, re-run `test_pg_claim_respects_the_concurrency_cap`, and confirm it FAILS on the third claim. Restore the guard and confirm it passes again. A cap test that passes at every cap value is measuring nothing.

- [ ] **Step 11: Verify and commit**

```bash
cargo clippy --all-targets --features web
cargo clippy --all-targets --features postgres
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "test result:|SKIP:"
git add migrations/postgres/0011_scan_queue.sql src/db/schema.rs src/db/traits.rs src/db/postgres.rs src/db/queries.rs src/db/sqlite.rs tests/db_postgres.rs
git commit -m 'feat(257): scan_queue table and Database methods (schema v11)

Replaces ScanManager any_running bool with a durable queue. Admission
becomes "how many rows are running?" — which survives a redeploy, is
correct with more than one replica, and makes position and ETA real
rather than guessed.

user_did as PK makes enqueue idempotent, so a double-click cannot
double-book. lease_expires is what makes the deploy cadence safe: a
killed scan lapses and is re-queued at boot, and #208 scan_phase means it
resumes rather than restarts.

Postgres claims with FOR UPDATE SKIP LOCKED so two admitters never take
the same row. SQLite uses a write transaction — single process, no
SKIP LOCKED needed — and is deliberately minimal because #263 deletes
that backend.

Cap test negative-controlled: with the guard widened it fails on the
third claim, so it is measuring the cap rather than passing vacuously.

Refs #257'
```

---

### Task 5: The admitter loop

> **⚠️ Signatures below are STALE. Read the trait on the branch.**
> Task 4's fix commit changed `claim_next_scan`, `heartbeat_scan`, and
> `finish_queued_scan` — they now carry a claim/fencing token so a worker
> whose lease lapsed cannot release or extend its successor's slot. The
> loop's `match state.db.claim_next_scan(...)` and the
> `finish_queued_scan` / heartbeat calls in the listings below predate
> that change. Take the real signatures from `src/db/traits.rs` on
> `feat/257-scan-admission`; the control flow shown here is still correct.

**Files:**
- Create: `src/web/admitter.rs`
- Modify: `src/web/mod.rs`

**Interfaces:**
- Consumes: `Database::claim_next_scan`, `reclaim_expired_scans` (Task 4); `ScanModels` (Task 3).
- Produces: `pub fn spawn_admitter(state: AppState) -> tokio::sync::mpsc::Sender<()>` — the returned sender is a wake channel; send `()` after enqueue or completion to admit immediately rather than waiting for the tick.

- [ ] **Step 1: Branch**

```bash
git checkout -b feat/257-admitter
```

- [ ] **Step 2: Write the failing test**

Create `src/web/admitter.rs` with the test first:

```rust
// Background admitter: claims queued scans while under the concurrency cap.

use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::web::AppState;

/// How long a claimed scan's lease is valid. The scan heartbeats at a third of
/// this, so it takes three consecutive missed beats before another process
/// considers it dead.
const LEASE_SECS: i64 = 120;

/// Backstop tick. Enqueue and completion both wake the admitter directly; this
/// only covers a missed wake.
const TICK: Duration = Duration::from_secs(30);

/// Concurrent scans allowed. Default 2 — conservative given the ~500MB shared
/// model floor and the Bluesky pressure #182 addresses. Raise only after #264
/// reports the fetch/inference split.
pub fn scan_concurrency() -> usize {
    std::env::var("CHARCOAL_SCAN_CONCURRENCY")
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(2)
        .clamp(1, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrency_defaults_to_two_when_unset() {
        std::env::remove_var("CHARCOAL_SCAN_CONCURRENCY");
        assert_eq!(scan_concurrency(), 2);
    }

    #[test]
    fn concurrency_honours_the_env_var() {
        std::env::set_var("CHARCOAL_SCAN_CONCURRENCY", "3");
        assert_eq!(scan_concurrency(), 3);
        std::env::remove_var("CHARCOAL_SCAN_CONCURRENCY");
    }

    /// A malformed or zero value must not disable admission entirely, which
    /// would silently stop every scan on the server.
    #[test]
    fn concurrency_rejects_zero_and_garbage() {
        std::env::set_var("CHARCOAL_SCAN_CONCURRENCY", "0");
        assert_eq!(scan_concurrency(), 2, "zero must fall back, not block all scans");
        std::env::set_var("CHARCOAL_SCAN_CONCURRENCY", "banana");
        assert_eq!(scan_concurrency(), 2);
        std::env::remove_var("CHARCOAL_SCAN_CONCURRENCY");
    }

    /// Clamped so a fat-fingered value cannot exhaust memory or breach the
    /// Bluesky ceiling by an order of magnitude.
    #[test]
    fn concurrency_is_clamped() {
        std::env::set_var("CHARCOAL_SCAN_CONCURRENCY", "500");
        assert_eq!(scan_concurrency(), 8);
        std::env::remove_var("CHARCOAL_SCAN_CONCURRENCY");
    }
}
```

Register the module in `src/web/mod.rs`: `pub mod admitter;`

- [ ] **Step 3: Run to verify it fails, then passes**

Run: `cargo test --features web --lib web::admitter`

These tests pass immediately once the module compiles — they specify the config contract. Confirm all four pass before continuing.

Note: these use `std::env::set_var` and must not run in parallel with each other. If they interfere, add `serial_test::serial` (already a dev-dependency) to each.

- [ ] **Step 4: Implement the loop**

Append to `src/web/admitter.rs`:

```rust
/// Spawn the background admitter. Returns a wake channel: send `()` after an
/// enqueue or a scan completion to admit immediately instead of waiting a tick.
pub fn spawn_admitter(state: AppState) -> mpsc::Sender<()> {
    let (tx, mut rx) = mpsc::channel::<()>(32);
    let wake = tx.clone();

    tokio::spawn(async move {
        // Reclaim first: any row still 'running' at boot belongs to a scan this
        // process did not start, so its lease is stale by definition.
        match state.db.reclaim_expired_scans().await {
            Ok(0) => {}
            Ok(n) => info!(reclaimed = n, "re-queued scans orphaned by a restart"),
            Err(e) => error!(error = %format!("{e:#}"), "lease reclaim failed at boot"),
        }

        loop {
            let cap = scan_concurrency();
            loop {
                match state.db.claim_next_scan(cap, LEASE_SECS).await {
                    Ok(Some(user_did)) => {
                        info!(user_did = %user_did, cap, "admitting queued scan");
                        if let Err(e) = start_admitted_scan(&state, &user_did, wake.clone()).await {
                            error!(user_did = %user_did, error = %format!("{e:#}"), "failed to start admitted scan");
                            // Release the slot rather than holding it until the
                            // lease lapses — otherwise one bad row throttles
                            // the whole server for two minutes.
                            let _ = state
                                .db
                                .finish_queued_scan(&user_did, Some(&format!("{e:#}")))
                                .await;
                        }
                    }
                    Ok(None) => break, // at capacity, or nothing queued
                    Err(e) => {
                        error!(error = %format!("{e:#}"), "claim failed");
                        break;
                    }
                }
            }

            tokio::select! {
                _ = rx.recv() => {}
                _ = tokio::time::sleep(TICK) => {}
            }
        }
    });

    tx
}

/// Look up the handle and launch the scan for an already-claimed user.
async fn start_admitted_scan(
    state: &AppState,
    user_did: &str,
    wake: mpsc::Sender<()>,
) -> anyhow::Result<()> {
    let handle = state
        .db
        .get_user_handle(user_did)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user not found — they must re-authenticate"))?;

    crate::web::scan_job::launch_scan(
        state.config.clone(),
        state.db.clone(),
        state.models.clone(),
        state.scan_manager.clone(),
        user_did.to_string(),
        handle,
        wake,
    );
    Ok(())
}
```

- [ ] **Step 5: Make the scan release its slot and heartbeat**

In `src/web/scan_job.rs`, add `wake: mpsc::Sender<()>` as the final parameter of `launch_scan`. Inside the spawned task:

- Before running, spawn a heartbeat task that calls `db.heartbeat_scan(&user_did, LEASE_SECS)` every `LEASE_SECS / 3` seconds, and abort it when the scan returns.
- After the scan completes (success, error, or caught panic), call `db.finish_queued_scan(&user_did, err_text)` and then `let _ = wake.send(()).await;` so the next queued user starts immediately rather than waiting up to 30 seconds.

The existing `AssertUnwindSafe` catch already covers the panic path — put the `finish_queued_scan` call after it so all three exits are covered.

- [ ] **Step 6: Spawn from `serve()`**

In `src/web/mod.rs`, after building `state` and before `build_router`:

```rust
    let scan_wake = admitter::spawn_admitter(state.clone());
    let state = AppState {
        scan_wake: Some(scan_wake),
        ..state
    };
```

Add `pub scan_wake: Option<mpsc::Sender<()>>` to `AppState` (Option so `test_helpers` can construct without an admitter).

- [ ] **Step 7: Verify and commit**

```bash
cargo clippy --all-targets --features web
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "test result:|SKIP:"
git add src/web/admitter.rs src/web/mod.rs src/web/scan_job.rs
git commit -m 'feat(257): admitter loop claims queued scans under a cap

One background task per process. Wakes on enqueue, on completion, and on
a 30s backstop tick; claims while running < CHARCOAL_SCAN_CONCURRENCY
(default 2, clamped 1..8).

Reclaims at boot before its first claim: any row still running when this
process starts belongs to a scan it did not start, so the lease is stale
by definition.

A scan that fails to launch releases its slot immediately rather than
holding it until the lease lapses — otherwise one bad row throttles the
whole server for two minutes.

Config is defensive: zero or garbage falls back to 2 rather than
disabling admission, which would silently stop every scan.

Refs #257'
```

---

### Task 6: API — enqueue instead of refuse

> **⚠️ SCOPE EXPANDED 2026-08-06 — Bryan's ruling + Task 5 review finding I1.**
>
> **1. The admin-triggered scan must enqueue like everyone else.**
> `src/web/handlers/admin.rs:227` calls `launch_scan(..., None)`, which never
> touches `scan_queue` and is therefore invisible to `claim_next_scan`'s
> running count. It is currently gated only by `try_start_scan` — the same
> `any_running` gate this task removes — so **as originally written, Task 6
> would ship an uncapped admission path**: real concurrency would become
> `CHARCOAL_SCAN_CONCURRENCY + admin scans`. Asked whether admin should
> enqueue, jump the queue while still counting, or be a true uncapped
> override, Bryan chose **enqueue like everyone else**: one admission path,
> the cap holds absolutely, GPU spend stays bounded. Admin gives up
> start-immediately in exchange.
>
> **2. Deleting `try_start_scan` from `handlers/scan.rs` is NOT sufficient.**
> Task 5's implementer assumed the `any_running` flag becomes moot once this
> task lands. It does not — as originally written this task removes the gate
> from `scan.rs` only, leaving the field live, still set by
> `begin_admitted_scan` (`scan_job.rs:98`) and still cleared *unconditionally*
> by `finish_scan` (`scan_job.rs:137`) on the success path. Two real
> consequences: an admin trigger gets a spurious 409 saying "scans run one at
> a time" whenever any admitted scan runs, and with two admitted scans the
> first to finish clears the flag globally while the second still runs,
> admitting an extra scan outside the cap. **Delete the `any_running` field
> and reduce or remove `try_start_scan`** once the queue is the admission
> authority.
>
> **3. After this task there must be ZERO `launch_scan(..., None)` call
> sites.** Make the slot parameter non-optional, or make `launch_scan`
> `pub(crate)` and reachable only from the admitter. Nothing currently forces
> that cleanup, and `Option<QueueSlot>` makes "uncapped" easy to pass.
>
> **4. Stale doc:** `src/web/scan_job.rs:9` still says "Only one scan can run
> at a time; POST /api/scan returns 409 if one is already active."

**Files:**
- Modify: `src/web/scan_job.rs` (`WebScanPhase::Queued`, delete `any_running`)
- Modify: `src/web/handlers/scan.rs`
- Modify: `src/web/handlers/admin.rs` — enqueue, per Bryan's ruling above
- Modify: `src/web/handlers/status.rs`
- Test: `tests/web_scan_queue.rs` (create)

**Interfaces:**
- Consumes: `Database::enqueue_scan`, `scan_queue_entry` (Task 4); `AppState::scan_wake` (Task 5).
- Produces: `POST /api/scan` returns `202 {"status":"queued","position":N,"eta_seconds":N|null}`. `GET /api/status` gains `"queue": {"position":N,"eta_seconds":N|null,"enqueued_at":"..."}` when queued.

- [ ] **Step 1: Branch and write the regression test**

```bash
git checkout -b feat/257-queue-api
```

Create `tests/web_scan_queue.rs`:

```rust
//! The #257 regression: a second user must be QUEUED, not refused.
#![cfg(feature = "web")]

/// Before #257 this returned 409 "Another scan is already in progress on this
/// server". Under open signup that is the second user's entire experience.
///
/// This test MUST fail against the pre-#257 `any_running` gate. If it passes
/// before the fix, it is not testing the bug.
#[tokio::test]
async fn second_user_is_queued_not_refused() {
    let Some(state) = charcoal::web::test_helpers::try_state().await else {
        eprintln!("SKIP: test state unavailable (models or DB missing)");
        return;
    };

    state.db.upsert_user("did:plc:qa", "a.bsky.social").await.unwrap();
    state.db.upsert_user("did:plc:qb", "b.bsky.social").await.unwrap();

    state.db.enqueue_scan("did:plc:qa").await.unwrap();
    state.db.enqueue_scan("did:plc:qb").await.unwrap();

    let b = state
        .db
        .scan_queue_entry("did:plc:qb")
        .await
        .unwrap()
        .expect("second user must have a queue entry, not a rejection");

    assert_eq!(b.status, "queued");
    assert_eq!(b.position, 2, "second user is position 2, not refused");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `CHARCOAL_MODEL_DIR=./models cargo test --features web --test web_scan_queue -- --show-output`

Expected: FAIL (or compile error on `try_state`). Add `try_state()` to `src/web/test_helpers.rs` returning `Option<AppState>` — `None` when models or DB are unavailable — then re-run and confirm the assertion is what fails, not the setup.

- [ ] **Step 3: Add the Queued phase**

In `src/web/scan_job.rs`, add to `WebScanPhase`:

```rust
    /// Enqueued, waiting for a slot (#257).
    Queued,
```

Add `WebScanPhase::Queued => "queued",` to its `as_str` match.

- [ ] **Step 4: Rewrite the trigger handler**

Replace the body of `trigger_scan` in `src/web/handlers/scan.rs`:

```rust
pub async fn trigger_scan(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthUser>,
) -> impl IntoResponse {
    // Idempotent: a second click while queued or running returns the current
    // position rather than a second row or an error.
    if let Err(e) = state.db.enqueue_scan(&auth.did).await {
        tracing::error!(error = %format!("{e:#}"), "enqueue failed");
        return api_error(StatusCode::SERVICE_UNAVAILABLE, "Could not queue the scan");
    }

    // Wake the admitter so a free slot is taken now, not on the next tick.
    if let Some(wake) = &state.scan_wake {
        let _ = wake.try_send(());
    }

    let entry = state.db.scan_queue_entry(&auth.did).await.ok().flatten();
    let (position, eta) = entry
        .map(|e| (e.position, e.eta_seconds))
        .unwrap_or((0, None));

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "queued",
            "position": position,
            "eta_seconds": eta,
        })),
    )
        .into_response()
}
```

Remove the now-dead `try_start_scan` / `finish_scan` rollback branches and the `launch_scan` call — the admitter owns launching now.

- [ ] **Step 5: Add the queue block to status**

In `src/web/handlers/status.rs`, after the existing phase resolution, look up `state.db.scan_queue_entry(&auth.effective_did)`. When its status is `"queued"`, override the reported phase with `WebScanPhase::Queued` and add to the response JSON:

```rust
    "queue": {
        "position": entry.position,
        "eta_seconds": entry.eta_seconds,
        "enqueued_at": entry.enqueued_at,
    }
```

Omit the key entirely when the user is not queued, so existing clients see no change.

- [ ] **Step 6: Verify and commit**

```bash
cargo clippy --all-targets --features web
CHARCOAL_MODEL_DIR=./models cargo test --features web -- --show-output 2>&1 | grep -E "test result:|SKIP:"
git add src/web/scan_job.rs src/web/handlers/scan.rs src/web/handlers/status.rs src/web/test_helpers.rs tests/web_scan_queue.rs
git commit -m 'feat(257): POST /api/scan queues instead of refusing

Before this, a second concurrent user got 409 "Another scan is already in
progress on this server" — under open signup (#256) that is their entire
experience, and with 22min-2h scans they could see it all day.

Now: enqueue (idempotent by user_did PK), wake the admitter so a free slot
is taken immediately rather than on the next 30s tick, and return 202 with
position and ETA. 409 survives only for a user who already has a scan
queued or running.

GET /api/status gains a queue block and a Queued phase, omitted entirely
when the user is not queued so existing clients see no change.

Refs #257'
```

---

### Task 7: Show queue position in the UI

**Files:**
- Modify: `web/src/lib/types.ts`
- Modify: `web/src/lib/components/ScanProgress.svelte`

**Interfaces:**
- Consumes: the `queue` block from Task 6.
- Produces: no downstream consumers.

- [ ] **Step 1: Branch and extend the types**

```bash
git checkout -b feat/257-queue-ui
```

In `web/src/lib/types.ts`, add to the `ScanStatus` interface:

```ts
	/** Present only while queued (#257). */
	queue?: {
		position: number;
		eta_seconds: number | null;
		enqueued_at: string;
	};
```

- [ ] **Step 2: Write the failing test**

In `web/src/lib/dashboard-state.test.ts`, add:

```ts
	it('formats a queue position with an ETA', () => {
		expect(queueMessage({ position: 3, eta_seconds: 4200, enqueued_at: '' })).toBe(
			"You're 3rd in line — about 70 minutes"
		);
	});

	it('omits the estimate when no scans have finished yet', () => {
		expect(queueMessage({ position: 1, eta_seconds: null, enqueued_at: '' })).toBe(
			"You're next in line"
		);
	});

	it('says next rather than 1st', () => {
		expect(queueMessage({ position: 1, eta_seconds: 600, enqueued_at: '' })).toBe(
			"You're next in line — about 10 minutes"
		);
	});
```

- [ ] **Step 3: Run to verify it fails**

Run: `npm --prefix web run test`

Expected: FAIL — `queueMessage is not defined`.

- [ ] **Step 4: Implement**

In `web/src/lib/dashboard-state.ts`:

```ts
/** Human phrasing for a queue position (#257). */
export function queueMessage(q: { position: number; eta_seconds: number | null }): string {
	const place =
		q.position <= 1 ? "You're next in line" : `You're ${ordinal(q.position)} in line`;
	if (q.eta_seconds == null) return place;
	const mins = Math.max(1, Math.round(q.eta_seconds / 60));
	return `${place} — about ${mins} minute${mins === 1 ? '' : 's'}`;
}

function ordinal(n: number): string {
	const s = ['th', 'st', 'nd', 'rd'];
	const v = n % 100;
	return n + (s[(v - 20) % 10] || s[v] || s[0]);
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `npm --prefix web run test`

Expected: PASS.

- [ ] **Step 6: Render it**

In `web/src/lib/components/ScanProgress.svelte`, when `status.queue` is present, show `queueMessage(status.queue)` in place of the error toast, styled as an informational state rather than a failure.

Use the `impeccable` design tokens: text in `#a8a29e`, no saturated colours. This is a waiting state, not an error — the Indoor Voice Rule applies.

- [ ] **Step 7: Verify and commit**

```bash
npm --prefix web run test
npm --prefix web run build
git add web/src/lib/types.ts web/src/lib/dashboard-state.ts web/src/lib/dashboard-state.test.ts web/src/lib/components/ScanProgress.svelte
git commit -m 'feat(257): show queue position instead of an error toast

A queued user saw "Another scan is already in progress on this server" in
an error toast. Now they see "You are 3rd in line - about 70 minutes".

ETA is omitted rather than fabricated when no scans have finished yet, so
a fresh install does not invent a number.

Styled as an informational state, not a failure: waiting is the system
working as designed, and the Indoor Voice Rule in DESIGN.md applies.

Refs #257'
```

- [ ] **Step 8: Open the PR**

```bash
gh pr create --base staging --title 'feat(257): durable scan queue with bounded concurrency' --body-file <path to a written description>
```

Note `web/build/` is gitignored-but-tracked (#228); if `npm run build` dirties it, do not stage those files.

---

## Post-merge verification

- [ ] Deploy to staging and confirm `/health` is 200 and boot logs show `Loaded ONNX models`.
- [ ] Trigger two scans from two accounts; confirm the second reports `position: 2` rather than a 409.
- [ ] Redeploy mid-scan; confirm the boot log shows `re-queued scans orphaned by a restart` and the scan resumes rather than restarting.
- [ ] Read the `Phase A timing split` line and set `CHARCOAL_SCAN_CONCURRENCY` from the measured `inference_pct`.
