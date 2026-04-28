//! Zentropi CoPE production client — binary toxicity classification.
//!
//! Calls the Zentropi `/v1/label` endpoint with a pre-built labeler ID.
//! The labeler holds a conversation-scoped policy (see `refs/labeler_prompt.txt`)
//! that flags content directed at conversation participants but not third-party
//! commentary or general venting.
//!
//! Used as the secondary classifier in `TwoStageToxicityScorer`. ONNX runs first
//! as a clean-pass filter (< 0.10 = cleared, no Zentropi call). Posts at or above
//! the threshold are sent to Zentropi for a binary label (1 = toxic, 0 = safe).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, warn};

const ZENTROPI_API_URL: &str = "https://api.zentropi.ai/v1/label";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RETRIES: u32 = 3;
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Request using a pre-built labeler. Tokens charged once per labeler version
/// (cheaper than sending the full policy with each request).
#[derive(Serialize)]
struct ZentropiLabelerRequest {
    content_text: String,
    labeler_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    labeler_version_id: Option<String>,
}

/// Response from Zentropi `/v1/label`.
#[derive(Deserialize, Debug, Clone)]
pub struct ZentropiResponse {
    /// "1" if toxic, "0" if safe (string per Zentropi API contract)
    pub label: String,
    /// Model confidence in [0.0, 1.0]
    pub confidence: f64,
    /// Server-side compute time in seconds (for diagnostics)
    pub compute_time: f64,
}

impl ZentropiResponse {
    /// True when the labeler flagged the content as toxic.
    pub fn is_toxic(&self) -> bool {
        self.label == "1"
    }
}

/// Production Zentropi client.
///
/// Holds a single `reqwest::Client` (connection-pooled) plus credentials and
/// labeler config. Cheap to clone — `reqwest::Client` uses an `Arc` internally.
#[derive(Clone, Debug)]
pub struct ZentropiClient {
    client: reqwest::Client,
    api_key: String,
    labeler_id: String,
    labeler_version_id: Option<String>,
}

impl ZentropiClient {
    /// Build a new client. Returns an error if `api_key` or `labeler_id` is empty
    /// since those are non-recoverable configuration mistakes.
    pub fn new(
        api_key: String,
        labeler_id: String,
        labeler_version_id: Option<String>,
    ) -> Result<Self> {
        if api_key.is_empty() {
            anyhow::bail!("ZENTROPI_API_KEY is empty");
        }
        if labeler_id.is_empty() {
            anyhow::bail!("ZENTROPI_LABELER_ID is empty");
        }

        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("Failed to build reqwest client for Zentropi")?;

        Ok(Self {
            client,
            api_key,
            labeler_id,
            labeler_version_id,
        })
    }

