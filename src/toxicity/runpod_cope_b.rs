//! Self-hosted CoPE-B-A4B classifier on RunPod Serverless.
//!
//! Wire shape (batch, Task 3):
//!   POST <endpoint_url>/runsync
//!   Authorization: Bearer <api_key>
//!   {"input": {"contents": ["<envelope>", ...]}}
//!   -> {"output": {"verdicts": [
//!         {"ok": true, "toxic": bool, "confidence": float,
//!          "model": str, "policy_version": str},
//!         {"ok": false, "error": str},
//!         ...
//!      ]}}
//!
//! classify() (single) delegates to a 1-element classify_batch, unwrapping
//! slot 0. classify_batch() is the single HTTP chokepoint for all call paths.
//!
//! Retries on 5xx with bounded decorrelated jitter. 4xx surfaces immediately
//! (config / contract issue). Timeout is split between warm-up and steady-
//! state — see `RunPodCopeBClient::classify_batch_with_timeout`.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use backon::{ExponentialBuilder, Retryable};
use serde::Deserialize;
use std::time::{Duration, Instant};
use thiserror::Error;

use std::sync::Arc;

use super::classifier::{
    ClassifierTransientError, ClassifierVerdict, ItemOutcome, ToxicityClassifier,
};
use super::cost_meter::{CostCeilingExceeded, ScanCostMeter};

/// Calibration note (Chunk 5 / Step 5): verify distribution parity vs the
/// reference (Zentropi) backend over ab_sample.jsonl; retune only if materially
/// shifted. See docs/superpowers/specs/2026-06-05-cope-b-self-hosted-design.md
/// §"Step 5". 0.5 is the initial value — a model emitting a binary token with
/// logprob-based confidence concentrates probability sharply, so the calibrated
/// threshold may land closer to 0.7+. Update via code change, never env
/// (spec §"Backend selection").
pub const COPE_B_THRESHOLD: f32 = 0.5;

const INITIAL_BACKOFF_MS: u64 = 500;
/// Cap on a single backoff sleep so the exponential schedule doesn't balloon.
/// With base 500ms and 6 retries the window spans ~20s+ (0.5,1,2,4,8,8) —
/// enough to bridge a typical RunPod serverless scale-up / transient blip.
const MAX_BACKOFF_MS: u64 = 8_000;
/// Default per-call retry budget. Widened from 3 after a single transport blip
/// (a ~few-second RunPod outage outran the old ~3.5s/3-retry window) aborted a
/// 4905-item burst (#183).
const DEFAULT_MAX_RETRIES: u32 = 6;

/// Resolve the per-call retry budget from the (already-read) env value.
/// Pure for testability: garbage / missing falls back to [`DEFAULT_MAX_RETRIES`].
fn classifier_max_retries(raw: Option<&str>) -> u32 {
    raw.and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(DEFAULT_MAX_RETRIES)
}

/// Resolve the RunPod batch size from env. Missing/garbage → 32; clamped 1..=128.
fn runpod_batch_size(raw: Option<&str>) -> usize {
    raw.and_then(|s| s.trim().parse::<usize>().ok())
        .unwrap_or(32)
        .clamp(1, 128)
}

/// Map a final (post-retry) [`RunPodError`] into the error the classifier
/// surfaces to callers. A *retryable* error reaching here means the retry budget
/// was exhausted on a transient failure (transport / 5xx) → a typed
/// [`ClassifierTransientError`] so the burst can stop gracefully and resumably.
/// A non-retryable error (4xx) is permanent and is propagated as-is so the burst
/// aborts (leaving its row pending would livelock every resume).
fn classifier_error_from_exhausted(e: RunPodError) -> anyhow::Error {
    // The cost backstop tripped mid-flight: surface the typed ceiling error so
    // the burst keys on it (downcastable), exactly like a pre-flight trip.
    if let RunPodError::CostCapped(c) = e {
        return c.into();
    }
    if e.is_retryable() {
        ClassifierTransientError::new(e.to_string()).into()
    } else {
        e.into()
    }
}

