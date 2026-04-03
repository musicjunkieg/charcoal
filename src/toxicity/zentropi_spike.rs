// src/toxicity/zentropi_spike.rs
//
// Spike: test Zentropi CoPE API for binary toxicity classification.
// This is temporary code for validation — will be promoted to production
// or deleted based on spike results.
//
// Uses Bryan's pre-built Zentropi labeler (labeler_id) which has a
// conversation-scoped toxic content policy. The policy only flags content
// directed at conversation participants — NOT third-party discussions,
// political commentary, or general venting. This is exactly right for
// Charcoal's use case (predicting reply harassment).
//
// The labeler policy is stored in refs/labeler_prompt.txt for reference.
// The labeler_id is configured via ZENTROPI_LABELER_ID env var.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const ZENTROPI_API_URL: &str = "https://api.zentropi.ai/v1/label";

/// Request using a pre-built labeler (saves tokens, ensures consistency).
/// Note: An inline `criteria_text` fallback was intentionally omitted for this
/// spike. If the labeler becomes unavailable, evaluate whether to add a
/// `ZentropiCriteriaRequest` variant with the policy from refs/labeler_prompt.txt.
#[derive(Serialize)]
struct ZentropiLabelerRequest {
    content_text: String,
    labeler_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    labeler_version_id: Option<String>,
}

#[derive(Deserialize, Debug)]
pub struct ZentropiResponse {
    pub label: String,
    pub confidence: f64,
    pub compute_time: f64,
}

impl ZentropiResponse {
    pub fn is_toxic(&self) -> bool {
        self.label == "1"
    }
}

pub struct ZentropiSpike {
    client: reqwest::Client,
    api_key: String,
    labeler_id: String,
    labeler_version_id: Option<String>,
}

impl ZentropiSpike {
    pub fn new(api_key: String, labeler_id: String, labeler_version_id: Option<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            labeler_id,
            labeler_version_id,
        }
    }

    /// Classify a single text using the pre-built labeler.
    pub async fn classify(&self, text: &str) -> Result<ZentropiResponse> {
        let request = ZentropiLabelerRequest {
            content_text: text.to_string(),
            labeler_id: self.labeler_id.clone(),
            labeler_version_id: self.labeler_version_id.clone(),
        };

        let response = self
            .client
            .post(ZENTROPI_API_URL)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .context("Failed to reach Zentropi API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Zentropi API error {}: {}", status, body);
        }

        response
            .json::<ZentropiResponse>()
            .await
            .context("Failed to parse Zentropi response")
    }

    /// Classify a reply in the context of its parent post.
    ///
    /// The labeler policy is conversation-scoped — it expects content
    /// that addresses participants. Framing as a conversation exchange
    /// lets the policy correctly evaluate whether the reply is toxic
    /// toward the parent post's author.
    pub async fn classify_pair(
        &self,
        parent_text: &str,
        reply_text: &str,
    ) -> Result<ZentropiResponse> {
        let combined = format!("[Parent post]: {}\n\n[Reply]: {}", parent_text, reply_text);
        self.classify(&combined).await
    }

    /// Run the spike validation suite.
    pub async fn run_validation(&self) -> Result<SpikeResults> {
        let test_cases = vec![
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

        let mut results = SpikeResults::default();
        for (text, expected_toxic) in &test_cases {
            match self.classify(text).await {
                Ok(response) => {
                    let actual_toxic = response.is_toxic();
                    let correct = *expected_toxic == actual_toxic;
                    results.total += 1;
                    if correct {
                        results.correct += 1;
                    }
                    results.details.push(SpikeDetail {
                        text: text.to_string(),
                        expected_toxic: *expected_toxic,
                        actual_toxic,
                        confidence: response.confidence,
                        compute_time: response.compute_time,
                        correct,
                    });
                }
                Err(e) => {
                    results.errors.push(format!("Failed on '{}': {}", text, e));
                }
            }
        }

        // Test reply pair classification
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

        for (parent, reply, expected_toxic) in &pair_cases {
            match self.classify_pair(parent, reply).await {
                Ok(response) => {
                    let actual_toxic = response.is_toxic();
                    let correct = *expected_toxic == actual_toxic;
                    results.total += 1;
                    if correct {
                        results.correct += 1;
                    }
                    results.details.push(SpikeDetail {
                        text: format!("[pair] {} -> {}", parent, reply),
                        expected_toxic: *expected_toxic,
                        actual_toxic,
                        confidence: response.confidence,
                        compute_time: response.compute_time,
                        correct,
                    });
                }
                Err(e) => {
                    results.errors.push(format!("Failed on pair: {}", e));
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

#[derive(Debug, Default)]
pub struct SpikeResults {
    pub total: usize,
    pub correct: usize,
    pub accuracy: f64,
    pub errors: Vec<String>,
    pub details: Vec<SpikeDetail>,
}

#[derive(Debug)]
pub struct SpikeDetail {
    pub text: String,
    pub expected_toxic: bool,
    pub actual_toxic: bool,
    pub confidence: f64,
    pub compute_time: f64,
    pub correct: bool,
}
