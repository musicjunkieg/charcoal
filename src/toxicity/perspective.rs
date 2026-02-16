// Google Perspective API implementation.
//
// Perspective API analyzes text for toxicity, identity attacks, insults, etc.
// It's free to use but rate-limited to ~1 QPS. The API is being sunset
// Dec 31, 2026 — this implementation is wrapped behind the ToxicityScorer
// trait so it can be swapped out when that happens.
//
// API docs: https://developers.perspectiveapi.com/s/about-the-api-methods

use anyhow::{Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use super::rate_limiter::RateLimiter;
use super::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

/// Perspective API toxicity scorer.
pub struct PerspectiveScorer {
    client: Client,
    api_key: String,
    rate_limiter: RateLimiter,
}

impl PerspectiveScorer {
    /// Create a new Perspective API scorer with the given API key.
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            // Perspective free tier: 1 query per second
            rate_limiter: RateLimiter::new(1.0),
        }
    }
}

#[async_trait]
impl ToxicityScorer for PerspectiveScorer {
    async fn score_text(&self, text: &str) -> Result<ToxicityResult> {
        // Respect rate limits before making the call
        self.rate_limiter.acquire().await;

        let url = format!(
            "https://commentanalyzer.googleapis.com/v1alpha1/comments:analyze?key={}",
            self.api_key
        );

        let request = PerspectiveRequest {
            comment: Comment {
                text: text.to_string(),
            },
            requested_attributes: RequestedAttributes {
                toxicity: AttributeConfig {},
                severe_toxicity: AttributeConfig {},
                identity_attack: AttributeConfig {},
                insult: AttributeConfig {},
                profanity: AttributeConfig {},
                threat: AttributeConfig {},
            },
            languages: vec!["en".to_string()],
        };

        let response = self
            .client
            .post(&url)
            .json(&request)
            .send()
            .await
            .context("Failed to call Perspective API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Perspective API returned {}: {}", status, body);
        }

        let result: PerspectiveResponse = response
            .json()
            .await
            .context("Failed to parse Perspective API response")?;

        let toxicity = extract_score(&result, "TOXICITY").unwrap_or(0.0);
        let severe_toxicity = extract_score(&result, "SEVERE_TOXICITY");
        let identity_attack = extract_score(&result, "IDENTITY_ATTACK");
        let insult = extract_score(&result, "INSULT");
        let profanity = extract_score(&result, "PROFANITY");
        let threat = extract_score(&result, "THREAT");

        debug!(
            toxicity = toxicity,
            severe_toxicity = ?severe_toxicity,
            identity_attack = ?identity_attack,
            text_preview = &text[..text.len().min(50)],
            "Scored text"
        );

        Ok(ToxicityResult {
            toxicity,
            attributes: ToxicityAttributes {
                severe_toxicity,
                identity_attack,
                insult,
                profanity,
                threat,
            },
        })
    }
}

/// Extract a specific attribute's summary score from the API response.
fn extract_score(response: &PerspectiveResponse, attribute: &str) -> Option<f64> {
    response
        .attribute_scores
        .get(attribute)
        .map(|score| score.summary_score.value)
}

// --- Perspective API request/response types ---

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PerspectiveRequest {
    comment: Comment,
    requested_attributes: RequestedAttributes,
    languages: Vec<String>,
}

#[derive(Serialize)]
struct Comment {
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
struct RequestedAttributes {
    toxicity: AttributeConfig,
    severe_toxicity: AttributeConfig,
    identity_attack: AttributeConfig,
    insult: AttributeConfig,
    profanity: AttributeConfig,
    threat: AttributeConfig,
}

#[derive(Serialize)]
struct AttributeConfig {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PerspectiveResponse {
    attribute_scores: std::collections::HashMap<String, AttributeScore>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AttributeScore {
    summary_score: SummaryScore,
}

#[derive(Deserialize)]
struct SummaryScore {
    value: f64,
}