#[derive(Debug, Clone)]
pub struct RunPodCopeBClient {
    client: reqwest::Client,
    endpoint_url: String,
    api_key: String,
    steady_timeout: Duration,
    warmup_timeout: Duration,
    max_retries: u32,
    /// Per-scan cost backstop. Default (from `new`) is disabled; `build_from_env`
    /// attaches an env-configured meter per scan.
    meter: Arc<ScanCostMeter>,
}

/// RunPod job envelope returned by `/runsync` and `/status/{id}`. `/runsync`
/// waits up to ~90s and, if the job hasn't finished, returns a non-terminal
/// status (`IN_QUEUE`/`IN_PROGRESS`) with NO `output` — the caller must then
/// poll `/status/{id}`. Cold starts (model load) routinely exceed the runsync
/// window, so this is the normal path, not an error.
#[derive(Debug, Deserialize)]
struct JobEnvelope {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    output: Option<RawBatchOutput>,
    #[serde(default)]
    error: Option<serde_json::Value>,
    /// RunPod-reported queue wait time in milliseconds. Present on terminal
    /// responses; absent while the job is still IN_QUEUE/IN_PROGRESS.
    #[serde(rename = "delayTime", default)]
    delay_time_ms: Option<u32>,
    /// RunPod-reported inference (execution) time in milliseconds. Present on
    /// terminal responses; absent while the job is still running.
    #[serde(rename = "executionTime", default)]
    execution_time_ms: Option<u32>,
}

/// Result of interpreting a single batch job-envelope response.
enum BatchJobOutcome {
    Completed(Vec<ItemOutcome>),
    /// Job accepted but not finished yet; carries the id to poll `/status` with.
    Pending(String),
}

/// Batch output body: the handler returns `{"verdicts": [...]}` under RunPod's
/// `output` wrapper.
#[derive(Debug, Deserialize)]
struct RawBatchOutput {
    verdicts: Vec<RawItem>,
}

/// One verdict slot. `ok:true` carries the verdict fields; `ok:false` carries
/// `error`. Fields are optional so a slot of either shape deserialises; the
/// mapping in `parse_batch_response` enforces which fields must be present.
#[derive(Debug, Deserialize)]
struct RawItem {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    toxic: Option<bool>,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default = "default_model")]
    model: String,
    #[serde(default = "default_policy_version")]
    policy_version: String,
    #[serde(default)]
    error: Option<String>,
}

fn default_model() -> String {
    "cope-b-a4b".into()
}
fn default_policy_version() -> String {
    "policy-unknown".into()
}

/// Typed retry classification so backon's `.when()` filter checks an enum
/// variant instead of grepping stringified error messages.
#[derive(Debug, Error)]
enum RunPodError {
    #[error("RunPod transport: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("RunPod HTTP 5xx: {0}")]
    ServerError(reqwest::StatusCode),
    #[error("RunPod HTTP {0} (non-retryable)")]
    ClientError(reqwest::StatusCode),
    /// The per-attempt cost backstop tripped — non-retryable. Carries the typed
    /// ceiling error so it can be surfaced (downcastable) to the burst phase.
    #[error("RunPod cost ceiling: {0}")]
    CostCapped(CostCeilingExceeded),
}

impl RunPodError {
    fn is_retryable(&self) -> bool {
        matches!(
            self,
            RunPodError::Transport(_) | RunPodError::ServerError(_)
        )
    }
}

/// Tracks retries inside the backon closure so the metrics module can emit
/// a real `classifier_retry_count` instead of a hardcoded zero.
#[derive(Default, Clone)]
struct RetryCounter(std::sync::Arc<std::sync::atomic::AtomicU32>);

impl RetryCounter {
    fn bump(&self) {
        use std::sync::atomic::Ordering;
        self.0.fetch_add(1, Ordering::Relaxed);
    }
    fn get(&self) -> u32 {
        use std::sync::atomic::Ordering;
        self.0.load(Ordering::Relaxed)
    }
}

