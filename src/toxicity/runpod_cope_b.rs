//! Self-hosted CoPE-B-A4B classifier on RunPod Serverless.
//!
//! Wire shape:
//!   POST <endpoint_url>/runsync
//!   Authorization: Bearer <api_key>
//!   {"input": {"content": "<envelope>"}}
//!   -> {"output": {"toxic": bool, "confidence": float, "model": str, "policy_version": str}}
//!
//! Retries on 5xx with bounded decorrelated jitter. 4xx surfaces immediately
//! (config / contract issue). Timeout is split between warm-up and steady-
//! state — see `RunPodCopeBClient::classify_with_timeout`.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use backon::{ExponentialBuilder, Retryable};
use serde::Deserialize;
use std::time::{Duration, Instant};
use thiserror::Error;

use std::sync::Arc;

use super::classifier::{ClassifierVerdict, ToxicityClassifier};
use super::cost_meter::ScanCostMeter;

/// Calibration note (Chunk 5 / Step 5): verify distribution parity vs the
/// reference (Zentropi) backend over ab_sample.jsonl; retune only if materially
/// shifted. See docs/superpowers/specs/2026-06-05-cope-b-self-hosted-design.md
/// §"Step 5". 0.5 is the initial value — a model emitting a binary token with
/// logprob-based confidence concentrates probability sharply, so the calibrated
/// threshold may land closer to 0.7+. Update via code change, never env
/// (spec §"Backend selection").
pub const COPE_B_THRESHOLD: f32 = 0.5;

const INITIAL_BACKOFF_MS: u64 = 500;

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
    output: Option<RawOutput>,
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

/// Result of interpreting a single job-envelope response.
enum JobOutcome {
    Completed(ClassifierVerdict),
    /// Job accepted but not finished yet; carries the id to poll `/status` with.
    Pending(String),
}

#[derive(Debug, Deserialize)]
struct RawOutput {
    toxic: bool,
    confidence: f32,
    #[serde(default = "default_model")]
    model: String,
    /// Chunk 3's handler.py always emits policy_version; we default if the
    /// field is missing (older handler, test stubs) so deserialization never
    /// hard-fails on an otherwise-valid response.
    #[serde(default = "default_policy_version")]
    policy_version: String,
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
        let max_retries = std::env::var("CHARCOAL_CLASSIFIER_MAX_RETRIES")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(3);

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

    pub fn build_request_body(content: &str) -> String {
        serde_json::json!({ "input": { "content": content } }).to_string()
    }

    /// Interpret one job-envelope response into a terminal verdict, a pending
    /// signal (poll `/status`), or an error.
    fn parse_job(raw: &str, latency_ms: u32) -> Result<JobOutcome> {
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

        // Capture timing fields before the partial move of `env.output`.
        let delay_time_ms = env.delay_time_ms;
        let execution_time_ms = env.execution_time_ms;

        if let Some(out) = env.output {
            // `confidence` crosses an external boundary. A NaN or out-of-[0,1]
            // value would silently skew `is_toxic` threshold comparisons, so
            // reject it loudly (no silent fallback) rather than propagate it.
            let confidence = out.confidence;
            if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
                bail!("RunPod confidence out of contract (expected finite value in [0,1]): {confidence}");
            }
            crate::observability::classifier_metrics::record_runpod_timing(
                delay_time_ms,
                execution_time_ms,
                latency_ms,
            );
            return Ok(JobOutcome::Completed(ClassifierVerdict {
                toxic_token: out.toxic,
                confidence,
                latency_ms,
                model_id: out.model,
                policy_version: out.policy_version,
            }));
        }