    /// Classify a single text. Retries up to `MAX_RETRIES` times on transient
    /// failures (5xx, 429, network errors) with exponential backoff.
    pub async fn classify(&self, text: &str) -> Result<ZentropiResponse> {
        let request = ZentropiLabelerRequest {
            content_text: text.to_string(),
            labeler_id: self.labeler_id.clone(),
            labeler_version_id: self.labeler_version_id.clone(),
        };

        let mut backoff = INITIAL_BACKOFF;
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 0..=MAX_RETRIES {
            match self.send_once(&request).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    if attempt < MAX_RETRIES && is_retriable(&e) {
                        debug!(attempt, backoff_ms = backoff.as_millis() as u64, error = %e, "Zentropi retry");
                        sleep(backoff).await;
                        backoff *= 2;
                        last_err = Some(e);
                        continue;
                    }
                    return Err(e);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("Zentropi exhausted retries")))
    }

    /// Classify a reply in the context of its parent post.
    ///
    /// The labeler policy is conversation-scoped — wrapping the parent and
    /// reply in a single `[Parent post] / [Reply]` envelope lets the policy
    /// evaluate whether the reply is hostile toward the parent's author.
    pub async fn classify_pair(
        &self,
        parent_text: &str,
        reply_text: &str,
    ) -> Result<ZentropiResponse> {
        let combined = format!("[Parent post]: {}\n\n[Reply]: {}", parent_text, reply_text);
        self.classify(&combined).await
    }

    async fn send_once(&self, request: &ZentropiLabelerRequest) -> Result<ZentropiResponse> {
        let response = self
            .client
            .post(ZENTROPI_API_URL)
            .bearer_auth(&self.api_key)
            .json(request)
            .send()
            .await
            .context("Failed to reach Zentropi API")?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Zentropi API error {}: {}", status, body);
        }

        response
            .json::<ZentropiResponse>()
            .await
            .context("Failed to parse Zentropi response")
    }

    /// Run the diagnostic suite — fixed test cases that exercise ally content,
    /// third-party commentary, hostile content, and reply pairs. Used by the
    /// `zentropi-check` CLI command to validate the labeler is configured
    /// correctly and producing the expected behavior.
    pub async fn run_diagnostic(&self) -> Result<DiagnosticResults> {
        let solo_cases = vec![
            // Ally content — should be NOT toxic (label: 0)
            ("Fuck yeah, fat liberation!", false),
            ("Fat people deserve respect and dignity", false),
            (
                "As a queer person, I love seeing this representation",
                false,
            ),
            ("Trans rights are human rights, full stop", false),
            (
                "This is such a powerful discussion about body politics",
                false,
            ),
            // Third-party commentary — should NOT be toxic per policy
            (
                "Politicians who cut disability funding are despicable",
                false,
            ),
            ("I can't believe the Supreme Court ruled that way", false),
            // Hostile content directed at participants — SHOULD be toxic
            ("You're disgusting and everyone knows it", true),
            ("You people are mentally ill and need help", true),
            ("Imagine being this stupid about basic biology", true),
            (
                "Allow me to break this down into simpler concepts for you",
                true,
            ),
            (
                "I bet your parents are so disappointed in what you became",
                true,
            ),
            // Passive-aggressive / patronizing — SHOULD be toxic
            (
                "It's actually surprising you arrived punctually this time",
                true,
            ),
            ("Oh, here comes the supposed expert", true),
            // Legitimate disagreement — should NOT be toxic
            ("I disagree with this policy approach entirely", false),
            ("That assertion is wrong — here's the real data", false),
        ];

        let pair_cases = vec![
            (
                "I've been working on fat liberation advocacy for years",
                "You're delusional if you think that's healthy",
                true,
            ),
            (
                "I've been working on fat liberation advocacy for years",
                "That's amazing work, keep it up!",
                false,
            ),
            (
                "Here's my take on the new policy",
                "I disagree — the data doesn't support that conclusion",
                false,
            ),
        ];

        let mut results = DiagnosticResults::default();

        for (text, expected_toxic) in &solo_cases {
            // Every test case counts toward `total` — including failures —
            // so accuracy reflects the end-to-end pass rate. Otherwise a
            // half-failing labeler would still print 100% accuracy across
            // the surviving subset, masking the outage.
            results.total += 1;
            match self.classify(text).await {
                Ok(response) => {
                    let actual_toxic = response.is_toxic();
                    let correct = *expected_toxic == actual_toxic;
                    if correct {
                        results.correct += 1;
                    }
                    results.details.push(DiagnosticDetail {
                        text: text.to_string(),
                        expected_toxic: *expected_toxic,
                        actual_toxic,
                        confidence: response.confidence,
                        compute_time: response.compute_time,
                        correct,
                    });
                }
                Err(e) => {
                    warn!(text, error = %e, "Zentropi diagnostic failed on solo case");
                    results.errors.push(format!("Failed on '{}': {}", text, e));
                    results.details.push(DiagnosticDetail {
                        text: format!("{} [API ERROR]", text),
                        expected_toxic: *expected_toxic,
                        actual_toxic: false,
                        confidence: 0.0,
                        compute_time: 0.0,
                        correct: false,
                    });
                }
            }
        }

        for (parent, reply, expected_toxic) in &pair_cases {
            results.total += 1;
            match self.classify_pair(parent, reply).await {
                Ok(response) => {
                    let actual_toxic = response.is_toxic();
                    let correct = *expected_toxic == actual_toxic;
                    if correct {
                        results.correct += 1;
                    }
                    results.details.push(DiagnosticDetail {
                        text: format!("[pair] {} -> {}", parent, reply),
                        expected_toxic: *expected_toxic,
                        actual_toxic,
                        confidence: response.confidence,
                        compute_time: response.compute_time,
                        correct,
                    });
                }
                Err(e) => {
                    warn!(parent, reply, error = %e, "Zentropi diagnostic failed on pair case");
                    results.errors.push(format!("Failed on pair: {}", e));
                    results.details.push(DiagnosticDetail {
                        text: format!("[pair] {} -> {} [API ERROR]", parent, reply),
                        expected_toxic: *expected_toxic,
                        actual_toxic: false,
                        confidence: 0.0,
                        compute_time: 0.0,
                        correct: false,
                    });
                }
            }
        }

        results.accuracy = if results.total > 0 {
            results.correct as f64 / results.total as f64
        } else {
            0.0
        };
        Ok(results)
    }
}