impl RunPodCopeBClient {
    pub fn new(endpoint_url: String, api_key: String) -> Result<Self> {
        if endpoint_url.is_empty() {
            bail!("RunPod endpoint URL is required");
        }
        if api_key.is_empty() {
            bail!("RunPod api key is required");
        }
        // Read timeouts + retries from env per spec §"Environment variables";
        // fall back to spec defaults if unset.
        let steady_ms = std::env::var("CHARCOAL_CLASSIFIER_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60_000);
        let warmup_ms = std::env::var("CHARCOAL_CLASSIFIER_WARMUP_TIMEOUT_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(180_000);
        let max_retries = classifier_max_retries(
            std::env::var("CHARCOAL_CLASSIFIER_MAX_RETRIES")
                .ok()
                .as_deref(),
        );

        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(steady_ms))
            .build()
            .context("build reqwest client")?;
        Ok(Self {
            client,
            endpoint_url,
            api_key,
            steady_timeout: Duration::from_millis(steady_ms),
            warmup_timeout: Duration::from_millis(warmup_ms),
            max_retries,
            meter: Arc::new(ScanCostMeter::new(
                0,
                super::cost_meter::DEFAULT_RATE_CENTS_PER_HOUR,
            )),
        })
    }

    /// Attach a per-scan cost meter. Builder so `new`'s signature (and its
    /// existing callers/tests) stay unchanged.
    pub fn with_meter(mut self, meter: Arc<ScanCostMeter>) -> Self {
        self.meter = meter;
        self
    }

    pub fn build_batch_request_body(contents: &[String]) -> String {
        serde_json::json!({ "input": { "contents": contents } }).to_string()
    }

    /// Map one completed batch envelope into per-slot outcomes. A slot whose
    /// `ok` is false, or whose `confidence` is missing/NaN/out-of-[0,1], becomes
    /// an `ItemOutcome::Error` (no silent fallback) rather than a Verdict.
    fn map_verdicts(out: RawBatchOutput, latency_ms: u32) -> Vec<ItemOutcome> {
        out.verdicts
            .into_iter()
            .map(|item| {
                if !item.ok {
                    return ItemOutcome::Error(
                        item.error
                            .unwrap_or_else(|| "unspecified item error".into()),
                    );
                }
                let (Some(toxic), Some(confidence)) = (item.toxic, item.confidence) else {
                    return ItemOutcome::Error("ok slot missing toxic/confidence".into());
                };
                if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                    return ItemOutcome::Error(format!(
                        "confidence out of contract (finite [0,1]): {confidence}"
                    ));
                }
                ItemOutcome::Verdict(ClassifierVerdict {
                    toxic_token: toxic,
                    confidence,
                    latency_ms,
                    model_id: item.model,
                    policy_version: item.policy_version,
                })
            })
            .collect()
    }

    /// Interpret one job-envelope response as either a terminal batch of
    /// outcomes or a pending signal (poll `/status`).
    fn parse_batch_job(raw: &str, latency_ms: u32) -> Result<BatchJobOutcome> {
        let env: JobEnvelope = serde_json::from_str(raw)
            .with_context(|| format!("parse RunPod response body: {raw}"))?;
        let status = env.status.as_deref().unwrap_or("").to_ascii_uppercase();

        if matches!(status.as_str(), "FAILED" | "CANCELLED" | "TIMED_OUT") {
            let detail = env
                .error
                .map(|e| e.to_string())
                .unwrap_or_else(|| raw.to_string());
            bail!("RunPod job {status}: {detail}");
        }

        let delay_time_ms = env.delay_time_ms;
        let execution_time_ms = env.execution_time_ms;

        if let Some(out) = env.output {
            crate::observability::classifier_metrics::record_runpod_timing(
                delay_time_ms,
                execution_time_ms,
                latency_ms,
            );
            return Ok(BatchJobOutcome::Completed(Self::map_verdicts(
                out, latency_ms,
            )));
        }

        if matches!(status.as_str(), "IN_QUEUE" | "IN_PROGRESS") {
            let id = env
                .id
                .ok_or_else(|| anyhow::anyhow!("RunPod {status} response missing job id: {raw}"))?;
            return Ok(BatchJobOutcome::Pending(id));
        }
        bail!("RunPod job {status:?} returned no output: {raw}");
    }