        // No output. If the job is still running, return its id to poll on;
        // otherwise the response is malformed (e.g. COMPLETED but no output).
        if matches!(status.as_str(), "IN_QUEUE" | "IN_PROGRESS") {
            let id = env
                .id
                .ok_or_else(|| anyhow::anyhow!("RunPod {status} response missing job id: {raw}"))?;
            return Ok(JobOutcome::Pending(id));
        }
        bail!("RunPod job {status:?} returned no output: {raw}");
    }

    pub fn parse_response(raw: &str, latency_ms: u32) -> Result<ClassifierVerdict> {
        match Self::parse_job(raw, latency_ms)? {
            JobOutcome::Completed(v) => Ok(v),
            JobOutcome::Pending(id) => {
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
    ) -> Result<ClassifierVerdict> {
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
            match Self::parse_job(&body, latency_ms)? {
                JobOutcome::Completed(v) => return Ok(v),
                JobOutcome::Pending(_) => continue,
            }
        }
    }

    /// Single attempt — issued from inside the retry loop in classify_with_timeout.
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

    async fn classify_with_timeout(
        &self,
        content: &str,
        timeout: Duration,
    ) -> Result<(ClassifierVerdict, u32)> {
        // Cost backstop: arm-then-check before issuing ANY RunPod request. This
        // is the single chokepoint every request path flows through (classify
        // and warm_up), so the meter cannot be bypassed. Over the ceiling this
        // returns a non-retryable error — it sits OUTSIDE the backon retry loop
        // below, so it is inherently non-retryable — that rides the same skip
        // path the live HTTP 402 already exercised.
        self.meter.arm_and_check()?;

        let body = Self::build_request_body(content);
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

        let attempt = move || {
            let client = client.clone();
            let url = url_owned.clone();
            let key = api_key.clone();
            let body = body_owned.clone();
            let retries = retries_in.clone();
            async move {
                let r = Self::attempt(&client, &url, &key, &body, timeout).await;
                if r.is_err() {
                    retries.bump();
                }
                r
            }
        };

        let response = attempt
            .retry(
                ExponentialBuilder::default()
                    .with_min_delay(Duration::from_millis(INITIAL_BACKOFF_MS))
                    .with_max_times(self.max_retries as usize)
                    .with_jitter(),
            )
            .when(|e: &RunPodError| e.is_retryable())
            .await?;

        let latency_ms: u32 = start.elapsed().as_millis().try_into().unwrap_or(u32::MAX);
        // RetryCounter bumps on every failed attempt. Each failure that doesn't
        // exhaust the budget triggers exactly one retry; the final successful
        // attempt does NOT bump. So `get()` already equals retries-issued.
        let observed = retries.get();
        // /runsync may return before the job finishes (cold starts exceed its
        // ~90s wait) — in that case poll /status until terminal.
        let verdict = match Self::parse_job(&response, latency_ms)? {
            JobOutcome::Completed(v) => v,
            JobOutcome::Pending(id) => self.poll_status(&id, start, timeout).await?,
        };
        Ok((verdict, observed))
    }
}

#[async_trait]
impl ToxicityClassifier for RunPodCopeBClient {
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
        // Cost backstop is enforced inside classify_with_timeout (the single
        // RunPod request chokepoint), so both classify and warm_up are gated.
        let (verdict, retries) = self
            .classify_with_timeout(content, self.steady_timeout)
            .await?;
        crate::observability::classifier_metrics::record_request(
            self.name(),
            verdict.latency_ms,
            verdict.toxic_token,
            retries,
        );
        Ok(verdict)
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
        .classify_with_timeout(
            "[Parent post]: warm-up\n\n[Reply]: warm-up",
            client.warmup_timeout,
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid COMPLETED envelope WITH delayTime/executionTime.
    fn completed_json_with_timing() -> &'static str {
        r#"{
            "id": "abc-123",
            "status": "COMPLETED",
            "delayTime": 4800,
            "executionTime": 700,
            "output": {
                "toxic": false,
                "confidence": 0.1,
                "model": "cope-b-a4b",
                "policy_version": "policy-v1"
            }
        }"#
    }

    /// A minimal valid COMPLETED envelope WITHOUT delayTime/executionTime
    /// (backward-compat: older handler responses, test stubs).
    fn completed_json_without_timing() -> &'static str {
        r#"{
            "id": "def-456",
            "status": "COMPLETED",
            "output": {
                "toxic": true,
                "confidence": 0.9,
                "model": "cope-b-a4b",
                "policy_version": "policy-v1"
            }
        }"#
    }

    #[test]
    fn test_runpod_timing_fields_parse_from_completed_envelope() {
        let env: JobEnvelope =
            serde_json::from_str(completed_json_with_timing()).expect("should deserialize");
        assert_eq!(
            env.delay_time_ms,
            Some(4800),
            "delayTime should deserialize to Some(4800)"
        );
        assert_eq!(
            env.execution_time_ms,
            Some(700),
            "executionTime should deserialize to Some(700)"
        );
    }

    #[test]
    fn test_parse_response_succeeds_with_timing_fields() {
        // parse_response is the public entry point; it should still return a
        // valid verdict even when delayTime/executionTime are present.
        let verdict = RunPodCopeBClient::parse_response(completed_json_with_timing(), 5500)
            .expect("should parse successfully");
        assert!(!verdict.toxic_token);
        assert!((verdict.confidence - 0.1).abs() < f32::EPSILON);
        assert_eq!(verdict.latency_ms, 5500);
    }

    #[test]
    fn test_timing_fields_absent_deserialize_to_none() {
        let env: JobEnvelope =
            serde_json::from_str(completed_json_without_timing()).expect("should deserialize");
        assert_eq!(env.delay_time_ms, None, "missing delayTime should be None");
        assert_eq!(
            env.execution_time_ms, None,
            "missing executionTime should be None"
        );
    }

    #[test]
    fn test_parse_response_backward_compat_without_timing() {
        // Older responses that omit timing fields should still parse correctly.
        let verdict = RunPodCopeBClient::parse_response(completed_json_without_timing(), 1000)
            .expect("should parse successfully without timing fields");
        assert!(verdict.toxic_token);
        assert!((verdict.confidence - 0.9).abs() < f32::EPSILON);
    }
}
