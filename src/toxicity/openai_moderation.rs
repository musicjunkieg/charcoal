// OpenAI Moderation API scorer — free endpoint for content moderation.
//
// Uses the omni-moderation-2024-09-26 model which provides category scores
// for hate, harassment, violence, etc. Mapped to our ToxicityAttributes
// for compatibility with the existing scoring pipeline.
//
// The endpoint is free (no token charges) but requires an API key.
// Rate limited with a concurrency semaphore (1 in-flight request at a time)
// and a minimum delay between requests to stay within OpenAI's rate limits.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{Duration, Instant};
use tracing::{debug, warn};

use super::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

const ENDPOINT: &str = "https://api.openai.com/v1/moderations";

/// Maximum retry attempts on 429 rate limit responses.
const MAX_RETRIES: u32 = 5;

/// Initial backoff delay in milliseconds (doubles each retry).
const INITIAL_BACKOFF_MS: u64 = 1000;

/// Maximum texts per batch request.
const MAX_BATCH_SIZE: usize = 32;

/// Minimum delay between API requests (200ms = ~5 requests/sec max).
const MIN_REQUEST_INTERVAL: Duration = Duration::from_millis(200);

/// OpenAI Moderation API scorer implementing the ToxicityScorer trait.
///
/// Uses a semaphore to ensure only one API request is in-flight at a time,
/// and a minimum interval between requests to avoid rate limiting.
pub struct OpenAiModerationScorer {
    client: reqwest::Client,
    api_key: String,
    /// Only one request at a time to avoid rate limiting.
    semaphore: Arc<Semaphore>,
    /// Track when the last request was sent to enforce minimum interval.
    last_request: Arc<Mutex<Instant>>,
}

impl OpenAiModerationScorer {
    /// Create a new scorer with the given API key.
    pub fn new(api_key: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent("charcoal/0.1 (threat-detection; @chaosgreml.in)")
            .build()
            .context("Failed to build HTTP client for OpenAI")?;

        Ok(Self {
            client,
            api_key: api_key.to_string(),
            semaphore: Arc::new(Semaphore::new(1)),
            last_request: Arc::new(Mutex::new(Instant::now() - MIN_REQUEST_INTERVAL)),
        })
    }

    /// Send a moderation request with rate limiting and retry on 429.
    ///
    /// Acquires the semaphore (ensuring only one request at a time),
    /// waits for the minimum interval since the last request, then sends.
    async fn send_with_retry(&self, body: &serde_json::Value) -> Result<ModerationResponse> {
        let _permit = self
            .semaphore
            .acquire()
            .await
            .map_err(|e| anyhow::anyhow!("Semaphore closed: {e}"))?;

        let mut backoff_ms = INITIAL_BACKOFF_MS;

        for attempt in 0..=MAX_RETRIES {
            // Enforce minimum interval between requests
            {
                let mut last = self.last_request.lock().await;
                let elapsed = last.elapsed();
                if elapsed < MIN_REQUEST_INTERVAL {
                    tokio::time::sleep(MIN_REQUEST_INTERVAL - elapsed).await;
                }
                *last = Instant::now();
            }

            let response = self
                .client
                .post(ENDPOINT)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .json(body)
                .send()
                .await
                .context("OpenAI Moderation API request failed")?;

            let status = response.status();

            if status.as_u16() == 429 {
                if attempt < MAX_RETRIES {
                    warn!(
                        attempt = attempt + 1,
                        backoff_ms, "OpenAI rate limited (429), retrying after backoff"
                    );
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    backoff_ms *= 2;
                    continue;
                }
                let error_body = response.text().await.unwrap_or_default();
                anyhow::bail!(
                    "OpenAI Moderation API rate limited after {MAX_RETRIES} retries: {error_body}"
                );
            }

            if !status.is_success() {
                let error_body = response.text().await.unwrap_or_default();
                anyhow::bail!("OpenAI Moderation API error {status}: {error_body}");
            }

            let moderation: ModerationResponse = response
                .json()
                .await
                .context("Failed to parse OpenAI Moderation response")?;

            return Ok(moderation);
        }

        anyhow::bail!("OpenAI Moderation API: exhausted retries")
    }
}