    /// Test/entry helper: parse a terminal batch response into outcomes.
    pub fn parse_batch_response(raw: &str, latency_ms: u32) -> Result<Vec<ItemOutcome>> {
        match Self::parse_batch_job(raw, latency_ms)? {
            BatchJobOutcome::Completed(v) => Ok(v),
            BatchJobOutcome::Pending(id) => {
                bail!("RunPod job {id} not terminal in the /runsync response (still pending)")
            }
        }
    }

    /// Poll `/status/{id}` until the job reaches a terminal state or `timeout`
    /// (measured from `start`) elapses. Used when `/runsync` returns before the
    /// job finishes (the cold-start path).
    async fn poll_status(
        &self,
        job_id: &str,
        start: Instant,
        timeout: Duration,
    ) -> Result<Vec<ItemOutcome>> {
        let url = format!(
            "{}/status/{}",
            self.endpoint_url.trim_end_matches('/'),
            job_id
        );
        let poll_interval = Duration::from_millis(2_000);
        loop {
            if start.elapsed() >= timeout {
                bail!("RunPod job {job_id} did not complete within {timeout:?}");
            }
            tokio::time::sleep(poll_interval).await;
            let resp = self
                .client
                .get(&url)
                .bearer_auth(&self.api_key)
                .timeout(Duration::from_secs(30))
                .send()
                .await
                .with_context(|| format!("poll RunPod status for {job_id}"))?;
            let http = resp.status();
            // 5xx while polling is transient — keep waiting. 4xx is a real
            // contract/config error.
            if http.is_server_error() {
                continue;
            }
            if !http.is_success() {
                bail!("RunPod /status HTTP {http} for {job_id}");
            }
            let body = resp.text().await?;
            let latency_ms: u32 = start.elapsed().as_millis().try_into().unwrap_or(u32::MAX);
            match Self::parse_batch_job(&body, latency_ms)? {
                BatchJobOutcome::Completed(v) => return Ok(v),
                BatchJobOutcome::Pending(_) => continue,
            }
        }
    }

    /// Single attempt — issued from inside the retry loop in classify_batch_with_timeout.
    /// Returns the JSON body string on 2xx, a typed RunPodError otherwise.
    async fn attempt(
        client: &reqwest::Client,
        url: &str,
        api_key: &str,
        body: &str,
        timeout: Duration,
    ) -> std::result::Result<String, RunPodError> {
        let resp = client
            .post(url)
            .bearer_auth(api_key)
            .header("content-type", "application/json")
            .timeout(timeout)
            .body(body.to_string())
            .send()
            .await?;
        let status = resp.status();
        if status.is_success() {
            Ok(resp.text().await?)
        } else if status.is_server_error() {
            Err(RunPodError::ServerError(status))
        } else {
            Err(RunPodError::ClientError(status))
        }
    }

