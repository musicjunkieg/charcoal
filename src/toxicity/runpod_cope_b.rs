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

use super::classifier::{ClassifierVerdict, ToxicityClassifier};

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
}

#[derive(Debug, Deserialize)]
struct RawResponseBody {
    output: RawOutput,
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
        })
    }

    pub fn build_request_body(content: &str) -> String {
        serde_json::json!({ "input": { "content": content } }).to_string()
    }

    pub fn parse_response(raw: &str, latency_ms: u32) -> Result<ClassifierVerdict> {
        let parsed: RawResponseBody = serde_json::from_str(raw)
            .with_context(|| format!("parse RunPod response body: {raw}"))?;
        Ok(ClassifierVerdict {
            toxic_token: parsed.output.toxic,
            confidence: parsed.output.confidence,
            latency_ms,
            model_id: parsed.output.model,
            policy_version: parsed.output.policy_version,
        })
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
        Ok((Self::parse_response(&response, latency_ms)?, observed))
    }
}

#[async_trait]
impl ToxicityClassifier for RunPodCopeBClient {
    async fn classify(&self, content: &str) -> Result<ClassifierVerdict> {
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