/// Category scores from the OpenAI Moderation API response.
#[derive(Debug, Deserialize)]
struct ModerationCategoryScores {
    hate: f64,
    #[serde(rename = "hate/threatening")]
    hate_threatening: f64,
    harassment: f64,
    #[serde(rename = "harassment/threatening")]
    harassment_threatening: f64,
    violence: f64,
    #[serde(rename = "violence/graphic")]
    violence_graphic: f64,
    #[serde(rename = "self-harm")]
    #[allow(dead_code)]
    self_harm: f64,
    #[serde(rename = "self-harm/intent")]
    #[allow(dead_code)]
    self_harm_intent: f64,
    #[serde(rename = "self-harm/instructions")]
    #[allow(dead_code)]
    self_harm_instructions: f64,
    #[allow(dead_code)]
    sexual: f64,
    #[serde(rename = "sexual/minors")]
    #[allow(dead_code)]
    sexual_minors: f64,
}

#[derive(Debug, Deserialize)]
struct ModerationResult {
    category_scores: ModerationCategoryScores,
}

#[derive(Debug, Deserialize)]
struct ModerationResponse {
    results: Vec<ModerationResult>,
}

impl ModerationCategoryScores {
    /// Map OpenAI categories to our ToxicityAttributes.
    fn to_toxicity_result(&self) -> ToxicityResult {
        let identity_attack = self.hate;
        let insult = self.harassment;
        let threat = self.harassment_threatening.max(self.violence);
        let severe_toxicity = self.hate_threatening.max(self.violence_graphic);

        // Overall toxicity = max of mapped categories
        let toxicity = identity_attack.max(insult).max(threat).max(severe_toxicity);

        ToxicityResult {
            toxicity,
            attributes: ToxicityAttributes {
                severe_toxicity: Some(severe_toxicity),
                identity_attack: Some(identity_attack),
                insult: Some(insult),
                profanity: None, // OpenAI doesn't have a profanity category
                threat: Some(threat),
            },
        }
    }
}

#[async_trait]
impl ToxicityScorer for OpenAiModerationScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        let body = serde_json::json!({
            "input": text,
        });

        let moderation = self.send_with_retry(&body).await?;

        let result = moderation
            .results
            .first()
            .ok_or_else(|| anyhow::anyhow!("Empty results from OpenAI Moderation"))?;

        let toxicity_result = result.category_scores.to_toxicity_result();

        debug!(
            toxicity = format!("{:.3}", toxicity_result.toxicity),
            "OpenAI Moderation scored text"
        );

        Ok(toxicity_result)
    }

    /// Batch score multiple texts in a single API call.
    /// The OpenAI Moderation API accepts an array of inputs and returns
    /// results in the same order. We chunk into MAX_BATCH_SIZE groups
    /// to keep request sizes reasonable.
    async fn score_batch(&self, texts: &[String]) -> Result<Vec<ToxicityResult>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_results = Vec::with_capacity(texts.len());

        for chunk in texts.chunks(MAX_BATCH_SIZE) {
            let body = serde_json::json!({
                "input": chunk,
            });

            let moderation = self.send_with_retry(&body).await?;

            if moderation.results.len() != chunk.len() {
                anyhow::bail!(
                    "OpenAI Moderation batch: expected {} results, got {}",
                    chunk.len(),
                    moderation.results.len()
                );
            }

            for result in &moderation.results {
                all_results.push(result.category_scores.to_toxicity_result());
            }

            debug!(batch_size = chunk.len(), "OpenAI Moderation scored batch");
        }

        Ok(all_results)
    }
}