    /// Single HTTP chokepoint for all classification paths. Takes a batch of
    /// content strings; classify() passes a 1-element slice. Returns the
    /// per-slot outcomes and the retry count for metrics.
    ///
    /// Cost backstop: the ceiling is checked per-attempt INSIDE the retry
    /// closure below (before each request), so even a mid-burst budget blow
    /// stops further retries — a trip becomes a non-retryable
    /// `RunPodError::CostCapped` that backon returns at once, riding the same
    /// skip path the live HTTP 402 already exercised. classify_batch_with_timeout
    /// is the single chokepoint every request path flows through (classify,
    /// classify_batch, and warm_up), so the meter cannot be bypassed. The
    /// in-flight worker time is billed by a guard scoped to each real request
    /// — never across the retry backoff gaps, which have no active GPU.
    async fn classify_batch_with_timeout(
        &self,
        contents: &[String],
        timeout: Duration,
    ) -> Result<(Vec<ItemOutcome>, u32)> {
        let body = Self::build_batch_request_body(contents);
        let url = format!("{}/runsync", self.endpoint_url.trim_end_matches('/'));
        let start = Instant::now();
        let retries = RetryCounter::default();

        // Owned clones moved into the closure satisfy backon's FnMut+'static
        // bound. Each retry calls the closure again; clones are cheap (reqwest::Client
        // is Arc-internal, the strings are small).
        let client = self.client.clone();
        let url_owned = url;
        let api_key = self.api_key.clone();
        let body_owned = body;
        let retries_in = retries.clone();
        let meter = self.meter.clone();

        let attempt = move || {
            let client = client.clone();
            let url = url_owned.clone();
            let key = api_key.clone();
            let body = body_owned.clone();
            let retries = retries_in.clone();
            let meter = meter.clone();
            async move {
                // Re-check the cost ceiling on EVERY attempt (including retries)
                // BEFORE issuing the request. CostCapped is non-retryable, so
                // backon returns it immediately — retries can't bypass the brake.
                meter.check().map_err(RunPodError::CostCapped)?;
                // Bill only the actual HTTP request: the guard drops before the
                // next attempt, so backon's backoff sleep is never billed.
                let _g = meter.guard();
                let r = Self::attempt(&client, &url, &key, &body, timeout).await;
                if r.is_err() {
                    retries.bump();
                }
                r
            }
        };

        let response = match attempt
            .retry(
                ExponentialBuilder::default()
                    .with_min_delay(Duration::from_millis(INITIAL_BACKOFF_MS))
                    .with_max_delay(Duration::from_millis(MAX_BACKOFF_MS))
                    .with_max_times(self.max_retries as usize)
                    .with_jitter(),
            )
            .when(|e: &RunPodError| e.is_retryable())
            .await
        {
            Ok(r) => r,
            // Retries exhausted (or a non-retryable error). Map to the typed error
            // the burst phase keys on: transient (transport/5xx) → graceful resume;
            // permanent (4xx) → abort. See `classifier_error_from_exhausted`.
            Err(e) => return Err(classifier_error_from_exhausted(e)),
        };

        let latency_ms: u32 = start.elapsed().as_millis().try_into().unwrap_or(u32::MAX);
        // RetryCounter bumps on every failed attempt. Each failure that doesn't
        // exhaust the budget triggers exactly one retry; the final successful
        // attempt does NOT bump. So `get()` already equals retries-issued.
        let observed = retries.get();
        // /runsync may return before the job finishes (cold starts exceed its
        // ~90s wait) — in that case poll /status until terminal.
        let verdicts = match Self::parse_batch_job(&response, latency_ms)? {
            BatchJobOutcome::Completed(v) => v,
            BatchJobOutcome::Pending(id) => {
                // The job is still executing server-side on a worker — that IS
                // billable, so hold a guard across the poll loop.
                let _g = self.meter.guard();
                self.poll_status(&id, start, timeout).await?
            }
        };
        Ok((verdicts, observed))
    }
}

