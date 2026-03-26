// OpenAI Moderation API scorer — free endpoint for content moderation.
//
// Uses the omni-moderation-2024-09-26 model which provides category scores
// for hate, harassment, violence, etc. Mapped to our ToxicityAttributes
// for compatibility with the existing scoring pipeline.
//
// The endpoint is free (no token charges) but requires an API key.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use tracing::debug;

use super::traits::{ToxicityAttributes, ToxicityResult, ToxicityScorer};

/// Pinned model version for reproducible scoring.
const MODEL: &str = "omni-moderation-2024-09-26";
const ENDPOINT: &str = "https://api.openai.com/v1/moderations";

/// OpenAI Moderation API scorer implementing the ToxicityScorer trait.
pub struct OpenAiModerationScorer {
    client: reqwest::Client,
    api_key: String,
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
        })
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
            "model": MODEL,
            "input": text,
        });

        let response = self
            .client
            .post(ENDPOINT)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .context("OpenAI Moderation API request failed")?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenAI Moderation API error {status}: {error_body}");
        }

        let moderation: ModerationResponse = response
            .json()
            .await
            .context("Failed to parse OpenAI Moderation response")?;

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
}