/// Network/server errors that warrant a retry — transient HTTP failures and
/// connection errors. 4xx (except 429) is treated as a hard failure since the
/// request is malformed.
fn is_retriable(err: &anyhow::Error) -> bool {
    let s = format!("{}", err);
    // 5xx server errors and 429 rate limiting from send_once
    if s.contains("Zentropi API error 5") || s.contains("Zentropi API error 429") {
        return true;
    }
    // Connection/transport errors from reqwest
    if s.contains("Failed to reach Zentropi API") {
        return true;
    }
    false
}

/// Aggregate diagnostic results — accuracy across the test suite plus per-case
/// detail for inspection.
#[derive(Debug, Default)]
pub struct DiagnosticResults {
    pub total: usize,
    pub correct: usize,
    pub accuracy: f64,
    pub errors: Vec<String>,
    pub details: Vec<DiagnosticDetail>,
}

/// Per-test-case diagnostic record.
#[derive(Debug)]
pub struct DiagnosticDetail {
    pub text: String,
    pub expected_toxic: bool,
    pub actual_toxic: bool,
    pub confidence: f64,
    pub compute_time: f64,
    pub correct: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_is_toxic_returns_true_for_label_1() {
        let r = ZentropiResponse {
            label: "1".to_string(),
            confidence: 0.95,
            compute_time: 0.18,
        };
        assert!(r.is_toxic());
    }

    #[test]
    fn response_is_toxic_returns_false_for_label_0() {
        let r = ZentropiResponse {
            label: "0".to_string(),
            confidence: 0.92,
            compute_time: 0.15,
        };
        assert!(!r.is_toxic());
    }

    #[test]
    fn new_rejects_empty_api_key() {
        let err = ZentropiClient::new(String::new(), "labeler-123".to_string(), None).unwrap_err();
        assert!(format!("{err}").contains("ZENTROPI_API_KEY"));
    }

    #[test]
    fn new_rejects_empty_labeler_id() {
        let err = ZentropiClient::new("key".to_string(), String::new(), None).unwrap_err();
        assert!(format!("{err}").contains("ZENTROPI_LABELER_ID"));
    }

    #[test]
    fn is_retriable_recognizes_5xx() {
        let e = anyhow::anyhow!("Zentropi API error 503: gateway timeout");
        assert!(is_retriable(&e));
    }

    #[test]
    fn is_retriable_recognizes_429() {
        let e = anyhow::anyhow!("Zentropi API error 429: too many requests");
        assert!(is_retriable(&e));
    }

    #[test]
    fn is_retriable_recognizes_network_error() {
        let e = anyhow::anyhow!("Failed to reach Zentropi API: connection refused");
        assert!(is_retriable(&e));
    }

    #[test]
    fn is_retriable_rejects_4xx() {
        let e = anyhow::anyhow!("Zentropi API error 401: unauthorized");
        assert!(!is_retriable(&e));
    }
}