#[async_trait]
impl ToxicityClassifier for RunPodCopeBClient {
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
        // Single classify rides the batch path (batch-only wire contract): send
        // a 1-element batch and unwrap slot 0.
        let (mut outcomes, retries) = self
            .classify_batch_with_timeout(&[content.to_string()], self.steady_timeout)
            .await?;
        let outcome = outcomes.drain(..).next().ok_or_else(|| {
            anyhow::anyhow!("RunPod returned an empty batch for a single classify")
        })?;
        match outcome {
            ItemOutcome::Verdict(verdict) => {
                crate::observability::classifier_metrics::record_request(
                    self.name(),
                    verdict.latency_ms,
                    verdict.toxic_token,
                    retries,
                );
                Ok(verdict)
            }
            ItemOutcome::Error(e) => Err(anyhow::anyhow!("RunPod decode error: {e}")),
        }
    }

    async fn classify_batch(&self, contents: &[String]) -> Result<Vec<ItemOutcome>> {
        let (outcomes, retries) = self
            .classify_batch_with_timeout(contents, self.steady_timeout)
            .await?;
        // Record the batch request's real observed latency (every verdict slot
        // shares the per-request wall clock stamped by classify_batch_with_timeout)
        // rather than a 0 that would dilute the classifier_request_latency_ms
        // histogram. Per-slot toxic flags aren't summed here (finalize aggregates
        // verdicts); retries are recorded once per batch request.
        let observed_latency_ms = outcomes
            .iter()
            .find_map(|o| match o {
                ItemOutcome::Verdict(v) => Some(v.latency_ms),
                ItemOutcome::Error(_) => None,
            })
            .unwrap_or(0);
        crate::observability::classifier_metrics::record_request(
            self.name(),
            observed_latency_ms,
            false,
            retries,
        );
        Ok(outcomes)
    }

    fn max_batch_size(&self) -> usize {
        runpod_batch_size(std::env::var("CHARCOAL_RUNPOD_BATCH_SIZE").ok().as_deref())
    }

    fn name(&self) -> &'static str {
        "runpod-cope-b"
    }
    fn model_id(&self) -> &'static str {
        "cope-b-a4b"
    }
    fn policy_version(&self) -> &'static str {
        // Default for trait-level callers (e.g. health-check banner). The
        // real per-call value lives on ClassifierVerdict.policy_version,
        // which carries the response field — Chunk 3's handler.py sets it
        // from the image's POLICY_VERSION build-arg.
        "policy-unknown"
    }
    fn threshold(&self) -> f32 {
        COPE_B_THRESHOLD
    }
}

/// Helper for the scan manager: invoke once at the start of a scan to absorb
/// FlashBoot cold start into the "warming up" UX message. Same retry policy,
/// longer timeout.
pub async fn warm_up(client: &RunPodCopeBClient) -> Result<()> {
    let (_, _retries) = client
        .classify_batch_with_timeout(
            &["[Parent post]: warm-up\n\n[Reply]: warm-up".to_string()],
            client.warmup_timeout,
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::toxicity::classifier::ItemOutcome;

    #[test]
    fn exhausted_server_error_maps_to_transient() {
        // A retryable error reaching the post-retry mapping means the budget was
        // exhausted on a transient failure (here a 5xx) → typed transient error
        // so the burst stops gracefully + resumably.
        let e = classifier_error_from_exhausted(RunPodError::ServerError(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        ));
        assert!(
            e.downcast_ref::<crate::toxicity::classifier::ClassifierTransientError>()
                .is_some(),
            "exhausted 5xx should surface as a transient classifier error"
        );
    }

    #[test]
    fn cost_capped_surfaces_as_ceiling_exceeded() {
        // A per-attempt cost check that trips inside the retry closure must
        // surface as the downcastable CostCeilingExceeded (the burst keys on it),
        // not as a generic/transient error.
        let e = classifier_error_from_exhausted(RunPodError::CostCapped(
            crate::toxicity::cost_meter::CostCeilingExceeded {
                est_cents: 600,
                ceiling_cents: 500,
            },
        ));
        assert!(
            e.downcast_ref::<crate::toxicity::cost_meter::CostCeilingExceeded>()
                .is_some(),
            "cost-capped retry must surface as CostCeilingExceeded"
        );
    }

    #[test]
    fn exhausted_client_error_stays_permanent() {
        // A 4xx is non-retryable → permanent → must NOT be transient (the burst
        // must abort, not interrupt-and-livelock on resume).
        let e = classifier_error_from_exhausted(RunPodError::ClientError(
            reqwest::StatusCode::BAD_REQUEST,
        ));
        assert!(
            e.downcast_ref::<crate::toxicity::classifier::ClassifierTransientError>()
                .is_none(),
            "a permanent 4xx must not be classified transient"
        );
    }

    #[test]
    fn classifier_max_retries_default_is_widened() {
        // Default retry budget must be wide enough to bridge a RunPod blip.
        assert!(
            classifier_max_retries(None) >= 6,
            "default retry budget should be widened to >= 6"
        );
        assert_eq!(
            classifier_max_retries(Some("2")),
            2,
            "valid env override honored"
        );
        assert_eq!(
            classifier_max_retries(Some("garbage")),
            classifier_max_retries(None),
            "garbage env falls back to default"
        );
    }

    // ── Batch wire fixtures (supersede the single-output fixtures) ──
    fn batch_json_with_timing() -> &'static str {
        r#"{
            "id": "abc-123",
            "status": "COMPLETED",
            "delayTime": 4800,
            "executionTime": 700,
            "output": { "verdicts": [
                {"ok": true, "toxic": false, "confidence": 0.1,
                 "model": "cope-b-a4b", "policy_version": "policy-v1"}
            ] }
        }"#
    }

    fn batch_json_without_timing() -> &'static str {
        r#"{
            "id": "def-456",
            "status": "COMPLETED",
            "output": { "verdicts": [
                {"ok": true, "toxic": true, "confidence": 0.9,
                 "model": "cope-b-a4b", "policy_version": "policy-v1"}
            ] }
        }"#
    }

    #[test]
    fn build_batch_body_wraps_contents_list() {
        let body = RunPodCopeBClient::build_batch_request_body(&["a".into(), "b".into()]);
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["input"]["contents"][0], "a");
        assert_eq!(v["input"]["contents"][1], "b");
    }

    #[test]
    fn parse_batch_maps_mixed_ok_and_error_slots_in_order() {
        let raw = r#"{
            "status": "COMPLETED",
            "output": { "verdicts": [
                {"ok": true, "toxic": true, "confidence": 0.8,
                 "model": "cope-b-a4b", "policy_version": "p1"},
                {"ok": false, "error": "unexpected model token: 'maybe'"}
            ] }
        }"#;
        let out = RunPodCopeBClient::parse_batch_response(raw, 1234).unwrap();
        assert_eq!(out.len(), 2);
        match &out[0] {
            ItemOutcome::Verdict(v) => {
                assert!(v.toxic_token);
                assert!((v.confidence - 0.8).abs() < f32::EPSILON);
                assert_eq!(v.latency_ms, 1234);
            }
            _ => panic!("slot 0 should be a Verdict"),
        }
        assert!(matches!(out[1], ItemOutcome::Error(ref e) if e.contains("maybe")));
    }

    #[test]
    fn parse_batch_rejects_out_of_range_confidence_as_item_error() {
        // A NaN/out-of-[0,1] confidence must not silently skew thresholds; the
        // slot becomes an ItemOutcome::Error, not a Verdict.
        let raw = r#"{
            "status": "COMPLETED",
            "output": { "verdicts": [
                {"ok": true, "toxic": true, "confidence": 1.7,
                 "model": "cope-b-a4b", "policy_version": "p1"}
            ] }
        }"#;
        let out = RunPodCopeBClient::parse_batch_response(raw, 1).unwrap();
        assert!(matches!(out[0], ItemOutcome::Error(_)));
    }

    #[test]
    fn parse_batch_timing_fields_parse_from_envelope() {
        let env: JobEnvelope = serde_json::from_str(batch_json_with_timing()).expect("deserialize");
        assert_eq!(env.delay_time_ms, Some(4800));
        assert_eq!(env.execution_time_ms, Some(700));
    }

    #[test]
    fn parse_batch_single_verdict_with_timing() {
        let out = RunPodCopeBClient::parse_batch_response(batch_json_with_timing(), 5500)
            .expect("parse ok");
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], ItemOutcome::Verdict(ref v)
            if !v.toxic_token && (v.confidence - 0.1).abs() < f32::EPSILON));
    }

    #[test]
    fn parse_batch_single_verdict_without_timing() {
        let out = RunPodCopeBClient::parse_batch_response(batch_json_without_timing(), 1000)
            .expect("parse ok");
        assert!(matches!(out[0], ItemOutcome::Verdict(ref v) if v.toxic_token));
    }
}
